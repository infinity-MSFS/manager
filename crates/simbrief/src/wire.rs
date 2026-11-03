//! The SimBrief dialect spoken to the aircraft, over the CommBus bridge
//! (gauge-initiated fetches) and the EFB (`fpl.*`). Both transports share
//! the payload and the error codes; only the envelopes differ. The host
//! reads the pilot ID from its preferences and does the sending.
//!
//! Bridge protocol:
//! - gauge → manager event [`FETCH_REQUEST_EVENT`] `{ "request_id"?: string }`;
//!   the manager answers with command [`DELIVER_COMMAND`] carrying
//!   [`bridge_deliver`].
//! - gauge → manager event [`STATUS_REQUEST_EVENT`]; the manager answers
//!   with command [`STATUS_COMMAND`] carrying [`bridge_status`].
//!
//! Undeliverable replies (gauge gone) are logged, not retried: the gauge
//! owns retries.

use std::time::Duration;

use serde::Serialize;
use serde_json::{Value, json};

use crate::client::SimBriefClient;
use crate::error::SimBriefError;
use crate::payload::SimBriefPayload;

/// Bridge event a gauge emits to request a fetch.
pub const FETCH_REQUEST_EVENT: &str = "simbrief.fetch_requested";
/// Bridge event a gauge emits to ask whether a pilot ID is set.
pub const STATUS_REQUEST_EVENT: &str = "simbrief.status_request";
/// Bridge command delivering a fetch result to the gauge.
pub const DELIVER_COMMAND: &str = "simbrief.deliver";
/// Bridge command pushing pilot-ID status to the gauge.
pub const STATUS_COMMAND: &str = "simbrief.status";
/// How long to wait for the gauge to ack a command. Wide because sim
/// frames stretch under load, and a spurious timeout leaves the gauge on
/// FETCHING.
pub const DELIVER_TIMEOUT: Duration = Duration::from_secs(10);

/// Where an error will be shown, which decides the wording of the
/// missing-pilot-ID prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    /// The aircraft MFD: points the pilot at the manager.
    Gauge,
    /// The EFB, which has its own Settings field.
    Efb,
}

/// `{ code, message }` as shipped to the gauge and the EFB.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WireError {
    pub code: String,
    pub message: String,
}

impl WireError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }

    /// For host failures such as unreadable preferences.
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new("internal", message)
    }

    pub fn from_error(e: &SimBriefError, surface: Surface) -> Self {
        let message = match (e, surface) {
            (SimBriefError::PilotIdMissing, Surface::Gauge) => {
                "Enter your SimBrief pilot ID in the Infinity Manager Settings.".to_string()
            }
            (SimBriefError::PilotIdMissing, Surface::Efb) => {
                "Enter your SimBrief pilot ID in Settings.".to_string()
            }
            _ => e.to_string(),
        };
        Self::new(e.code(), message)
    }
}

/// Trims and validates a pilot ID before saving: blank clears (`None`),
/// anything else must be all ASCII digits. Shared by every write path
/// (manager Settings, EFB `fpl.setPilotId`) so they cannot drift.
pub fn normalize_pilot_id(pilot_id: Option<String>) -> Result<Option<String>, SimBriefError> {
    let trimmed = pilot_id
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    if let Some(id) = trimmed.as_ref()
        && !id.bytes().all(|c| c.is_ascii_digit())
    {
        return Err(SimBriefError::PilotIdInvalid);
    }
    Ok(trimmed)
}

pub fn pilot_id_set(pilot_id: Option<&str>) -> bool {
    pilot_id.is_some_and(|s| !s.trim().is_empty())
}

/// Fetches the plan for the saved pilot ID; `None` or blank is
/// [`SimBriefError::PilotIdMissing`].
pub async fn fetch_plan(
    client: &SimBriefClient,
    pilot_id: Option<&str>,
) -> Result<SimBriefPayload, SimBriefError> {
    match pilot_id.filter(|s| !s.trim().is_empty()) {
        Some(id) => client.fetch(id).await,
        None => Err(SimBriefError::PilotIdMissing),
    }
}

