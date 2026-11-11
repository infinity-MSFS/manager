//! Airport charts: one model for every chart source, and the sim-session
//! lease that gates all of them.
//!
//! A [`ChartProvider`] is one source (Navigraph, a freeware service): it
//! lists an airport's charts as [`ChartEntry`]s and fetches their images.
//! Providers know nothing about the EFB, the lease or caching; the
//! [`hub`](self::hub) owns those, so every source gets the same rules and a
//! new one cannot forget them.
//!
//! **Charts exist only inside a running aircraft.** Navigraph licenses charts
//! to an addon for use *in the simulator*; the manager must never turn into
//! a free-standing chart viewer. Every chart request, socket or HTTP, goes
//! through [`lease::Lease::access`] first, and the lease is only live while
//! the simulator runs, the aircraft's WASM module is connected over its
//! bridge AND the in-sim EFB panel has checked in recently. A tablet mirror
//! rides on that session; it cannot open one.

pub(crate) mod hub;
pub mod lease;

use async_trait::async_trait;
use serde::Serialize;

use crate::providers::ProviderError;

pub use lease::{Access, LockReason, SimPresence};

/// Error codes the hub itself raises. Provider codes pass through verbatim.
pub mod codes {
    /// The lease is not live; `message` names the reason.
    pub const LOCKED: &str = "charts_locked";
    pub const UNKNOWN_SOURCE: &str = "unknown_source";
    pub const BAD_ICAO: &str = "bad_icao";
    /// An image key that is in no index the source returned for the airport.
    pub const UNKNOWN_IMAGE: &str = "unknown_image";
    /// Transient: the hub retries these once.
    pub const NETWORK: &str = "network";
    pub const RATE_LIMITED: &str = "rate_limited";
}

/// Who a source is, as the EFB shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChartSource {
    /// Stable, URL-safe (`navigraph`). Part of the image route.
    pub id: String,
    pub name: String,
    /// The credit line drawn under every chart from this source.
    pub attribution: String,
}

/// Whether a source can answer right now. Independent of the lease: a
/// source can be ready while the lease is locked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SourceState {
    Ready,
    /// No account linked in the manager.
    NotLinked,
    /// The account link died and must be made again.
    RelinkRequired,
    /// Linked, but the account may not use charts.
    NoSubscription,
    Unavailable {
        message: String,
    },
}

/// Procedure-chart grouping, the order the EFB tabs them in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum ChartCategory {
    #[serde(rename = "APT")]
    Airport,
    #[serde(rename = "DEP")]
    Departure,
    #[serde(rename = "ARR")]
    Arrival,
    #[serde(rename = "APP")]
    Approach,
    #[serde(rename = "REF")]
    Reference,
}

impl ChartCategory {
    /// Lenient: an unknown code files under Reference rather than dropping
    /// the chart.
    pub fn from_code(code: &str) -> Self {
        match code.trim().to_ascii_uppercase().as_str() {
            "APT" | "AIRPORT" => Self::Airport,
            "DEP" | "SID" | "DEPARTURE" => Self::Departure,
            "ARR" | "STAR" | "ARRIVAL" => Self::Arrival,
            "APP" | "IAP" | "APPROACH" => Self::Approach,
            _ => Self::Reference,
        }
    }
}

/// A rectangle in chart-image pixels.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct PixelRect {
    pub x1: f64,
    pub y1: f64,
    pub x2: f64,
    pub y2: f64,
}

/// Geographic corners matching a [`PixelRect`] pairwise: `(lng1, lat1)` is
/// at `(x1, y1)` and `(lng2, lat2)` at `(x2, y2)`. Pairwise, not by corner
/// name, so either corner convention maps correctly.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct GeoRect {
    pub lat1: f64,
    pub lng1: f64,
    pub lat2: f64,
    pub lng2: f64,
}

/// Where the to-scale plan view sits on a chart. Own-ship is drawn only
/// inside `pixels` and never inside an inset, which is not to scale.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GeoRef {
    pub pixels: PixelRect,
    pub latlng: GeoRect,
    pub insets: Vec<PixelRect>,
}

impl GeoRef {
    /// `None` unless every number is finite and neither box is degenerate,
    /// so a client never divides by a zero span.
    pub fn checked(pixels: PixelRect, latlng: GeoRect, insets: Vec<PixelRect>) -> Option<Self> {
        let finite = [
            pixels.x1,
            pixels.y1,
            pixels.x2,
            pixels.y2,
            latlng.lat1,
            latlng.lng1,
            latlng.lat2,
            latlng.lng2,
        ]
        .iter()
        .all(|v| v.is_finite());
        if !finite {
            return None;
        }
        let spans = [
            pixels.x2 - pixels.x1,
            pixels.y2 - pixels.y1,
            latlng.lng2 - latlng.lng1,
            latlng.lat2 - latlng.lat1,
        ];
        if spans.iter().any(|s| s.abs() < f64::EPSILON) {
            return None;
        }
        let in_range = latlng.lat1.abs() <= 90.0
            && latlng.lat2.abs() <= 90.0
            && latlng.lng1.abs() <= 180.0
            && latlng.lng2.abs() <= 180.0;
        if !in_range {
            return None;
        }
        let insets = insets
            .into_iter()
            .filter(|r| [r.x1, r.y1, r.x2, r.y2].iter().all(|v| v.is_finite()))
            .collect();
        Some(Self {
            pixels,
            latlng,
            insets,
        })
    }
}

