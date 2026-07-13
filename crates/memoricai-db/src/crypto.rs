//! At-rest encryption for provider credentials (connector OAuth tokens etc.).
//!
//! A 32-byte key is read from `MEMORICAI_ENCRYPTION_KEY` (base64 or hex). When set,
//! [`encrypt`] wraps values as `enc:v1:<base64(nonce||ciphertext)>` with AES-256-GCM;
//! [`decrypt`] reverses it. Local development may explicitly run without a key, but a
//! configured invalid key always fails and production requires a valid key. Encrypted
//! values never fall back to ciphertext or plaintext on cryptographic failure.

use aes_gcm::aead::{Aead, KeyInit, OsRng};
use aes_gcm::{AeadCore, Aes256Gcm, Key, Nonce};
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};
use base64::Engine;
use memoricai_core::error::{Error, Result};

const PREFIX: &str = "enc:v1:";

fn key_bytes() -> Result<Option<[u8; 32]>> {
    let Some(raw) = std::env::var("MEMORICAI_ENCRYPTION_KEY")
        .ok()
        .filter(|value| !value.trim().is_empty())
    else {
        return Ok(None);
    };
    let raw = raw.trim();
    let decoded = if raw.len() == 64 && raw.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        decode_hex(raw)
    } else {
        STANDARD
            .decode(raw)
            .ok()
            .or_else(|| STANDARD_NO_PAD.decode(raw).ok())
    };
    let bytes = decoded.ok_or_else(|| {
        Error::Internal("MEMORICAI_ENCRYPTION_KEY must be 32 bytes encoded as base64 or hex".into())
    })?;
    if bytes.len() != 32 {
        return Err(Error::Internal(
            "MEMORICAI_ENCRYPTION_KEY must decode to exactly 32 bytes".into(),
        ));
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    Ok(Some(out))
}

fn decode_hex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

/// Validate encryption configuration before opening the service. The caller supplies the
/// resolved production mode; encryption can also be required explicitly with
/// `MEMORICAI_REQUIRE_ENCRYPTION=true`.
pub fn validate_configuration(production: bool) -> Result<()> {
    let required = std::env::var("MEMORICAI_REQUIRE_ENCRYPTION")
        .ok()
        .is_some_and(|value| matches!(value.to_ascii_lowercase().as_str(), "1" | "true" | "yes"));
    let configured = key_bytes()?.is_some();
    if (production || required) && !configured {
        return Err(Error::Internal(
            "MEMORICAI_ENCRYPTION_KEY is required in production".into(),
        ));
    }
    if !configured {
        tracing::warn!(
            "connector credentials will use plaintext storage in development; set \
             MEMORICAI_ENCRYPTION_KEY before storing real credentials"
        );
    }
    Ok(())
}

pub fn is_encrypted(value: &str) -> bool {
    value.starts_with(PREFIX)
}

/// Encrypt a credential for storage. Local development without a configured key stores
/// plaintext; callers must run [`validate_configuration`] at startup.
pub fn encrypt(plaintext: &str) -> Result<String> {
    if is_encrypted(plaintext) {
        return Ok(plaintext.to_string());
    }
    let Some(key) = key_bytes()? else {
        return Ok(plaintext.to_string());
    };
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key));
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let ciphertext = cipher
        .encrypt(&nonce, plaintext.as_bytes())
        .map_err(|_| Error::Internal("credential encryption failed".into()))?;
    let mut blob = nonce.to_vec();
    blob.extend_from_slice(&ciphertext);
    Ok(format!("{PREFIX}{}", STANDARD.encode(blob)))
}

