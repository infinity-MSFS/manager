use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use infinity_ipc::dto::UserProfile;
use infinity_ipc::{IpcError, IpcErrorCode, IpcResult};
use serde::Deserialize;

use crate::AuthConfig;

#[derive(Debug, Clone)]
pub(crate) struct Tokens {
    pub(crate) access_token: String,
    pub(crate) refresh_token: Option<String>,
    pub(crate) expires_at: u64,
}

impl Tokens {
    pub(crate) fn is_fresh(&self) -> bool {
        now() + 60 < self.expires_at
    }
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: u64,
}

pub(crate) async fn exchange_code(
    http: &reqwest::Client,
    cfg: &AuthConfig,
    code: &str,
    verifier: &str,
) -> IpcResult<Tokens> {
    let form = [
        ("grant_type", "authorization_code"),
        ("client_id", cfg.client_id.as_str()),
        ("code", code),
        ("code_verifier", verifier),
        ("redirect_uri", cfg.redirect_uri.as_str()),
    ];
    token_request(http, cfg, &form, IpcErrorCode::AuthForbidden).await
}

pub(crate) async fn refresh(
    http: &reqwest::Client,
    cfg: &AuthConfig,
    refresh_token: &str,
) -> IpcResult<Tokens> {
    let form = [
        ("grant_type", "refresh_token"),
        ("client_id", cfg.client_id.as_str()),
        ("refresh_token", refresh_token),
    ];
    let mut tokens = token_request(http, cfg, &form, IpcErrorCode::AuthExpired).await?;
    tokens
        .refresh_token
        .get_or_insert_with(|| refresh_token.to_owned());
    Ok(tokens)
}

async fn token_request(
    http: &reqwest::Client,
    cfg: &AuthConfig,
    form: &[(&str, &str)],
    rejected: IpcErrorCode,
) -> IpcResult<Tokens> {
    let resp = http
        .post(cfg.endpoints.token.clone())
        .form(form)
        .send()
        .await
        .map_err(network)?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(
            IpcError::new(rejected, format!("token endpoint returned {status}"))
                .with_details(serde_json::json!({ "body": body })),
        );
    }
    let raw: TokenResponse = resp
        .json()
        .await
        .map_err(|e| IpcError::internal(format!("token response: {e}")))?;
    Ok(Tokens {
        access_token: raw.access_token,
        refresh_token: raw.refresh_token,
        expires_at: now() + raw.expires_in,
    })
}

pub(crate) async fn userinfo(
    http: &reqwest::Client,
    cfg: &AuthConfig,
    access_token: &str,
) -> IpcResult<UserProfile> {
    #[derive(Deserialize)]
    struct Raw {
        sub: String,
        name: Option<String>,
        email: Option<String>,
        picture: Option<String>,
    }

    let resp = http
        .get(cfg.endpoints.userinfo.clone())
        .bearer_auth(access_token)
        .send()
        .await
        .map_err(network)?;
    let status = resp.status();
    if !status.is_success() {
        let code = if status == reqwest::StatusCode::UNAUTHORIZED {
            IpcErrorCode::AuthExpired
        } else {
            IpcErrorCode::AuthForbidden
        };
        return Err(IpcError::new(code, format!("userinfo returned {status}")));
    }
    let raw: Raw = resp
        .json()
        .await
        .map_err(|e| IpcError::internal(format!("userinfo response: {e}")))?;
    Ok(UserProfile {
        subject: raw.sub,
        name: raw.name,
        email: raw.email,
        picture: raw.picture,
        permissions: token_permissions(access_token),
    })
}

pub(crate) fn token_permissions(access_token: &str) -> Vec<String> {
    #[derive(Deserialize)]
    struct Claims {
        #[serde(default)]
        permissions: Vec<String>,
    }
    access_token
        .split('.')
        .nth(1)
        .and_then(|payload| URL_SAFE_NO_PAD.decode(payload).ok())
        .and_then(|bytes| serde_json::from_slice::<Claims>(&bytes).ok())
        .map(|c| c.permissions)
        .unwrap_or_default()
}

pub(crate) async fn revoke(
    http: &reqwest::Client,
    cfg: &AuthConfig,
    refresh_token: &str,
) -> IpcResult<()> {
    let Some(endpoint) = &cfg.endpoints.revoke else {
        return Ok(());
    };
    let form = [
        ("client_id", cfg.client_id.as_str()),
        ("token", refresh_token),
    ];
    let resp = http
        .post(endpoint.clone())
        .form(&form)
        .send()
        .await
        .map_err(network)?;
    if resp.status().is_success() {
        Ok(())
    } else {
        Err(IpcError::new(
            IpcErrorCode::AuthForbidden,
            format!("revoke returned {}", resp.status()),
        ))
    }
}

fn network(e: reqwest::Error) -> IpcError {
    IpcError::new(IpcErrorCode::NetworkUnreachable, e.to_string())
}

pub(crate) fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_permissions_claim() {
        let payload =
            URL_SAFE_NO_PAD.encode(br#"{"sub":"u","permissions":["developer","channel:beta"]}"#);
        let token = format!("h.{payload}.s");
        assert_eq!(token_permissions(&token), ["developer", "channel:beta"]);
        assert!(token_permissions("not-a-jwt").is_empty());
    }
}
