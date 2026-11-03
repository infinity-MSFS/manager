//! The Perf page's charts: plain SVG and divs, no charting dependency, so a
//! refresh four times a second re-renders cheaply.

use std::collections::HashMap;

use infinity_sim_tools_ipc::perf::{Anatomy, BUCKETS_MS, FrameTime, Segment, SegmentKind};
use infinity_ui::prelude::*;

const BUDGET_60_MS: f64 = 1000.0 / 60.0;
const BUDGET_30_MS: f64 = 1000.0 / 30.0;

fn palette(kind: SegmentKind) -> &'static [&'static str] {
    match kind {
        SegmentKind::Phase => &[
            "#a78bfa", "#c4b5fd", "#8b5cf6", "#ddd6fe", "#7c3aed", "#6d28d9", "#e9d5ff",
        ],
        SegmentKind::Stage => &[
            "#38bdf8", "#22d3ee", "#7dd3fc", "#0ea5e9", "#67e8f9", "#06b6d4", "#bae6fd", "#0284c7",
        ],
        SegmentKind::Gauge => &[
            "#34d399", "#4ade80", "#6ee7b7", "#a3e635", "#10b981", "#86efac", "#bef264", "#22c55e",
            "#a7f3d0", "#65a30d",
        ],
        SegmentKind::Probe => &[
            "#fbbf24", "#f59e0b", "#fcd34d", "#fb923c", "#fdba74", "#f97316", "#fde68a",
        ],
        SegmentKind::Rest => &["#64748b"],
        SegmentKind::Untimed => &["#334155"],
    }
}

/// A colour per segment: shades of its kind, in order.
pub(super) fn colors_for(segments: &[Segment]) -> HashMap<String, &'static str> {
    let mut used: HashMap<SegmentKind, usize> = HashMap::new();
    segments
        .iter()
        .map(|s| {
            let shades = palette(s.kind);
            let i = used.entry(s.kind).or_default();
            let color = shades.get(*i % shades.len()).copied().unwrap_or("#94a3b8");
            *i += 1;
            (s.key.clone(), color)
        })
        .collect()
}

/// The colour of a probe group.
pub(super) fn group_stroke(group: &str) -> &'static str {
    match group {
        "stage" => "#7dd3fc",
        "phase" => "#c4b5fd",
        "gauge" => "#6ee7b7",
        _ => "rgba(255,255,255,0.55)",
    }
}

pub(super) fn fmt_ms(ms: f64) -> String {
    if ms >= 100.0 {
        format!("{ms:.0}")
    } else if ms >= 10.0 {
        format!("{ms:.1}")
    } else {
        format!("{ms:.2}")
    }
}

pub(super) fn fmt_us(us: f64) -> String {
    if us >= 1000.0 {
        format!("{:.2} ms", us / 1000.0)
    } else if us >= 100.0 {
        format!("{us:.0} µs")
    } else {
        format!("{us:.1} µs")
    }
}

fn percentile(values: &[f64], p: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let i = (((p / 100.0) * sorted.len() as f64).floor() as usize).min(sorted.len() - 1);
    sorted.get(i).copied().unwrap_or(0.0)
}

/// The frame as one bar, every segment its share, with a legend that reads
/// as a ranked list.
#[component]
pub(super) fn AnatomyBar(anatomy: Anatomy) -> impl IntoView {
    let hover = RwSignal::new(None::<String>);
    let total = match anatomy.segments.iter().map(|s| s.ms).sum::<f64>() {
        t if t > 0.0 => t,
        _ => 1.0,
    };
    let colors = colors_for(&anatomy.segments);
    let mut ranked = anatomy.segments.clone();
    ranked.sort_by(|a, b| b.ms.total_cmp(&a.ms));

    let bar = anatomy
        .segments
        .iter()
        .filter(|s| s.ms > 0.0)
        .map(|s| {
            let pct = s.ms / total * 100.0;
            let key = s.key.clone();
            let enter = key.clone();
            let color = colors.get(&s.key).copied().unwrap_or("#94a3b8");
            let title = format!("{} - {} ms ({pct:.1} %)", s.label, fmt_ms(s.ms));
            let label = (pct > 7.0).then(|| {
                view! { <span class="truncate px-1 text-[10px] font-medium text-black/70">{s.label.clone()}</span> }
            });
            view! {
                <div
                    title=title
                    on:mouseenter=move |_| hover.set(Some(enter.clone()))
                    on:mouseleave=move |_| hover.set(None)
                    class="flex h-full items-center justify-center overflow-hidden transition-opacity duration-150"
                    style:width=format!("{pct}%")
                    style:background=color
                    style:opacity=move || {
                        if hover.with(|h| h.as_ref().is_some_and(|h| *h != key)) { "0.3" } else { "1" }
                    }
                >
                    {label}
                </div>
            }
        })
        .collect_view();

    let legend = ranked
        .into_iter()
        .map(|s| {
            let key = s.key.clone();
            let enter = key.clone();
            let color = colors.get(&s.key).copied().unwrap_or("#94a3b8");
            view! {
                <div
                    on:mouseenter=move |_| hover.set(Some(enter.clone()))
                    on:mouseleave=move |_| hover.set(None)
                    class=move || {
                        cn!(
                            "flex items-center gap-2 rounded px-1.5 py-0.5 text-xs",
                            hover.with(|h| h.as_deref() == Some(key.as_str())).then_some("bg-white/10")
                        )
                    }
                >
                    <span class="inline-block h-2.5 w-2.5 shrink-0 rounded-sm" style:background=color></span>
                    <span class="flex-1 truncate font-mono text-white/80">{s.label}</span>
                    <span class="tabular-nums text-white/90">{fmt_ms(s.ms)}</span>
                    <span class="w-12 text-right tabular-nums text-white/40">
                        {format!("{:.1}%", s.ms / total * 100.0)}
                    </span>
                </div>
            }
        })
        .collect_view();

    view! {
        <div>
            <div class="flex h-9 w-full overflow-hidden rounded-sm ring-1 ring-white/10">{bar}</div>
            <div class="mt-4 grid grid-cols-1 gap-x-6 gap-y-1.5 md:grid-cols-2 xl:grid-cols-3">{legend}</div>
        </div>
    }
}

