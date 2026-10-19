#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::process::ExitCode;

use infinity_providers::Providers;

const DEFAULT_CATALOG_URL: &str = "https://manager.infinity-simulations.com/v1/catalog/{channel}";

fn main() -> ExitCode {
    infinity_app::init_logging();
    let catalog_url =
        std::env::var("INFINITY_CATALOG_URL").unwrap_or_else(|_| DEFAULT_CATALOG_URL.to_owned());
    let artifact_base = std::env::var("INFINITY_ARTIFACT_BASE_URL")
        .ok()
        .and_then(|s| url::Url::parse(&s).ok());

    let providers = match Providers::community(&catalog_url, artifact_base) {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(error = %e, "invalid community configuration");
            return ExitCode::FAILURE;
        }
    };

    infinity_app::Builder::new(providers).run(tauri::generate_context!())
}
