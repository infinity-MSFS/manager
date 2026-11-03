//! End-to-end jobs against a local HTTP server.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Notify;

use super::*;
use crate::extract::tests::zip_of;
use crate::tarzst::tests::{file, tar_zst_of};

const ETAG: &str = "\"v1\"";

#[derive(Clone, Copy)]
enum Fault {
    None,
    /// The first archive response closes after this many bytes.
    Drop(usize),
    /// The first archive response stalls after this many bytes until released.
    Stall(usize),
}

struct Server {
    base: Url,
    requests: Arc<Mutex<Vec<String>>>,
    release: Arc<Notify>,
}

impl Server {
    async fn start(files: HashMap<&'static str, Vec<u8>>, fault: Fault) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let release = Arc::new(Notify::new());
        let files = Arc::new(files);
        let archive_hits = Arc::new(AtomicUsize::new(0));
        {
            let requests = requests.clone();
            let release = release.clone();
            tokio::spawn(async move {
                loop {
                    let Ok((socket, _)) = listener.accept().await else {
                        return;
                    };
                    let (files, requests, release, hits) = (
                        files.clone(),
                        requests.clone(),
                        release.clone(),
                        archive_hits.clone(),
                    );
                    tokio::spawn(async move {
                        serve(socket, &files, &requests, &release, &hits, fault).await;
                    });
                }
            });
        }
        Self {
            base,
            requests,
            release,
        }
    }

    fn url(&self, path: &str) -> Url {
        self.base.join(path).unwrap()
    }

    fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }
}

async fn serve(
    mut socket: tokio::net::TcpStream,
    files: &HashMap<&'static str, Vec<u8>>,
    requests: &Mutex<Vec<String>>,
    release: &Notify,
    hits: &AtomicUsize,
    fault: Fault,
) {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if socket.read(&mut byte).await.unwrap_or(0) == 0 {
            return;
        }
        head.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&head).into_owned();
    requests.lock().unwrap().push(head.clone());
    let path = head
        .split_whitespace()
        .nth(1)
        .unwrap_or("/")
        .trim_start_matches('/');
    let header = |name: &str| {
        head.lines().find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.eq_ignore_ascii_case(name).then(|| v.trim().to_owned())
        })
    };
    let Some(body) = files.get(path) else {
        let _ = socket
            .write_all(b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\n\r\n")
            .await;
        return;
    };
    let start = header("range")
        .filter(|_| header("if-range").as_deref() == Some(ETAG))
        .and_then(|r| {
            r.strip_prefix("bytes=")?
                .strip_suffix('-')?
                .parse::<usize>()
                .ok()
        });
    let (status, slice) = match start {
        Some(s) => (
            format!(
                "206 Partial Content\r\ncontent-range: bytes {s}-{}/{}",
                body.len() - 1,
                body.len()
            ),
            &body[s..],
        ),
        None => ("200 OK".to_owned(), &body[..]),
    };
    let response = format!(
        "HTTP/1.1 {status}\r\ncontent-length: {}\r\netag: {ETAG}\r\nconnection: close\r\n\r\n",
        slice.len()
    );
    let _ = socket.write_all(response.as_bytes()).await;

    let first_archive = path.ends_with(".zip") && hits.fetch_add(1, Ordering::SeqCst) == 0;
    match fault {
        Fault::Drop(n) if first_archive => {
            let _ = socket.write_all(&slice[..n]).await;
        }
        Fault::Stall(n) if first_archive => {
            let _ = socket.write_all(&slice[..n]).await;
            let _ = socket.flush().await;
            release.notified().await;
        }
        _ => {
            let _ = socket.write_all(slice).await;
        }
    }
}

struct Fixture {
    tmp: tempfile::TempDir,
    community: PathBuf,
    registry: Arc<InstalledStore>,
    installer: Installer,
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let community = tmp.path().join("Community");
        std::fs::create_dir_all(&community).unwrap();
        let registry = Arc::new(InstalledStore::new(&tmp.path().join("appdata")));
        let installer = Installer::new(registry.clone()).unwrap();
        Self {
            tmp,
            community,
            registry,
            installer,
        }
    }

    fn plan(&self, server: &Server, manifest: bool) -> Plan {
        Plan {
            product_id: "aerodynamics-test".into(),
            name: "Test".into(),
            version: "1.0.0".into(),
            channel: Channel::Stable,
            archive_url: server.url("pkg.zip"),
            archive_key: "packages/aerodynamics-test/1.0.0/pkg.zip".into(),
            format: ArchiveFormat::Zip,
            manifest_url: manifest.then(|| server.url("manifest.json")),
            package_dir_name: "aerodynamics-test".into(),
            install_dir: self.community.clone(),
        }
    }

    fn package(&self) -> PathBuf {
        self.community.join("aerodynamics-test")
    }
}

