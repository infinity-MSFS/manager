use infinity_providers::Providers;

#[derive(Debug)]
pub(crate) struct AppState {
    pub(crate) providers: Providers,
}

impl AppState {
    pub(crate) fn new(providers: Providers) -> Self {
        Self { providers }
    }
}
