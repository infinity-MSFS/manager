//! Live timings from a 10-systems build with its `sim-perf` probes compiled
//! in (`cargo fs-build --perf` there).
//!
//! Each probed WASM module (the systems module and the gauges module
//! separately) sends one `perf_sample` event four times a second through the
//! Coherent relay to `ws://127.0.0.1:9879/perf`. A normal build sends
//! nothing, so this sits idle for everyone who is not profiling.
//!
//! The history lives here rather than in the page, so it accumulates while
//! the page is closed; the page asks for one module at a time, analysed and
//! trimmed to what it draws.
//!
//! What the numbers mean:
//! - A module FRAME is one call of its update. The systems module times its
//!   whole update; the gauges module sums every gauge's update and draw
//!   between two updates of one leader gauge. `interval` is the wall time
//!   between frames, i.e. the sim's own frame time.
//! - ms / frame is a probe's total time in a window over that window's
//!   frames. The systems module steps its physics at 20 Hz, so a `stage/...`
//!   row is the per-frame average of a cost paid on roughly one frame in
//!   three at 60 fps; its max column is the per-step cost.
//! - `stage/...` probes run inside `phase/advance`; [`anatomy`] nests them.
//!
//! Payload (`sim_perf::Report::to_json` in 10-systems):
//!
//! ```text
//! { v: 1, module, seq, window_s, clock_res_ns,
//!   frames:   [[busy_ms, interval_ms], ...],
//!   probes:   [[group, name, calls, total_us, max_us], ...],
//!   counters: [[group, name, count], ...] }
//! ```

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use infinity_sim_tools_ipc::perf::{
    Anatomy, BUCKETS_MS, CounterRow, FrameTime, GaugeTile, ModuleSummary, PerfOverview, PerfView,
    PerfViewArgs, ProbeRow, Segment, SegmentKind, Spike,
};
use serde::Deserialize;

/// Bridge the 10-systems relay connects its perf stream to. A three-repo
/// contract: 10-systems' `sim-perf` and `coherent/bridge/Main.res` spell
/// the same port, namespace and event.
pub(crate) const PORT: u16 = 9879;
pub(crate) const NAMESPACE: &str = "perf";
pub(crate) const SAMPLE_EVENT: &str = "perf_sample";

/// Samples of history kept (two minutes at four a second).
pub(crate) const HISTORY_SAMPLES: usize = 480;
/// Individual frames kept for the frame-time chart.
pub(crate) const HISTORY_FRAMES: usize = 900;
/// Samples averaged for tables and tiles: two seconds.
pub(crate) const RECENT_SAMPLES: usize = 8;
/// Samples the sparklines and the stacked chart show: thirty seconds.
pub(crate) const SPARK_SAMPLES: usize = 120;
/// A module with no sample for this long is stale.
pub(crate) const STALE: Duration = Duration::from_secs(2);
/// Probe rows shown before "show all".
pub(crate) const TOP_ROWS: usize = 40;
pub(crate) const SPIKES_SHOWN: usize = 8;

/// Stages shown on their own before the rest are lumped together.
const TOP_STAGES: usize = 7;
const TOP_GAUGES: usize = 10;
const TOP_PROBES: usize = 10;
const BUDGET_60_MS: f64 = 1000.0 / 60.0;

/// The wire payload. Every field is optional on the way in.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct Sample {
    pub module: String,
    pub seq: u64,
    pub window_s: f64,
    pub clock_res_ns: u64,
    pub frames: Vec<(f64, f64)>,
    pub probes: Vec<(String, String, f64, f64, f64)>,
    pub counters: Vec<(String, String, f64)>,
}

#[derive(Debug, Clone)]
struct Summary {
    at: Instant,
    busy_ms: f64,
    max_busy_ms: f64,
    fps: f64,
}

#[derive(Debug, Clone)]
struct ProbeHistory {
    group: String,
    name: String,
    ms_per_frame: Vec<f64>,
    calls_per_sec: Vec<f64>,
    max_us: Vec<f64>,
    last_avg_us: f64,
    last_max_us: f64,
    peak_us: f64,
}