/// A zip of a small package plus the manifest of its contents.
fn package(tmp: &Path, payload: &[u8]) -> (Vec<u8>, Manifest) {
    let src = tmp.join("src");
    std::fs::create_dir_all(src.join("SimObjects")).unwrap();
    std::fs::write(src.join("manifest.json"), b"{}").unwrap();
    std::fs::write(src.join("SimObjects/model.bin"), payload).unwrap();
    let archive = tmp.join("upload.zip");
    zip_of(
        &archive,
        &[
            ("aerodynamics-test/manifest.json", b"{}"),
            ("aerodynamics-test/SimObjects/model.bin", payload),
        ],
    );
    let manifest = Manifest::build(&src, Some(&archive)).unwrap();
    (std::fs::read(archive).unwrap(), manifest)
}

fn noise(len: usize) -> Vec<u8> {
    let mut x: u32 = 7;
    (0..len)
        .map(|_| {
            x = x.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            (x >> 16) as u8
        })
        .collect()
}

async fn until(rx: &mut broadcast::Receiver<Job>, done: impl Fn(&Job) -> bool) -> Job {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let job = rx.recv().await.unwrap();
            if done(&job) {
                return job;
            }
        }
    })
    .await
    .expect("job did not reach the expected phase")
}

fn terminal(job: &Job) -> bool {
    !job.is_active()
}

#[tokio::test]
async fn installs_verifies_and_records() {
    let fx = Fixture::new();
    let (zip, manifest) = package(fx.tmp.path(), &noise(300_000));
    let server = Server::start(
        HashMap::from([
            ("pkg.zip", zip),
            ("manifest.json", serde_json::to_vec(&manifest).unwrap()),
        ]),
        Fault::None,
    )
    .await;
    let mut rx = fx.installer.subscribe();
    fx.installer.start(fx.plan(&server, true)).unwrap();
    let job = until(&mut rx, terminal).await;
    assert_eq!(job.phase, JobPhase::Completed);

    let pkg = fx.package();
    assert_eq!(std::fs::read(pkg.join("manifest.json")).unwrap(), b"{}");
    assert_eq!(package_version(&pkg).as_deref(), Some("1.0.0"));
    let record = fx.registry.get("aerodynamics-test").await.unwrap().unwrap();
    assert_eq!(record.version.as_deref(), Some("1.0.0"));
    assert_eq!(PathBuf::from(record.path), pkg);
    assert!(!fx.tmp.path().join(".infinity-staging").exists());
    assert!(fx.installer.jobs().is_empty());
}

#[tokio::test]
async fn damaged_package_never_reaches_the_install_folder() {
    let fx = Fixture::new();
    let (zip, mut manifest) = package(fx.tmp.path(), &noise(10_000));
    // The upload's manifest says a different model than the archive holds.
    if let Some(f) = manifest.files.get_mut("SimObjects/model.bin") {
        f.sha256 = "0".repeat(64);
    }
    manifest.archive = None;
    let server = Server::start(
        HashMap::from([
            ("pkg.zip", zip),
            ("manifest.json", serde_json::to_vec(&manifest).unwrap()),
        ]),
        Fault::None,
    )
    .await;
    std::fs::create_dir_all(fx.package()).unwrap();
    std::fs::write(fx.package().join("old.txt"), "old").unwrap();

    let mut rx = fx.installer.subscribe();
    fx.installer.start(fx.plan(&server, true)).unwrap();
    let job = until(&mut rx, terminal).await;
    let JobPhase::Failed { error } = job.phase else {
        panic!("expected failure, got {:?}", job.phase);
    };
    assert_eq!(error.code, IpcErrorCode::ManifestInvalid);
    assert!(
        error.message.contains("SimObjects/model.bin is damaged"),
        "{}",
        error.message
    );
    // Downloaded twice: once, then again from scratch.
    let archive_requests = server
        .requests()
        .iter()
        .filter(|r| r.contains("pkg.zip"))
        .count();
    assert_eq!(archive_requests, 2);
    assert_eq!(
        std::fs::read_to_string(fx.package().join("old.txt")).unwrap(),
        "old"
    );
}

