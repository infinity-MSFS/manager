use std::sync::Mutex;

use infinity_ipc::catalog::BridgeSpec;
use infinity_ipc::{IpcError, IpcErrorCode};
use serde_json::json;

use super::*;
use crate::sink::NoBridge;

const ID: &str = "aerodynamics-t38c";

fn product() -> Product {
    serde_json::from_value(json!({
        "id": ID,
        "name": "T-38C",
        "category": "aircraft",
        "vendor": "Aerodynamics",
        "version": "1.0.0",
        "package": { "kind": "msfs2024", "packageDirName": "aero-t38c",
                     "archive": { "format": "zip", "key": "t38.zip" } },
        "ownership": { "kind": "free" },
        "bridges": [{ "id": "dtc", "kind": "tauri-ws-bridge", "port": 9000, "namespace": "t38" }],
        "configurator": {
            "engine": "schema-v1",
            "exportFile": "work/config_incoming.json",
            "schema": [{ "id": "COM", "fields": [
                { "id": "ils", "type": "freq", "exportTransform": "mhzToKhzInt" }
            ] }],
            "migrations": [{ "from": 1, "to": 2, "ops": [
                { "op": "rename", "path": "COM.old", "to": "COM.ils" }
            ] }],
            "outputs": [
                { "kind": "file", "id": "write", "path": "work/out.json", "transforms": [
                    { "op": "injectTimestamp", "path": "META.created_at" }
                ] },
                { "kind": "bridge", "id": "send", "bridgeRef": "dtc", "command": "set_config",
                  "transforms": [{ "op": "injectConstant", "path": "version", "value": 2 }] },
                { "kind": "bridge", "id": "orphan", "bridgeRef": "missing", "command": "x" },
                { "kind": "file", "id": "escape", "path": "../evil.json" }
            ]
        }
    }))
    .unwrap()
}

fn envelope(schema_version: u32, value: Value) -> ConfigEnvelope {
    ConfigEnvelope {
        schema_version,
        product_id: ID.into(),
        value,
    }
}

fn ctx() -> EmitContext {
    EmitContext {
        now_iso: "2026-01-01T00:00:00.000Z".into(),
    }
}

fn code(e: ConfiguratorError) -> IpcErrorCode {
    IpcError::from(e).code
}

fn no_work_dir() -> IpcResult<PathBuf> {
    panic!("work dir must not be resolved")
}

#[derive(Default)]
struct RecordingBridge {
    calls: Mutex<Vec<(String, String, Value)>>,
}

impl BridgeSink for RecordingBridge {
    async fn command(
        &self,
        bridge: &BridgeSpec,
        command: &str,
        payload: Value,
    ) -> IpcResult<Value> {
        self.calls
            .lock()
            .unwrap()
            .push((bridge.namespace.clone(), command.into(), payload));
        Ok(json!({ "ok": true }))
    }
}

#[tokio::test]
async fn load_defaults_when_never_saved() {
    let tmp = tempfile::tempdir().unwrap();
    let store = ConfigStore::new(tmp.path());
    let loaded = load_config(&store, &product(), 2).await.unwrap();
    assert_eq!(loaded.envelope, envelope(2, json!({})));
    assert!(!loaded.migrated);
    assert!(!store.exists(ID).await.unwrap());
}

#[tokio::test]
async fn load_migrates_and_saves_back() {
    let tmp = tempfile::tempdir().unwrap();
    let store = ConfigStore::new(tmp.path());
    store
        .save(&envelope(1, json!({ "COM": { "old": 1 } })))
        .await
        .unwrap();

    let loaded = load_config(&store, &product(), 2).await.unwrap();
    assert!(loaded.migrated);
    assert_eq!(loaded.envelope, envelope(2, json!({ "COM": { "ils": 1 } })));
    assert_eq!(store.load(ID).await.unwrap(), loaded.envelope);

    let again = load_config(&store, &product(), 2).await.unwrap();
    assert!(!again.migrated);
}

#[tokio::test]
async fn failed_migration_leaves_the_file_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let store = ConfigStore::new(tmp.path());
    let original = envelope(1, json!({ "x": 1 }));
    store.save(&original).await.unwrap();
    let err = load_config(&store, &product(), 3).await.unwrap_err();
    assert_eq!(code(err), IpcErrorCode::MigrationFailed);
    assert_eq!(store.load(ID).await.unwrap(), original);
}

