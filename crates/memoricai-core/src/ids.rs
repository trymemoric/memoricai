//! Prefixed id generation. Entity ids use an alphanumeric alphabet so the
//! `mc_<orgId>_<rand>` key format can be split unambiguously on `_`.

use nanoid::nanoid;

const ALPHANUM: [char; 62] = [
    '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', 'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i',
    'j', 'k', 'l', 'm', 'n', 'o', 'p', 'q', 'r', 's', 't', 'u', 'v', 'w', 'x', 'y', 'z', 'A', 'B',
    'C', 'D', 'E', 'F', 'G', 'H', 'I', 'J', 'K', 'L', 'M', 'N', 'O', 'P', 'Q', 'R', 'S', 'T', 'U',
    'V', 'W', 'X', 'Y', 'Z',
];

/// Random alphanumeric token of `len` characters.
pub fn token(len: usize) -> String {
    nanoid!(len, &ALPHANUM)
}

fn mk_id(prefix: &str) -> String {
    format!("{prefix}{}", token(21))
}

pub fn document_id() -> String {
    mk_id("doc_")
}
pub fn memory_id() -> String {
    mk_id("mem_")
}
pub fn chunk_id() -> String {
    mk_id("chunk_")
}
pub fn project_id() -> String {
    mk_id("proj_")
}
pub fn user_id() -> String {
    mk_id("user_")
}
pub fn org_id() -> String {
    mk_id("org_")
}
pub fn api_key_id() -> String {
    mk_id("key_")
}
pub fn connection_id() -> String {
    mk_id("conn_")
}
pub fn sync_run_id() -> String {
    mk_id("sync_")
}
pub fn request_id() -> String {
    mk_id("req_")
}
pub fn batch_id() -> String {
    mk_id("fb_")
}
pub fn embedding_index_id() -> String {
    mk_id("eidx_")
}

/// Mint an org-scoped API key display string: `mc_<orgId>_<random>`.
/// `org_id` is used verbatim after stripping any `org_` prefix so the key stays parseable.
pub fn org_api_key(org_id: &str) -> String {
    let short = org_id.strip_prefix("org_").unwrap_or(org_id);
    format!("mc_{short}_{}", token(32))
}
