use std::fs;
use std::path::Path;
use std::time::UNIX_EPOCH;

use infinity_ipc::catalog::{PresetGating, PresetGroup};
use infinity_ipc::dto::UserProfile;

use crate::inspect::MARKER_FILE;
use crate::presets::*;

const ROOT: &str = "SimObjects/Airplanes/DC10/presets/Aero_Dynamics";
const CFG: &str = "config/aircraft.cfg";
const LAYOUT: &str = "layout.json";
const FREE: [&str; 7] = [
    "DC_10_30_EARLY",
    "DC_10_30_LATE",
    "DC_10_30F_EARLY",
    "DC_10_30F_LATE",
    "DC_10_30_TANKER",
    "DC_10_30_KC10",
    "DC_10_30_KDC10",
];
const DC10: [&str; 8] = [
    "DC_10_10_EARLY",
    "DC_10_10_LATE",
    "DC_10_10F_LATE",
    "DC_10_10_TANKER",
    "DC_10_15",
    "DC_10_40_LATE",
    "DC_10_40F_LATE",
    "DC_10_40_OMEGA",
];
const MD10: [&str; 4] = ["MD_10_10_HUD", "MD_10_30_HUD", "MD_10_10", "MD_10_30"];

/// One of the real package's preset cfgs, byte for byte.
const PRESET: &str = "[MODULAR_MERGE]\r\nauto = true\r\n\r\n[GENERAL]\r\natc_model = \"DC15\"\r\n\r\n[FLTSIM.0]\r\ntitle = \"Aero Dynamics DC-10-15\" ; Variation name\r\nui_type=\"DC-10\"\r\n";
const COMMON: &str = "[FLTSIM.0]\r\nisUserSelectable=1\r\n";

fn gating() -> PresetGating {
    let group = |requires: Option<&str>, presets: &[&str]| PresetGroup {
        requires: requires.map(Into::into),
        presets: presets.iter().map(|p| (*p).to_owned()).collect(),
    };
    PresetGating {
        root: ROOT.into(),
        groups: vec![
            group(None, &FREE),
            group(Some("aerodynamics-dc10"), &DC10),
            group(Some("aerodynamics-md10"), &MD10),
        ],
        see_all: vec!["developer".into()],
    }
}

fn profile(permissions: &[&str]) -> UserProfile {
    UserProfile {
        subject: "auth0|x".into(),
        name: None,
        email: None,
        picture: None,
        permissions: permissions.iter().map(|p| (*p).to_owned()).collect(),
    }
}

fn set(cfg: &str, shown: bool) -> Edit {
    set_user_selectable(cfg.as_bytes(), shown)
}

fn changed(cfg: &str, shown: bool) -> String {
    match set(cfg, shown) {
        Edit::Changed(b) => String::from_utf8(b).unwrap(),
        other => panic!("expected a change, got {other:?}"),
    }
}

#[test]
fn visible_counts() {
    let g = gating();
    let count = |p: Option<&UserProfile>| visible(&g, p).len();
    assert_eq!(count(None), 7);
    assert_eq!(count(Some(&profile(&[]))), 7);
    assert_eq!(count(Some(&profile(&["channel:beta"]))), 7);
    assert_eq!(count(Some(&profile(&["aerodynamics-dc10"]))), 15);
    assert_eq!(count(Some(&profile(&["aerodynamics-md10"]))), 11);
    assert_eq!(
        count(Some(&profile(&["aerodynamics-dc10", "aerodynamics-md10"]))),
        19
    );
    assert_eq!(count(Some(&profile(&["developer"]))), 19);
    // Only a group's own permission unlocks it.
    let dc10 = visible(&g, Some(&profile(&["aerodynamics-dc10"])));
    assert!(!dc10.contains("MD_10_10_HUD"));
    let md10 = visible(&g, Some(&profile(&["aerodynamics-md10"])));
    assert!(!md10.contains("DC_10_15"));
}

#[test]
fn inserts_after_the_header() {
    let out = changed(PRESET, false);
    assert_eq!(
        out,
        PRESET.replace("[FLTSIM.0]\r\n", "[FLTSIM.0]\r\nisUserSelectable=0\r\n")
    );
    assert_eq!(set(&out, false), Edit::Unchanged);
}

