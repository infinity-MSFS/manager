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
    /// Which of the package's sim presets each account may fly. Absent:
    /// the manager leaves the presets alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presets: Option<PresetGating>,
    /// The product's name for an account that owns more than its base, in
    /// order: the first rule whose `requires` the account owns names it
    /// ([`Product::name_for`]), and [`Product::name`] names it for everyone
    /// else — signed out, a base owner, and a manager too old to know this
    /// field. Display only, like [`PresetGating`]: it unlocks nothing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub name_by_ownership: Vec<OwnershipName>,
}

/// One [`Product::name_by_ownership`] rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnershipName {
    /// The product the account must own, as in
    /// [`EntitlementsResponse::products`].
    pub requires: ProductId,
    /// The name it is shown under when it does.
    pub name: String,
}

impl Product {
    /// The name an account owning `owned` sees — see
    /// [`Product::name_by_ownership`].
    pub fn name_for(&self, owned: &[ProductId]) -> &str {
        self.name_by_ownership
            .iter()
            .find(|rule| owned.contains(&rule.requires))
            .map_or(self.name.as_str(), |rule| rule.name.as_str())
    }
}

/// Preset visibility: the manager writes `isUserSelectable` into each
/// preset's `aircraft.cfg` so the sim lists only the presets the account
/// covers. A convenience for the aircraft list, not a lock.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PresetGating {
    /// The folder holding one folder per preset, relative to the package
    /// root, `/`-separated.
    pub root: String,
    pub groups: Vec<PresetGroup>,
    /// Permissions that see every listed preset.
    #[serde(default)]
    pub see_all: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PresetGroup {
    /// The permission that unlocks the group; `None` is free for everyone,
    /// signed out included.
    #[serde(default)]
    pub requires: Option<String>,
    /// Preset folder names under [`PresetGating::root`].
    pub presets: Vec<String>,
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
        /// The chunked release, when published. Preferred over `archive`
        /// by builds that understand it; `archive` stays for older ones.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        depot: Option<DepotRef>,
    },
    #[serde(rename_all = "camelCase")]
    Msfs2020 {
        package_dir_name: String,
        archive: ArchiveRef,
        #[serde(default)]
        manifest_key: Option<String>,
        /// The chunked release, when published. Preferred over `archive`
        /// by builds that understand it; `archive` stays for older ones.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        depot: Option<DepotRef>,
    },
    Generic {
        archive: ArchiveRef,
    },
}

/// A release published as content-addressed chunks (see `infinity-depot`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DepotRef {
    /// Storage key of the zstd-compressed manifest.
    pub manifest_key: String,
    /// BLAKE3 of the manifest bytes, lowercase hex.
    pub manifest_hash: String,
    /// Manifest format version; a build that cannot read it falls back to
    /// the archive.
    #[serde(default = "DepotRef::first_schema")]
    pub schema: u32,
    /// Bytes on disk once installed.
    #[serde(default)]
    pub install_size: u64,
}

impl DepotRef {
    const fn first_schema() -> u32 {
        1
    }
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
    /// `.tar.zst`; `"tarzst"` on the wire, as the CDN Worker writes it.
    TarZst,
    /// A format newer than this build. Parsed rather than rejected so one
    /// such product does not make the whole catalog unreadable; installing
    /// it fails with `Unsupported`.
    #[serde(other)]
    Unknown,
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

impl PackageSpec {
    pub fn archive(&self) -> &ArchiveRef {
        match self {
            Self::Msfs2024 { archive, .. }
            | Self::Msfs2020 { archive, .. }
            | Self::Generic { archive } => archive,
        }
    }

    /// The folder the package occupies in the sim's Community folder.
    pub fn package_dir_name(&self) -> Option<&str> {
        match self {
            Self::Msfs2024 {
                package_dir_name, ..
            }
            | Self::Msfs2020 {
                package_dir_name, ..
            } => Some(package_dir_name),
            Self::Generic { .. } => None,
        }
    }

    /// The chunked release, when the catalog has one.
    pub fn depot(&self) -> Option<&DepotRef> {
        match self {
            Self::Msfs2024 { depot, .. } | Self::Msfs2020 { depot, .. } => depot.as_ref(),
            Self::Generic { .. } => None,
        }
    }

    /// Key of the package's integrity manifest, when it ships one.
    pub fn manifest_key(&self) -> Option<&str> {
        match self {
            Self::Msfs2024 { manifest_key, .. } | Self::Msfs2020 { manifest_key, .. } => {
                manifest_key.as_deref()
            }
            Self::Generic { .. } => None,
        }
    }
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
    /// The chunked release's manifest (`package.depot.manifestKey`).
    DepotManifest,
}

/// Worker `POST /v1/depot/presign` request: download URLs for packs of a
/// product's depot.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DepotPresignRequest {
    pub product_id: ProductId,
    pub keys: Vec<String>,
}

/// Worker `POST /v1/depot/presign` response, in request order.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DepotPresigned {
    pub urls: Vec<PresignedUrl>,
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

/// A published EFB document (Worker `GET /v1/efb/docs`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EfbDocMeta {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub category: String,
}

