use serde::{Deserialize, Serialize};

use crate::IpcError;
use crate::catalog::ProductId;

/// Indicator of what version the manager binary is
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Edition {
    /// Built from public repo free, doesnt contain any store or DRM providers thus making it not useful for payware
    Community,
    /// Official signed binary
    Official,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    pub product_name: String,
    pub version: String,
    pub edition: Edition,
    /// windows, linux
    pub os: String,
    pub arch: String,
}

/// Build capabilities
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    /// Sign-in is available
    pub accounts: bool,
    /// Paid products can be entitled and downloaded
    pub store: bool,
    /// In-sim DRM handshakes are served
    pub drm: bool,
    /// Update channels this build may switch between
    pub channels: Vec<Channel>,
    /// A Navigraph account can be linked for charts and navdata.
    #[serde(default)]
    pub navigraph: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    #[default]
    Stable,
    Beta,
    Nightly,
}

impl Channel {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Beta => "beta",
            Self::Nightly => "nightly",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "state")]
pub enum SessionState {
    SignedOut,
    Pending,
    SignedIn { profile: UserProfile },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserProfile {
    pub subject: String,
    pub name: Option<String>,
    pub email: Option<String>,
    pub picture: Option<String>,
    #[serde(default)]
    pub permissions: Vec<String>,
}

impl UserProfile {
    pub fn has_permission(&self, permission: &str) -> bool {
        self.permissions.iter().any(|p| p == permission)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginStarted {
    pub authorize_url: String,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Preferences {
    #[serde(default)]
    pub motion: MotionPreference,
    #[serde(default, skip_serializing_if = "Theme::is_default")]
    pub theme: Theme,
    /// `#rrggbb` the custom theme's background is drawn from. Same key as v3.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme_accent: Option<String>,
    /// `#rrggbb` for buttons, highlights and links. `None` is the brand
    /// emerald. Same key as v3.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accent_color: Option<String>,
    /// Community folder products install into. `None` follows the sim.
    /// Same key as v3.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install_dir: Option<String>,
    /// A `WASM/MSFS20xx` folder, for installs discovery cannot find.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub msfs_work_root: Option<String>,
    /// Linux: the Proton/Wine prefix the sim runs in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wine_prefix: Option<String>,
    /// Release channel the library shows. Same key as v3.
    #[serde(default)]
    pub channel: Channel,
    /// Serve the EFB to tablets on the local network, not just the sim.
    /// Same key as v3.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub efb_lan_share: bool,
    /// SimBrief pilot ID (the numeric user number). Same key as v3.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub simbrief_pilot_id: Option<String>,
    /// Where liveries install. `None` installs them beside aircraft.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub livery_install_dir: Option<String>,
    /// The first-run wizard was finished or skipped. Same key as v3.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub first_run_complete: bool,
    #[serde(flatten)]
    pub rest: serde_json::Map<String, serde_json::Value>,
}

/// The app is always dark. v3's `system` and `light` read as the default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    /// The brand gradient.
    #[default]
    #[serde(alias = "system", alias = "light")]
    Dark,
    /// True black, nothing animated: kind to OLED panels.
    Amoled,
    /// A gradient drawn from [`Preferences::theme_accent`].
    Custom,
}

impl Theme {
    pub fn is_default(&self) -> bool {
        *self == Self::Dark
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MotionPreference {
    #[default]
    Auto,
    Full,
    Reduced,
}

impl MotionPreference {
    pub const fn reduce(self, sim_running: bool) -> bool {
        match self {
            Self::Auto => sim_running,
            Self::Full => false,
            Self::Reduced => true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SimStatus {
    pub running: Option<Sim>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sim {
    Msfs2020,
    Msfs2024,
}

impl Sim {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Msfs2024 => "MSFS 2024",
            Self::Msfs2020 => "MSFS 2020",
        }
    }
}

/// How a sim was installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Storefront {
    MicrosoftStore,
    Steam,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SimInstall {
    pub sim: Sim,
    pub storefront: Storefront,
    pub community: Option<String>,
    pub wasm_root: Option<String>,
    /// Linux: the Proton/Wine prefix the sim runs in.
    pub wine_prefix: Option<String>,
}

/// What the manager knows about sims on this machine.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SimPaths {
    pub installs: Vec<SimInstall>,
    /// Where products install, or `None` when neither a preference nor a
    /// discovered sim says.
    pub install_dir: Option<InstallDir>,
}

/// What a folder picked as the MSFS work folder holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkRootProbe {
    /// The folder that would be searched: the pick, or its `MSFS20xx` child.
    /// `None` when the pick is not a folder.
    pub resolved: Option<String>,
    pub sim: Option<Sim>,
    /// Package folders with a `work` folder inside.
    pub packages: Vec<String>,
}

/// What a Proton/Wine prefix holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrefixProbe {
    pub prefix: String,
    /// `drive_c` exists.
    pub is_prefix: bool,
    pub installs: Vec<SimInstall>,
}

/// Opens the system folder picker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PickFolder {
    pub title: String,
    /// Where the picker opens.
    pub start: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallDir {
    pub path: String,
    /// `true` when it comes from [`Preferences::install_dir`].
    pub chosen: bool,
    /// `false` when a chosen folder has gone missing (an unplugged drive).
    pub exists: bool,
}

/// A product present in the install folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstalledPackage {
    pub product_id: ProductId,
    /// `None` when a package found on disk does not say (a manual or
    /// pre-manager install).
    pub version: Option<String>,
    pub path: String,
    pub package_dir_name: String,
    /// Seconds since the Unix epoch.
    pub installed_at: Option<u64>,
}

/// Job and installed-record id of a livery, shared with v3:
/// `livery:<aircraft product id>:<livery id>`.
pub fn livery_job_id(product_id: &str, livery_id: &str) -> String {
    format!("{LIVERY_PREFIX}{product_id}:{livery_id}")
}

/// The aircraft and livery ids in a [`livery_job_id`].
pub fn parse_livery_job_id(id: &str) -> Option<(&str, &str)> {
    id.strip_prefix(LIVERY_PREFIX)?.split_once(':')
}

const LIVERY_PREFIX: &str = "livery:";

/// Starts or resumes installing a product from a channel's catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallRequest {
    pub product_id: ProductId,
    pub channel: Channel,
}

/// An install in flight, stopped, or just finished. One per product.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    pub product_id: ProductId,
    pub name: String,
    pub version: String,
    pub channel: Channel,
    pub phase: JobPhase,
    /// Bytes downloaded while downloading or verifying; bytes written while
    /// extracting a zip, or compressed bytes read while extracting a
    /// `.tar.zst` (a stream does not declare its unpacked size). A job
    /// restored as paused after a restart carries the bytes on disk.
    pub progress: Option<Progress>,
}

impl Job {
    /// Paused, failed or cancelled. A new install request takes it over.
    pub fn is_stopped(&self) -> bool {
        matches!(
            self.phase,
            JobPhase::Paused | JobPhase::Failed { .. } | JobPhase::Cancelled
        )
    }

