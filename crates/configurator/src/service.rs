//! The configurator workflows the app exposes as commands: each one runs the
//! engine steps in the right order against a [`ConfigStore`]. Everything the
//! app resolves (work directory, bridges, the clock, dialog paths) comes in
//! as an argument.

use std::path::{Component, Path, PathBuf};

use infinity_ipc::IpcResult;
use infinity_ipc::catalog::{ConfiguratorSpec, OutputSpec, Product};

use serde_json::Value;

use crate::error::{ConfiguratorError, IoContext, Result};
use crate::export::apply_export_transforms;
use crate::fsio::{to_pretty, write_atomic};
use crate::legacy::{LegacyShare, coerce_legacy_v1_full, detect_legacy_share};
use crate::migrate::{migrate, parse_migrations};
use crate::pipeline::{EmitContext, parse_ops, run_pipeline};
use crate::sink::BridgeSink;
use crate::store::{ConfigEnvelope, ConfigStore};
use crate::validate::validate_schema;

pub use infinity_ipc::dto::{EmitResult, LoadedConfig};

/// The product's config at `target_schema_version`. Never-saved products
/// get an empty value; older configs are migrated and saved back so the
/// ladder runs once.
pub async fn load_config(
    store: &ConfigStore,
    product: &Product,
    target_schema_version: u32,
) -> Result<LoadedConfig> {
    let Some(mut envelope) = store.get(&product.id).await? else {
        return Ok(LoadedConfig {
            envelope: ConfigEnvelope {
                schema_version: target_schema_version,
                product_id: product.id.clone(),
                value: Value::Object(serde_json::Map::new()),
            },
            migrated: false,
        });
    };
    if envelope.schema_version == target_schema_version {
        return Ok(LoadedConfig {
            envelope,
            migrated: false,
        });
    }

    let migrations = parse_migrations(&spec(product)?.migrations)?;
    envelope.value = migrate(
        envelope.value,
        &migrations,
        envelope.schema_version,
        target_schema_version,
    )?;
    envelope.schema_version = target_schema_version;
    store.save(&envelope).await?;
    Ok(LoadedConfig {
        envelope,
        migrated: true,
    })
}

/// Validates the product's schema, then saves `envelope` under the
/// product's id.
pub async fn save_config(
    store: &ConfigStore,
    product: &Product,
    envelope: &ConfigEnvelope,
) -> Result<()> {
    validate_schema(&spec(product)?.schema)?;
    if envelope.product_id != product.id {
        return Err(ConfiguratorError::Validation(format!(
            "config for `{}` cannot be saved as `{}`",
            envelope.product_id, product.id
        )));
    }
    store.save(envelope).await
}

/// Deletes the saved config so the next load starts from defaults. Returns
/// whether there was one.
pub async fn reset_config(store: &ConfigStore, product: &Product) -> Result<bool> {
    store.delete(&product.id).await
}

pub fn validate_product_schema(product: &Product) -> Result<()> {
    validate_schema(&spec(product)?.schema)
}

/// The deprecated `exportFile` path: applies the schema's `exportTransform`s
/// and writes `<work_dir>/<exportFile>`. Returns the written path.
pub async fn export_to_sim(
    store: &ConfigStore,
    product: &Product,
    work_dir: &Path,
) -> Result<PathBuf> {
    let cfg = spec(product)?;
    let export_file = cfg
        .export_file
        .as_deref()
        .ok_or_else(|| ConfiguratorError::SchemaInvalid("configurator has no exportFile".into()))?;
    let envelope = store.load(&product.id).await?;
    let transformed = apply_export_transforms(&cfg.schema, envelope.value)?;
    write_output(work_dir, export_file, &transformed).await
}

/// Runs the stored config through output `output_id`'s pipeline and
/// delivers it. `work_dir` is only called for `file` outputs, so a product
/// whose sim isn't installed can still use its bridge outputs.
pub async fn emit_output<B, W>(
    store: &ConfigStore,
    product: &Product,
    output_id: &str,
    ctx: &EmitContext,
    work_dir: W,
    bridge: &B,
) -> Result<EmitResult>
where
    B: BridgeSink,
    W: FnOnce() -> IpcResult<PathBuf>,
{
    let output = spec(product)?
        .outputs
        .iter()
        .find(|o| o.id() == output_id)
        .ok_or_else(|| {
            ConfiguratorError::SchemaInvalid(format!(
                "product `{}` has no configurator output `{output_id}`",
                product.id
            ))
        })?;
    let ops = parse_ops(output.transforms())?;
    let envelope = store.load(&product.id).await?;
    let payload = run_pipeline(envelope.value, &ops, ctx)?;

    match output {
        OutputSpec::File { path, .. } => {
            let path = write_output(&work_dir()?, path, &payload).await?;
            Ok(EmitResult::File {
                path: path.to_string_lossy().into_owned(),
            })
        }
        OutputSpec::Bridge {
            bridge_ref,
            command,
            ..
        } => {
            let spec = product
                .bridges
                .iter()
                .find(|b| b.id == *bridge_ref)
                .ok_or_else(|| {
                    ConfiguratorError::SchemaInvalid(format!(
                        "product `{}` has no bridge `{bridge_ref}`",
                        product.id
                    ))
                })?;
            let response = bridge.command(spec, command, payload).await?;
            Ok(EmitResult::Bridge {
                namespace: spec.namespace.clone(),
                command: command.clone(),
                response,
            })
        }
    }
}

