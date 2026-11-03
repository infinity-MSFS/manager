use infinity_ipc::{IpcError, IpcErrorCode};

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SimBriefError {
    #[error("no SimBrief pilot ID set in preferences")]
    PilotIdMissing,

    #[error("SimBrief pilot ID must be numeric (your Navigraph/SimBrief user number)")]
    PilotIdInvalid,

    #[error("SimBrief pilot ID rejected (not found): {0}")]
    PilotIdNotFound(String),

    #[error("network error talking to SimBrief: {0}")]
    Network(#[from] reqwest::Error),

    #[error("SimBrief returned an unparseable response: {0}")]
    BadResponse(String),

    #[error("No SimBrief flight plan on file yet. Generate one on simbrief.com first.")]
    Empty,

    #[error("SimBrief returned a fatal error: {0}")]
    Upstream(String),
}

impl SimBriefError {
    /// Stable wire code. Part of the SimBrief wire vocabulary: the CommBus
    /// bridge (`simbrief.deliver`) and the EFB (`fpl.result`) ship these
    /// strings and the MFD gauge and EFB client key their error UI off them.
    /// Change values only together with every consumer.
    pub fn code(&self) -> &'static str {
        match self {
            Self::PilotIdMissing => "pilot_id_missing",
            Self::PilotIdInvalid => "validation",
            Self::PilotIdNotFound(_) => "pilot_id_not_found",
            Self::Network(_) => "network",
            Self::BadResponse(_) => "bad_response",
            Self::Empty => "empty",
            Self::Upstream(_) => "upstream",
        }
    }
}

impl From<SimBriefError> for IpcError {
    fn from(e: SimBriefError) -> Self {
        let code = match &e {
            SimBriefError::PilotIdMissing
            | SimBriefError::PilotIdInvalid
            | SimBriefError::PilotIdNotFound(_) => IpcErrorCode::ValidationFailed,
            SimBriefError::Network(n) if n.is_timeout() => IpcErrorCode::CdnTimeout,
            SimBriefError::Network(_) => IpcErrorCode::NetworkUnreachable,
            SimBriefError::BadResponse(_) => IpcErrorCode::CatalogInvalid,
            SimBriefError::Empty => IpcErrorCode::ProductUnknown,
            SimBriefError::Upstream(_) => IpcErrorCode::Internal,
        };
        let reason = e.code();
        IpcError::new(code, e.to_string()).with_details(serde_json::json!({ "reason": reason }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_to_stable_codes_with_reason() {
        let err = IpcError::from(SimBriefError::Empty);
        assert_eq!(err.code, IpcErrorCode::ProductUnknown);
        assert_eq!(err.details.unwrap()["reason"], "empty");
        let err = IpcError::from(SimBriefError::PilotIdMissing);
        assert_eq!(err.code, IpcErrorCode::ValidationFailed);
        let err = IpcError::from(SimBriefError::Upstream("x".into()));
        assert_eq!(err.code, IpcErrorCode::Internal);
    }
}
