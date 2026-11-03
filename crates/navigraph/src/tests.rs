//! End-to-end tests against a local HTTP server.

use std::io::Write as _;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use infinity_ipc::catalog::NavdataSpec;
use infinity_ipc::{IpcError, IpcErrorCode};
use sha2::{Digest, Sha256};

use crate::test_server::{Request, Response, TestServer};
use crate::*;

fn tokens_response(access: &str, refresh: &str) -> Response {
    Response::json(
        200,
        serde_json::json!({
            "access_token": access,
            "expires_in": 3600,
            "refresh_token": refresh,
            "token_type": "Bearer"
        }),
    )
    .header("set-cookie", "CloudFront-Policy=pol; Path=/; Secure")
    .header("set-cookie", "CloudFront-Signature=sig; Path=/")
    .header("set-cookie", "CloudFront-Key-Pair-Id=kp; Path=/")
}

fn oauth_error(code: &str) -> Response {
    Response::json(400, serde_json::json!({ "error": code }))
}

fn navigraph(server: &TestServer, store: Arc<dyn RefreshTokenStore>) -> Navigraph {
    let client = NavigraphClient::new(Endpoints::single(server.base())).unwrap();
    Navigraph::new(
        client,
        store,
        Arc::new(StaticCredentials(AppCredentials::new("cid", "csecret"))),
    )
}

fn device_auth() -> Response {
    Response::json(
        200,
        serde_json::json!({
            "device_code": "dev-code",
            "user_code": "ABCD-EFGH",
            "verification_uri": "https://navigraph.com/code",
            "verification_uri_complete": "https://navigraph.com/code?c=ABCD-EFGH",
            "expires_in": 300,
            "interval": 0
        }),
    )
}

#[tokio::test]
async fn device_flow_links_persists_and_unlinks() {
    let polls = Arc::new(AtomicUsize::new(0));
    let counter = polls.clone();
    let server = TestServer::start(move |req: &Request| match req.path() {
        "/connect/deviceauthorization" => device_auth(),
        "/connect/token" => match counter.fetch_add(1, Ordering::SeqCst) {
            0 => oauth_error("authorization_pending"),
            1 => oauth_error("slow_down"),
            _ => tokens_response("access-1", "refresh-1"),
        },
        _ => Response::bytes(404, ""),
    })
    .await;
    let store = Arc::new(MemoryTokenStore::default());
    let nav = navigraph(&server, store.clone());

    assert!(matches!(
        nav.link_poll().await.unwrap_err(),
        NavigraphError::LinkNotStarted
    ));

    let start = nav.link_start().await.unwrap();
    assert_eq!(start.user_code, "ABCD-EFGH");
    assert_eq!(start.interval, 1, "interval is at least a second");
    assert_eq!(start.expires_in, 300);

    assert_eq!(nav.link_poll().await.unwrap(), LinkPoll::Pending);
    assert_eq!(nav.link_poll().await.unwrap(), LinkPoll::SlowDown);
    assert_eq!(nav.link_poll().await.unwrap(), LinkPoll::Linked);
    assert!(matches!(
        nav.link_poll().await.unwrap_err(),
        NavigraphError::LinkNotStarted
    ));

    let reqs = server.requests();
    assert_eq!(reqs[0].method, "POST");
    let auth = reqs[0].form();
    assert_eq!(auth["client_id"], "cid");
    assert_eq!(auth["client_secret"], "csecret");
    assert_eq!(auth["code_challenge_method"], "S256");
    let token = reqs[1].form();
    assert_eq!(
        token["grant_type"],
        "urn:ietf:params:oauth:grant-type:device_code"
    );
    assert_eq!(token["device_code"], "dev-code");
    assert_eq!(token["scope"], SCOPES);
    assert_eq!(token["code_verifier"].len(), 43);

    assert_eq!(store.load().await.unwrap().unwrap().expose(), "refresh-1");
    let status = nav.status().await;
    assert!(status.linked && status.session_live && !status.relink_required);
    assert_eq!(nav.bearer().await.unwrap().expose(), "access-1");
    let cookies = nav.tile_cookies().await.unwrap();
    assert!(cookies.is_complete());
    assert_eq!(cookies.policy, "pol");

    nav.unlink().await.unwrap();
    assert!(store.load().await.unwrap().is_none());
    let status = nav.status().await;
    assert!(!status.linked && !status.session_live);
    assert!(matches!(
        nav.bearer().await.unwrap_err(),
        NavigraphError::NotLinked
    ));
}

