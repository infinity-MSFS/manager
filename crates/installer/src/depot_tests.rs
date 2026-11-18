//! End-to-end depot jobs against a local HTTP server that speaks ranges.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use infinity_depot::build::{self, Options};
use infinity_depot::cdc::Params;
use infinity_depot::{Hash, Manifest as DepotManifest};
use infinity_ipc::IpcErrorCode;
use infinity_ipc::dto::{Channel, Job, JobKind, JobPhase};
use infinity_persistence::InstalledStore;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{Notify, broadcast};
use url::Url;

use crate::depot::stage::Dir;
use crate::{DepotSource, Installer, PackUrls, Plan, Source};

const ID: &str = "aerodynamics-depot";
const SMALL: Params = Params {
    min: 2048,
    avg: 8192,
    max: 32768,
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Fault {
    None,
    /// Pack responses close after this many bytes, the first time.
    DropOnce(usize),
    /// The first pack response has one byte flipped.
    CorruptOnce,
    /// Every pack response has one byte flipped.
    CorruptAlways,
    /// The first pack request is refused, as an expired URL is.
    ForbidOnce,
    /// Pack responses stall after this many bytes until released.
    Stall(usize),
}

#[derive(Clone)]
struct Server {
    base: Url,
    files: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    /// (path, bytes sent) per request.
    log: Arc<Mutex<Vec<(String, usize)>>>,
    release: Arc<Notify>,
}

impl Server {
    async fn start(fault: Fault) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        let server = Self {
            base,
            files: Arc::default(),
            log: Arc::default(),
            release: Arc::new(Notify::new()),
        };
        let hits = Arc::new(AtomicUsize::new(0));
        let s = server.clone();
        tokio::spawn(async move {
            loop {
                let Ok((socket, _)) = listener.accept().await else {
                    return;
                };
                let (s, hits) = (s.clone(), hits.clone());
                tokio::spawn(async move { s.serve(socket, &hits, fault).await });
            }
        });
        server
    }

    fn put(&self, key: &str, bytes: Vec<u8>) {
        self.files.lock().unwrap().insert(key.to_owned(), bytes);
    }

    fn url(&self, key: &str) -> Url {
        self.base.join(key).unwrap()
    }

    fn pack_bytes_sent(&self) -> usize {
        self.log
            .lock()
            .unwrap()
            .iter()
            .filter(|(p, _)| p.ends_with(".pack"))
            .map(|(_, n)| n)
            .sum()
    }

    fn clear_log(&self) {
        self.log.lock().unwrap().clear();
    }

    async fn serve(&self, mut socket: tokio::net::TcpStream, hits: &AtomicUsize, fault: Fault) {
        loop {
            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") {
                if socket.read(&mut byte).await.unwrap_or(0) == 0 {
                    return;
                }
                head.push(byte[0]);
            }
            let head = String::from_utf8_lossy(&head).into_owned();
            let path = head
                .split_whitespace()
                .nth(1)
                .unwrap_or("/")
                .trim_start_matches('/')
                .to_owned();
            let range = head.lines().find_map(|l| {
                let (k, v) = l.split_once(':')?;
                k.eq_ignore_ascii_case("range").then(|| v.trim().to_owned())
            });
            let Some(body) = self.files.lock().unwrap().get(&path).cloned() else {
                self.log.lock().unwrap().push((path, 0));
                let _ = socket
                    .write_all(b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\n\r\n")
                    .await;
                continue;
            };
            let is_pack = path.ends_with(".pack");
            let first = is_pack && hits.fetch_add(1, Ordering::SeqCst) == 0;
            if is_pack && fault == Fault::ForbidOnce && first {
                self.log.lock().unwrap().push((path, 0));
                let _ = socket
                    .write_all(b"HTTP/1.1 403 Forbidden\r\ncontent-length: 0\r\n\r\n")
                    .await;
                continue;
            }
            let (status, mut slice) = match range.as_deref().and_then(parse_range) {
                Some((a, b)) => {
                    let b = b.min(body.len() - 1);
                    (
                        format!(
                            "206 Partial Content\r\ncontent-range: bytes {a}-{b}/{}",
                            body.len()
                        ),
                        body[a..=b].to_vec(),
                    )
                }
                None => ("200 OK".to_owned(), body.clone()),
            };
            if is_pack
                && (fault == Fault::CorruptAlways || (fault == Fault::CorruptOnce && first))
                && !slice.is_empty()
            {
                let mid = slice.len() / 2;
                slice[mid] ^= 0x55;
            }
            let header = format!(
                "HTTP/1.1 {status}\r\ncontent-length: {}\r\n\r\n",
                slice.len()
            );
            let _ = socket.write_all(header.as_bytes()).await;
            let sent = match fault {
                Fault::DropOnce(n) if first => n.min(slice.len()),
                Fault::Stall(n) if is_pack => n.min(slice.len()),
                _ => slice.len(),
            };
            let _ = socket.write_all(&slice[..sent]).await;
            let _ = socket.flush().await;
            self.log.lock().unwrap().push((path, sent));
            match fault {
                Fault::DropOnce(_) if first => return,
                Fault::Stall(_) if is_pack && sent < slice.len() => {
                    self.release.notified().await;
                    return;
                }
                _ => {}
            }
        }
    }
}

