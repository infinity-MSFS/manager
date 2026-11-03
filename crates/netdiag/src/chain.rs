//! Rendering a whole error chain, and guessing which layer failed.
//!
//! `reqwest::Error`'s `Display` is the outer layer only ("error sending
//! request for url (…)"); the cause (`invalid peer certificate:
//! UnknownIssuer`, `dns error`, `connection refused`) is in `source()`.
//! Log [`error_chain`], never the bare error, and tag it with
//! [`NetFailure::classify`].

use std::error::Error;

/// Bound on the walk, against a `source()` cycle.
const MAX_DEPTH: usize = 12;

/// `outer: inner: root`, skipping layers that repeat their child.
pub fn error_chain(e: &dyn Error) -> String {
    let mut out = e.to_string();
    let mut src = e.source();
    let mut depth = 0;
    while let Some(cause) = src {
        if depth >= MAX_DEPTH {
            out.push_str(": ...");
            break;
        }
        let text = cause.to_string();
        if !out.ends_with(&text) {
            out.push_str(": ");
            out.push_str(&text);
        }
        src = cause.source();
        depth += 1;
    }
    out
}

/// Coarse cause of a network failure, read from an [`error_chain`].
///
/// String matching on purpose: the concrete error types are behind
/// reqwest's private wrappers, while their messages have been stable for
/// years. A miss is [`NetFailure::Unclassified`] and the chain is still
/// logged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetFailure {
    /// Certificate outside its validity window: almost always the clock.
    TlsCertExpired,
    /// Chain ends in an untrusted root: TLS-inspecting antivirus or proxy,
    /// or an unusable OS root store.
    TlsUnknownIssuer,
    /// Certificate for another host: a captive portal or transparent proxy.
    TlsBadName,
    TlsHandshake,
    Dns,
    ConnectRefused,
    /// No route, network down, local firewall.
    ConnectUnreachable,
    Timeout,
    Proxy,
    Unclassified,
}

impl NetFailure {
    /// Certificate verdicts first: they surface inside what hyper still
    /// calls a connect error.
    pub fn classify(chain: &str) -> Self {
        let c = chain.to_ascii_lowercase();
        let any = |needles: &[&str]| needles.iter().any(|n| c.contains(n));
        if c.contains("expired") && c.contains("cert") {
            Self::TlsCertExpired
        } else if any(&[
            "unknownissuer",
            "unknown issuer",
            "unable to get local issuer",
            "self-signed",
            "self signed",
        ]) {
            Self::TlsUnknownIssuer
        } else if any(&["notvalidforname", "not valid for name"]) {
            Self::TlsBadName
        } else if any(&[
            "invalid peer certificate",
            "certificate",
            "handshake",
            "badsignature",
            "tls",
        ]) {
            Self::TlsHandshake
        } else if any(&[
            "dns error",
            "failed to lookup address",
            "name or service not known",
            "no record found",
        ]) {
            Self::Dns
        } else if c.contains("proxy") {
            Self::Proxy
        } else if any(&["connection refused", "actively refused"]) {
            Self::ConnectRefused
        } else if any(&[
            "network is unreachable",
            "no route to host",
            "host is unreachable",
            "network is down",
        ]) {
            Self::ConnectUnreachable
        } else if any(&["timed out", "timeout"]) {
            Self::Timeout
        } else {
            Self::Unclassified
        }
    }

    /// Stable tag for log fields. Same tags as v3.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TlsCertExpired => "tls_cert_expired",
            Self::TlsUnknownIssuer => "tls_unknown_issuer",
            Self::TlsBadName => "tls_bad_name",
            Self::TlsHandshake => "tls_handshake",
            Self::Dns => "dns",
            Self::ConnectRefused => "connect_refused",
            Self::ConnectUnreachable => "connect_unreachable",
            Self::Timeout => "timeout",
            Self::Proxy => "proxy",
            Self::Unclassified => "unclassified",
        }
    }

    /// The next step, for whoever reads the log.
    pub const fn hint(self) -> &'static str {
        match self {
            Self::TlsCertExpired => {
                "Compare clock_skew_secs on the netdiag line; more than a few minutes means the \
                 system clock is wrong."
            }
            Self::TlsUnknownIssuer => {
                "Read the netdiag environment line. root_store near 0, or \
                 root_auto_update_disabled=Some(true), means the Windows store never fetched the \
                 issuing root (browsers fetch it on demand, rustls cannot). Otherwise antivirus \
                 HTTPS scanning or a corporate proxy is re-signing traffic."
            }
            Self::TlsBadName => "Suspect a captive portal or transparent proxy.",
            Self::TlsHandshake => {
                "A middlebox limited to TLS 1.0/1.1 fails here; rustls needs 1.2+."
            }
            Self::Dns => "Try a public resolver; check for a half-connected VPN.",
            Self::ConnectRefused => "Check the configured server URL.",
            Self::ConnectUnreachable => {
                "Check the proxy fields on the netdiag line. With only pac_url set the browser \
                 goes through the proxy and the manager does not; set HTTPS_PROXY."
            }
            Self::Timeout => "Compare with the TCP step to tell a slow link from a black hole.",
            Self::Proxy => "Check that HTTPS_PROXY / the registry ProxyServer is reachable.",
            Self::Unclassified => "Read the chain in full.",
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fmt;

    use super::*;

    #[derive(Debug)]
    struct Layer {
        msg: &'static str,
        src: Option<Box<Layer>>,
    }

    impl fmt::Display for Layer {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(self.msg)
        }
    }

    impl Error for Layer {
        fn source(&self) -> Option<&(dyn Error + 'static)> {
            self.src.as_deref().map(|l| l as &(dyn Error + 'static))
        }
    }

    fn layered(msgs: &[&'static str]) -> Layer {
        let mut it = msgs.iter().rev();
        let mut cur = Layer {
            msg: it.next().copied().unwrap_or(""),
            src: None,
        };
        for m in it {
            cur = Layer {
                msg: m,
                src: Some(Box::new(cur)),
            };
        }
        cur
    }

    #[test]
    fn chain_renders_every_source() {
        let e = layered(&[
            "error sending request for url (https://example.test/)",
            "client error (Connect)",
            "invalid peer certificate: UnknownIssuer",
        ]);
        assert_eq!(
            error_chain(&e),
            "error sending request for url (https://example.test/): client error (Connect): \
             invalid peer certificate: UnknownIssuer"
        );
        assert_eq!(error_chain(&layered(&["boom", "boom"])), "boom");
    }

    #[test]
    fn classification() {
        let cases = [
            (
                "client error (Connect): invalid peer certificate: UnknownIssuer",
                NetFailure::TlsUnknownIssuer,
            ),
            (
                "client error (Connect): invalid peer certificate: Expired",
                NetFailure::TlsCertExpired,
            ),
            (
                "invalid peer certificate: NotValidForName",
                NetFailure::TlsBadName,
            ),
            ("dns error: no record found", NetFailure::Dns),
            (
                "tcp connect error: Connection refused (os error 111)",
                NetFailure::ConnectRefused,
            ),
            (
                "tcp connect error: Network is unreachable",
                NetFailure::ConnectUnreachable,
            ),
            ("operation timed out", NetFailure::Timeout),
            ("something new", NetFailure::Unclassified),
        ];
        for (chain, want) in cases {
            assert_eq!(NetFailure::classify(chain), want, "{chain}");
        }
    }
}
