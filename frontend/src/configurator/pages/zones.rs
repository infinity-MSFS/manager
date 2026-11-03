//! Zones page (v3 `ZonesPage.tsx`): `ZONES.training_zones` (airspace and
//! MTR corridors) and `ZONES.no_fly_zones` (circles) in two tabs over one
//! shared map. Right-click the map for nearby airspace and MTRs; arm
//! placement to drop a no-fly zone with a click. Each bucket holds at most
//! ten: the gauge's enable mask is ten bits and it truncates past that.

use infinity_ipc::commands as cmd;
use infinity_ipc::nav::{
    MtrFamily, MtrNearby, MtrRouteSummary, MtrRouteWithDistance, NavNearby, NavSearch,
    RestrictiveAirspaceData, RestrictiveAirspaceDataWithDistance,
};
use infinity_ui::prelude::*;
use serde_json::Value;

use super::PageCtx;
use crate::configurator::map::{InteractiveMap, MapContextMenu, MapLayers, MapPointer, MenuMode};
use crate::configurator::schema::{object_mut, path_value};
use crate::configurator::state::Configurator;
use crate::configurator::widgets::zone_ui::{
    no_fly_center, no_fly_circles, selection_after_remove, training_areas,
};
use crate::configurator::widgets::{
    AddMode, ModePill, NoFlyEdit, NoFlyZoneRow, ResultRow, TrainingZoneRow, debounce, describe,
    mtr_summary_line,
};
use crate::configurator::zones::{
    NoFlyZone, TrainingZone, airspace_type_label, airspace_zone, mtr_zone, read_list, write_list,
};
use crate::editorial::EYEBROW;
use crate::ipc;

const MAX_TRAINING_ZONES: usize = 10;
const MAX_NO_FLY_ZONES: usize = 10;
const SEARCH_DEBOUNCE_MS: u32 = 250;
const NEARBY_NM: f64 = 50.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Training,
    NoFly,
}

fn training_limit_message() -> String {
    format!(
        "Training-zone limit reached ({MAX_TRAINING_ZONES} airspace + MTR combined). Remove one to add another."
    )
}

fn no_fly_limit_message() -> String {
    format!("No-fly limit reached ({MAX_NO_FLY_ZONES}). Remove one to add another.")
}

/// Where the map looks: the selection, else the first training zone, else
/// the first placed no-fly zone, else the middle of the US.
fn map_center(
    training: &[TrainingZone],
    no_fly: &[NoFlyZone],
    selected: Option<(Kind, usize)>,
) -> (f64, f64) {
    let first = |z: &TrainingZone| {
        z.boundary
            .first()
            .and_then(|r| r.first())
            .map(|p| (p.lat, p.lon))
    };
    let placed = |z: &NoFlyZone| (z.lat.is_finite() && z.lon.is_finite()).then_some((z.lat, z.lon));
    let picked = match selected {
        Some((Kind::Training, i)) => training.get(i).and_then(first),
        Some((Kind::NoFly, i)) => no_fly.get(i).and_then(placed),
        None => None,
    };
    picked
        .or_else(|| training.first().and_then(first))
        .unwrap_or_else(|| no_fly_center(no_fly, None))
}

/// Writes both buckets back under `ZONES`, keeping its other keys.
fn write_zones(root: &mut Value, training: &[TrainingZone], no_fly: &[NoFlyZone]) {
    let Some(map) = object_mut(root) else { return };
    let zones = map.entry("ZONES".to_owned()).or_insert(Value::Null);
    if let Some(z) = object_mut(zones) {
        z.insert("training_zones".to_owned(), write_list(training));
        z.insert("no_fly_zones".to_owned(), write_list(no_fly));
    }
}

#[derive(Debug, Clone, Copy)]
struct Zones {
    cfg: Configurator,
    training: Memo<Vec<TrainingZone>>,
    no_fly: Memo<Vec<NoFlyZone>>,
    tab: RwSignal<Kind>,
    selected: RwSignal<Option<(Kind, usize)>>,
}

