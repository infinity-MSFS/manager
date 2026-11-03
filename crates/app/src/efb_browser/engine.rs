//! Finding and launching the rendering engine: any installed Chromium, run
//! headless and driven over the DevTools protocol.
//!
//! "Edge is on every Windows box" is false often enough to matter (v3 was
//! developed on a machine with only a Chromium fork under `%LOCALAPPDATA%`),
//! so discovery is a ladder ending in an explicit override, and a miss is
//! reported to the crew rather than assumed away:
//!
//! 1. [`ENGINE_ENV`];
//! 2. the registry's `App Paths` for `msedge.exe` then `chrome.exe`, HKCU
//!    before HKLM, which is the only thing that finds an unguessable install;
//! 3. well-known Edge and Chrome install paths.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::process::{Child, Command};
use tracing::{debug, info};

use super::error::BrowserError;

/// Point this at a Chromium binary to override discovery entirely.
pub(crate) const ENGINE_ENV: &str = "INFINITY_EFB_BROWSER";

/// The whole budget for the browser to write `DevToolsActivePort`. A fork's
/// very first launch can be slow.
const PORT_FILE_TIMEOUT: Duration = Duration::from_secs(30);
const PORT_FILE_POLL: Duration = Duration::from_millis(100);

#[derive(Debug, Clone)]
pub(crate) struct Engine {
    pub(crate) exe: PathBuf,
    /// Where it was found, for the log.
    pub(crate) source: &'static str,
}

/// The first Chromium on the ladder.
pub(crate) fn discover() -> Option<Engine> {
    if let Some(raw) = std::env::var_os(ENGINE_ENV) {
        let exe = PathBuf::from(raw);
        if exe.is_file() {
            return Some(Engine {
                exe,
                source: ENGINE_ENV,
            });
        }
        debug!(?exe, "efb-browser: {ENGINE_ENV} does not point at a file");
    }
    candidates()
        .into_iter()
        .find(|(path, _)| path.is_file())
        .map(|(exe, source)| Engine { exe, source })
}

