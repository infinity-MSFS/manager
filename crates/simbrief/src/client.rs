//! Client for the public SimBrief fetcher (`/api/xml.fetcher.php`).

use std::time::Duration;

use url::Url;

use crate::error::SimBriefError;
use crate::payload::{SIMBRIEF_WIRE_VERSION, SimBriefPayload, SimBriefWaypoint};
use crate::response::{SimBriefOfp, parse_f64, parse_i32, parse_navlog_fixes};

pub const DEFAULT_BASE_URL: &str = "https://www.simbrief.com";
const FETCHER_PATH: &[&str] = &["api", "xml.fetcher.php"];
const TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone)]
pub struct SimBriefClient {
    http: reqwest::Client,
    base_url: Url,
}

impl SimBriefClient {
    /// A client for simbrief.com.
    pub fn simbrief() -> Result<Self, SimBriefError> {
        let base =
            Url::parse(DEFAULT_BASE_URL).map_err(|e| SimBriefError::BadResponse(e.to_string()))?;
        Self::new(base)
    }

    pub fn new(base_url: Url) -> Result<Self, SimBriefError> {
        let http = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .user_agent(concat!("infinity-manager/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self::with_http(http, base_url))
    }

    pub fn with_http(http: reqwest::Client, base_url: Url) -> Self {
        Self { http, base_url }
    }

    /// The latest OFP for `pilot_id`, lowered to the gauge wire payload.
    /// The ID must be a decimal SimBrief user number; anything else is
    /// rejected without a request.
    pub async fn fetch(&self, pilot_id: &str) -> Result<SimBriefPayload, SimBriefError> {
        let id = pilot_id.trim();
        if id.is_empty() {
            return Err(SimBriefError::PilotIdMissing);
        }
        if !id.bytes().all(|c| c.is_ascii_digit()) {
            return Err(SimBriefError::PilotIdNotFound(id.to_string()));
        }

        let mut url = self.base_url.clone();
        url.path_segments_mut()
            .map_err(|()| SimBriefError::BadResponse(format!("bad base URL {}", self.base_url)))?
            .pop_if_empty()
            .extend(FETCHER_PATH);
        url.query_pairs_mut()
            .append_pair("userid", id)
            .append_pair("json", "1");
        tracing::debug!(pilot_id = %id, "simbrief: fetching");

        let resp = self.http.get(url).send().await?;
        let status = resp.status();
        if !status.is_success() {
            // Lookup failures come back as HTTP 400 with the fetcher JSON
            // body (`{"fetch":{"status":"Error: ..."}}`); classify them like
            // the 200 path and keep the raw body for the log.
            let body = resp.text().await.unwrap_or_default();
            tracing::warn!(%status, body = %body, "simbrief: non-200");
            let fetch_status = serde_json::from_str::<SimBriefOfp>(&body)
                .ok()
                .and_then(|ofp| ofp.fetch.map(|f| f.status));
            return Err(match fetch_status {
                Some(s) => classify_fetch_error(&s, id),
                None => SimBriefError::Upstream(format!("SimBrief request failed (HTTP {status})")),
            });
        }

        let bytes = resp.bytes().await?;
        let ofp: SimBriefOfp = serde_json::from_slice(&bytes)
            .map_err(|e| SimBriefError::BadResponse(e.to_string()))?;
        // Successful responses carry `fetch.status = "Success"`.
        if let Some(fetch) = ofp.fetch.as_ref()
            && fetch.status.starts_with("Error")
        {
            return Err(classify_fetch_error(&fetch.status, id));
        }

        let payload = to_payload(&ofp);
        if payload.is_empty() {
            return Err(SimBriefError::Empty);
        }
        Ok(payload)
    }
}

/// Maps a `fetch.status` error string onto the typed error, so consumers
/// show the human case instead of the upstream text. Unrecognised statuses
/// pass through as `Upstream` with the `Error:` prefix trimmed.
fn classify_fetch_error(status: &str, pilot_id: &str) -> SimBriefError {
    let lower = status.to_lowercase();
    if lower.contains("no flight plan") {
        SimBriefError::Empty
    } else if lower.contains("not found")
        || lower.contains("invalid user")
        || lower.contains("unknown user")
    {
        SimBriefError::PilotIdNotFound(pilot_id.to_string())
    } else {
        let msg = status.trim_start_matches("Error:").trim();
        SimBriefError::Upstream(msg.to_string())
    }
}

/// Lowers the OFP to the wire payload: one waypoint per navlog fix, airway
/// labels dropped (the gauges have no airway storage), blank idents skipped
/// and a zero altitude (airports) omitted.
pub fn to_payload(ofp: &SimBriefOfp) -> SimBriefPayload {
    let waypoints = parse_navlog_fixes(&ofp.navlog.fix)
        .into_iter()
        .filter_map(|fix| {
            let ident = fix.ident.trim();
            if ident.is_empty() {
                return None;
            }
            let alt = parse_i32(&fix.altitude_feet);
            Some(SimBriefWaypoint {
                ident: ident.to_string(),
                lat: parse_f64(&fix.pos_lat),
                lon: parse_f64(&fix.pos_long),
                alt_ft: (alt > 0).then_some(alt),
            })
        })
        .collect();

    SimBriefPayload {
        version: SIMBRIEF_WIRE_VERSION,
        ofp_id: ofp.general.ofp_id.clone(),
        origin: ofp.origin.icao_code.clone(),
        destination: ofp.destination.icao_code.clone(),
        waypoints,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_server::{Response, TestServer};

    #[test]
    fn lowers_full_ofp() {
        let raw = serde_json::json!({
            "general": { "ofp_id": "AAL123-2026-05-28" },
            "origin": { "icao_code": "KEDW" },
            "destination": { "icao_code": "KLSV" },
            "navlog": {
                "fix": [
                    { "ident": "KEDW", "pos_lat": "34.91", "pos_long": "-117.88",
                      "altitude_feet": "0", "type": "apt", "via_airway": "" },
                    { "ident": "DAG", "pos_lat": "35.23", "pos_long": "-116.57",
                      "altitude_feet": "18000", "type": "vor", "via_airway": "J18" },
                    { "ident": "KLSV", "pos_lat": "36.23", "pos_long": "-115.03",
                      "altitude_feet": "0", "type": "apt", "via_airway": "" },
                ]
            },
        });
        let ofp: SimBriefOfp = serde_json::from_value(raw).unwrap();
        let payload = to_payload(&ofp);
        assert_eq!(payload.version, SIMBRIEF_WIRE_VERSION);
        assert_eq!(payload.origin, "KEDW");
        assert_eq!(payload.destination, "KLSV");
        assert_eq!(payload.ofp_id, "AAL123-2026-05-28");
        assert_eq!(payload.waypoints.len(), 3);
        assert_eq!(payload.waypoints[1].ident, "DAG");
        assert_eq!(payload.waypoints[1].alt_ft, Some(18_000));
        assert_eq!(payload.waypoints[1].lat, 35.23);
        // Airports report "0": suppressed.
        assert_eq!(payload.waypoints[0].alt_ft, None);
    }

    #[test]
    fn single_fix_object_is_accepted() {
        let raw = serde_json::json!({
            "navlog": { "fix": { "ident": "DAG", "pos_lat": "1", "pos_long": "2" } }
        });
        let ofp: SimBriefOfp = serde_json::from_value(raw).unwrap();
        assert_eq!(to_payload(&ofp).waypoints[0].ident, "DAG");
    }

    #[test]
    fn skips_blank_idents() {
        let raw = serde_json::json!({
            "navlog": { "fix": [ { "ident": "", "pos_lat": "0", "pos_long": "0" } ] }
        });
        let ofp: SimBriefOfp = serde_json::from_value(raw).unwrap();
        assert!(to_payload(&ofp).waypoints.is_empty());
    }

    #[test]
    fn classifies_no_flight_plan_as_empty() {
        // Exact shape SimBrief sends with HTTP 400 when the user exists but
        // has no OFP on file.
        let body = r#"{"fetch":{"userid":"555605","static_id":"","status":"Error: No flight plan on file for the specified user","time":"0.0002"}}"#;
        let ofp: SimBriefOfp = serde_json::from_str(body).unwrap();
        let status = ofp.fetch.unwrap().status;
        assert!(matches!(
            classify_fetch_error(&status, "555605"),
            SimBriefError::Empty
        ));
    }

    #[test]
    fn classifies_unknown_user_as_not_found() {
        assert!(matches!(
            classify_fetch_error("Error: Unknown UserID", "1"),
            SimBriefError::PilotIdNotFound(_)
        ));
    }

    #[test]
    fn unrecognised_status_trims_error_prefix() {
        match classify_fetch_error("Error: something else broke", "1") {
            SimBriefError::Upstream(m) => assert_eq!(m, "something else broke"),
            other => panic!("expected Upstream, got {other:?}"),
        }
    }

    fn ofp_body() -> serde_json::Value {
        serde_json::json!({
            "fetch": { "status": "Success" },
            "general": { "ofp_id": "OFP1" },
            "origin": { "icao_code": "KEDW" },
            "destination": { "icao_code": "KLSV" },
            "navlog": { "fix": [
                { "ident": "DAG", "pos_lat": "35.23", "pos_long": "-116.57", "altitude_feet": "18000" }
            ] }
        })
    }

    async fn client_for(server: &TestServer) -> SimBriefClient {
        SimBriefClient::new(server.base()).unwrap()
    }

    #[tokio::test]
    async fn fetch_queries_by_pilot_id_and_lowers() {
        let server = TestServer::start(|_| Response::json(200, ofp_body())).await;
        let payload = client_for(&server).await.fetch(" 555605 ").await.unwrap();
        assert_eq!(payload.ofp_id, "OFP1");
        assert_eq!(payload.waypoints[0].alt_ft, Some(18_000));
        let req = &server.requests()[0];
        assert_eq!(req.method, "GET");
        assert_eq!(req.target, "/api/xml.fetcher.php?userid=555605&json=1");
    }

    #[tokio::test]
    async fn http_400_with_fetcher_body_is_classified() {
        let server = TestServer::start(|_| {
            Response::json(
                400,
                serde_json::json!({ "fetch": { "status": "Error: No flight plan on file for the specified user" } }),
            )
        })
        .await;
        let err = client_for(&server).await.fetch("1").await.unwrap_err();
        assert!(matches!(err, SimBriefError::Empty), "{err:?}");
    }

    #[tokio::test]
    async fn error_status_on_200_is_classified() {
        let server = TestServer::start(|_| {
            Response::json(
                200,
                serde_json::json!({ "fetch": { "status": "Error: User not found" } }),
            )
        })
        .await;
        let err = client_for(&server).await.fetch("42").await.unwrap_err();
        match err {
            SimBriefError::PilotIdNotFound(id) => assert_eq!(id, "42"),
            other => panic!("expected PilotIdNotFound, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn non_json_failure_is_upstream() {
        let server = TestServer::start(|_| Response::bytes(502, "<html>bad gateway</html>")).await;
        match client_for(&server).await.fetch("1").await.unwrap_err() {
            SimBriefError::Upstream(m) => assert!(m.contains("502"), "{m}"),
            other => panic!("expected Upstream, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn garbage_200_is_bad_response_and_empty_ofp_is_empty() {
        let server = TestServer::start(|req| {
            if req.target.contains("userid=1&") {
                Response::bytes(200, "not json")
            } else {
                Response::json(200, serde_json::json!({ "fetch": { "status": "Success" } }))
            }
        })
        .await;
        let client = client_for(&server).await;
        assert!(matches!(
            client.fetch("1").await.unwrap_err(),
            SimBriefError::BadResponse(_)
        ));
        assert!(matches!(
            client.fetch("2").await.unwrap_err(),
            SimBriefError::Empty
        ));
    }

    #[tokio::test]
    async fn invalid_ids_never_hit_the_network() {
        let server = TestServer::start(|_| Response::json(200, ofp_body())).await;
        let client = client_for(&server).await;
        assert!(matches!(
            client.fetch("not-a-number").await,
            Err(SimBriefError::PilotIdNotFound(_))
        ));
        assert!(matches!(
            client.fetch("  ").await,
            Err(SimBriefError::PilotIdMissing)
        ));
        assert!(server.requests().is_empty());
    }
}