#[tokio::test]
async fn archive_checksum_mismatch_fails_before_extracting() {
    let fx = Fixture::new();
    let (zip, mut manifest) = package(fx.tmp.path(), &noise(10_000));
    if let Some(a) = manifest.archive.as_mut() {
        a.sha256 = "f".repeat(64);
    }
    let server = Server::start(
        HashMap::from([
            ("pkg.zip", zip),
            ("manifest.json", serde_json::to_vec(&manifest).unwrap()),
        ]),
        Fault::None,
    )
    .await;
    let mut rx = fx.installer.subscribe();
    fx.installer.start(fx.plan(&server, true)).unwrap();
    let job = until(&mut rx, terminal).await;
    let JobPhase::Failed { error } = job.phase else {
        panic!("expected failure, got {:?}", job.phase);
    };
    assert_eq!(error.code, IpcErrorCode::CdnSignatureInvalid);
    assert!(!fx.package().exists());
}

#[tokio::test]
async fn dropped_connection_resumes_with_if_range() {
    let fx = Fixture::new();
    let (zip, manifest) = package(fx.tmp.path(), &noise(400_000));
    let server = Server::start(
        HashMap::from([
            ("pkg.zip", zip),
            ("manifest.json", serde_json::to_vec(&manifest).unwrap()),
        ]),
        Fault::Drop(100_000),
    )
    .await;
    let mut rx = fx.installer.subscribe();
    fx.installer.start(fx.plan(&server, true)).unwrap();
    assert_eq!(until(&mut rx, terminal).await.phase, JobPhase::Completed);
    let resumed = server
        .requests()
        .into_iter()
        .filter(|r| r.contains("pkg.zip"))
        .nth(1)
        .unwrap()
        .to_lowercase();
    assert!(resumed.contains("range: bytes=100000-"), "{resumed}");
    assert!(resumed.contains("if-range: \"v1\""), "{resumed}");
}

#[tokio::test]
async fn pause_keeps_the_download_and_a_new_start_resumes_it() {
    let fx = Fixture::new();
    let (zip, _) = package(fx.tmp.path(), &noise(400_000));
    let server = Server::start(HashMap::from([("pkg.zip", zip)]), Fault::Stall(50_000)).await;
    let mut rx = fx.installer.subscribe();
    fx.installer.start(fx.plan(&server, false)).unwrap();
    until(&mut rx, |j| j.progress.is_some()).await;
    // Let the stalled response's first bytes land on disk.
    tokio::time::sleep(Duration::from_millis(500)).await;

    fx.installer.pause("aerodynamics-test").unwrap();
    assert_eq!(until(&mut rx, terminal).await.phase, JobPhase::Paused);
    server.release.notify_waiters();
    assert_eq!(fx.installer.jobs()[0].phase, JobPhase::Paused);

    fx.installer.start(fx.plan(&server, false)).unwrap();
    assert_eq!(until(&mut rx, terminal).await.phase, JobPhase::Completed);
    let last = server.requests().pop().unwrap().to_lowercase();
    assert!(last.contains("range: bytes=50000-"), "{last}");
}

#[tokio::test]
async fn cancelling_a_paused_job_discards_it() {
    let fx = Fixture::new();
    let (zip, _) = package(fx.tmp.path(), &noise(400_000));
    let server = Server::start(HashMap::from([("pkg.zip", zip)]), Fault::Stall(50_000)).await;
    let mut rx = fx.installer.subscribe();
    fx.installer.start(fx.plan(&server, false)).unwrap();
    until(&mut rx, |j| j.progress.is_some()).await;
    fx.installer.pause("aerodynamics-test").unwrap();
    until(&mut rx, terminal).await;
    server.release.notify_waiters();

    fx.installer.cancel("aerodynamics-test").await.unwrap();
    assert!(fx.installer.jobs().is_empty());
    assert!(!fx.tmp.path().join(".infinity-staging").exists());
}

#[tokio::test]
async fn missing_archive_reports_not_found() {
    let fx = Fixture::new();
    let server = Server::start(HashMap::new(), Fault::None).await;
    let mut rx = fx.installer.subscribe();
    fx.installer.start(fx.plan(&server, false)).unwrap();
    let JobPhase::Failed { error } = until(&mut rx, terminal).await.phase else {
        panic!("expected failure");
    };
    assert_eq!(error.code, IpcErrorCode::CdnNotFound);
}