impl Zones {
    fn new(cfg: Configurator) -> Self {
        let training = Memo::new(move |_| {
            cfg.value
                .with(|v| read_list::<TrainingZone>(&path_value(v, "ZONES.training_zones")))
        });
        let no_fly = Memo::new(move |_| {
            cfg.value
                .with(|v| read_list::<NoFlyZone>(&path_value(v, "ZONES.no_fly_zones")))
        });
        let selected =
            RwSignal::new((!training.with_untracked(Vec::is_empty)).then_some((Kind::Training, 0)));
        Self {
            cfg,
            training,
            no_fly,
            tab: RwSignal::new(Kind::Training),
            selected,
        }
    }

    fn write_training(self, next: Vec<TrainingZone>) {
        let no_fly = self.no_fly.get_untracked();
        self.cfg.update(|root| write_zones(root, &next, &no_fly));
    }

    fn write_no_fly(self, next: Vec<NoFlyZone>) {
        let training = self.training.get_untracked();
        self.cfg.update(|root| write_zones(root, &training, &next));
    }

    fn training_full(self) -> bool {
        self.training.with_untracked(Vec::len) >= MAX_TRAINING_ZONES
    }

    fn no_fly_full(self) -> bool {
        self.no_fly.with_untracked(Vec::len) >= MAX_NO_FLY_ZONES
    }

    /// Appends a training zone and selects it. `false` at the cap.
    fn push_training(self, zone: TrainingZone) -> bool {
        if self.training_full() {
            return false;
        }
        let mut next = self.training.get_untracked();
        next.push(zone);
        let last = next.len() - 1;
        self.write_training(next);
        self.tab.set(Kind::Training);
        self.selected.set(Some((Kind::Training, last)));
        true
    }

    fn add_no_fly_at(self, lat: f64, lon: f64) {
        if self.no_fly_full() {
            self.cfg.toasts.warn("Zones", no_fly_limit_message());
            return;
        }
        let mut next = self.no_fly.get_untracked();
        let mut zone = NoFlyZone::blank(next.len());
        zone.lat = lat;
        zone.lon = lon;
        next.push(zone);
        let last = next.len() - 1;
        self.write_no_fly(next);
        self.tab.set(Kind::NoFly);
        self.selected.set(Some((Kind::NoFly, last)));
    }

    fn remove(self, kind: Kind, i: usize) {
        let remaining = match kind {
            Kind::Training => {
                let mut next = self.training.get_untracked();
                if i < next.len() {
                    next.remove(i);
                }
                let n = next.len();
                self.write_training(next);
                n
            }
            Kind::NoFly => {
                let mut next = self.no_fly.get_untracked();
                if i < next.len() {
                    next.remove(i);
                }
                let n = next.len();
                self.write_no_fly(next);
                n
            }
        };
        self.selected.update(|s| {
            if let Some((k, idx)) = *s
                && k == kind
            {
                *s = selection_after_remove(Some(idx), i, remaining).map(|j| (k, j));
            }
        });
    }

    fn patch_training(self, i: usize, f: Box<dyn FnOnce(&mut TrainingZone)>) {
        let mut next = self.training.get_untracked();
        if let Some(z) = next.get_mut(i) {
            f(z);
        }
        self.write_training(next);
    }

    fn patch_no_fly(self, i: usize, edit: NoFlyEdit) {
        let mut next = self.no_fly.get_untracked();
        if let Some(z) = next.get_mut(i) {
            edit.apply(z);
        }
        self.write_no_fly(next);
    }

    /// From the right-click menu: limit and failures go to toasts.
    fn add_nearby_airspace(self, a: RestrictiveAirspaceDataWithDistance) {
        if self.training_full() {
            self.cfg.toasts.warn("Zones", training_limit_message());
            return;
        }
        let cfg = self.cfg;
        leptos::task::spawn_local(async move {
            match airspace_zone(cfg, a.name, a.designation, a.airspace_type).await {
                Ok(z) => {
                    if !self.push_training(z) {
                        cfg.toasts.warn("Zones", training_limit_message());
                    }
                }
                Err(e) => cfg.toasts.error("Could not add airspace", &e),
            }
        });
    }

