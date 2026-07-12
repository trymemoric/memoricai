//! OAuth2/OIDC provider helpers: PKCE verification and token generation.

use base64::Engine;
use sha2::{Digest, Sha256};

/// Verify a PKCE `code_verifier` against the stored `code_challenge`.
/// Supports only `S256`.
pub fn verify_pkce(verifier: &str, challenge: &str, method: Option<&str>) -> bool {
    match method.unwrap_or("S256") {
        "S256" => {
            let digest = Sha256::digest(verifier.as_bytes());
            let computed = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest);
            computed == challenge
        }
        _ => false,
    }
}

/// A URL-safe opaque token.
pub fn opaque_token() -> String {
    memoricai_core::ids::token(40)
}
