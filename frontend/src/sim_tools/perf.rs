//! Developer-only live performance page for the 10-systems WASM modules,
//! with the MD-10 HUD FLIR panel as its second tab.
//!
//! The plugin keeps every module's history (samples from a
//! `cargo fs-build --perf` build, four a second per module) and hands this
//! page one module at a time, analysed. Each module (`md10`, `md10_gauges`,
//! and the DC/KC equivalents) is its own tab: they are separate WASM
//! instances with separate frames.

use crate::editorial::{EYEBROW, ErrorNote, Page, PageHead};
use crate::ipc;
use infinity_ipc::IpcError;
use infinity_sim_tools_ipc::perf::{PerfOverview, PerfView, PerfViewArgs, Spike};
use infinity_sim_tools_ipc::{
    PerfOverviewGet, PerfReset, PerfSetPaused, PerfSnapshot, PerfViewGet,
};
use infinity_ui::prelude::*;

use super::charts::{
    AnatomyBar, FrameChart, Histogram, Sparkline, StackedArea, fmt_ms, fmt_us, group_stroke,
};
use super::{Section, poll};

/// The page refresh: samples arrive four times a second.
const REFRESH_MS: u32 = 300;
const BUDGET_60_MS: f64 = 1000.0 / 60.0;
const BUDGET_30_MS: f64 = 1000.0 / 30.0;
/// Probe rows the plugin sends unless asked for all.
const TOP_ROWS: usize = 40;

#[component]
pub fn Perf() -> impl IntoView {
    let tab = RwSignal::new("perf".to_owned());
    let tabs = Signal::derive(|| {
        vec![
            TabItem::new("perf", "Performance").with_icon(icons::GAUGE),
            TabItem::new("flir", "FLIR").with_icon(icons::EYE),
        ]
    });
    view! {
        <Page>
            <PageHead
                eyebrow="Infinity / Developer"
                title="Perf"
                accent="live."
                lead="Timings from a 10-systems build with its probes compiled in (cargo fs-build --perf), and the MD-10 HUD's FLIR. A normal build sends nothing."
                aside=ViewFn::from(move || {
                    view! { <Tabs value=tab on_change=Callback::new(move |v| tab.set(v)) items=tabs /> }
                })
            />
            <Show when=move || tab.get() == "perf" fallback=|| view! { <super::flir::FlirPanel /> }>
                <Performance />
            </Show>
        </Page>
    }
}