/// Worker `POST /v1/entitlements` response
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntitlementsResponse {
    pub products: Vec<ProductId>,
    #[serde(default)]
    pub channels: Vec<crate::dto::Channel>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn depot_is_optional_beside_the_archive() {
        let spec: PackageSpec = serde_json::from_value(serde_json::json!({
            "kind": "msfs2024",
            "packageDirName": "p",
            "archive": { "format": "zip", "key": "a.zip" },
            "depot": { "manifestKey": "depots/p/m", "manifestHash": "00", "installSize": 5 }
        }))
        .unwrap();
        let depot = spec.depot().unwrap();
        assert_eq!(depot.schema, 1);
        assert_eq!(depot.install_size, 5);
        let legacy: PackageSpec = serde_json::from_value(serde_json::json!({
            "kind": "msfs2020",
            "packageDirName": "p",
            "archive": { "format": "zip", "key": "a.zip" }
        }))
        .unwrap();
        assert!(legacy.depot().is_none());
        assert_eq!(
            serde_json::to_value(PresignArtifact::DepotManifest).unwrap(),
            "depot_manifest"
        );
    }

    #[test]
    fn presets_are_optional() {
        let product = |extra: serde_json::Value| {
            let mut v = serde_json::json!({
                "id": "p", "name": "P", "category": "aircraft", "vendor": "v",
                "version": "1.0.0", "ownership": { "kind": "free" },
                "package": { "kind": "generic", "archive": { "format": "zip", "key": "a.zip" } }
            });
            if let (Some(o), Some(e)) = (v.as_object_mut(), extra.as_object()) {
                o.extend(e.clone());
            }
            serde_json::from_value::<Product>(v).unwrap()
        };
        assert!(product(serde_json::json!({})).presets.is_none());
        let gated = product(serde_json::json!({ "presets": {
            "root": "SimObjects/x",
            "groups": [{ "presets": ["A"] }, { "requires": "perm", "presets": ["B"] }]
        }}));
        let presets = gated.presets.unwrap();
        assert_eq!(presets.groups[0].requires, None);
        assert_eq!(presets.groups[1].requires.as_deref(), Some("perm"));
        assert!(presets.see_all.is_empty());
    }

    #[test]
    fn archive_formats_match_the_worker() {
        let parse = |s: &str| serde_json::from_str::<ArchiveFormat>(s).unwrap();
        assert_eq!(parse("\"zip\""), ArchiveFormat::Zip);
        assert_eq!(parse("\"tarzst\""), ArchiveFormat::TarZst);
        assert_eq!(parse("\"7z\""), ArchiveFormat::Unknown);
        assert_eq!(
            serde_json::to_string(&ArchiveFormat::TarZst).unwrap(),
            "\"tarzst\""
        );
    }

    /// The DC-10's names (operator, 18 Nov 2026): "DC-10 Series" for the
    /// base, "DC-10 Premium Series" with the premium, "DC-10 / MD-10 Series"
    /// with the MD-10 — the first rule the account owns wins, so the MD-10's
    /// comes first.
    #[test]
    fn a_product_is_named_by_the_first_rule_the_account_owns() {
        let product: Product = serde_json::from_value(serde_json::json!({
            "id": "aerodynamics-dc10", "name": "DC-10 Series", "category": "aircraft",
            "vendor": "v", "version": "1.0.0", "ownership": { "kind": "free" },
            "package": { "kind": "generic", "archive": { "format": "zip", "key": "a.zip" } },
            "nameByOwnership": [
                { "requires": "aerodynamics-md10", "name": "DC-10 / MD-10 Series" },
                { "requires": "aerodynamics-dc10-prem", "name": "DC-10 Premium Series" }
            ]
        }))
        .unwrap();
        let owns = |ids: &[&str]| ids.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(product.name_for(&[]), "DC-10 Series", "signed out");
        assert_eq!(
            product.name_for(&owns(&["aerodynamics-dc10"])),
            "DC-10 Series"
        );
        assert_eq!(
            product.name_for(&owns(&["aerodynamics-dc10", "aerodynamics-dc10-prem"])),
            "DC-10 Premium Series"
        );
        assert_eq!(
            product.name_for(&owns(&[
                "aerodynamics-dc10",
                "aerodynamics-dc10-prem",
                "aerodynamics-md10"
            ])),
            "DC-10 / MD-10 Series"
        );
    }

    #[test]
    fn a_catalog_without_names_by_ownership_still_parses_and_says_nothing_new() {
        let product: Product = serde_json::from_value(serde_json::json!({
            "id": "p", "name": "P", "category": "aircraft", "vendor": "v",
            "version": "1.0.0", "ownership": { "kind": "free" },
            "package": { "kind": "generic", "archive": { "format": "zip", "key": "a.zip" } }
        }))
        .unwrap();
        assert!(product.name_by_ownership.is_empty());
        assert_eq!(product.name_for(&["anything".to_string()]), "P");
        // An empty rule list is left out when the catalog is written back.
        let written = serde_json::to_value(&product).unwrap();
        assert!(written.get("nameByOwnership").is_none());
    }
}
