//! The Perf page: live timings from a 10-systems `sim-perf` build, analysed
//! by the plugin and shipped ready to draw.

use serde::{Deserialize, Serialize};

/// Frame-time histogram buckets, ms (upper bounds; the last is open).
pub const BUCKETS_MS: [f64; 9] = [0.25, 0.5, 1.0, 2.0, 4.0, 8.0, 16.7, 33.3, f64::INFINITY];

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PerfOverview {
    pub paused: bool,
    /// Sorted by name.
    pub modules: Vec<ModuleSummary>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModuleSummary {
    pub module: String,
    /// A gauges module (`*_gauges`) rather than a systems module.
    pub gauges: bool,
    /// No sample for a while.
    pub stale: bool,
    /// Mean busy ms per frame, last two seconds.
    pub busy_ms: f64,
    /// Busy ms per frame, last thirty seconds.
    pub busy_series: Vec<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PerfViewArgs {
    pub module: String,
    /// Only probes of this group; `None` for every group.
    pub group: Option<String>,
    /// Case-insensitive substring of `group/name`.
    pub query: String,
    /// Every matching probe rather than the top rows.
    pub all_probes: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SegmentKind {
    Phase,
    Stage,
    Gauge,
    Probe,
    Rest,
    Untimed,
}

/// One slice of a module frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Segment {
    pub key: String,
    pub label: String,
    pub kind: SegmentKind,
    /// Mean ms per frame over the recent window.
    pub ms: f64,
    /// ms per frame for each recent sample (the last thirty seconds).
    pub series: Vec<f64>,
}

/// The pieces of a module frame, nested so they add up to its busy time.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Anatomy {
    pub busy_ms: f64,
    pub segments: Vec<Segment>,
}

/// A sample whose worst frame stood well clear of the module's usual.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Spike {
    pub age_ms: u64,
    pub max_busy_ms: f64,
    /// The probe with the single worst call in that sample, and that call.
    pub culprit: Option<String>,
    pub culprit_max_us: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GaugeTile {
    pub slot: String,
    pub update_ms: f64,
    pub draw_ms: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeRow {
    pub group: String,
    pub name: String,
    pub ms_per_frame: f64,
    pub calls_per_sec: f64,
    pub last_avg_us: f64,
    pub last_max_us: f64,
    /// Worst single call since the last reset.
    pub peak_us: f64,
    /// ms per frame, last thirty seconds.
    pub series: Vec<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CounterRow {
    pub key: String,
    pub per_frame: f64,
    pub per_sec: f64,
    /// Count per frame, last thirty seconds.
    pub series: Vec<f64>,
}

/// One frame as the module reported it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameTime {
    pub busy_ms: f64,
    /// Wall time since the previous frame: the sim's own frame time.
    pub interval_ms: f64,
}

/// Everything the Perf page draws for one module.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PerfView {
    pub module: String,
    pub gauges: bool,
    pub stale: bool,
    pub clock_res_ns: u64,
    /// Samples lost on the way (a `seq` jump).
    pub gaps: u64,
    pub fps: f64,
    pub p99_ms: f64,
    pub worst_ms: f64,
    /// Share of recent frames over 16.7 ms.
    pub over_budget: f64,
    /// Host variable calls per frame.
    pub host_calls: f64,
    /// NanoVG primitives per frame.
    pub prims: f64,
    /// Physics steps per frame, for a systems module.
    pub steps: Option<f64>,
    pub anatomy: Anatomy,
    pub frames: Vec<FrameTime>,
    /// Counts per [`BUCKETS_MS`] bucket over `frames`.
    pub histogram: Vec<u32>,
    pub spikes: Vec<Spike>,
    /// Most expensive first.
    pub gauge_tiles: Vec<GaugeTile>,
    pub groups: Vec<String>,
    /// Matching probes, most expensive first.
    pub probes: Vec<ProbeRow>,
    /// Probes matching the filter, shown or not.
    pub probes_matching: usize,
    pub counters: Vec<CounterRow>,
}
