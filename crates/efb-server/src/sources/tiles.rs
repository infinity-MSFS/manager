use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::Mutex;

use crate::providers::{ProviderError, TileSource};

/// Tiles kept in memory. A map view is a few dozen tiles, and panning back
/// over them is the common case.
const CACHE_CAP: usize = 128;

/// Map tiles from free public servers: CARTO dark, Esri imagery and the
/// FAA sectional / IFR charts hosted on ArcGIS.
#[derive(Debug)]
pub struct PublicTiles {
    http: reqwest::Client,
    cache: Mutex<Lru>,
}

impl PublicTiles {
    /// `http` should carry a timeout and a User-Agent naming the app.
    pub fn new(http: reqwest::Client) -> Self {
        Self {
            http,
            cache: Mutex::new(Lru::default()),
        }
    }
}

fn upstream(layer: &str, z: u8, x: u32, y: u32) -> Option<String> {
    const FAA: &str = "https://tiles.arcgis.com/tiles/ssFJjBXIUyZDrSYZ/arcgis/rest/services";
    Some(match layer {
        // CARTO's dark basemap now watermarks keyless requests; Esri's
        // dark canvas does not (it stops at zoom 16).
        "dark" if z > 16 => return None,
        "dark" => format!(
            "https://server.arcgisonline.com/ArcGIS/rest/services/Canvas/World_Dark_Gray_Base/MapServer/tile/{z}/{y}/{x}"
        ),
        "sat" => format!(
            "https://server.arcgisonline.com/ArcGIS/rest/services/World_Imagery/MapServer/tile/{z}/{y}/{x}"
        ),
        "vfr" => format!("{FAA}/VFR_Sectional/MapServer/tile/{z}/{y}/{x}"),
        "ifr.lo" => format!("{FAA}/IFR_AreaLow/MapServer/tile/{z}/{y}/{x}"),
        "ifr.hi" => format!("{FAA}/IFR_High/MapServer/tile/{z}/{y}/{x}"),
        _ => return None,
    })
}

#[async_trait]
impl TileSource for PublicTiles {
    async fn tile(&self, layer: &str, z: u8, x: u32, y: u32) -> Result<Vec<u8>, ProviderError> {
        let Some(url) = upstream(layer, z, x, y) else {
            return Err(ProviderError::new(
                "not_found",
                format!("unknown layer {layer}"),
            ));
        };
        let key = format!("{layer}/{z}/{x}/{y}");
        if let Some(bytes) = self.cache.lock().await.get(&key) {
            return Ok(bytes.to_vec());
        }
        let network = |e: reqwest::Error| ProviderError::new("network", e.to_string());
        let resp = self.http.get(&url).send().await.map_err(network)?;
        if !resp.status().is_success() {
            return Err(ProviderError::new(
                "not_found",
                format!("HTTP {}", resp.status().as_u16()),
            ));
        }
        let bytes: Arc<[u8]> = resp.bytes().await.map_err(network)?.to_vec().into();
        self.cache.lock().await.put(key, bytes.clone());
        Ok(bytes.to_vec())
    }
}

#[derive(Debug, Default)]
struct Lru {
    entries: HashMap<String, (u64, Arc<[u8]>)>,
    tick: u64,
}

impl Lru {
    fn get(&mut self, key: &str) -> Option<Arc<[u8]>> {
        self.tick += 1;
        let tick = self.tick;
        self.entries.get_mut(key).map(|(used, bytes)| {
            *used = tick;
            bytes.clone()
        })
    }

    fn put(&mut self, key: String, bytes: Arc<[u8]>) {
        self.tick += 1;
        self.entries.insert(key, (self.tick, bytes));
        if self.entries.len() > CACHE_CAP
            && let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, (used, _))| *used)
                .map(|(k, _)| k.clone())
        {
            self.entries.remove(&oldest);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::TILE_LAYERS;

    #[test]
    fn every_layer_has_an_upstream() {
        for layer in TILE_LAYERS {
            assert!(upstream(layer, 3, 1, 2).is_some(), "{layer}");
        }
        assert!(upstream("nope", 3, 1, 2).is_none());
        // ArcGIS is row-major: z/y/x.
        assert!(upstream("sat", 3, 1, 2).is_some_and(|u| u.ends_with("/3/2/1")));
    }

    #[test]
    fn the_least_recently_used_tile_goes_first() {
        let mut lru = Lru::default();
        let bytes: Arc<[u8]> = Arc::from(&b"x"[..]);
        for i in 0..CACHE_CAP {
            lru.put(i.to_string(), bytes.clone());
        }
        assert!(lru.get("0").is_some());
        lru.put("new".into(), bytes);
        assert_eq!(lru.entries.len(), CACHE_CAP);
        assert!(lru.get("0").is_some(), "recently read");
        assert!(lru.get("1").is_none(), "evicted");
    }
}
