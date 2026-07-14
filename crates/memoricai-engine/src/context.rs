//! Budgeted, source-fair context assembly for `/v1/context`.

use crate::search::is_aggregation_query;
use crate::Engine;
use memoricai_core::dto::{
    ContextDiagnostics, ContextEvidence, ContextOmission, ContextRequest, ContextResponse,
    DocumentSearchRequest, DocumentSearchResult, MemorySearchRequest, SearchInclude,
};
use memoricai_core::error::{Error, Result};
use std::collections::HashSet;
use std::time::Instant;

const CHARS_PER_TOKEN: usize = 4;
const MIN_BUDGET_TOKENS: u32 = 256;
const MAX_BUDGET_TOKENS: u32 = 32_768;
const MAX_SOURCES: u32 = 20;
const MIN_SOURCE_CHARS: usize = 128;
const DIGEST_SHARE: usize = 4;
const DIGEST_HEADING: &str = "Memory digest:\n";
const EVIDENCE_HEADING: &str = "Relevant source excerpts:\n";
const EXCERPT_MARKER: &str = " … [excerpt truncated]";
const DIGEST_MARKER: &str = "\n… [digest truncated]";

#[derive(Debug, Clone)]
struct SourceCandidate {
    rank: u32,
    source_id: String,
    document_id: String,
    session_id: Option<String>,
    date: Option<String>,
    score: f32,
    content: String,
}

impl SourceCandidate {
    fn available_chars(&self) -> usize {
        char_len(&self.content)
    }

    fn header(&self) -> String {
        let date = display_field(self.date.as_deref().unwrap_or("unknown"), 64);
        let source = display_field(&self.source_id, 128);
        format!(
            "\n## Source {} | date={} | source={}\n",
            self.rank, date, source
        )
    }
}

impl Engine {
    /// Retrieve memory facts and relevant source excerpts, then compose a bounded
    /// context without ever slicing the final assembled string.
    pub async fn build_context(
        &self,
        org_id: &str,
        req: &ContextRequest,
    ) -> Result<ContextResponse> {
        Self::validate_query(&req.q)?;
        Self::validate_threshold(req.threshold, "threshold")?;
        if !(MIN_BUDGET_TOKENS..=MAX_BUDGET_TOKENS).contains(&req.budget_tokens) {
            return Err(Error::BadRequest(format!(
                "budgetTokens must be between {MIN_BUDGET_TOKENS} and {MAX_BUDGET_TOKENS}"
            )));
        }
        if req.max_sources == 0 || req.max_sources > MAX_SOURCES {
            return Err(Error::BadRequest(format!(
                "maxSources must be between 1 and {MAX_SOURCES}"
            )));
        }
        if let Some(tag) = req.container_tag.as_deref() {
            if !memoricai_core::is_valid_container_tag(tag) {
                return Err(Error::BadRequest("invalid container tag".into()));
            }
        }

        let aggregate = match req.mode.as_str() {
            "auto" => is_aggregation_query(&req.q),
            "lookup" => false,
            "aggregation" => true,
            _ => {
                return Err(Error::BadRequest(
                    "mode must be auto, lookup, or aggregation".into(),
                ))
            }
        };
        let resolved_mode = if aggregate { "aggregation" } else { "lookup" };
        let start = Instant::now();

        // Run memory retrieval first. Document retrieval then reuses the cached
        // query embedding instead of making a second provider round trip.
        let digest = if req.include_digest {
            let memory_request = MemorySearchRequest {
                q: req.q.clone(),
                container_tag: req.container_tag.clone(),
                search_mode: "memories".to_string(),
                limit: 20,
                threshold: req.threshold,
                rerank: false,
                rewrite_query: req.rewrite_query,
                filters: req.filters.clone(),
                include: SearchInclude::default(),
                digest: true,
            };
            self.search_memories_for_context(org_id, &memory_request, aggregate)
                .await?
                .digest
        } else {
            None
        };

        let multiplier = if aggregate { 3 } else { 2 };
        let candidate_limit = req
            .max_sources
            .saturating_mul(multiplier)
            .max(if aggregate { 30 } else { 20 })
            .min(100);
        let document_request = DocumentSearchRequest {
            q: req.q.clone(),
            limit: candidate_limit,
            container_tags: req.container_tag.clone().map(|tag| vec![tag]),
            filters: req.filters.clone(),
            rerank: false,
            rewrite_query: req.rewrite_query,
            chunk_threshold: req.threshold,
            document_threshold: req.threshold,
            doc_id: None,
            include_full_docs: false,
            include_summary: false,
        };
        let documents = self.search_documents(org_id, &document_request).await?;
        let mut candidates = documents
            .results
            .into_iter()
            .enumerate()
            .map(|(index, result)| source_candidate(index as u32 + 1, result))
            .collect::<Vec<_>>();
        let mut seen_sources = HashSet::new();
        candidates.retain(|candidate| seen_sources.insert(candidate.source_id.clone()));
        for (index, candidate) in candidates.iter_mut().enumerate() {
            candidate.rank = index as u32 + 1;
        }
        let budget_chars = (req.budget_tokens as usize)
            .checked_mul(CHARS_PER_TOKEN)
            .ok_or_else(|| Error::BadRequest("budgetTokens is too large".into()))?;
        let mut response = pack_context(
            digest.as_deref(),
            candidates,
            budget_chars,
            req.budget_tokens,
            req.max_sources as usize,
            resolved_mode,
            aggregate,
        );
        response.timing = start.elapsed().as_millis() as u64;

        tracing::info!(
            org_id,
            mode = resolved_mode,
            query_bytes = req.q.len(),
            budget_tokens = req.budget_tokens,
            used_chars = response.diagnostics.used_chars,
            estimated_tokens = response.diagnostics.estimated_tokens,
            sources_considered = response.diagnostics.sources_considered,
            sources_included = response.diagnostics.sources_included,
            sources_omitted = response.diagnostics.sources_omitted,
            truncated_sources = response.diagnostics.truncated_sources,
            digest_truncated = response.diagnostics.digest_truncated,
            timing_ms = response.timing,
            "context assembled"
        );
        Ok(response)
    }
}