/// Each segment's ms per frame over the recent samples, stacked, biggest at
/// the bottom.
#[component]
pub(super) fn StackedArea(segments: Vec<Segment>) -> impl IntoView {
    const W: f64 = 600.0;
    const H: f64 = 200.0;
    let colors = colors_for(&segments);
    let mut ordered = segments;
    ordered.sort_by(|a, b| b.ms.total_cmp(&a.ms));
    let n = ordered.iter().map(|s| s.series.len()).max().unwrap_or(0);
    if n < 2 {
        return view! { <div class="h-[200px] text-sm text-white/40">"Collecting..."</div> }
            .into_any();
    }
    let mut totals = vec![0.0; n];
    for s in &ordered {
        for (t, v) in totals.iter_mut().zip(&s.series) {
            *t += v;
        }
    }
    let top = totals.iter().copied().fold(0.01, f64::max) * 1.1;
    let x = |i: usize| i as f64 / (n - 1) as f64 * W;
    let y = |v: f64| H - v / top * H;
    let mut base = vec![0.0; n];
    let paths = ordered
        .iter()
        .map(|s| {
            let lower = base.clone();
            for (b, v) in base.iter_mut().zip(&s.series) {
                *b += v;
            }
            let up: Vec<String> = base
                .iter()
                .enumerate()
                .map(|(i, v)| format!("{:.1},{:.1}", x(i), y(*v)))
                .collect();
            let down: Vec<String> = lower
                .iter()
                .enumerate()
                .rev()
                .map(|(i, v)| format!("{:.1},{:.1}", x(i), y(*v)))
                .collect();
            let d = format!("M{} L{} Z", up.join(" L"), down.join(" L"));
            let fill = colors.get(&s.key).copied().unwrap_or("#94a3b8");
            view! {
                <path d=d fill=fill fill-opacity="0.85">
                    <title>{s.label.clone()}</title>
                </path>
            }
        })
        .collect_view();
    view! {
        <svg viewBox=format!("0 0 {W} {H}") preserveAspectRatio="none" class="h-[200px] w-full">
            {paths}
            <text x="6" y="14" fill="rgba(255,255,255,0.6)" font-size="12">
                {format!("{} ms", fmt_ms(top))}
            </text>
        </svg>
    }
    .into_any()
}

/// How the module's frames are distributed, on budget-shaped buckets.
#[component]
pub(super) fn Histogram(counts: Vec<u32>) -> impl IntoView {
    let max = counts.iter().copied().max().unwrap_or(0).max(1);
    let total = counts.iter().sum::<u32>().max(1);
    let bars = counts
        .into_iter()
        .enumerate()
        .map(|(i, c)| {
            let hi = BUCKETS_MS.get(i).copied().unwrap_or(f64::INFINITY);
            let lo = if i == 0 {
                0.0
            } else {
                BUCKETS_MS.get(i - 1).copied().unwrap_or(0.0)
            };
            let label = if hi.is_infinite() {
                format!("> {lo}")
            } else {
                format!("≤ {hi}")
            };
            let tone = if hi > BUDGET_30_MS {
                "bg-red-400/80"
            } else if hi > BUDGET_60_MS {
                "bg-amber-400/80"
            } else if hi > 4.0 {
                "bg-sky-400/70"
            } else {
                "bg-accent-400/80"
            };
            let share = f64::from(c) / f64::from(total);
            let pct = if c == 0 {
                String::new()
            } else if share < 0.1 {
                format!("{:.1}%", share * 100.0)
            } else {
                format!("{:.0}%", share * 100.0)
            };
            view! {
                <div class="flex h-full flex-1 flex-col items-center justify-end gap-1">
                    <span class="text-[10px] tabular-nums text-white/50">{pct}</span>
                    <div
                        class=cn!("w-full rounded-t transition-all duration-300", tone)
                        style:height=format!("{}%", f64::from(c) / f64::from(max) * 75.0)
                        style:min-height={if c > 0 { "2px" } else { "0" }}
                    ></div>
                    <span class="whitespace-nowrap text-[10px] tabular-nums text-white/45">{label}</span>
                </div>
            }
        })
        .collect_view();
    view! { <div class="flex h-[200px] items-end gap-2">{bars}</div> }
}

