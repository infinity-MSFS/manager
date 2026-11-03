//! Which addresses a session may be sent to.
//!
//! `browser.*` is a `cmd`, and `cmd` has no per-client authorisation: once a
//! tablet is paired it can drive the engine. The engine runs on this machine,
//! so a `file:///C:/Users/...` navigation would screencast the user's files
//! to the LAN. Only `http`, `https` and `about:blank` are let through.
//!
//! This is enforced where the EFB names an address (`browser.open`,
//! `browser.nav`) and where a new tab is folded into the visible one. Links
//! and redirects inside a page are the engine's business; Chromium already
//! refuses to let web content navigate to `file:` or `chrome:`, so the
//! entry points are the only way to reach them. Private-range hosts are NOT
//! refused: the tablet is on the LAN already, and local tools (a NAS, a
//! router page) are a legitimate thing to open.

use url::Url;

/// Longer than this is not an address anyone typed.
const MAX_URL_LEN: usize = 8 * 1024;

pub(crate) const BLANK: &str = "about:blank";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum Refused {
    #[error("Only web addresses (http and https) can be opened.")]
    Scheme,
    #[error("That address is not valid.")]
    Invalid,
}

/// The address to navigate to, normalised. Empty means a blank page, and a
/// bare `host/path` gets `https://` (the EFB does this itself, but an older
/// client or a hand-typed frame should not fail on it).
pub(crate) fn check(raw: &str) -> Result<String, Refused> {
    let s = raw.trim();
    if s.is_empty() || s.eq_ignore_ascii_case(BLANK) {
        return Ok(BLANK.to_owned());
    }
    if s.len() > MAX_URL_LEN || s.chars().any(char::is_whitespace) {
        return Err(Refused::Invalid);
    }
    let url = match Url::parse(s) {
        Ok(url) => url,
        Err(url::ParseError::RelativeUrlWithoutBase) => {
            Url::parse(&format!("https://{s}")).map_err(|_| Refused::Invalid)?
        }
        Err(_) => return Err(Refused::Invalid),
    };
    match url.scheme() {
        "http" | "https" if url.host().is_some() => Ok(url.into()),
        "http" | "https" => Err(Refused::Invalid),
        // `localhost:3000` parses as scheme `localhost`; a real scheme never
        // has a port-looking path, so read it as a host instead.
        _ if url.cannot_be_a_base() && looks_like_host_port(s) => check(&format!("https://{s}")),
        _ => Err(Refused::Scheme),
    }
}

/// `name:1234` or `name:1234/path`, which `Url` reads as scheme `name`.
fn looks_like_host_port(s: &str) -> bool {
    s.split_once(':').is_some_and(|(_, rest)| {
        let port = rest.split(['/', '?', '#']).next().unwrap_or("");
        !port.is_empty() && port.len() <= 5 && port.bytes().all(|b| b.is_ascii_digit())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_addresses_pass_and_are_normalised() {
        assert_eq!(
            check(" https://example.com ").as_deref(),
            Ok("https://example.com/")
        );
        assert_eq!(
            check("http://10.0.0.5:8080/x?y=1").as_deref(),
            Ok("http://10.0.0.5:8080/x?y=1")
        );
        assert_eq!(
            check("duckduckgo.com/?q=a").as_deref(),
            Ok("https://duckduckgo.com/?q=a")
        );
        assert_eq!(
            check("localhost:3000/app").as_deref(),
            Ok("https://localhost:3000/app")
        );
    }

    #[test]
    fn blank_and_empty_open_a_blank_page() {
        assert_eq!(check("").as_deref(), Ok(BLANK));
        assert_eq!(check("about:blank").as_deref(), Ok(BLANK));
    }

    #[test]
    fn local_and_script_schemes_are_refused() {
        for url in [
            "file:///C:/Users/me/secrets.txt",
            "FILE:///etc/passwd",
            "chrome://settings",
            "edge://settings",
            "javascript:alert(1)",
            "data:text/html,<h1>x</h1>",
            "about:settings",
            "view-source:https://example.com",
            "ftp://example.com",
            "devtools://devtools/bundled/inspector.html",
        ] {
            assert_eq!(check(url), Err(Refused::Scheme), "{url}");
        }
    }

    #[test]
    fn malformed_input_is_refused() {
        assert_eq!(check("two words"), Err(Refused::Invalid));
        assert_eq!(check("http://"), Err(Refused::Invalid));
        assert_eq!(
            check(&format!("https://e.com/{}", "a".repeat(MAX_URL_LEN))),
            Err(Refused::Invalid)
        );
    }
}
