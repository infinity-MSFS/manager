//! Pure translation between CDP's shapes and the EFB's: navigation history,
//! the user-agent override, new-tab adoption and screencast frames. Kept
//! apart from the session so each rule can be tested without an engine.

use std::collections::HashSet;

use base64::Engine as _;
use serde_json::{Value, json};

/// JPEG quality for screencast frames: the knee where headings stop looking
/// soft without doubling the byte count.
const JPEG_QUALITY: u32 = 60;

/// `Page.getNavigationHistory`, the one call that answers url, title and both
/// history flags together, so they cannot disagree the way separate reads
/// could.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct History {
    index: usize,
    entries: Vec<Entry>,
}

#[derive(Debug, Clone, PartialEq)]
struct Entry {
    id: i64,
    url: String,
    title: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Direction {
    Back,
    Forward,
}

impl History {
    pub(crate) fn parse(v: &Value) -> Self {
        let entries: Vec<Entry> = v
            .get("entries")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .map(|e| Entry {
                        id: e.get("id").and_then(Value::as_i64).unwrap_or(-1),
                        url: str_of(e, "url"),
                        title: str_of(e, "title"),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let index = v
            .get("currentIndex")
            .and_then(Value::as_u64)
            .and_then(|i| usize::try_from(i).ok())
            .unwrap_or(0)
            .min(entries.len().saturating_sub(1));
        Self { index, entries }
    }

    /// The `browser.state` payload, minus the session.
    pub(crate) fn state(&self, loading: bool, error: Option<&str>) -> Value {
        let current = self.entries.get(self.index);
        let mut v = json!({
            "url": current.map_or("", |e| e.url.as_str()),
            "title": current.map_or("", |e| e.title.as_str()),
            "loading": loading,
            "canBack": self.index > 0,
            "canForward": self.index + 1 < self.entries.len(),
        });
        if let Some(error) = error {
            v["error"] = json!(error);
        }
        v
    }

    /// The entry id to pass to `Page.navigateToHistoryEntry`. History moves
    /// by entry id, not by a step count.
    pub(crate) fn neighbour(&self, dir: Direction) -> Option<i64> {
        let i = match dir {
            Direction::Back => self.index.checked_sub(1)?,
            Direction::Forward => self.index + 1,
        };
        self.entries.get(i).map(|e| e.id).filter(|id| *id >= 0)
    }
}

fn str_of(v: &Value, k: &str) -> String {
    v.get(k).and_then(Value::as_str).unwrap_or("").to_owned()
}

/// `Emulation.setUserAgentOverride` params from `Browser.getVersion`.
///
/// Headless puts `HeadlessChrome` in the user agent, the most-checked bot
/// signal there is and half of why search engines and Cloudflare-fronted
/// sites met v3's first Web tab with a robot check. The override is the
/// engine's own UA with that one word corrected, so the version stays true;
/// client hints are set alongside on Windows because `Sec-CH-UA` would
/// otherwise still disagree with the UA string.
pub(crate) fn user_agent_override(version: &Value, windows: bool) -> Option<Value> {
    let ua = version
        .get("userAgent")
        .and_then(Value::as_str)?
        .replace("HeadlessChrome", "Chrome");
    let full = version
        .get("product")
        .and_then(Value::as_str)
        .and_then(|p| p.split('/').nth(1))
        .unwrap_or("");
    let major = full.split('.').next().unwrap_or("");
    let mut params = json!({ "userAgent": ua, "acceptLanguage": "en-US,en;q=0.9" });
    if windows && !major.is_empty() {
        params["platform"] = json!("Win32");
        params["userAgentMetadata"] = json!({
            "brands": [
                { "brand": "Chromium", "version": major },
                { "brand": "Not_A Brand", "version": "24" },
            ],
            "fullVersionList": [
                { "brand": "Chromium", "version": full },
                { "brand": "Not_A Brand", "version": "24.0.0.0" },
            ],
            "fullVersion": full,
            "platform": "Windows",
            "platformVersion": "10.0.0",
            "architecture": "x86",
            "bitness": "64",
            "model": "",
            "mobile": false,
        });
    }
    Some(params)
}

/// What to do with a `Target.targetCreated`/`targetInfoChanged`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NewTab {
    /// Not a new page tab of ours to fold in.
    Ignore,
    /// A tab opened by `target=_blank` or `window.open`: close it and load
    /// its address in the visible tab, since the crew can only see one.
    Adopt { target: String, url: String },
}

/// `seen` holds the tabs that existed before the session (the launch tab,
/// anything a persistent profile restored) and ones already adopted. A new
/// tab is born at `about:blank` and navigated a moment later, so it is
/// adopted once it has somewhere real to go. Its opener link is lost, so a
/// sign-in popup that reports back to its opener will not work.
pub(crate) fn new_tab(params: &Value, own: &str, seen: &mut HashSet<String>) -> NewTab {
    let Some(info) = params.get("targetInfo") else {
        return NewTab::Ignore;
    };
    let (id, kind, url) = (
        str_of(info, "targetId"),
        str_of(info, "type"),
        str_of(info, "url"),
    );
    if kind != "page" || id == own || seen.contains(&id) || url.is_empty() || url == "about:blank" {
        return NewTab::Ignore;
    }
    seen.insert(id.clone());
    NewTab::Adopt { target: id, url }
}

/// A `Page.screencastFrame`: its ack token, and the JPEG if it decodes.
pub(crate) fn screencast_frame(params: &Value) -> (Option<Value>, Option<Vec<u8>>) {
    let ack = params.get("sessionId").cloned();
    let bytes = params
        .get("data")
        .and_then(Value::as_str)
        .and_then(|b64| base64::engine::general_purpose::STANDARD.decode(b64).ok());
    (ack, bytes)
}

/// Whether a `Page.frameNavigated` is the main document rather than a child
/// frame.
pub(crate) fn is_main_frame(params: &Value) -> bool {
    params
        .get("frame")
        .is_some_and(|f| f.get("parentId").is_none())
}

/// The viewport is rendered 1:1 and the client maps a click offset straight
/// to a page coordinate, so the scale factor must be exactly 1: anything else
/// silently doubles every coordinate.
pub(crate) fn metrics(width: u32, height: u32) -> Value {
    json!({ "width": width, "height": height, "deviceScaleFactor": 1, "mobile": false })
}

/// Every frame the engine paints. The rate is set by the client: each ack is
/// held until the frame is fetched, so skipping frames here would only add
/// latency to the one asked for next.
pub(crate) fn screencast(width: u32, height: u32) -> Value {
    json!({
        "format": "jpeg", "quality": JPEG_QUALITY,
        "maxWidth": width, "maxHeight": height, "everyNthFrame": 1,
    })
}

/// `errorText` on a `Page.navigate` reply (`net::ERR_NAME_NOT_RESOLVED`...).
/// The call itself succeeds when a page fails to load.
pub(crate) fn navigate_error(reply: &Value) -> Option<String> {
    reply
        .get("errorText")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn history(index: u64, n: i64) -> History {
        let entries: Vec<Value> = (0..n)
            .map(|i| json!({ "id": 10 + i, "url": format!("https://e.com/{i}"), "title": format!("T{i}") }))
            .collect();
        History::parse(&json!({ "currentIndex": index, "entries": entries }))
    }

    #[test]
    fn state_reports_the_current_entry_and_both_flags() {
        let h = history(1, 3);
        assert_eq!(
            h.state(true, None),
            json!({ "url": "https://e.com/1", "title": "T1", "loading": true,
                    "canBack": true, "canForward": true })
        );
        let first = history(0, 1).state(false, Some("net::ERR_FAILED"));
        assert_eq!(first["canBack"], false);
        assert_eq!(first["canForward"], false);
        assert_eq!(first["error"], "net::ERR_FAILED");
        assert_eq!(History::parse(&json!({})).state(false, None)["url"], "");
    }

    #[test]
    fn back_and_forward_name_entry_ids() {
        let h = history(1, 3);
        assert_eq!(h.neighbour(Direction::Back), Some(10));
        assert_eq!(h.neighbour(Direction::Forward), Some(12));
        assert_eq!(history(0, 2).neighbour(Direction::Back), None);
        assert_eq!(history(1, 2).neighbour(Direction::Forward), None);
        // An index past the end is clamped rather than trusted.
        assert_eq!(history(9, 2).neighbour(Direction::Back), Some(10));
    }

    #[test]
    fn the_headless_tell_is_removed_and_hints_agree() {
        let version = json!({
            "product": "HeadlessChrome/131.0.6778.86",
            "userAgent": "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) HeadlessChrome/131.0.6778.86 Safari/537.36",
        });
        let p = user_agent_override(&version, true).unwrap_or_default();
        let ua = p["userAgent"].as_str().unwrap_or_default();
        assert!(!ua.contains("Headless") && ua.contains("Chrome/131.0.6778.86"));
        assert_eq!(p["userAgentMetadata"]["brands"][0]["version"], "131");
        assert_eq!(p["userAgentMetadata"]["fullVersion"], "131.0.6778.86");

        let p = user_agent_override(&version, false).unwrap_or_default();
        assert!(p.get("userAgentMetadata").is_none());
        assert_eq!(user_agent_override(&json!({}), true), None);
    }

    #[test]
    fn only_real_new_page_tabs_are_adopted_once() {
        let mut seen: HashSet<String> = ["restored".to_owned()].into();
        let ev = |id: &str, kind: &str, url: &str| json!({ "targetInfo": { "targetId": id, "type": kind, "url": url } });
        assert_eq!(
            new_tab(&ev("own", "page", "https://a"), "own", &mut seen),
            NewTab::Ignore
        );
        assert_eq!(
            new_tab(&ev("restored", "page", "https://a"), "own", &mut seen),
            NewTab::Ignore
        );
        assert_eq!(
            new_tab(&ev("w", "service_worker", "https://a"), "own", &mut seen),
            NewTab::Ignore
        );
        assert_eq!(
            new_tab(&ev("t", "page", "about:blank"), "own", &mut seen),
            NewTab::Ignore
        );
        assert_eq!(
            new_tab(&ev("t", "page", "https://b/"), "own", &mut seen),
            NewTab::Adopt {
                target: "t".into(),
                url: "https://b/".into()
            }
        );
        assert_eq!(
            new_tab(&ev("t", "page", "https://c/"), "own", &mut seen),
            NewTab::Ignore
        );
        assert_eq!(new_tab(&json!({}), "own", &mut seen), NewTab::Ignore);
    }

    #[test]
    fn frames_decode_and_keep_their_ack() {
        let (ack, bytes) = screencast_frame(&json!({ "sessionId": 7, "data": "/9j/" }));
        assert_eq!(ack, Some(json!(7)));
        assert_eq!(bytes.as_deref(), Some(&[0xFF, 0xD8, 0xFF][..]));
        let (ack, bytes) = screencast_frame(&json!({ "sessionId": 8, "data": "%%" }));
        assert_eq!((ack, bytes), (Some(json!(8)), None));
    }

    #[test]
    fn small_shapes() {
        assert!(is_main_frame(&json!({ "frame": { "id": "a" } })));
        assert!(!is_main_frame(
            &json!({ "frame": { "id": "a", "parentId": "b" } })
        ));
        assert!(!is_main_frame(&json!({})));
        assert_eq!(metrics(400, 300)["deviceScaleFactor"], 1);
        assert_eq!(screencast(400, 300)["maxWidth"], 400);
        assert_eq!(
            navigate_error(&json!({ "frameId": "f", "errorText": "net::ERR_NAME_NOT_RESOLVED" }))
                .as_deref(),
            Some("net::ERR_NAME_NOT_RESOLVED")
        );
        assert_eq!(navigate_error(&json!({ "frameId": "f" })), None);
    }
}