#[derive(Debug, Clone)]
struct CounterHistory {
    group: String,
    per_frame: Vec<f64>,
    per_sec: Vec<f64>,
}

#[derive(Debug, Clone)]
struct ModuleHistory {
    module: String,
    last_seen: Instant,
    last_seq: u64,
    clock_res_ns: u64,
    gaps: u64,
    summaries: Vec<Summary>,
    frames: Vec<FrameTime>,
    probes: BTreeMap<String, ProbeHistory>,
    counters: BTreeMap<String, CounterHistory>,
}

/// Every module's history.
#[derive(Debug, Default)]
pub(crate) struct Perf {
    modules: BTreeMap<String, ModuleHistory>,
    paused: bool,
}

fn push<T>(ring: &mut Vec<T>, value: T, cap: usize) {
    ring.push(value);
    if ring.len() > cap {
        let excess = ring.len() - cap;
        ring.drain(..excess);
    }
}

fn tail<T: Clone>(values: &[T], n: usize) -> Vec<T> {
    values[values.len().saturating_sub(n)..].to_vec()
}

pub(crate) fn mean(values: &[f64], last: usize) -> f64 {
    let t = &values[values.len().saturating_sub(last)..];
    if t.is_empty() {
        0.0
    } else {
        t.iter().sum::<f64>() / t.len() as f64
    }
}

pub(crate) fn percentile(values: &[f64], p: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let i = (((p / 100.0) * sorted.len() as f64).floor() as usize).min(sorted.len() - 1);
    sorted[i]
}

/// `ring`'s last `n` entries, zero-padded at the front to exactly `n`.
fn aligned(ring: &[f64], n: usize) -> Vec<f64> {
    let mut out = vec![0.0; n];
    let take = n.min(ring.len());
    out[n - take..].copy_from_slice(&ring[ring.len() - take..]);
    out
}

fn sum_series<'a>(series: impl IntoIterator<Item = &'a [f64]>, n: usize) -> Vec<f64> {
    let mut out = vec![0.0; n];
    for s in series {
        for (o, v) in out.iter_mut().zip(s) {
            *o += v;
        }
    }
    out
}

fn diff_series(a: &[f64], b: &[f64]) -> Vec<f64> {
    a.iter()
        .enumerate()
        .map(|(i, v)| (v - b.get(i).copied().unwrap_or(0.0)).max(0.0))
        .collect()
}

fn segment(key: String, label: String, kind: SegmentKind, series: Vec<f64>) -> Segment {
    Segment {
        ms: mean(&series, RECENT_SAMPLES),
        key,
        label,
        kind,
        series,
    }
}

/// Keeps the `top` biggest and lumps the rest into one `rest` segment.
fn top_and_rest(
    mut segs: Vec<Segment>,
    top: usize,
    rest_key: &str,
    rest_label: &str,
    n: usize,
) -> Vec<Segment> {
    segs.sort_by(|a, b| b.ms.total_cmp(&a.ms));
    let rest = segs.split_off(top.min(segs.len()));
    if !rest.is_empty() {
        let series = sum_series(rest.iter().map(|s| s.series.as_slice()), n);
        segs.push(segment(
            rest_key.into(),
            format!("{rest_label} ({})", rest.len()),
            SegmentKind::Rest,
            series,
        ));
    }
    segs
}

/// `slot.update` / `slot.draw` -> (`slot`, `draw?`).
fn gauge_slot(name: &str) -> (&str, bool) {
    if let Some(slot) = name.strip_suffix(".update") {
        (slot, false)
    } else if let Some(slot) = name.strip_suffix(".draw") {
        (slot, true)
    } else {
        (name, false)
    }
}

impl ModuleHistory {
    fn new(module: &str, now: Instant) -> Self {
        Self {
            module: module.to_owned(),
            last_seen: now,
            last_seq: 0,
            clock_res_ns: 0,
            gaps: 0,
            summaries: Vec::new(),
            frames: Vec::new(),
            probes: BTreeMap::new(),
            counters: BTreeMap::new(),
        }
    }

    fn is_gauges(&self) -> bool {
        self.probes.values().any(|p| p.group == "gauge") || self.module.ends_with("_gauges")
    }

