//! Schema-driven product configurators.
//!
//! A product's catalog entry declares a `schema` (rendered by the frontend),
//! a `migrations` ladder, and `outputs` (file or bridge sinks, each with an
//! op pipeline). This crate owns everything between the UI and those sinks:
//!
//! - [`ConfigStore`]: per-product config files, wire-compatible with v3;
//! - [`migrate`], [`validate_schema`], [`apply_export_transforms`],
//!   [`run_pipeline`]: the pure engine;
//! - [`coerce_legacy_v1_full`]: v1 `.infinity` import;
//! - [`load_config`], [`emit_output`], …: the workflows the app exposes as commands.
//!
//! Schemas stay opaque `serde_json::Value`s so the widget catalog can grow
//! without touching the IPC types.

mod error;
mod export;
mod fsio;
mod legacy;
mod migrate;
mod path;
mod pipeline;
mod service;
mod sink;
mod store;
mod time;
mod transforms;
mod validate;

pub use error::{ConfiguratorError, Result};
pub use export::apply_export_transforms;
pub use legacy::{LegacyShare, coerce_legacy_v1_full, detect_legacy_share};
pub use migrate::{Migration, MigrationOp, migrate, parse_migrations};
pub use path::{LeafKey, PathStep, parse_path, set_path_unchecked, walk_mut};
pub use pipeline::{EmitContext, FLIGHT_PLAN_WIRE_VERSION, Op, PickRing, parse_ops, run_pipeline};
pub use service::{
    EmitResult, LoadedConfig, coerce_share, emit_output, export_to_sim, load_config, reset_config,
    save_config, share_load, share_save, validate_product_schema,
};
pub use sink::{BridgeSink, NoBridge};
pub use store::{
    ConfigEnvelope, ConfigStore, MigrationReport, find_v1_app_data, validate_product_id,
};
pub use time::{epoch_ms_to_iso8601, now_iso8601_ms};
pub use transforms::{BUILTIN_TRANSFORMS, apply_transform};
pub use validate::validate_schema;