#[tokio::test]
async fn denied_and_expired_end_the_attempt() {
    let polls = Arc::new(AtomicUsize::new(0));
    let counter = polls.clone();
    let server = TestServer::start(move |req: &Request| match req.path() {
        "/connect/deviceauthorization" => device_auth(),
        _ => match counter.fetch_add(1, Ordering::SeqCst) {
            0 => oauth_error("access_denied"),
            1 => oauth_error("expired_token"),
            _ => oauth_error("server_on_fire"),
        },
    })
    .await;
    let nav = navigraph(&server, Arc::new(MemoryTokenStore::default()));

    nav.link_start().await.unwrap();
    assert_eq!(nav.link_poll().await.unwrap(), LinkPoll::Denied);
    assert!(nav.link_poll().await.is_err());

    nav.link_start().await.unwrap();
    assert_eq!(nav.link_poll().await.unwrap(), LinkPoll::Expired);

    nav.link_start().await.unwrap();
    match nav.link_poll().await.unwrap_err() {
        NavigraphError::Api { status: 400, body } => assert!(body.contains("server_on_fire")),
        other => panic!("expected Api error, got {other:?}"),
    }
}

#[tokio::test]
async fn store_write_failure_still_links_for_the_session() {
    #[derive(Debug)]
    struct BrokenStore;
    #[async_trait::async_trait]
    impl RefreshTokenStore for BrokenStore {
        async fn load(&self) -> infinity_ipc::IpcResult<Option<Secret>> {
            Err(IpcError::new(
                IpcErrorCode::NetworkUnreachable,
                "vault down",
            ))
        }
        async fn save(&self, _: &Secret) -> infinity_ipc::IpcResult<()> {
            Err(IpcError::new(
                IpcErrorCode::NetworkUnreachable,
                "vault down",
            ))
        }
        async fn clear(&self) -> infinity_ipc::IpcResult<()> {
            Err(IpcError::new(
                IpcErrorCode::NetworkUnreachable,
                "vault down",
            ))
        }
    }
    let server = TestServer::start(|req: &Request| match req.path() {
        "/connect/deviceauthorization" => device_auth(),
        _ => tokens_response("access-1", "refresh-1"),
    })
    .await;
    let nav = navigraph(&server, Arc::new(BrokenStore));
    let before = nav.status().await;
    assert_eq!(before.vault_error.as_deref(), Some("vault down"));
    assert!(!before.linked);

    nav.link_start().await.unwrap();
    assert_eq!(nav.link_poll().await.unwrap(), LinkPoll::Linked);
    assert_eq!(nav.bearer().await.unwrap().expose(), "access-1");
    assert!(nav.status().await.linked);

    let err = IpcError::from(nav.unlink().await.unwrap_err());
    assert_eq!(err.code, IpcErrorCode::NetworkUnreachable);
}

#[tokio::test]
async fn cold_start_refreshes_and_rotates_the_stored_token() {
    let server = TestServer::start(|req: &Request| {
        let form = req.form();
        match form.get("refresh_token").map(String::as_str) {
            Some("stored") => tokens_response("access-2", "rotated"),
            _ => oauth_error("invalid_grant"),
        }
    })
    .await;
    let store = Arc::new(MemoryTokenStore::default());
    store.save(&Secret::new("stored")).await.unwrap();
    let nav = navigraph(&server, store.clone());

    assert_eq!(nav.bearer().await.unwrap().expose(), "access-2");
    assert_eq!(store.load().await.unwrap().unwrap().expose(), "rotated");
    let form = server.requests()[0].form();
    assert_eq!(form["grant_type"], "refresh_token");
    assert_eq!(form["client_secret"], "csecret");
}