    fn ingest(&mut self, s: Sample, now: Instant) {
        // A module reload restarts `seq` at 1; that is not a gap.
        if self.last_seq > 0 && s.seq > self.last_seq + 1 {
            self.gaps += s.seq - self.last_seq - 1;
        }
        self.last_seq = s.seq;
        self.last_seen = now;
        self.clock_res_ns = s.clock_res_ns;

        let window_s = if s.window_s > 0.0 { s.window_s } else { 0.25 };
        let n_frames = s.frames.len().max(1) as f64;
        let (mut busy_sum, mut busy_max, mut interval_sum, mut intervals) =
            (0.0, 0.0f64, 0.0, 0u32);
        for &(busy, interval) in &s.frames {
            busy_sum += busy;
            busy_max = busy_max.max(busy);
            if interval > 0.0 {
                interval_sum += interval;
                intervals += 1;
            }
            push(
                &mut self.frames,
                FrameTime {
                    busy_ms: busy,
                    interval_ms: interval,
                },
                HISTORY_FRAMES,
            );
        }
        push(
            &mut self.summaries,
            Summary {
                at: now,
                busy_ms: if s.frames.is_empty() {
                    0.0
                } else {
                    busy_sum / s.frames.len() as f64
                },
                max_busy_ms: busy_max,
                fps: if intervals > 0 {
                    1000.0 / (interval_sum / f64::from(intervals))
                } else {
                    0.0
                },
            },
            HISTORY_SAMPLES,
        );

        // Every known probe gets an entry for this sample: absent means it
        // did not run, a 0 on its chart, not a gap.
        let mut seen = BTreeSet::new();
        let before = self.summaries.len().saturating_sub(1);
        for (group, name, calls, total_us, max_us) in s.probes {
            let key = format!("{group}/{name}");
            let p = self.probes.entry(key.clone()).or_insert_with(|| {
                // Back-filled so every ring ends on the same sample as
                // `summaries`: the charts index them together from the tail.
                ProbeHistory {
                    group,
                    name,
                    ms_per_frame: vec![0.0; before],
                    calls_per_sec: vec![0.0; before],
                    max_us: vec![0.0; before],
                    last_avg_us: 0.0,
                    last_max_us: 0.0,
                    peak_us: 0.0,
                }
            });
            push(
                &mut p.ms_per_frame,
                total_us / 1000.0 / n_frames,
                HISTORY_SAMPLES,
            );
            push(&mut p.calls_per_sec, calls / window_s, HISTORY_SAMPLES);
            push(&mut p.max_us, max_us, HISTORY_SAMPLES);
            p.last_avg_us = if calls > 0.0 { total_us / calls } else { 0.0 };
            p.last_max_us = max_us;
            p.peak_us = p.peak_us.max(max_us);
            seen.insert(key);
        }
        for (key, p) in &mut self.probes {
            if seen.contains(key) {
                continue;
            }
            push(&mut p.ms_per_frame, 0.0, HISTORY_SAMPLES);
            push(&mut p.calls_per_sec, 0.0, HISTORY_SAMPLES);
            push(&mut p.max_us, 0.0, HISTORY_SAMPLES);
            p.last_avg_us = 0.0;
            p.last_max_us = 0.0;
        }

        for (group, name, count) in s.counters {
            let c = self
                .counters
                .entry(format!("{group}/{name}"))
                .or_insert_with(|| CounterHistory {
                    group,
                    per_frame: Vec::new(),
                    per_sec: Vec::new(),
                });
            push(&mut c.per_frame, count / n_frames, HISTORY_SAMPLES);
            push(&mut c.per_sec, count / window_s, HISTORY_SAMPLES);
        }
    }

    fn busy_series(&self) -> Vec<f64> {
        self.summaries.iter().map(|s| s.busy_ms).collect()
    }

    fn frame_busies(&self) -> Vec<f64> {
        self.frames.iter().map(|f| f.busy_ms).collect()
    }

    fn counter_per_frame(&self, group: &str) -> f64 {
        self.counters
            .values()
            .filter(|c| c.group == group)
            .map(|c| mean(&c.per_frame, RECENT_SAMPLES))
            .sum()
    }
}

