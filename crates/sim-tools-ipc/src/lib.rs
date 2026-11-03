//! IPC contract of the `infinity-sim-tools` plugin: the terrain radar
//! database, the MD-10 HUD's FLIR (nose camera) and the developer Perf page.
//!
//! Shared by the plugin (native) and the official UI (wasm32), so it holds
//! plain data only.

pub mod flir;
pub mod perf;
pub mod terrain;

use infinity_ipc::define_commands;

define_commands! {
    plugin = "infinity-sim-tools";

    /// The terrain database on disk and in the renderer.
    terrain_status => TerrainStatusGet(()) -> terrain::TerrainStatus;
    /// Starts downloading the terrain database. Progress arrives as
    /// `jobs:changed` for [`terrain::TERRAIN_JOB_ID`]; returns at once.
    terrain_download => TerrainDownload(terrain::TerrainDownloadArgs) -> ();
    /// Stops the download and deletes what it fetched.
    terrain_cancel => TerrainCancel(()) -> ();

    flir_diag => FlirDiagGet(()) -> flir::FlirDiag;
    flir_settings_get => FlirSettingsGet(()) -> flir::FlirSettings;
    /// The engine's defaults, for a reset in the UI.
    flir_settings_default => FlirSettingsDefault(()) -> flir::FlirSettings;
    /// Applies from the next frame; `persist` also writes them to disk.
    flir_settings_set => FlirSettingsSet(flir::FlirSettingsSetArgs) -> ();
    /// Tears the nose view and the bridge DLL out of the sim.
    flir_unload => FlirUnload(()) -> ();
    /// The latest frame as the gauge receives it, as a PNG.
    flir_preview => FlirPreviewGet(flir::FlirPreviewArgs) -> Option<flir::FlirPreview>;

    /// Every module that has sent samples.
    perf_overview => PerfOverviewGet(()) -> perf::PerfOverview;
    /// One module, analysed and ready to draw. `None` for an unknown module.
    perf_view => PerfViewGet(perf::PerfViewArgs) -> Option<perf::PerfView>;
    /// One module's current state as a plain-text table.
    perf_snapshot => PerfSnapshot(String) -> Option<String>;
    perf_set_paused => PerfSetPaused(bool) -> ();
    perf_reset => PerfReset(()) -> ();
}

#[cfg(test)]
mod tests {
    use infinity_ipc::Command;

    #[test]
    fn commands_are_namespaced_to_the_plugin() {
        assert_eq!(super::PLUGIN, "infinity-sim-tools");
        assert_eq!(
            super::TerrainStatusGet::NAME,
            "plugin:infinity-sim-tools|terrain_status"
        );
        assert_eq!(super::ALL.len(), 14);
    }
}
