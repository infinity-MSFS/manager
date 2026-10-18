use serde::{Deserialize, Serialize};

pub type ProductId = String;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Catalog {
    pub schema_version: u32,
    pub products: Vec<Product>,
    #[serde(default)]
    pub widget_types: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Product {
    pub id: ProductId,
    pub name: String,
    pub category: ProductCategory,
    pub vendor: String,
    pub version: String,
    pub package: PackageSpec,
    pub ownership: OwnershipPolicy,
    #[serde(default)]
    pub work_dir: Option<WorkDirSpec>,
    #[serde(default)]
    pub configurator: Option<ConfiguratorSpec>,
    #[serde(default)]
    pub bridges: Vec<BridgeSpec>,
    #[serde(default)]
    pub availability: Availability,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub image_src: Option<String>,
    #[serde(default)]
    pub has_liveries: bool,
    #[serde(default)]
    pub coming_soon: bool,
    #[serde(default)]
    pub navdata: Option<NavdataSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NavdataSpec {
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default)]
    pub package_match: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProductCategory {
    Aircraft,
    Tool,
    Scenery,
    Livery,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PackageSpec {
    #[serde(rename_all = "camelCase")]
    Msfs2024 {
        package_dir_name: String,
        archive: ArchiveRef,
        #[serde(default)]
        manifest_key: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    Msfs2020 {
        package_dir_name: String,
        archive: ArchiveRef,
        #[serde(default)]
        manifest_key: Option<String>,
    },
    Generic {
        archive: ArchiveRef,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveRef {
    pub format: ArchiveFormat,
    pub key: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ArchiveFormat {
    Zip,
    TarZst,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OwnershipPolicy {
    Auth0Claim { claim: String },
    Auth0Metadata { key: String },
    Free,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkDirSpec {
    #[serde(rename_all = "camelCase")]
    MsfsPackageWork {
        package_prefix: String,
    },
    AppData {
        relative: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfiguratorSpec {
    pub engine: String,
    #[serde(default)]
    pub config_file: Option<String>,
    /// **Deprecated**
    #[serde(default)]
    pub export_file: Option<String>,
    #[serde(default)]
    pub transport: Option<ConfiguratorTransport>,
    #[serde(default)]
    pub bridge_ref: Option<String>,
    pub schema: serde_json::Value,
    #[serde(default)]
    pub migrations: Vec<serde_json::Value>,
    #[serde(default)]
    pub outputs: Vec<OutputSpec>,
    #[serde(default)]
    pub ui: Option<UiSpec>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiSpec {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub subtitle: Option<String>,
    #[serde(default)]
    pub layout: UiLayout,
    #[serde(default)]
    pub indicators: Vec<IndicatorSpec>,
    #[serde(default)]
    pub actions: Vec<ActionSpec>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiLayout {
    #[default]
    Stacked,
    Tabs,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum IndicatorSpec {
    #[serde(rename_all = "camelCase")]
    BridgeConnected {
        id: String,
        label: String,
        bridge_ref: String,
    },
    NavDbCycle {
        id: String,
        label: String,
    },
}

/// Sidebar action button. `EmitOutput` is the workhorse — points at one of
/// the product's `outputs[]` by id. The Save / Load / Reset variants are
/// thin wrappers around persistence commands so authors don't have to
/// re-declare them every time.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ActionSpec {
    /// Run `configurator_emit(product, output_id)`. The most common action
    /// — used for "Send to Aircraft", "Write to Sim", etc.
    #[serde(rename_all = "camelCase")]
    EmitOutput {
        id: String,
        label: String,
        output_id: String,
        #[serde(default)]
        icon: Option<String>,
        #[serde(default)]
        primary: bool,
    },
    /// Pick a save destination via the OS dialog and write the current
    /// envelope as JSON. v3.0 ships a simple "envelope dump" share format;
    /// the v1 curated-subset format is not produced.
    #[serde(rename_all = "camelCase")]
    ShareableSave {
        id: String,
        label: String,
        #[serde(default)]
        icon: Option<String>,
    },
    /// Pick a JSON file via the OS dialog and import it as the current
    /// envelope. Inverse of [`ShareableSave`].
    #[serde(rename_all = "camelCase")]
    ShareableLoad {
        id: String,
        label: String,
        #[serde(default)]
        icon: Option<String>,
    },
    /// Delete the persisted config and reload defaults.
    #[serde(rename_all = "camelCase")]
    ResetConfig {
        id: String,
        label: String,
        #[serde(default)]
        icon: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfiguratorTransport {
    File,
    WsBridge,
    /// No local persistence — values live in the bridge target (e.g. DC-10
    /// equipment state). Configurator load/save short-circuit; widgets bind
    /// directly to bridge query/command pairs.
    LiveBridge,
}

/// One sink the configurator can write the user's value to. Stored opaquely
/// in `outputs[]`; resolved by the app's `configurator_emit` command. The
/// `transforms` list is parsed by the `configurator` crate as `Vec<Op>`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum OutputSpec {
    /// Write the transformed JSON to `<workDir>/<path>`. Atomic write.
    #[serde(rename_all = "camelCase")]
    File {
        id: String,
        path: String,
        #[serde(default)]
        transforms: Vec<serde_json::Value>,
    },
    /// Send the transformed JSON as a `BridgeManager::command(namespace,
    /// command, payload)` against the bridge identified by `bridge_ref`.
    #[serde(rename_all = "camelCase")]
    Bridge {
        id: String,
        bridge_ref: String,
        command: String,
        #[serde(default)]
        transforms: Vec<serde_json::Value>,
    },
}

impl OutputSpec {
    pub fn id(&self) -> &str {
        match self {
            OutputSpec::File { id, .. } => id,
            OutputSpec::Bridge { id, .. } => id,
        }
    }

    pub fn transforms(&self) -> &[serde_json::Value] {
        match self {
            OutputSpec::File { transforms, .. } => transforms,
            OutputSpec::Bridge { transforms, .. } => transforms,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeSpec {
    pub id: String,
    pub kind: BridgeKind,
    pub port: u16,
    pub namespace: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BridgeKind {
    TauriWsBridge,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Availability {
    #[serde(default)]
    pub channels: Vec<crate::dto::Channel>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangelogEntry {
    pub version: String,
    pub changelog: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PresignArtifact {
    Archive,
    Manifest,
}

/// Worker `POST /v1/presign` response
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PresignedUrl {
    pub url: String,
    /// ISO-8601 expiry timestamp.
    pub expires_at: String,
    pub key: String,
}

/// Worker `GET /v1/liveries/:productId` response
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveryCatalog {
    pub schema_version: u32,
    pub product_id: ProductId,
    pub liveries: Vec<Livery>,
    /// ISO-8601 timestamp; URLs in this response stop working after this.
    pub expires_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Livery {
    pub id: String,
    pub name: String,
    pub creator: String,
    pub version: String,
    pub description: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub contents: Vec<String>,
    /// Presigned R2 URL for the installable zip.
    pub archive_url: String,
    pub archive_key: String,
    /// Presigned R2 URL for the hero image.
    pub preview_url: String,
    /// Presigned R2 URLs for additional gallery images. First entry mirrors
    /// `preview_url` so the gallery viewer can iterate in order.
    #[serde(default)]
    pub gallery_urls: Vec<String>,
    /// Sim package directory created when the archive is installed.
    pub package_dir_name: String,
}

/// Worker `POST /v1/entitlements` response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntitlementsResponse {
    pub products: Vec<ProductId>,
    #[serde(default)]
    pub channels: Vec<crate::dto::Channel>,
}
