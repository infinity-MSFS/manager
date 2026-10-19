use std::cell::{Cell, RefCell};
use std::rc::Rc;

use infinity_ipc::{Command, Event, IpcError, IpcResult};
use leptos::prelude::*;
use send_wrapper::SendWrapper;
use serde::Serialize;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

#[derive(Serialize)]
struct Envelope<'a, T> {
    args: &'a T,
}

pub async fn invoke<C: Command>(args: &C::Args) -> IpcResult<C::Output> {
    let invoke = tauri_fn(&["core", "invoke"])?;
    let payload = to_js(&Envelope { args })?;
    let promise = invoke
        .call2(&JsValue::NULL, &JsValue::from_str(C::NAME), &payload)
        .map_err(js_error)?;
    let value = JsFuture::from(js_sys::Promise::from(promise))
        .await
        .map_err(js_error)?;
    serde_wasm_bindgen::from_value(value)
        .map_err(|e| IpcError::internal(format!("{}: bad response: {e}", C::NAME)))
}

type JsCallback = Closure<dyn Fn(JsValue)>;

struct Listener {
    disposed: Cell<bool>,
    unlisten: RefCell<Option<js_sys::Function>>,
    callback: RefCell<Option<JsCallback>>,
}

impl Listener {
    fn unlisten(&self) {
        if let Some(f) = self.unlisten.borrow_mut().take() {
            let _ = f.call0(&JsValue::NULL);
        }
    }
}

pub fn listen<E: Event>(handler: impl Fn(E::Payload) + 'static) {
    let Ok(listen) = tauri_fn(&["event", "listen"]) else {
        return;
    };
    let state = Rc::new(Listener {
        disposed: Cell::new(false),
        unlisten: RefCell::new(None),
        callback: RefCell::new(None),
    });

    let weak = Rc::downgrade(&state);
    let callback = JsCallback::new(move |event: JsValue| {
        if weak.upgrade().is_none_or(|s| s.disposed.get()) {
            return;
        }
        let payload = js_sys::Reflect::get(&event, &JsValue::from_str("payload"))
            .unwrap_or(JsValue::UNDEFINED);
        match serde_wasm_bindgen::from_value::<E::Payload>(payload) {
            Ok(p) => handler(p),
            Err(e) => warn(&format!("{}: bad payload: {e}", E::NAME)),
        }
    });
    let registered = listen.call2(
        &JsValue::NULL,
        &JsValue::from_str(E::NAME),
        callback.as_ref().unchecked_ref(),
    );
    *state.callback.borrow_mut() = Some(callback);

    let pending = state.clone();
    wasm_bindgen_futures::spawn_local(async move {
        let result = match registered {
            Ok(promise) => JsFuture::from(js_sys::Promise::from(promise)).await,
            Err(e) => Err(e),
        };
        match result.map(|f| f.dyn_into::<js_sys::Function>()) {
            Ok(Ok(f)) => {
                *pending.unlisten.borrow_mut() = Some(f);
                if pending.disposed.get() {
                    pending.unlisten();
                }
            }
            _ => warn(&format!("{}: listen failed", E::NAME)),
        }
    });

    let guard = SendWrapper::new(state);
    on_cleanup(move || {
        let state = guard.take();
        state.disposed.set(true);
        state.unlisten();
    });
}

pub fn in_shell() -> bool {
    tauri_fn(&["core", "invoke"]).is_ok()
}

pub(crate) fn tauri_fn(path: &[&str]) -> IpcResult<js_sys::Function> {
    tauri_value(path)?.dyn_into().map_err(|_| not_in_shell())
}

pub(crate) fn tauri_value(path: &[&str]) -> IpcResult<JsValue> {
    let mut value: JsValue = web_sys::window().ok_or_else(not_in_shell)?.into();
    for key in std::iter::once(&"__TAURI__").chain(path) {
        value =
            js_sys::Reflect::get(&value, &JsValue::from_str(key)).map_err(|_| not_in_shell())?;
        if value.is_undefined() || value.is_null() {
            return Err(not_in_shell());
        }
    }
    Ok(value)
}

fn not_in_shell() -> IpcError {
    IpcError::unsupported("not running inside the Infinity Manager shell")
}

fn to_js<T: Serialize + ?Sized>(value: &T) -> IpcResult<JsValue> {
    value
        .serialize(&serde_wasm_bindgen::Serializer::json_compatible())
        .map_err(|e| IpcError::internal(format!("could not encode arguments: {e}")))
}

pub(crate) fn js_error(value: JsValue) -> IpcError {
    serde_wasm_bindgen::from_value::<IpcError>(value.clone()).unwrap_or_else(|_| {
        let text = value
            .as_string()
            .or_else(|| {
                value
                    .dyn_ref::<js_sys::Error>()
                    .map(|e| String::from(e.message()))
            })
            .unwrap_or_else(|| format!("{value:?}"));
        IpcError::internal(text)
    })
}

pub fn warn(message: &str) {
    web_sys::console::warn_1(&JsValue::from_str(message));
}