fn parse_range(v: &str) -> Option<(usize, usize)> {
    let (a, b) = v.strip_prefix("bytes=")?.split_once('-')?;
    Some((a.parse().ok()?, b.parse().ok()?))
}

/// Pack URLs straight off the test server, counting lookups.
#[derive(Debug)]
struct Urls {
    base: Url,
    calls: AtomicUsize,
}

#[async_trait::async_trait]
impl PackUrls for Urls {
    async fn resolve(&self, keys: &[String]) -> infinity_ipc::IpcResult<Vec<Url>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(keys.iter().map(|k| self.base.join(k).unwrap()).collect())
    }
}

struct Release {
    manifest: DepotManifest,
    hash: Hash,
    key: String,
}

struct Fixture {
    tmp: tempfile::TempDir,
    community: PathBuf,
    registry: Arc<InstalledStore>,
    installer: Installer,
    server: Server,
    urls: Arc<Urls>,
    published: Vec<DepotManifest>,
}

impl Fixture {
    async fn new(fault: Fault) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let community = tmp.path().join("Community");
        std::fs::create_dir_all(&community).unwrap();
        let registry = Arc::new(InstalledStore::new(&tmp.path().join("appdata")));
        let installer = Installer::new(registry.clone()).unwrap();
        let server = Server::start(fault).await;
        let urls = Arc::new(Urls {
            base: server.base.clone(),
            calls: AtomicUsize::new(0),
        });
        Self {
            tmp,
            community,
            registry,
            installer,
            server,
            urls,
            published: Vec::new(),
        }
    }

    fn src(&self) -> PathBuf {
        self.tmp.path().join("src")
    }

    fn package(&self) -> PathBuf {
        self.community.join(ID)
    }

    /// Builds the source folder as `version` and puts it on the server.
    fn publish(&mut self, version: &str) -> Release {
        let out = self.tmp.path().join(format!("out-{version}"));
        let opts = Options {
            params: SMALL,
            level: 3,
            pack_target: 256 * 1024,
            ..Options::new(ID, version)
        };
        let built = build::build(
            &self.src(),
            &out,
            &opts,
            &self.published,
            &mut |_| {},
            &|| false,
        )
        .unwrap();
        for pack in &built.new_packs {
            self.server.put(
                &infinity_depot::keys::pack_key(ID, &pack.id),
                std::fs::read(&pack.path).unwrap(),
            );
        }
        let bytes = built.manifest.to_bytes().unwrap();
        let hash = Hash::of(&bytes);
        let key = infinity_depot::keys::manifest_key(ID, "stable", version, &hash);
        self.server.put(&key, bytes);
        self.published.push(built.manifest.clone());
        Release {
            manifest: built.manifest,
            hash,
            key,
        }
    }

    fn plan(&self, release: &Release) -> Plan {
        Plan {
            product_id: ID.into(),
            name: "Depot test".into(),
            version: release.manifest.version.clone(),
            channel: Channel::Stable,
            package_dir_name: ID.into(),
            install_dir: self.community.clone(),
            source: Source::Depot(DepotSource {
                manifest_url: self.server.url(&release.key),
                manifest_hash: release.hash,
                packs: self.urls.clone(),
            }),
        }
    }

    async fn run(&self, plan: Plan, kind: JobKind) -> Job {
        let mut rx = self.installer.subscribe();
        match kind {
            JobKind::Install => self.installer.start(plan).unwrap(),
            JobKind::Verify => self.installer.verify(plan).unwrap(),
        };
        until(&mut rx, |j| !j.is_active()).await
    }
}

