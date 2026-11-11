//! The EFB mirror server, as the app runs it: always on loopback for the
//! in-sim panel, on the network when the user shares it with tablets.

use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use infinity_efb_server::charts::{
    ChartEntry, ChartImage, ChartProvider, ChartSource, SourceState, codes,
};
use infinity_efb_server::sources::{AviationWeather, FileCtlStore, PublicTiles, navigraph_entries};
use infinity_efb_server::{
    DEFAULT_PORT, EfbConfig, EfbServer, FplProvider, PairingSecret, ProviderError, Providers,
};
use infinity_ipc::dto::Preferences;
use infinity_ipc::events::{EfbChanged, PreferencesChanged};
use infinity_ipc::{IpcError, IpcResult};
use infinity_persistence::PreferencesStore;
use infinity_simbrief::SimBriefClient;
use infinity_simbrief::wire::{self, Surface, WireError};
use serde_json::Value;
use tauri::Manager;
use tokio::sync::Mutex;

use crate::commands::navigraph::NavigraphState;
use crate::events::emit;

const PAIRING_KEY: &str = "efb-pairing-secret";

#[derive(Debug)]
pub(crate) struct Efb {
    pub(crate) server: EfbServer,
    /// Serialises restarts.
    apply: Mutex<()>,
}

impl Efb {
    pub(crate) fn new() -> Self {
        Self {
            server: EfbServer::new(),
            apply: Mutex::new(()),
        }
    }
}

fn bind(lan: bool) -> SocketAddr {
    let ip = if lan {
        Ipv4Addr::UNSPECIFIED
    } else {
        Ipv4Addr::LOCALHOST
    };
    SocketAddr::from((ip, DEFAULT_PORT))
}

/// Starts the server, or rebinds it when sharing changed. Logs rather than
/// fails: a port in use must not stop the app.
pub(crate) async fn apply(app: &tauri::AppHandle, prefs: &Preferences) {
    if let Err(e) = try_apply(app, prefs).await {
        tracing::warn!(error = %e, "EFB server not started");
    }
    let efb = app.state::<Efb>();
    emit::<EfbChanged>(app, &efb.server.status().await);
}

async fn try_apply(app: &tauri::AppHandle, prefs: &Preferences) -> IpcResult<()> {
    let efb = app.state::<Efb>();
    let _guard = efb.apply.lock().await;
    let wanted = bind(prefs.efb_lan_share);
    if efb.server.bind_addr().await == Some(wanted) {
        return Ok(());
    }
    if efb.server.is_running().await {
        efb.server.stop().await?;
    }
    let mut config = EfbConfig::new(pairing_secret(app)?);
    config.bind = wanted;
    config.static_dir = static_dir(app);
    config.providers = providers(app)?;
    let addr = efb.server.start(config).await?;
    tracing::info!(%addr, "EFB server listening");
    Ok(())
}

/// The secret pairing tokens are signed with, created on first use. Kept
/// in the OS keyring under the app's identifier, so the official build
/// keeps v3's pairings.
fn pairing_secret(app: &tauri::AppHandle) -> IpcResult<PairingSecret> {
    let entry = keyring_entry(app)?;
    match entry.get_password() {
        Ok(hex) => Ok(PairingSecret::from_hex(&hex)?),
        Err(keyring::Error::NoEntry) => {
            let secret = PairingSecret::generate();
            entry
                .set_password(&secret.to_hex())
                .map_err(|e| IpcError::internal(format!("keyring write: {e}")))?;
            Ok(secret)
        }
        Err(e) => Err(IpcError::internal(format!("keyring read: {e}"))),
    }
}

/// Replaces the secret, so every paired tablet must pair again.
pub(crate) async fn forget_devices(app: &tauri::AppHandle) -> IpcResult<()> {
    let secret = PairingSecret::generate();
    keyring_entry(app)?
        .set_password(&secret.to_hex())
        .map_err(|e| IpcError::internal(format!("keyring write: {e}")))?;
    app.state::<Efb>().server.rotate_secret(secret).await;
    Ok(())
}