#[tokio::test]
async fn uninstall_refuses_unexpected_paths() {
    let fx = Fixture::new();
    std::fs::create_dir_all(fx.package()).unwrap();
    let record = |path: &Path, dir: &str| InstalledPackage {
        product_id: "aerodynamics-test".into(),
        version: None,
        path: path.to_string_lossy().into_owned(),
        package_dir_name: dir.into(),
        installed_at: None,
    };
    for bad in [
        record(&fx.community, "aerodynamics-test"),
        record(&fx.package(), ".."),
    ] {
        let err = fx.installer.uninstall(&bad).await.unwrap_err();
        assert_eq!(err.code, IpcErrorCode::PackageDirNotFound);
    }
    assert!(fx.package().exists());

    fx.registry
        .upsert(record(&fx.package(), "aerodynamics-test"))
        .await
        .unwrap();
    fx.installer
        .uninstall(&record(&fx.package(), "aerodynamics-test"))
        .await
        .unwrap();
    assert!(!fx.package().exists());
    assert!(fx.registry.list().await.unwrap().is_empty());
}

#[test]
fn plans_with_unsafe_folder_names_are_rejected() {
    for bad in ["", "..", "a/b", "a\\..\\b", "/abs"] {
        assert!(
            !is_plain_name(bad) || cfg!(not(windows)) && bad.contains('\\'),
            "{bad}"
        );
    }
    assert!(is_plain_name("aerodynamics-t38c"));
}

/// A `.tar.zst` of the same package as [`package`], with its manifest.
fn tar_package(tmp: &Path, payload: &[u8]) -> (Vec<u8>, Manifest) {
    let src = tmp.join("src");
    std::fs::create_dir_all(src.join("SimObjects")).unwrap();
    std::fs::write(src.join("manifest.json"), b"{}").unwrap();
    std::fs::write(src.join("SimObjects/model.bin"), payload).unwrap();
    let archive = tmp.join("upload.tar.zst");
    tar_zst_of(
        &archive,
        &[
            file("aerodynamics-test/", b""),
            file("aerodynamics-test/manifest.json", b"{}"),
            file("aerodynamics-test/SimObjects/model.bin", payload),
        ],
    );
    let manifest = Manifest::build(&src, Some(&archive)).unwrap();
    (std::fs::read(archive).unwrap(), manifest)
}

#[tokio::test]
async fn installs_a_tar_zst_package() {
    let fx = Fixture::new();
    let (archive, manifest) = tar_package(fx.tmp.path(), &noise(300_000));
    let server = Server::start(
        HashMap::from([
            ("pkg.tar.zst", archive),
            ("manifest.json", serde_json::to_vec(&manifest).unwrap()),
        ]),
        Fault::None,
    )
    .await;
    let plan = Plan {
        archive_url: server.url("pkg.tar.zst"),
        format: ArchiveFormat::TarZst,
        ..fx.plan(&server, true)
    };
    let mut rx = fx.installer.subscribe();
    fx.installer.start(plan).unwrap();
    let job = until(&mut rx, terminal).await;
    assert_eq!(job.phase, JobPhase::Completed);
    let pkg = fx.package();
    assert_eq!(std::fs::read(pkg.join("manifest.json")).unwrap(), b"{}");
    assert_eq!(
        std::fs::read(pkg.join("SimObjects/model.bin"))
            .unwrap()
            .len(),
        300_000
    );
    assert_eq!(package_version(&pkg).as_deref(), Some("1.0.0"));
    assert!(!fx.tmp.path().join(".infinity-staging").exists());
}

#[tokio::test]
async fn a_tar_zst_that_differs_from_its_manifest_is_refused() {
    let fx = Fixture::new();
    let (archive, mut manifest) = tar_package(fx.tmp.path(), &noise(10_000));
    manifest.archive = None;
    manifest.files.insert(
        "SimObjects/extra.bin".into(),
        manifest::FileDigest {
            size: 1,
            sha256: "0".repeat(64),
        },
    );
    let server = Server::start(
        HashMap::from([
            ("pkg.tar.zst", archive),
            ("manifest.json", serde_json::to_vec(&manifest).unwrap()),
        ]),
        Fault::None,
    )
    .await;
    let plan = Plan {
        archive_url: server.url("pkg.tar.zst"),
        format: ArchiveFormat::TarZst,
        ..fx.plan(&server, true)
    };
    let mut rx = fx.installer.subscribe();
    fx.installer.start(plan).unwrap();
    let JobPhase::Failed { error } = until(&mut rx, terminal).await.phase else {
        panic!("expected failure");
    };
    assert_eq!(error.code, IpcErrorCode::ManifestInvalid);
    assert!(
        error.message.contains("SimObjects/extra.bin is missing"),
        "{}",
        error.message
    );
    assert!(!fx.package().exists());
}

