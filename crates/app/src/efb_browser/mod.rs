//! The EFB's Web tab: pages rendered host-side by a real browser engine and
//! delivered to the panel as JPEG frames.
//!
//! The in-sim panel runs Coherent, a Safari-9-era engine that cannot run a
//! modern site, and an `<iframe>` is refused by framing headers anyway. So an
//! installed Chromium (Edge, Chrome or a fork) runs headless, driven over the
//! DevTools protocol: `Page.startScreencast` is a JPEG stream with ack-based
//! backpressure, and `Input.*` takes the crew's pointer and keyboard.
//!
//! The wire contract (the `browser.*` cmds and
//! `/efb/browser/{session}/frame.jpg`) belongs to `infinity-efb-server`; see
//! [`infinity_efb_server::BrowserProvider`]. This module only implements it:
//!
//! - [`engine`] finds and launches the engine;
//! - [`cdp`] is the DevTools client;
//! - [`session`] owns one engine and its page target;
//! - [`input`] queues and translates input, [`focus`] probes for text fields;
//! - [`page`] translates CDP shapes, [`policy`] decides which addresses may
//!   be opened at all.

mod cdp;
mod engine;
mod error;
mod focus;
mod input;
mod page;
mod policy;
mod session;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

use async_trait::async_trait;
use infinity_efb_server::{BrowserEmitter, BrowserProvider};
use serde_json::{Value, json};
use tracing::{info, warn};

use self::input::InputEvent;
use self::session::Session;

/// A session with no input and no frame fetch for this long is reaped: the
/// backstop for a client gone without `browser.close` (a sim crash, a panel
/// unloaded with the aeroplane).
const IDLE_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const REAP_INTERVAL: Duration = Duration::from_secs(60);

/// Locks, ignoring poisoning: every guarded value here is a plain
/// replace-on-write cell, so a panic mid-write cannot leave it inconsistent.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What the provider, the sessions' event loops and the reaper share.
struct Shared {
    emit: BrowserEmitter,
    profile_root: PathBuf,
    /// A std mutex, never held across an await, so the synchronous
    /// [`BrowserProvider::input`] can route straight to a session's queue in
    /// the order the socket delivered.
    sessions: Mutex<HashMap<String, Arc<Session>>>,
    /// Serialises session starts only, so two `browser.open`s racing on one
    /// id cannot each launch an engine, without blocking frame fetches for as
    /// long as a cold engine takes to start.
    start_lock: tokio::sync::Mutex<()>,
    /// Whether the persistent profile is held; see `session::Lease`.
    persistent_busy: Arc<AtomicBool>,
}

impl Shared {
    fn get(&self, id: &str) -> Option<Arc<Session>> {
        let found = lock(&self.sessions).get(id).cloned();
        if let Some(s) = &found {
            s.touch();
        }
        found
    }

    fn gone(&self, id: &str, reason: &str) {
        warn!(session = id, reason, "efb-browser: session unavailable");
        (self.emit)(
            "browser.gone".to_owned(),
            json!({ "session": id, "reason": reason }),
        );
    }

    /// The engine behind `session` went away. Only an engine that died on
    /// its own is reported: a deliberate close has already removed it, and
    /// matching by identity rather than id keeps a late report from an old
    /// engine from killing a newer session that reused the id.
    async fn engine_stopped(&self, id: &str, session: &Weak<Session>) {
        let removed = {
            let mut map = lock(&self.sessions);
            let ours = map
                .get(id)
                .is_some_and(|s| std::ptr::eq(Arc::as_ptr(s), session.as_ptr()));
            if ours { map.remove(id) } else { None }
        };
        if let Some(s) = removed {
            self.gone(id, "The browser engine stopped.");
            s.shutdown().await;
        }
    }
}

/// The app's [`BrowserProvider`]. One per server start; the server calls
/// [`BrowserProvider::shutdown`] when it stops.
pub(crate) struct EfbBrowser {
    shared: Arc<Shared>,
}

impl EfbBrowser {
    /// `emit` pushes a `cmd` to every client (build it over
    /// `EfbState::forward_cmd`); profiles live under `profile_root`. Must be
    /// called inside the Tokio runtime: it starts the idle reaper.
    pub(crate) fn new(emit: BrowserEmitter, profile_root: PathBuf) -> Self {
        let shared = Arc::new(Shared {
            emit,
            profile_root,
            sessions: Mutex::new(HashMap::new()),
            start_lock: tokio::sync::Mutex::new(()),
            persistent_busy: Arc::new(AtomicBool::new(false)),
        });
        tokio::spawn(reap(Arc::downgrade(&shared)));
        Self { shared }
    }

