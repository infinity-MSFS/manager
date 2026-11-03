//! Boot-time network diagnostics, written to the log for support.
//!
//! When "nothing downloads" for one user, the log has to say which layer
//! broke. The manager's HTTP stack (reqwest + rustls + the OS root store)
//! fails differently from the browser the user compares it with:
//!
//! - rustls needs TLS 1.2+ and well-formed certificates; a TLS-inspecting
//!   antivirus that Schannel accepts can be rejected here.
//! - reqwest honours `HTTPS_PROXY` and the static WinINET proxy but does not
//!   evaluate PAC scripts.
//! - rustls trusts exactly what the OS store holds. Windows fills that store
//!   on demand, which only Schannel triggers, so a store that never grew
//!   means `UnknownIssuer` everywhere.
//! - A wrong clock invalidates every certificate at once.
//!
//! [`log_environment`] records the static picture; [`probe`] walks DNS,
//! TCP and HTTPS against a real origin, logging the first layer to fail and
//! the clock skew against the server's `Date`.

mod chain;
#[cfg(windows)]
mod registry;

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub use chain::{NetFailure, error_chain};
use tracing::{info, warn};

/// Shorter than any real client timeout: this is background context, and a
/// black-holing firewall should not keep it alive.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// Skew beyond this fails certificate validation outright.
const SKEW_WARN_SECS: i64 = 300;

/// Logs proxy configuration, root-store health and the clock. Cheap and
/// synchronous; call it before the first request.
pub fn log_environment() {
    let (roots, root_errors) = root_store();
    if roots == 0 {
        warn!(
            root_store = roots,
            errors = %root_errors,
            "netdiag: the OS root store has no certificates; all HTTPS will fail"
        );
    }
    let auto_update_disabled = root_auto_update_disabled();
    if auto_update_disabled == Some(true) {
        warn!(
            root_store = roots,
            "netdiag: root certificate auto-update is disabled by policy; HTTPS may fail here \
             while browsers succeed"
        );
    }

    let env_proxy = proxy_env();
    let (registry_proxy, pac_url) = registry_proxy();
    if pac_url.is_some() && env_proxy.is_none() && registry_proxy.is_none() {
        warn!(
            pac_url = %pac_url.as_deref().unwrap_or_default(),
            "netdiag: PAC-only proxy configuration; the manager cannot evaluate PAC and connects \
             directly. Set HTTPS_PROXY if that is blocked"
        );
    }

    info!(
        root_store = roots,
        root_store_errors = %root_errors,
        proxy_env = %env_proxy.as_deref().unwrap_or("none"),
        proxy_registry = %registry_proxy.as_deref().unwrap_or("none"),
        pac_url = %pac_url.as_deref().unwrap_or("none"),
        root_auto_update_disabled = ?auto_update_disabled,
        clock_utc = unix_now(),
        "netdiag: environment"
    );
}

/// DNS, then TCP, then HTTPS against `url`, then clock skew. Each layer
/// logs its own outcome. Never fails; spawn it and forget it.
pub async fn probe(url: &str) {
    let Ok(url) = reqwest::Url::parse(url) else {
        warn!(url, "netdiag: unparseable probe URL");
        return;
    };
    let Some(host) = url.host_str().map(str::to_owned) else {
        warn!(%url, "netdiag: probe URL has no host");
        return;
    };
    let port = url.port_or_known_default().unwrap_or(443);

    let t = Instant::now();
    let addrs = match tokio::net::lookup_host((host.as_str(), port)).await {
        Ok(it) => it.collect::<Vec<_>>(),
        Err(e) => {
            warn!(%host, ms = millis(t), error = %e, "netdiag: DNS failed");
            return;
        }
    };
    let ips = addrs
        .iter()
        .map(|a| a.ip().to_string())
        .collect::<Vec<_>>()
        .join(",");
    info!(%host, ms = millis(t), addrs = %ips, "netdiag: DNS ok");

    // Separate from TLS, so "the SYN is dropped" and "the handshake is
    // mangled" read differently.
    if let Some(addr) = addrs.first() {
        let t = Instant::now();
        match tokio::time::timeout(PROBE_TIMEOUT, tokio::net::TcpStream::connect(addr)).await {
            Ok(Ok(_)) => info!(%addr, ms = millis(t), "netdiag: TCP ok"),
            Ok(Err(e)) => warn!(%addr, ms = millis(t), error = %e, "netdiag: TCP connect failed"),
            Err(_) => warn!(%addr, ms = millis(t), "netdiag: TCP connect timed out"),
        }
    }

    let client = match reqwest::Client::builder()
        .user_agent(concat!(
            "infinity-manager/",
            env!("CARGO_PKG_VERSION"),
            " netdiag"
        ))
        .timeout(PROBE_TIMEOUT)
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            warn!(error = %error_chain(&e), "netdiag: could not build the probe client");
            return;
        }
    };
    let t = Instant::now();
    match client.get(url.clone()).send().await {
        Ok(resp) => {
            info!(%url, status = resp.status().as_u16(), ms = millis(t), "netdiag: HTTPS ok");
            report_skew(date_header(&resp).as_deref());
        }
        Err(e) => {
            let chain = error_chain(&e);
            let net = NetFailure::classify(&chain);
            warn!(
                %url,
                ms = millis(t),
                net = net.as_str(),
                hint = net.hint(),
                %chain,
                "netdiag: HTTPS failed"
            );
            // TLS is out, so ask for the server's time over plain HTTP: a
            // redirect still carries `Date`, which separates "the clock is
            // wrong" from "certificates are being rewritten".
            clock_over_plaintext(&host).await;
        }
    }
}

