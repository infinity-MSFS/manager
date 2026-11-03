//! The MD-10 HUD FLIR panel (developer tooling).
//!
//! The picture comes from a nose camera that infinity-viewports adds to the
//! sim (two DLLs injected into the sim), developed by infinity-flir's
//! thermal model and streamed to the HUD gauge. The nose camera looks down
//! the HUD's boresight with the HUD's field (pitch -3.9°, vertical FOV
//! 27.8°), so its centre square is the HUD field.
//!
//! The panel shows what the engine is doing, the frame exactly as the gauge
//! receives it, and edits the settings live (every change applies from the
//! next frame; Save writes `flir/settings.json`).
//!
//! First-flight checklist:
//!   1. Lower the HUD in the MD-10: the state should go attaching, running.
//!      "error" names the missing piece (DLL path, SimConnect, ...).
//!   2. "camera" should read `placed`. `parented` means no pose (SimConnect
//!      missing) and the offsets are ignored; `follow` is the pilot's eye.
//!   3. Bank and climb: the horizon in the preview must stay level with the
//!      world. If it tilts or runs the wrong way, flip "invert bank" /
//!      "invert pitch" (the pose feed's sign convention is unflown).

use crate::editorial::ErrorNote;
use crate::ipc;
use infinity_ipc::IpcError;
use infinity_sim_tools_ipc::flir::{
    FlirDiag, FlirPreview, FlirPreviewArgs, FlirSettings, FlirSettingsSetArgs, FlirState,
};
use infinity_sim_tools_ipc::{
    FlirDiagGet, FlirPreviewGet, FlirSettingsDefault, FlirSettingsGet, FlirSettingsSet, FlirUnload,
};
use infinity_ui::prelude::*;
use leptos::task::spawn_local;

use super::{Section, poll};

const DIAG_POLL_MS: u32 = 250;
const PREVIEW_POLL_MS: u32 = 150;
/// Settings edits are sent after this quiet period: a slider drag is one
/// send at its end.
const APPLY_DEBOUNCE_MS: u32 = 120;

/// Live settings: every edit applies (debounced) without saving.
#[derive(Clone, Copy)]
struct Editor {
    settings: RwSignal<Option<FlirSettings>>,
    dirty: RwSignal<bool>,
    generation: RwSignal<u64>,
    error: RwSignal<Option<IpcError>>,
}

impl Editor {
    fn read<T: Default>(self, f: impl Fn(&FlirSettings) -> T) -> T {
        self.settings
            .with(|s| s.as_ref().map(&f).unwrap_or_default())
    }

    fn edit(self, f: impl FnOnce(&mut FlirSettings)) {
        self.settings.update(|s| {
            if let Some(s) = s {
                f(s);
            }
        });
        self.dirty.set(true);
        let generation = self.generation.get_untracked() + 1;
        self.generation.set(generation);
        spawn_local(async move {
            ipc::sleep(APPLY_DEBOUNCE_MS).await;
            if self.generation.get_untracked() != generation {
                return;
            }
            if let Some(settings) = self.settings.get_untracked() {
                let args = FlirSettingsSetArgs {
                    settings,
                    persist: false,
                };
                if let Err(e) = ipc::invoke::<FlirSettingsSet>(&args).await {
                    self.error.set(Some(e));
                }
            }
        });
    }

    /// A slider bound to one `f32` field.
    fn slider(
        self,
        label: &'static str,
        get: fn(&FlirSettings) -> f32,
        set: fn(&mut FlirSettings, f32),
        range: (f64, f64, f64),
        unit: &'static str,
    ) -> impl IntoView {
        let value = Signal::derive(move || f64::from(self.read(get)));
        let on_input = Callback::new(move |v: f64| self.edit(|s| set(s, v as f32)));
        view! { <SliderRow label=label value=value range=range unit=unit on_input=on_input /> }
    }

    fn toggle(
        self,
        label: &'static str,
        get: fn(&FlirSettings) -> bool,
        set: fn(&mut FlirSettings, bool),
    ) -> impl IntoView {
        view! {
            <div class="py-1.5">
                <Switch
                    checked=Signal::derive(move || self.read(get))
                    on_change=Callback::new(move |v| self.edit(|s| set(s, v)))
                    label=label
                />
            </div>
        }
    }
}