#[tokio::test]
async fn invalid_grant_maps_to_relink() {
    let server = TestServer::start(|_: &Request| oauth_error("invalid_grant")).await;
    let client = NavigraphClient::new(Endpoints::single(server.base())).unwrap();
    let err = client
        .refresh(&AppCredentials::new("i", "s"), &Secret::new("dead"))
        .await
        .unwrap_err();
    assert!(matches!(err, NavigraphError::RelinkRequired));
    assert_eq!(IpcError::from(err).code, IpcErrorCode::AuthExpired);
}

#[tokio::test]
async fn api_statuses_map_to_errors() {
    let server = TestServer::start(|req: &Request| match req.path() {
        "/v1/navdata/packages" => Response::bytes(403, "no"),
        "/v2/charts/KXXX" => Response::bytes(404, ""),
        "/v2/charts/KEDW" => Response::json(200, serde_json::json!({ "charts": [1] })),
        "/styles/vfr/3/1/2.png" => Response::bytes(200, "png"),
        _ => Response::bytes(500, "boom"),
    })
    .await;
    let client = NavigraphClient::new(Endpoints::single(server.base())).unwrap();
    let bearer = Secret::new("tok");

    assert!(matches!(
        client.navdata_packages(&bearer).await.unwrap_err(),
        NavigraphError::NoSubscription
    ));
    assert_eq!(
        client.charts_index(&bearer, "KXXX").await.unwrap(),
        serde_json::json!({ "charts": [] })
    );
    assert_eq!(
        client.charts_index(&bearer, "KEDW").await.unwrap()["charts"][0],
        1
    );
    assert!(matches!(
        client
            .chart_bytes(&bearer, "KEDW", "x.png")
            .await
            .unwrap_err(),
        NavigraphError::Api { status: 500, .. }
    ));
    let cookies = TileCookies {
        policy: "p".into(),
        signature: "s".into(),
        key_pair_id: "k".into(),
    };
    assert_eq!(
        client.tile_bytes(&cookies, "vfr", 3, 1, 2).await.unwrap(),
        b"png"
    );

    let reqs = server.requests();
    assert_eq!(reqs[0].header("authorization"), Some("Bearer tok"));
    let tile = reqs
        .iter()
        .find(|r| r.path().starts_with("/styles"))
        .unwrap();
    assert_eq!(
        tile.header("cookie"),
        Some("CloudFront-Policy=p; CloudFront-Signature=s; CloudFront-Key-Pair-Id=k")
    );
}

// ---- navdata ----

/// A zip with the given entries.
fn zip_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut zip = zip::ZipWriter::new(&mut buf);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, data) in entries {
            if name.ends_with('/') {
                zip.add_directory(*name, opts).unwrap();
            } else {
                zip.start_file(*name, opts).unwrap();
                zip.write_all(data).unwrap();
            }
        }
        zip.finish().unwrap();
    }
    buf.into_inner()
}

fn sha_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

struct NavdataServer {
    server: TestServer,
    nav: Navigraph,
}

/// A linked account whose packages API lists one `current` T-38C package
/// served from `/files/t38.zip`.
async fn navdata_server(archive: Vec<u8>, listed_hash: Option<String>) -> NavdataServer {
    let base: Arc<OnceLock<String>> = Arc::new(OnceLock::new());
    let base_for_handler = base.clone();
    let hash = listed_hash.unwrap_or_else(|| sha_hex(&archive));
    let server = TestServer::start(move |req: &Request| match req.path() {
        "/connect/token" => tokens_response("access", "refresh-next"),
        "/v1/navdata/packages" => {
            let base = base_for_handler.get().cloned().unwrap_or_default();
            Response::json(
                200,
                serde_json::json!([
                    {
                        "package_id": "old", "cycle": "2605", "revision": "1",
                        "description": "Aero Dynamics T-38C DFD v2", "format": "dfdv2",
                        "package_status": "outdated", "files": []
                    },
                    {
                        "package_id": "p1", "cycle": "2606", "revision": "",
                        "description": "Aero Dynamics T-38C DFD v2", "format": "dfdv2",
                        "package_status": "current",
                        "files": [{
                            "key": "t38c.zip", "hash": hash.to_uppercase(),
                            "signed_url": format!("{base}files/t38.zip?Signature=xyz")
                        }]
                    }
                ]),
            )
        }
        "/files/t38.zip" => Response::bytes(200, archive.clone()),
        _ => Response::bytes(404, ""),
    })
    .await;
    let _ = base.set(server.base().to_string());
    let store = Arc::new(MemoryTokenStore::default());
    store.save(&Secret::new("refresh")).await.unwrap();
    let nav = navigraph(&server, store);
    NavdataServer { server, nav }
}