fn noise(len: usize, seed: u64) -> Vec<u8> {
    let mut x = seed | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect()
}

fn write(root: &Path, rel: &str, bytes: &[u8]) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// A package: one big file, a few small ones, an empty file and folder.
fn source_v1(root: &Path) {
    write(root, "layout.json", b"{\"content\":[]}");
    write(root, "manifest.json", b"{\"package_version\":\"1.0.0\"}");
    write(
        root,
        "SimObjects/Airplanes/test/model/model.bin",
        &noise(600_000, 1),
    );
    write(
        root,
        "SimObjects/Airplanes/test/texture/a.dds",
        &noise(90_000, 2),
    );
    write(
        root,
        "SimObjects/Airplanes/test/aircraft.cfg",
        b"[GENERAL]\natc_type=TEST\n",
    );
    write(root, "empty.txt", b"");
    std::fs::create_dir_all(root.join("html_ui/empty")).unwrap();
}

/// Every file of `src` is in `package` with the same bytes.
fn assert_same(src: &Path, package: &Path) {
    let mut pending = vec![PathBuf::new()];
    while let Some(rel) = pending.pop() {
        for e in std::fs::read_dir(src.join(&rel)).unwrap() {
            let e = e.unwrap();
            let rel = rel.join(e.file_name());
            if e.file_type().unwrap().is_dir() {
                assert!(package.join(&rel).is_dir(), "{} missing", rel.display());
                pending.push(rel);
            } else {
                assert_eq!(
                    std::fs::read(package.join(&rel)).ok(),
                    Some(std::fs::read(src.join(&rel)).unwrap()),
                    "{} differs",
                    rel.display()
                );
            }
        }
    }
}

