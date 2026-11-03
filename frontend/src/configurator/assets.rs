//! Loads vendored scripts and stylesheets from `public/vendor/` on first
//! use. The app's CSP allows scripts only from `'self'`, so nothing comes
//! from a CDN; loading lazily keeps Leaflet off the startup path.

use std::cell::RefCell;
use std::collections::HashMap;

use infinity_ipc::IpcError;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

pub(crate) const LEAFLET_JS: &str = "/vendor/leaflet/leaflet.js";
pub(crate) const LEAFLET_CSS: &str = "/vendor/leaflet/leaflet.css";
pub(crate) const MAP_CSS: &str = "/vendor/leaflet/infinity-map.css";
pub(crate) const POLYGON_CLIPPING_JS: &str = "/vendor/polygon-clipping/polygon-clipping.umd.min.js";

thread_local! {
    /// One promise per URL, so concurrent callers share a single load.
    static LOADS: RefCell<HashMap<&'static str, js_sys::Promise>> = RefCell::new(HashMap::new());
}

fn element(tag: &str) -> Result<web_sys::Element, IpcError> {
    let document = web_sys::window()
        .and_then(|w| w.document())
        .ok_or_else(|| IpcError::unsupported("no document"))?;
    document
        .create_element(tag)
        .map_err(|_| IpcError::internal(format!("could not create <{tag}>")))
}

fn head() -> Option<web_sys::Element> {
    web_sys::window()?.document()?.query_selector("head").ok()?
}

fn start(url: &'static str) -> Result<js_sys::Promise, IpcError> {
    let is_css = url.ends_with(".css");
    let el = if is_css {
        let link = element("link")?;
        let _ = link.set_attribute("rel", "stylesheet");
        let _ = link.set_attribute("href", url);
        link
    } else {
        let script = element("script")?;
        let _ = script.set_attribute("src", url);
        script
    };
    let target = el.clone();
    let promise = js_sys::Promise::new(&mut |resolve, reject| {
        let _ = js_sys::Reflect::set(&target, &JsValue::from_str("onload"), &resolve);
        let _ = js_sys::Reflect::set(&target, &JsValue::from_str("onerror"), &reject);
    });
    let parent = head().ok_or_else(|| IpcError::unsupported("no <head>"))?;
    parent
        .append_child(&el)
        .map_err(|_| IpcError::internal(format!("could not load {url}")))?;
    Ok(promise)
}

/// Resolves once `url` has loaded; later calls resolve immediately.
pub(crate) async fn ensure(url: &'static str) -> Result<(), IpcError> {
    let promise = LOADS.with(|loads| {
        if let Some(p) = loads.borrow().get(url) {
            return Ok(p.clone());
        }
        let p = start(url)?;
        loads.borrow_mut().insert(url, p.clone());
        Ok::<_, IpcError>(p)
    })?;
    match JsFuture::from(promise).await {
        Ok(_) => Ok(()),
        Err(_) => {
            // Let a later attempt retry.
            LOADS.with(|loads| loads.borrow_mut().remove(url));
            Err(IpcError::internal(format!("could not load {url}")))
        }
    }
}

/// Leaflet and its stylesheets.
pub(crate) async fn ensure_leaflet() -> Result<(), IpcError> {
    ensure(LEAFLET_CSS).await?;
    ensure(MAP_CSS).await?;
    ensure(LEAFLET_JS).await
}

/// A global set by a vendored script, if it has loaded.
pub(crate) fn global(name: &str) -> Option<JsValue> {
    let window: JsValue = web_sys::window()?.into();
    js_sys::Reflect::get(&window, &JsValue::from_str(name))
        .ok()
        .filter(|v| !v.is_undefined() && !v.is_null())
}

pub(crate) fn global_fn(object: &str, method: &str) -> Option<js_sys::Function> {
    let obj = global(object)?;
    js_sys::Reflect::get(&obj, &JsValue::from_str(method))
        .ok()?
        .dyn_into()
        .ok()
}
