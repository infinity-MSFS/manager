//! One browser session: an engine process, the page target it is attached
//! to, the newest frame, and the tasks that pump events and input.
//!
//! Ownership is what keeps engines from leaking. The provider's session map
//! holds the only long-lived `Arc<Session>`; the event loop and the ack
//! backstops hold `Weak`s. Drop the session and its `Child` goes with it
//! (`kill_on_drop`), so even a provider dropped without `shutdown` takes its
//! engines down; [`Session::shutdown`] is the graceful path that also takes
//! down the engine's child processes.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use infinity_efb_server::BrowserEmitter;
use serde_json::{Value, json};
use tokio::process::Child;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use super::cdp::{Cdp, Event};
use super::engine::{self, Engine};
use super::error::BrowserError;
use super::focus::{Focus, ProbeCause};
use super::input::{self, InputEvent};
use super::page::{self, Direction, History, NewTab};
use super::{Shared, lock, policy};

/// How long a frame's ack is held waiting for a fetch before it is released
/// anyway: the backstop through a lost notification or a vanished client,
/// and the idle rate when nothing fetches (one frame a second, not thirty).
const ACK_FALLBACK: Duration = Duration::from_secs(1);

/// How long an engine gets to close itself before it is killed. Under the
/// server's own 5 s shutdown budget, so a stop always gets to the kill.
const GRACEFUL_EXIT: Duration = Duration::from_secs(4);

/// Sub-directory of the profile root holding the EFB's persistent profile.
const PERSISTENT_PROFILE: &str = "persistent";

/// The persistent profile, held. Only one engine can use a profile directory
/// at a time (a second launch hands its command line to the first and exits),
/// so a second concurrent session gets a throwaway instead. Released on drop,
/// so no failure path can strand it.
#[derive(Debug)]
pub(super) struct Lease(Arc<AtomicBool>);