#[tokio::test]
async fn a_mislabelled_archive_fails_without_downloading_again() {
    let fx = Fixture::new();
    let (zip, _) = package(fx.tmp.path(), &noise(10_000));
    let server = Server::start(HashMap::from([("pkg.zip", zip)]), Fault::None).await;
    let plan = Plan {
        format: ArchiveFormat::TarZst,
        ..fx.plan(&server, false)
    };
    let mut rx = fx.installer.subscribe();
    fx.installer.start(plan).unwrap();
    let JobPhase::Failed { error } = until(&mut rx, terminal).await.phase else {
        panic!("expected failure");
    };
    assert_eq!(error.code, IpcErrorCode::ManifestInvalid);
    assert!(error.message.contains("tar.zst"), "{}", error.message);
    assert_eq!(server.requests().len(), 1);
    assert!(!fx.package().exists());
}

#[test]
fn unknown_formats_are_refused_up_front() {
    let fx = Fixture::new();
    let plan = Plan {
        product_id: "aerodynamics-test".into(),
        name: "Test".into(),
        version: "1.0.0".into(),
        channel: Channel::Stable,
        archive_url: Url::parse("http://127.0.0.1:9/pkg").unwrap(),
        archive_key: "k".into(),
        format: ArchiveFormat::Unknown,
        manifest_url: None,
        package_dir_name: "aerodynamics-test".into(),
        install_dir: fx.community.clone(),
    };
    let err = fx.installer.start(plan).unwrap_err();
    assert_eq!(err.code, IpcErrorCode::Unsupported);
    assert!(fx.installer.jobs().is_empty());
}

#[tokio::test]
async fn a_paused_download_survives_a_restart() {
    let fx = Fixture::new();
    let (zip, _) = package(fx.tmp.path(), &noise(400_000));
    let server = Server::start(HashMap::from([("pkg.zip", zip)]), Fault::Stall(50_000)).await;
    let mut rx = fx.installer.subscribe();
    fx.installer.start(fx.plan(&server, false)).unwrap();
    until(&mut rx, |j| j.progress.is_some()).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    fx.installer.pause("aerodynamics-test").unwrap();
    assert_eq!(until(&mut rx, terminal).await.phase, JobPhase::Paused);
    server.release.notify_waiters();

    // A new process: same disk, no jobs in memory.
    let installer = Installer::new(fx.registry.clone()).unwrap();
    assert!(installer.jobs().is_empty());
    let found = installer
        .find_interrupted(vec![fx.community.clone()], KEEP_INTERRUPTED_FOR)
        .await;
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].product_id(), "aerodynamics-test");
    assert_eq!(found[0].version(), "1.0.0");
    assert_eq!(found[0].channel(), Channel::Stable);

    let mut rx = installer.subscribe();
    let found = found.into_iter().next().unwrap();
    assert!(installer.restore(found));
    let listed = rx.recv().await.unwrap();
    assert_eq!(listed.phase, JobPhase::Paused);
    assert_eq!(listed.name, "Test");
    assert_eq!(listed.progress.map(|p| p.done), Some(50_000));
    assert_eq!(installer.jobs(), vec![listed]);

    // Resuming is an ordinary start, which continues the partial file.
    installer.start(fx.plan(&server, false)).unwrap();
    assert_eq!(until(&mut rx, terminal).await.phase, JobPhase::Completed);
    let last = server.requests().pop().unwrap().to_lowercase();
    assert!(last.contains("range: bytes=50000-"), "{last}");
    assert!(last.contains(&format!("if-range: {ETAG}")), "{last}");
    assert!(fx.package().join("manifest.json").is_file());
}

#[tokio::test]
async fn a_changed_archive_key_starts_the_download_over() {
    let fx = Fixture::new();
    let (zip, _) = package(fx.tmp.path(), &noise(400_000));
    let server = Server::start(HashMap::from([("pkg.zip", zip)]), Fault::Stall(50_000)).await;
    let mut rx = fx.installer.subscribe();
    fx.installer.start(fx.plan(&server, false)).unwrap();
    until(&mut rx, |j| j.progress.is_some()).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    fx.installer.pause("aerodynamics-test").unwrap();
    until(&mut rx, terminal).await;
    server.release.notify_waiters();

    let plan = Plan {
        archive_key: "packages/aerodynamics-test/1.0.0/rebuilt.zip".into(),
        ..fx.plan(&server, false)
    };
    fx.installer.start(plan).unwrap();
    assert_eq!(until(&mut rx, terminal).await.phase, JobPhase::Completed);
    let last = server.requests().pop().unwrap().to_lowercase();
    assert!(!last.contains("range:"), "{last}");
}