async fn until(rx: &mut broadcast::Receiver<Job>, done: impl Fn(&Job) -> bool) -> Job {
    tokio::time::timeout(Duration::from_secs(30), async {
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

fn failed(job: &Job) -> IpcErrorCode {
    match &job.phase {
        JobPhase::Failed { error } => error.code,
        other => panic!("expected a failure, got {other:?}"),
    }
}

#[tokio::test]
async fn installs_a_depot_release_and_records_it() {
    let mut fx = Fixture::new(Fault::None).await;
    source_v1(&fx.src());
    let v1 = fx.publish("1.0.0");
    assert!(v1.manifest.packs.len() > 1);

    let job = fx.run(fx.plan(&v1), JobKind::Install).await;
    assert_eq!(job.phase, JobPhase::Completed, "{job:?}");
    assert_same(&fx.src(), &fx.package());
    assert!(fx.package().join("infinity.json").is_file());
    assert!(fx.package().join("infinity.depot").is_file());
    assert_eq!(
        crate::package_version(&fx.package()).as_deref(),
        Some("1.0.0")
    );
    let record = fx.registry.get(ID).await.unwrap().unwrap();
    assert_eq!(record.version.as_deref(), Some("1.0.0"));
    // The job folder is gone.
    let staging = fx.tmp.path().join(".infinity-staging");
    assert!(!staging.join(format!("{ID}.depot")).exists());
}

#[tokio::test]
async fn an_update_downloads_only_what_changed() {
    let mut fx = Fixture::new(Fault::None).await;
    source_v1(&fx.src());
    let v1 = fx.publish("1.0.0");
    assert_eq!(
        fx.run(fx.plan(&v1), JobKind::Install).await.phase,
        JobPhase::Completed
    );
    let full = fx.server.pack_bytes_sent();

    // Patch the middle of the big file, add one, drop one.
    let big = fx.src().join("SimObjects/Airplanes/test/model/model.bin");
    let mut bytes = std::fs::read(&big).unwrap();
    bytes[300_000..300_016].copy_from_slice(b"patched-patched!");
    std::fs::write(&big, &bytes).unwrap();
    write(&fx.src(), "SimObjects/Airplanes/test/new.cfg", b"new");
    std::fs::remove_dir_all(fx.src().join("SimObjects/Airplanes/test/texture")).unwrap();
    let v2 = fx.publish("1.0.1");

    fx.server.clear_log();
    let job = fx.run(fx.plan(&v2), JobKind::Install).await;
    assert_eq!(job.phase, JobPhase::Completed, "{job:?}");
    assert_same(&fx.src(), &fx.package());
    assert!(
        !fx.package()
            .join("SimObjects/Airplanes/test/texture/a.dds")
            .exists()
    );
    assert!(
        !fx.package()
            .join("SimObjects/Airplanes/test/texture")
            .exists(),
        "emptied folder pruned"
    );
    let delta = fx.server.pack_bytes_sent();
    assert!(delta * 10 < full, "update fetched {delta} of {full} bytes");
    assert_eq!(
        crate::package_version(&fx.package()).as_deref(),
        Some("1.0.1")
    );
}

#[tokio::test]
async fn a_legacy_install_updates_by_delta() {
    // A package installed some other way (an archive, by hand) has no
    // depot state: the first depot update checks every file and still
    // downloads only what differs.
    let mut fx = Fixture::new(Fault::None).await;
    source_v1(&fx.src());
    let v1 = fx.publish("1.0.0");
    copy_dir(&fx.src(), &fx.package());
    write(
        &fx.package(),
        "SimObjects/Airplanes/test/aircraft.cfg",
        b"old contents",
    );
    let job = fx.run(fx.plan(&v1), JobKind::Install).await;
    assert_eq!(job.phase, JobPhase::Completed, "{job:?}");
    assert_same(&fx.src(), &fx.package());
    assert!(
        fx.server.pack_bytes_sent() < 20_000,
        "{}",
        fx.server.pack_bytes_sent()
    );
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &to.join(e.file_name()));
        } else {
            std::fs::copy(e.path(), to.join(e.file_name())).unwrap();
        }
    }
}

#[tokio::test]
async fn verify_repairs_only_the_damaged_chunks() {
    let mut fx = Fixture::new(Fault::None).await;
    source_v1(&fx.src());
    let v1 = fx.publish("1.0.0");
    assert_eq!(
        fx.run(fx.plan(&v1), JobKind::Install).await.phase,
        JobPhase::Completed
    );
    let full = fx.server.pack_bytes_sent();

    // Bit rot in the middle of the big file, same size and time.
    let big = fx
        .package()
        .join("SimObjects/Airplanes/test/model/model.bin");
    let f = std::fs::OpenOptions::new().write(true).open(&big).unwrap();
    let mtime = f.metadata().unwrap().modified().unwrap();
    crate::depot::io::write_at(&f, b"rot", 400_000).unwrap();
    f.set_modified(mtime).unwrap();
    drop(f);
    std::fs::remove_file(fx.package().join("layout.json")).unwrap();

    // An update to the same version trusts the recorded state ...
    fx.server.clear_log();
    let job = fx.run(fx.plan(&v1), JobKind::Install).await;
    assert_eq!(job.phase, JobPhase::Completed);
    assert!(
        fx.package().join("layout.json").is_file(),
        "missing files are always noticed"
    );
    assert_ne!(
        std::fs::read(&big).unwrap(),
        std::fs::read(fx.src().join("SimObjects/Airplanes/test/model/model.bin")).unwrap()
    );

    // ... a verify reads everything and fixes the rot.
    fx.server.clear_log();
    let job = fx.run(fx.plan(&v1), JobKind::Verify).await;
    assert_eq!(job.phase, JobPhase::Completed, "{job:?}");
    assert_eq!(job.kind, JobKind::Verify);
    assert_same(&fx.src(), &fx.package());
    let repaired = fx.server.pack_bytes_sent();
    assert!(
        repaired > 0 && repaired * 10 < full,
        "repair fetched {repaired} of {full}"
    );

    // A clean package verifies without downloading anything.
    fx.server.clear_log();
    assert_eq!(
        fx.run(fx.plan(&v1), JobKind::Verify).await.phase,
        JobPhase::Completed
    );
    assert_eq!(fx.server.pack_bytes_sent(), 0);
}