fn keyring_entry(app: &tauri::AppHandle) -> IpcResult<keyring::Entry> {
    keyring::Entry::new(&app.config().identifier, PAIRING_KEY)
        .map_err(|e| IpcError::internal(format!("keyring: {e}")))
}

/// Where the user drops their own PDFs for the EFB.
pub(crate) fn user_docs_dir(app_data: &Path) -> PathBuf {
    app_data.join("efb-docs").join("user")
}

fn static_dir(app: &tauri::AppHandle) -> Option<PathBuf> {
    app.path()
        .resolve("resources/efb-static", tauri::path::BaseDirectory::Resource)
        .ok()
        .filter(|p| p.join("index.html").is_file())
}

fn providers(app: &tauri::AppHandle) -> IpcResult<Providers> {
    let http = reqwest::Client::builder()
        .user_agent(concat!("infinity-manager/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|e| IpcError::internal(format!("http client: {e}")))?;
    let app_data = app
        .path()
        .app_data_dir()
        .map_err(|e| IpcError::internal(format!("app data dir: {e}")))?;
    let cache = app
        .path()
        .app_cache_dir()
        .map_err(|e| IpcError::internal(format!("cache dir: {e}")))?;
    let pdfium = app
        .path()
        .resolve("resources", tauri::path::BaseDirectory::Resource)
        .ok();
    let content = app
        .state::<crate::state::AppState>()
        .providers
        .content
        .clone();
    // The Web tab's engine never runs on the user's own browser profile;
    // its profiles live here. Pushes go to every client, which filter on
    // their session id.
    let state = app.state::<Efb>().server.state();
    let browser = crate::efb_browser::EfbBrowser::new(
        Arc::new(move |name, data| state.forward_cmd(name, data, None)),
        app_data.join("efb-browser"),
    );
    Ok(Providers {
        browser: Some(Arc::new(browser)),
        docs: Some(Arc::new(crate::efb_docs::Docs::new(
            content,
            cache.join("efb-docs"),
            user_docs_dir(&app_data),
            pdfium,
        ))),
        fpl: Some(Arc::new(SimBriefPlans(app.clone()))),
        charts: vec![Arc::new(NavigraphCharts(app.clone()))],
        presence: Some(
            app.state::<Arc<crate::efb_presence::ChartsPresence>>()
                .inner()
                .clone(),
        ),
        wx: Some(Arc::new(AviationWeather::new(http.clone()))),
        tiles: Some(Arc::new(PublicTiles::new(http))),
        ctl: Some(Arc::new(FileCtlStore::in_app_data(Path::new(&app_data)))),
    })
}

/// The EFB's flight-plan tab, backed by SimBrief and the saved pilot ID.
struct SimBriefPlans(tauri::AppHandle);

impl SimBriefPlans {
    async fn pilot_id(&self) -> Option<String> {
        let prefs = self.0.state::<PreferencesStore>();
        prefs.load().await.ok().and_then(|p| p.simbrief_pilot_id)
    }
}

fn provider_error(e: WireError) -> ProviderError {
    ProviderError::new(e.code, e.message)
}

#[async_trait]
impl FplProvider for SimBriefPlans {
    async fn status(&self) -> Value {
        wire::efb_status(self.pilot_id().await.as_deref())
    }

    async fn fetch(&self) -> Result<Value, ProviderError> {
        let client = self.0.state::<SimBriefClient>();
        let plan = wire::fetch_plan(&client, self.pilot_id().await.as_deref())
            .await
            .map_err(|e| provider_error(WireError::from_error(&e, Surface::Efb)))?;
        serde_json::to_value(plan).map_err(|e| ProviderError::new("internal", e.to_string()))
    }

    async fn set_pilot_id(&self, pilot_id: Option<String>) -> Result<(), ProviderError> {
        let pilot_id = wire::normalize_pilot_id(pilot_id)
            .map_err(|e| provider_error(WireError::from_error(&e, Surface::Efb)))?;
        let store = self.0.state::<PreferencesStore>();
        let saved = async {
            let mut prefs = store.load().await?;
            prefs.simbrief_pilot_id = pilot_id;
            store.save(&prefs).await?;
            Ok::<_, IpcError>(prefs)
        }
        .await
        .map_err(|e| ProviderError::new("internal", e.message))?;
        emit::<PreferencesChanged>(&self.0, &saved);
        Ok(())
    }
}

/// The EFB's Navigraph charts, from the account linked in the manager. Served
/// only through the server's sim-session lease; nothing here may bypass it.
struct NavigraphCharts(tauri::AppHandle);

/// What the EFB credits under every Navigraph chart.
const NAVIGRAPH_ATTRIBUTION: &str = "Charts \u{a9} Navigraph. Not for real-world navigation.";

fn nav_error(e: infinity_navigraph::NavigraphError) -> ProviderError {
    use infinity_navigraph::NavigraphError as E;
    let code = match &e {
        E::Api { status: 429, .. } => codes::RATE_LIMITED,
        E::Api { status: 404, .. } => codes::UNKNOWN_IMAGE,
        E::Api { status, .. } if *status >= 500 => codes::NETWORK,
        // Timeouts and refused connections: the hub retries these once.
        E::Network(_) => codes::NETWORK,
        other => other.reason(),
    };
    ProviderError::new(code, e.to_string())
}

impl NavigraphCharts {
    fn account(&self) -> Result<&infinity_navigraph::Navigraph, ProviderError> {
        self.0
            .state::<NavigraphState>()
            .inner()
            .account()
            .map_err(|e| ProviderError::new(e.code.to_string(), e.message))
    }
}

#[async_trait]
impl ChartProvider for NavigraphCharts {
    fn source(&self) -> ChartSource {
        ChartSource {
            id: "navigraph".into(),
            name: "Navigraph".into(),
            attribution: NAVIGRAPH_ATTRIBUTION.into(),
        }
    }

    async fn status(&self) -> SourceState {
        use infinity_navigraph::NavigraphError as E;
        let Ok(account) = self.account() else {
            return SourceState::Unavailable {
                message: "This build cannot link a Navigraph account.".into(),
            };
        };
        let s = account.status().await;
        if let Some(message) = s.vault_error {
            return SourceState::Unavailable { message };
        }
        if s.relink_required {
            return SourceState::RelinkRequired;
        }
        if !s.linked {
            return SourceState::NotLinked;
        }
        match account.bearer().await {
            Ok(bearer) => match infinity_navigraph::token_subscriptions(&bearer) {
                Some(subs) if !subs.iter().any(|s| s == "charts") => SourceState::NoSubscription,
                _ => SourceState::Ready,
            },
            Err(E::RelinkRequired) => SourceState::RelinkRequired,
            Err(E::NotLinked) => SourceState::NotLinked,
            Err(e) => SourceState::Unavailable {
                message: e.to_string(),
            },
        }
    }

    async fn index(&self, icao: &str) -> Result<Vec<ChartEntry>, ProviderError> {
        let raw = self
            .account()?
            .charts_index(icao)
            .await
            .map_err(nav_error)?;
        Ok(navigraph_entries(&raw))
    }

    async fn image(&self, icao: &str, key: &str) -> Result<ChartImage, ProviderError> {
        let bytes = self
            .account()?
            .chart_bytes(icao, key)
            .await
            .map_err(nav_error)?;
        Ok(ChartImage {
            content_type: sniff_image(&bytes),
            bytes,
        })
    }
}

/// Navigraph serves PNG; sniffed rather than assumed so a format change
/// cannot hand the panel a body labelled wrongly.
fn sniff_image(bytes: &[u8]) -> &'static str {
    match bytes {
        [0xFF, 0xD8, 0xFF, ..] => "image/jpeg",
        _ => "image/png",
    }
}

/// Relays server changes (start, stop, a tablet joining) to the UI.
pub(crate) fn forward_changes(app: &tauri::AppHandle) {
    let mut changes = app.state::<Efb>().server.subscribe_changes();
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        while changes.changed().await.is_ok() {
            let status = app.state::<Efb>().server.status().await;
            emit::<EfbChanged>(&app, &status);
        }
    });
}