#[component]
pub(super) fn FlirPanel() -> impl IntoView {
    let diag = RwSignal::new(None::<FlirDiag>);
    let editor = Editor {
        settings: RwSignal::new(None),
        dirty: RwSignal::new(false),
        generation: RwSignal::new(0),
        error: RwSignal::new(None),
    };
    let saved = RwSignal::new(false);

    poll(DIAG_POLL_MS, move || async move {
        match ipc::invoke::<FlirDiagGet>(&()).await {
            Ok(d) => diag.set(Some(d)),
            Err(e) => editor.error.set(Some(e)),
        }
    });
    spawn_local(async move {
        match ipc::invoke::<FlirSettingsGet>(&()).await {
            Ok(s) => editor.settings.set(Some(s)),
            Err(e) => editor.error.set(Some(e)),
        }
    });

    let save = Action::new_local(move |(): &()| {
        let settings = editor.settings.get_untracked();
        async move {
            let Some(settings) = settings else { return };
            let args = FlirSettingsSetArgs {
                settings,
                persist: true,
            };
            match ipc::invoke::<FlirSettingsSet>(&args).await {
                Ok(()) => {
                    editor.dirty.set(false);
                    saved.set(true);
                }
                Err(e) => editor.error.set(Some(e)),
            }
        }
    });
    let defaults = Action::new_local(move |(): &()| async move {
        match ipc::invoke::<FlirSettingsDefault>(&()).await {
            Ok(d) => editor.edit(|s| {
                let (bridge_dll, core_dll) = (s.bridge_dll.take(), s.core_dll.take());
                *s = FlirSettings {
                    bridge_dll,
                    core_dll,
                    ..d
                };
            }),
            Err(e) => editor.error.set(Some(e)),
        }
    });
    let unload = Action::new_local(move |(): &()| async move {
        editor.error.set(ipc::invoke::<FlirUnload>(&()).await.err());
    });

    let loaded = move || editor.settings.with(Option::is_some);
    view! {
        <div class="flex flex-col gap-6">
            <StatusStrip diag=diag />
            {move || editor.error.get().map(|e| view! { <ErrorNote error=e /> })}
            <Show when=loaded fallback=|| view! { <Skeleton class="h-80 w-full rounded-sm" /> }>
                <div class="grid grid-cols-1 gap-6 xl:grid-cols-2">
                    <PreviewCard diag=diag />
                    <EngineCard diag=diag editor=editor on_unload=Callback::new(move |()| {
                        unload.dispatch(());
                    }) />
                </div>
                <div class="grid grid-cols-1 gap-6 xl:grid-cols-2">
                    <CameraCard editor=editor />
                    <LookCard editor=editor />
                </div>
                <div class="flex items-center gap-3">
                    <Button
                        variant=Variant::Solid
                        icon=icons::CHECK
                        disabled=Signal::derive(move || !editor.dirty.get())
                        loading=save.pending()
                        on_click=Callback::new(move |()| {
                            save.dispatch(());
                        })
                    >
                        "Save"
                    </Button>
                    <Button
                        variant=Variant::Ghost
                        icon=icons::REFRESH_CW
                        on_click=Callback::new(move |()| {
                            defaults.dispatch(());
                        })
                    >
                        "Defaults"
                    </Button>
                    <span class="text-xs text-white/45">
                        {move || {
                            if editor.dirty.get() {
                                "Live now, not saved: the next session starts from the saved file."
                            } else if saved.get() {
                                "Saved."
                            } else {
                                "As saved."
                            }
                        }}
                    </span>
                </div>
            </Show>
        </div>
    }
}

fn state_label(d: Option<&FlirDiag>) -> &'static str {
    match d {
        None => "-",
        Some(d) => match d.state {
            FlirState::Running => "Running",
            FlirState::Attaching => "Attaching to the sim...",
            FlirState::Error => "Error",
            FlirState::Unsupported => "Needs Windows",
            FlirState::Idle if d.requested => "Starting",
            FlirState::Idle => "Idle: lower the HUD",
        },
    }
}

#[component]
fn Pill(
    on: Signal<bool>,
    #[prop(optional)] warn: Option<Signal<bool>>,
    children: Children,
) -> impl IntoView {
    let warn = move || warn.is_some_and(|w| w.get());
    view! {
        <span class=move || {
            cn!(
                "inline-flex items-center gap-1.5 rounded-full border px-2.5 py-1 text-xs",
                if warn() {
                    "border-amber-400/40 bg-amber-500/10 text-amber-200"
                } else if on.get() {
                    "border-accent-400/40 bg-accent-500/10 text-accent-200"
                } else {
                    "border-white/10 bg-white/5 text-white/45"
                }
            )
        }>
            <span class=move || {
                cn!(
                    "h-1.5 w-1.5 rounded-full",
                    if warn() { "bg-amber-400" } else if on.get() { "bg-accent-400 animate-pulse" } else { "bg-white/25" }
                )
            }></span>
            {children()}
        </span>
    }
}

