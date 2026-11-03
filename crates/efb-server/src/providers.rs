//! What the host plugs into the server.
//!
//! The server owns every wire format (the `cmd` frames, the ui-state keys,
//! the HTTP image routes); a provider only does the work behind one. They
//! trade plain JSON where the shape belongs to the EFB client, so this crate
//! needs no SimBrief, Navigraph, PDF or browser code. Each is optional: with
//! none wired, its `cmd`s are rebroadcast unanswered and its routes 404.
//!
//! All but [`BrowserProvider::input`] are called on spawned tasks, so they
//! may take their time.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde::Serialize;
use serde_json::{Value, json};

/// A failure shipped to the client verbatim as `{ code, message }`. `code`
/// is the provider's vocabulary (`pilot_id_missing`, `network`,
/// `bad_response`...), which the EFB matches on.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{code}: {message}")]
pub struct ProviderError {
    pub code: String,
    pub message: String,
}

impl ProviderError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }

    pub(crate) fn to_json(&self) -> Value {
        json!({ "code": self.code, "message": self.message })
    }
}

/// One document in the `doc.catalog` frame. Page counts and download state
/// travel separately in `doc.status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DocMeta {
    pub id: String,
    pub title: String,
    pub category: String,
}

/// Documents for the Docs tab (`doc.list`, `doc.open`,
/// `/efb/doc/{id}/page/{n}`).
#[async_trait]
pub trait DocProvider: Send + Sync {
    async fn list(&self) -> Result<Vec<DocMeta>, ProviderError>;

    /// Makes the document available and returns its page count. Must be
    /// quick (download if remote, read the count); pages render lazily in
    /// [`Self::page`]. Idempotent.
    async fn ensure(&self, id: &str) -> Result<usize, ProviderError>;

    /// The JPEG for 0-based `page`, rendered on demand and cached, so a huge
    /// PDF opens instantly and only viewed pages cost anything. JPEG because
    /// the in-sim browser cannot decode WebP.
    async fn page(&self, id: &str, page: usize) -> Result<PathBuf, ProviderError>;
}

/// SimBrief flight plans (`fpl.status`, `fpl.fetch`, `fpl.setPilotId`).
#[async_trait]
pub trait FplProvider: Send + Sync {
    /// `{ "pilot_id_set": bool, "pilot_id": string|null }`, sent as
    /// `fpl.pilotId`.
    async fn status(&self) -> Value;

    /// The latest OFP as SimBrief wire JSON (`{version, ofp_id, origin,
    /// destination, waypoints}`, the shape the CommBus path ships). The
    /// server stamps `fetched_at` and publishes it as the `fplPlan` ui key.
    async fn fetch(&self) -> Result<Value, ProviderError>;

    /// `None` clears.
    async fn set_pilot_id(&self, pilot_id: Option<String>) -> Result<(), ProviderError>;
}

/// Station weather (`wx.fetch`).
#[async_trait]
pub trait WxProvider: Send + Sync {
    /// Current METAR/TAF for upper-cased ICAO ids as a JSON array; unknown
    /// ids are just absent. Published as the `wxData` ui key, wrapped with
    /// the requested ids and `fetched_at`.
    async fn fetch(&self, ids: Vec<String>) -> Result<Value, ProviderError>;
}

/// Host-side storage for the aircraft's controls bindings (`ctl.load`,
/// `ctl.save`). The WASM holds them in memory only, since sim-side file IO
/// is unreliable. The blob is opaque `{version, bindings}` JSON; the WASM
/// validates it on restore.
#[async_trait]
pub trait CtlStore: Send + Sync {
    /// `Value::Null` when nothing is stored.
    async fn load(&self) -> Value;
    async fn save(&self, blob: Value) -> Result<(), ProviderError>;
}

/// Airport charts (`nav.status`, `nav.charts`, `/efb/nav/chart/{icao}/{file}`).
#[async_trait]
pub trait NavProvider: Send + Sync {
    /// `{ linked, relink_required, session_live }`, sent as `nav.info`.
    async fn status(&self) -> Value;

    /// `{ "charts": [...] }` for an upper-cased ICAO; see
    /// [`crate::sources::charts_index_for_efb`] for the entry shape.
    async fn charts_index(&self, icao: &str) -> Result<Value, ProviderError>;