#[component]
fn Performance() -> impl IntoView {
    let overview = RwSignal::new(None::<PerfOverview>);
    let view_data = RwSignal::new(None::<PerfView>);
    let error = RwSignal::new(None::<IpcError>);
    let selected = RwSignal::new(None::<String>);
    let group = RwSignal::new(None::<String>);
    let query = RwSignal::new(String::new());
    let all_probes = RwSignal::new(false);
    let copied = RwSignal::new(false);

    let active = Memo::new(move |_| {
        let names: Vec<String> = overview.with(|o| {
            o.as_ref()
                .map(|o| o.modules.iter().map(|m| m.module.clone()).collect())
                .unwrap_or_default()
        });
        selected
            .get()
            .filter(|s| names.contains(s))
            .or_else(|| names.first().cloned())
    });

    poll(REFRESH_MS, move || async move {
        match ipc::invoke::<PerfOverviewGet>(&()).await {
            Ok(o) => {
                overview.set(Some(o));
                error.set(None);
            }
            Err(e) => return error.set(Some(e)),
        }
        let Some(module) = active.get_untracked() else {
            view_data.set(None);
            return;
        };
        let args = PerfViewArgs {
            module,
            group: group.get_untracked(),
            query: query.get_untracked(),
            all_probes: all_probes.get_untracked(),
        };
        match ipc::invoke::<PerfViewGet>(&args).await {
            Ok(v) => view_data.set(v),
            Err(e) => error.set(Some(e)),
        }
    });

    let paused = move || overview.with(|o| o.as_ref().is_some_and(|o| o.paused));
    let toggle_pause = Action::new_local(move |(): &()| {
        let next = !paused();
        async move { error.set(ipc::invoke::<PerfSetPaused>(&next).await.err()) }
    });
    let reset = Action::new_local(move |(): &()| async move {
        error.set(ipc::invoke::<PerfReset>(&()).await.err());
        view_data.set(None);
    });
    let copy = Action::new_local(move |(): &()| {
        let module = active.get_untracked();
        async move {
            let Some(module) = module else { return };
            match ipc::invoke::<PerfSnapshot>(&module).await {
                Ok(Some(text)) => {
                    write_clipboard(&text);
                    copied.set(true);
                    ipc::sleep(1500).await;
                    copied.set(false);
                }
                Ok(None) => {}
                Err(e) => error.set(Some(e)),
            }
        }
    });

    let toolbar = move || {
        view! {
            <div class="flex items-center gap-2">
                <Button
                    variant=Variant::Ghost
                    size=Size::Sm
                    icon=icons::COPY
                    disabled=Signal::derive(move || active.get().is_none())
                    on_click=Callback::new(move |()| {
                        copy.dispatch(());
                    })
                >
                    {move || if copied.get() { "Copied" } else { "Copy snapshot" }}
                </Button>
                <Button
                    variant=Variant::Ghost
                    size=Size::Sm
                    tone=if paused() { Tone::Warning } else { Tone::Accent }
                    icon=if paused() { icons::PLAY } else { icons::PAUSE }
                    on_click=Callback::new(move |()| {
                        toggle_pause.dispatch(());
                    })
                >
                    {if paused() { "Resume" } else { "Pause" }}
                </Button>
                <Button
                    variant=Variant::Ghost
                    size=Size::Sm
                    icon=icons::REFRESH_CW
                    on_click=Callback::new(move |()| {
                        reset.dispatch(());
                    })
                >
                    "Reset"
                </Button>
            </div>
        }
    };

    let body = move || {
        let modules = overview.with(|o| o.as_ref().map(|o| o.modules.clone()));
        match modules {
            None => view! { <Skeleton class="h-40 w-full rounded-sm" /> }.into_any(),
            Some(m) if m.is_empty() => view! { <Waiting /> }.into_any(),
            Some(modules) => {
                let tabs = modules
                    .into_iter()
                    .map(|m| {
                        let name = m.module.clone();
                        let pick = name.clone();
                        let is_active = move || active.get().as_deref() == Some(name.as_str());
                        view! {
                            <button
                                type="button"
                                on:click=move |_| selected.set(Some(pick.clone()))
                                class=move || {
                                    cn!(
                                        "flex items-center gap-3 rounded-sm border px-4 py-2.5 text-left transition-colors",
                                        if is_active() {
                                            "border-accent-400/40 bg-accent-500/10"
                                        } else {
                                            "border-white/10 bg-white/5 hover:bg-white/10"
                                        }
                                    )
                                }
                            >
                                <StatusDot tone=if m.stale { Tone::Neutral } else { Tone::Accent } pulse=!m.stale />
                                <span class="flex flex-col">
                                    <span class="font-mono text-sm text-white/90">{m.module.clone()}</span>
                                    <span class="text-[10px] tracking-widest text-white/40 uppercase">
                                        {if m.gauges { "gauges" } else { "systems" }}
                                    </span>
                                </span>
                                <span class="w-24 text-right text-xs tabular-nums text-white/60">
                                    {if m.stale { "stale".to_owned() } else { format!("{} ms/frame", fmt_ms(m.busy_ms)) }}
                                </span>
                                <Sparkline values=m.busy_series stroke="#34d399" width=80.0 height=22.0 />
                            </button>
                        }
                    })
                    .collect_view();
                view! {
                    <div class="flex flex-col gap-6">
                        <div class="flex flex-wrap gap-3">{tabs}</div>
                        {move || view_data.get().map(|v| view! { <ModuleView view=v /> })}
                    </div>
                }
                .into_any()
            }
        }
    };
    let has_view = move || view_data.with(Option::is_some);

    view! {
        <div class="flex flex-col gap-6">
            <div class="flex items-center justify-between gap-4">
                <p class=EYEBROW>"Performance"</p>
                {toolbar}
            </div>
            {move || error.get().map(|e| view! { <ErrorNote error=e /> })}
            {body}
            // Mounted once, so the filter keeps its focus across refreshes.
            <Show when=has_view>
                <Breakdown data=view_data group=group query=query all_probes=all_probes />
            </Show>
        </div>
    }
}