/// Writes the saved envelope to `dest` (the path the user picked; v3
/// suggested `<productId>.infinity`). The v1 curated-subset share format is
/// intentionally not produced.
pub async fn share_save(store: &ConfigStore, product: &Product, dest: &Path) -> Result<()> {
    let envelope = store.load(&product.id).await?;
    write_atomic(dest, &to_pretty(&envelope)?).await
}

/// Imports `src` as the product's config and saves it. See [`coerce_share`]
/// for the accepted shapes.
pub async fn share_load(
    store: &ConfigStore,
    product: &Product,
    src: &Path,
) -> Result<ConfigEnvelope> {
    let bytes = tokio::fs::read(src).await.at(src)?;
    let raw: Value = serde_json::from_slice(&bytes)
        .map_err(|e| ConfiguratorError::SchemaInvalid(format!("invalid share JSON: {e}")))?;
    // An unreadable existing config just means "no version to inherit".
    let existing = store.get(&product.id).await.ok().flatten();
    let envelope = coerce_share(raw, &product.id, existing.map_or(1, |e| e.schema_version))?;
    store.save(&envelope).await?;
    Ok(envelope)
}

/// Turns a parsed share file into an envelope for `product_id`:
/// - a v3 envelope keeps its `schemaVersion` (its `productId` is ignored:
///   sharing doesn't transfer ownership);
/// - a v1 full `.infinity` export is coerced to the modern value shape;
/// - the v1 curated-subset share is rejected with directions;
/// - any other bare value is taken as-is at `bare_schema_version` (the
///   existing config's version, or 1 so the ladder runs on next load).
pub fn coerce_share(
    raw: Value,
    product_id: &str,
    bare_schema_version: u32,
) -> Result<ConfigEnvelope> {
    if let (Some(sv), Some(value)) = (raw.get("schemaVersion"), raw.get("value")) {
        let schema_version = sv
            .as_u64()
            .and_then(|v| u32::try_from(v).ok())
            .ok_or_else(|| {
                ConfiguratorError::SchemaInvalid(
                    "share envelope: schemaVersion must be a number".into(),
                )
            })?;
        return Ok(ConfigEnvelope {
            schema_version,
            product_id: product_id.to_owned(),
            value: value.clone(),
        });
    }

    let mut value = raw;
    match detect_legacy_share(&value) {
        Some(LegacyShare::V1Full) => coerce_legacy_v1_full(&mut value)?,
        Some(LegacyShare::V1Curated) => {
            return Err(ConfiguratorError::SchemaInvalid(
                "this file is a v1 \"shareable\" subset export, which can't be imported — \
                 open the config in the v1 manager and use Save to produce a full .infinity file"
                    .into(),
            ));
        }
        None => {}
    }
    Ok(ConfigEnvelope {
        schema_version: bare_schema_version,
        product_id: product_id.to_owned(),
        value,
    })
}

fn spec(product: &Product) -> Result<&ConfiguratorSpec> {
    product.configurator.as_ref().ok_or_else(|| {
        ConfiguratorError::SchemaInvalid(format!("product `{}` has no configurator", product.id))
    })
}

/// Writes `payload` as pretty JSON to `work_dir/rel`, atomically.
async fn write_output(work_dir: &Path, rel: &str, payload: &Value) -> Result<PathBuf> {
    let rel_path = Path::new(rel);
    // Catalog paths are relative to the work dir; anything that could step
    // outside it is an authoring error.
    let inside = rel_path
        .components()
        .all(|c| matches!(c, Component::Normal(_) | Component::CurDir));
    if rel.is_empty() || !inside {
        return Err(ConfiguratorError::SchemaInvalid(format!(
            "output path `{rel}` must be relative to the work directory"
        )));
    }
    // Joined part by part so a catalog's `/` becomes the native separator.
    let target = rel_path
        .components()
        .fold(work_dir.to_path_buf(), |path, part| path.join(part));
    write_atomic(&target, &to_pretty(payload)?).await?;
    Ok(target)
}

#[cfg(test)]
mod tests;