    fn add_nearby_mtr(self, m: MtrRouteWithDistance) {
        self.add_mtr(m.route.ident_full, m.route.family, None, None);
    }

    /// Adds an MTR corridor. Problems go to `error` when given (the search
    /// panel), else to toasts (the map menu); `on_added` runs on success.
    fn add_mtr(
        self,
        ident: String,
        family: MtrFamily,
        error: Option<RwSignal<Option<String>>>,
        on_added: Option<Callback<()>>,
    ) {
        let cfg = self.cfg;
        let report = move |msg: String| match error {
            Some(e) => {
                let _ = e.try_set(Some(msg));
            }
            None => cfg.toasts.warn("Zones", msg),
        };
        if self.training_full() {
            report(training_limit_message());
            return;
        }
        leptos::task::spawn_local(async move {
            match mtr_zone(ident.clone(), family).await {
                Ok(Some(z)) => {
                    if self.push_training(z) {
                        if let Some(cb) = on_added {
                            cb.run(());
                        }
                    } else {
                        report(training_limit_message());
                    }
                }
                Ok(None) => report(format!(
                    "{ident} has no segments with a non-zero corridor width; nothing to draw."
                )),
                Err(e) => report(describe(&e)),
            }
        });
    }
}

pub(super) fn page(ctx: PageCtx) -> AnyView {
    let cfg = ctx.cfg;
    let z = Zones::new(cfg);
    let place_mode = RwSignal::new(false);
    use_escape(move || place_mode.set(false));

    let menu = RwSignal::new(None::<MapPointer>);
    let menu_loading = RwSignal::new(false);
    let nearby = RwSignal::new(Vec::<RestrictiveAirspaceDataWithDistance>::new());
    let nearby_mtrs = RwSignal::new(Vec::<MtrRouteWithDistance>::new());
    // Ignores answers for a menu that was replaced or closed.
    let menu_generation = StoredValue::new(0u64);

    let on_right_click = Callback::new(move |at: MapPointer| {
        menu.set(Some(at));
        menu_loading.set(true);
        nearby.set(Vec::new());
        nearby_mtrs.set(Vec::new());
        let Some(ticket) = menu_generation.try_update_value(|g| {
            *g += 1;
            *g
        }) else {
            return;
        };
        leptos::task::spawn_local(async move {
            let airspace_args = NavNearby {
                product: cfg.product_ref(),
                lat: at.lat,
                lon: at.lon,
                max_nm: NEARBY_NM,
            };
            let mtr_args = MtrNearby {
                lat: at.lat,
                lon: at.lon,
                max_nm: NEARBY_NM,
                limit: None,
            };
            // Both lookups at once; one failing leaves the other's results.
            let (airspaces, mtrs) = futures_join(
                ipc::invoke::<cmd::NavNearbyAirspace>(&airspace_args),
                ipc::invoke::<cmd::MtrNearbyRoutes>(&mtr_args),
            )
            .await;
            if menu_generation.try_get_value() != Some(ticket) {
                return;
            }
            let _ = nearby.try_set(airspaces.unwrap_or_default());
            let _ = nearby_mtrs.try_set(mtrs.unwrap_or_default());
            let _ = menu_loading.try_set(false);
        });
    });
    let on_click = Callback::new(move |(lat, lon): (f64, f64)| {
        if !place_mode.get_untracked() {
            return;
        }
        z.add_no_fly_at(lat, lon);
        place_mode.set(false);
    });

    let layers = Signal::derive(move || MapLayers {
        areas: z.training.with(|t| training_areas(t)),
        circles: z.no_fly.with(|n| no_fly_circles(n)),
        ..MapLayers::default()
    });
    let center = Signal::derive(move || {
        z.training
            .with(|t| z.no_fly.with(|n| map_center(t, n, z.selected.get())))
    });
    let zoom = Signal::derive(move || {
        let any = z.selected.get().is_some()
            || z.training.with(|t| !t.is_empty())
            || z.no_fly.with(|n| !n.is_empty());
        if any { 8.0 } else { 5.0 }
    });

    let tab_items = Signal::derive(move || {
        vec![
            TabItem::new(
                "training",
                format!(
                    "Training ({}/{MAX_TRAINING_ZONES})",
                    z.training.with(Vec::len)
                ),
            ),
            TabItem::new(
                "nofly",
                format!("No-fly ({}/{MAX_NO_FLY_ZONES})", z.no_fly.with(Vec::len)),
            ),
        ]
    });

    view! {
        <div class="flex flex-col">
            <header class="mb-10">
                <p class=cn!("mb-4", EYEBROW)>"Training areas · No-fly"</p>
                <h2 class="text-3xl font-extralight tracking-tight">"Zones"</h2>
            </header>
            <div class="flex h-[680px] min-h-0 gap-6">
                <aside class="flex min-h-0 w-96 shrink-0 flex-col gap-4">
                    <Tabs
                        value=Signal::derive(move || {
                            if z.tab.get() == Kind::Training { "training" } else { "nofly" }.to_owned()
                        })
                        items=tab_items
                        full_width=true
                        on_change=Callback::new(move |v: String| {
                            z.tab.set(if v == "nofly" { Kind::NoFly } else { Kind::Training });
                        })
                    />
                    <div class="min-h-0 flex-1 overflow-y-auto pr-1">
                        <Show
                            when=move || z.tab.get() == Kind::Training
                            fallback=move || view! { <NoFlyPanel z=z place_mode=place_mode /> }
                        >
                            <TrainingPanel z=z />
                        </Show>
                    </div>
                </aside>
                <div class="relative min-h-0 flex-1">
                    <InteractiveMap
                        center=center
                        zoom=zoom
                        layers=layers
                        on_click=on_click
                        on_right_click=on_right_click
                    />
                    <div class=move || {
                        cn!(
                            "pointer-events-none absolute bottom-4 left-4 z-[500] max-w-[80%] rounded-xl border px-3 py-2 text-xs backdrop-blur",
                            if place_mode.get() {
                                "border-fuchsia-400/40 bg-fuchsia-500/15 text-white"
                            } else {
                                "border-white/10 bg-black/70 text-white/60"
                            },
                        )
                    }>
                        {move || {
                            if place_mode.get() {
                                "Placement armed: click the map to drop a no-fly zone (Esc to cancel)"
                            } else {
                                "Arm \u{201c}Place no-fly zone\u{201d} in the No-fly tab to drop a zone · Right-click for nearby airspace and MTRs"
                            }
                        }}
                    </div>
                </div>
            </div>
            {move || {
                menu.get()
                    .map(|at| {
                        view! {
                            <MapContextMenu
                                at=at
                                mode=MenuMode::Airspaces
                                loading=menu_loading
                                airspaces=nearby
                                mtrs=nearby_mtrs
                                on_close=Callback::new(move |()| {
                                    let _ = menu_generation.try_update_value(|g| *g += 1);
                                    menu.set(None);
                                    menu_loading.set(false);
                                })
                                on_airspace=Callback::new(move |a| z.add_nearby_airspace(a))
                                on_mtr=Callback::new(move |m| z.add_nearby_mtr(m))
                            />
                        }
                    })
            }}
        </div>
    }
    .into_any()
}