fn target(id: &str, package_match: &str, work_dir: Option<&Path>) -> NavdataTarget {
    NavdataTarget {
        product_id: id.into(),
        product_name: format!("{id} name"),
        spec: NavdataSpec {
            format: Some("DFDv2".into()),
            package_match: Some(package_match.into()),
        },
        work_dir: work_dir.map(Path::to_path_buf),
    }
}

fn write_cycle(work: &Path, cycle: &str, revision: &str) {
    let dir = work.join(NAVDATA_DIR);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(CYCLE_FILE),
        serde_json::json!({
            "cycle": cycle, "revision": revision, "name": "x",
            "format": "dfdv2", "validityPeriod": ""
        })
        .to_string(),
    )
    .unwrap();
}

#[tokio::test]
async fn report_compares_installed_cycles_with_latest() {
    let fx = navdata_server(zip_of(&[("a", b"a")]), None).await;
    let tmp = tempfile::tempdir().unwrap();
    let fresh = tmp.path().join("fresh");
    let stale = tmp.path().join("stale");
    let empty = tmp.path().join("empty");
    let other = tmp.path().join("other");
    write_cycle(&fresh, "2606", "1");
    write_cycle(&stale, "2605", "1");
    std::fs::create_dir_all(&empty).unwrap();
    write_cycle(&other, "2601", "1");

    let report = navdata_report(
        &fx.nav,
        &[
            target("absent", "t-38c", None),
            target("fresh", "t-38c", Some(&fresh)),
            target("stale", "T-38C", Some(&stale)),
            target("empty", "t-38c", Some(&empty)),
            target("other", "f-16", Some(&other)),
        ],
    )
    .await;

    assert!(report.linked);
    assert!(report.fetch_error.is_none());
    let item = |id: &str| report.items.iter().find(|i| i.product_id == id).unwrap();

    let absent = item("absent");
    assert!(!absent.installed && !absent.update_available && absent.latest.is_none());
    assert_eq!(absent.job_id, "navdata-absent");

    let fresh = item("fresh");
    assert!(fresh.installed && !fresh.update_available);
    let latest = fresh.latest.as_ref().unwrap();
    assert_eq!(latest.cycle, "2606");
    assert_eq!(latest.revision, "1");
    assert_eq!(
        latest.validity_period.as_deref(),
        Some("2026-06-11/2026-07-08")
    );
    assert_eq!(fresh.current.as_ref().unwrap().cycle, "2606");

    assert!(item("stale").update_available);
    let empty = item("empty");
    assert!(empty.update_available && empty.current.is_none());

    let other = item("other");
    assert!(other.latest.is_none() && !other.update_available);
    assert_eq!(
        other.note.as_deref(),
        Some("No matching Navigraph package for this product.")
    );
}

#[tokio::test]
async fn report_without_link_or_install_stays_local() {
    let server = TestServer::start(|_: &Request| Response::bytes(500, "")).await;
    let nav = navigraph(&server, Arc::new(MemoryTokenStore::default()));
    let tmp = tempfile::tempdir().unwrap();

    let report = navdata_report(&nav, &[target("a", "x", None)]).await;
    assert!(!report.linked && report.fetch_error.is_none());
    assert_eq!(report.items.len(), 1);

    let report = navdata_report(&nav, &[target("a", "x", Some(tmp.path()))]).await;
    assert!(!report.linked && report.fetch_error.is_none());
    assert!(report.items[0].update_available);
    assert!(server.requests().is_empty(), "nothing linked: no traffic");
}

