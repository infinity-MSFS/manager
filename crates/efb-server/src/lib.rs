//! The LAN EFB mirror server: pairing, WebSocket state sync and static hosting.
//!
//! - **Off by default.** Nothing binds until [`EfbServer::start`], which the
//!   user opts into.
//! - **Pairing-token gate.** `/ws` and the image routes need an HMAC-SHA256
//!   pairing token (`?token=`) minted from a per-install secret; loopback
//!   (the in-sim panel) may be let through without one.
//! - **One WebSocket.** `/ws` carries the `sim`, `ui` and `cmd` channels plus
//!   snapshots; the wire contract is `rescript-efb-mirror/DESIGN.md`.
//! - **Static SPA.** The mirror EFB build is served from
//!   [`EfbConfig::static_dir`], so a tablet connects by IP and port alone.
//!
//! The server owns the protocol; anything that needs an account, a network
//! service or native code (documents, flight plans, charts, the web view)
//! comes in through the traits in [`Providers`]. A missing provider turns
//! its `cmd` frames into plain rebroadcasts, which clients time out on.
//! [`sources`] has ready-made providers that need nothing but HTTP or a file.

mod cmd;
mod dto;
mod error;
mod http;
pub mod lan;
mod pairing;
mod protocol;
mod providers;
mod server;
pub mod sources;
mod state;

pub use dto::{EfbConnectedClient, EfbStatus};
pub use error::EfbError;
pub use pairing::{
    DEFAULT_TOKEN_TTL_SECS, KEYRING_PAIRING_KEY, KEYRING_SERVICE, PAIRING_CODE_LEN,
    PAIRING_CODE_TTL_SECS, PairingSecret, sign_pairing_token, verify_pairing_token,
};
pub use protocol::{Broadcast, ClientId, Incoming, Outgoing, PROTOCOL_VERSION, close_code};
pub use providers::{
    BrowserEmitter, BrowserProvider, CtlStore, DocMeta, DocProvider, FplProvider, MAX_TILE_ZOOM,
    MAX_VIEWPORT, MIN_VIEWPORT, NavProvider, ProviderError, Providers, TILE_LAYERS, TileSource,
    WxProvider,
};
pub use server::{DEFAULT_PORT, EfbConfig, EfbServer, default_bind};
pub use state::EfbState;