/// Awaits two futures concurrently.
async fn futures_join<A, B>(a: A, b: B) -> (A::Output, B::Output)
where
    A: std::future::Future,
    B: std::future::Future,
{
    use std::pin::pin;
    use std::task::Poll;

    let mut a = pin!(a);
    let mut b = pin!(b);
    let mut ra = None;
    let mut rb = None;
    std::future::poll_fn(move |cx| {
        if ra.is_none()
            && let Poll::Ready(v) = a.as_mut().poll(cx)
        {
            ra = Some(v);
        }
        if rb.is_none()
            && let Poll::Ready(v) = b.as_mut().poll(cx)
        {
            rb = Some(v);
        }
        if ra.is_some() && rb.is_some() {
            match (ra.take(), rb.take()) {
                (Some(x), Some(y)) => Poll::Ready((x, y)),
                _ => Poll::Pending,
            }
        } else {
            Poll::Pending
        }
    })
    .await
}

#[component]
fn LimitNote(count: Signal<usize>, max: usize) -> impl IntoView {
    move || {
        (count.get() >= max).then(|| {
            view! {
                <p class="border-l border-amber-400/50 py-1 pl-4 text-xs font-light text-amber-200/90">
                    {format!("Limit reached ({}/{max}). Remove a zone to add another.", count.get())}
                </p>
            }
        })
    }
}