fn source_candidate(rank: u32, mut result: DocumentSearchResult) -> SourceCandidate {
    result.chunks.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut seen = HashSet::new();
    let passages = result
        .chunks
        .into_iter()
        .filter(|chunk| chunk.is_relevant)
        .map(|chunk| chunk.content.trim().to_string())
        .filter(|content| !content.is_empty() && seen.insert(content.clone()))
        .collect::<Vec<_>>();
    let session_id = metadata_string(&result.metadata, "sessionId", 512);
    let date = metadata_string(&result.metadata, "date", 64);
    let source_id = session_id
        .clone()
        .unwrap_or_else(|| result.document_id.clone());
    SourceCandidate {
        rank,
        source_id,
        document_id: result.document_id,
        session_id,
        date,
        score: result.score,
        content: passages.join("\n\n"),
    }
}

#[allow(clippy::too_many_arguments)]
fn pack_context(
    digest: Option<&str>,
    candidates: Vec<SourceCandidate>,
    budget_chars: usize,
    budget_tokens: u32,
    max_sources: usize,
    mode: &str,
    aggregate: bool,
) -> ContextResponse {
    let mut context = String::new();
    let mut digest_truncated = false;
    let mut packed_digest = None;

    if let Some(digest) = digest.filter(|value| !value.trim().is_empty()) {
        let fixed_chars = char_len(DIGEST_HEADING) + 2 + char_len(EVIDENCE_HEADING);
        let allocation =
            (budget_chars / DIGEST_SHARE).min(budget_chars.saturating_sub(fixed_chars));
        let (included, truncated) = truncate_with_marker(digest.trim(), allocation, DIGEST_MARKER);
        if !included.is_empty() {
            context.push_str(DIGEST_HEADING);
            context.push_str(&included);
            context.push_str("\n\n");
            packed_digest = Some(included);
            digest_truncated = truncated;
        }
    }
    context.push_str(EVIDENCE_HEADING);

    let mut reasons: Vec<Option<&'static str>> = vec![None; candidates.len()];
    let mut selected = Vec::new();
    let static_chars = char_len(&context);
    let mut header_chars = 0;
    let mut minimum_content_chars = 0;
    for (index, candidate) in candidates.iter().enumerate() {
        if candidate.content.is_empty() {
            reasons[index] = Some("noContent");
            continue;
        }
        if selected.len() >= max_sources {
            reasons[index] = Some("sourceLimit");
            continue;
        }
        let minimum = candidate.available_chars().min(MIN_SOURCE_CHARS);
        let header = char_len(&candidate.header());
        let required = static_chars
            .saturating_add(header_chars)
            .saturating_add(minimum_content_chars)
            .saturating_add(header)
            .saturating_add(minimum);
        if required > budget_chars {
            reasons[index] = Some("budget");
            continue;
        }
        header_chars += header;
        minimum_content_chars += minimum;
        selected.push(index);
    }

    let content_budget = budget_chars.saturating_sub(static_chars + header_chars);
    let capacities = selected
        .iter()
        .map(|index| candidates[*index].available_chars())
        .collect::<Vec<_>>();
    let allocations = fair_allocations(&capacities, content_budget);
    let mut included_content: Vec<Option<String>> = vec![None; candidates.len()];
    let mut truncated: Vec<bool> = vec![false; candidates.len()];
    for (selected_index, allocation) in selected.iter().zip(allocations) {
        let candidate = &candidates[*selected_index];
        if allocation == 0 {
            reasons[*selected_index] = Some("budget");
            continue;
        }
        let (content, was_truncated) =
            truncate_with_marker(&candidate.content, allocation, EXCERPT_MARKER);
        if content.is_empty() {
            reasons[*selected_index] = Some("budget");
            continue;
        }
        context.push_str(&candidate.header());
        context.push_str(&content);
        included_content[*selected_index] = Some(content);
        truncated[*selected_index] = was_truncated;
    }

    debug_assert!(char_len(&context) <= budget_chars);
    let mut omissions = Vec::new();
    let evidence = candidates
        .into_iter()
        .enumerate()
        .map(|(index, candidate)| {
            let content = included_content[index].take();
            let included = content.is_some();
            let omission_reason =
                (!included).then(|| reasons[index].unwrap_or("budget").to_string());
            if let Some(reason) = omission_reason.as_deref() {
                omissions.push(ContextOmission {
                    rank: candidate.rank,
                    source_id: candidate.source_id.clone(),
                    document_id: candidate.document_id.clone(),
                    reason: reason.to_string(),
                });
            }
            let included_chars = content.as_deref().map(char_len).unwrap_or_default();
            let available_chars = candidate.available_chars();
            ContextEvidence {
                rank: candidate.rank,
                source_id: candidate.source_id,
                document_id: candidate.document_id,
                session_id: candidate.session_id,
                date: candidate.date,
                score: candidate.score,
                included,
                available_chars,
                included_chars,
                truncated: truncated[index],
                omission_reason,
                content,
            }
        })
        .collect::<Vec<_>>();

    let used_chars = char_len(&context);
    let digest_chars = packed_digest.as_deref().map(char_len).unwrap_or_default();
    let evidence_chars = evidence.iter().map(|item| item.included_chars).sum();
    let sources_included = evidence.iter().filter(|item| item.included).count();
    let sources_selected = evidence
        .iter()
        .filter(|item| item.included || item.omission_reason.as_deref() == Some("budget"))
        .count();
    let truncated_sources = evidence.iter().filter(|item| item.truncated).count();
    ContextResponse {
        context,
        digest: packed_digest,
        evidence,
        diagnostics: ContextDiagnostics {
            mode: mode.to_string(),
            aggregation_query: aggregate,
            budget_tokens,
            budget_chars,
            used_chars,
            estimated_tokens: used_chars.div_ceil(CHARS_PER_TOKEN),
            digest_chars,
            evidence_chars,
            sources_considered: reasons.len(),
            sources_selected,
            sources_included,
            sources_omitted: omissions.len(),
            truncated_sources,
            digest_truncated,
            hard_truncated: false,
            omissions,
        },
        timing: 0,
    }
}