fn millis(t: Instant) -> u64 {
    u64::try_from(t.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn date_header(resp: &reqwest::Response) -> Option<String> {
    resp.headers()
        .get(reqwest::header::DATE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
}

async fn clock_over_plaintext(host: &str) {
    let Ok(client) = reqwest::Client::builder()
        .timeout(PROBE_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
    else {
        return;
    };
    match client.get(format!("http://{host}/")).send().await {
        Ok(resp) => report_skew(date_header(&resp).as_deref()),
        Err(e) => info!(error = %error_chain(&e), "netdiag: plaintext clock check unavailable"),
    }
}

fn report_skew(date: Option<&str>) {
    let Some(skew) = date.and_then(skew_secs) else {
        return;
    };
    if skew.abs() >= SKEW_WARN_SECS {
        warn!(
            clock_skew_secs = skew,
            server_date = date,
            "netdiag: the system clock is wrong; certificate validation will fail"
        );
    } else {
        info!(
            clock_skew_secs = skew,
            server_date = date,
            "netdiag: clock ok"
        );
    }
}

/// Local clock minus the server's, in seconds.
fn skew_secs(date: &str) -> Option<i64> {
    let server = httpdate::parse_http_date(date).ok()?;
    let server = i64::try_from(server.duration_since(UNIX_EPOCH).ok()?.as_secs()).ok()?;
    Some(unix_now() - server)
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_secs()).ok())
        .unwrap_or(0)
}

/// Certificates the same loader reqwest uses finds, and its errors.
fn root_store() -> (usize, String) {
    let result = rustls_native_certs::load_native_certs();
    let errors = result
        .errors
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ");
    (result.certs.len(), errors)
}

/// The proxy variables reqwest reads, credentials removed: these logs get
/// pasted into support threads.
fn proxy_env() -> Option<String> {
    const VARS: [&str; 8] = [
        "HTTP_PROXY",
        "http_proxy",
        "HTTPS_PROXY",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
        "NO_PROXY",
        "no_proxy",
    ];
    let found: Vec<String> = VARS
        .iter()
        .filter_map(|k| std::env::var(k).ok().map(|v| format!("{k}={}", redact(&v))))
        .collect();
    (!found.is_empty()).then(|| found.join(" "))
}

/// Strips `user:pass@` from a proxy URL.
fn redact(value: &str) -> String {
    match value.split_once("://") {
        Some((scheme, rest)) => match rest.rsplit_once('@') {
            Some((_, host)) => format!("{scheme}://***@{host}"),
            None => value.to_owned(),
        },
        None => match value.rsplit_once('@') {
            Some((_, host)) => format!("***@{host}"),
            None => value.to_owned(),
        },
    }
}

/// WinINET's static proxy (honoured by reqwest) and PAC URL (not).
#[cfg(windows)]
fn registry_proxy() -> (Option<String>, Option<String>) {
    let (server, pac) = registry::proxy();
    (server.map(|s| redact(&s)), pac)
}

#[cfg(not(windows))]
fn registry_proxy() -> (Option<String>, Option<String>) {
    (None, None)
}

/// Group Policy has switched off on-demand root updates. `None` when unset
/// or not on Windows.
#[cfg(windows)]
fn root_auto_update_disabled() -> Option<bool> {
    registry::root_auto_update_disabled()
}

#[cfg(not(windows))]
fn root_auto_update_disabled() -> Option<bool> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_are_redacted() {
        assert_eq!(
            redact("http://bob:hunter2@proxy.corp:8080"),
            "http://***@proxy.corp:8080"
        );
        assert_eq!(redact("http://proxy.corp:8080"), "http://proxy.corp:8080");
        assert_eq!(redact("bob:pw@proxy.corp:8080"), "***@proxy.corp:8080");
        assert_eq!(redact("proxy.corp:8080"), "proxy.corp:8080");
    }

    #[test]
    fn skew_is_measured_from_the_date_header() {
        let now = httpdate::fmt_http_date(SystemTime::now());
        assert!(skew_secs(&now).is_some_and(|s| s.abs() <= 2));
        let old = httpdate::fmt_http_date(UNIX_EPOCH + Duration::from_secs(1_000_000_000));
        assert!(skew_secs(&old).is_some_and(|s| s > SKEW_WARN_SECS));
        assert_eq!(skew_secs("not a date"), None);
    }

    #[test]
    fn environment_logging_does_not_panic() {
        log_environment();
    }

    /// The whole probe against a real origin. Run on a machine reproducing
    /// a "nothing downloads" report:
    ///
    /// ```text
    /// cargo test -p infinity-netdiag -- --ignored --nocapture
    /// ```
    #[tokio::test]
    #[ignore = "hits the network"]
    async fn probe_against_the_manager_origin() {
        let _ = tracing_subscriber::fmt().try_init();
        log_environment();
        probe("https://manager.infinity-simulations.com/").await;
    }
}