#[component]
fn Waiting() -> impl IntoView {
    view! {
        <EmptyState
            title="Waiting for samples"
            description="Build 10-systems with cargo fs-build --perf and load the aircraft: samples arrive four times a second per module over ws://127.0.0.1:9879/perf. Collection runs in the background, so this page can be opened at any time."
            icon=icons::CLOCK
        />
    }
}

#[component]
fn Tile(
    label: &'static str,
    value: String,
    unit: &'static str,
    #[prop(optional)] warn: bool,
    #[prop(optional)] accent: bool,
) -> impl IntoView {
    view! {
        <div class=cn!(
            "rounded-sm border px-4 py-3",
            if warn { "border-red-400/30 bg-red-500/[0.07]" } else { "border-white/10 bg-white/5" }
        )>
            <div class="text-[10px] tracking-widest text-white/45 uppercase">{label}</div>
            <div class="mt-1 flex items-baseline gap-1.5">
                <span class=cn!(
                    "text-2xl font-semibold tabular-nums",
                    if warn { "text-red-300" } else if accent { "text-accent-300" } else { "text-white/90" }
                )>{value}</span>
                <span class="text-xs text-white/45">{unit}</span>
            </div>
        </div>
    }
}

fn aside(text: impl Into<String>) -> ViewFn {
    let text = text.into();
    ViewFn::from(move || view! { <span class="text-xs text-white/45">{text.clone()}</span> })
}

#[component]
fn ModuleView(view: PerfView) -> impl IntoView {
    let v = view;
    let dash = |x: f64, digits: usize| {
        if x > 0.0 {
            format!("{x:.digits$}")
        } else {
            "-".to_owned()
        }
    };
    let last_tile = match v.steps {
        Some(steps) => {
            view! { <Tile label="Physics steps" value=format!("{steps:.2}") unit="/ frame" /> }
        }
        None => view! { <Tile label="NanoVG prims" value=dash(v.prims, 0) unit="/ frame" /> },
    };
    let anatomy_note = if v.gauges {
        "last 2 s · each gauge's update + draw"
    } else {
        "last 2 s · phases, with advance split into its stages"
    };
    let frame_note = format!(
        "clock step {} · {}",
        if v.clock_res_ns > 0 {
            format!("{} ns", v.clock_res_ns)
        } else {
            "unknown".into()
        },
        if v.gaps > 0 {
            format!("{} samples lost", v.gaps)
        } else {
            "no samples lost".into()
        }
    );
    let frames_note = format!("last {} frames", v.frames.len());
    let anatomy = v.anatomy.clone();
    let segments = v.anatomy.segments.clone();
    let histogram = v.histogram.clone();
    let frames = v.frames.clone();
    let spikes = v.spikes.clone();
    let gauges = v.gauges;
    let heatmap = gauges.then(|| view! { <GaugeHeatmap view=v.clone() /> });

    view! {
        <div class="flex flex-col gap-6">
            <div class="grid grid-cols-2 gap-4 md:grid-cols-4 xl:grid-cols-7">
                <Tile label="Sim frame rate" value=dash(v.fps, 0) unit="fps" />
                <Tile label="Module time" value=fmt_ms(v.anatomy.busy_ms) unit="ms / frame" accent=true />
                <Tile label="p99 frame" value=fmt_ms(v.p99_ms) unit="ms" />
                <Tile label="Worst frame" value=fmt_ms(v.worst_ms) unit="ms" warn={v.worst_ms > BUDGET_30_MS} />
                <Tile
                    label="Over 16.7 ms"
                    value=format!("{:.1}", v.over_budget * 100.0)
                    unit="% of frames"
                    warn={v.over_budget > 0.01}
                />
                {last_tile}
                <Tile label="Host var calls" value=dash(v.host_calls, 0) unit="/ frame" />
            </div>

            <Section title="Frame anatomy" icon=icons::LAYERS aside=aside(anatomy_note)>
                <AnatomyBar anatomy=anatomy />
            </Section>

            <div class="grid grid-cols-1 gap-6 xl:grid-cols-2">
                <Section title="Composition over time" icon=icons::LAYERS aside=aside("last 30 s, ms / frame")>
                    <StackedArea segments=segments />
                </Section>
                <Section title="Frame time distribution" icon=icons::GAUGE aside=aside(frames_note)>
                    <Histogram counts=histogram />
                </Section>
            </div>

            <Section title="Frame time" icon=icons::CLOCK aside=aside(frame_note)>
                <FrameChart frames=frames />
                <div class="mt-3 flex gap-5 text-xs text-white/55">
                    <Legend swatch="bg-accent-400" label="module busy" />
                    <Legend swatch="bg-red-400" label="busy over 16.7 ms" />
                    <Legend swatch="bg-white/40" label="sim frame interval" />
                    <Legend swatch="bg-amber-400/70" label="16.7 / 33.3 ms" />
                </div>
            </Section>

            <div class=cn!("grid grid-cols-1 gap-6", gauges.then_some("xl:grid-cols-2"))>
                <Hitches spikes=spikes />
                {heatmap}
            </div>

            <Counters view=v />
        </div>
    }
}

