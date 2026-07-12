//! At-rest encryption for provider credentials (connector OAuth tokens etc.).
//!
//! A 32-byte key is read from `MEMORICAI_ENCRYPTION_KEY` (base64 or hex). When set,
//! [`encrypt`] wraps values as `enc:v1:<base64(nonce||ciphertext)>` with AES-256-GCM;
//! [`decrypt`] reverses it. When the key is absent (e.g. local dev) values are stored
//! verbatim, and [`decrypt`] passes through anything without the `enc:v1:` prefix, so
//! legacy plaintext rows keep working after a key is introduced.

use aes_gcm::aead::{Aead, KeyInit, OsRng};
use aes_gcm::{AeadCore, Aes256Gcm, Key, Nonce};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;

const PREFIX: &str = "enc:v1:";

fn key_bytes() -> Option<[u8; 32]> {
    let raw = std::env::var("MEMORICAI_ENCRYPTION_KEY").ok()?;
    let raw = raw.trim();
    let decoded = STANDARD
        .decode(raw)
        .ok()
        .or_else(|| STANDARD.decode(raw.trim_end_matches('=')).ok())
        .or_else(|| decode_hex(raw));
    let bytes = decoded?;
    if bytes.len() != 32 {
        tracing::warn!("MEMORICAI_ENCRYPTION_KEY must decode to 32 bytes; ignoring");
        return None;
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    Some(out)
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

/// Encrypt a credential for storage. If no key is configured, returns it unchanged.
pub fn encrypt(plaintext: &str) -> String {
    let Some(key) = key_bytes() else {
        return plaintext.to_string();
    };
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key));
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    match cipher.encrypt(&nonce, plaintext.as_bytes()) {
        Ok(ct) => {
            let mut blob = nonce.to_vec();
            blob.extend_from_slice(&ct);
            format!("{PREFIX}{}", STANDARD.encode(blob))
        }
        Err(_) => plaintext.to_string(),
    }
}

/// Decrypt a stored credential. Values without the `enc:v1:` prefix (legacy plaintext)
/// are returned unchanged.
pub fn decrypt(stored: &str) -> String {
    let Some(rest) = stored.strip_prefix(PREFIX) else {
        return stored.to_string();
    };
    let Some(key) = key_bytes() else {
        return stored.to_string();
    };
    let Ok(blob) = STANDARD.decode(rest) else {
        return stored.to_string();
    };
    if blob.len() < 12 {
        return stored.to_string();
    }
    let (nonce, ct) = blob.split_at(12);
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key));
    match cipher.decrypt(Nonce::from_slice(nonce), ct) {
        Ok(pt) => String::from_utf8_lossy(&pt).into_owned(),
        Err(_) => stored.to_string(),
    }
}

/// Encrypt an optional credential.
pub fn encrypt_opt(plaintext: Option<&str>) -> Option<String> {
    plaintext.map(encrypt)
}

/// Decrypt an optional credential.
pub fn decrypt_opt(stored: Option<String>) -> Option<String> {
    stored.map(|s| decrypt(&s))
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
pub fn encrypt_metadata(value: &mut serde_json::Value) {
    walk_metadata(value, &encrypt, None);
}

/// Reverse of [`encrypt_metadata`].
pub fn decrypt_metadata(value: &mut serde_json::Value) {
    walk_metadata(value, &|s| decrypt(s), None);
}

fn walk_metadata(value: &mut serde_json::Value, f: &dyn Fn(&str) -> String, key: Option<&str>) {
    match value {
        serde_json::Value::Object(map) => {
            for (k, v) in map.iter_mut() {
                walk_metadata(v, f, Some(k));
            }
        }
        serde_json::Value::Array(items) => {
            for v in items.iter_mut() {
                walk_metadata(v, f, key);
            }
        }
        serde_json::Value::String(s) if key.is_some_and(is_sensitive_key) => {
            *s = f(s);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passthrough_without_key() {
        // No MEMORICAI_ENCRYPTION_KEY in the test env: values are stored/read verbatim,
        // and decrypt never mangles a plaintext value.
        assert_eq!(decrypt(&encrypt("ya29.secret-token")), "ya29.secret-token");
        assert_eq!(decrypt("legacy-plaintext"), "legacy-plaintext");
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
        encrypt_metadata(&mut v);
        decrypt_metadata(&mut v);
        // Without a key encryption is a no-op, but non-sensitive fields are never altered
        // and sensitive ones round-trip.
        assert_eq!(v["bucket"], "public-bucket");
        assert_eq!(v["region"], "us-east-1");
        assert_eq!(v["secretAccessKey"], "abc");
        assert_eq!(v["apiKey"], "xyz");
    }
}
