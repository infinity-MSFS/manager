#![allow(unreachable_pub)]

mod app;
mod brand;
pub mod editorial;
pub mod ext;
pub mod ipc;
mod pages;
mod routes;
mod shell;
pub mod state;
mod window;

pub use ext::{Extensions, Gate, Page as ExtensionPage};
pub use infinity_ipc;
pub use infinity_ui;
pub use leptos;

/// Mounts the app into `<body>`.
pub fn mount(extensions: Extensions) {
    console_error_panic_hook::set_once();
    leptos::mount::mount_to_body(move || app::App(app::AppProps { extensions }));
}