#[component]
fn StatusStrip(diag: RwSignal<Option<FlirDiag>>) -> impl IntoView {
    let field = move |f: fn(&FlirDiag) -> bool| {
        Signal::derive(move || diag.with(|d| d.as_ref().is_some_and(f)))
    };
    let state = move || diag.with(|d| state_label(d.as_ref()));
    let fps = move || diag.with(|d| d.as_ref().map_or(0.0, |d| d.develop_fps));
    let camera = move || {
        diag.with(|d| {
            d.as_ref()
                .map_or_else(|| "-".to_owned(), |d| d.camera.clone())
        })
    };
    view! {
        <div class="flex flex-wrap items-center gap-3">
            <Pill on=field(|d| d.state == FlirState::Running) warn=field(|d| d.state == FlirState::Error)>
                {state}
            </Pill>
            <Pill on=field(|d| d.requested)>
                {move || if field(|d| d.requested).get() { "HUD asking" } else { "HUD not asking" }}
            </Pill>
            <Pill on=field(|d| d.develop_fps > 1.0)>{move || format!("{:.0} fps", fps())}</Pill>
            <Pill
                on=field(|d| d.camera == "placed")
                warn=field(|d| d.camera == "parented" || d.camera == "follow")
            >
                {move || format!("camera {}", camera())}
            </Pill>
            <Pill on=field(|d| d.pose) warn=field(|d| !d.pose && d.state == FlirState::Running)>
                {move || if field(|d| d.pose).get() { "pose live" } else { "no pose" }}
            </Pill>
            <span class="ml-auto text-xs tabular-nums text-white/35">
                {move || {
                    diag.with(|d| {
                        d.as_ref()
                            .map(|d| format!("{} sent · {} dropped", d.frames_sent, d.frames_failed))
                            .unwrap_or_default()
                    })
                }}
            </span>
        </div>
    }
}

/// The latest frame, drawn the way the HUD draws it (green over black).
#[component]
fn PreviewCard(diag: RwSignal<Option<FlirDiag>>) -> impl IntoView {
    let tinted = RwSignal::new(true);
    let frame = RwSignal::new(None::<FlirPreview>);
    poll(PREVIEW_POLL_MS, move || async move {
        let args = FlirPreviewArgs {
            tinted: tinted.get_untracked(),
            after_seq: frame.with_untracked(|f| f.as_ref().map(|f| f.seq)),
        };
        // An unchanged frame answers nothing; a failed poll retries.
        if let Ok(Some(p)) = ipc::invoke::<FlirPreviewGet>(&args).await {
            frame.set(Some(p));
        }
    });
    let flip = move |()| {
        tinted.update(|t| *t = !*t);
        // Fetch the current frame again in the new colours.
        frame.set(None);
    };
    let aside = ViewFn::from(move || {
        view! {
            <Button variant=Variant::Ghost size=Size::Sm on_click=Callback::new(flip)>
                {move || if tinted.get() { "HUD green" } else { "Gray" }}
            </Button>
        }
    });
    let footer = move || {
        diag.with(|d| {
            d.as_ref()
                .map(|d| {
                    format!(
                        "{}² px · develop {:.1} ms · {} KB/frame on the wire",
                        d.size,
                        d.develop_ms,
                        d.wire_bytes / 1024
                    )
                })
                .unwrap_or_default()
        })
    };
    view! {
        <Section title="What the HUD receives" icon=icons::EYE aside=aside>
            <div class="relative mx-auto aspect-square w-full max-w-[520px] overflow-hidden rounded-sm border border-white/10 bg-black">
                {move || {
                    frame
                        .get()
                        .map(|f| {
                            view! {
                                <img src=f.data_url alt="FLIR frame" class="h-full w-full [image-rendering:pixelated]" />
                            }
                        })
                }}
                <Show when=move || frame.with(Option::is_none)>
                    <div class="absolute inset-0 flex items-center justify-center text-sm text-white/40">
                        "No frame yet: lower the HUD in the MD-10."
                    </div>
                </Show>
                // The boresight: the HUD's aircraft symbol sits 3.9° above centre.
                <div class="pointer-events-none absolute top-[36%] left-1/2 h-3 w-8 -translate-x-1/2 -translate-y-1/2 border-b border-accent-300/60"></div>
            </div>
            <div class="mt-3 text-center text-xs tabular-nums text-white/40">{footer}</div>
        </Section>
    }
}