/// One chart, in the shape the EFB reads (`charts.indexResult`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ChartEntry {
    /// Unique within the airport's index.
    pub id: String,
    /// The source's sheet number (`10-9`), possibly empty.
    pub index: String,
    pub name: String,
    pub category: ChartCategory,
    pub runways: Vec<String>,
    pub procedures: Vec<String>,
    /// Image keys for `/efb/charts/{source}/{icao}/{key}`. Opaque to
    /// everything but the source, and always [`valid_key`].
    pub day: Option<String>,
    pub night: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub georef: Option<GeoRef>,
}

impl ChartEntry {
    pub(crate) fn has_image(&self, key: &str) -> bool {
        self.day.as_deref() == Some(key) || self.night.as_deref() == Some(key)
    }
}

/// One chart image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChartImage {
    pub bytes: Vec<u8>,
    /// `image/png`, `image/jpeg`. Coherent cannot decode WebP, so a source
    /// that serves it must convert.
    pub content_type: &'static str,
}

/// One chart source. Implementations do the network work only: the hub
/// validates arguments, enforces the lease, caches and retries.
#[async_trait]
pub trait ChartProvider: Send + Sync {
    fn source(&self) -> ChartSource;

    async fn status(&self) -> SourceState;

    /// The airport's charts; an airport the source has nothing for is an
    /// empty list, not an error. `icao` is already [`normalize_icao`]d.
    async fn index(&self, icao: &str) -> Result<Vec<ChartEntry>, ProviderError>;

    /// One image named by a key from [`Self::index`] for the same airport.
    async fn image(&self, icao: &str, key: &str) -> Result<ChartImage, ProviderError>;
}

/// Upper-cased 3 to 5 ASCII letters or digits, or `None`. Every ICAO goes
/// through this before it reaches a provider or a cache key, so a path
/// segment can never smuggle a `/` or `..` to a source's API.
pub fn normalize_icao(raw: &str) -> Option<String> {
    let s = raw.trim().to_ascii_uppercase();
    let ok = (3..=5).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric());
    ok.then_some(s)
}

/// Image keys are one URL path segment: ASCII letters, digits, `.`, `_`,
/// `-`, at most 160 bytes, not starting with a dot.
pub fn valid_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 160
        && !key.starts_with('.')
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icao_is_normalised_and_bounded() {
        assert_eq!(normalize_icao(" ksfo ").as_deref(), Some("KSFO"));
        assert_eq!(normalize_icao("EGLL").as_deref(), Some("EGLL"));
        assert_eq!(normalize_icao("K2B2").as_deref(), Some("K2B2"));
        for bad in ["", "KS", "KSFO/../x", "K SF", "ABCDEF", "KSF%2F"] {
            assert_eq!(normalize_icao(bad), None, "{bad}");
        }
    }

    #[test]
    fn keys_are_single_safe_segments() {
        assert!(valid_key("abc_d-1.png"));
        for bad in ["", ".png", "../x", "a/b", "a?b", "a b", &"x".repeat(161)] {
            assert!(!valid_key(bad), "{bad}");
        }
    }

    #[test]
    fn categories_are_lenient() {
        assert_eq!(ChartCategory::from_code("app"), ChartCategory::Approach);
        assert_eq!(ChartCategory::from_code("STAR"), ChartCategory::Arrival);
        assert_eq!(ChartCategory::from_code("??"), ChartCategory::Reference);
        assert_eq!(
            serde_json::to_value(ChartCategory::Departure).ok(),
            Some(serde_json::json!("DEP"))
        );
    }

    #[test]
    fn georef_rejects_degenerate_and_non_finite_boxes() {
        let px = PixelRect {
            x1: 0.0,
            y1: 100.0,
            x2: 100.0,
            y2: 0.0,
        };
        let geo = GeoRect {
            lat1: 37.0,
            lng1: -122.0,
            lat2: 38.0,
            lng2: -121.0,
        };
        assert!(GeoRef::checked(px, geo, vec![]).is_some());
        assert!(GeoRef::checked(PixelRect { x2: 0.0, ..px }, geo, vec![]).is_none());
        assert!(
            GeoRef::checked(
                px,
                GeoRect {
                    lat1: f64::NAN,
                    ..geo
                },
                vec![]
            )
            .is_none()
        );
        assert!(GeoRef::checked(px, GeoRect { lat2: 91.0, ..geo }, vec![]).is_none());
        let inset = PixelRect {
            x1: f64::INFINITY,
            ..px
        };
        assert_eq!(
            GeoRef::checked(px, geo, vec![inset, px]).map(|g| g.insets.len()),
            Some(1)
        );
    }
}