#[component]
fn TrainingPanel(z: Zones) -> impl IntoView {
    let cfg = z.cfg;
    let mode = RwSignal::new(AddMode::Airspace);
    let search = RwSignal::new(String::new());
    let airspace_hits = RwSignal::new(Vec::<RestrictiveAirspaceData>::new());
    let mtr_hits = RwSignal::new(Vec::<MtrRouteSummary>::new());
    let searching = RwSignal::new(false);
    let error = RwSignal::new(None::<String>);
    let count = Signal::derive(move || z.training.with(Vec::len));

    debounce(
        move || (search.get(), mode.get()),
        SEARCH_DEBOUNCE_MS,
        move |(query, m): (String, AddMode)| {
            error.set(None);
            if m == AddMode::Airspace && query.chars().count() < 2 {
                airspace_hits.set(Vec::new());
                searching.set(false);
                return;
            }
            searching.set(true);
            leptos::task::spawn_local(async move {
                // MTR search covers the bundled database; an empty query
                // lists every route.
                let result = match m {
                    AddMode::Airspace => {
                        let args = NavSearch {
                            product: cfg.product_ref(),
                            query,
                        };
                        ipc::invoke::<cmd::NavSearchAirspace>(&args)
                            .await
                            .map(|rows| {
                                let _ = airspace_hits.try_set(rows.into_iter().take(30).collect());
                            })
                    }
                    AddMode::Mtr => ipc::invoke::<cmd::MtrSearch>(&query).await.map(|rows| {
                        let _ = mtr_hits.try_set(rows.into_iter().take(60).collect());
                    }),
                };
                if let Err(e) = result {
                    let _ = airspace_hits.try_set(Vec::new());
                    let _ = mtr_hits.try_set(Vec::new());
                    let _ = error.try_set(Some(describe(&e)));
                }
                let _ = searching.try_set(false);
            });
        },
    );

    let clear_search = move || {
        search.set(String::new());
        airspace_hits.set(Vec::new());
        mtr_hits.set(Vec::new());
    };
    let add_airspace = move |row: RestrictiveAirspaceData| {
        error.set(None);
        if z.training_full() {
            error.set(Some(training_limit_message()));
            return;
        }
        leptos::task::spawn_local(async move {
            match airspace_zone(cfg, row.name, row.designation, row.airspace_type).await {
                Ok(zone) => {
                    if z.push_training(zone) {
                        clear_search();
                    } else {
                        let _ = error.try_set(Some(training_limit_message()));
                    }
                }
                Err(e) => {
                    let _ = error.try_set(Some(describe(&e)));
                }
            }
        });
    };
    let add_mtr = move |row: MtrRouteSummary| {
        error.set(None);
        z.add_mtr(
            row.ident_full,
            row.family,
            Some(error),
            Some(Callback::new(move |()| clear_search())),
        );
    };

    view! {
        <div class="flex flex-col gap-3">
            <ModePill
                mode=mode
                on_change=Callback::new(move |m| {
                    mode.set(m);
                    clear_search();
                })
            />
            <div class="relative">
                {move || {
                    let placeholder = if mode.get() == AddMode::Airspace {
                        "Search by name (WESTOVER, R-5107…)"
                    } else {
                        "Search MTR (VR1064, IR, 002…)"
                    };
                    view! {
                        <TextInput
                            value=search
                            icon=icons::SEARCH
                            placeholder=placeholder
                            on_input=Callback::new(move |s| search.set(s))
                        />
                    }
                }}
                {move || {
                    searching
                        .get()
                        .then(|| view! { <div class="absolute top-2.5 right-3"><Spinner size=Size::Xs /></div> })
                }}
            </div>
            <LimitNote count=count max=MAX_TRAINING_ZONES />
            {move || error.get().map(|e| view! { <p data-selectable class="text-xs text-red-300">{e}</p> })}
            {move || {
                let rows = airspace_hits.get();
                (mode.get() == AddMode::Airspace && !rows.is_empty())
                    .then(|| {
                        view! {
                            <div class="max-h-56 overflow-y-auto border-y border-white/10">
                                {rows
                                    .into_iter()
                                    .map(|row| {
                                        let detail = format!(
                                            "{} · {}",
                                            row.designation,
                                            airspace_type_label(&row.airspace_type),
                                        );
                                        let name = row.name.clone();
                                        view! {
                                            <ResultRow
                                                title=name
                                                detail=detail
                                                on_pick=Callback::new(move |()| add_airspace(row.clone()))
                                            />
                                        }
                                    })
                                    .collect_view()}
                            </div>
                        }
                    })
            }}
            {move || {
                let rows = mtr_hits.get();
                (mode.get() == AddMode::Mtr && !rows.is_empty())
                    .then(|| {
                        view! {
                            <div class="max-h-56 overflow-y-auto border-y border-white/10">
                                {rows
                                    .into_iter()
                                    .map(|row| {
                                        let title = if row.is_low_level {
                                            format!("{} · LL", row.ident_full)
                                        } else {
                                            row.ident_full.clone()
                                        };
                                        let detail = mtr_summary_line(&row);
                                        view! {
                                            <ResultRow
                                                title=title
                                                detail=detail
                                                on_pick=Callback::new(move |()| add_mtr(row.clone()))
                                            />
                                        }
                                    })
                                    .collect_view()}
                            </div>
                        }
                    })
            }}
            <div class="flex flex-col gap-2">
                {move || {
                    z.training
                        .with(Vec::is_empty)
                        .then(|| {
                            view! {
                                <p class="border-t border-white/10 py-6 text-center text-sm font-light text-white/40">
                                    "No training zones yet. Search above to add one."
                                </p>
                            }
                        })
                }}
                <For each=move || 0..z.training.with(Vec::len) key=|i| *i let:i>
                    <TrainingZoneRow
                        index=i
                        zone=Signal::derive(move || z.training.with(|t| t.get(i).cloned()))
                        selected=Signal::derive(move || z.selected.get() == Some((Kind::Training, i)))
                        on_select=Callback::new(move |()| z.selected.set(Some((Kind::Training, i))))
                        on_remove=Callback::new(move |()| z.remove(Kind::Training, i))
                        on_name=Callback::new(move |s: String| z.patch_training(i, Box::new(move |t| t.name = s)))
                        on_designation=Callback::new(move |s: String| {
                            z.patch_training(i, Box::new(move |t| t.designation = s));
                        })
                        on_color=Callback::new(move |c| z.patch_training(i, Box::new(move |t| t.color = c)))
                    />
                </For>
            </div>
        </div>
    }
}