#[tokio::test]
async fn report_surfaces_fetch_errors() {
    let server = TestServer::start(|req: &Request| match req.path() {
        "/connect/token" => tokens_response("access", "next"),
        _ => Response::bytes(403, ""),
    })
    .await;
    let store = Arc::new(MemoryTokenStore::default());
    store.save(&Secret::new("refresh")).await.unwrap();
    let nav = navigraph(&server, store);
    let tmp = tempfile::tempdir().unwrap();
    let report = navdata_report(&nav, &[target("a", "x", Some(tmp.path()))]).await;
    assert!(report.linked);
    assert!(report.fetch_error.unwrap().contains("subscription"));
    assert!(report.items[0].note.is_none());
}

#[tokio::test]
async fn update_installs_into_navigation_data() {
    let archive = zip_of(&[
        ("T38C-2606/", b""),
        ("T38C-2606/nav.db", b"fresh nav"),
        ("T38C-2606/procedures/KEDW.json", b"{}"),
    ]);
    let fx = navdata_server(archive, None).await;
    let tmp = tempfile::tempdir().unwrap();
    let work = tmp.path().join("work");
    std::fs::create_dir_all(work.join(NAVDATA_DIR)).unwrap();
    std::fs::write(work.join(NAVDATA_DIR).join("stale.db"), b"old").unwrap();
    std::fs::write(work.join("settings.json"), b"keep me").unwrap();

    let plan = plan_navdata_update(&fx.nav, &target("t38", "t-38c", Some(&work)))
        .await
        .unwrap();
    assert_eq!(plan.job_id, "navdata-t38");
    assert_eq!(plan.name, "t38 name Navdata — AIRAC 2606");
    assert_eq!(plan.cycle.revision, "1");
    assert!(!format!("{plan:?}").contains("Signature=xyz"));

    let mut phases = Vec::new();
    let installed = install_navdata(&download_client().unwrap(), &plan, &mut |p| phases.push(p))
        .await
        .unwrap();

    assert_eq!(installed, work.join(NAVDATA_DIR));
    assert_eq!(
        std::fs::read(installed.join("nav.db")).unwrap(),
        b"fresh nav"
    );
    assert!(installed.join("procedures").join("KEDW.json").is_file());
    assert!(!installed.join("stale.db").exists());
    assert!(!installed.join("T38C-2606").exists());
    let cycle: CycleJson =
        serde_json::from_slice(&std::fs::read(installed.join(CYCLE_FILE)).unwrap()).unwrap();
    assert_eq!(cycle, plan.cycle);
    assert_eq!(cycle.validity_period, "2026-06-11/2026-07-08");
    assert_eq!(read_cycle_json(&work).await.unwrap(), plan.cycle);
    assert_eq!(
        std::fs::read(work.join("settings.json")).unwrap(),
        b"keep me"
    );

    let mut names: Vec<String> = std::fs::read_dir(&work)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, vec![NAVDATA_DIR.to_string(), "settings.json".into()]);

    assert!(matches!(
        phases.first(),
        Some(NavdataProgress::Downloading { done: 0, .. })
    ));
    assert_eq!(phases.last(), Some(&NavdataProgress::Finalizing));
    assert_eq!(fx.server.hits("/files/t38.zip"), 1);

    // Up to date now.
    let report = navdata_report(&fx.nav, &[target("t38", "t-38c", Some(&work))]).await;
    assert!(!report.items[0].update_available);
}

