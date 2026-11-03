//! The configurator's map (v3 `components/map/InteractiveMap.tsx`): Leaflet
//! 1.9.4, vendored under `public/vendor/leaflet/` and loaded on first use
//! (see [`super::assets`]). Tiles are the same as v3: CARTO dark, Esri
//! imagery and the FAA VFR / IFR charts, clamped to US bounds.
//!
//! The component owns one Leaflet map for its lifetime. Layers redraw when
//! [`MapLayers`] changes; the view follows `center` / `zoom`.

mod context_menu;
mod leaflet;

use std::cell::RefCell;
use std::rc::Rc;

use infinity_ui::prelude::*;
use leptos::html::Div;
use serde_json::json;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

pub(crate) use context_menu::{MapContextMenu, MenuMode};

use self::leaflet::{LMap, Layer};
use super::assets;
use super::zones::LatLon;
use crate::state::use_app;

pub(crate) const US_CENTER: (f64, f64) = (39.8283, -98.5795);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum MapType {
    #[default]
    Street,
    Satellite,
    Vfr,
    IfrLow,
    IfrHigh,
}

impl MapType {
    const ALL: [Self; 5] = [
        Self::Street,
        Self::Satellite,
        Self::Vfr,
        Self::IfrLow,
        Self::IfrHigh,
    ];