#[component]
fn Legend(swatch: &'static str, label: &'static str) -> impl IntoView {
    view! {
        <span class="inline-flex items-center gap-1.5">
            <span class=cn!("inline-block h-1.5 w-3 rounded-sm", swatch)></span>
            {label}
        </span>
    }
}

/// Recent samples whose worst frame stood well clear of the median, each
/// traced to the probe with the worst single call in that window.
#[component]
fn Hitches(spikes: Vec<Spike>) -> impl IntoView {
    let list = if spikes.is_empty() {
        view! { <div class="text-sm text-white/45">"None in the last two minutes."</div> }
            .into_any()
    } else {
        spikes
            .into_iter()
            .map(|s| {
                let tone = if s.max_busy_ms > BUDGET_30_MS {
                    "text-red-300"
                } else if s.max_busy_ms > BUDGET_60_MS {
                    "text-amber-300"
                } else {
                    "text-white/80"
                };
                let culprit = match s.culprit {
                    Some(key) => view! {
                        <span class="flex-1 truncate">
                            <span class="text-white/45">"worst call "</span>
                            <span class="font-mono text-white/85">{key}</span>
                            <span class="text-white/45">{format!(" · {}", fmt_us(s.culprit_max_us))}</span>
                        </span>
                    }
                    .into_any(),
                    None => view! { <span class="flex-1 text-white/40">"no probe ran"</span> }.into_any(),
                };
                view! {
                    <li class="flex items-center gap-3 rounded-sm border border-white/10 bg-black/20 px-3 py-2 text-xs">
                        <span class=cn!("w-16 font-semibold tabular-nums", tone)>{format!("{} ms", fmt_ms(s.max_busy_ms))}</span>
                        <span class="w-16 tabular-nums text-white/40">{format!("{} s ago", s.age_ms / 1000)}</span>
                        {culprit}
                    </li>
                }
            })
            .collect_view()
            .into_any()
    };
    view! {
        <Section title="Hitches" icon=icons::ZAP aside=aside("worst frame ≥ 3× median and ≥ 4 ms")>
            <ul class="flex flex-col gap-2">{list}</ul>
        </Section>
    }
}

