//! The one door to every chart source: lease, validation, caching, retry.
//!
//! Caches are memory-only and are dropped the moment the lease locks
//! ([`ChartsHub::purge`]), so no chart outlives the session that fetched it
//! and nothing licensed is ever written to disk.

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::sync::Mutex;
use tracing::debug;

use super::lease::{Access, Lease, PANEL_LEASE};
use super::{ChartEntry, ChartImage, ChartProvider, SimPresence, codes, normalize_icao, valid_key};
use crate::providers::ProviderError;

/// An index is re-fetched after this; AIRAC changes every 28 days, but a
/// chart amended mid-cycle should still reach a long session.
const INDEX_TTL: Duration = Duration::from_secs(10 * 60);
const INDEX_CAP: usize = 64;
/// Navigraph sheets are 0.5 to 3 MB; this holds a busy flight's worth
/// (both ends, several approaches) without growing without bound.
const IMAGE_BUDGET_BYTES: usize = 96 * 1024 * 1024;
const IMAGE_MAX_BYTES: usize = 24 * 1024 * 1024;
/// A provider call that takes longer is abandoned (and retried once if the
/// failure is transient).
const PROVIDER_TIMEOUT: Duration = Duration::from_secs(25);
const RETRY_DELAY: Duration = Duration::from_millis(400);

type IndexKey = (String, String);
type ImageKey = (String, String, String);

#[derive(Debug)]
struct CachedIndex {
    at: Instant,
    charts: Arc<Vec<ChartEntry>>,
}

#[derive(Debug, Default)]
struct ImageCache {
    order: VecDeque<ImageKey>,
    entries: HashMap<ImageKey, Arc<ChartImage>>,
    bytes: usize,
}

impl ImageCache {
    fn get(&mut self, key: &ImageKey) -> Option<Arc<ChartImage>> {
        let hit = self.entries.get(key).cloned()?;
        // Most recently used goes to the back.
        if let Some(pos) = self.order.iter().position(|k| k == key)
            && let Some(k) = self.order.remove(pos)
        {
            self.order.push_back(k);
        }
        Some(hit)
    }

    fn insert(&mut self, key: ImageKey, image: Arc<ChartImage>) {
        let size = image.bytes.len();
        if size > IMAGE_MAX_BYTES {
            return;
        }
        if let Some(old) = self.entries.remove(&key) {
            self.bytes -= old.bytes.len();
            self.order.retain(|k| k != &key);
        }
        while self.bytes + size > IMAGE_BUDGET_BYTES {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            if let Some(old) = self.entries.remove(&oldest) {
                self.bytes -= old.bytes.len();
            }
        }
        self.bytes += size;
        self.order.push_back(key.clone());
        self.entries.insert(key, image);
    }

    fn clear(&mut self) {
        self.order.clear();
        self.entries.clear();
        self.bytes = 0;
    }
}

pub(crate) struct ChartsHub {
    sources: Vec<Arc<dyn ChartProvider>>,
    presence: Option<Arc<dyn SimPresence>>,
    lease: Lease,
    indexes: Mutex<HashMap<IndexKey, CachedIndex>>,
    images: Mutex<ImageCache>,
    /// One fetch per image at a time: the panel and a tablet opening the
    /// same chart share one download.
    inflight: Mutex<HashMap<ImageKey, Arc<Mutex<()>>>>,
    retry_delay: Duration,
}

impl std::fmt::Debug for ChartsHub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChartsHub")
            .field("sources", &self.sources.len())
            .field("presence", &self.presence.is_some())
            .finish_non_exhaustive()
    }
}

fn locked(access: Access) -> ProviderError {
    let reason = access.reason.map_or("locked", |r| r.message());
    ProviderError::new(codes::LOCKED, reason)
}

fn transient(e: &ProviderError) -> bool {
    e.code == codes::NETWORK
}

