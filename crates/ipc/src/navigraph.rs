//! Navigraph account linking and navdata updates.

use serde::{Deserialize, Serialize};

/// `navigraph_link_start`: show `user_code` and open
/// `verification_uri_complete`, then poll every `interval` seconds until
/// `expires_in` runs out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkStart {
    pub user_code: String,
    pub verification_uri_complete: String,
    pub expires_in: u32,
    pub interval: u32,
}

/// `navigraph_link_poll`. `slow_down` asks the caller to lengthen its
/// interval; `denied` and `expired` end the attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkPoll {
    Pending,
    SlowDown,
    Linked,
    Denied,
    Expired,
}

impl LinkPoll {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::SlowDown => "slow_down",
            Self::Linked => "linked",
            Self::Denied => "denied",
            Self::Expired => "expired",
        }
    }
}

/// `navigraph_status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NavigraphStatus {
    pub linked: bool,
    pub relink_required: bool,
    pub session_live: bool,
    /// The token store could not be read, so `linked` is unknown.
    pub vault_error: Option<String>,
}

/// Latest matching package, as shown next to the installed cycle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NavdataLatest {
    pub cycle: String,
    pub revision: String,
    pub name: String,
    pub validity_period: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NavdataItem {
    pub product_id: String,
    pub product_name: String,
    /// The aircraft's work folder was found.
    pub installed: bool,
    /// Parsed `cycle.json`; `None` when missing or corrupt (update needed).
    pub current: Option<CycleJson>,
    pub latest: Option<NavdataLatest>,
    pub update_available: bool,
    /// Install job id (`navdata-<productId>`) for progress tracking.
    pub job_id: String,
    pub note: Option<String>,
}

/// `navdata_status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NavdataReport {
    pub linked: bool,
    /// The latest-cycle lookup failed; local info in `items` is still valid.
    pub fetch_error: Option<String>,
    pub items: Vec<NavdataItem>,
}

/// `NavigationData/cycle.json`, in the format Navigraph Hub writes, so a
/// package installed by either tool is recognised by the other.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CycleJson {
    pub cycle: String,
    #[serde(default)]
    pub revision: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub format: String,
    #[serde(default)]
    pub validity_period: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_shapes() {
        assert_eq!(
            serde_json::to_value(LinkPoll::SlowDown).unwrap(),
            serde_json::json!("slow_down")
        );
        assert_eq!(
            serde_json::to_value(LinkPoll::SlowDown).unwrap(),
            serde_json::json!(LinkPoll::SlowDown.as_str())
        );
        let status = NavigraphStatus {
            linked: true,
            relink_required: false,
            session_live: true,
            vault_error: None,
        };
        assert_eq!(
            serde_json::to_value(status).unwrap(),
            serde_json::json!({
                "linked": true, "relinkRequired": false,
                "sessionLive": true, "vaultError": null
            })
        );
        let item = NavdataItem {
            product_id: "t38".into(),
            product_name: "T-38C".into(),
            installed: false,
            current: None,
            latest: None,
            update_available: false,
            job_id: "navdata-t38".into(),
            note: None,
        };
        let v = serde_json::to_value(item).unwrap();
        assert_eq!(v["productId"], "t38");
        assert_eq!(v["updateAvailable"], false);
        assert_eq!(v["jobId"], "navdata-t38");
    }
}
