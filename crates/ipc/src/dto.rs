use serde::{Deserialize, Serialize};

/// Indicator of what version the manager binary is
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Edition {
    /// Built from public repo free, doesnt contain any store or DRM providers thus making it not useful for payware
    Community,
    /// Official signed binary
    Official,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    pub product_name: String,
    pub version: String,
    pub edition: Edition,
    /// windows, linux
    pub os: String,
    pub arch: String,
}

/// Build capabilities
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    /// Sign-in is available
    pub accounts: bool,
    /// Paid products can be entitled and downloaded
    pub store: bool,
    /// In-sim DRM handshakes are served
    pub drm: bool,
    /// Update channels this build may switch between
    pub channels: Vec<Channel>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    Stable,
    Beta,
    Nightly,
}

impl Channel {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Beta => "beta",
            Self::Nightly => "nightly",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "state")]
pub enum SessionState {
    SignedOut,
    Pending,
    SignedIn { profile: UserProfile },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserProfile {
    pub subject: String,
    pub name: Option<String>,
    pub email: Option<String>,
    pub picture: Option<String>,
    #[serde(default)]
    pub permissions: Vec<String>,
}

impl UserProfile {
    pub fn has_permission(&self, permission: &str) -> bool {
        self.permissions.iter().any(|p| p == permission)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginStarted {
    pub authorize_url: String,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Preferences {
    #[serde(default)]
    pub motion: MotionPreference,
    #[serde(flatten)]
    pub rest: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MotionPreference {
    #[default]
    Auto,
    Full,
    Reduced,
}

impl MotionPreference {
    pub const fn reduce(self, sim_running: bool) -> bool {
        match self {
            Self::Auto => sim_running,
            Self::Full => false,
            Self::Reduced => true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SimStatus {
    pub running: Option<Sim>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sim {
    Msfs2020,
    Msfs2024,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preferences_keep_unknown_v3_fields() {
        let v3 = serde_json::json!({
            "channel": "beta",
            "theme": "amoled",
            "installDir": "D:\\Community",
            "firstRunComplete": true
        });
        let prefs: Preferences = serde_json::from_value(v3.clone()).unwrap();
        assert_eq!(prefs.motion, MotionPreference::Auto);

        let mut out = serde_json::to_value(&prefs).unwrap();
        out.as_object_mut().unwrap().remove("motion");
        assert_eq!(out, v3);
    }

    #[test]
    fn auto_motion_follows_the_sim() {
        assert!(MotionPreference::Auto.reduce(true));
        assert!(!MotionPreference::Auto.reduce(false));
        assert!(MotionPreference::Reduced.reduce(false));
        assert!(!MotionPreference::Full.reduce(true));
    }
}