/// Per-frame busy time as bars, the sim's frame interval as a line, with
/// the 60 and 30 fps budgets marked.
#[component]
pub(super) fn FrameChart(frames: Vec<FrameTime>) -> impl IntoView {
    const W: f64 = 1000.0;
    const H: f64 = 180.0;
    if frames.is_empty() {
        return view! { <div class="h-[180px] text-sm text-white/40">"No frames yet."</div> }
            .into_any();
    }
    let values: Vec<f64> = frames
        .iter()
        .flat_map(|f| [f.busy_ms, f.interval_ms])
        .collect();
    let top = (percentile(&values, 99.5) * 1.15).max(20.0);
    let y = |ms: f64| H - ms.min(top) / top * H;
    let step = W / frames.len() as f64;
    let width = (step - 0.4).max(0.6);
    // Two paths of bars rather than a rect per frame: hundreds of frames
    // redraw four times a second.
    let (mut ok, mut over) = (String::new(), String::new());
    for (i, f) in frames.iter().enumerate() {
        let path = if f.busy_ms > BUDGET_60_MS {
            &mut over
        } else {
            &mut ok
        };
        let top_y = y(f.busy_ms);
        path.push_str(&format!(
            "M{:.1},{H} V{top_y:.1} h{width:.2} V{H} Z ",
            i as f64 * step
        ));
    }
    let interval: Vec<String> = frames
        .iter()
        .enumerate()
        .map(|(i, f)| {
            format!(
                "{:.1},{:.1}",
                i as f64 * step + step / 2.0,
                y(f.interval_ms)
            )
        })
        .collect();
    let budgets = [BUDGET_60_MS, BUDGET_30_MS]
        .into_iter()
        .filter(|ms| *ms < top)
        .map(|ms| {
            let at = format!("{:.1}", y(ms));
            view! {
                <line
                    x1="0"
                    x2=W
                    y1=at.clone()
                    y2=at
                    stroke="rgba(251,191,36,0.6)"
                    stroke-dasharray="6 6"
                    vector-effect="non-scaling-stroke"
                />
            }
        })
        .collect_view();
    view! {
        <svg viewBox=format!("0 0 {W} {H}") preserveAspectRatio="none" class="h-[180px] w-full">
            <path d=ok fill="rgba(52,211,153,0.8)" />
            <path d=over fill="rgba(248,113,113,0.9)" />
            <polyline
                points=interval.join(" ")
                fill="none"
                stroke="rgba(255,255,255,0.4)"
                stroke-width="1.2"
                vector-effect="non-scaling-stroke"
            />
            {budgets}
            <text x="6" y="14" fill="rgba(255,255,255,0.5)" font-size="12">
                {format!("{} ms", fmt_ms(top))}
            </text>
        </svg>
    }
    .into_any()
}

/// A small line over the last `samples` values, right-aligned.
#[component]
pub(super) fn Sparkline(
    values: Vec<f64>,
    stroke: &'static str,
    #[prop(default = 120)] samples: usize,
    #[prop(default = 120.0)] width: f64,
    #[prop(default = 24.0)] height: f64,
) -> impl IntoView {
    let tail = &values[values.len().saturating_sub(samples)..];
    if tail.len() < 2 || samples < 2 {
        return view! { <svg width=width height=height></svg> }.into_any();
    }
    let top = tail.iter().copied().fold(1e-6, f64::max);
    let dx = width / (samples - 1) as f64;
    let x0 = width - (tail.len() - 1) as f64 * dx;
    let points: Vec<(f64, f64)> = tail
        .iter()
        .enumerate()
        .map(|(i, v)| (x0 + i as f64 * dx, height - 1.0 - v / top * (height - 2.0)))
        .collect();
    let line: Vec<String> = points
        .iter()
        .map(|(px, py)| format!("{px:.1},{py:.1}"))
        .collect();
    let area = match (points.first(), points.last()) {
        (Some(first), Some(last)) => format!(
            "M{:.1},{height} L{} L{:.1},{height} Z",
            first.0,
            line.join(" L"),
            last.0
        ),
        _ => String::new(),
    };
    view! {
        <svg width=width height=height class="block">
            <path d=area fill=stroke fill-opacity="0.15" />
            <polyline points=line.join(" ") fill="none" stroke=stroke stroke-width="1.2" />
        </svg>
    }
    .into_any()
}