    /// `open` without the address policy, for tests that need a `data:`
    /// page; `refused` is reported once the session is up.
    async fn open_checked(
        &self,
        id: &str,
        url: &str,
        width: u32,
        height: u32,
        refused: Option<String>,
    ) {
        // Re-attach rather than restart: the EFB opens on every mount of its
        // Web tab, and switching away and back must keep the page and history.
        if let Some(existing) = self.shared.get(id) {
            existing.reattach(width, height).await;
            if let Some(error) = refused {
                existing.push_state(false, Some(&error)).await;
            }
            return;
        }
        let guard = self.shared.start_lock.lock().await;
        // A racing open may have started it while this one waited.
        if let Some(existing) = self.shared.get(id) {
            drop(guard);
            existing.reattach(width, height).await;
            return;
        }
        match session::start(&self.shared, id, url, width, height).await {
            Ok(s) => {
                drop(guard);
                if let Some(error) = refused {
                    s.push_state(false, Some(&error)).await;
                }
            }
            Err(e) => self.shared.gone(id, &e.to_string()),
        }
    }

    fn missing(&self, id: &str) {
        self.shared
            .gone(id, "That browser session is no longer open.");
    }
}

/// Closes sessions idle past [`IDLE_TIMEOUT`]; ends with the provider.
async fn reap(shared: Weak<Shared>) {
    let mut tick = tokio::time::interval(REAP_INTERVAL);
    loop {
        tick.tick().await;
        let Some(shared) = shared.upgrade() else {
            return;
        };
        let stale: Vec<(String, Arc<Session>)> = {
            let mut map = lock(&shared.sessions);
            let ids: Vec<String> = map
                .iter()
                .filter(|(_, s)| s.idle_for() > IDLE_TIMEOUT)
                .map(|(id, _)| id.clone())
                .collect();
            ids.into_iter()
                .filter_map(|id| map.remove(&id).map(|s| (id, s)))
                .collect()
        };
        for (id, session) in stale {
            info!(session = %id, "efb-browser: reaping idle session");
            session.shutdown().await;
        }
    }
}

#[async_trait]
impl BrowserProvider for EfbBrowser {
    async fn open(&self, session: &str, url: &str, width: u32, height: u32) {
        let (url, refused) = match policy::check(url) {
            Ok(url) => (url, None),
            Err(e) => (policy::BLANK.to_owned(), Some(e.to_string())),
        };
        self.open_checked(session, &url, width, height, refused)
            .await;
    }

    async fn navigate(&self, session: &str, url: &str) {
        let Some(s) = self.shared.get(session) else {
            return self.missing(session);
        };
        match policy::check(url) {
            Ok(url) => s.navigate(&url).await,
            Err(e) => s.push_state(false, Some(&e.to_string())).await,
        }
    }

    async fn action(&self, session: &str, action: &str) {
        let Some(s) = self.shared.get(session) else {
            return self.missing(session);
        };
        s.action(action).await;
    }

    fn input(&self, session: &str, event: Value) {
        // Parsed and routed inline, never awaited: see the trait.
        let Some(event) = InputEvent::parse(&event) else {
            return;
        };
        if let Some(s) = lock(&self.shared.sessions).get(session) {
            s.enqueue(event);
        }
    }

    async fn close(&self, session: &str) {
        let removed = lock(&self.shared.sessions).remove(session);
        if let Some(s) = removed {
            info!(session, "efb-browser: session closed");
            s.shutdown().await;
        }
    }

    /// `_seq` is a cache-buster, not a selector: the newest frame is never a
    /// worse answer. Fetching is what lets the engine paint the next frame.
    async fn frame(&self, session: &str, _seq: u64) -> Option<Vec<u8>> {
        self.shared.get(session)?.take_frame()
    }

    /// Concurrently: the server bounds this, and closing engines one after
    /// another could run past that and leave the rest to be killed.
    async fn shutdown(&self) {
        let sessions: Vec<Arc<Session>> = lock(&self.shared.sessions)
            .drain()
            .map(|(_, s)| s)
            .collect();
        futures_util::future::join_all(sessions.iter().map(|s| s.shutdown())).await;
    }
}

/// End-to-end against a real engine. Ignored by default: they launch a
/// browser, which CI and most `cargo test` runs should not. Run with
/// `cargo test -p infinity-app efb_browser -- --ignored`; each skips if no
/// Chromium is found (set `INFINITY_EFB_BROWSER` to point at one).
#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]
mod live_tests {
    use super::*;
    use std::time::Instant;

    type Seen = Arc<Mutex<Vec<(String, Value)>>>;