#[test]
fn replaces_an_existing_value() {
    let cfg = "[FLTSIM.0]\r\ntitle=\"KC-10\"\r\nIsUserSelectable  =  1   ; shown\r\n[GENERAL]\r\nisUserSelectable=1\r\n";
    assert_eq!(
        changed(cfg, false),
        "[FLTSIM.0]\r\ntitle=\"KC-10\"\r\nIsUserSelectable  =  0   ; shown\r\n[GENERAL]\r\nisUserSelectable=1\r\n"
    );
    assert_eq!(set(cfg, true), Edit::Unchanged);
    assert_eq!(
        changed("[FLTSIM.0]\nisUserSelectable=1;x\n", false),
        "[FLTSIM.0]\nisUserSelectable=0;x\n"
    );
}

#[test]
fn keeps_line_endings() {
    let lf = PRESET.replace("\r\n", "\n");
    assert_eq!(
        changed(&lf, true),
        lf.replace("[FLTSIM.0]\n", "[FLTSIM.0]\nisUserSelectable=1\n")
    );
    let crlf = changed(PRESET, true);
    assert_eq!(crlf.matches("\r\n").count(), crlf.matches('\n').count());
    // A header on an unterminated last line.
    assert_eq!(
        changed("a=1\r\n[FLTSIM.0]", true),
        "a=1\r\n[FLTSIM.0]\r\nisUserSelectable=1"
    );
    // Unterminated last line inside the section is kept unterminated.
    assert_eq!(
        changed("[FLTSIM.0]\r\nx=1", true),
        "[FLTSIM.0]\r\nisUserSelectable=1\r\nx=1"
    );
}

#[test]
fn every_fltsim_section_and_nothing_else() {
    assert_eq!(
        changed(
            "[fltsim.0]\n; isUserSelectable=1\n[FLTSIM.1]\nx=1\n[FLTSIM.X]\n",
            false
        ),
        "[fltsim.0]\nisUserSelectable=0\n; isUserSelectable=1\n[FLTSIM.1]\nisUserSelectable=0\nx=1\n[FLTSIM.X]\n"
    );
    assert_eq!(set("[GENERAL]\r\nx=1\r\n", true), Edit::NoFltsim);
}

/// A package laid out like the real one, with `presets` on disk.
fn package(presets: &[&str]) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    fs::write(dir.join(MARKER_FILE), r#"{"version":"1.0.0"}"#).unwrap();
    let common = dir.join("SimObjects/Airplanes/DC10/common/config");
    fs::create_dir_all(&common).unwrap();
    fs::write(common.join("aircraft.cfg"), COMMON).unwrap();
    let mut content = vec![serde_json::json!({
        "path": "simobjects/airplanes/dc10/common/config/aircraft.cfg",
        "size": COMMON.len(),
        "date": 1
    })];
    for preset in presets {
        let config = dir.join(ROOT).join(preset).join("config");
        fs::create_dir_all(&config).unwrap();
        fs::write(config.join("aircraft.cfg"), PRESET).unwrap();
        content.push(serde_json::json!({
            "path": format!("{ROOT}/{preset}/{CFG}").to_lowercase(),
            "size": PRESET.len(),
            "date": 134_381_791_460_000_001_u64
        }));
    }
    let layout = serde_json::to_vec_pretty(&serde_json::json!({ "content": content })).unwrap();
    fs::write(dir.join(LAYOUT), layout).unwrap();
    tmp
}

fn cfg(dir: &Path, preset: &str) -> String {
    fs::read_to_string(dir.join(ROOT).join(preset).join(CFG)).unwrap()
}

fn applied(outcome: Outcome) -> Report {
    match outcome {
        Outcome::Applied(r) => r,
        other => panic!("expected applied, got {other:?}"),
    }
}

fn layout_entry(dir: &Path, preset: &str) -> serde_json::Value {
    let layout: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.join(LAYOUT)).unwrap()).unwrap();
    let key = format!("{ROOT}/{preset}/{CFG}").to_lowercase();
    layout["content"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["path"] == key.as_str())
        .unwrap()
        .clone()
}

