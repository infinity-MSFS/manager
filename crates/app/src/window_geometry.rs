//! Repairs a main window restored at a degenerate size.
//!
//! The window-state plugin can persist the geometry of a minimized window
//! (Windows reports roughly 144x19 for one) and restores it verbatim. That
//! becomes the window's restore rectangle, so the manager opens as a
//! sliver, or snaps back to one when un-maximized or dragged. A maximized
//! window hides the problem (it reports the monitor's size), so the saved
//! size is checked as well as the live one.
//!
//! Anything under the configured minimum is treated as corrupt: the window
//! gets its configured default size, centred on its monitor, and is
//! maximized again if it was.

use std::path::Path;

use tauri::{LogicalPosition, LogicalSize, Manager, WebviewWindow};

const MAIN: &str = "main";
/// The window-state plugin's file in the app config dir.
const STATE_FILE: &str = ".window-state.json";

pub(crate) fn repair(app: &tauri::AppHandle) {
    let Some(window) = app.get_webview_window(MAIN) else {
        return;
    };
    let Some(config) = app.config().app.windows.iter().find(|w| w.label == MAIN) else {
        return;
    };
    let min = (
        config.min_width.unwrap_or(0.0),
        config.min_height.unwrap_or(0.0),
    );
    let saved = app
        .path()
        .app_config_dir()
        .ok()
        .and_then(|dir| saved_size(&dir.join(STATE_FILE)));
    let scale = window.scale_factor().unwrap_or(1.0);
    let live = window
        .inner_size()
        .ok()
        .map(|s| (f64::from(s.width), f64::from(s.height)));

    let degenerate = [live, saved]
        .into_iter()
        .flatten()
        .any(|size| too_small(size, scale, min));
    if !degenerate {
        return;
    }
    tracing::warn!(
        ?live,
        ?saved,
        "restored window size below minimum; resetting geometry"
    );
    let maximized = window.is_maximized().unwrap_or(false);
    if maximized {
        let _ = window.unmaximize();
    }
    reset(&window, (config.width, config.height));
    if maximized {
        let _ = window.maximize();
    }
}

/// Physical `(width, height)` the plugin saved for the main window.
fn saved_size(path: &Path) -> Option<(f64, f64)> {
    let raw = std::fs::read_to_string(path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let main = json.get(MAIN)?;
    Some((main.get("width")?.as_f64()?, main.get("height")?.as_f64()?))
}

/// A physical size under the logical minimum.
fn too_small((width, height): (f64, f64), scale: f64, (min_w, min_h): (f64, f64)) -> bool {
    let scale = if scale > 0.0 { scale } else { 1.0 };
    width / scale < min_w || height / scale < min_h
}

/// The default size, kept on screen, centred on the window's monitor.
fn reset(window: &WebviewWindow, (width, height): (f64, f64)) {
    let Ok(Some(monitor)) = window
        .current_monitor()
        .or_else(|_| window.primary_monitor())
    else {
        let _ = window.set_size(LogicalSize::new(width, height));
        let _ = window.center();
        return;
    };
    let scale = monitor.scale_factor();
    let area = monitor.size().to_logical::<f64>(scale);
    let origin = monitor.position().to_logical::<f64>(scale);
    let w = width.min(area.width * 0.9);
    let h = height.min(area.height * 0.9);
    let _ = window.set_size(LogicalSize::new(w, h));
    let _ = window.set_position(LogicalPosition::new(
        origin.x + (area.width - w) / 2.0,
        origin.y + (area.height - h) / 2.0,
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_minimized_capture_is_too_small() {
        let min = (1024.0, 680.0);
        assert!(too_small((144.0, 19.0), 1.0, min));
        assert!(too_small((1440.0, 900.0), 1.5, min), "logical 960x600");
        assert!(!too_small((1440.0, 900.0), 1.0, min));
        assert!(!too_small((2160.0, 1350.0), 1.5, min));
    }

    #[test]
    fn reads_the_plugin_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(STATE_FILE);
        std::fs::write(
            &path,
            r#"{"main":{"width":144,"height":19,"x":0,"y":0,"prev_x":0,"prev_y":0,"maximized":false,"visible":true,"decorated":false,"fullscreen":false}}"#,
        )
        .unwrap();
        assert_eq!(saved_size(&path), Some((144.0, 19.0)));
        assert_eq!(saved_size(&dir.path().join("missing.json")), None);
    }
}