    /// Queued or working.
    pub fn is_active(&self) -> bool {
        !self.is_stopped() && self.phase != JobPhase::Completed
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum JobPhase {
    /// Waiting for another install to finish.
    Queued,
    Downloading,
    Verifying,
    Extracting,
    /// Swapping the new package into the install folder.
    Installing,
    /// Stopped by the user; the partial download is kept.
    Paused,
    Cancelled,
    Failed {
        error: IpcError,
    },
    Completed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub done: u64,
    pub total: Option<u64>,
}

impl Progress {
    /// Completion in `0.0..=1.0`, when the total is known.
    #[allow(clippy::cast_precision_loss)]
    pub fn fraction(self) -> Option<f64> {
        let total = self.total.filter(|t| *t > 0)?;
        Some((self.done as f64 / total as f64).clamp(0.0, 1.0))
    }
}

/// A product's saved configurator values. Also the on-disk and share-file
/// format, shared with v3.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigEnvelope {
    pub schema_version: u32,
    pub product_id: ProductId,
    /// Whatever the product's schema produces.
    pub value: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadedConfig {
    pub envelope: ConfigEnvelope,
    /// The stored value was migrated (and saved back) on this load.
    pub migrated: bool,
}

/// What a configurator output did.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum EmitResult {
    #[serde(rename_all = "camelCase")]
    File { path: String },
    #[serde(rename_all = "camelCase")]
    Bridge {
        namespace: String,
        command: String,
        response: serde_json::Value,
    },
}

/// A gauge connected to, or every gauge left, a bridge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionChange {
    pub namespace: String,
    pub connected: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeStatus {
    pub id: String,
    pub namespace: String,
    pub port: u16,
    /// A relay's socket is open. True from the moment the panel loads.
    pub connected: bool,
    /// A relay reported the module behind it reachable.
    pub ready: bool,
}

/// An event a gauge sent over a bridge.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeMessage {
    pub namespace: String,
    pub name: String,
    pub data: serde_json::Value,
}

/// A request/response command to the gauges on a bridge.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeCommand {
    pub namespace: String,
    pub command: String,
    #[serde(default)]
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EfbConnectedClient {
    /// Pass to [`crate::EfbServer::disconnect_client`].
    pub client_id: u64,
    /// The device name from its pairing token, or `loopback`.
    pub device: String,
    pub addr: String,
    pub age_secs: u64,
    /// The in-sim panel, or a browser on this machine. Survives "forget all
    /// devices".
    pub loopback: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EfbStatus {
    pub running: bool,
    /// Where it is listening, e.g. `0.0.0.0:28012`.
    pub bind_addr: Option<String>,
    pub clients: Vec<EfbConnectedClient>,
}

/// Names a product in a channel's catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProductRef {
    pub product_id: ProductId,
    pub channel: Channel,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfiguratorSave {
    pub product: ProductRef,
    pub envelope: ConfigEnvelope,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfiguratorEmit {
    pub product: ProductRef,
    pub output_id: String,
}

/// How the launch supervisor (which starts the manager when an in-sim
/// aircraft asks for it) is implemented here. Same values as v3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SupervisorMechanism {
    /// The `InfinityLaunchSvc` Windows service. Changes need elevation.
    WindowsService,
    /// systemd user units. No elevation.
    SystemdUser,
    /// Nothing on this platform.
    None,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SupervisorStatus {
    /// This platform has a supervisor at all.
    pub supported: bool,
    pub mechanism: SupervisorMechanism,
    pub installed: bool,
    pub running: bool,
    /// The service binary on Windows, the unit folder on Linux.
    pub exe_path: Option<String>,
    /// What the installed copy reports. `None` when not installed or it
    /// predates versioning; `"stale"` for systemd units that point at
    /// another manager binary.
    pub installed_version: Option<String>,
    /// The version this manager would install.
    pub bundled_version: String,
    /// This build can install the supervisor. `false` for Windows builds
    /// made without the service binary (`cargo xtask dist` bundles it).
    pub bundled: bool,
}

impl SupervisorStatus {
    /// Installed, and not the copy this build would install.
    pub fn update_available(&self) -> bool {
        self.installed
            && self.bundled
            && self.installed_version.as_deref() != Some(self.bundled_version.as_str())
    }
}

/// What importing v1's settings did. Kept on disk, so asking again
/// returns the first run's answer.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct V1Migration {
    /// v1's data folder.
    pub source: String,
    pub ran_at: String,
    /// Products whose configuration was imported.
    pub migrated_configs: Vec<String>,
    pub skipped: Vec<String>,
    pub errors: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preferences_keep_unknown_v3_fields() {
        let v3 = serde_json::json!({
            "channel": "beta",
            "theme": "amoled",
            "installDir": "D:\\Community",
            "firstRunComplete": true
        });
        let prefs: Preferences = serde_json::from_value(v3.clone()).unwrap();
        assert_eq!(prefs.motion, MotionPreference::Auto);

        let mut out = serde_json::to_value(&prefs).unwrap();
        out.as_object_mut().unwrap().remove("motion");
        assert_eq!(out, v3);
    }

    #[test]
    fn auto_motion_follows_the_sim() {
        assert!(MotionPreference::Auto.reduce(true));
        assert!(!MotionPreference::Auto.reduce(false));
        assert!(MotionPreference::Reduced.reduce(false));
        assert!(!MotionPreference::Full.reduce(true));
    }

    #[test]
    fn v3_themes_map_onto_dark_amoled_custom() {
        let theme = |v: &str| {
            serde_json::from_value::<Preferences>(serde_json::json!({ "theme": v }))
                .unwrap()
                .theme
        };
        assert_eq!(theme("system"), Theme::Dark);
        assert_eq!(theme("light"), Theme::Dark);
        assert_eq!(theme("amoled"), Theme::Amoled);
        assert_eq!(theme("custom"), Theme::Custom);
        let out = serde_json::to_value(Preferences::default()).unwrap();
        assert!(out.get("theme").is_none());
    }

    #[test]
    fn v3_paths_are_typed_and_absent_ones_stay_absent() {
        let prefs: Preferences =
            serde_json::from_value(serde_json::json!({ "installDir": "D:\\Community" })).unwrap();
        assert_eq!(prefs.install_dir.as_deref(), Some("D:\\Community"));
        assert!(prefs.rest.is_empty());
        let out = serde_json::to_value(&prefs).unwrap();
        assert!(out.get("winePrefix").is_none());
    }

    #[test]
    fn livery_ids_round_trip() {
        let id = livery_job_id("aerodynamics-t38c", "beale");
        assert_eq!(id, "livery:aerodynamics-t38c:beale");
        assert_eq!(
            parse_livery_job_id(&id),
            Some(("aerodynamics-t38c", "beale"))
        );
        assert_eq!(parse_livery_job_id("aerodynamics-t38c"), None);
    }

    #[test]
    fn job_phase_wire_shape() {
        let failed = JobPhase::Failed {
            error: IpcError::new(crate::IpcErrorCode::DiskFull, "full"),
        };
        assert_eq!(
            serde_json::to_value(&failed).unwrap(),
            serde_json::json!({
                "state": "failed",
                "error": { "code": "disk_full", "message": "full" }
            })
        );
        assert_eq!(
            serde_json::to_value(JobPhase::Downloading).unwrap(),
            serde_json::json!({ "state": "downloading" })
        );
    }

    #[test]
    fn progress_fraction() {
        let p = |done, total| Progress { done, total };
        assert_eq!(p(5, Some(10)).fraction(), Some(0.5));
        assert_eq!(p(5, None).fraction(), None);
        assert_eq!(p(5, Some(0)).fraction(), None);
        assert_eq!(p(11, Some(10)).fraction(), Some(1.0));
    }
}
