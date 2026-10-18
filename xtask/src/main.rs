#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

type Result<T = ()> = std::result::Result<T, String>;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match args.as_slice() {
        ["dev", rest @ ..] => dev(rest),
        ["dist", rest @ ..] => dist(rest),
        ["check"] => check(),
        ["frontend", command, rest @ ..] => frontend(command, rest),
        _ => {
            print_usage();
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("xtask: {e}");
            ExitCode::FAILURE
        }
    }
}

fn print_usage() {
    eprintln!("usage: cargo xtask <dev | dist | check | frontend <prep|serve|build> [--dir PATH]>");
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn desktop_dir() -> PathBuf {
    root().join("apps").join("desktop")
}

fn run(cmd: &mut Command) -> Result {
    let shown = format!("{cmd:?}");
    let status = cmd
        .status()
        .map_err(|e| format!("could not start {shown}: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{shown} failed with {status}"))
    }
}

fn cargo() -> Command {
    Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
}

fn dev(extra: &[&str]) -> Result {
    run(cargo()
        .current_dir(desktop_dir())
        .args(["tauri", "dev"])
        .args(extra))
}

fn dist(extra: &[&str]) -> Result {
    run(cargo()
        .current_dir(desktop_dir())
        .args(["tauri", "build"])
        .args(extra))
}

fn check() -> Result {
    run(cargo()
        .current_dir(root())
        .args(["fmt", "--all", "--check"]))?;
    frontend_prep(&Frontend::locate(None)?)?;
    run(cargo().current_dir(root()).args([
        "clippy",
        "--workspace",
        "--exclude",
        "infinity-manager-ui",
        "--all-targets",
        "--",
        "-D",
        "warnings",
    ]))?;
    run(cargo().current_dir(root()).args([
        "clippy",
        "-p",
        "infinity-manager-ui",
        "-p",
        "infinity-ipc",
        "--target",
        "wasm32-unknown-unknown",
        "--",
        "-D",
        "warnings",
    ]))?;
    run(cargo().current_dir(root()).args([
        "test",
        "--workspace",
        "--exclude",
        "infinity-manager-ui",
    ]))
}

#[derive(Debug)]
struct Frontend {
    dir: PathBuf,
    workspace_root: PathBuf,
    ui_root: PathBuf,
    core_root: PathBuf,
}

impl Frontend {
    fn locate(dir: Option<&str>) -> Result<Self> {
        let dir = match dir {
            Some(d) => std::path::absolute(d).map_err(|e| format!("--dir {d}: {e}"))?,
            None => root().join("frontend"),
        };
        let manifest = dir.join("Cargo.toml");
        let output = cargo()
            .args(["metadata", "--format-version", "1", "--manifest-path"])
            .arg(&manifest)
            .output()
            .map_err(|e| format!("cargo metadata: {e}"))?;
        if !output.status.success() {
            return Err(format!(
                "cargo metadata failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        let meta: serde_json::Value = serde_json::from_slice(&output.stdout)
            .map_err(|e| format!("cargo metadata output: {e}"))?;
        let package_dir = |name: &str, up: usize| -> Result<PathBuf> {
            let manifest = meta["packages"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|p| p["name"] == name)
                .and_then(|p| p["manifest_path"].as_str())
                .ok_or_else(|| {
                    format!("{name} is not in the dependency graph of {}", dir.display())
                })?;
            Path::new(manifest)
                .ancestors()
                .nth(up)
                .map(Path::to_path_buf)
                .ok_or_else(|| format!("unexpected layout at {manifest}"))
        };
        Ok(Self {
            ui_root: package_dir("infinity-ui", 3)?,
            core_root: package_dir("infinity-manager-ui", 1)?,
            workspace_root: meta["workspace_root"]
                .as_str()
                .map(PathBuf::from)
                .ok_or("cargo metadata has no workspace_root")?,
            dir,
        })
    }

    fn trunk(&self) -> Command {
        let mut cmd = Command::new("trunk");
        cmd.current_dir(&self.dir).env(
            "CARGO_TARGET_DIR",
            self.workspace_root.join("target").join("frontend"),
        );
        cmd
    }
}

fn frontend(command: &str, rest: &[&str]) -> Result {
    let dir = match rest {
        [] => None,
        ["--dir", dir] => Some(*dir),
        _ => return Err(format!("unexpected arguments: {rest:?}")),
    };
    let frontend = Frontend::locate(dir)?;
    match command {
        "prep" => frontend_prep(&frontend),
        "serve" => {
            frontend_prep(&frontend)?;
            run(frontend.trunk().arg("serve"))
        }
        "build" => {
            frontend_prep(&frontend)?;
            run(frontend
                .trunk()
                .args(["build", "--release", "--cargo-profile", "wasm-release"]))
        }
        other => Err(format!("unknown frontend command {other:?}")),
    }
}

fn frontend_prep(frontend: &Frontend) -> Result {
    let out = frontend.dir.join(".generated");
    std::fs::create_dir_all(&out).map_err(|e| format!("create {}: {e}", out.display()))?;

    let js_path = |p: &Path| serde_json::Value::String(p.to_string_lossy().replace('\\', "/"));
    let js = format!(
        "// Generated by `cargo xtask frontend prep`. Do not edit.\nmodule.exports = {{ uiRoot: {}, coreRoot: {} }};\n",
        js_path(&frontend.ui_root),
        js_path(&frontend.core_root),
    );
    write_if_changed(&out.join("paths.js"), js.as_bytes())?;

    copy_file(
        &frontend.ui_root.join("style").join("infinity-ui.css"),
        &out.join("infinity-ui.css"),
    )?;
    copy_file(
        &frontend.core_root.join("style").join("app.css"),
        &out.join("app.css"),
    )?;
    if frontend.dir != frontend.core_root {
        copy_dir(&frontend.core_root.join("public"), &out.join("public"))?;
    }
    Ok(())
}

fn copy_file(from: &Path, to: &Path) -> Result {
    let bytes = std::fs::read(from).map_err(|e| format!("read {}: {e}", from.display()))?;
    write_if_changed(to, &bytes)
}

fn copy_dir(from: &Path, to: &Path) -> Result {
    std::fs::create_dir_all(to).map_err(|e| format!("create {}: {e}", to.display()))?;
    let entries = std::fs::read_dir(from).map_err(|e| format!("read {}: {e}", from.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("read {}: {e}", from.display()))?;
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            copy_file(&entry.path(), &target)?;
        }
    }
    Ok(())
}

fn write_if_changed(path: &Path, contents: &[u8]) -> Result {
    if std::fs::read(path).is_ok_and(|current| current == contents) {
        return Ok(());
    }
    std::fs::write(path, contents).map_err(|e| format!("write {}: {e}", path.display()))
}
