pub mod catalog;
pub mod commands;
pub mod dto;
pub mod error;
pub mod events;

pub use error::{IpcError, IpcErrorCode, IpcResult};

use serde::{Serialize, de::DeserializeOwned};

pub trait Command: 'static {
    const NAME: &'static str;
    type Args: Serialize + DeserializeOwned + Send + 'static;
    type Output: Serialize + DeserializeOwned + Send + 'static;
}

pub trait Event: 'static {
    const NAME: &'static str;
    type Payload: Serialize + DeserializeOwned + Clone + Send + 'static;
}

/// Declares IPC commands as marker types plus an `ALL` name list
#[macro_export]
macro_rules! define_commands {
    (plugin = $plugin:literal; $( $(#[$meta:meta])* $name:ident => $ty:ident($args:ty) -> $out:ty; )*) => {
        pub const PLUGIN: &str = $plugin;

        $(
            $(#[$meta])*
            #[derive(Debug, Clone, Copy)]
            pub struct $ty;

            impl $crate::Command for $ty {
                const NAME: &'static str = concat!("plugin:", $plugin, "|", stringify!($name));
                type Args = $args;
                type Output = $out;
            }
        )*

        pub const ALL: &[&str] = &[$(stringify!($name)),*];
    };
    ($( $(#[$meta:meta])* $name:ident => $ty:ident($args:ty) -> $out:ty; )*) => {
        $(
            $(#[$meta])*
            #[derive(Debug, Clone, Copy)]
            pub struct $ty;

            impl $crate::Command for $ty {
                const NAME: &'static str = stringify!($name);
                type Args = $args;
                type Output = $out;
            }
        )*

        pub const ALL: &[&str] = &[$(stringify!($name)),*];
    };
}

/// Declares IPC events as marker types plus an `ALL` name list.
#[macro_export]
macro_rules! define_events {
    ($( $(#[$meta:meta])* $name:literal => $ty:ident($payload:ty); )*) => {
        $(
            $(#[$meta])*
            #[derive(Debug, Clone, Copy)]
            pub struct $ty;

            impl $crate::Event for $ty {
                const NAME: &'static str = $name;
                type Payload = $payload;
            }
        )*

        pub const ALL: &[&str] = &[$($name),*];
    };
}