#[tokio::test]
async fn save_validates_schema_and_owner() {
    let tmp = tempfile::tempdir().unwrap();
    let store = ConfigStore::new(tmp.path());
    save_config(&store, &product(), &envelope(2, json!({ "a": 1 })))
        .await
        .unwrap();

    let mut other = envelope(2, json!({}));
    other.product_id = "someone-else".into();
    let err = save_config(&store, &product(), &other).await.unwrap_err();
    assert_eq!(code(err), IpcErrorCode::ValidationFailed);

    let mut broken = product();
    if let Some(cfg) = broken.configurator.as_mut() {
        cfg.schema = json!({});
    }
    let err = save_config(&store, &broken, &envelope(2, json!({})))
        .await
        .unwrap_err();
    assert_eq!(code(err), IpcErrorCode::SchemaInvalid);
    assert!(validate_product_schema(&product()).is_ok());
}

#[tokio::test]
async fn reset_deletes() {
    let tmp = tempfile::tempdir().unwrap();
    let store = ConfigStore::new(tmp.path());
    store.save(&envelope(2, json!({}))).await.unwrap();
    assert!(reset_config(&store, &product()).await.unwrap());
    assert!(!reset_config(&store, &product()).await.unwrap());
}

#[tokio::test]
async fn file_output_writes_pretty_json_into_work_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let store = ConfigStore::new(tmp.path());
    store
        .save(&envelope(2, json!({ "b": 1, "a": 2 })))
        .await
        .unwrap();

    let work_dir = work.path().to_path_buf();
    let result = emit_output(
        &store,
        &product(),
        "write",
        &ctx(),
        || Ok(work_dir),
        &NoBridge,
    )
    .await
    .unwrap();
    let expected_path = work.path().join("work").join("out.json");
    assert_eq!(
        result,
        EmitResult::File {
            path: expected_path.to_string_lossy().into_owned()
        }
    );
    assert_eq!(
        std::fs::read_to_string(expected_path).unwrap(),
        "{\n  \"b\": 1,\n  \"a\": 2,\n  \"META\": {\n    \"created_at\": \"2026-01-01T00:00:00.000Z\"\n  }\n}"
    );
}

#[tokio::test]
async fn file_output_surfaces_work_dir_errors_and_rejects_escapes() {
    let tmp = tempfile::tempdir().unwrap();
    let store = ConfigStore::new(tmp.path());
    store.save(&envelope(2, json!({}))).await.unwrap();

    let missing = || Err(IpcError::new(IpcErrorCode::PackageDirNotFound, "no sim"));
    let err = emit_output(&store, &product(), "write", &ctx(), missing, &NoBridge)
        .await
        .unwrap_err();
    assert_eq!(code(err), IpcErrorCode::PackageDirNotFound);

    let work = tempfile::tempdir().unwrap();
    let work_dir = work.path().to_path_buf();
    let err = emit_output(
        &store,
        &product(),
        "escape",
        &ctx(),
        || Ok(work_dir),
        &NoBridge,
    )
    .await
    .unwrap_err();
    assert_eq!(code(err), IpcErrorCode::SchemaInvalid);
}

#[tokio::test]
async fn bridge_output_sends_through_the_sink() {
    let tmp = tempfile::tempdir().unwrap();
    let store = ConfigStore::new(tmp.path());
    store.save(&envelope(2, json!({ "a": 1 }))).await.unwrap();
    let bridge = RecordingBridge::default();

    let result = emit_output(&store, &product(), "send", &ctx(), no_work_dir, &bridge)
        .await
        .unwrap();
    assert_eq!(
        result,
        EmitResult::Bridge {
            namespace: "t38".into(),
            command: "set_config".into(),
            response: json!({ "ok": true }),
        }
    );
    let calls = bridge.calls.lock().unwrap();
    assert_eq!(
        *calls,
        vec![(
            "t38".into(),
            "set_config".into(),
            json!({ "a": 1, "version": 2 })
        )]
    );
}

