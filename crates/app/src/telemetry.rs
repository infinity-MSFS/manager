use tracing_subscriber::EnvFilter;

pub(crate) fn init() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,infinity_app=debug,infinity_auth=debug"));
    let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();
}