impl ChartsHub {
    pub(crate) fn new(
        sources: Vec<Arc<dyn ChartProvider>>,
        presence: Option<Arc<dyn SimPresence>>,
    ) -> Self {
        Self {
            sources,
            presence,
            lease: Lease::new(PANEL_LEASE),
            indexes: Mutex::new(HashMap::new()),
            images: Mutex::new(ImageCache::default()),
            inflight: Mutex::new(HashMap::new()),
            retry_delay: RETRY_DELAY,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_retry_delay(mut self, delay: Duration) -> Self {
        self.retry_delay = delay;
        self
    }

    pub(crate) fn has_sources(&self) -> bool {
        !self.sources.is_empty()
    }

    pub(crate) fn panel_alive(&self) {
        self.lease.panel_alive();
    }

    pub(crate) fn access(&self) -> Access {
        self.lease.access(self.presence.as_deref())
    }

    fn gate(&self) -> Result<(), ProviderError> {
        let access = self.access();
        if access.live {
            Ok(())
        } else {
            Err(locked(access))
        }
    }

    fn source(&self, id: &str) -> Result<&Arc<dyn ChartProvider>, ProviderError> {
        self.sources
            .iter()
            .find(|s| s.source().id == id)
            .ok_or_else(|| ProviderError::new(codes::UNKNOWN_SOURCE, format!("no source {id}")))
    }

    /// `{ access, sources: [{ id, name, attribution, state, message? }] }`,
    /// sent as `charts.info`. Answered while locked too: the EFB needs the
    /// reason, and a source's link state is not licensed content.
    pub(crate) async fn info(&self) -> Value {
        let mut sources = Vec::with_capacity(self.sources.len());
        for s in &self.sources {
            let meta = s.source();
            let state = match tokio::time::timeout(PROVIDER_TIMEOUT, s.status()).await {
                Ok(state) => serde_json::to_value(state).unwrap_or(Value::Null),
                Err(_) => json!({ "state": "unavailable", "message": "timed out" }),
            };
            let mut entry = json!({
                "id": meta.id, "name": meta.name, "attribution": meta.attribution,
            });
            if let (Some(obj), Some(st)) = (entry.as_object_mut(), state.as_object()) {
                obj.extend(st.clone());
            }
            sources.push(entry);
        }
        json!({ "access": self.access(), "sources": sources })
    }

    /// The airport's charts from one source.
    pub(crate) async fn index(
        &self,
        source: &str,
        icao: &str,
    ) -> Result<Arc<Vec<ChartEntry>>, ProviderError> {
        self.gate()?;
        let icao = normalize_icao(icao)
            .ok_or_else(|| ProviderError::new(codes::BAD_ICAO, "not an airport identifier"))?;
        let provider = self.source(source)?.clone();
        let key = (source.to_owned(), icao.clone());
        if let Some(hit) = self.indexes.lock().await.get(&key)
            && hit.at.elapsed() < INDEX_TTL
        {
            return Ok(hit.charts.clone());
        }
        self.fetch_index(&provider, key).await
    }

    async fn fetch_index(
        &self,
        provider: &Arc<dyn ChartProvider>,
        key: IndexKey,
    ) -> Result<Arc<Vec<ChartEntry>>, ProviderError> {
        let icao = key.1.clone();
        let mut charts = self.retrying(|| provider.index(&icao)).await?;
        // A source's keys become URL segments; one that is not a plain
        // segment is dropped rather than trusted.
        for c in &mut charts {
            c.day = c.day.take().filter(|k| valid_key(k));
            c.night = c.night.take().filter(|k| valid_key(k));
        }
        // The lease may have locked while the source was answering.
        self.gate()?;
        let charts = Arc::new(charts);
        let mut indexes = self.indexes.lock().await;
        if indexes.len() >= INDEX_CAP && !indexes.contains_key(&key) {
            let oldest = indexes
                .iter()
                .min_by_key(|(_, v)| v.at)
                .map(|(k, _)| k.clone());
            if let Some(k) = oldest {
                indexes.remove(&k);
            }
        }
        indexes.insert(
            key,
            CachedIndex {
                at: Instant::now(),
                charts: charts.clone(),
            },
        );
        Ok(charts)
    }

    /// One chart image. `key` must be in the airport's index from that
    /// source: the route can name only charts the source itself listed.
    pub(crate) async fn image(
        &self,
        source: &str,
        icao: &str,
        key: &str,
    ) -> Result<Arc<ChartImage>, ProviderError> {
        self.gate()?;
        let icao = normalize_icao(icao)
            .ok_or_else(|| ProviderError::new(codes::BAD_ICAO, "not an airport identifier"))?;
        if !valid_key(key) {
            return Err(ProviderError::new(codes::UNKNOWN_IMAGE, "bad image key"));
        }
        let provider = self.source(source)?.clone();
        let image_key = (source.to_owned(), icao.clone(), key.to_owned());

        let slot = {
            let mut inflight = self.inflight.lock().await;
            inflight.entry(image_key.clone()).or_default().clone()
        };
        let result = {
            let _turn = slot.lock().await;
            self.image_locked(&provider, &image_key).await
        };
        {
            let mut inflight = self.inflight.lock().await;
            // Only the last waiter's clone is left besides the map's own.
            if Arc::strong_count(&slot) <= 2 {
                inflight.remove(&image_key);
            }
        }
        result
    }

    async fn image_locked(
        &self,
        provider: &Arc<dyn ChartProvider>,
        image_key: &ImageKey,
    ) -> Result<Arc<ChartImage>, ProviderError> {
        if let Some(hit) = self.images.lock().await.get(image_key) {
            return Ok(hit);
        }
        let (source, icao, key) = image_key;
        self.ensure_listed(provider, source, icao, key).await?;
        let image = self.retrying(|| provider.image(icao, key)).await?;
        self.gate()?;
        let image = Arc::new(image);
        self.images
            .lock()
            .await
            .insert(image_key.clone(), image.clone());
        Ok(image)
    }

    async fn ensure_listed(
        &self,
        provider: &Arc<dyn ChartProvider>,
        source: &str,
        icao: &str,
        key: &str,
    ) -> Result<(), ProviderError> {
        let index_key = (source.to_owned(), icao.to_owned());
        let cached = self
            .indexes
            .lock()
            .await
            .get(&index_key)
            .map(|c| c.charts.clone());
        if cached.is_some_and(|c| c.iter().any(|e| e.has_image(key))) {
            return Ok(());
        }
        // Not in what we hold (expired, purged, or amended since): ask once.
        let fresh = self.fetch_index(provider, index_key).await?;
        if fresh.iter().any(|e| e.has_image(key)) {
            Ok(())
        } else {
            debug!(source, icao, key, "charts: image not in the index");
            Err(ProviderError::new(codes::UNKNOWN_IMAGE, "chart not found"))
        }
    }

    async fn retrying<T, F, Fut>(&self, mut call: F) -> Result<T, ProviderError>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<T, ProviderError>>,
    {
        let once = |fut: Fut| async move {
            match tokio::time::timeout(PROVIDER_TIMEOUT, fut).await {
                Ok(r) => r,
                Err(_) => Err(ProviderError::new(
                    codes::NETWORK,
                    "the chart source timed out",
                )),
            }
        };
        match once(call()).await {
            Err(e) if transient(&e) => {
                debug!(code = e.code, "charts: transient failure, retrying once");
                tokio::time::sleep(self.retry_delay).await;
                self.gate()?;
                once(call()).await
            }
            other => other,
        }
    }

    /// Drops every cached index and image. Called when the lease locks.
    pub(crate) async fn purge(&self) {
        self.indexes.lock().await.clear();
        self.images.lock().await.clear();
    }

    /// The pre-hub `nav.chartsIndex` entry shape, for EFBs (the T-38's)
    /// that still read it.
    pub(crate) fn legacy_entry(c: &ChartEntry) -> Value {
        json!({
            "id": c.id,
            "index_number": c.index,
            "name": c.name,
            "category": c.category,
            "day_file": c.day,
            "night_file": c.night,
            "width": c.width,
            "height": c.height,
            "is_georeferenced": c.georef.is_some(),
        })
    }

    #[cfg(test)]
    pub(crate) async fn cached_images(&self) -> usize {
        self.images.lock().await.entries.len()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use async_trait::async_trait;

    use super::*;
    use crate::charts::{ChartCategory, ChartSource, LockReason, SourceState};

    #[derive(Default)]
    pub(crate) struct Presence {
        pub(crate) sim: AtomicBool,
        pub(crate) aircraft: AtomicBool,
    }

    impl SimPresence for Presence {
        fn sim_running(&self) -> bool {
            self.sim.load(Ordering::SeqCst)
        }
        fn aircraft_connected(&self) -> bool {
            self.aircraft.load(Ordering::SeqCst)
        }
    }

    #[derive(Default)]
    struct Source {
        index_calls: AtomicUsize,
        image_calls: AtomicUsize,
        fail_next: AtomicUsize,
    }

    fn entry(id: &str, day: &str) -> ChartEntry {
        ChartEntry {
            id: id.into(),
            index: "10-9".into(),
            name: id.into(),
            category: ChartCategory::Airport,
            runways: vec![],
            procedures: vec![],
            day: Some(day.into()),
            night: None,
            width: None,
            height: None,
            georef: None,
        }
    }

    #[async_trait]
    impl ChartProvider for Source {
        fn source(&self) -> ChartSource {
            ChartSource {
                id: "test".into(),
                name: "Test".into(),
                attribution: "(c) test".into(),
            }
        }
        async fn status(&self) -> SourceState {
            SourceState::Ready
        }
        async fn index(&self, icao: &str) -> Result<Vec<ChartEntry>, ProviderError> {
            self.index_calls.fetch_add(1, Ordering::SeqCst);
            if self.fail_next.load(Ordering::SeqCst) > 0 {
                self.fail_next.fetch_sub(1, Ordering::SeqCst);
                return Err(ProviderError::new(codes::NETWORK, "down"));
            }
            Ok(vec![
                entry(&format!("{icao}-apt"), "apt.png"),
                entry("evil", "../../etc/passwd"),
            ])
        }
        async fn image(&self, _icao: &str, key: &str) -> Result<ChartImage, ProviderError> {
            self.image_calls.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(20)).await;
            Ok(ChartImage {
                bytes: key.as_bytes().to_vec(),
                content_type: "image/png",
            })
        }
    }

    fn hub() -> (Arc<ChartsHub>, Arc<Presence>, Arc<Source>) {
        let presence = Arc::new(Presence::default());
        let source = Arc::new(Source::default());
        let hub = ChartsHub::new(vec![source.clone()], Some(presence.clone()))
            .with_retry_delay(Duration::from_millis(1));
        (Arc::new(hub), presence, source)
    }

    fn go_live(hub: &ChartsHub, p: &Presence) {
        p.sim.store(true, Ordering::SeqCst);
        p.aircraft.store(true, Ordering::SeqCst);
        hub.panel_alive();
    }

    #[tokio::test]
    async fn nothing_is_served_without_the_lease() {
        let (hub, presence, source) = hub();
        let err = hub.index("test", "KSFO").await.unwrap_err();
        assert_eq!(err.code, codes::LOCKED);
        assert_eq!(err.message, LockReason::SimNotRunning.message());
        assert_eq!(
            hub.image("test", "KSFO", "apt.png").await.unwrap_err().code,
            codes::LOCKED
        );
        assert_eq!(source.index_calls.load(Ordering::SeqCst), 0);

        go_live(&hub, &presence);
        assert_eq!(hub.index("test", "ksfo").await.unwrap()[0].id, "KSFO-apt");
        // Aircraft unloads: locked again, even with the index cached.
        presence.aircraft.store(false, Ordering::SeqCst);
        assert_eq!(
            hub.index("test", "KSFO").await.unwrap_err().code,
            codes::LOCKED
        );
    }

    #[tokio::test]
    async fn unlisted_and_unsafe_keys_are_refused() {
        let (hub, presence, source) = hub();
        go_live(&hub, &presence);
        let charts = hub.index("test", "KSFO").await.unwrap();
        assert_eq!(charts[1].day, None, "a path-like key is stripped");
        assert_eq!(
            hub.image("test", "KSFO", "other.png")
                .await
                .unwrap_err()
                .code,
            codes::UNKNOWN_IMAGE
        );
        assert_eq!(
            hub.image("test", "KSFO", "../x").await.unwrap_err().code,
            codes::UNKNOWN_IMAGE
        );
        assert_eq!(
            hub.image("test", "KS/FO", "apt.png")
                .await
                .unwrap_err()
                .code,
            codes::BAD_ICAO
        );
        assert_eq!(
            hub.image("nope", "KSFO", "apt.png").await.unwrap_err().code,
            codes::UNKNOWN_SOURCE
        );
        assert_eq!(source.image_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn images_are_cached_shared_and_purged() {
        let (hub, presence, source) = hub();
        go_live(&hub, &presence);
        // No index fetched yet: the image fetches it to check the key.
        let (a, b) = tokio::join!(
            hub.image("test", "KSFO", "apt.png"),
            hub.image("test", "KSFO", "apt.png")
        );
        assert_eq!(a.unwrap().bytes, b"apt.png");
        assert_eq!(b.unwrap().bytes, b"apt.png");
        assert_eq!(source.image_calls.load(Ordering::SeqCst), 1, "one download");
        assert_eq!(source.index_calls.load(Ordering::SeqCst), 1);
        assert!(hub.inflight.lock().await.is_empty());

        hub.purge().await;
        assert_eq!(hub.cached_images().await, 0);
        hub.image("test", "KSFO", "apt.png").await.unwrap();
        assert_eq!(source.image_calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn transient_failures_retry_once() {
        let (hub, presence, source) = hub();
        go_live(&hub, &presence);
        source.fail_next.store(1, Ordering::SeqCst);
        assert!(hub.index("test", "KSFO").await.is_ok());
        assert_eq!(source.index_calls.load(Ordering::SeqCst), 2);

        source.fail_next.store(2, Ordering::SeqCst);
        assert_eq!(
            hub.index("test", "EGLL").await.unwrap_err().code,
            codes::NETWORK
        );
        assert_eq!(source.index_calls.load(Ordering::SeqCst), 4);
    }

    #[tokio::test]
    async fn info_reports_access_and_sources_while_locked() {
        let (hub, _, _) = hub();
        let info = hub.info().await;
        assert_eq!(info["access"]["live"], false);
        assert_eq!(info["access"]["reason"], "sim_not_running");
        assert_eq!(info["sources"][0]["id"], "test");
        assert_eq!(info["sources"][0]["state"], "ready");
        assert_eq!(info["sources"][0]["attribution"], "(c) test");
    }

    #[test]
    fn image_cache_respects_its_budget() {
        let mut cache = ImageCache::default();
        let img = |n: usize| {
            Arc::new(ChartImage {
                bytes: vec![0; n],
                content_type: "image/png",
            })
        };
        let k = |s: &str| ("s".to_string(), "KSFO".to_string(), s.to_string());
        let chunk = IMAGE_BUDGET_BYTES / 4;
        for name in ["a", "b", "c", "d"] {
            cache.insert(k(name), img(chunk));
        }
        assert!(cache.get(&k("a")).is_some(), "touch a: b is now oldest");
        cache.insert(k("e"), img(chunk));
        assert!(cache.get(&k("b")).is_none());
        assert!(cache.get(&k("a")).is_some());
        assert!(cache.bytes <= IMAGE_BUDGET_BYTES);
        cache.insert(k("huge"), img(IMAGE_MAX_BYTES + 1));
        assert!(cache.get(&k("huge")).is_none());
    }
}
