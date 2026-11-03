//! Pairing: the per-install secret, the tokens signed with it, and the short
//! codes a tablet trades for a token.
//!
//! A token is `device_id|expiry_unix_secs|hex(hmac_sha256(secret,
//! "device_id|expiry"))`, URL-safe without padding. The secret never leaves
//! this machine, so a sniffed token cannot be used to forge others, and
//! rotating the secret invalidates every token at once ("forget all
//! devices").
//!
//! Typing a 70-character token on a tablet is miserable, so the manager also
//! mints a 5-character code that the tablet redeems once at `GET /pair` for
//! the real token.

use std::collections::HashMap;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use hmac::{Hmac, Mac};
use rand::Rng;
use sha2::Sha256;
use subtle::ConstantTimeEq;

use crate::error::EfbError;

type HmacSha256 = Hmac<Sha256>;

/// Where v3 keeps the secret: the OS keyring, under the manager's Tauri
/// identifier, as [`PairingSecret::to_hex`]. Reading the same entry keeps
/// tablets paired across the upgrade.
pub const KEYRING_SERVICE: &str = "com.infinity.infinity-manager";
pub const KEYRING_PAIRING_KEY: &str = "efb-pairing-secret";

/// How long a token stays valid once redeemed.
pub const DEFAULT_TOKEN_TTL_SECS: u64 = 7 * 24 * 3600;
/// How long a short code can be redeemed. The token under it lives longer.
pub const PAIRING_CODE_TTL_SECS: u64 = 600;
pub const PAIRING_CODE_LEN: usize = 5;
/// Crockford base32: no I, L, O or U to misread.
const PAIRING_ALPHABET: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
/// Failed redemptions before every pending code is thrown away. `/pair` is
/// open to the LAN, and 32^5 codes is a space a script can walk inside a
/// code's lifetime; this caps it at a guess per code in about 1.6 million.
const MAX_FAILED_REDEEMS: u32 = 20;

const MIN_SECRET_BYTES: usize = 16;
/// Comfortably above the HMAC minimum, and what v3 generates.
const SECRET_BYTES: usize = 32;

#[derive(Clone)]
pub struct PairingSecret(Vec<u8>);

impl std::fmt::Debug for PairingSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PairingSecret(<redacted>)")
    }
}

impl PairingSecret {
    pub fn new(bytes: impl Into<Vec<u8>>) -> Result<Self, EfbError> {
        let bytes = bytes.into();
        if bytes.len() < MIN_SECRET_BYTES {
            return Err(EfbError::InvalidSecret(bytes.len()));
        }
        Ok(Self(bytes))
    }

    /// A fresh random secret from the OS-seeded CSPRNG.
    pub fn generate() -> Self {
        Self(rand::random::<[u8; SECRET_BYTES]>().to_vec())
    }

    /// The persisted form (see [`KEYRING_SERVICE`]).
    pub fn from_hex(hex_str: &str) -> Result<Self, EfbError> {
        let bytes = hex::decode(hex_str.trim()).map_err(|_| EfbError::SecretEncoding)?;
        Self::new(bytes)
    }

    pub fn to_hex(&self) -> String {
        hex::encode(&self.0)
    }

    fn tag(&self, payload: &str) -> Vec<u8> {
        // HMAC takes keys of any length; the error arm is unreachable.
        let Ok(mut mac) = HmacSha256::new_from_slice(&self.0) else {
            return Vec::new();
        };
        mac.update(payload.as_bytes());
        mac.finalize().into_bytes().to_vec()
    }
}

pub(crate) fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

pub fn sign_pairing_token(
    secret: &PairingSecret,
    device_id: &str,
    expiry_unix_secs: u64,
) -> Result<String, EfbError> {
    if device_id.contains('|') {
        return Err(EfbError::InvalidDeviceId);
    }
    let payload = format!("{device_id}|{expiry_unix_secs}");
    let tag = hex::encode(secret.tag(&payload));
    Ok(format!("{payload}|{tag}"))
}

/// The token's device id, if the signature holds and `now <= expiry`. The
/// signature is compared in constant time.
pub fn verify_pairing_token(
    secret: &PairingSecret,
    token: &str,
    now_unix_secs: u64,
) -> Option<String> {
    let mut parts = token.splitn(3, '|');
    let device_id = parts.next()?;
    let expiry: u64 = parts.next()?.parse().ok()?;
    let got = hex::decode(parts.next()?).ok()?;
    if expiry < now_unix_secs {
        return None;
    }
    let expected = secret.tag(&format!("{device_id}|{expiry}"));
    if expected.is_empty() || got.len() != expected.len() {
        return None;
    }
    bool::from(got.ct_eq(&expected)).then(|| device_id.to_owned())
}

pub(crate) fn generate_pairing_code() -> String {
    let mut rng = rand::rng();
    (0..PAIRING_CODE_LEN)
        .map(|_| char::from(PAIRING_ALPHABET[rng.random_range(0..PAIRING_ALPHABET.len())]))
        .collect()
}

struct PendingCode {
    token: String,
    expires_at: Instant,
}

impl std::fmt::Debug for PendingCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingCode")
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Redeem {
    Token(String),
    Unknown,
    Expired,
}

/// Unredeemed codes. Each is single use.
#[derive(Debug, Default)]
pub(crate) struct PairingCodes {
    pending: HashMap<String, PendingCode>,
    failures: u32,
}

