//! The slice of Leaflet's API the map uses. Every binding resolves the
//! global `L` when called, so these are safe to declare before the script
//! has loaded; callers load it first ([`crate::configurator::assets`]).

use wasm_bindgen::prelude::*;

#[wasm_bindgen]
extern "C" {
    /// `L.Map`.
    #[derive(Debug, Clone)]
    pub(super) type LMap;

    #[wasm_bindgen(catch, js_namespace = L, js_name = map)]
    pub(super) fn map(el: &web_sys::HtmlElement, options: &JsValue) -> Result<LMap, JsValue>;

    #[wasm_bindgen(method, js_name = setView)]
    pub(super) fn set_view(this: &LMap, center: &JsValue, zoom: f64, options: &JsValue);

    #[wasm_bindgen(method, js_name = getZoom)]
    pub(super) fn get_zoom(this: &LMap) -> f64;

    #[wasm_bindgen(method, js_name = setZoom)]
    pub(super) fn set_zoom(this: &LMap, zoom: f64);

    #[wasm_bindgen(method, js_name = zoomIn)]
    pub(super) fn zoom_in(this: &LMap);

    #[wasm_bindgen(method, js_name = setMinZoom)]
    pub(super) fn set_min_zoom(this: &LMap, zoom: f64);

    #[wasm_bindgen(method, js_name = setMaxZoom)]
    pub(super) fn set_max_zoom(this: &LMap, zoom: f64);

    #[wasm_bindgen(method, js_name = invalidateSize)]
    pub(super) fn invalidate_size(this: &LMap);

    #[wasm_bindgen(method, js_name = removeLayer)]
    pub(super) fn remove_layer(this: &LMap, layer: &Layer);

    #[wasm_bindgen(method, js_name = hasLayer)]
    pub(super) fn has_layer(this: &LMap, layer: &Layer) -> bool;

    #[wasm_bindgen(method, js_name = on)]
    pub(super) fn on(this: &LMap, event: &str, handler: &js_sys::Function);

    #[wasm_bindgen(method)]
    pub(super) fn remove(this: &LMap);

    /// Any `L.Layer`: tile layers, groups, markers, vectors.
    #[derive(Debug, Clone)]
    pub(super) type Layer;

    #[wasm_bindgen(method, js_name = addTo)]
    pub(super) fn add_to(this: &Layer, target: &JsValue);

    #[wasm_bindgen(method, js_name = bindPopup)]
    pub(super) fn bind_popup(this: &Layer, html: &str);

    #[wasm_bindgen(method, js_name = on)]
    pub(super) fn on(this: &Layer, event: &str, handler: &js_sys::Function);

    #[wasm_bindgen(method, js_name = clearLayers)]
    pub(super) fn clear_layers(this: &Layer);

    #[wasm_bindgen(js_namespace = L, js_name = tileLayer)]
    pub(super) fn tile_layer(url: &str, options: &JsValue) -> Layer;

    #[wasm_bindgen(js_namespace = L, js_name = layerGroup)]
    pub(super) fn layer_group() -> Layer;

    #[wasm_bindgen(js_namespace = L, js_name = marker)]
    pub(super) fn marker(latlng: &JsValue, options: &JsValue) -> Layer;

    #[wasm_bindgen(js_namespace = L, js_name = circle)]
    pub(super) fn circle(latlng: &JsValue, options: &JsValue) -> Layer;

    #[wasm_bindgen(js_namespace = L, js_name = polygon)]
    pub(super) fn polygon(latlngs: &JsValue, options: &JsValue) -> Layer;

    #[wasm_bindgen(js_namespace = L, js_name = polyline)]
    pub(super) fn polyline(latlngs: &JsValue, options: &JsValue) -> Layer;

    #[wasm_bindgen(js_namespace = L, js_name = divIcon)]
    pub(super) fn div_icon(options: &JsValue) -> JsValue;

    #[wasm_bindgen(js_namespace = L, js_name = latLngBounds)]
    pub(super) fn lat_lng_bounds(south_west: &JsValue, north_east: &JsValue) -> JsValue;
}