    /// PNG bytes of one chart image named in the index.
    async fn chart(&self, icao: &str, filename: &str) -> Result<Vec<u8>, ProviderError>;
}

/// Layers the map may ask for.
pub const TILE_LAYERS: [&str; 5] = ["dark", "sat", "vfr", "ifr.lo", "ifr.hi"];
pub const MAX_TILE_ZOOM: u8 = 18;

/// Map tiles (`/efb/nav/tile/{layer}/{z}/{x}/{y}`). Separate from
/// [`NavProvider`] so a build without a charts account still gets maps; see
/// [`crate::sources::PublicTiles`].
#[async_trait]
pub trait TileSource: Send + Sync {
    /// PNG bytes. `layer` is one of [`TILE_LAYERS`], `z <= MAX_TILE_ZOOM`.
    async fn tile(&self, layer: &str, z: u8, x: u32, y: u32) -> Result<Vec<u8>, ProviderError>;
}

/// Largest viewport a client may ask for. Sessions render 1:1, so a silly
/// size becomes a silly framebuffer.
pub const MAX_VIEWPORT: u32 = 4096;
/// Below this the client measured its pane before layout ran.
pub const MIN_VIEWPORT: u32 = 64;

/// How a [`BrowserProvider`] pushes `cmd`s (`browser.frame`, `.state`,
/// `.focus`, `.gone`) to clients: `(name, data)`. The host builds it as a
/// closure over [`crate::EfbState::forward_cmd`], which keeps this crate's
/// delivery out of the provider and lets tests record what was pushed.
pub type BrowserEmitter = Arc<dyn Fn(String, Value) + Send + Sync>;

/// A web page rendered by a real engine host-side and delivered as JPEG
/// frames (`browser.*`, `/efb/browser/{session}/frame.jpg`). The in-sim
/// panel's engine cannot run modern sites and an iframe is refused by
/// framing headers, hence pixels.
///
/// Frames arrive unprompted, so unlike the other providers this one is
/// handed a way to push, a [`BrowserEmitter`].
/// Frames themselves go over HTTP, never the socket, which is JSON text and
/// re-serialised per client.
#[async_trait]
pub trait BrowserProvider: Send + Sync {
    /// Start or re-point a session. Idempotent per session id: a remounted
    /// Web tab must get the session it had, not a second engine.
    async fn open(&self, session: &str, url: &str, width: u32, height: u32);

    /// A no-op for an unknown session; the client learns of the loss from
    /// `browser.gone` instead.
    async fn navigate(&self, session: &str, url: &str);

    /// `"back" | "forward" | "reload" | "stop" | "clearData"`.
    async fn action(&self, session: &str, action: &str);

    /// One input event: the client's `browser.input` payload.
    ///
    /// Synchronous and must only enqueue. Input is order-sensitive and the
    /// server calls this inline from the socket's read loop, the last place
    /// the order is known; spawning per event once let releases overtake
    /// presses and typed text arrive shuffled.
    fn input(&self, session: &str, event: Value);

    async fn close(&self, session: &str);

    /// The newest frame, or `None` if the session has none. `seq` is a
    /// cache-buster, not a selector: a 404 for a superseded frame would put
    /// a broken-image icon in the viewport, and a newer frame is never a
    /// worse answer.
    async fn frame(&self, session: &str, seq: u64) -> Option<Vec<u8>>;

    /// Drop every session; called on server stop so no engine outlives it.
    async fn shutdown(&self);
}

/// Everything plugged in. Clone shares the providers.
#[derive(Clone, Default)]
pub struct Providers {
    pub docs: Option<Arc<dyn DocProvider>>,
    pub fpl: Option<Arc<dyn FplProvider>>,
    pub wx: Option<Arc<dyn WxProvider>>,
    pub ctl: Option<Arc<dyn CtlStore>>,
    pub nav: Option<Arc<dyn NavProvider>>,
    pub tiles: Option<Arc<dyn TileSource>>,
    pub browser: Option<Arc<dyn BrowserProvider>>,
}

impl std::fmt::Debug for Providers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Providers")
            .field("docs", &self.docs.is_some())
            .field("fpl", &self.fpl.is_some())
            .field("wx", &self.wx.is_some())
            .field("ctl", &self.ctl.is_some())
            .field("nav", &self.nav.is_some())
            .field("tiles", &self.tiles.is_some())
            .field("browser", &self.browser.is_some())
            .finish()
    }
}
