//! HTTP client for the Navigraph identity (device flow), charts, navdata and
//! enroute-tile APIs.

use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngCore;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use url::Url;

use crate::error::{NavigraphError, Result};
use crate::navdata::NavdataPackage;
use crate::secret::Secret;

/// Scopes requested at link time. `fmsdata` is what the navdata packages
/// endpoint needs; links made without it get 403s there.
pub const SCOPES: &str = "openid offline_access charts fmsdata";

const TIMEOUT: Duration = Duration::from_secs(20);
const USER_AGENT: &str = concat!("infinity-manager/", env!("CARGO_PKG_VERSION"));

/// Base URLs of the Navigraph services. Overridable so tests can point at a
/// local server.
#[derive(Debug, Clone)]
pub struct Endpoints {
    pub identity: Url,
    pub api: Url,
    pub tiles: Url,
}

impl Endpoints {
    pub fn navigraph() -> Result<Self> {
        let parse = |s: &str| Url::parse(s).map_err(|e| NavigraphError::Internal(e.to_string()));
        Ok(Self {
            identity: parse("https://identity.api.navigraph.com")?,
            api: parse("https://api.navigraph.com")?,
            tiles: parse("https://enroute-bitmap.charts.api-v2.navigraph.com")?,
        })
    }

    /// All three services on one base, for tests and proxies.
    pub fn single(base: Url) -> Self {
        Self {
            identity: base.clone(),
            api: base.clone(),
            tiles: base,
        }
    }
}

/// The Navigraph app's OAuth client. Supplied at runtime by a
/// [`CredentialsSource`](crate::CredentialsSource); never compiled in.
#[derive(Debug, Clone)]
pub struct AppCredentials {
    pub client_id: String,
    pub client_secret: Secret,
}

impl AppCredentials {
    pub fn new(client_id: impl Into<String>, client_secret: impl Into<Secret>) -> Self {
        Self {
            client_id: client_id.into(),
            client_secret: client_secret.into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Pkce {
    pub verifier: Secret,
    pub challenge: String,
}

impl Pkce {
    pub fn generate() -> Self {
        let mut bytes = [0u8; 32];
        rand::rng().fill_bytes(&mut bytes);
        let verifier = URL_SAFE_NO_PAD.encode(bytes);
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        Self {
            verifier: Secret::new(verifier),
            challenge,
        }
    }
}

/// `POST /connect/deviceauthorization` response.
#[derive(Debug, Clone, Deserialize)]
pub struct DeviceAuth {
    pub device_code: Secret,
    pub user_code: String,
    #[serde(default)]
    pub verification_uri: String,
    pub verification_uri_complete: String,
    pub expires_in: u32,
    #[serde(default)]
    pub interval: u32,
}

/// CloudFront signed cookies that unlock the enroute tile server. Issued
/// as `Set-Cookie` headers on token responses.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct TileCookies {
    pub policy: String,
    pub signature: String,
    pub key_pair_id: String,
}

impl std::fmt::Debug for TileCookies {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TileCookies")
            .field("complete", &self.is_complete())
            .finish_non_exhaustive()
    }
}

impl TileCookies {
    pub fn is_complete(&self) -> bool {
        !self.policy.is_empty() && !self.signature.is_empty() && !self.key_pair_id.is_empty()
    }

    /// `Cookie` header value for tile requests.
    pub fn header(&self) -> String {
        format!(
            "CloudFront-Policy={}; CloudFront-Signature={}; CloudFront-Key-Pair-Id={}",
            self.policy, self.signature, self.key_pair_id
        )
    }
}

#[derive(Debug, Clone)]
pub struct NavTokens {
    pub access_token: Secret,
    pub expires_in: u32,
    pub refresh_token: Secret,
    pub cookies: TileCookies,
}

/// One device-flow poll.
#[derive(Debug)]
pub enum Poll {
    Pending,
    SlowDown,
    Denied,
    Expired,
    Ready(Box<NavTokens>),
}

#[derive(Deserialize)]
struct TokenBody {
    access_token: String,
    expires_in: u32,
    refresh_token: Option<String>,
}

#[derive(Deserialize)]
struct OauthErrorBody {
    error: String,
}

#[derive(Debug, Clone)]
pub struct NavigraphClient {
    http: reqwest::Client,
    endpoints: Endpoints,
}

impl NavigraphClient {
    pub fn new(endpoints: Endpoints) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(USER_AGENT)
            .build()?;
        Ok(Self::with_http(http, endpoints))
    }