/// Every gauge as a tile, brighter the more it costs.
#[component]
fn GaugeHeatmap(view: PerfView) -> impl IntoView {
    let max = view
        .gauge_tiles
        .iter()
        .map(|t| t.update_ms + t.draw_ms)
        .fold(1e-6, f64::max);
    let tiles = view
        .gauge_tiles
        .into_iter()
        .map(|t| {
            let total = t.update_ms + t.draw_ms;
            let heat = total / max;
            let rgb = if heat > 0.66 { "248,113,113" } else { "52,211,153" };
            let background = format!(
                "linear-gradient(135deg, rgba({rgb},{:.3}), rgba(255,255,255,0.02))",
                0.06 + 0.5 * heat
            );
            view! {
                <div
                    title=format!("update {} ms · draw {} ms", fmt_ms(t.update_ms), fmt_ms(t.draw_ms))
                    class="rounded-sm border border-white/10 px-3 py-2 transition-colors duration-300"
                    style:background=background
                >
                    <div class="truncate font-mono text-xs text-white/85">{t.slot}</div>
                    <div class="mt-0.5 flex items-baseline justify-between">
                        <span class="text-sm font-semibold tabular-nums text-white/90">{fmt_ms(total)}</span>
                        <span class="text-[10px] tabular-nums text-white/45">
                            {format!("u {} · d {}", fmt_ms(t.update_ms), fmt_ms(t.draw_ms))}
                        </span>
                    </div>
                </div>
            }
        })
        .collect_view();
    view! {
        <Section title="Gauges" icon=icons::MONITOR aside=aside("update + draw, ms / frame")>
            <div class="grid grid-cols-2 gap-2 md:grid-cols-3 2xl:grid-cols-4">{tiles}</div>
        </Section>
    }
}

fn group_chip(group: &str) -> &'static str {
    match group {
        "stage" => "text-sky-300 bg-sky-500/10 border-sky-400/25",
        "phase" => "text-violet-300 bg-violet-500/10 border-violet-400/25",
        "gauge" => "text-emerald-300 bg-emerald-500/10 border-emerald-400/25",
        _ => "text-white/60 bg-white/5 border-white/15",
    }
}