impl PairingCodes {
    pub(crate) fn insert(&mut self, token: String, ttl: Duration) -> String {
        let now = Instant::now();
        self.pending.retain(|_, c| c.expires_at > now);
        let code = loop {
            let code = generate_pairing_code();
            if !self.pending.contains_key(&code) {
                break code;
            }
        };
        self.pending.insert(
            code.clone(),
            PendingCode {
                token,
                expires_at: now + ttl,
            },
        );
        code
    }

    pub(crate) fn redeem(&mut self, code: &str) -> Redeem {
        match self.pending.remove(&code.trim().to_uppercase()) {
            Some(c) if c.expires_at > Instant::now() => {
                self.failures = 0;
                Redeem::Token(c.token)
            }
            Some(_) => Redeem::Expired,
            None => {
                self.failures += 1;
                if self.failures >= MAX_FAILED_REDEEMS {
                    tracing::warn!(
                        dropped = self.pending.len(),
                        "efb-server: too many wrong pairing codes; discarding pending codes"
                    );
                    self.clear();
                }
                Redeem::Unknown
            }
        }
    }

    pub(crate) fn clear(&mut self) {
        self.pending.clear();
        self.failures = 0;
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn secret() -> PairingSecret {
        PairingSecret::new(b"test-secret-bytes-of-sufficient-length".to_vec()).unwrap()
    }

    #[test]
    fn round_trip_verifies() {
        let s = secret();
        let token = sign_pairing_token(&s, "tablet-001", 2_000_000_000).unwrap();
        let device = verify_pairing_token(&s, &token, 1_700_000_000);
        assert_eq!(device.as_deref(), Some("tablet-001"));
    }

    #[test]
    fn token_format_matches_v3() {
        // Pinned so tokens already on tablets keep verifying.
        let s = PairingSecret::new(vec![7u8; 32]).unwrap();
        let token = sign_pairing_token(&s, "tablet", 2_000_000_000).unwrap();
        let parts: Vec<&str> = token.split('|').collect();
        assert_eq!(parts[..2], ["tablet", "2000000000"]);
        assert_eq!(parts[2].len(), 64);
        assert!(
            parts[2]
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
    }

    #[test]
    fn expired_token_rejected() {
        let s = secret();
        let token = sign_pairing_token(&s, "tablet-001", 1_000_000_000).unwrap();
        assert!(verify_pairing_token(&s, &token, 1_700_000_000).is_none());
    }

    #[test]
    fn tampered_signature_rejected() {
        let s = secret();
        let mut token = sign_pairing_token(&s, "tablet-001", 2_000_000_000).unwrap();
        token.push('0');
        assert!(verify_pairing_token(&s, &token, 1_700_000_000).is_none());
        let forged = sign_pairing_token(&s, "tablet-001", 2_000_000_000)
            .unwrap()
            .replace("2000000000", "2000000001");
        assert!(verify_pairing_token(&s, &forged, 1_700_000_000).is_none());
    }

    #[test]
    fn wrong_secret_rejected() {
        let s2 = PairingSecret::new(b"different-secret-also-long-enough".to_vec()).unwrap();
        let token = sign_pairing_token(&secret(), "tablet-001", 2_000_000_000).unwrap();
        assert!(verify_pairing_token(&s2, &token, 1_700_000_000).is_none());
    }

    #[test]
    fn short_secret_rejected() {
        assert!(matches!(
            PairingSecret::new(vec![0u8; 8]),
            Err(EfbError::InvalidSecret(8))
        ));
    }

    #[test]
    fn secret_round_trips_through_hex_and_hides_in_debug() {
        let s = PairingSecret::generate();
        let back = PairingSecret::from_hex(&s.to_hex()).unwrap();
        assert_eq!(s.to_hex(), back.to_hex());
        assert_eq!(s.to_hex().len(), 64);
        assert!(!format!("{s:?}").contains(&s.to_hex()));
        assert!(matches!(
            PairingSecret::from_hex("zz"),
            Err(EfbError::SecretEncoding)
        ));
    }

    #[test]
    fn device_ids_cannot_smuggle_a_separator() {
        assert!(matches!(
            sign_pairing_token(&secret(), "a|b", 1),
            Err(EfbError::InvalidDeviceId)
        ));
    }

    #[test]
    fn codes_are_single_use_and_case_insensitive() {
        let mut codes = PairingCodes::default();
        let code = codes.insert("tok".into(), Duration::from_secs(60));
        assert_eq!(code.len(), PAIRING_CODE_LEN);
        assert!(code.bytes().all(|b| PAIRING_ALPHABET.contains(&b)));
        assert_eq!(
            codes.redeem(&code.to_lowercase()),
            Redeem::Token("tok".into())
        );
        assert_eq!(codes.redeem(&code), Redeem::Unknown);
        let stale = codes.insert("tok".into(), Duration::ZERO);
        assert_eq!(codes.redeem(&stale), Redeem::Expired);
    }

    #[test]
    fn guessing_discards_pending_codes() {
        let mut codes = PairingCodes::default();
        let code = codes.insert("tok".into(), Duration::from_secs(60));
        let wrong = if code == "00000" { "11111" } else { "00000" };
        for _ in 0..MAX_FAILED_REDEEMS {
            assert_eq!(codes.redeem(wrong), Redeem::Unknown);
        }
        assert_eq!(codes.redeem(&code), Redeem::Unknown);
    }
}
