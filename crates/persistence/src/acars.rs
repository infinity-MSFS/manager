//! The saved ACARS account, per bridge namespace: `<app_data>/acars/<ns>.json`.
//!
//! The aircraft's ACARS link (the Hoppie network) needs the user's callsign
//! and logon code. They are typed into the aircraft's EFB; this is what
//! remembers them between flights, as [`crate::equipment`] remembers the
//! fit. [`observe`] is the whole rule, kept pure so it is testable without a
//! sim: fed every `acars.account` snapshot the aircraft pushes, it says what
//! to save and what to send back.
//!
//! The file holds the CALLSIGN only. The logon code is a credential and goes
//! in the OS keyring, which the app owns (`infinity-app`'s `acars`), so
//! [`Account`] is what the rule works on and [`AcarsProfile`] what reaches
//! disk.

use std::fmt;
use std::path::{Path, PathBuf};

use infinity_ipc::{IpcError, IpcErrorCode, IpcResult};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Mutex;

use crate::atomic::{read_json_or_default, write_json};

/// What is written to disk. Never the logon code.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AcarsProfile {
    pub callsign: String,
}

#[derive(Debug)]
pub struct AcarsStore {
    dir: PathBuf,
    write: Mutex<()>,
}

impl AcarsStore {
    pub fn new(app_data: &Path) -> Self {
        Self {
            dir: app_data.join("acars"),
            write: Mutex::new(()),
        }
    }

    fn path(&self, namespace: &str) -> IpcResult<PathBuf> {
        let ok = !namespace.is_empty()
            && namespace
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        if !ok {
            return Err(IpcError::new(
                IpcErrorCode::ValidationFailed,
                format!("not a bridge namespace: {namespace:?}"),
            ));
        }
        Ok(self.dir.join(format!("{namespace}.json")))
    }

    pub async fn load(&self, namespace: &str) -> IpcResult<AcarsProfile> {
        read_json_or_default(&self.path(namespace)?).await
    }

    /// Writes `profile` if it differs from what is saved. Whether it wrote.
    pub async fn save(&self, namespace: &str, profile: &AcarsProfile) -> IpcResult<bool> {
        let path = self.path(namespace)?;
        let _guard = self.write.lock().await;
        let current: AcarsProfile = read_json_or_default(&path).await?;
        if &current == profile {
            return Ok(false);
        }
        write_json(&path, profile).await?;
        Ok(true)
    }
}

/// A callsign and a logon code; either may be empty, for "none".
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Account {
    pub callsign: String,
    pub logon: String,
}

/// Never the logon code: this type ends up in `tracing` fields.
impl fmt::Debug for Account {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Account")
            .field("callsign", &self.callsign)
            .field("logon_set", &!self.logon.is_empty())
            .finish()
    }
}

/// The parts of an `acars.account` snapshot persistence reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observed {
    /// `default`, `restore` or `select`.
    pub origin: String,
    pub restored: bool,
    /// The aircraft's account. Its logon is empty unless `origin` is
    /// `select`: the aircraft sends the code only when there is a user's
    /// choice to save.
    pub account: Account,
}

impl Observed {
    /// `None` unless the snapshot is from an aircraft that carries ACARS.
    pub fn parse(snapshot: &Value) -> Option<Self> {
        let o = snapshot.as_object()?;
        if o.get("fitted").and_then(Value::as_bool) != Some(true) {
            return None;
        }
        let text = |key: &str| {
            o.get(key)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        Some(Self {
            origin: text("origin"),
            restored: o.get("restored").and_then(Value::as_bool) == Some(true),
            account: Account {
                callsign: text("callsign"),
                logon: text("logon"),
            },
        })
    }
}

/// What to do about one snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Observation {
    /// Save this account: the user's choice, when it differs from `saved`.
    pub save: Option<Account>,
    /// Send `acars_account_apply {callsign, logon, restore: true}` with
    /// this account.
    pub send: Option<Account>,
}