    pub fn with_http(http: reqwest::Client, endpoints: Endpoints) -> Self {
        Self { http, endpoints }
    }

    pub fn endpoints(&self) -> &Endpoints {
        &self.endpoints
    }

    pub async fn device_authorization(
        &self,
        creds: &AppCredentials,
        pkce: &Pkce,
    ) -> Result<DeviceAuth> {
        let url = endpoint(
            &self.endpoints.identity,
            &["connect", "deviceauthorization"],
        )?;
        let resp = self
            .http
            .post(url)
            .form(&[
                ("client_id", creds.client_id.as_str()),
                ("client_secret", creds.client_secret.expose()),
                ("code_challenge", pkce.challenge.as_str()),
                ("code_challenge_method", "S256"),
            ])
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(api_error(resp).await);
        }
        Ok(resp.json::<DeviceAuth>().await?)
    }

    pub async fn poll_device_token(
        &self,
        creds: &AppCredentials,
        pkce: &Pkce,
        device_code: &Secret,
    ) -> Result<Poll> {
        let url = endpoint(&self.endpoints.identity, &["connect", "token"])?;
        let resp = self
            .http
            .post(url)
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("device_code", device_code.expose()),
                ("code_verifier", pkce.verifier.expose()),
                ("client_id", creds.client_id.as_str()),
                ("client_secret", creds.client_secret.expose()),
                ("scope", SCOPES),
            ])
            .send()
            .await?;

        if resp.status().is_success() {
            return Ok(Poll::Ready(Box::new(parse_tokens(resp).await?)));
        }
        if resp.status() == reqwest::StatusCode::BAD_REQUEST {
            let body = resp.text().await.unwrap_or_default();
            return match oauth_error(&body).as_str() {
                "authorization_pending" => Ok(Poll::Pending),
                "slow_down" => Ok(Poll::SlowDown),
                "access_denied" => Ok(Poll::Denied),
                "expired_token" => Ok(Poll::Expired),
                _ => Err(NavigraphError::Api { status: 400, body }),
            };
        }
        Err(api_error(resp).await)
    }

    /// Exchanges a refresh token. Navigraph rotates refresh tokens: the one
    /// passed in is dead once this succeeds. `invalid_grant` means the link
    /// is gone and maps to [`NavigraphError::RelinkRequired`].
    pub async fn refresh(
        &self,
        creds: &AppCredentials,
        refresh_token: &Secret,
    ) -> Result<NavTokens> {
        let url = endpoint(&self.endpoints.identity, &["connect", "token"])?;
        let resp = self
            .http
            .post(url)
            .form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh_token.expose()),
                ("client_id", creds.client_id.as_str()),
                ("client_secret", creds.client_secret.expose()),
            ])
            .send()
            .await?;

        if resp.status().is_success() {
            return parse_tokens(resp).await;
        }
        if resp.status() == reqwest::StatusCode::BAD_REQUEST {
            let body = resp.text().await.unwrap_or_default();
            if oauth_error(&body) == "invalid_grant" {
                return Err(NavigraphError::RelinkRequired);
            }
            return Err(NavigraphError::Api { status: 400, body });
        }
        Err(api_error(resp).await)
    }

    /// `GET /v2/charts/{icao}`. An airport without charts is an empty index.
    pub async fn charts_index(&self, bearer: &Secret, icao: &str) -> Result<serde_json::Value> {
        let url = endpoint(&self.endpoints.api, &["v2", "charts", icao])?;
        let resp = self
            .http
            .get(url)
            .bearer_auth(bearer.expose())
            .send()
            .await?;
        match resp.status().as_u16() {
            200 => Ok(resp.json().await?),
            401 | 403 => Err(NavigraphError::NoSubscription),
            404 => Ok(serde_json::json!({ "charts": [] })),
            _ => Err(api_error(resp).await),
        }
    }

    pub async fn chart_bytes(
        &self,
        bearer: &Secret,
        icao: &str,
        filename: &str,
    ) -> Result<Vec<u8>> {
        let url = endpoint(&self.endpoints.api, &["v2", "charts", icao, filename])?;
        let resp = self
            .http
            .get(url)
            .bearer_auth(bearer.expose())
            .send()
            .await?;
        match resp.status().as_u16() {
            200 => Ok(resp.bytes().await?.to_vec()),
            401 | 403 => Err(NavigraphError::NoSubscription),
            404 => Err(NavigraphError::Api {
                status: 404,
                body: "chart not found".into(),
            }),
            _ => Err(api_error(resp).await),
        }
    }

    /// `GET /v1/navdata/packages`: the packages this client may hand out to
    /// the linked user.
    pub async fn navdata_packages(&self, bearer: &Secret) -> Result<Vec<NavdataPackage>> {
        let url = endpoint(&self.endpoints.api, &["v1", "navdata", "packages"])?;
        let resp = self
            .http
            .get(url)
            .bearer_auth(bearer.expose())
            .send()
            .await?;
        match resp.status().as_u16() {
            200 => {
                let body = resp.text().await?;
                serde_json::from_str(&body).map_err(|e| {
                    let head: String = body.chars().take(500).collect();
                    NavigraphError::Api {
                        status: 200,
                        body: format!("unexpected packages response ({e}): {head}"),
                    }
                })
            }
            401 | 403 => Err(NavigraphError::NoSubscription),
            _ => Err(api_error(resp).await),
        }
    }

    pub async fn tile_bytes(
        &self,
        cookies: &TileCookies,
        layer: &str,
        z: u8,
        x: u32,
        y: u32,
    ) -> Result<Vec<u8>> {
        let (z, x, y) = (z.to_string(), x.to_string(), format!("{y}.png"));
        let url = endpoint(&self.endpoints.tiles, &["styles", layer, &z, &x, &y])?;
        let resp = self
            .http
            .get(url)
            .header(reqwest::header::COOKIE, cookies.header())
            .send()
            .await?;
        match resp.status().as_u16() {
            200 => Ok(resp.bytes().await?.to_vec()),
            401 | 403 => Err(NavigraphError::NoSubscription),
            404 => Err(NavigraphError::Api {
                status: 404,
                body: "tile not found".into(),
            }),
            _ => Err(api_error(resp).await),
        }
    }
}

