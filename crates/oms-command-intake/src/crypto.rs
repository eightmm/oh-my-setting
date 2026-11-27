//! HMAC-SHA256 verification, timestamp-window check, and redacted secret storage.

use crate::source::EnvVarSpec;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt;
use std::hint::black_box;
use std::time::{SystemTime, UNIX_EPOCH};

const BLOCK_LEN: usize = 64;
pub const TIMESTAMP_WINDOW_MS: u64 = 60_000;

/// In-memory credential wrapper that redacts `Debug` output and zeroes bytes on drop.
#[derive(Clone)]
pub struct SecretValue {
    bytes: Vec<u8>,
}

impl SecretValue {
    pub fn from_env(var_name: &str) -> Result<Self, String> {
        let Some(raw) = std::env::var_os(var_name) else {
            return Err(format!("missing required environment variable {var_name}"));
        };
        let Ok(text) = raw.into_string() else {
            return Err(format!(
                "invalid environment variable {var_name}: must be valid UTF-8"
            ));
        };
        Self::parse_checked(var_name, text)
    }

    pub fn parse_checked(label: &str, mut text: String) -> Result<Self, String> {
        let invalid = text.is_empty() || text.chars().any(|c| c.is_whitespace() || c.is_control());
        if invalid {
            let mut raw = text.into_bytes();
            raw.fill(0);
            black_box(&raw);
            return Err(format!(
                "invalid {label}: must be non-empty without whitespace or control characters"
            ));
        }
        let bytes = std::mem::take(&mut text).into_bytes();
        Ok(Self { bytes })
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.bytes).expect("validated UTF-8 credential")
    }
}

impl fmt::Debug for SecretValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}

impl Drop for SecretValue {
    fn drop(&mut self) {
        self.bytes.fill(0);
        black_box(&self.bytes);
    }
}

/// Named collection of environment-sourced secrets for an intake adapter.
#[derive(Clone, Debug, Default)]
pub struct SecretStore {
    entries: BTreeMap<String, SecretValue>,
}

impl SecretStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_env_specs(specs: &[EnvVarSpec]) -> Result<Self, String> {
        let mut store = Self::new();
        for spec in specs {
            if !spec.sensitive {
                continue;
            }
            match std::env::var_os(spec.name) {
                None => {
                    if spec.required {
                        return Err(format!(
                            "missing required environment variable {}",
                            spec.name
                        ));
                    }
                }
                Some(raw) => {
                    let Ok(text) = raw.into_string() else {
                        return Err(format!(
                            "invalid environment variable {}: must be valid UTF-8",
                            spec.name
                        ));
                    };
                    let val = SecretValue::parse_checked(spec.name, text)?;
                    store.entries.insert(spec.name.to_owned(), val);
                }
            }
        }
        Ok(store)
    }

    pub fn insert(&mut self, name: impl Into<String>, value: SecretValue) {
        self.entries.insert(name.into(), value);
    }

    pub fn get(&self, name: &str) -> Option<&SecretValue> {
        self.entries.get(name)
    }

    pub fn entries(&self) -> impl Iterator<Item = (&str, &SecretValue)> {
        self.entries.iter().map(|(k, v)| (k.as_str(), v))
    }
}

/// RFC 2104 / RFC 4231 HMAC-SHA256 over `sha2::Sha256`.
pub fn hmac_sha256(signing_material: &[u8], message: &[u8]) -> [u8; 32] {
    let mut key_block = [0u8; BLOCK_LEN];
    if signing_material.len() > BLOCK_LEN {
        let hashed = Sha256::digest(signing_material);
        key_block[..32].copy_from_slice(&hashed);
    } else {
        key_block[..signing_material.len()].copy_from_slice(signing_material);
    }

    let mut ipad = [0x36u8; BLOCK_LEN];
    let mut opad = [0x5cu8; BLOCK_LEN];
    for i in 0..BLOCK_LEN {
        ipad[i] ^= key_block[i];
        opad[i] ^= key_block[i];
    }

    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(message);
    let inner_digest = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner_digest);
    let out: [u8; 32] = outer.finalize().into();

    key_block.fill(0);
    ipad.fill(0);
    opad.fill(0);
    black_box((&key_block, &ipad, &opad));

    out
}

pub fn hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{b:02x}");
    }
    out
}

pub fn hmac_sha256_hex(signing_material: &[u8], message: &[u8]) -> String {
    hex_lower(&hmac_sha256(signing_material, message))
}

fn decode_lowercase_hex_32(hex: &str) -> Option<[u8; 32]> {
    let raw = hex.as_bytes();
    if raw.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for i in 0..32 {
        let hi = decode_lower_nibble(raw[i * 2])?;
        let lo = decode_lower_nibble(raw[i * 2 + 1])?;
        out[i] = (hi << 4) | lo;
    }
    Some(out)
}

fn decode_lower_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        _ => None,
    }
}

/// Constant-time 32-byte digest comparison.
pub fn ct_eq_32(left: &[u8; 32], right: &[u8; 32]) -> bool {
    let mut diff = 0u8;
    for i in 0..32 {
        diff |= left[i] ^ right[i];
    }
    black_box(diff) == 0
}

/// Verify a lowercase hex HMAC-SHA256 digest in constant time.
pub fn verify_hmac_sha256_hex(
    signing_material: &[u8],
    message: &[u8],
    signature_hex: &str,
) -> bool {
    let trimmed = signature_hex.trim_matches([' ', '\t']);
    let Some(provided) = decode_lowercase_hex_32(trimmed) else {
        return false;
    };
    let expected = hmac_sha256(signing_material, message);
    ct_eq_32(&provided, &expected)
}

pub fn current_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

pub fn within_timestamp_window_ms(now_ms: i64, event_ts_ms: i64, window_ms: u64) -> bool {
    now_ms.abs_diff(event_ts_ms) <= window_ms
}