#[component]
fn Breakdown(
    data: RwSignal<Option<PerfView>>,
    group: RwSignal<Option<String>>,
    query: RwSignal<String>,
    all_probes: RwSignal<bool>,
) -> impl IntoView {
    let groups =
        Memo::new(move |_| data.with(|d| d.as_ref().map(|v| v.groups.clone()).unwrap_or_default()));
    let chips = move || {
        std::iter::once(None)
            .chain(groups.get().into_iter().map(Some))
            .map(|g| {
                let label = g.clone().unwrap_or_else(|| "all".into());
                let pick = g.clone();
                view! {
                    <button
                        type="button"
                        on:click=move |_| group.set(pick.clone())
                        class=move || {
                            cn!(
                                "rounded-sm border px-2.5 py-1 text-xs",
                                if group.get() == g {
                                    "border-white/30 bg-white/15 text-white"
                                } else {
                                    "border-white/10 bg-white/5 text-white/60 hover:bg-white/10"
                                }
                            )
                        }
                    >
                        {label}
                    </button>
                }
            })
            .collect_view()
    };
    let rows = move || {
        let Some(view) = data.get() else {
            return ().into_any();
        };
        let busy = view.anatomy.busy_ms;
        view
        .probes
        .into_iter()
        .map(|r| {
            let share = if busy > 0.0 { (r.ms_per_frame / busy).min(1.0) } else { 0.0 };
            let stroke = group_stroke(&r.group);
            view! {
                <tr class="border-t border-white/5 hover:bg-white/[0.03]">
                    <td class="py-1.5 pr-3">
                        <span class="inline-flex items-center gap-2">
                            <span class=cn!("rounded border px-1.5 py-0.5 text-[10px]", group_chip(&r.group))>{r.group.clone()}</span>
                            <span data-selectable class="font-mono text-white/85">{r.name}</span>
                        </span>
                    </td>
                    <td class="py-1.5 pr-3 text-right tabular-nums text-white/90">{fmt_ms(r.ms_per_frame)}</td>
                    <td class="py-1.5 pr-3">
                        <div class="h-1.5 overflow-hidden rounded-full bg-white/5">
                            <div
                                class="h-full rounded-full transition-all duration-300"
                                style:width=format!("{}%", share * 100.0)
                                style:background=stroke
                            ></div>
                        </div>
                    </td>
                    <td class="py-1.5 pr-3 text-right tabular-nums text-white/60">{format!("{:.0}", r.calls_per_sec)}</td>
                    <td class="py-1.5 pr-3 text-right tabular-nums text-white/60">{fmt_us(r.last_avg_us)}</td>
                    <td class="py-1.5 pr-3 text-right tabular-nums text-white/60">{fmt_us(r.last_max_us)}</td>
                    <td class="py-1.5 pr-3 text-right tabular-nums text-white/40">{fmt_us(r.peak_us)}</td>
                    <td class="py-1.5">
                        <Sparkline values=r.series stroke=stroke />
                    </td>
                </tr>
            }
        })
        .collect_view()
        .into_any()
    };
    let matching = Memo::new(move |_| data.with(|d| d.as_ref().map_or(0, |v| v.probes_matching)));
    let more = move || {
        (matching.get() > TOP_ROWS).then(|| {
            view! {
                <button
                    type="button"
                    on:click=move |_| all_probes.update(|a| *a = !*a)
                    class="mt-3 text-xs text-white/55 hover:text-white/85"
                >
                    {move || {
                        if all_probes.get() {
                            format!("Show the top {TOP_ROWS}")
                        } else {
                            format!("Show all {}", matching.get())
                        }
                    }}
                </button>
            }
        })
    };
    view! {
        <Section title="Every probe" icon=icons::CPU aside=aside("last 2 s · flat: stage/... also counts in phase/advance")>
            <div class="mb-4 flex flex-wrap items-center gap-2">
                {chips}
                <div class="ml-auto w-56">
                    <SearchInput value=query on_input=Callback::new(move |q| query.set(q)) size=Size::Sm />
                </div>
            </div>
            <div class="overflow-x-auto">
                <table class="w-full text-xs">
                    <thead>
                        <tr class="text-left text-[10px] tracking-widest text-white/40 uppercase">
                            <th class="py-2 pr-3 font-medium">"Probe"</th>
                            <th class="py-2 pr-3 text-right font-medium">"ms / frame"</th>
                            <th class="w-40 py-2 pr-3 font-medium">"share of frame"</th>
                            <th class="py-2 pr-3 text-right font-medium">"calls / s"</th>
                            <th class="py-2 pr-3 text-right font-medium">"avg / call"</th>
                            <th class="py-2 pr-3 text-right font-medium">"max"</th>
                            <th class="py-2 pr-3 text-right font-medium">"peak"</th>
                            <th class="py-2 font-medium">"30 s"</th>
                        </tr>
                    </thead>
                    <tbody>{rows}</tbody>
                </table>
            </div>
            {more}
        </Section>
    }
}

#[component]
fn Counters(view: PerfView) -> impl IntoView {
    if view.counters.is_empty() {
        return ().into_any();
    }
    let rows = view
        .counters
        .into_iter()
        .map(|c| {
            let per_frame = if c.per_frame < 10.0 {
                format!("{:.2}", c.per_frame)
            } else {
                format!("{:.0}", c.per_frame)
            };
            view! {
                <div class="flex items-center gap-3 rounded-sm border border-white/10 bg-black/20 px-3 py-2">
                    <div class="min-w-0 flex-1">
                        <div class="truncate font-mono text-xs text-white/80">{c.key}</div>
                        <div class="text-[11px] tabular-nums text-white/45">
                            {format!("{per_frame} / frame · {:.0} / s", c.per_sec)}
                        </div>
                    </div>
                    <Sparkline values=c.series stroke="rgba(255,255,255,0.55)" />
                </div>
            }
        })
        .collect_view();
    view! {
        <Section title="Counters" icon=icons::HASH aside=aside("host calls, NanoVG primitives, physics steps")>
            <div class="grid grid-cols-1 gap-3 md:grid-cols-2 xl:grid-cols-3">{rows}</div>
        </Section>
    }
    .into_any()
}