    fn key(self) -> &'static str {
        match self {
            Self::Street => "street",
            Self::Satellite => "satellite",
            Self::Vfr => "vfr",
            Self::IfrLow => "ifr-low",
            Self::IfrHigh => "ifr-high",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Street => "Street",
            Self::Satellite => "Satellite",
            Self::Vfr => "VFR Sectional",
            Self::IfrLow => "IFR Low",
            Self::IfrHigh => "IFR High",
        }
    }

    fn from_key(key: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|t| t.key() == key)
            .unwrap_or_default()
    }

    /// (min, max) zoom.
    pub(crate) fn zoom_limits(self) -> (f64, f64) {
        match self {
            Self::Street => (0.0, 20.0),
            Self::Satellite => (0.0, 19.0),
            Self::Vfr | Self::IfrLow => (8.0, 12.0),
            Self::IfrHigh => (7.0, 12.0),
        }
    }

    pub(crate) fn clamp(self, zoom: f64) -> f64 {
        let (lo, hi) = self.zoom_limits();
        zoom.clamp(lo, hi)
    }

    /// FAA IFR charts cover only part of the frame; a dark base goes under.
    fn needs_base(self) -> bool {
        matches!(self, Self::IfrLow | Self::IfrHigh)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum PointKind {
    Destination,
    #[default]
    Custom,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MapPoint {
    pub id: String,
    pub lat: f64,
    pub lon: f64,
    pub label: Option<String>,
    pub color: Option<String>,
    pub kind: PointKind,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MapCircle {
    pub id: String,
    pub name: String,
    pub lat: f64,
    pub lon: f64,
    pub radius_nm: f64,
    pub color: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MapArea {
    pub id: String,
    pub name: String,
    /// Closed rings.
    pub boundary: Vec<Vec<LatLon>>,
    pub color: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MapRoute {
    pub id: String,
    pub name: String,
    /// (ident, lat, lon), in order.
    pub waypoints: Vec<(String, f64, f64)>,
    pub color: Option<String>,
}

/// Everything drawn over the tiles.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct MapLayers {
    pub points: Vec<MapPoint>,
    pub circles: Vec<MapCircle>,
    pub areas: Vec<MapArea>,
    pub routes: Vec<MapRoute>,
}

/// A right-click on the map: where, and where on screen.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct MapPointer {
    pub lat: f64,
    pub lon: f64,
    pub x: f64,
    pub y: f64,
}

/// Escapes text for Leaflet popups, which take HTML.
pub(crate) fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// A colour safe to put in an inline style: `#rrggbb`-ish or a word.
fn safe_color(c: &str) -> &str {
    if c.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '#') {
        c
    } else {
        "#3b82f6"
    }
}

type Handler = Closure<dyn Fn(JsValue)>;

struct Live {
    map: LMap,
    overlay: Layer,
    tiles: RefCell<Layer>,
    base: RefCell<Option<Layer>>,
    /// Keeps event closures alive as long as the map.
    handlers: RefCell<Vec<Handler>>,
    observer: RefCell<Option<JsValue>>,
    resize: RefCell<Option<Handler>>,
}

impl Live {
    fn teardown(&self) {
        if let Some(obs) = self.observer.borrow_mut().take()
            && let Ok(f) = js_sys::Reflect::get(&obs, &JsValue::from_str("disconnect"))
            && let Ok(f) = f.dyn_into::<js_sys::Function>()
        {
            let _ = f.call0(&obs);
        }
        self.map.remove();
        self.handlers.borrow_mut().clear();
        self.resize.borrow_mut().take();
    }
}

type Slot = Rc<RefCell<Option<Live>>>;

fn latlng(lat: f64, lon: f64) -> JsValue {
    let a = js_sys::Array::new();
    a.push(&JsValue::from_f64(lat));
    a.push(&JsValue::from_f64(lon));
    a.into()
}

fn js(v: &serde_json::Value) -> JsValue {
    serde::Serialize::serialize(v, &serde_wasm_bindgen::Serializer::json_compatible())
        .unwrap_or(JsValue::UNDEFINED)
}

fn us_bounds() -> JsValue {
    leaflet::lat_lng_bounds(&latlng(14.0, -171.0), &latlng(72.0, -50.0))
}

/// Esri's dark canvas. CARTO's dark basemap (v3's) now watermarks every
/// request without an API key.
const DARK_TILES: &str = "https://server.arcgisonline.com/ArcGIS/rest/services/Canvas/World_Dark_Gray_Base/MapServer/tile/{z}/{y}/{x}";

fn tiles_for(t: MapType) -> Layer {
    let faa = |url: &str, attribution: &str, min_native: f64, max_native: f64| {
        let opts = js(&json!({
            "attribution": attribution,
            "minZoom": min_native,
            "maxZoom": 18,
            "minNativeZoom": min_native,
            "maxNativeZoom": max_native,
            "noWrap": true,
        }));
        let _ = js_sys::Reflect::set(&opts, &JsValue::from_str("bounds"), &us_bounds());
        leaflet::tile_layer(url, &opts)
    };
    match t {
        MapType::Street => leaflet::tile_layer(
            DARK_TILES,
            &js(&json!({
                "attribution": "Tiles &copy; Esri",
                "maxZoom": 20,
                "maxNativeZoom": 16,
            })),
        ),
        MapType::Satellite => leaflet::tile_layer(
            "https://server.arcgisonline.com/ArcGIS/rest/services/World_Imagery/MapServer/tile/{z}/{y}/{x}",
            &js(&json!({ "attribution": "Tiles &copy; Esri", "maxZoom": 19 })),
        ),
        MapType::Vfr => faa(
            "https://tiles.arcgis.com/tiles/ssFJjBXIUyZDrSYZ/arcgis/rest/services/VFR_Sectional/MapServer/tile/{z}/{y}/{x}",
            "FAA VFR Sectional",
            8.0,
            12.0,
        ),
        MapType::IfrLow => faa(
            "https://tiles.arcgis.com/tiles/ssFJjBXIUyZDrSYZ/arcgis/rest/services/IFR_AreaLow/MapServer/tile/{z}/{y}/{x}",
            "FAA IFR Low",
            7.0,
            12.0,
        ),
        MapType::IfrHigh => faa(
            "https://tiles.arcgis.com/tiles/ssFJjBXIUyZDrSYZ/arcgis/rest/services/IFR_High/MapServer/tile/{z}/{y}/{x}",
            "FAA IFR High",
            5.0,
            9.0,
        ),
    }
}

fn dark_base() -> Layer {
    leaflet::tile_layer(
        DARK_TILES,
        &js(&json!({
            "attribution": "Tiles &copy; Esri",
            "maxZoom": 20,
            "maxNativeZoom": 16,
        })),
    )
}

fn marker_icon(color: &str, text: &str, size: u32) -> JsValue {
    let half = size / 2;
    leaflet::div_icon(&js(&json!({
        "html": format!(
            "<div style=\"background-color:{}\">{}</div>",
            safe_color(color),
            escape_html(text)
        ),
        "className": "im-marker",
        "iconSize": [size, size],
        "iconAnchor": [half, half],
        "popupAnchor": [0, -i64::from(half)],
    })))
}

fn draw(live: &Live, layers: &MapLayers, on_point: Option<Callback<String>>) {
    live.overlay.clear_layers();
    let target: &JsValue = live.overlay.as_ref();
    let mut handlers = Vec::new();

    for p in &layers.points {
        let color = p.color.clone().unwrap_or_else(|| {
            if p.kind == PointKind::Destination {
                "#ef4444"
            } else {
                "#3b82f6"
            }
            .to_owned()
        });
        let letter = match p.kind {
            PointKind::Destination => "D",
            PointKind::Custom => "",
        };
        let opts = js_sys::Object::new();
        let _ = js_sys::Reflect::set(
            &opts,
            &JsValue::from_str("icon"),
            &marker_icon(&color, letter, 24),
        );
        let m = leaflet::marker(&latlng(p.lat, p.lon), &opts);
        if let Some(label) = &p.label {
            m.bind_popup(&escape_html(label));
        }
        if let Some(cb) = on_point {
            let id = p.id.clone();
            let h = Closure::<dyn Fn(JsValue)>::new(move |_| cb.run(id.clone()));
            m.on("click", h.as_ref().unchecked_ref());
            handlers.push(h);
        }
        m.add_to(target);
    }

    for z in &layers.circles {
        let color = safe_color(z.color.as_deref().unwrap_or("#facc15")).to_owned();
        let c = leaflet::circle(
            &latlng(z.lat, z.lon),
            &js(&json!({
                "radius": z.radius_nm * 1852.0,
                "color": color,
                "fillColor": color,
                "fillOpacity": 0.2,
                "weight": 2,
            })),
        );
        c.bind_popup(&escape_html(&z.name));
        c.add_to(target);
    }

    for a in &layers.areas {
        let rings: Vec<Vec<[f64; 2]>> = a
            .boundary
            .iter()
            .filter(|r| r.len() >= 3)
            .map(|r| r.iter().map(|p| [p.lat, p.lon]).collect())
            .collect();
        if rings.is_empty() {
            continue;
        }
        let color = safe_color(a.color.as_deref().unwrap_or("#22c55e")).to_owned();
        let poly = leaflet::polygon(
            &js(&json!(rings)),
            &js(&json!({
                "color": color,
                "fillColor": color,
                "fillOpacity": 0.15,
                "weight": 2,
            })),
        );
        poly.bind_popup(&escape_html(&a.name));
        poly.add_to(target);
    }

    for r in &layers.routes {
        let color = safe_color(r.color.as_deref().unwrap_or("#8b5cf6")).to_owned();
        if r.waypoints.len() >= 2 {
            let pts: Vec<[f64; 2]> = r.waypoints.iter().map(|(_, la, lo)| [*la, *lo]).collect();
            let line = leaflet::polyline(
                &js(&json!(pts)),
                &js(&json!({ "color": color, "weight": 3, "opacity": 0.8 })),
            );
            line.bind_popup(&escape_html(&r.name));
            line.add_to(target);
        }
        for (i, (ident, lat, lon)) in r.waypoints.iter().enumerate() {
            let opts = js_sys::Object::new();
            let _ = js_sys::Reflect::set(
                &opts,
                &JsValue::from_str("icon"),
                &marker_icon(&color, &(i + 1).to_string(), 28),
            );
            let m = leaflet::marker(&latlng(*lat, *lon), &opts);
            m.bind_popup(&format!(
                "{}<br/>WP {}: {}",
                escape_html(&r.name),
                i + 1,
                escape_html(ident)
            ));
            m.add_to(target);
        }
    }
    // The first handlers are the map's own; replace the per-marker ones.
    let mut all = live.handlers.borrow_mut();
    all.truncate(MAP_HANDLERS);
    all.extend(handlers);
}

/// Map-level handlers registered at creation; marker handlers follow.
const MAP_HANDLERS: usize = 3;

fn mouse_latlng(e: &JsValue) -> Option<(f64, f64)> {
    let ll = js_sys::Reflect::get(e, &JsValue::from_str("latlng")).ok()?;
    let lat = js_sys::Reflect::get(&ll, &JsValue::from_str("lat"))
        .ok()?
        .as_f64()?;
    let lng = js_sys::Reflect::get(&ll, &JsValue::from_str("lng"))
        .ok()?
        .as_f64()?;
    Some((lat, lng))
}

fn swap_tiles(live: &Live, t: MapType) {
    let (lo, hi) = t.zoom_limits();
    live.map.set_min_zoom(lo);
    live.map.set_max_zoom(hi);
    let z = live.map.get_zoom();
    let clamped = t.clamp(z);
    if (clamped - z).abs() > f64::EPSILON {
        live.map.set_zoom(clamped);
    }
    live.map.remove_layer(&live.tiles.borrow());
    let map_js: &JsValue = live.map.as_ref();
    if t.needs_base() {
        let mut base = live.base.borrow_mut();
        let layer = base.get_or_insert_with(dark_base);
        if !live.map.has_layer(layer) {
            layer.add_to(map_js);
        }
    } else if let Some(base) = live.base.borrow().as_ref()
        && live.map.has_layer(base)
    {
        live.map.remove_layer(base);
    }
    let next = tiles_for(t);
    next.add_to(map_js);
    *live.tiles.borrow_mut() = next;
}

#[component]
pub(crate) fn InteractiveMap(
    #[prop(into)] center: Signal<(f64, f64)>,
    #[prop(into)] zoom: Signal<f64>,
    #[prop(into)] layers: Signal<MapLayers>,
    #[prop(into, optional)] on_click: Option<Callback<(f64, f64)>>,
    #[prop(into, optional)] on_right_click: Option<Callback<MapPointer>>,
    #[prop(into, optional)] on_point_click: Option<Callback<String>>,
    #[prop(default = true)] show_controls: bool,
    #[prop(into, optional)] class: String,
) -> impl IntoView {
    let reduce = use_app().reduce_motion();
    let node = NodeRef::<Div>::new();
    let slot: Slot = Rc::new(RefCell::new(None));
    let slot_store = StoredValue::new_local(slot.clone());
    let ready = RwSignal::new(false);
    let failed = RwSignal::new(None::<String>);
    let map_type = RwSignal::new(MapType::Street);

    let with_live = move |f: &dyn Fn(&Live)| {
        slot_store.with_value(|s| {
            if let Some(live) = s.borrow().as_ref() {
                f(live);
            }
        });
    };

    // Create the map once the container exists and Leaflet has loaded.
    Effect::new(move |created: Option<bool>| {
        if created == Some(true) {
            return true;
        }
        let Some(el) = node.get() else {
            return false;
        };
        let slot = slot_store.get_value();
        let (lat, lon) = center.get_untracked();
        let z = map_type.get_untracked().clamp(zoom.get_untracked());
        let still = reduce.get_untracked();
        leptos::task::spawn_local(async move {
            if let Err(e) = assets::ensure_leaflet().await {
                let _ = failed.try_set(Some(e.message));
                return;
            }
            let (lo, hi) = MapType::Street.zoom_limits();
            let opts = js(&json!({
                "center": [lat, lon],
                "zoom": z,
                "minZoom": lo,
                "maxZoom": hi,
                "zoomControl": false,
                "keyboard": true,
                "zoomAnimation": !still,
                "fadeAnimation": !still,
                "markerZoomAnimation": !still,
            }));
            let html: &web_sys::HtmlElement = el.as_ref();
            let map = match leaflet::map(html, &opts) {
                Ok(m) => m,
                Err(e) => {
                    let _ = failed.try_set(Some(format!("{e:?}")));
                    return;
                }
            };
            let map_js: &JsValue = map.as_ref();
            let tiles = tiles_for(MapType::Street);
            tiles.add_to(map_js);
            let overlay = leaflet::layer_group();
            overlay.add_to(map_js);

            let click = Closure::<dyn Fn(JsValue)>::new(move |e: JsValue| {
                if let (Some(cb), Some(ll)) = (on_click, mouse_latlng(&e)) {
                    cb.run(ll);
                }
            });
            let context = Closure::<dyn Fn(JsValue)>::new(move |e: JsValue| {
                let original = js_sys::Reflect::get(&e, &JsValue::from_str("originalEvent")).ok();
                if let Some(ev) = original
                    .as_ref()
                    .and_then(|o| o.dyn_ref::<web_sys::MouseEvent>())
                {
                    ev.prevent_default();
                    if let (Some(cb), Some((lat, lon))) = (on_right_click, mouse_latlng(&e)) {
                        cb.run(MapPointer {
                            lat,
                            lon,
                            x: f64::from(ev.client_x()),
                            y: f64::from(ev.client_y()),
                        });
                    }
                }
            });
            let zoomend_map = map.clone();
            let zoomend = Closure::<dyn Fn(JsValue)>::new(move |_| {
                let t = map_type.try_get_untracked().unwrap_or_default();
                let z = zoomend_map.get_zoom();
                let c = t.clamp(z);
                if (c - z).abs() > f64::EPSILON {
                    zoomend_map.set_zoom(c);
                }
            });
            map.on("click", click.as_ref().unchecked_ref());
            map.on("contextmenu", context.as_ref().unchecked_ref());
            map.on("zoomend", zoomend.as_ref().unchecked_ref());

            // Leaflet measures its container once; follow resizes (a tab
            // becoming visible, the window growing).
            let resize_map = map.clone();
            let resize = Closure::<dyn Fn(JsValue)>::new(move |_| resize_map.invalidate_size());
            let observer = assets::global("ResizeObserver").and_then(|ctor| {
                let ctor: js_sys::Function = ctor.dyn_into().ok()?;
                let obs =
                    js_sys::Reflect::construct(&ctor, &js_sys::Array::of1(resize.as_ref())).ok()?;
                let observe: js_sys::Function =
                    js_sys::Reflect::get(&obs, &JsValue::from_str("observe"))
                        .ok()?
                        .dyn_into()
                        .ok()?;
                let _ = observe.call1(&obs, html);
                Some(obs)
            });

            *slot.borrow_mut() = Some(Live {
                map,
                overlay,
                tiles: RefCell::new(tiles),
                base: RefCell::new(None),
                handlers: RefCell::new(vec![click, context, zoomend]),
                observer: RefCell::new(observer),
                resize: RefCell::new(Some(resize)),
            });
            let _ = ready.try_set(true);
        });
        true
    });

    // Redraw layers.
    Effect::new(move |_| {
        let current = layers.get();
        if !ready.get() {
            return;
        }
        with_live(&|live| draw(live, &current, on_point_click));
    });

    // Swap tiles with the map type.
    Effect::new(move |prev: Option<MapType>| {
        let t = map_type.get();
        if ready.get() && prev.is_some_and(|p| p != t) {
            with_live(&|live| swap_tiles(live, t));
        }
        t
    });

    // Follow the requested view.
    Effect::new(move |_| {
        let (lat, lon) = center.get();
        let z = zoom.get();
        if !ready.get() {
            return;
        }
        let t = map_type.get_untracked();
        let animate = !reduce.get_untracked();
        with_live(&|live| {
            live.map.set_view(
                &latlng(lat, lon),
                t.clamp(z),
                &js(&json!({ "animate": animate })),
            );
        });
    });

    on_cleanup(move || {
        if let Some(slot) = slot_store.try_get_value()
            && let Some(live) = slot.borrow_mut().take()
        {
            live.teardown();
        }
    });

    let zoom_in = move |()| with_live(&|live| live.map.zoom_in());
    let zoom_out = move |()| {
        let t = map_type.get_untracked();
        with_live(&|live| live.map.set_zoom(t.clamp(live.map.get_zoom() - 1.0)));
    };
    let recenter = move |()| {
        let (lat, lon) = center.get_untracked();
        let t = map_type.get_untracked();
        let z = zoom.get_untracked();
        with_live(&|live| {
            live.map.set_view(
                &latlng(lat, lon),
                t.clamp(z),
                &js(&json!({ "animate": false })),
            );
        });
    };
    let options = SelectGroup::new(
        "",
        MapType::ALL
            .into_iter()
            .map(|t| SelectOption::new(t.key(), t.label()))
            .collect(),
    );

    view! {
        <div class=cn!("relative h-full w-full overflow-hidden rounded-sm border border-white/10 bg-[#0a0a0a]", class)>
            <div node_ref=node class="im-map absolute inset-0 z-0"></div>
            {move || {
                failed
                    .get()
                    .map(|e| {
                        view! {
                            <div class="absolute inset-0 z-10 flex items-center justify-center p-6 text-center text-sm font-light text-white/55">
                                {format!("The map could not load: {e}")}
                            </div>
                        }
                    })
            }}
            {show_controls
                .then(|| {
                    view! {
                        <div class="absolute top-3 right-3 z-[1000] flex flex-col items-end gap-2">
                            <div class="w-44">
                                <Select
                                    value=Signal::derive(move || map_type.get().key().to_owned())
                                    groups=Signal::stored(vec![options])
                                    size=Size::Sm
                                    on_change=Callback::new(move |k: String| map_type.set(MapType::from_key(&k)))
                                />
                            </div>
                            <div class="flex flex-col gap-1">
                                <IconButton icon=icons::ZOOM_IN label="Zoom in" variant=Variant::Glass size=Size::Sm on_click=Callback::new(zoom_in) />
                                <IconButton icon=icons::MINUS label="Zoom out" variant=Variant::Glass size=Size::Sm on_click=Callback::new(zoom_out) />
                                <IconButton icon=NAVIGATION label="Reset view" variant=Variant::Glass size=Size::Sm on_click=Callback::new(recenter) />
                            </div>
                        </div>
                    }
                })}
        </div>
    }
}

/// lucide `navigation`.
pub(crate) const NAVIGATION: IconData =
    IconData::new(r#"<polygon points="3 11 22 2 13 21 11 13 3 11"/>"#);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn popups_are_escaped() {
        assert_eq!(escape_html("<b>R&D</b>"), "&lt;b&gt;R&amp;D&lt;/b&gt;");
        assert_eq!(safe_color("#facc15"), "#facc15");
        assert_eq!(safe_color("red;x:1"), "#3b82f6");
    }

    #[test]
    fn zoom_limits_follow_the_chart() {
        assert_eq!(MapType::Vfr.clamp(5.0), 8.0);
        assert_eq!(MapType::Street.clamp(5.0), 5.0);
        assert_eq!(MapType::IfrHigh.clamp(15.0), 12.0);
        assert_eq!(MapType::from_key("ifr-low"), MapType::IfrLow);
        assert_eq!(MapType::from_key("?"), MapType::Street);
    }
}
