#![allow(unreachable_pub)]

mod app;
mod brand;
mod configurator;
pub mod editorial;
pub mod ext;
pub mod ipc;
pub mod markdown;
pub mod notices;
mod pages;
mod routes;
mod shell;
pub mod sim_tools;
pub mod state;
mod theme;
mod window;

pub use ext::{ConfiguratorPanel, Extensions, Gate, Page as ExtensionPage, SettingsSection};
pub use infinity_ipc;
pub use infinity_ui;
pub use leptos;

/// Mounts the app into `<body>`.
pub fn mount(extensions: Extensions) {
    console_error_panic_hook::set_once();
    leptos::mount::mount_to_body(move || app::App(app::AppProps { extensions }));
}