#[tokio::test]
async fn verifying_what_is_not_installed_fails_clearly() {
    let mut fx = Fixture::new(Fault::None).await;
    source_v1(&fx.src());
    let v1 = fx.publish("1.0.0");
    let job = fx.run(fx.plan(&v1), JobKind::Verify).await;
    assert_eq!(failed(&job), IpcErrorCode::PackageDirNotFound);
}

#[tokio::test]
async fn a_chunk_damaged_in_transit_is_fetched_again() {
    let mut fx = Fixture::new(Fault::CorruptOnce).await;
    source_v1(&fx.src());
    let v1 = fx.publish("1.0.0");
    let job = fx.run(fx.plan(&v1), JobKind::Install).await;
    assert_eq!(job.phase, JobPhase::Completed, "{job:?}");
    assert_same(&fx.src(), &fx.package());
}

#[tokio::test]
async fn a_chunk_damaged_at_the_source_fails_without_installing() {
    let mut fx = Fixture::new(Fault::CorruptAlways).await;
    source_v1(&fx.src());
    let v1 = fx.publish("1.0.0");
    let job = fx.run(fx.plan(&v1), JobKind::Install).await;
    assert_eq!(failed(&job), IpcErrorCode::CdnSignatureInvalid);
    assert!(!fx.package().exists());
}

#[tokio::test]
async fn a_dropped_connection_continues_where_it_stopped() {
    let mut fx = Fixture::new(Fault::DropOnce(70_000)).await;
    source_v1(&fx.src());
    let v1 = fx.publish("1.0.0");
    let job = fx.run(fx.plan(&v1), JobKind::Install).await;
    assert_eq!(job.phase, JobPhase::Completed, "{job:?}");
    assert_same(&fx.src(), &fx.package());
    let packed: u64 = v1.manifest.packs.iter().map(|p| p.size).sum();
    assert!((fx.server.pack_bytes_sent() as u64) < packed + 70_000);
}

#[tokio::test]
async fn a_refused_url_is_refreshed() {
    let mut fx = Fixture::new(Fault::ForbidOnce).await;
    source_v1(&fx.src());
    let v1 = fx.publish("1.0.0");
    let job = fx.run(fx.plan(&v1), JobKind::Install).await;
    assert_eq!(job.phase, JobPhase::Completed, "{job:?}");
    assert!(fx.urls.calls.load(Ordering::SeqCst) >= 2);
}

#[tokio::test]
async fn a_manifest_that_is_not_the_published_one_is_refused() {
    let mut fx = Fixture::new(Fault::None).await;
    source_v1(&fx.src());
    let mut v1 = fx.publish("1.0.0");
    v1.hash = Hash::of(b"something else");
    let job = fx.run(fx.plan(&v1), JobKind::Install).await;
    assert_eq!(failed(&job), IpcErrorCode::ManifestInvalid);
    assert!(!fx.package().exists());
}