    fn provider(dir: &str) -> (EfbBrowser, Seen, PathBuf) {
        let seen: Seen = Arc::default();
        let sink = seen.clone();
        let root = std::env::temp_dir().join(dir);
        let p = EfbBrowser::new(
            Arc::new(move |name, data| lock(&sink).push((name, data))),
            root.clone(),
        );
        (p, seen, root)
    }

    /// Polls rather than sleeping a fixed time: a cold start is unpredictable.
    async fn wait_for(seen: &Seen, what: &str, pick: impl Fn(&str, &Value) -> bool) -> Value {
        for _ in 0..200 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let events = lock(seen).clone();
            if let Some(gone) = events.iter().find(|(n, _)| n == "browser.gone") {
                panic!("session did not start: {gone:?}");
            }
            if let Some((_, data)) = events.iter().find(|(n, d)| pick(n, d)) {
                return data.clone();
            }
        }
        panic!("no {what} within 20s");
    }

    #[tokio::test]
    #[ignore = "launches a real browser"]
    async fn a_session_renders_frames_and_hides_headless() {
        if engine::discover().is_none() {
            eprintln!("no Chromium found; skipping");
            return;
        }
        let (p, seen, _) = provider("infinity-efb-browser-test");
        let page = "data:text/html,<body style='background:%23112233'><h1>EFB</h1>";
        p.open_checked("test", page, 400, 300, None).await;

        let frame = wait_for(&seen, "browser.frame", |n, _| n == "browser.frame").await;
        assert_eq!(frame["w"], 400);
        let seq = frame["seq"].as_u64().expect("seq");
        let bytes = p.frame("test", seq).await.expect("a frame");
        assert_eq!(&bytes[..2], &[0xFF, 0xD8], "not a JPEG");
        // A superseded sequence still gets the newest frame, never a 404.
        assert!(p.frame("test", seq.saturating_sub(1)).await.is_some());

        let s = p.shared.get("test").expect("session");
        let tells = s
            .cdp
            .call(
                Some(&s.page),
                "Runtime.evaluate",
                json!({ "expression": "navigator.userAgent + '|' + navigator.webdriver",
                        "returnByValue": true }),
            )
            .await
            .expect("evaluate");
        let tells = tells["result"]["value"].as_str().unwrap_or("").to_owned();
        assert!(!tells.contains("Headless"), "headless UA leaked: {tells}");
        assert!(
            tells.ends_with("|false"),
            "navigator.webdriver set: {tells}"
        );
        drop(s);

        // The address policy holds on a live session.
        p.navigate("test", "file:///C:/Windows/win.ini").await;
        let refused = wait_for(&seen, "a refusal", |n, d| {
            n == "browser.state" && d.get("error").is_some()
        })
        .await;
        assert!(!refused["url"].as_str().unwrap_or("").starts_with("file:"));

        p.close("test").await;
        assert!(p.frame("test", seq).await.is_none());
        let late = lock(&seen)
            .iter()
            .filter(|(n, _)| n == "browser.gone")
            .count();
        assert_eq!(late, 0, "a deliberate close is not reported as gone");
    }

    #[tokio::test]
    #[ignore = "launches a real browser"]
    async fn focus_is_reported_and_edits_land_in_order() {
        if engine::discover().is_none() {
            eprintln!("no Chromium found; skipping");
            return;
        }
        let (p, seen, _) = provider("infinity-efb-browser-focus-test");
        let page = "data:text/html,<input id=q value='hello' \
                    style='position:absolute;left:10px;top:10px;width:300px;height:30px'>";
        p.open_checked("focus", page, 400, 300, None).await;
        wait_for(&seen, "browser.frame", |n, _| n == "browser.frame").await;

        let click = |p: &EfbBrowser| {
            p.input(
                "focus",
                json!({ "type": "mouse", "kind": "down", "x": 60, "y": 25 }),
            );
            p.input(
                "focus",
                json!({ "type": "mouse", "kind": "up", "x": 60, "y": 25 }),
            );
        };
        click(&p);
        let focus = wait_for(&seen, "browser.focus", |n, d| {
            n == "browser.focus" && d["editable"] == true
        })
        .await;
        assert_eq!(focus["value"], "hello");

        // Back to back, no waiting: only ordered dispatch makes "hello world".
        p.input("focus", json!({ "type": "edit", "del": 0, "text": " wor" }));
        p.input("focus", json!({ "type": "edit", "del": 1, "text": "rld" }));
        lock(&seen).clear();
        click(&p);
        let again = wait_for(&seen, "browser.focus after edits", |n, d| {
            n == "browser.focus" && d["editable"] == true
        })
        .await;
        assert_eq!(again["value"], "hello world");

        let started = Instant::now();
        p.shutdown().await;
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "shutdown overran"
        );
    }
}