#[test]
fn applies_then_a_second_run_writes_nothing() {
    let tmp = package(&["DC_10_30_EARLY", "DC_10_15", "MD_10_30_HUD", "DC_10_99_NEW"]);
    let dir = tmp.path();
    let g = gating();
    let shown = visible(&g, Some(&profile(&["aerodynamics-dc10"])));
    let report = applied(apply(dir, &g, &shown));
    assert!(report.failed.is_empty(), "{:?}", report.failed);
    assert_eq!(report.unlisted, ["DC_10_99_NEW"]);
    assert!(report.missing.contains(&"MD_10_10".to_owned()));
    // Four cfgs and layout.json.
    assert_eq!(report.changed.len(), 5, "{:?}", report.changed);
    assert!(cfg(dir, "DC_10_30_EARLY").contains("[FLTSIM.0]\r\nisUserSelectable=1\r\ntitle"));
    assert!(cfg(dir, "DC_10_15").contains("isUserSelectable=1"));
    assert!(cfg(dir, "MD_10_30_HUD").contains("isUserSelectable=0"));
    // In no group: hidden for everyone.
    assert!(cfg(dir, "DC_10_99_NEW").contains("isUserSelectable=0"));
    // common/ is never touched.
    assert_eq!(
        fs::read_to_string(dir.join("SimObjects/Airplanes/DC10/common/config/aircraft.cfg"))
            .unwrap(),
        COMMON
    );

    let second = applied(apply(dir, &g, &shown));
    assert!(second.changed.is_empty(), "{:?}", second.changed);
    assert!(second.failed.is_empty());
    let leftovers = fs::read_dir(dir.join(ROOT).join("DC_10_15/config"))
        .unwrap()
        .flatten()
        .any(|e| e.file_name().to_string_lossy().ends_with(".tmp"));
    assert!(!leftovers);
}

#[test]
fn a_no_op_run_leaves_every_file_alone() {
    let tmp = package(&["DC_10_15"]);
    let dir = tmp.path();
    let g = gating();
    let shown = visible(&g, None);
    applied(apply(dir, &g, &shown));
    let snapshot = |p: &Path| {
        (
            fs::read(p).unwrap(),
            fs::metadata(p).unwrap().modified().unwrap(),
        )
    };
    let cfg_path = dir.join(ROOT).join("DC_10_15").join(CFG);
    let before = (snapshot(&cfg_path), snapshot(&dir.join(LAYOUT)));
    std::thread::sleep(std::time::Duration::from_millis(20));
    let report = applied(apply(dir, &g, &shown));
    assert!(report.changed.is_empty());
    assert_eq!((snapshot(&cfg_path), snapshot(&dir.join(LAYOUT))), before);
}

#[test]
fn layout_follows_size_and_date() {
    let tmp = package(&["DC_10_15", "DC_10_30_EARLY"]);
    let dir = tmp.path();
    let g = gating();
    applied(apply(dir, &g, &visible(&g, None)));
    let path = dir.join(ROOT).join("DC_10_15").join(CFG);
    let meta = fs::metadata(&path).unwrap();
    let entry = layout_entry(dir, "DC_10_15");
    assert_eq!(
        meta.len() as usize,
        PRESET.len() + "isUserSelectable=0\r\n".len()
    );
    assert_eq!(entry["size"], meta.len());
    let since_unix = meta.modified().unwrap().duration_since(UNIX_EPOCH).unwrap();
    let expected = since_unix.as_nanos() / 100 + 116_444_736_000_000_000;
    assert_eq!(entry["date"].as_u64().map(u128::from), Some(expected));
    // Untouched entries keep their values.
    let layout: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.join(LAYOUT)).unwrap()).unwrap();
    assert_eq!(layout["content"][0]["date"], 1);

    // Same-size replace (0 -> 1) still moves the date.
    let old_date = entry["date"].clone();
    std::thread::sleep(std::time::Duration::from_millis(20));
    let report = applied(apply(
        dir,
        &g,
        &visible(&g, Some(&profile(&["aerodynamics-dc10"]))),
    ));
    assert_eq!(report.changed.len(), 2, "{:?}", report.changed);
    let entry = layout_entry(dir, "DC_10_15");
    assert_eq!(entry["size"], fs::metadata(&path).unwrap().len());
    assert_ne!(entry["date"], old_date);
}

#[test]
fn layout_is_rewritten_in_its_own_format() {
    let tmp = package(&["DC_10_15"]);
    let dir = tmp.path();
    let raw = fs::read_to_string(dir.join(LAYOUT)).unwrap();
    let g = gating();
    applied(apply(dir, &g, &visible(&g, None)));
    let after = fs::read_to_string(dir.join(LAYOUT)).unwrap();
    // Same shape: two-space indent, LF, no trailing newline, key order.
    assert_eq!(raw.lines().count(), after.lines().count());
    assert!(!after.contains('\r') && !after.ends_with('\n'));
    assert!(after.contains(
        "\"path\": \"simobjects/airplanes/dc10/common/config/aircraft.cfg\",\n      \"size\""
    ));
}