/// Writes a partial download and its state as a previous run would have.
fn leave_download(fx: &Fixture, product_id: &str, record: bool) -> Staging {
    let staging = Staging::create(&fx.community, product_id).unwrap();
    std::fs::write(staging.download(), vec![1u8; 1000]).unwrap();
    let mut state = serde_json::json!({
        "version": "1.0.0",
        "etag": ETAG,
        "lastModified": null,
        "total": 5000,
        "complete": false,
        "updatedAt": unix_now(),
    });
    if record {
        state["record"] = serde_json::json!({
            "productId": product_id,
            "name": "Left behind",
            "channel": "stable",
            "packageDirName": product_id,
            "installDir": fx.community,
            "format": "zip",
            "archiveKey": "k",
        });
    }
    std::fs::write(staging.download_state(), state.to_string()).unwrap();
    staging
}

#[test]
fn discovery_lists_recent_downloads_and_removes_stale_ones() {
    let fx = Fixture::new();
    let kept = leave_download(&fx, "aerodynamics-kept", true);
    let legacy = leave_download(&fx, "aerodynamics-legacy", false);
    let junk = kept.root().join("junk.download.json");
    std::fs::write(&junk, b"not json").unwrap();

    let found = resume::find(
        std::slice::from_ref(&fx.community),
        unix_now(),
        KEEP_INTERRUPTED_FOR,
    );
    assert_eq!(found.len(), 1);
    let job = found[0].job();
    assert_eq!(job.product_id, "aerodynamics-kept");
    assert_eq!(job.name, "Left behind");
    assert_eq!(job.phase, JobPhase::Paused);
    assert_eq!(
        job.progress,
        Some(Progress {
            done: 1000,
            total: Some(5000)
        })
    );
    // Recent leftovers it cannot list are left alone.
    assert!(legacy.download().is_file());
    assert!(junk.is_file());

    // A month and a day later, everything is cleaned up.
    let later = unix_now() + KEEP_INTERRUPTED_FOR.as_secs() + 86_400;
    assert!(
        resume::find(
            std::slice::from_ref(&fx.community),
            later,
            KEEP_INTERRUPTED_FOR
        )
        .is_empty()
    );
    for gone in [
        kept.download(),
        kept.download_state(),
        legacy.download(),
        legacy.download_state(),
        junk,
    ] {
        assert!(!gone.exists(), "{}", gone.display());
    }
}

#[tokio::test]
async fn discarding_or_cancelling_a_found_download_deletes_it() {
    let fx = Fixture::new();
    let a = leave_download(&fx, "aerodynamics-a", true);
    let b = leave_download(&fx, "aerodynamics-b", true);
    let mut found = fx
        .installer
        .find_interrupted(vec![fx.community.clone()], KEEP_INTERRUPTED_FOR)
        .await;
    assert_eq!(found.len(), 2);
    let second = found.pop().unwrap();
    let first = found.pop().unwrap();
    assert_eq!(first.product_id(), "aerodynamics-a");

    fx.installer.discard(first).await;
    assert!(!a.download().exists() && !a.download_state().exists());

    assert!(fx.installer.restore(second));
    fx.installer.cancel("aerodynamics-b").await.unwrap();
    assert!(!b.download().exists());
    assert!(fx.installer.jobs().is_empty());
    assert!(!fx.tmp.path().join(".infinity-staging").exists());
}

#[tokio::test]
async fn restore_never_replaces_a_live_job() {
    let fx = Fixture::new();
    let (zip, _) = package(fx.tmp.path(), &noise(400_000));
    let server = Server::start(HashMap::from([("pkg.zip", zip)]), Fault::Stall(10)).await;
    let mut rx = fx.installer.subscribe();
    fx.installer.start(fx.plan(&server, false)).unwrap();
    until(&mut rx, |j| j.phase == JobPhase::Downloading).await;
    let found = resume::find(
        std::slice::from_ref(&fx.community),
        unix_now(),
        KEEP_INTERRUPTED_FOR,
    );
    for f in found {
        assert!(!fx.installer.restore(f));
    }
    assert!(fx.installer.jobs()[0].is_active());
    fx.installer.cancel("aerodynamics-test").await.unwrap();
    server.release.notify_waiters();
}