/// The pieces of a module frame, nested so they add up to its busy time:
///
/// - systems module: each phase; `phase/advance` replaced by its stages (the
///   biggest on their own) plus the advance time no stage covers; then the
///   frame time no phase covers.
/// - gauges module: each gauge (update + draw), the biggest on their own.
/// - anything else: its probes.
fn anatomy(m: &ModuleHistory) -> Anatomy {
    let n = m.summaries.len();
    let busy = m.busy_series();
    let series = |p: &ProbeHistory| aligned(&p.ms_per_frame, n);
    let mut segments = if let Some(advance) = m.probes.get("phase/advance") {
        let phases = m.probes.values().filter(|p| {
            // `late_frame` runs in another export, outside the frame.
            p.group == "phase" && p.name != "advance" && p.name != "late_frame"
        });
        let stages: Vec<Segment> = m
            .probes
            .values()
            .filter(|p| p.group == "stage")
            .map(|p| {
                segment(
                    format!("stage/{}", p.name),
                    p.name.clone(),
                    SegmentKind::Stage,
                    series(p),
                )
            })
            .collect();
        let stage_sum = sum_series(stages.iter().map(|s| s.series.as_slice()), n);
        let outside = diff_series(&series(advance), &stage_sum);
        let mut segs: Vec<Segment> = phases
            .map(|p| {
                segment(
                    format!("phase/{}", p.name),
                    p.name.clone(),
                    SegmentKind::Phase,
                    series(p),
                )
            })
            .collect();
        segs.extend(top_and_rest(
            stages,
            TOP_STAGES,
            "stage/*",
            "other stages",
            n,
        ));
        segs.push(segment(
            "advance/outside".into(),
            "advance, outside stages".into(),
            SegmentKind::Rest,
            outside,
        ));
        segs
    } else if m.probes.values().any(|p| p.group == "gauge") {
        let mut slots: BTreeMap<&str, Vec<Vec<f64>>> = BTreeMap::new();
        for p in m.probes.values().filter(|p| p.group == "gauge") {
            slots
                .entry(gauge_slot(&p.name).0)
                .or_default()
                .push(series(p));
        }
        let gauges = slots
            .into_iter()
            .map(|(slot, parts)| {
                segment(
                    format!("gauge/{slot}"),
                    slot.to_owned(),
                    SegmentKind::Gauge,
                    sum_series(parts.iter().map(Vec::as_slice), n),
                )
            })
            .collect();
        top_and_rest(gauges, TOP_GAUGES, "gauge/*", "other gauges", n)
    } else {
        let all = m
            .probes
            .iter()
            .map(|(key, p)| segment(key.clone(), key.clone(), SegmentKind::Probe, series(p)))
            .collect();
        top_and_rest(all, TOP_PROBES, "probe/*", "other", n)
    };

    let covered = sum_series(segments.iter().map(|s| s.series.as_slice()), n);
    segments.push(segment(
        "untimed".into(),
        "untimed".into(),
        SegmentKind::Untimed,
        diff_series(&busy, &covered),
    ));
    segments.retain(|s| s.ms > 0.0 || s.kind != SegmentKind::Untimed);
    Anatomy {
        busy_ms: mean(&busy, RECENT_SAMPLES),
        segments,
    }
}

/// The most recent `limit` samples whose worst frame was at least `factor`
/// times the median frame and at least `floor_ms`, each traced to the probe
/// with the worst single call in that sample.
fn spikes(m: &ModuleHistory, limit: usize, factor: f64, floor_ms: f64, now: Instant) -> Vec<Spike> {
    let threshold = (percentile(&m.frame_busies(), 50.0) * factor).max(floor_ms);
    let n = m.summaries.len();
    let mut out = Vec::new();
    for (i, s) in m.summaries.iter().enumerate().rev() {
        if out.len() >= limit {
            break;
        }
        if s.max_busy_ms < threshold {
            continue;
        }
        let back = n - i;
        let culprit = m
            .probes
            .iter()
            // `phase/advance` contains the stages; name the stage.
            .filter(|(key, _)| key.as_str() != "phase/advance")
            .map(|(key, p)| {
                let v = p
                    .max_us
                    .len()
                    .checked_sub(back)
                    .and_then(|j| p.max_us.get(j))
                    .copied()
                    .unwrap_or(0.0);
                (key, v)
            })
            .max_by(|a, b| a.1.total_cmp(&b.1));
        out.push(Spike {
            age_ms: now.saturating_duration_since(s.at).as_millis() as u64,
            max_busy_ms: s.max_busy_ms,
            culprit: culprit.map(|(k, _)| k.clone()),
            culprit_max_us: culprit.map_or(0.0, |(_, v)| v),
        });
    }
    out
}