/// `base` with `segments` appended, each percent-encoded.
fn endpoint(base: &Url, segments: &[&str]) -> Result<Url> {
    let mut url = base.clone();
    url.path_segments_mut()
        .map_err(|()| NavigraphError::Internal(format!("{base} cannot be a base URL")))?
        .pop_if_empty()
        .extend(segments);
    Ok(url)
}

fn oauth_error(body: &str) -> String {
    serde_json::from_str::<OauthErrorBody>(body)
        .map(|e| e.error)
        .unwrap_or_default()
}

async fn parse_tokens(resp: reqwest::Response) -> Result<NavTokens> {
    let cookies = parse_tile_cookies(resp.headers());
    let body: TokenBody = resp.json().await?;
    let refresh_token = body.refresh_token.ok_or_else(|| NavigraphError::Api {
        status: 200,
        body: "token response missing refresh_token (offline_access scope?)".into(),
    })?;
    Ok(NavTokens {
        access_token: Secret::new(body.access_token),
        expires_in: body.expires_in,
        refresh_token: Secret::new(refresh_token),
        cookies,
    })
}

/// Picks the CloudFront cookies out of `Set-Cookie` headers, matching names
/// case-insensitively.
pub fn parse_tile_cookies(headers: &reqwest::header::HeaderMap) -> TileCookies {
    let mut cookies = TileCookies::default();
    for value in headers.get_all(reqwest::header::SET_COOKIE) {
        let Ok(s) = value.to_str() else { continue };
        let Some(pair) = s.split(';').next() else {
            continue;
        };
        let Some((name, val)) = pair.split_once('=') else {
            continue;
        };
        match name.trim().to_ascii_lowercase().as_str() {
            "cloudfront-policy" => cookies.policy = val.trim().to_string(),
            "cloudfront-signature" => cookies.signature = val.trim().to_string(),
            "cloudfront-key-pair-id" => cookies.key_pair_id = val.trim().to_string(),
            _ => {}
        }
    }
    cookies
}

