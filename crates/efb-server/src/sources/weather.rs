use std::collections::HashMap;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::{Map, Value};
use tokio::sync::Mutex;
use tracing::{debug, warn};

use crate::providers::{ProviderError, WxProvider};

const BASE_URL: &str = "https://aviationweather.gov/api/data/metar";
/// aviationweather.gov asks for at most one request a minute per caller. The
/// EFB only fetches on a button press, so a minute's cache per station set
/// keeps a few impatient taps inside that.
const CACHE_TTL: Duration = Duration::from_secs(60);

/// The fields the EFB renders; the rest is dropped to keep the `wxData` ui
/// key small.
const KEEP_FIELDS: [&str; 12] = [
    "icaoId",
    "name",
    "reportTime",
    "temp",
    "dewp",
    "wdir",
    "wspd",
    "visib",
    "altim",
    "fltCat",
    "rawOb",
    "rawTaf",
];

/// METAR/TAF from aviationweather.gov's open data API. No key needed.
#[derive(Debug)]
pub struct AviationWeather {
    http: reqwest::Client,
    base_url: String,
    cache: Mutex<HashMap<String, (Instant, Value)>>,
}

impl AviationWeather {
    /// `http` should carry a timeout and a User-Agent naming the app; the
    /// service asks for one.
    pub fn new(http: reqwest::Client) -> Self {
        Self::with_base_url(http, BASE_URL)
    }

    pub fn with_base_url(http: reqwest::Client, base_url: impl Into<String>) -> Self {
        Self {
            http,
            base_url: base_url.into(),
            cache: Mutex::new(HashMap::new()),
        }
    }
}

fn slim(entry: &Value) -> Value {
    let mut out = Map::new();
    if let Some(obj) = entry.as_object() {
        for key in KEEP_FIELDS {
            if let Some(v) = obj.get(key).filter(|v| !v.is_null()) {
                out.insert(key.to_owned(), v.clone());
            }
        }
    }
    Value::Object(out)
}

fn cache_key(ids: &[String]) -> String {
    let mut sorted = ids.to_vec();
    sorted.sort();
    sorted.join(",")
}

#[async_trait]
impl WxProvider for AviationWeather {
    async fn fetch(&self, ids: Vec<String>) -> Result<Value, ProviderError> {
        let key = cache_key(&ids);
        {
            let mut cache = self.cache.lock().await;
            cache.retain(|_, (at, _)| at.elapsed() < CACHE_TTL);
            if let Some((_, v)) = cache.get(&key) {
                debug!(ids = %key, "efb-wx: cache hit");
                return Ok(v.clone());
            }
        }

        let resp = self
            .http
            .get(&self.base_url)
            .query(&[
                ("ids", ids.join(",").as_str()),
                ("format", "json"),
                ("taf", "true"),
            ])
            .send()
            .await
            .map_err(|e| {
                ProviderError::new("network", format!("Weather service unreachable: {e}"))
            })?;
        let status = resp.status();
        if !status.is_success() {
            warn!(%status, "efb-wx: weather service error");
            return Err(ProviderError::new(
                "bad_response",
                format!("Weather service error (HTTP {status})"),
            ));
        }
        let body: Value = resp.json().await.map_err(|e| {
            ProviderError::new("bad_response", format!("Unparseable weather data: {e}"))
        })?;
        let stations = Value::Array(
            body.as_array()
                .map(|a| a.iter().map(slim).collect())
                .unwrap_or_default(),
        );
        self.cache
            .lock()
            .await
            .insert(key, (Instant::now(), stations.clone()));
        Ok(stations)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn slim_keeps_only_known_fields_and_drops_nulls() {
        let entry = json!({
            "icaoId": "KEDW", "rawOb": "METAR KEDW ...", "fltCat": "VFR",
            "temp": 24, "dewp": null, "qcField": 4, "clouds": [], "lat": 34.9,
        });
        let s = slim(&entry);
        let o = s.as_object().unwrap();
        assert_eq!(o["icaoId"], "KEDW");
        assert_eq!(o["fltCat"], "VFR");
        assert!(o.contains_key("temp"));
        assert!(!o.contains_key("dewp"), "nulls dropped");
        assert!(!o.contains_key("qcField"), "unknown fields dropped");
        assert!(!o.contains_key("clouds"));
    }

    #[test]
    fn station_order_does_not_split_the_cache() {
        let a = cache_key(&["KSFO".into(), "KEDW".into()]);
        assert_eq!(a, cache_key(&["KEDW".into(), "KSFO".into()]));
    }

    #[tokio::test]
    async fn unreachable_service_is_a_network_error() {
        // Nothing listens on port 9 of the discard address.
        let wx = AviationWeather::with_base_url(reqwest::Client::new(), "http://127.0.0.1:9/");
        let err = wx.fetch(vec!["KEDW".into()]).await.unwrap_err();
        assert_eq!(err.code, "network");
    }
}