/// The persistence rule, applied to every snapshot the aircraft pushes.
///
/// * A fresh aircraft (`restored: false`) is sent the saved account — an
///   empty one when nothing is saved: it still has to hear that the restore
///   happened, or it would ask on every push. Nothing it reports is recorded
///   until that has landed.
/// * A USER change (`origin: select`, typed into the EFB) is saved, and the
///   same account is sent back as a restore. That is the acknowledgement:
///   the aircraft stops pushing the logon code once it has it.
/// * Anything else (`restore`, `default`) is not a choice and is ignored.
pub fn observe(saved: &Account, seen: &Observed) -> Observation {
    if !seen.restored {
        return Observation {
            save: None,
            send: Some(saved.clone()),
        };
    }
    if seen.origin != "select" {
        return Observation::default();
    }
    Observation {
        save: (seen.account != *saved).then(|| seen.account.clone()),
        send: Some(seen.account.clone()),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn account(callsign: &str, logon: &str) -> Account {
        Account {
            callsign: callsign.into(),
            logon: logon.into(),
        }
    }

    fn seen(origin: &str, restored: bool, callsign: &str, logon: &str) -> Observed {
        Observed {
            origin: origin.into(),
            restored,
            account: account(callsign, logon),
        }
    }

    #[test]
    fn a_fresh_aircraft_is_sent_the_saved_account_and_what_it_reports_is_not_recorded() {
        let saved = account("FDX123", "Code1");
        let o = observe(&saved, &seen("default", false, "", ""));
        assert_eq!(o.send, Some(saved.clone()));
        assert_eq!(o.save, None);
        // Even a choice is not recorded before the restore has landed.
        let o = observe(&saved, &seen("select", false, "NEW1", "Other"));
        assert_eq!(o.save, None);
        assert_eq!(o.send, Some(saved));
    }

    #[test]
    fn with_nothing_saved_the_restore_is_still_sent_empty() {
        let o = observe(&Account::default(), &seen("default", false, "", ""));
        assert_eq!(o.send, Some(Account::default()));
    }

    #[test]
    fn a_users_choice_is_saved_and_acknowledged() {
        let saved = account("FDX123", "Code1");
        let o = observe(&saved, &seen("select", true, "FDX456", "Code1"));
        assert_eq!(o.save, Some(account("FDX456", "Code1")));
        assert_eq!(o.send, Some(account("FDX456", "Code1")));
        // The re-push of a choice already saved writes nothing, and is
        // acknowledged again in case the first acknowledgement was lost.
        let o = observe(
            &account("FDX456", "Code1"),
            &seen("select", true, "FDX456", "Code1"),
        );
        assert_eq!(o.save, None);
        assert!(o.send.is_some());
    }

    #[test]
    fn removing_the_logon_code_is_a_choice_too() {
        let o = observe(
            &account("FDX123", "Code1"),
            &seen("select", true, "FDX123", ""),
        );
        assert_eq!(o.save, Some(account("FDX123", "")));
    }

    #[test]
    fn a_restore_and_the_defaults_are_not_choices() {
        let saved = account("FDX123", "Code1");
        for origin in ["restore", "default", ""] {
            let o = observe(&saved, &seen(origin, true, "FDX123", ""));
            assert_eq!(o, Observation::default(), "{origin}");
        }
    }

    #[test]
    fn parse_needs_an_aircraft_that_carries_acars() {
        let ok = json!({ "fitted": true, "origin": "select", "restored": true,
            "callsign": "FDX123", "logon": "Code1" });
        assert_eq!(
            Observed::parse(&ok),
            Some(seen("select", true, "FDX123", "Code1"))
        );
        // The logon is absent unless there is a choice to save.
        let quiet =
            json!({ "fitted": true, "origin": "restore", "restored": true, "callsign": "FDX123" });
        assert_eq!(
            Observed::parse(&quiet),
            Some(seen("restore", true, "FDX123", ""))
        );
        for bad in [
            json!({ "fitted": false, "origin": "default", "restored": false, "callsign": "" }),
            json!({ "origin": "select" }),
            json!("text"),
        ] {
            assert_eq!(Observed::parse(&bad), None, "{bad}");
        }
    }

    #[test]
    fn an_account_never_prints_its_logon_code() {
        let text = format!("{:?}", account("FDX123", "S3cretCode"));
        assert!(text.contains("FDX123"));
        assert!(!text.contains("S3cret"));
    }

    #[tokio::test]
    async fn the_store_round_trips_writes_only_a_change_and_rejects_bad_namespaces() {
        let Ok(dir) = tempfile::tempdir() else {
            return;
        };
        let store = AcarsStore::new(dir.path());
        assert_eq!(store.load("dc10").await.ok(), Some(AcarsProfile::default()));
        let profile = AcarsProfile {
            callsign: "FDX123".into(),
        };
        assert_eq!(store.save("dc10", &profile).await.ok(), Some(true));
        assert_eq!(store.save("dc10", &profile).await.ok(), Some(false));
        assert_eq!(store.load("dc10").await.ok(), Some(profile));
        assert!(store.load("../x").await.is_err());
    }
}
