use wasm_bindgen::JsValue;
use wasm_bindgen_futures::JsFuture;

use crate::ipc::{self, tauri_fn};

#[derive(Debug, Clone, Copy)]
pub(crate) enum WindowAction {
    Minimize,
    ToggleMaximize,
    Close,
}

impl WindowAction {
    const fn method(self) -> &'static str {
        match self {
            Self::Minimize => "minimize",
            Self::ToggleMaximize => "toggleMaximize",
            Self::Close => "close",
        }
    }
}

pub(crate) fn perform(action: WindowAction) {
    wasm_bindgen_futures::spawn_local(async move {
        if let Err(e) = call(action).await {
            ipc::warn(&format!("window {}: {}", action.method(), e.message));
        }
    });
}

async fn call(action: WindowAction) -> infinity_ipc::IpcResult<()> {
    let current = tauri_fn(&["window", "getCurrentWindow"])?
        .call0(&JsValue::NULL)
        .map_err(ipc::js_error)?;
    let method: js_sys::Function =
        js_sys::Reflect::get(&current, &JsValue::from_str(action.method()))
            .map_err(ipc::js_error)?
            .into();
    let promise = method.call0(&current).map_err(ipc::js_error)?;
    JsFuture::from(js_sys::Promise::from(promise))
        .await
        .map_err(ipc::js_error)?;
    Ok(())
}