fn fair_allocations(capacities: &[usize], budget: usize) -> Vec<usize> {
    let mut allocations = vec![0; capacities.len()];
    let mut remaining = budget.min(capacities.iter().sum());
    while remaining > 0 {
        let active = capacities
            .iter()
            .enumerate()
            .filter(|(index, capacity)| allocations[*index] < **capacity)
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        if active.is_empty() {
            break;
        }
        let share = remaining / active.len();
        let extra = remaining % active.len();
        let mut distributed = 0;
        for (position, index) in active.into_iter().enumerate() {
            let fair_share = share + usize::from(position < extra);
            let amount = (capacities[index] - allocations[index])
                .min(fair_share)
                .min(remaining - distributed);
            allocations[index] += amount;
            distributed += amount;
            if distributed == remaining {
                break;
            }
        }
        if distributed == 0 {
            break;
        }
        remaining -= distributed;
    }
    allocations
}

fn metadata_string(metadata: &serde_json::Value, key: &str, max_chars: usize) -> Option<String> {
    metadata
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.chars().take(max_chars).collect())
}

fn display_field(value: &str, max_chars: usize) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .take(max_chars)
        .collect::<String>()
        .trim()
        .to_string()
}

fn truncate_with_marker(text: &str, max_chars: usize, marker: &str) -> (String, bool) {
    if char_len(text) <= max_chars {
        return (text.to_string(), false);
    }
    if max_chars == 0 {
        return (String::new(), true);
    }
    let marker_chars = char_len(marker);
    if max_chars <= marker_chars {
        return (marker.chars().take(max_chars).collect(), true);
    }
    let target = max_chars - marker_chars;
    let mut prefix = text.chars().take(target).collect::<String>();
    if let Some((byte_index, _)) = prefix.char_indices().rev().find(|(_, c)| c.is_whitespace()) {
        if prefix[..byte_index].chars().count() >= target.saturating_mul(3) / 4 {
            prefix.truncate(byte_index);
        }
    }
    let mut output = prefix.trim_end().to_string();
    output.push_str(marker);
    (output, true)
}