#[cfg(windows)]
fn candidates() -> Vec<(PathBuf, &'static str)> {
    let mut out: Vec<(PathBuf, &'static str)> = ["msedge.exe", "chrome.exe"]
        .into_iter()
        .filter_map(app_paths_lookup)
        .map(|p| (p, "registry App Paths"))
        .collect();
    let known = [
        (
            "ProgramFiles(x86)",
            r"Microsoft\Edge\Application\msedge.exe",
        ),
        ("ProgramFiles", r"Microsoft\Edge\Application\msedge.exe"),
        ("ProgramFiles", r"Google\Chrome\Application\chrome.exe"),
        ("ProgramFiles(x86)", r"Google\Chrome\Application\chrome.exe"),
        ("LOCALAPPDATA", r"Google\Chrome\Application\chrome.exe"),
    ];
    for (var, rel) in known {
        if let Some(base) = std::env::var_os(var) {
            out.push((Path::new(&base).join(rel), "well-known path"));
        }
    }
    out
}

/// The default value of `...\CurrentVersion\App Paths\<exe>`, which is how
/// Windows itself records where an executable lives. HKCU first: a per-user
/// fork is the case this exists for, and machine-wide Edge would otherwise
/// always win.
#[cfg(windows)]
fn app_paths_lookup(exe: &str) -> Option<PathBuf> {
    let subkey = format!(r"SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths\{exe}");
    [
        windows_registry::CURRENT_USER,
        windows_registry::LOCAL_MACHINE,
    ]
    .into_iter()
    .filter_map(|root| root.open(&subkey).ok()?.get_string("").ok())
    .map(|s| PathBuf::from(s.trim().trim_matches('"')))
    .find(|p| p.is_file())
}

#[cfg(not(windows))]
fn candidates() -> Vec<(PathBuf, &'static str)> {
    [
        "/usr/bin/microsoft-edge",
        "/usr/bin/google-chrome",
        "/usr/bin/google-chrome-stable",
        "/usr/bin/chromium",
        "/usr/bin/chromium-browser",
        "/snap/bin/chromium",
        "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    ]
    .iter()
    .map(|p| (PathBuf::from(p), "well-known path"))
    .collect()
}

#[derive(Debug)]
pub(crate) struct Launched {
    pub(crate) child: Child,
    /// The browser-level DevTools endpoint.
    pub(crate) ws_url: String,
}

/// The command line. Each flag is here for a reason that is not obvious from
/// its name, so they are listed with it.
fn args(user_data_dir: &Path, width: u32, height: u32) -> Vec<OsString> {
    let mut profile = OsString::from("--user-data-dir=");
    profile.push(user_data_dir);
    let mut out = vec![
        "--headless=new".into(),
        // Port 0 lets it choose and write the choice to DevToolsActivePort;
        // a fixed port collides and rules out a second session.
        "--remote-debugging-port=0".into(),
        // Never the user's real profile: nothing browsed from the flight deck
        // or a paired tablet touches their cookies or saved passwords.
        profile,
        format!("--window-size={width},{height}").into(),
        "--no-first-run".into(),
        "--no-default-browser-check".into(),
        "--disable-extensions".into(),
        // Nobody is at this browser to answer a prompt.
        "--disable-notifications".into(),
        // Popups are allowed so `target=_blank` does something; the session
        // folds each new tab into the visible one.
        "--disable-popup-blocking".into(),
        "--mute-audio".into(),
        // Headless sets `navigator.webdriver`, the first thing every bot check
        // reads. The user-agent half is `Emulation.setUserAgentOverride`.
        "--disable-blink-features=AutomationControlled".into(),
        "--lang=en-US".into(),
        // Every wheel step would otherwise be an animated scroll: a burst of
        // frames per tick that the panel's frame rate cannot show.
        "--disable-smooth-scrolling".into(),
        // A persistent profile whose last run crashed would put a "restore
        // pages?" bubble in the first frame.
        "--hide-crash-restore-bubble".into(),
        // Headless with no GPU session (remote desktop, a service) hangs on
        // GPU init rather than falling back.
        "--disable-gpu".into(),
        "--disable-dev-shm-usage".into(),
    ];
    out.push("about:blank".into());
    out
}

/// `DevToolsActivePort` is the port on the first line and the browser
/// target's path on the second.
fn parse_port_file(text: &str) -> Option<String> {
    let mut lines = text.lines();
    let port: u16 = lines.next()?.trim().parse().ok()?;
    let path = lines.next()?.trim();
    if port == 0 || !path.starts_with('/') {
        return None;
    }
    Some(format!("ws://127.0.0.1:{port}{path}"))
}

/// Launches a headless engine on `user_data_dir` and waits for its DevTools
/// endpoint. `fresh` wipes the directory first (a throwaway profile).
pub(crate) async fn launch(
    engine: &Engine,
    user_data_dir: &Path,
    fresh: bool,
    width: u32,
    height: u32,
) -> Result<Launched, BrowserError> {
    if fresh {
        let _ = tokio::fs::remove_dir_all(user_data_dir).await;
    }
    tokio::fs::create_dir_all(user_data_dir)
        .await
        .map_err(BrowserError::Profile)?;
    // A reused profile still holds the last run's file, which would be read
    // at once and dialled with nothing listening.
    let port_file = user_data_dir.join("DevToolsActivePort");
    let _ = tokio::fs::remove_file(&port_file).await;

    let exe = engine.exe.display().to_string();
    let mut cmd = Command::new(&engine.exe);
    cmd.args(args(user_data_dir, width, height))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        // The backstop if a session is dropped without its shutdown running.
        // It reaches only the parent; a graceful `Browser.close` is what
        // takes the whole process tree down.
        .kill_on_drop(true);
    #[cfg(windows)]
    {
        // No console window flashing up from a GUI app.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd.spawn().map_err(|source| BrowserError::Spawn {
        exe: exe.clone(),
        source,
    })?;

    let deadline = tokio::time::Instant::now() + PORT_FILE_TIMEOUT;
    loop {
        // The child first: a browser that refused its arguments exits at once,
        // and polling for a file it will never write is a silent 30 s stall.
        if let Ok(Some(status)) = child.try_wait() {
            return Err(BrowserError::ExitedEarly {
                exe,
                status: status.to_string(),
            });
        }
        if let Some(ws_url) = tokio::fs::read_to_string(&port_file)
            .await
            .ok()
            .as_deref()
            .and_then(parse_port_file)
        {
            info!(exe, source = engine.source, "efb-browser: engine ready");
            return Ok(Launched { child, ws_url });
        }
        if tokio::time::Instant::now() >= deadline {
            let _ = child.kill().await;
            return Err(BrowserError::NoDevToolsPort {
                exe,
                secs: PORT_FILE_TIMEOUT.as_secs(),
            });
        }
        tokio::time::sleep(PORT_FILE_POLL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_file_parses_into_a_loopback_endpoint() {
        assert_eq!(
            parse_port_file("9222\n/devtools/browser/abc-123\n").as_deref(),
            Some("ws://127.0.0.1:9222/devtools/browser/abc-123")
        );
        // Half-written while the browser starts.
        assert_eq!(parse_port_file("9222\n"), None);
        assert_eq!(parse_port_file(""), None);
        assert_eq!(parse_port_file("x\n/devtools/browser/a"), None);
        assert_eq!(parse_port_file("0\n/devtools/browser/a"), None);
    }

    #[test]
    fn the_engine_never_gets_the_users_profile_or_a_fixed_port() {
        let dir = Path::new("profiles").join("persistent");
        let args: Vec<String> = args(&dir, 800, 600)
            .into_iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let profile = format!("--user-data-dir={}", dir.display());
        assert!(args.contains(&profile));
        assert!(args.iter().any(|a| a == "--remote-debugging-port=0"));
        assert!(args.iter().any(|a| a == "--headless=new"));
        assert!(args.iter().any(|a| a == "--window-size=800,600"));
        assert_eq!(args.last().map(String::as_str), Some("about:blank"));
    }
}