/// The `scope` claim of a JWT access token, for diagnostics. `None` when the
/// token is not a JWT or has no scope claim.
pub fn token_scopes(bearer: &Secret) -> Option<Vec<String>> {
    let payload = bearer.expose().split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(payload).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    match v.get("scope")? {
        serde_json::Value::Array(a) => Some(
            a.iter()
                .filter_map(|s| s.as_str().map(str::to_string))
                .collect(),
        ),
        serde_json::Value::String(s) => Some(s.split(' ').map(str::to_string).collect()),
        _ => None,
    }
}

async fn api_error(resp: reqwest::Response) -> NavigraphError {
    let status = resp.status().as_u16();
    let body = resp.text().await.unwrap_or_default();
    NavigraphError::Api { status, body }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_shapes() {
        let p = Pkce::generate();
        assert_eq!(p.verifier.expose().len(), 43);
        assert_eq!(p.challenge.len(), 43);
        assert!(!p.verifier.expose().contains('='));
        let expected = URL_SAFE_NO_PAD.encode(Sha256::digest(p.verifier.expose().as_bytes()));
        assert_eq!(p.challenge, expected);
    }

    #[test]
    fn tile_cookie_parsing_case_insensitive() {
        let mut h = reqwest::header::HeaderMap::new();
        h.append(
            reqwest::header::SET_COOKIE,
            "Cloudfront-Policy=abc123; Path=/; Secure".parse().unwrap(),
        );
        h.append(
            reqwest::header::SET_COOKIE,
            "CloudFront-Signature=sig; Path=/".parse().unwrap(),
        );
        h.append(
            reqwest::header::SET_COOKIE,
            "CloudFront-Key-Pair-Id=KP; Path=/".parse().unwrap(),
        );
        let c = parse_tile_cookies(&h);
        assert!(c.is_complete());
        assert_eq!(c.policy, "abc123");
        assert!(c.header().contains("CloudFront-Signature=sig"));
        assert!(!format!("{c:?}").contains("abc123"));
    }

    #[test]
    fn endpoint_appends_encoded_segments() {
        let base = Url::parse("https://api.example.com/").unwrap();
        let url = endpoint(&base, &["v2", "charts", "KE DW"]).unwrap();
        assert_eq!(url.as_str(), "https://api.example.com/v2/charts/KE%20DW");
        let base = Url::parse("http://127.0.0.1:9/prefix").unwrap();
        let url = endpoint(&base, &["connect", "token"]).unwrap();
        assert_eq!(url.as_str(), "http://127.0.0.1:9/prefix/connect/token");
    }

    #[test]
    fn scopes_read_from_jwt_payload() {
        let payload = URL_SAFE_NO_PAD.encode(br#"{"scope":["openid","fmsdata"]}"#);
        let jwt = Secret::new(format!("h.{payload}.s"));
        assert_eq!(
            token_scopes(&jwt).unwrap(),
            vec!["openid".to_string(), "fmsdata".to_string()]
        );
        assert!(token_scopes(&Secret::new("opaque")).is_none());
    }

    #[test]
    fn debug_redacts_secrets() {
        let creds = AppCredentials::new("id", "topsecret");
        assert!(!format!("{creds:?}").contains("topsecret"));
        let tokens = NavTokens {
            access_token: "acc".into(),
            expires_in: 1,
            refresh_token: "ref-token".into(),
            cookies: TileCookies::default(),
        };
        assert!(!format!("{tokens:?}").contains("ref-token"));
    }
}