#[component]
fn Readout(label: &'static str, #[prop(into)] value: Signal<String>) -> impl IntoView {
    view! {
        <div class="flex items-baseline justify-between gap-2 py-0.5 text-xs">
            <span class="text-white/45">{label}</span>
            <span data-selectable class="truncate text-right font-mono tabular-nums text-white/90">
                {move || value.get()}
            </span>
        </div>
    }
}

#[component]
fn SliderRow(
    label: &'static str,
    value: Signal<f64>,
    /// `(min, max, step)`.
    range: (f64, f64, f64),
    unit: &'static str,
    on_input: Callback<f64>,
) -> impl IntoView {
    let (min, max, step) = range;
    let digits = if step < 0.01 {
        3
    } else if step < 0.1 {
        2
    } else if step < 1.0 {
        1
    } else {
        0
    };
    view! {
        <label class="flex flex-col gap-1 py-1.5">
            <span class="flex justify-between text-xs">
                <span class="text-white/70">{label}</span>
                <span class="font-mono tabular-nums text-white/90">
                    {move || format!("{:.digits$}{unit}", value.get())}
                </span>
            </span>
            <Slider value=value on_input=on_input min=min max=max step=step />
        </label>
    }
}

#[component]
fn PathField(
    label: &'static str,
    hint: &'static str,
    found: Signal<Option<String>>,
    value: Signal<String>,
    on_input: Callback<String>,
) -> impl IntoView {
    view! {
        <div class="flex flex-col gap-1 py-1.5">
            <span class="flex justify-between gap-2 text-xs">
                <span class="text-white/70">{label}</span>
                <span class="max-w-[60%] truncate text-white/35" title=move || found.get().unwrap_or_default()>
                    {move || found.get().map_or_else(|| "not found".to_owned(), |f| format!("using {f}"))}
                </span>
            </span>
            <TextInput value=value on_input=on_input placeholder=format!("auto: {hint}") size=Size::Sm />
        </div>
    }
}

#[component]
fn EngineCard(
    diag: RwSignal<Option<FlirDiag>>,
    editor: Editor,
    on_unload: Callback<()>,
) -> impl IntoView {
    let text = move |f: fn(&FlirDiag) -> String| {
        Signal::derive(move || diag.with(|d| d.as_ref().map_or_else(|| "-".to_owned(), f)))
    };
    fn dash(s: &str) -> String {
        if s.is_empty() {
            "-".to_owned()
        } else {
            s.to_owned()
        }
    }
    let aside = ViewFn::from(move || {
        view! {
            <Button variant=Variant::Ghost tone=Tone::Danger size=Size::Sm icon=icons::X on_click=on_unload>
                "Unload from sim"
            </Button>
        }
    });
    let path = move |get: fn(&FlirSettings) -> Option<String>| {
        Signal::derive(move || editor.read(|s| get(s).unwrap_or_default()))
    };
    let set_path = |v: String| (!v.trim().is_empty()).then_some(v);
    view! {
        <Section title="Engine" icon=icons::CPU aside=aside>
            {move || {
                diag.with(|d| d.as_ref().and_then(|d| d.error.clone()))
                    .map(|e| view! { <Callout tone=Tone::Warning class="mb-3">{e}</Callout> })
            }}
            <Readout label="sim pid" value=text(|d| d.pid.map_or_else(|| "-".to_owned(), |p| p.to_string())) />
            <Readout label="nose stream" value=text(|d| format!("{:.0} fps", d.stream_fps)) />
            <Readout label="frames developed" value=text(|d| d.frames_developed.to_string()) />
            <Readout label="camera note" value=text(|d| dash(&d.camera_note)) />
            <Readout
                label="pose feed"
                value=text(|d| if d.pose { "SimConnect live".to_owned() } else { d.pose_error.clone().unwrap_or_else(|| "-".to_owned()) })
            />
            <div class="my-3 h-px bg-white/10"></div>
            <PathField
                label="Bridge DLL"
                hint="infinity_viewports_bridge.dll"
                found=Signal::derive(move || diag.with(|d| d.as_ref().and_then(|d| d.bridge_dll.clone())))
                value=path(|s| s.bridge_dll.clone())
                on_input=Callback::new(move |v: String| editor.edit(|s| s.bridge_dll = set_path(v)))
            />
            <PathField
                label="Core DLL"
                hint="infinity_viewports_core.dll"
                found=Signal::derive(move || diag.with(|d| d.as_ref().and_then(|d| d.core_dll.clone())))
                value=path(|s| s.core_dll.clone())
                on_input=Callback::new(move |v: String| editor.edit(|s| s.core_dll = set_path(v)))
            />
            <p class="mt-2 text-[11px] leading-relaxed text-white/35">
                "Empty searches next to the manager, then the app data folder's flir/viewports, then a sibling infinity-viewports/target. Build the DLLs with the sim closed: a loaded DLL is locked until it is unloaded."
            </p>
            <div class="my-3 h-px bg-white/10"></div>
            {editor.slider("Max frame rate", |s| s.max_fps, |s, v| s.max_fps = v, (1.0, 30.0, 1.0), " fps")}
            {editor.slider(
                "Gray quantisation (1 = lossless)",
                |s| f32::from(s.quant),
                |s, v| s.quant = v.round().clamp(1.0, 16.0) as u8,
                (1.0, 8.0, 1.0),
                "",
            )}
        </Section>
    }
}