#[tokio::test]
async fn shipped_cycle_json_is_kept() {
    let shipped =
        br#"{"cycle":"2606","revision":"7","name":"hub","format":"dfdv2","validityPeriod":"x"}"#;
    let archive = zip_of(&[("cycle.json", shipped), ("nav.db", b"n")]);
    let fx = navdata_server(archive, None).await;
    let tmp = tempfile::tempdir().unwrap();
    let plan = plan_navdata_update(&fx.nav, &target("t38", "t-38c", Some(tmp.path())))
        .await
        .unwrap();
    let dir = install_navdata(&download_client().unwrap(), &plan, &mut |_| {})
        .await
        .unwrap();
    assert_eq!(std::fs::read(dir.join(CYCLE_FILE)).unwrap(), shipped);
    assert!(dir.join("nav.db").is_file());
}

#[tokio::test]
async fn checksum_mismatch_leaves_the_old_navdata() {
    let fx = navdata_server(zip_of(&[("nav.db", b"new")]), Some("ab".repeat(32))).await;
    let tmp = tempfile::tempdir().unwrap();
    let work = tmp.path();
    write_cycle(work, "2605", "1");

    let plan = plan_navdata_update(&fx.nav, &target("t38", "t-38c", Some(work)))
        .await
        .unwrap();
    let err = install_navdata(&download_client().unwrap(), &plan, &mut |_| {})
        .await
        .unwrap_err();
    assert!(matches!(err, NavigraphError::Checksum { .. }));
    assert_eq!(IpcError::from(err).code, IpcErrorCode::CdnSignatureInvalid);
    assert_eq!(read_cycle_json(work).await.unwrap().cycle, "2605");
    assert_eq!(std::fs::read_dir(work).unwrap().count(), 1, "no leftovers");
}

#[tokio::test]
async fn interrupted_swap_is_recovered() {
    let fx = navdata_server(zip_of(&[("nav.db", b"new")]), None).await;
    let tmp = tempfile::tempdir().unwrap();
    let work = tmp.path();
    // A crash between the two renames left only the backup.
    let old = work.join(format!(".{NAVDATA_DIR}.old"));
    std::fs::create_dir_all(&old).unwrap();
    std::fs::write(old.join("nav.db"), b"old").unwrap();

    let plan = plan_navdata_update(&fx.nav, &target("t38", "t-38c", Some(work)))
        .await
        .unwrap();
    // Make the download fail so only the recovery runs.
    let bad = NavdataUpdatePlan {
        url: Secret::new(fx.server.url("missing.zip").to_string()),
        ..plan
    };
    let err = install_navdata(&download_client().unwrap(), &bad, &mut |_| {})
        .await
        .unwrap_err();
    assert!(matches!(err, NavigraphError::Download(404)));
    assert_eq!(IpcError::from(err).code, IpcErrorCode::CdnNotFound);
    assert_eq!(
        std::fs::read(work.join(NAVDATA_DIR).join("nav.db")).unwrap(),
        b"old"
    );
    assert!(!old.exists());
}

#[tokio::test]
async fn unsafe_archive_is_rejected() {
    let fx = navdata_server(zip_of(&[("../evil.txt", b"x")]), None).await;
    let tmp = tempfile::tempdir().unwrap();
    let work = tmp.path().join("work");
    let plan = plan_navdata_update(&fx.nav, &target("t38", "t-38c", Some(&work)))
        .await
        .unwrap();
    let err = install_navdata(&download_client().unwrap(), &plan, &mut |_| {})
        .await
        .unwrap_err();
    assert!(matches!(err, NavigraphError::Archive(_)));
    assert!(!tmp.path().join("evil.txt").exists());
    assert!(!work.join(NAVDATA_DIR).exists());
}

#[tokio::test]
async fn plan_errors() {
    let fx = navdata_server(zip_of(&[("a", b"a")]), None).await;
    let tmp = tempfile::tempdir().unwrap();
    let err = plan_navdata_update(&fx.nav, &target("t38", "t-38c", None))
        .await
        .unwrap_err();
    assert_eq!(IpcError::from(err).code, IpcErrorCode::PackageDirNotFound);
    let err = plan_navdata_update(&fx.nav, &target("f16", "f-16", Some(tmp.path())))
        .await
        .unwrap_err();
    assert!(matches!(err, NavigraphError::NoMatchingPackage));
}