/// The optional `request_id` of a [`FETCH_REQUEST_EVENT`], echoed back so
/// the gauge can correlate retries.
pub fn request_id(event_data: &Value) -> Option<String> {
    event_data
        .get("request_id")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// [`STATUS_COMMAND`] body: `{ "pilot_id_set": bool }`.
pub fn bridge_status(pilot_id: Option<&str>) -> Value {
    json!({ "pilot_id_set": pilot_id_set(pilot_id) })
}

/// EFB `fpl.pilotId` body: `{ "pilot_id_set": bool, "pilot_id": string|null }`.
pub fn efb_status(pilot_id: Option<&str>) -> Value {
    json!({ "pilot_id_set": pilot_id_set(pilot_id), "pilot_id": pilot_id })
}

/// [`DELIVER_COMMAND`] body: the payload fields plus
/// `"request_id"` and `"error": null` on success, or
/// `{ "request_id", "error": { code, message } }` on failure.
pub fn bridge_deliver(
    request_id: Option<&str>,
    outcome: Result<&SimBriefPayload, &WireError>,
) -> Value {
    let error = match outcome {
        Ok(payload) => match serde_json::to_value(payload) {
            Ok(Value::Object(mut obj)) => {
                obj.insert("request_id".into(), json!(request_id));
                obj.insert("error".into(), Value::Null);
                return Value::Object(obj);
            }
            Ok(_) => WireError::internal("payload did not serialise to an object"),
            Err(e) => WireError::internal(e.to_string()),
        },
        Err(e) => e.clone(),
    };
    json!({ "request_id": request_id, "error": error })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::payload::SimBriefWaypoint;

    #[test]
    fn pilot_id_normalisation() {
        assert_eq!(normalize_pilot_id(None).unwrap(), None);
        assert_eq!(normalize_pilot_id(Some("  ".into())).unwrap(), None);
        assert_eq!(
            normalize_pilot_id(Some(" 555605 ".into())).unwrap(),
            Some("555605".into())
        );
        let err = normalize_pilot_id(Some("abc".into())).unwrap_err();
        assert_eq!(err.code(), "validation");
    }

    #[test]
    fn status_bodies() {
        assert_eq!(bridge_status(Some("1")), json!({ "pilot_id_set": true }));
        assert_eq!(bridge_status(Some("")), json!({ "pilot_id_set": false }));
        assert_eq!(
            efb_status(None),
            json!({ "pilot_id_set": false, "pilot_id": null })
        );
        assert_eq!(
            efb_status(Some("7")),
            json!({ "pilot_id_set": true, "pilot_id": "7" })
        );
    }

    #[test]
    fn deliver_envelopes() {
        let payload = SimBriefPayload {
            ofp_id: "OFP".into(),
            origin: "KEDW".into(),
            waypoints: vec![SimBriefWaypoint {
                ident: "DAG".into(),
                lat: 1.0,
                lon: 2.0,
                alt_ft: None,
            }],
            ..SimBriefPayload::empty()
        };
        let ok = bridge_deliver(Some("r1"), Ok(&payload));
        assert_eq!(ok["request_id"], "r1");
        assert_eq!(ok["error"], Value::Null);
        assert_eq!(ok["ofp_id"], "OFP");
        assert_eq!(ok["version"], 1);
        assert_eq!(ok["waypoints"][0]["ident"], "DAG");

        let err = WireError::from_error(&SimBriefError::PilotIdMissing, Surface::Gauge);
        assert_eq!(
            bridge_deliver(None, Err(&err)),
            json!({
                "request_id": null,
                "error": {
                    "code": "pilot_id_missing",
                    "message": "Enter your SimBrief pilot ID in the Infinity Manager Settings."
                }
            })
        );
        let efb = WireError::from_error(&SimBriefError::PilotIdMissing, Surface::Efb);
        assert_eq!(efb.message, "Enter your SimBrief pilot ID in Settings.");
        let empty = WireError::from_error(&SimBriefError::Empty, Surface::Efb);
        assert_eq!(empty.code, "empty");
    }

    #[test]
    fn request_id_is_optional() {
        assert_eq!(
            request_id(&json!({ "request_id": "x" })).as_deref(),
            Some("x")
        );
        assert_eq!(request_id(&json!({})), None);
        assert_eq!(request_id(&json!({ "request_id": 5 })), None);
    }

    #[tokio::test]
    async fn fetch_plan_requires_a_pilot_id() {
        let client = SimBriefClient::new(url::Url::parse("http://127.0.0.1:1").unwrap()).unwrap();
        assert!(matches!(
            fetch_plan(&client, None).await,
            Err(SimBriefError::PilotIdMissing)
        ));
        assert!(matches!(
            fetch_plan(&client, Some(" ")).await,
            Err(SimBriefError::PilotIdMissing)
        ));
    }
}