#[test]
fn leaves_an_unmarked_package_alone() {
    let tmp = package(&["DC_10_15"]);
    fs::remove_file(tmp.path().join(MARKER_FILE)).unwrap();
    let layout = fs::read(tmp.path().join(LAYOUT)).unwrap();
    let g = gating();
    assert!(matches!(
        apply(tmp.path(), &g, &visible(&g, None)),
        Outcome::Unmanaged
    ));
    assert_eq!(cfg(tmp.path(), "DC_10_15"), PRESET);
    assert_eq!(fs::read(tmp.path().join(LAYOUT)).unwrap(), layout);
}

#[test]
fn a_cfg_without_fltsim_is_skipped() {
    let tmp = package(&["DC_10_15"]);
    let path = tmp.path().join(ROOT).join("DC_10_15").join(CFG);
    fs::write(&path, "[GENERAL]\r\nx=1\r\n").unwrap();
    let g = gating();
    let report = applied(apply(tmp.path(), &g, &visible(&g, None)));
    assert!(report.changed.is_empty() && report.failed.is_empty());
    assert_eq!(fs::read_to_string(&path).unwrap(), "[GENERAL]\r\nx=1\r\n");
}

#[test]
fn a_product_without_lists_is_left_alone() {
    let tmp = package(&["DC_10_15"]);
    let layout = fs::read(tmp.path().join(LAYOUT)).unwrap();
    assert!(gate(tmp.path(), None, None).is_none());
    assert_eq!(cfg(tmp.path(), "DC_10_15"), PRESET);
    assert_eq!(fs::read(tmp.path().join(LAYOUT)).unwrap(), layout);
    let g = gating();
    assert!(matches!(
        gate(tmp.path(), Some(&g), None),
        Some(Outcome::Applied(_))
    ));
}

/// Against a copy of a real package's presets and layout.json:
/// `DC10_PACKAGE=<path to aerodynamics-dc10> cargo test -p infinity-installer -- --ignored real_package`
#[test]
#[ignore = "needs DC10_PACKAGE"]
fn real_package() {
    let Ok(source) = std::env::var("DC10_PACKAGE") else {
        return;
    };
    let source = Path::new(&source);
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    fs::write(dir.join(MARKER_FILE), "{}").unwrap();
    fs::copy(source.join(LAYOUT), dir.join(LAYOUT)).unwrap();
    let mut originals = Vec::new();
    for entry in fs::read_dir(source.join(ROOT)).unwrap().flatten() {
        let name = entry.file_name().into_string().unwrap();
        let from = entry.path().join(CFG);
        let to = dir.join(ROOT).join(&name).join(CFG);
        fs::create_dir_all(to.parent().unwrap()).unwrap();
        fs::copy(&from, &to).unwrap();
        originals.push((name, fs::read(&from).unwrap()));
    }
    let g = gating();
    for (permissions, count) in [
        (&[][..], 7),
        (&["aerodynamics-dc10"][..], 15),
        (&["aerodynamics-md10"][..], 11),
        (&["developer"][..], 19),
    ] {
        let shown = visible(&g, Some(&profile(permissions)));
        assert_eq!(shown.len(), count);
        let report = applied(apply(dir, &g, &shown));
        assert!(
            report.failed.is_empty() && report.unlisted.is_empty(),
            "{report:?}"
        );
        for (name, original) in &originals {
            let now = fs::read(dir.join(ROOT).join(name).join(CFG)).unwrap();
            let want = if shown.contains(name) { "1" } else { "0" };
            let text = String::from_utf8(now.clone()).unwrap();
            let shipped = String::from_utf8(original.clone()).unwrap();
            // Only the one line differs from what shipped: the key's
            // value, or the key inserted after the header.
            let expected = if shipped.contains("isUserSelectable=") {
                shipped.replace("isUserSelectable=1", &format!("isUserSelectable={want}"))
            } else {
                shipped.replacen(
                    "[FLTSIM.0]\r\n",
                    &format!("[FLTSIM.0]\r\nisUserSelectable={want}\r\n"),
                    1,
                )
            };
            assert_eq!(text, expected, "{name}");
            assert_eq!(layout_entry(dir, name)["size"], now.len(), "{name}");
        }
        assert!(applied(apply(dir, &g, &shown)).changed.is_empty());
    }
    let before: serde_json::Value =
        serde_json::from_slice(&fs::read(source.join(LAYOUT)).unwrap()).unwrap();
    let after: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.join(LAYOUT)).unwrap()).unwrap();
    assert_eq!(
        before["content"].as_array().unwrap().len(),
        after["content"].as_array().unwrap().len()
    );
}