#[tokio::test]
async fn pausing_keeps_the_chunks_and_survives_a_restart() {
    let mut fx = Fixture::new(Fault::Stall(40_000)).await;
    source_v1(&fx.src());
    let v1 = fx.publish("1.0.0");
    let mut rx = fx.installer.subscribe();
    fx.installer.start(fx.plan(&v1)).unwrap();
    until(&mut rx, |j| {
        j.phase == JobPhase::Downloading && j.progress.is_some_and(|p| p.done > 0)
    })
    .await;
    fx.installer.pause(ID).unwrap();
    assert_eq!(
        until(&mut rx, |j| !j.is_active()).await.phase,
        JobPhase::Paused
    );
    fx.server.release.notify_waiters();

    // A new app instance finds it, paused, with the bytes it had.
    let installer = Installer::new(fx.registry.clone()).unwrap();
    let found = installer
        .find_interrupted(vec![fx.community.clone()], crate::KEEP_INTERRUPTED_FOR)
        .await;
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].product_id(), ID);
    assert_eq!(found[0].job().phase, JobPhase::Paused);
    assert!(found[0].job().progress.is_some_and(|p| p.done > 0));
    assert!(installer.restore(found.into_iter().next().unwrap()));
    assert_eq!(installer.jobs().len(), 1);
}

#[tokio::test]
async fn cancelling_deletes_the_job_folder() {
    let mut fx = Fixture::new(Fault::Stall(40_000)).await;
    source_v1(&fx.src());
    let v1 = fx.publish("1.0.0");
    let mut rx = fx.installer.subscribe();
    fx.installer.start(fx.plan(&v1)).unwrap();
    until(&mut rx, |j| {
        j.phase == JobPhase::Downloading && j.progress.is_some_and(|p| p.done > 0)
    })
    .await;
    fx.installer.cancel(ID).await.unwrap();
    assert_eq!(
        until(&mut rx, |j| !j.is_active()).await.phase,
        JobPhase::Cancelled
    );
    fx.server.release.notify_waiters();
    assert!(
        !fx.tmp
            .path()
            .join(".infinity-staging")
            .join(format!("{ID}.depot"))
            .exists()
    );
}

/// A crash between the two commit phases: the next job (or startup)
/// finishes or undoes it, never leaving a mix.
#[test]
fn interrupted_commits_are_finished_or_undone() {
    use crate::depot::commit::{CommitJournal, Phase};

    for phase in [Phase::Aside, Phase::Place] {
        let tmp = tempfile::tempdir().unwrap();
        let community = tmp.path().join("Community");
        let target = community.join("pkg");
        std::fs::create_dir_all(&target).unwrap();
        let staging = crate::staging::Staging::create(&community, "pkg").unwrap();
        let dir = Dir::new(staging.depot());
        std::fs::create_dir_all(dir.root()).unwrap();
        // `a` was set aside, its replacement staged; `b` untouched.
        write(&dir.aside(), "sub/a", b"old");
        write(&dir.tree(), "sub/a", b"new");
        write(&target, "b", b"keep");
        let journal = CommitJournal {
            target: target.clone(),
            phase,
            replace: vec!["sub/a".into()],
            remove: vec![],
            version: "2.0.0".into(),
            package_dir_name: "pkg".into(),
            manifest_hash: Hash::of(b"m"),
        };
        std::fs::write(dir.commit(), serde_json::to_vec(&journal).unwrap()).unwrap();

        crate::depot::recover(&staging);
        let want: &[u8] = match phase {
            Phase::Aside => b"old",
            Phase::Place => b"new",
        };
        assert_eq!(std::fs::read(target.join("sub/a")).unwrap(), want);
        assert_eq!(std::fs::read(target.join("b")).unwrap(), b"keep");
        assert!(!dir.commit().exists());
        if phase == Phase::Place {
            assert_eq!(crate::package_version(&target).as_deref(), Some("2.0.0"));
        }
    }
}