#[component]
fn NoFlyPanel(z: Zones, place_mode: RwSignal<bool>) -> impl IntoView {
    let count = Signal::derive(move || z.no_fly.with(Vec::len));
    let at_limit = Signal::derive(move || count.get() >= MAX_NO_FLY_ZONES);
    let toggle = move |()| {
        if !place_mode.get_untracked() && z.no_fly_full() {
            z.cfg.toasts.warn("Zones", no_fly_limit_message());
            return;
        }
        place_mode.update(|p| *p = !*p);
    };
    view! {
        <div class="flex flex-col gap-3">
            <div>
                {move || {
                    let armed = place_mode.get();
                    view! {
                        <Button
                            variant=if armed { Variant::Solid } else { Variant::Glass }
                            size=Size::Sm
                            icon=icons::PLUS
                            disabled=Signal::derive(move || at_limit.get() && !place_mode.get())
                            on_click=Callback::new(toggle)
                        >
                            {if armed { "Cancel placement (Esc)" } else { "Place no-fly zone" }}
                        </Button>
                    }
                }}
            </div>
            <LimitNote count=count max=MAX_NO_FLY_ZONES />
            {move || {
                z.no_fly
                    .with(Vec::is_empty)
                    .then(|| {
                        view! {
                            <p class="border-t border-white/10 py-6 text-center text-sm font-light text-white/40">
                                "No no-fly zones yet."
                            </p>
                        }
                    })
            }}
            <For each=move || 0..z.no_fly.with(Vec::len) key=|i| *i let:i>
                <NoFlyZoneRow
                    zone=Signal::derive(move || z.no_fly.with(|n| n.get(i).cloned()))
                    selected=Signal::derive(move || z.selected.get() == Some((Kind::NoFly, i)))
                    on_select=Callback::new(move |()| z.selected.set(Some((Kind::NoFly, i))))
                    on_remove=Callback::new(move |()| z.remove(Kind::NoFly, i))
                    on_edit=Callback::new(move |edit: NoFlyEdit| z.patch_no_fly(i, edit))
                />
            </For>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::configurator::map::US_CENTER;
    use crate::configurator::zones::LatLon;

    fn tz(lat: f64, lon: f64) -> TrainingZone {
        TrainingZone {
            name: "T".into(),
            designation: String::new(),
            zone_type: String::new(),
            color: crate::configurator::zones::ZoneColor::Yellow,
            boundary: vec![vec![LatLon { lat, lon }]],
            extra: serde_json::Map::new(),
        }
    }

    #[test]
    fn center_prefers_selection_then_training_then_no_fly() {
        let t = [tz(1.0, 2.0), tz(3.0, 4.0)];
        let mut n = NoFlyZone::blank(0);
        n.lat = 5.0;
        n.lon = 6.0;
        let n = [n];
        assert_eq!(map_center(&t, &n, Some((Kind::Training, 1))), (3.0, 4.0));
        assert_eq!(map_center(&t, &n, Some((Kind::NoFly, 0))), (5.0, 6.0));
        assert_eq!(map_center(&t, &n, None), (1.0, 2.0));
        assert_eq!(map_center(&[], &n, None), (5.0, 6.0));
        assert_eq!(map_center(&[], &[], None), US_CENTER);
    }

    #[test]
    fn writes_both_buckets_and_keeps_other_keys() {
        let mut root = json!({ "ZONES": { "keep": 1 }, "OTHER": true });
        write_zones(&mut root, &[tz(1.0, 2.0)], &[NoFlyZone::blank(0)]);
        assert_eq!(root["ZONES"]["keep"], json!(1));
        assert_eq!(root["OTHER"], json!(true));
        assert_eq!(root["ZONES"]["training_zones"][0]["name"], json!("T"));
        assert_eq!(root["ZONES"]["no_fly_zones"][0]["radius"], json!(5.0));

        let mut empty = Value::Null;
        write_zones(&mut empty, &[], &[]);
        assert_eq!(
            empty,
            json!({ "ZONES": { "training_zones": [], "no_fly_zones": [] } })
        );
    }
}
