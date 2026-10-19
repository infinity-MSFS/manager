use infinity_ipc::Event;
use tauri::Emitter;

pub(crate) fn emit<E: Event>(app: &tauri::AppHandle, payload: &E::Payload) {
    if let Err(e) = app.emit(E::NAME, payload) {
        tracing::warn!(event = E::NAME, error = %e, "event emit failed");
    }
}