impl Lease {
    pub(super) fn take(busy: &Arc<AtomicBool>) -> Option<Self> {
        (!busy.swap(true, Ordering::SeqCst)).then(|| Self(busy.clone()))
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// Where a session's engine keeps its profile; never the user's own.
#[derive(Debug)]
enum Profile {
    /// Wiped before launch, removed after.
    Throwaway(PathBuf),
    /// Kept between sessions so cookies survive: a passed captcha's
    /// clearance, a dismissed consent banner, a sign-in. Wiping every session
    /// made every visit a first visit, half of why v3's first Web tab met a
    /// robot check on every page. `clearData` is the crew's way out.
    Persistent(#[allow(dead_code)] Lease),
}

struct Frame {
    seq: u64,
    bytes: Vec<u8>,
}

pub(super) struct Session {
    id: String,
    emit: BrowserEmitter,
    pub(super) cdp: Arc<Cdp>,
    /// The CDP session attached to the page target, not the EFB's id.
    pub(super) page: String,
    child: tokio::sync::Mutex<Child>,
    profile: Mutex<Option<Profile>>,
    /// Exactly one frame: a queue would turn a slow client into unbounded
    /// memory, and a screencast consumer wants the newest picture.
    latest: Mutex<Option<Frame>>,
    /// The ack for the newest frame, with its sequence, held until the frame
    /// is fetched. The engine paints nothing new until the last frame is
    /// acked, so acking on fetch carries the panel's pace back to the engine.
    /// v3 first acked on receipt; the engine then painted at its own rate and
    /// the panel, decoding on the sim's UI thread, spent its time starting
    /// image loads it abandoned for newer ones.
    pending_ack: Mutex<Option<(u64, Value)>>,
    viewport: Mutex<(u32, u32)>,
    last_seen: Mutex<Instant>,
    input: mpsc::UnboundedSender<InputEvent>,
    pub(super) focus: Arc<Focus>,
}

impl Session {
    pub(super) fn touch(&self) {
        *lock(&self.last_seen) = Instant::now();
    }

    pub(super) fn idle_for(&self) -> Duration {
        lock(&self.last_seen).elapsed()
    }

    /// Queue one input event. Order is the queue's; see `input.rs`.
    pub(super) fn enqueue(&self, event: InputEvent) {
        self.touch();
        let _ = self.input.send(event);
    }

    fn emit(&self, name: &str, mut data: Value) {
        data["session"] = json!(self.id);
        (self.emit)(name.to_owned(), data);
    }

    async fn call(&self, method: &str, params: Value) -> Result<Value, BrowserError> {
        Ok(self.cdp.call(Some(&self.page), method, params).await?)
    }

    /// The newest frame, releasing the engine to paint the next.
    pub(super) fn take_frame(&self) -> Option<Vec<u8>> {
        let bytes = lock(&self.latest).as_ref().map(|f| f.bytes.clone());
        self.ack(None);
        bytes
    }

    /// Release the held ack: whichever is held, or only the one for `seq`.
    fn ack(&self, seq: Option<u64>) {
        let taken = {
            let mut pending = lock(&self.pending_ack);
            match (pending.as_ref(), seq) {
                (Some((held, _)), Some(want)) if *held != want => None,
                _ => pending.take(),
            }
        };
        if let Some((_, ack)) = taken {
            self.send_ack(ack);
        }
    }

    fn send_ack(&self, ack: Value) {
        self.cdp.notify(
            Some(&self.page),
            "Page.screencastFrameAck",
            json!({ "sessionId": ack }),
        );
    }

    fn on_frame(self: &Arc<Self>, seq: &mut u64, params: &Value) {
        let (ack, bytes) = page::screencast_frame(params);
        let Some(bytes) = bytes else {
            // Nothing for a client to fetch, so nothing would ever ack it,
            // and the engine sends nothing more until it is.
            debug!("efb-browser: undecodable screencast frame");
            if let Some(ack) = ack {
                self.send_ack(ack);
            }
            return;
        };
        *seq += 1;
        let this = *seq;
        *lock(&self.latest) = Some(Frame { seq: this, bytes });
        if let Some(ack) = ack {
            *lock(&self.pending_ack) = Some((this, ack));
            let held = Arc::downgrade(self);
            tokio::spawn(async move {
                tokio::time::sleep(ACK_FALLBACK).await;
                if let Some(s) = held.upgrade() {
                    s.ack(Some(this));
                }
            });
        }
        self.announce(this);
    }

    fn announce(&self, seq: u64) {
        let (w, h) = *lock(&self.viewport);
        self.emit("browser.frame", json!({ "seq": seq, "w": w, "h": h }));
    }

    /// Reads navigation state out of the engine and sends `browser.state`.
    pub(super) async fn push_state(&self, loading: bool, error: Option<&str>) {
        match self.call("Page.getNavigationHistory", json!({})).await {
            Ok(v) => self.emit("browser.state", History::parse(&v).state(loading, error)),
            Err(e) => debug!(error = %e, "efb-browser: navigation history unavailable"),
        }
    }

    /// A client re-mounting its Web tab: it has none of what was announced
    /// before it left, so resize if needed and re-announce state, the newest
    /// frame (a static page may never paint another) and the focused field.
    /// The client's url is not reloaded; it gets the page it left.
    pub(super) async fn reattach(&self, width: u32, height: u32) {
        let resized = {
            let mut v = lock(&self.viewport);
            let changed = *v != (width, height);
            *v = (width, height);
            changed
        };
        if resized {
            let _ = self
                .call(
                    "Emulation.setDeviceMetricsOverride",
                    page::metrics(width, height),
                )
                .await;
            // The screencast is bound to the size it started at: without a
            // restart every later frame is the old shape and every click lands
            // in the wrong place.
            let _ = self.call("Page.stopScreencast", json!({})).await;
            self.ack(None);
            let _ = self
                .call("Page.startScreencast", page::screencast(width, height))
                .await;
        }
        self.push_state(false, None).await;
        let latest = lock(&self.latest).as_ref().map(|f| f.seq);
        if let Some(seq) = latest {
            self.announce(seq);
        }
        self.focus.forget();
        self.focus.probe(ProbeCause::Settle).await;
    }

    /// Navigates to an already policy-checked address.
    pub(super) async fn navigate(&self, url: &str) {
        match self.call("Page.navigate", json!({ "url": url })).await {
            Ok(reply) => {
                if let Some(error) = page::navigate_error(&reply) {
                    debug!(url, error, "efb-browser: navigation failed");
                    self.push_state(false, Some(&error)).await;
                }
            }
            Err(e) => debug!(error = %e, "efb-browser: navigate failed"),
        }
    }

    pub(super) async fn action(&self, action: &str) {
        match action {
            "reload" => {
                let _ = self.call("Page.reload", json!({})).await;
            }
            "stop" => {
                let _ = self.call("Page.stopLoading", json!({})).await;
            }
            "back" => self.go(Direction::Back).await,
            "forward" => self.go(Direction::Forward).await,
            "clearData" => self.clear_data().await,
            other => debug!(action = other, "efb-browser: unknown action"),
        }
    }

    async fn go(&self, dir: Direction) {
        let Ok(v) = self.call("Page.getNavigationHistory", json!({})).await else {
            return;
        };
        if let Some(id) = History::parse(&v).neighbour(dir) {
            let _ = self
                .call("Page.navigateToHistoryEntry", json!({ "entryId": id }))
                .await;
        }
    }

    /// The persistent profile's counterpart: cookies and cache engine-wide,
    /// then the current site's own storage (the only origin nameable without
    /// enumerating every site ever visited), then a reload to show it.
    async fn clear_data(&self) {
        let _ = self.cdp.call(None, "Storage.clearCookies", json!({})).await;
        let _ = self.call("Network.clearBrowserCache", json!({})).await;
        let origin = self
            .call(
                "Runtime.evaluate",
                json!({ "expression": "location.origin", "returnByValue": true, "silent": true }),
            )
            .await
            .ok()
            .and_then(|v| v.pointer("/result/value")?.as_str().map(str::to_owned))
            .filter(|o| o.starts_with("http"));
        if let Some(origin) = origin {
            let _ = self
                .cdp
                .call(
                    None,
                    "Storage.clearDataForOrigin",
                    json!({ "origin": origin, "storageTypes": "all" }),
                )
                .await;
        }
        info!(session = %self.id, "efb-browser: browsing data cleared");
        let _ = self.call("Page.reload", json!({})).await;
    }

    /// Stops the engine and reclaims its profile.
    pub(super) async fn shutdown(&self) {
        // `notify`: the reply races the socket closing under it.
        self.cdp.notify(None, "Browser.close", json!({}));
        stop_engine(&mut *self.child.lock().await).await;
        let profile = lock(&self.profile).take();
        if let Some(Profile::Throwaway(dir)) = profile {
            // Removable only once the whole tree has let go of its files, a
            // moment after the parent exits. A leftover is overwritten by the
            // next session on that id, so it is not worth failing over.
            for _ in 0..5 {
                if tokio::fs::remove_dir_all(&dir).await.is_ok() {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            debug!(dir = %dir.display(), "efb-browser: profile dir left behind");
        }
    }
}

/// Waits for an engine asked to close, killing it if it will not.
///
/// The order matters. A Chromium is a process tree (browser, GPU, one
/// renderer per site) and `Child::kill` reaches only the parent; asking
/// `Browser.close` then killing at once cut the polite shutdown off midway
/// and orphaned eight processes per session in v3.
async fn stop_engine(child: &mut Child) {
    if tokio::time::timeout(GRACEFUL_EXIT, child.wait())
        .await
        .is_err()
    {
        warn!("efb-browser: engine did not exit on request; killing");
        let _ = child.kill().await;
    }
}

/// Launches an engine, attaches to a fresh page and registers the session in
/// `shared`. The caller holds the start lock.
pub(super) async fn start(
    shared: &Arc<Shared>,
    id: &str,
    url: &str,
    width: u32,
    height: u32,
) -> Result<Arc<Session>, BrowserError> {
    let engine = engine::discover().ok_or(BrowserError::NoEngine)?;
    let (mut child, ws_url, profile) =
        launch_with_profile(shared, &engine, id, width, height).await?;

    let (events_tx, events_rx) = mpsc::unbounded_channel();
    let cdp = match Cdp::connect(&ws_url, events_tx).await {
        Ok(cdp) => cdp,
        Err(e) => {
            let _ = child.kill().await;
            return Err(e.into());
        }
    };
    let attached = match attach(&cdp, width, height).await {
        Ok(a) => a,
        Err(e) => {
            cdp.notify(None, "Browser.close", json!({}));
            stop_engine(&mut child).await;
            return Err(e);
        }
    };

    let focus = Arc::new(Focus::new(
        cdp.clone(),
        attached.page.clone(),
        id.to_owned(),
        shared.emit.clone(),
    ));
    let (input_tx, input_rx) = mpsc::unbounded_channel();
    input::spawn_worker(cdp.clone(), attached.page.clone(), focus.clone(), input_rx);

    let session = Arc::new(Session {
        id: id.to_owned(),
        emit: shared.emit.clone(),
        cdp,
        page: attached.page,
        child: tokio::sync::Mutex::new(child),
        profile: Mutex::new(Some(profile)),
        latest: Mutex::new(None),
        pending_ack: Mutex::new(None),
        viewport: Mutex::new((width, height)),
        last_seen: Mutex::new(Instant::now()),
        input: input_tx,
        focus,
    });
    // Registered before anything can fail it, so an engine that dies from
    // here on is noticed by the event loop and reported as `browser.gone`.
    lock(&shared.sessions).insert(id.to_owned(), session.clone());
    tokio::spawn(run_events(
        Arc::downgrade(shared),
        id.to_owned(),
        Arc::downgrade(&session),
        attached.target,
        attached.existing,
        events_rx,
    ));

    session.navigate(url).await;
    if let Err(e) = session
        .call("Page.startScreencast", page::screencast(width, height))
        .await
    {
        warn!(error = %e, "efb-browser: screencast did not start");
    }
    info!(session = id, "efb-browser: session started");
    Ok(session)
}

/// The persistent profile when it is free, else (or if that launch fails,
/// most likely an engine orphaned by a manager crash still holding the
/// directory) a throwaway one.
async fn launch_with_profile(
    shared: &Shared,
    engine: &Engine,
    id: &str,
    width: u32,
    height: u32,
) -> Result<(Child, String, Profile), BrowserError> {
    if let Some(lease) = Lease::take(&shared.persistent_busy) {
        let dir = shared.profile_root.join(PERSISTENT_PROFILE);
        match engine::launch(engine, &dir, false, width, height).await {
            Ok(l) => return Ok((l.child, l.ws_url, Profile::Persistent(lease))),
            Err(e) => {
                warn!(error = %e, "efb-browser: persistent profile unavailable; using a throwaway");
            }
        }
    }
    let dir = shared.profile_root.join(format!("session-{id}"));
    let l = engine::launch(engine, &dir, true, width, height).await?;
    Ok((l.child, l.ws_url, Profile::Throwaway(dir)))
}

struct Attached {
    target: String,
    page: String,
    /// Page targets that existed before ours; never adopted as new tabs.
    existing: HashSet<String>,
}

async fn attach(cdp: &Cdp, width: u32, height: u32) -> Result<Attached, BrowserError> {
    // A fresh target rather than the launch tab gives a clean thing to attach
    // to and close. No width/height here: headless rejects them ("Target
    // position can only be set for new windows"), and the metrics override
    // below is what sizes the viewport anyway.
    let created = cdp
        .call(None, "Target.createTarget", json!({ "url": "about:blank" }))
        .await?;
    let target = created
        .get("targetId")
        .and_then(Value::as_str)
        .ok_or(BrowserError::Protocol {
            method: "Target.createTarget",
            field: "targetId",
        })?
        .to_owned();
    // flatten: page events arrive on the same socket tagged with a sessionId,
    // rather than wrapped in Target.receivedMessageFromTarget.
    let attached = cdp
        .call(
            None,
            "Target.attachToTarget",
            json!({ "targetId": target, "flatten": true }),
        )
        .await?;
    let page = attached
        .get("sessionId")
        .and_then(Value::as_str)
        .ok_or(BrowserError::Protocol {
            method: "Target.attachToTarget",
            field: "sessionId",
        })?
        .to_owned();

    cdp.call(Some(&page), "Page.enable", json!({})).await?;
    disguise(cdp, &page).await;
    // A target created in the background is never composited, and a
    // screencast of something that never paints delivers nothing, silently.
    let _ = cdp
        .call(None, "Target.activateTarget", json!({ "targetId": target }))
        .await;
    cdp.call(
        Some(&page),
        "Emulation.setDeviceMetricsOverride",
        page::metrics(width, height),
    )
    .await?;

    let existing = cdp
        .call(None, "Target.getTargets", json!({}))
        .await
        .ok()
        .and_then(|v| {
            v.get("targetInfos").and_then(Value::as_array).map(|a| {
                a.iter()
                    .filter_map(|t| t.get("targetId")?.as_str().map(str::to_owned))
                    .collect()
            })
        })
        .unwrap_or_default();
    let _ = cdp
        .call(
            None,
            "Target.setDiscoverTargets",
            json!({ "discover": true }),
        )
        .await;
    Ok(Attached {
        target,
        page,
        existing,
    })
}

/// Best-effort: a refusal leaves the page working, just more suspicious.
/// (`navigator.webdriver` is the launch flag in `engine.rs`.)
async fn disguise(cdp: &Cdp, page: &str) {
    let Ok(version) = cdp.call(None, "Browser.getVersion", json!({})).await else {
        return;
    };
    let Some(params) = page::user_agent_override(&version, cfg!(windows)) else {
        return;
    };
    if let Err(e) = cdp
        .call(Some(page), "Emulation.setUserAgentOverride", params)
        .await
    {
        debug!(error = %e, "efb-browser: user-agent override refused");
    }
}

/// Drains one session's CDP events: frames, navigation state and focus out
/// to the EFB, and new tabs folded into the visible one.
async fn run_events(
    shared: Weak<Shared>,
    id: String,
    session: Weak<Session>,
    target: String,
    mut seen: HashSet<String>,
    mut events: mpsc::UnboundedReceiver<Event>,
) {
    let mut seq = 0u64;
    let mut loading = false;
    while let Some(event) = events.recv().await {
        let Some(s) = session.upgrade() else { break };
        // Read with a round trip, so spawned: it must not hold up the frames
        // queued behind it.
        let push_state = |loading: bool| {
            let s = s.clone();
            tokio::spawn(async move { s.push_state(loading, None).await });
        };
        match event.method.as_str() {
            "Page.screencastFrame" => s.on_frame(&mut seq, &event.params),
            "Page.frameStartedLoading" => loading = true,
            "Page.frameStoppedLoading" => {
                loading = false;
                push_state(false);
            }
            "Page.loadEventFired" => {
                loading = false;
                push_state(false);
                // An autofocused field (a search page's box) is ready for
                // text the moment the page is.
                let focus = s.focus.clone();
                tokio::spawn(async move { focus.probe(ProbeCause::Settle).await });
            }
            "Page.frameNavigated" => {
                // A new main document took the focused field with it. Cleared
                // here, in event order, so it cannot land after the new page's
                // own autofocus report.
                if page::is_main_frame(&event.params) {
                    s.focus.clear();
                }
                push_state(loading);
            }
            "Page.navigatedWithinDocument" => push_state(loading),
            "Target.targetCreated" | "Target.targetInfoChanged" => {
                if let NewTab::Adopt { target: tab, url } =
                    page::new_tab(&event.params, &target, &mut seen)
                {
                    s.cdp
                        .notify(None, "Target.closeTarget", json!({ "targetId": tab }));
                    if policy::check(&url).is_ok() {
                        s.cdp
                            .notify(Some(&s.page), "Page.navigate", json!({ "url": url }));
                    }
                }
            }
            _ => {}
        }
    }
    // The channel closing means the socket closed: the engine is gone.
    if let Some(shared) = shared.upgrade() {
        shared.engine_stopped(&id, &session).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_persistent_profile_is_held_by_one_session_and_released_on_drop() {
        let busy = Arc::new(AtomicBool::new(false));
        let first = Lease::take(&busy);
        assert!(first.is_some());
        assert!(
            Lease::take(&busy).is_none(),
            "a second session must fall back"
        );
        drop(first);
        assert!(Lease::take(&busy).is_some(), "released on drop");
    }
}
