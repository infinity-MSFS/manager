//! Stages the service binary for `include_bytes!` and records its version.
//!
//! Building the service is the release build's job, not this script's:
//! `cargo xtask dist` builds `infinity-launch-svc` and points
//! `INFINITY_LAUNCH_SVC_PREBUILT` at it. Without that variable (plain
//! `cargo build`, clippy, tests) an empty placeholder is embedded and the
//! manager looks for the service at runtime instead; see `windows.rs`.

use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=INFINITY_LAUNCH_SVC_PREBUILT");
    let Some(out) = std::env::var_os("OUT_DIR").map(PathBuf::from) else {
        println!("cargo:warning=OUT_DIR is not set");
        return;
    };
    let manifest_dir = std::env::var_os("CARGO_MANIFEST_DIR")
        .map(PathBuf::from)
        .unwrap_or_default();
    let svc_manifest = manifest_dir
        .join("..")
        .join("launch-svc")
        .join("Cargo.toml");
    println!("cargo:rerun-if-changed={}", svc_manifest.display());

    let version = package_version(&svc_manifest).unwrap_or_else(|| {
        println!(
            "cargo:warning=no version in {}; reporting 0.0.0",
            svc_manifest.display()
        );
        "0.0.0".to_owned()
    });
    write(&out.join("launch-svc.version"), version.as_bytes());

    let windows = std::env::var("CARGO_CFG_TARGET_OS").is_ok_and(|os| os == "windows");
    let bytes = match std::env::var_os("INFINITY_LAUNCH_SVC_PREBUILT").map(PathBuf::from) {
        Some(path) if windows => {
            println!("cargo:rerun-if-changed={}", path.display());
            match std::fs::read(&path) {
                Ok(b) => b,
                Err(e) => {
                    println!(
                        "cargo:warning=INFINITY_LAUNCH_SVC_PREBUILT={}: {e}; the supervisor will not be bundled",
                        path.display()
                    );
                    Vec::new()
                }
            }
        }
        _ => Vec::new(),
    };
    write(&out.join("launch-svc.exe"), &bytes);
}

fn write(path: &Path, bytes: &[u8]) {
    if let Err(e) = std::fs::write(path, bytes) {
        println!("cargo:warning=write {}: {e}", path.display());
    }
}

/// The literal `version = "…"` in a manifest's `[package]` table.
fn package_version(manifest: &Path) -> Option<String> {
    let text = std::fs::read_to_string(manifest).ok()?;
    let mut in_package = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            in_package = line == "[package]";
            continue;
        }
        if !in_package {
            continue;
        }
        let Some(rest) = line.strip_prefix("version") else {
            continue;
        };
        let value = rest
            .trim_start()
            .strip_prefix('=')?
            .trim()
            .trim_matches('"');
        if !value.is_empty() {
            return Some(value.to_owned());
        }
    }
    None
}
