//! Deterministic markdown rendering for `recall` / `context` (inventory §6).

use std::fmt::Write;

use memoricai_core::dto::MemorySearchResult;
use memoricai_core::model::Profile;

use crate::MAX_RECALL_CHARS;

pub fn profile_section(profile: &Profile) -> String {
    let mut out = String::from("## User Profile\n");
    let mut any = false;
    if let Some(statics) = &profile.r#static {
        for s in statics {
            out.push_str("- ");
            out.push_str(s);
            out.push('\n');
            any = true;
        }
    }
    if let Some(dynamic) = &profile.dynamic {
        for d in dynamic {
            out.push_str("- ");
            out.push_str(d);
            out.push('\n');
            any = true;
        }
    }
    if !any {
        out.push_str("_(no profile yet)_\n");
    }
    out
}

pub fn recall_markdown(profile: Option<&Profile>, results: &[MemorySearchResult]) -> String {
    let mut out = String::new();
    if let Some(p) = profile {
        out.push_str(&profile_section(p));
        out.push('\n');
    }
    out.push_str("## Relevant Memories\n");
    if results.is_empty() {
        out.push_str("_(no matching memories)_\n");
    } else {
        for (i, r) in results.iter().enumerate() {
            let text = r.memory.as_deref().or(r.chunk.as_deref()).unwrap_or("");
            let _ = writeln!(out, "{}. [{:.2}] {}", i + 1, r.similarity, text);
        }
    }
    if out.len() > MAX_RECALL_CHARS {
        let mut end = MAX_RECALL_CHARS;
        while !out.is_char_boundary(end) {
            end -= 1;
        }
        out.truncate(end);
    }
    out
}