#[component]
fn CameraCard(editor: Editor) -> impl IntoView {
    view! {
        <Section title="Nose camera" icon=icons::EYE>
            <p class="mb-2 text-[11px] leading-relaxed text-white/40">
                "Mounted on the airframe, relative to the reference datum. Pitch and vertical FOV must match the HUD field (panel.cfg hud = 2, 0.0, 3.9, 27.8, 27.8) or conformal symbols drift off what they mark."
            </p>
            {editor.slider("Forward", |s| s.camera.forward_m, |s, v| s.camera.forward_m = v, (-10.0, 60.0, 0.5), " m")}
            {editor.slider("Up", |s| s.camera.up_m, |s, v| s.camera.up_m = v, (-10.0, 10.0, 0.1), " m")}
            {editor.slider("Right", |s| s.camera.right_m, |s, v| s.camera.right_m = v, (-10.0, 10.0, 0.1), " m")}
            {editor.slider("Pitch", |s| s.camera.pitch_deg, |s, v| s.camera.pitch_deg = v, (-20.0, 20.0, 0.1), "°")}
            {editor.slider("Yaw", |s| s.camera.yaw_deg, |s, v| s.camera.yaw_deg = v, (-20.0, 20.0, 0.1), "°")}
            {editor.slider("Vertical FOV", |s| s.camera.fov_deg, |s, v| s.camera.fov_deg = v, (10.0, 60.0, 0.1), "°")}
            <div class="my-3 h-px bg-white/10"></div>
            {editor.toggle("Invert pose pitch (MSFS: + is nose down)", |s| s.invert_pitch, |s, v| s.invert_pitch = v)}
            {editor.toggle("Invert pose bank", |s| s.invert_bank, |s, v| s.invert_bank = v)}
        </Section>
    }
}

#[component]
fn LookCard(editor: Editor) -> impl IntoView {
    view! {
        <Section title="Thermal look" icon=icons::SPARKLES>
            {editor.slider("Gamma", |s| s.sensor.gamma, |s, v| s.sensor.gamma = v, (0.3, 8.0, 0.1), "")}
            {editor.slider("Brightness", |s| s.sensor.brightness, |s, v| s.sensor.brightness = v, (0.05, 1.5, 0.01), "")}
            {editor.slider("Detail gain", |s| s.sensor.detail_gain, |s, v| s.sensor.detail_gain = v, (0.0, 4.0, 0.05), "")}
            {editor.slider("AGC clip", |s| s.sensor.agc_clip, |s, v| s.sensor.agc_clip = v, (0.0, 0.2, 0.005), "")}
            {editor.slider("AGC plateau", |s| s.sensor.agc_plateau, |s, v| s.sensor.agc_plateau = v, (0.0, 1.0, 0.01), "")}
            {editor.slider("AGC speed (1 = instant)", |s| s.sensor.agc_speed, |s, v| s.sensor.agc_speed = v, (0.02, 1.0, 0.01), "")}
            {editor.slider("Noise σ", |s| s.sensor.noise_sigma, |s, v| s.sensor.noise_sigma = v, (0.0, 0.03, 0.001), "")}
            <div class="my-3 h-px bg-white/10"></div>
            {editor.toggle("Clouds from the colour layer", |s| s.clouds, |s, v| s.clouds = v)}
            {editor.slider("Cloud altitude", |s| s.cloud_alt_m, |s, v| s.cloud_alt_m = v, (0.0, 12_000.0, 100.0), " m")}
        </Section>
    }
}