/// Decrypt a stored credential. Development plaintext values without the `enc:v1:` prefix
/// are returned unchanged.
pub fn decrypt(stored: &str) -> Result<String> {
    let Some(rest) = stored.strip_prefix(PREFIX) else {
        return Ok(stored.to_string());
    };
    let Some(key) = key_bytes()? else {
        return Err(Error::Internal(
            "encrypted credentials exist but MEMORICAI_ENCRYPTION_KEY is unavailable".into(),
        ));
    };
    let blob = STANDARD
        .decode(rest)
        .map_err(|_| Error::Internal("stored credential is not valid base64".into()))?;
    if blob.len() < 12 {
        return Err(Error::Internal(
            "stored credential ciphertext is truncated".into(),
        ));
    }
    let (nonce, ct) = blob.split_at(12);
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key));
    let plaintext = cipher
        .decrypt(Nonce::from_slice(nonce), ct)
        .map_err(|_| Error::Internal("credential decryption failed".into()))?;
    String::from_utf8(plaintext)
        .map_err(|_| Error::Internal("decrypted credential is not valid UTF-8".into()))
}

/// Encrypt an optional credential.
pub fn encrypt_opt(plaintext: Option<&str>) -> Result<Option<String>> {
    plaintext.map(encrypt).transpose()
}

/// Decrypt an optional credential.
pub fn decrypt_opt(stored: Option<String>) -> Result<Option<String>> {
    stored.map(|value| decrypt(&value)).transpose()
}

/// One-way hash for opaque high-entropy credentials that are only ever *verified* by
/// equality (OAuth access/refresh tokens, confidential client secrets). Deterministic
/// SHA-256 hex so lookups/comparisons still work while the plaintext is never stored.
pub fn hash_token(token: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(token.as_bytes());
    let mut out = String::with_capacity(64);
    for b in digest {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// True if a metadata key names a sensitive value (mirrors the API redaction heuristic).
fn is_sensitive_key(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    k.contains("secret") || k.contains("key") || k.contains("token") || k.contains("password")
}

/// Encrypt sensitive string fields inside a connection-metadata JSON object in place
/// (e.g. S3 `secretAccessKey`/`accessKeyId`, Granola `apiKey`). No-op without a key.
pub fn encrypt_metadata(value: &mut serde_json::Value) -> Result<()> {
    walk_metadata(value, &encrypt, None)
}

/// Reverse of [`encrypt_metadata`].
pub fn decrypt_metadata(value: &mut serde_json::Value) -> Result<()> {
    walk_metadata(value, &decrypt, None)
}

fn walk_metadata(
    value: &mut serde_json::Value,
    f: &dyn Fn(&str) -> Result<String>,
    key: Option<&str>,
) -> Result<()> {
    match value {
        serde_json::Value::Object(map) => {
            for (k, v) in map.iter_mut() {
                walk_metadata(v, f, Some(k))?;
            }
        }
        serde_json::Value::Array(items) => {
            for v in items.iter_mut() {
                walk_metadata(v, f, key)?;
            }
        }
        serde_json::Value::String(s) if key.is_some_and(is_sensitive_key) => {
            *s = f(s)?;
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passthrough_without_key() {
        // No MEMORICAI_ENCRYPTION_KEY in the test env: values are stored/read verbatim,
        // and decrypt never mangles a plaintext value.
        let encrypted = encrypt("ya29.secret-token").unwrap();
        assert_eq!(decrypt(&encrypted).unwrap(), "ya29.secret-token");
        assert_eq!(
            decrypt("development-plaintext").unwrap(),
            "development-plaintext"
        );
    }

    #[test]
    fn hash_token_is_deterministic_hex() {
        let h = hash_token("mc-secret");
        assert_eq!(h, hash_token("mc-secret"));
        assert_ne!(h, hash_token("other"));
        assert_eq!(h.len(), 64);
        assert!(h.bytes().all(|b| b.is_ascii_hexdigit()));
    }

    #[test]
    fn metadata_only_touches_sensitive_keys() {
        let mut v = serde_json::json!({
            "bucket": "public-bucket",
            "region": "us-east-1",
            "secretAccessKey": "abc",
            "apiKey": "xyz",
        });
        encrypt_metadata(&mut v).unwrap();
        decrypt_metadata(&mut v).unwrap();
        // Without a key encryption is a no-op, but non-sensitive fields are never altered
        // and sensitive ones round-trip.
        assert_eq!(v["bucket"], "public-bucket");
        assert_eq!(v["region"], "us-east-1");
        assert_eq!(v["secretAccessKey"], "abc");
        assert_eq!(v["apiKey"], "xyz");
    }
}
