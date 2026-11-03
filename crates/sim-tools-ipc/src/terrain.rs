//! The custom terrain radar's database.

use serde::{Deserialize, Serialize};

/// Job id of the database download, as it appears in `jobs:changed`.
/// Deliberately not a catalog product id, so it never shows as installed.
pub const TERRAIN_JOB_ID: &str = "terrain-data-t38n";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerrainStatus {
    /// `world.itr` exists on disk.
    pub installed: bool,
    /// The renderer has it open and answers gauge requests.
    pub loaded: bool,
    pub path: String,
    pub size_bytes: u64,
    pub tiles: Option<u64>,
    pub resolution: Option<u32>,
    /// A download is running.
    pub downloading: bool,
    /// Why the database on disk could not be opened, if it could not.
    pub load_error: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TerrainDownloadArgs {
    /// Overrides the canonical Worker URL (e.g. a presigned GET).
    pub url: Option<String>,
    /// Lowercase hex SHA-256 the download must match.
    pub sha256: Option<String>,
}
