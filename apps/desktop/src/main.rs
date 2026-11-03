#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::process::ExitCode;

use infinity_providers::Providers;

const DEFAULT_CATALOG_URL: &str = "https://manager.infinity-simulations.com/v1/catalog/{channel}";
const DEFAULT_EFB_DOCS_URL: &str = "https://manager.infinity-simulations.com/v1/efb/";
const DEFAULT_CHANGELOG_URL: &str =
    "https://manager.infinity-simulations.com/v1/changelog/{product}";

fn main() -> ExitCode {
    infinity_app::init_logging();
    let catalog_url =
        std::env::var("INFINITY_CATALOG_URL").unwrap_or_else(|_| DEFAULT_CATALOG_URL.to_owned());
    let artifact_base = std::env::var("INFINITY_ARTIFACT_BASE_URL")
        .ok()
        .and_then(|s| url::Url::parse(&s).ok());

    let changelog_url = std::env::var("INFINITY_CHANGELOG_URL")
        .unwrap_or_else(|_| DEFAULT_CHANGELOG_URL.to_owned());
    let efb_docs = url::Url::parse(
        &std::env::var("INFINITY_EFB_DOCS_URL").unwrap_or_else(|_| DEFAULT_EFB_DOCS_URL.to_owned()),
    )
    .ok();
    let providers =
        match Providers::community(&catalog_url, Some(&changelog_url), efb_docs, artifact_base) {
            Ok(p) => p,
            Err(e) => {
                tracing::error!(error = %e, "invalid community configuration");
                return ExitCode::FAILURE;
            }
        };

    let mut providers = providers;
    // Navigraph developer credentials, for builders who have their own.
    if let (Ok(id), Ok(secret)) = (
        std::env::var("INFINITY_NAVIGRAPH_CLIENT_ID"),
        std::env::var("INFINITY_NAVIGRAPH_CLIENT_SECRET"),
    ) {
        providers.navigraph = Some(infinity_providers::NavigraphAccess {
            credentials: std::sync::Arc::new(infinity_navigraph::StaticCredentials(
                infinity_navigraph::AppCredentials::new(id, secret),
            )),
            tokens: None,
        });
    }

    // Support logs probe the server the catalog comes from.
    let probe = url::Url::parse(&catalog_url)
        .ok()
        .filter(|u| matches!(u.scheme(), "http" | "https"))
        .map(|u| u.origin().ascii_serialization() + "/");
    let mut builder = infinity_app::Builder::new(providers).plugin(infinity_sim_tools::init(
        infinity_sim_tools::Config::default(),
    ));
    if let Some(origin) = probe {
        builder = builder.network_probe(origin);
    }
    builder.run(tauri::generate_context!())
}