pub(crate) fn histogram(values: &[f64]) -> Vec<u32> {
    let mut counts = vec![0u32; BUCKETS_MS.len()];
    for v in values {
        let i = BUCKETS_MS
            .iter()
            .position(|b| v <= b)
            .unwrap_or(BUCKETS_MS.len() - 1);
        counts[i] += 1;
    }
    counts
}

fn gauge_tiles(m: &ModuleHistory) -> Vec<GaugeTile> {
    let mut slots: BTreeMap<&str, (f64, f64)> = BTreeMap::new();
    for p in m.probes.values().filter(|p| p.group == "gauge") {
        let (slot, draw) = gauge_slot(&p.name);
        let t = slots.entry(slot).or_default();
        let ms = mean(&p.ms_per_frame, RECENT_SAMPLES);
        if draw {
            t.1 += ms;
        } else {
            t.0 += ms;
        }
    }
    let mut tiles: Vec<GaugeTile> = slots
        .into_iter()
        .map(|(slot, (update_ms, draw_ms))| GaugeTile {
            slot: slot.to_owned(),
            update_ms,
            draw_ms,
        })
        .collect();
    tiles.sort_by(|a, b| (b.update_ms + b.draw_ms).total_cmp(&(a.update_ms + a.draw_ms)));
    tiles
}

pub(crate) fn fmt_ms(ms: f64) -> String {
    if ms >= 100.0 {
        format!("{ms:.0}")
    } else if ms >= 10.0 {
        format!("{ms:.1}")
    } else {
        format!("{ms:.2}")
    }
}

pub(crate) fn fmt_us(us: f64) -> String {
    if us >= 1000.0 {
        format!("{:.2} ms", us / 1000.0)
    } else if us >= 100.0 {
        format!("{us:.0} µs")
    } else {
        format!("{us:.1} µs")
    }
}