#[tokio::test]
async fn emit_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let store = ConfigStore::new(tmp.path());
    let err = emit_output(&store, &product(), "send", &ctx(), no_work_dir, &NoBridge)
        .await
        .unwrap_err();
    assert_eq!(code(err), IpcErrorCode::ProductUnknown, "nothing saved yet");

    store.save(&envelope(2, json!({}))).await.unwrap();
    for id in ["nope", "orphan"] {
        let err = emit_output(&store, &product(), id, &ctx(), no_work_dir, &NoBridge)
            .await
            .unwrap_err();
        assert_eq!(code(err), IpcErrorCode::SchemaInvalid, "{id}");
    }
    let err = emit_output(&store, &product(), "send", &ctx(), no_work_dir, &NoBridge)
        .await
        .unwrap_err();
    assert_eq!(code(err), IpcErrorCode::Unsupported);
}

#[tokio::test]
async fn export_to_sim_applies_export_transforms() {
    let tmp = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let store = ConfigStore::new(tmp.path());
    store
        .save(&envelope(2, json!({ "COM": { "ils": 122.4 } })))
        .await
        .unwrap();
    let path = export_to_sim(&store, &product(), work.path())
        .await
        .unwrap();
    assert_eq!(path, work.path().join("work").join("config_incoming.json"));
    let written: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(written, json!({ "COM": { "ils": 122_400 } }));
}

#[tokio::test]
async fn share_round_trips_between_installs() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let file = a.path().join("shared.infinity");
    let source = ConfigStore::new(a.path());
    source
        .save(&envelope(4, json!({ "MAIN": { "x": 1 } })))
        .await
        .unwrap();
    share_save(&source, &product(), &file).await.unwrap();

    let dest = ConfigStore::new(b.path());
    let imported = share_load(&dest, &product(), &file).await.unwrap();
    assert_eq!(imported, envelope(4, json!({ "MAIN": { "x": 1 } })));
    assert_eq!(dest.load(ID).await.unwrap(), imported);
}

#[tokio::test]
async fn share_load_rejects_bad_files() {
    let tmp = tempfile::tempdir().unwrap();
    let store = ConfigStore::new(tmp.path());
    let file = tmp.path().join("bad.json");
    std::fs::write(&file, "nope").unwrap();
    let err = share_load(&store, &product(), &file).await.unwrap_err();
    assert_eq!(code(err), IpcErrorCode::SchemaInvalid);
    let err = share_load(&store, &product(), &tmp.path().join("missing"))
        .await
        .unwrap_err();
    assert_eq!(code(err), IpcErrorCode::IoError);
}

#[tokio::test]
async fn bare_share_inherits_the_existing_schema_version() {
    let tmp = tempfile::tempdir().unwrap();
    let store = ConfigStore::new(tmp.path());
    let file = tmp.path().join("bare.json");
    std::fs::write(&file, r#"{ "anything": true }"#).unwrap();
    let first = share_load(&store, &product(), &file).await.unwrap();
    assert_eq!(first.schema_version, 1);

    store.save(&envelope(7, json!({}))).await.unwrap();
    let second = share_load(&store, &product(), &file).await.unwrap();
    assert_eq!(second, envelope(7, json!({ "anything": true })));
}

#[test]
fn coerce_share_shapes() {
    let env = coerce_share(
        json!({ "schemaVersion": 3, "productId": "other", "value": { "a": 1 } }),
        ID,
        1,
    )
    .unwrap();
    assert_eq!(env, envelope(3, json!({ "a": 1 })));

    let bad_version = coerce_share(json!({ "schemaVersion": "3", "value": {} }), ID, 1);
    assert!(matches!(
        bad_version,
        Err(ConfiguratorError::SchemaInvalid(_))
    ));

    let v1 = coerce_share(
        json!({ "version": 2, "MAIN": {}, "COM": { "VHF": [118300] } }),
        ID,
        5,
    )
    .unwrap();
    assert_eq!(
        v1,
        envelope(
            5,
            json!({ "version": 2, "MAIN": {}, "COM": { "VHF": [118.3] } })
        )
    );

    let curated = coerce_share(json!({ "approaches": {}, "training_areas": [] }), ID, 1);
    assert!(matches!(curated, Err(ConfiguratorError::SchemaInvalid(_))));
    let old = coerce_share(json!({ "version": 1, "MAIN": {}, "COM": {} }), ID, 1);
    assert!(matches!(old, Err(ConfiguratorError::Migration(_))));
}