fn char_len(value: &str) -> usize {
    value.chars().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(rank: u32, source: &str, chars: usize) -> SourceCandidate {
        SourceCandidate {
            rank,
            source_id: source.to_string(),
            document_id: format!("doc-{rank}"),
            session_id: Some(source.to_string()),
            date: Some(format!("2025-01-{rank:02}")),
            score: 1.0 - (rank as f32 / 100.0),
            content: std::iter::repeat_n('x', chars).collect(),
        }
    }

    #[test]
    fn packer_represents_sources_fairly_within_budget() {
        let response = pack_context(
            Some(&"fact ".repeat(300)),
            vec![
                candidate(1, "session-1", 5_000),
                candidate(2, "session-2", 5_000),
                candidate(3, "session-3", 5_000),
                candidate(4, "session-4", 5_000),
            ],
            4_000,
            1_000,
            4,
            "aggregation",
            true,
        );

        assert!(char_len(&response.context) <= 4_000);
        assert_eq!(response.diagnostics.sources_included, 4);
        assert!(response.evidence.iter().all(|item| item.included));
        let included = response
            .evidence
            .iter()
            .map(|item| item.included_chars)
            .collect::<Vec<_>>();
        assert!(included.iter().all(|chars| *chars >= MIN_SOURCE_CHARS));
        assert!(
            included.iter().max().unwrap() - included.iter().min().unwrap() <= 1,
            "allocations were not fair: {included:?}"
        );
        assert!(!response.diagnostics.hard_truncated);
    }

    #[test]
    fn packer_reports_source_limit_and_empty_content() {
        let mut empty = candidate(3, "session-3", 0);
        empty.content.clear();
        let response = pack_context(
            None,
            vec![
                candidate(1, "session-1", 200),
                candidate(2, "session-2", 200),
                empty,
            ],
            2_000,
            500,
            1,
            "lookup",
            false,
        );

        assert_eq!(response.diagnostics.sources_included, 1);
        assert_eq!(
            response.evidence[1].omission_reason.as_deref(),
            Some("sourceLimit")
        );
        assert_eq!(
            response.evidence[2].omission_reason.as_deref(),
            Some("noContent")
        );
        assert_eq!(response.diagnostics.omissions.len(), 2);
    }

    #[test]
    fn truncation_is_unicode_safe_and_explicit() {
        let input = "東京でコーヒーを飲みました。".repeat(20);
        let (output, truncated) = truncate_with_marker(&input, 80, EXCERPT_MARKER);
        assert!(truncated);
        assert!(char_len(&output) <= 80);
        assert!(output.ends_with(EXCERPT_MARKER));
    }

    #[test]
    fn fair_allocation_redistributes_unused_capacity() {
        assert_eq!(fair_allocations(&[10, 100, 100], 100), vec![10, 45, 45]);
    }
}