impl Perf {
    pub(crate) fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
    }

    pub(crate) fn reset(&mut self) {
        self.modules.clear();
    }

    /// Takes one `perf_sample` payload. Ignored while paused or malformed.
    pub(crate) fn ingest(&mut self, data: serde_json::Value, now: Instant) {
        if self.paused {
            return;
        }
        let Ok(sample) = serde_json::from_value::<Sample>(data) else {
            tracing::debug!("perf: malformed sample");
            return;
        };
        if sample.module.is_empty() {
            return;
        }
        self.modules
            .entry(sample.module.clone())
            .or_insert_with(|| ModuleHistory::new(&sample.module, now))
            .ingest(sample, now);
    }

    pub(crate) fn overview(&self, now: Instant) -> PerfOverview {
        PerfOverview {
            paused: self.paused,
            modules: self
                .modules
                .values()
                .map(|m| {
                    let busy = m.busy_series();
                    ModuleSummary {
                        module: m.module.clone(),
                        gauges: m.is_gauges(),
                        stale: now.saturating_duration_since(m.last_seen) > STALE,
                        busy_ms: mean(&busy, RECENT_SAMPLES),
                        busy_series: tail(&busy, SPARK_SAMPLES),
                    }
                })
                .collect(),
        }
    }

    pub(crate) fn view(&self, args: &PerfViewArgs, now: Instant) -> Option<PerfView> {
        let m = self.modules.get(&args.module)?;
        let busies = m.frame_busies();
        let fps = mean(
            &m.summaries.iter().map(|s| s.fps).collect::<Vec<_>>(),
            RECENT_SAMPLES,
        );
        let mut anatomy = anatomy(m);
        for s in &mut anatomy.segments {
            s.series = tail(&s.series, SPARK_SAMPLES);
        }

        let query = args.query.trim().to_lowercase();
        let mut probes: Vec<ProbeRow> = m
            .probes
            .iter()
            .filter(|(_, p)| args.group.as_ref().is_none_or(|g| &p.group == g))
            .filter(|(key, _)| query.is_empty() || key.to_lowercase().contains(&query))
            .map(|(_, p)| ProbeRow {
                group: p.group.clone(),
                name: p.name.clone(),
                ms_per_frame: mean(&p.ms_per_frame, RECENT_SAMPLES),
                calls_per_sec: mean(&p.calls_per_sec, RECENT_SAMPLES),
                last_avg_us: p.last_avg_us,
                last_max_us: p.last_max_us,
                peak_us: p.peak_us,
                series: tail(&p.ms_per_frame, SPARK_SAMPLES),
            })
            .collect();
        probes.sort_by(|a, b| b.ms_per_frame.total_cmp(&a.ms_per_frame));
        let probes_matching = probes.len();
        if !args.all_probes {
            probes.truncate(TOP_ROWS);
        }

        let mut counters: Vec<CounterRow> = m
            .counters
            .iter()
            .map(|(key, c)| CounterRow {
                key: key.clone(),
                per_frame: mean(&c.per_frame, RECENT_SAMPLES),
                per_sec: mean(&c.per_sec, RECENT_SAMPLES),
                series: tail(&c.per_frame, SPARK_SAMPLES),
            })
            .collect();
        counters.sort_by(|a, b| b.per_sec.total_cmp(&a.per_sec));

        Some(PerfView {
            module: m.module.clone(),
            gauges: m.is_gauges(),
            stale: now.saturating_duration_since(m.last_seen) > STALE,
            clock_res_ns: m.clock_res_ns,
            gaps: m.gaps,
            fps,
            p99_ms: percentile(&busies, 99.0),
            worst_ms: busies.iter().copied().fold(0.0, f64::max),
            over_budget: if busies.is_empty() {
                0.0
            } else {
                busies.iter().filter(|b| **b > BUDGET_60_MS).count() as f64 / busies.len() as f64
            },
            host_calls: m.counter_per_frame("host"),
            prims: m.counter_per_frame("nvg"),
            steps: m
                .counters
                .get("physics/steps")
                .map(|c| mean(&c.per_frame, RECENT_SAMPLES)),
            anatomy,
            frames: m.frames.clone(),
            histogram: histogram(&busies),
            spikes: spikes(m, SPIKES_SHOWN, 3.0, 4.0, now),
            gauge_tiles: gauge_tiles(m),
            groups: m
                .probes
                .values()
                .map(|p| p.group.clone())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect(),
            probes,
            probes_matching,
            counters,
        })
    }

    /// The current state as a plain-text table, for pasting into an issue:
    /// the numbers an optimisation pass starts from.
    pub(crate) fn snapshot(&self, module: &str) -> Option<String> {
        let m = self.modules.get(module)?;
        let a = anatomy(m);
        let fps = mean(
            &m.summaries.iter().map(|s| s.fps).collect::<Vec<_>>(),
            RECENT_SAMPLES,
        );
        let mut lines = vec![
            format!(
                "module {} - {} ms/frame at {fps:.0} fps (last 2 s)",
                m.module,
                fmt_ms(a.busy_ms)
            ),
            format!(
                "p99 frame {} ms, clock step {} ns",
                fmt_ms(percentile(&m.frame_busies(), 99.0)),
                m.clock_res_ns
            ),
            String::new(),
            "anatomy (ms/frame):".into(),
        ];
        let mut segs = a.segments;
        segs.sort_by(|x, y| y.ms.total_cmp(&x.ms));
        lines.extend(
            segs.iter()
                .map(|s| format!("  {:<32} {:>8}", s.label, fmt_ms(s.ms))),
        );
        lines.push(String::new());
        lines.push("probes (ms/frame, avg/call, max, peak):".into());
        let mut rows: Vec<(&String, &ProbeHistory, f64)> = m
            .probes
            .iter()
            .map(|(k, p)| (k, p, mean(&p.ms_per_frame, RECENT_SAMPLES)))
            .collect();
        rows.sort_by(|x, y| y.2.total_cmp(&x.2));
        lines.extend(rows.iter().take(TOP_ROWS).map(|(key, p, ms)| {
            format!(
                "  {key:<40} {:>8} {:>10} {:>10} {:>10}",
                fmt_ms(*ms),
                fmt_us(p.last_avg_us),
                fmt_us(p.last_max_us),
                fmt_us(p.peak_us)
            )
        }));
        Some(lines.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn systems_sample(seq: u64, advance_us: f64) -> serde_json::Value {
        json!({
            "v": 1, "module": "md10", "seq": seq, "window_s": 0.25, "clock_res_ns": 100,
            "frames": [[2.0, 16.0], [2.0, 16.0], [2.0, 17.0], [10.0, 16.0]],
            "probes": [
                ["phase", "advance", 4, advance_us, 900.0],
                ["phase", "read", 4, 400.0, 150.0],
                ["phase", "late_frame", 4, 400.0, 150.0],
                ["stage", "hydraulics", 2, 1600.0, 800.0],
                ["stage", "electrics", 2, 800.0, 400.0]
            ],
            "counters": [["physics", "steps", 2], ["host", "get", 400]]
        })
    }

    #[test]
    fn systems_anatomy_nests_stages_inside_advance() {
        let mut perf = Perf::default();
        let now = Instant::now();
        perf.ingest(systems_sample(1, 4000.0), now);
        let view = perf
            .view(
                &PerfViewArgs {
                    module: "md10".into(),
                    ..PerfViewArgs::default()
                },
                now,
            )
            .unwrap();
        let ms = |key: &str| {
            view.anatomy
                .segments
                .iter()
                .find(|s| s.key == key)
                .map(|s| s.ms)
        };
        // advance 4 ms over 4 frames = 1 ms/frame; stages 0.4 + 0.2.
        assert_eq!(ms("stage/hydraulics"), Some(0.4));
        assert_eq!(ms("stage/electrics"), Some(0.2));
        assert!((ms("advance/outside").unwrap() - 0.4).abs() < 1e-9);
        assert_eq!(ms("phase/read"), Some(0.1));
        assert_eq!(ms("phase/late_frame"), None, "outside the frame");
        assert_eq!(ms("phase/advance"), None, "replaced by its stages");
        // busy 4 ms/frame; covered 1.1.
        assert!((ms("untimed").unwrap() - 2.9).abs() < 1e-9);
        assert_eq!(view.steps, Some(0.5));
        assert_eq!(view.host_calls, 100.0);
        assert!((view.fps - 1000.0 / 16.25).abs() < 1e-9);
        assert_eq!(view.worst_ms, 10.0);
        assert_eq!(view.histogram.iter().sum::<u32>(), 4);
    }

    #[test]
    fn gauges_anatomy_sums_update_and_draw() {
        let mut perf = Perf::default();
        let now = Instant::now();
        perf.ingest(
            json!({
                "module": "md10_gauges", "seq": 1, "window_s": 0.25,
                "frames": [[3.0, 16.0]],
                "probes": [
                    ["gauge", "pfd.update", 1, 1000.0, 1000.0],
                    ["gauge", "pfd.draw", 1, 500.0, 500.0],
                    ["gauge", "nd.update", 1, 250.0, 250.0]
                ]
            }),
            now,
        );
        let view = perf
            .view(
                &PerfViewArgs {
                    module: "md10_gauges".into(),
                    ..PerfViewArgs::default()
                },
                now,
            )
            .unwrap();
        assert!(view.gauges);
        assert_eq!(view.anatomy.segments[0].key, "gauge/pfd");
        assert_eq!(view.anatomy.segments[0].ms, 1.5);
        assert_eq!(
            view.gauge_tiles[0],
            GaugeTile {
                slot: "pfd".into(),
                update_ms: 1.0,
                draw_ms: 0.5
            }
        );
    }

    #[test]
    fn gaps_backfill_and_absent_probes() {
        let mut perf = Perf::default();
        let now = Instant::now();
        perf.ingest(systems_sample(1, 4000.0), now);
        perf.ingest(
            json!({ "module": "md10", "seq": 4, "frames": [[1.0, 16.0]],
                    "probes": [["extra", "late", 1, 100.0, 100.0]] }),
            now,
        );
        let m = &perf.modules["md10"];
        assert_eq!(m.gaps, 2);
        // A new probe is back-filled to the same length as the summaries.
        assert_eq!(m.probes["extra/late"].ms_per_frame, vec![0.0, 0.1]);
        // A probe that did not run reads zero.
        assert_eq!(m.probes["stage/hydraulics"].ms_per_frame[1], 0.0);
        assert_eq!(m.probes["stage/hydraulics"].peak_us, 800.0);
    }

    #[test]
    fn spikes_name_the_worst_call() {
        let mut perf = Perf::default();
        let now = Instant::now();
        for seq in 1..=10 {
            perf.ingest(
                json!({ "module": "m", "seq": seq, "frames": [[1.0, 16.0], [1.0, 16.0]],
                        "probes": [["p", "a", 1, 10.0, 10.0]] }),
                now,
            );
        }
        perf.ingest(
            json!({ "module": "m", "seq": 11, "frames": [[25.0, 30.0]],
                    "probes": [["p", "a", 1, 10.0, 10.0], ["p", "b", 1, 20000.0, 20000.0]] }),
            now,
        );
        let s = spikes(&perf.modules["m"], 8, 3.0, 4.0, now);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].max_busy_ms, 25.0);
        assert_eq!(s[0].culprit.as_deref(), Some("p/b"));
        assert_eq!(s[0].culprit_max_us, 20000.0);
    }

    #[test]
    fn filters_pause_reset_and_snapshot() {
        let mut perf = Perf::default();
        let now = Instant::now();
        perf.ingest(systems_sample(1, 4000.0), now);
        let view = perf
            .view(
                &PerfViewArgs {
                    module: "md10".into(),
                    group: Some("stage".into()),
                    query: "HYDR".into(),
                    all_probes: false,
                },
                now,
            )
            .unwrap();
        assert_eq!(view.probes.len(), 1);
        assert_eq!(view.probes[0].name, "hydraulics");
        assert_eq!(view.groups, vec!["phase", "stage"]);

        let text = perf.snapshot("md10").unwrap();
        assert!(text.starts_with("module md10"));
        assert!(text.contains("stage/hydraulics"));

        perf.set_paused(true);
        perf.ingest(systems_sample(2, 4000.0), now);
        assert_eq!(perf.modules["md10"].last_seq, 1);
        perf.reset();
        assert!(perf.overview(now).modules.is_empty());
        assert!(perf.snapshot("md10").is_none());
    }

    #[test]
    fn stale_after_two_seconds() {
        let mut perf = Perf::default();
        let then = Instant::now();
        perf.ingest(systems_sample(1, 4000.0), then);
        assert!(!perf.overview(then).modules[0].stale);
        assert!(perf.overview(then + Duration::from_secs(3)).modules[0].stale);
    }

    #[test]
    fn helpers() {
        assert_eq!(aligned(&[1.0, 2.0], 4), vec![0.0, 0.0, 1.0, 2.0]);
        assert_eq!(aligned(&[1.0, 2.0, 3.0], 2), vec![2.0, 3.0]);
        assert_eq!(percentile(&[5.0, 1.0, 3.0], 50.0), 3.0);
        assert_eq!(mean(&[1.0, 2.0, 3.0, 4.0], 2), 3.5);
        assert_eq!(
            histogram(&[0.1, 3.0, 50.0]),
            vec![1, 0, 0, 0, 1, 0, 0, 0, 1]
        );
        assert_eq!(fmt_ms(123.4), "123");
        assert_eq!(fmt_us(1500.0), "1.50 ms");
        let mut ring = vec![1, 2, 3];
        push(&mut ring, 4, 3);
        assert_eq!(ring, vec![2, 3, 4]);
    }
}
