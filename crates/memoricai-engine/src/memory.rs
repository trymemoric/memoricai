//! Memory extraction, relation inference, version chaining, forgetting, and
//! profile building. The deterministic graph bookkeeping is exact; the LLM
//! stages tolerate malformed model output by dropping invalid facts.

use crate::Engine;
use memoricai_core::dto::{ForgetRequest, PatchMemoryRequest};
use memoricai_core::enums::MemoryRelation;
use memoricai_core::error::{Error, Result};
use memoricai_core::model::{Memory, OrgSettings, Profile, Timestamp};
use memoricai_core::ports::CONTENT_MARKER;
use memoricai_core::ports::{ChatMessage, ChatOptions};
use serde::Deserialize;

/// A fact proposed by the extractor.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractedFact {
    pub content: String,
    #[serde(default)]
    pub is_static: bool,
    #[serde(default)]
    pub forget_after: Option<String>,
    /// When the described event happened (ISO date), if the fact is about a
    /// dated occurrence. Distinct from the conversation/document date.
    #[serde(default)]
    pub event_date: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ExtractionOut {
    #[serde(default)]
    memories: Vec<ExtractedFact>,
}

// Similarity thresholds for relation inference (design.md §5).
const UPDATE_THRESHOLD: f32 = 0.97;
const EXTEND_THRESHOLD: f32 = 0.85;

impl Engine {
    /// LLM fact extraction. Returns candidate facts (possibly empty).
    pub async fn extract_memories(
        &self,
        text: &str,
        entity_context: Option<&str>,
        settings: Option<&OrgSettings>,
    ) -> Result<Vec<ExtractedFact>> {
        let mut system_prompt = String::from(
            "You extract atomic, self-contained facts (memories) from the user's content. \
             Respond ONLY with JSON of the form \
             {\"memories\":[{\"content\":string,\"isStatic\":boolean,\"forgetAfter\":string|null,\
             \"eventDate\":string|null}]}. \
             Set isStatic=true for stable identity facts or lasting preferences. \
             Set forgetAfter to an ISO-8601 date if the fact is inherently time-bound, else null. \
             Set eventDate to the ISO-8601 date the described event happened or will happen \
             (the event's own date — resolve relative references like 'yesterday' against any \
             stated conversation date), or null if the fact is not tied to a specific date. \
             Extract nothing (empty array) for pure noise.",
        );
        if let Some(settings) = settings.filter(|settings| settings.should_llm_filter) {
            if let Some(prompt) = &settings.filter_prompt {
                system_prompt.push_str("\nAdditional filtering policy: ");
                system_prompt.push_str(prompt);
            }
            if let Some(categories) = &settings.categories {
                system_prompt.push_str("\nRelevant categories: ");
                system_prompt.push_str(&categories.join(", "));
            }
            if let Some(include) = &settings.include_items {
                system_prompt.push_str("\nOnly retain facts matching: ");
                system_prompt.push_str(&include.join(", "));
            }
            if let Some(exclude) = &settings.exclude_items {
                system_prompt.push_str("\nNever retain facts matching: ");
                system_prompt.push_str(&exclude.join(", "));
            }
        }
        let system = ChatMessage::system(system_prompt);
        let mut user = String::new();
        if let Some(ctx) = entity_context {
            user.push_str("Entity context: ");
            user.push_str(ctx);
            user.push_str("\n\n");
        }
        user.push_str(CONTENT_MARKER);
        user.push('\n');
        user.push_str(text);

        let opts = ChatOptions {
            temperature: Some(0.1),
            json: true,
            ..Default::default()
        };
        let raw = self
            .models
            .llm
            .complete(vec![system, ChatMessage::user(user)], opts)
            .await?;
        let parsed: ExtractionOut = serde_json::from_str(raw.trim())
            .or_else(|_| serde_json::from_str(extract_json_block(&raw)))
            .map_err(|error| {
                Error::Model(format!("invalid memory extraction response: {error}"))
            })?;
        let mut memories = parsed.memories;
        if memories.len() > 100 {
            tracing::warn!(
                count = memories.len(),
                "memory extraction returned more than 100 facts; keeping the first 100"
            );
            memories.truncate(100);
        }
        let proposed = memories.len();
        memories.retain(|fact| {
            fact.content.len() <= 10 * 1024
                && fact
                    .forget_after
                    .as_deref()
                    .is_none_or(|value| parse_iso_date(value).is_some())
        });
        // An unparseable eventDate nulls the date, not the fact.
        for fact in &mut memories {
            if fact
                .event_date
                .as_deref()
                .is_some_and(|value| parse_iso_date(value).is_none())
            {
                fact.event_date = None;
            }
        }
        if memories.len() < proposed {
            tracing::warn!(
                dropped = proposed - memories.len(),
                "dropped extracted facts with oversized content or invalid forgetAfter"
            );
        }
        let mut facts: Vec<ExtractedFact> = memories
            .into_iter()
            .filter(|f| f.content.trim().len() >= 3)
            .collect();
        if let Some(settings) = settings.filter(|settings| settings.should_llm_filter) {
            let includes = settings.include_items.as_deref().unwrap_or(&[]);
            let excludes = settings.exclude_items.as_deref().unwrap_or(&[]);
            facts.retain(|fact| {
                let content = fact.content.to_lowercase();
                let included = includes.is_empty()
                    || includes
                        .iter()
                        .any(|item| content.contains(&item.to_lowercase()));
                let excluded = excludes
                    .iter()
                    .any(|item| content.contains(&item.to_lowercase()));
                included && !excluded
            });
        }
        Ok(facts)
    }

    /// Persist one extracted fact, wiring version chains + relation edges.
    ///
    /// Concurrent ingestion into the same container can race on the version
    /// chain (`replace_latest_memory` refuses to retire a memory that is no
    /// longer latest). That conflict is transient: re-reading the neighbors
    /// picks up the winner's successor, so retry here instead of failing the
    /// whole document job.
    pub async fn store_extracted(
        &self,
        org_id: &str,
        user_id: Option<&str>,
        document_id: &str,
        container_tag: &str,
        fact: &ExtractedFact,
        embedding: &[f32],
    ) -> Result<String> {
        const MAX_ATTEMPTS: u32 = 4;
        let mut attempt = 0;
        loop {
            attempt += 1;
            match self
                .store_extracted_once(org_id, user_id, document_id, container_tag, fact, embedding)
                .await
            {
                Err(Error::Conflict(_)) if attempt < MAX_ATTEMPTS => {
                    tokio::time::sleep(std::time::Duration::from_millis(10 * attempt as u64)).await;
                }
                other => return other,
            }
        }
    }

    async fn store_extracted_once(
        &self,
        org_id: &str,
        user_id: Option<&str>,
        document_id: &str,
        container_tag: &str,
        fact: &ExtractedFact,
        embedding: &[f32],
    ) -> Result<String> {
        crate::validate_embedding(embedding, self.models.dim())?;
        let embedding_index = self.embedding_index(org_id).await?;
        let now: Timestamp = chrono::Utc::now();
        let new_id = memoricai_core::ids::memory_id();

        // Find the nearest existing memory to decide the relation.
        let neighbors = self
            .db
            .neighbor_memories(
                org_id,
                &embedding_index.id,
                embedding_index.dimension,
                container_tag,
                embedding,
                3,
                &new_id,
            )
            .await?;
        let top = neighbors.first();

        let forget_after = fact.forget_after.as_deref().and_then(parse_iso_date);

        let (version, parent, root, relation, supersede) = match top {
            Some(hit) if hit.similarity >= UPDATE_THRESHOLD => {
                let root = hit
                    .memory
                    .root_memory_id
                    .clone()
                    .unwrap_or_else(|| hit.memory.id.clone());
                (
                    hit.memory.version + 1,
                    Some(hit.memory.id.clone()),
                    Some(root),
                    Some(MemoryRelation::Updates),
                    Some(hit.memory.id.clone()),
                )
            }
            Some(hit) if hit.similarity >= EXTEND_THRESHOLD => {
                (1, None, None, Some(MemoryRelation::Extends), None)
            }
            _ => (1, None, None, None, None),
        };

        let mem = Memory {
            id: new_id.clone(),
            custom_id: None,
            document_id: Some(document_id.to_string()),
            org_id: org_id.to_string(),
            user_id: user_id.map(|s| s.to_string()),
            memory: fact.content.clone(),
            summary: None,
            mem_type: None,
            space_container_tag: container_tag.to_string(),
            version,
            is_latest: true,
            parent_memory_id: parent.clone(),
            root_memory_id: root,
            relation,
            source_count: 1,
            is_static: fact.is_static,
            is_inference: false,
            review_status: None,
            is_forgotten: false,
            forget_reason: None,
            forget_after,
            forget_batch_id: None,
            event_date: fact.event_date.as_deref().and_then(parse_iso_date),
            metadata: serde_json::json!({}),
            created_at: now,
            updated_at: now,
        };
        if let Some(prev_id) = supersede {
            self.db
                .replace_latest_memory(&prev_id, &mem, &embedding_index.id, embedding)
                .await?;
        } else if let (Some(hit), Some(MemoryRelation::Extends)) = (top, relation) {
            self.db
                .insert_memory(&mem, &embedding_index.id, embedding)
                .await?;
            self.db
                .insert_edge(&hit.memory.id, &new_id, MemoryRelation::Extends)
                .await?;
        } else {
            self.db
                .insert_memory(&mem, &embedding_index.id, embedding)
                .await?;
        }
        Ok(new_id)
    }

    /// Build a container's profile from its memories, buckets, and summaries.
    pub async fn build_profile(&self, org_id: &str, container_tag: &str) -> Result<Profile> {
        let (statics, recents, summaries, bucket_defs) = tokio::try_join!(
            self.db.static_memories(org_id, container_tag, 100),
            self.db.recent_memories(org_id, container_tag, 50),
            self.db.get_profile_summaries(org_id, container_tag),
            self.db.list_buckets(org_id, Some(container_tag)),
        )?;

        let static_list: Vec<String> = statics.into_iter().map(|m| m.memory).collect();

        // General summary (bucket_key == None) is surfaced at the top of dynamic.
        let mut dynamic_list: Vec<String> = summaries
            .iter()
            .filter(|(b, _)| b.is_none())
            .map(|(_, s)| format!("[Summary] {s}"))
            .collect();
        dynamic_list.extend(recents.into_iter().enumerate().map(|(i, m)| {
            if i < 3 {
                format!("[Recent] {}", m.memory)
            } else {
                format!("[{}] {}", m.created_at.format("%Y-%m-%d"), m.memory)
            }
        }));

        // Buckets: memories grouped by bucket_key + any per-bucket summaries.
        let mut buckets: std::collections::BTreeMap<String, Vec<String>> = Default::default();
        let bucket_keys: Vec<String> = bucket_defs.iter().map(|def| def.key.clone()).collect();
        let mut bucket_mems = self
            .db
            .memories_in_buckets(org_id, container_tag, &bucket_keys, 20)
            .await?;
        for def in &bucket_defs {
            let mems = bucket_mems.remove(&def.key).unwrap_or_default();
            let mut entries: Vec<String> = summaries
                .iter()
                .filter(|(b, _)| b.as_deref() == Some(def.key.as_str()))
                .map(|(_, s)| format!("[Summary] {s}"))
                .collect();
            entries.extend(mems.into_iter().map(|m| format!("[Recent] {}", m.memory)));
            if !entries.is_empty() {
                buckets.insert(def.key.clone(), entries);
            }
        }

        Ok(Profile {
            r#static: (!static_list.is_empty()).then_some(static_list),
            dynamic: (!dynamic_list.is_empty()).then_some(dynamic_list),
            buckets: (!buckets.is_empty()).then_some(buckets),
        })
    }

    /// Classify a memory into one of the container's buckets (LLM zero-shot).
    /// Returns the chosen bucket key, or None if it fits none.
    pub async fn classify_bucket(
        &self,
        org_id: &str,
        container_tag: &str,
        memory: &str,
    ) -> Result<Option<String>> {
        let buckets = self.db.list_buckets(org_id, Some(container_tag)).await?;
        if buckets.is_empty() {
            return Ok(None);
        }
        let listing = buckets
            .iter()
            .map(|b| format!("- {}: {}", b.key, b.description))
            .collect::<Vec<_>>()
            .join("\n");
        let system = ChatMessage::system(
            "Classify the memory into ONE bucket by key, or \"none\" if it fits none. \
             Respond ONLY as JSON {\"bucket\":string}.",
        );
        let user = ChatMessage::user(format!("Buckets:\n{listing}\n\nMemory: {memory}"));
        let raw = self
            .models
            .llm
            .complete(
                vec![system, user],
                ChatOptions {
                    json: true,
                    temperature: Some(0.0),
                    ..Default::default()
                },
            )
            .await?;
        let chosen = serde_json::from_str::<serde_json::Value>(raw.trim())
            .ok()
            .and_then(|v| v["bucket"].as_str().map(|s| s.to_string()));
        match chosen {
            Some(k) if k != "none" && buckets.iter().any(|b| b.key == k) => Ok(Some(k)),
            _ => Ok(None),
        }
    }

    /// Assign every fact for one container tag to a profile bucket in a SINGLE LLM call,
    /// returning one entry per fact (aligned by order). This replaces the previous
    /// per-fact fan-out (one `list_buckets` query + one completion per fact). If the
    /// batched response is malformed or the wrong length, it falls back to per-fact
    /// [`Self::classify_bucket`] so classification quality is never worse than before.
    pub async fn classify_buckets_batch(
        &self,
        org_id: &str,
        container_tag: &str,
        facts: &[String],
    ) -> Result<Vec<Option<String>>> {
        if facts.is_empty() {
            return Ok(Vec::new());
        }
        let buckets = self.db.list_buckets(org_id, Some(container_tag)).await?;
        if buckets.is_empty() {
            return Ok(vec![None; facts.len()]);
        }
        let listing = buckets
            .iter()
            .map(|b| format!("- {}: {}", b.key, b.description))
            .collect::<Vec<_>>()
            .join("\n");
        let numbered = facts
            .iter()
            .enumerate()
            .map(|(i, fact)| format!("{i}. {fact}"))
            .collect::<Vec<_>>()
            .join("\n");
        let system = ChatMessage::system(
            "Assign each numbered memory to ONE bucket by key, or \"none\" if it fits none. \
             Respond ONLY as JSON {\"assignments\":[string,...]} with exactly one entry per \
             memory, in the same order as the memories.",
        );
        let user = ChatMessage::user(format!("Buckets:\n{listing}\n\nMemories:\n{numbered}"));
        let raw = self
            .models
            .llm
            .complete(
                vec![system, user],
                ChatOptions {
                    json: true,
                    temperature: Some(0.0),
                    ..Default::default()
                },
            )
            .await?;
        let keys: Vec<&str> = buckets.iter().map(|b| b.key.as_str()).collect();
        if let Some(assignments) = parse_bucket_assignments(&raw, &keys, facts.len()) {
            return Ok(assignments);
        }
        // Malformed or mis-sized batch response: fall back to per-fact classification so we
        // never do worse than the previous behaviour (only reached when the model doesn't
        // honour the batch format, so the fan-out cost is confined to that rare case).
        tracing::warn!(
            container_tag,
            "batched bucket classification response was malformed; falling back to per-fact"
        );
        let mut out = Vec::with_capacity(facts.len());
        for fact in facts {
            out.push(
                self.classify_bucket(org_id, container_tag, fact)
                    .await
                    .ok()
                    .flatten(),
            );
        }
        Ok(out)
    }

    /// Aggregate old memories into `[Summary]` entries (per bucket + general).
    /// Runs periodically from the binary. Returns the number of summaries written.
    pub async fn aggregate_profile(&self, org_id: &str, container_tag: &str) -> Result<usize> {
        const MIN_TO_AGGREGATE: usize = 8;
        const OLDER_THAN_DAYS: i64 = 30;
        let mut written = 0;

        // General summary from old memories.
        let old = self
            .db
            .aggregatable_memories(org_id, container_tag, OLDER_THAN_DAYS, 100)
            .await?;
        if old.len() >= MIN_TO_AGGREGATE {
            let joined = old
                .iter()
                .map(|m| format!("- {}", m.memory))
                .collect::<Vec<_>>()
                .join("\n");
            // Roll the existing summary forward so history is preserved even though only
            // newly-aggregatable memories are summarized this cycle.
            let existing = self
                .db
                .get_profile_summary(org_id, container_tag, None)
                .await?;
            let input = match existing {
                Some(prev) if !prev.trim().is_empty() => {
                    format!("Existing summary:\n{prev}\n\nAdditional facts:\n{joined}")
                }
                _ => joined,
            };
            if let Ok(summary) = self.summarize_facts(&input).await {
                self.db
                    .upsert_profile_summary(org_id, container_tag, None, &summary)
                    .await?;
                // Mark these memories aggregated so they are not re-summarized forever and
                // the next cycle advances to memories beyond the first 100.
                let ids: Vec<String> = old.iter().map(|m| m.id.clone()).collect();
                self.db.mark_memories_aggregated(&ids).await?;
                written += 1;
            }
        }
        Ok(written)
    }

    async fn summarize_facts(&self, facts: &str) -> Result<String> {
        let system = ChatMessage::system(
            "Summarize these facts into a concise 2-3 sentence synthesis capturing stable patterns.",
        );
        self.models
            .llm
            .complete(
                vec![system, ChatMessage::user(facts.to_string())],
                ChatOptions {
                    temperature: Some(0.2),
                    max_tokens: Some(200),
                    ..Default::default()
                },
            )
            .await
    }

    /// Soft-delete a memory by id or exact content (`DELETE /v1/memories`).
    pub async fn forget(&self, org_id: &str, req: &ForgetRequest) -> Result<Memory> {
        if !memoricai_core::is_valid_container_tag(&req.container_tag) {
            return Err(Error::BadRequest("invalid container tag".into()));
        }
        if req.id.is_some() == req.content.is_some() {
            return Err(Error::BadRequest(
                "provide exactly one of id or content to forget".into(),
            ));
        }
        if req
            .content
            .as_ref()
            .is_some_and(|content| content.trim().is_empty() || content.len() > 10 * 1024)
        {
            return Err(Error::BadRequest(
                "content must contain 1..=10240 bytes".into(),
            ));
        }
        if req.reason.as_ref().is_some_and(|reason| reason.len() > 512) {
            return Err(Error::BadRequest("reason exceeds 512 bytes".into()));
        }
        let reason = req.reason.as_deref().or(Some("user-requested"));
        let result = if let Some(id) = &req.id {
            let target = self.db.get_memory(org_id, id).await?;
            if target.space_container_tag != req.container_tag {
                return Err(Error::NotFound("memory to forget".into()));
            }
            self.db
                .forget_memory_by_id(org_id, id, reason, None)
                .await?
        } else if let Some(content) = &req.content {
            self.db
                .forget_memory_by_content(org_id, &req.container_tag, content, reason)
                .await?
        } else {
            return Err(Error::BadRequest("provide id or content to forget".into()));
        };
        result.ok_or_else(|| Error::NotFound("memory to forget".into()))
    }

    /// Versioned update (`PATCH /v1/memories`): append a new version, retire the old.
    pub async fn patch_memory(&self, org_id: &str, req: &PatchMemoryRequest) -> Result<Memory> {
        if req.new_content.trim().is_empty() || req.new_content.len() > 10 * 1024 {
            return Err(Error::BadRequest(
                "newContent must contain 1..=10240 bytes".into(),
            ));
        }
        if let Some(metadata) = &req.metadata {
            crate::validate_metadata(metadata)?;
        }
        let target = match &req.id {
            Some(id) => self.db.get_memory(org_id, id).await?,
            None => {
                return Err(Error::BadRequest(
                    "patch by content is not supported yet; provide id".into(),
                ))
            }
        };
        let now: Timestamp = chrono::Utc::now();
        let embedding = self.models.embedder.embed(&req.new_content).await?;
        crate::validate_embedding(&embedding, self.models.dim())?;
        let embedding_index = self.embedding_index(org_id).await?;
        let new_id = memoricai_core::ids::memory_id();
        let root = target
            .root_memory_id
            .clone()
            .unwrap_or_else(|| target.id.clone());
        let mem = Memory {
            id: new_id.clone(),
            custom_id: target.custom_id.clone(),
            document_id: target.document_id.clone(),
            org_id: org_id.to_string(),
            user_id: target.user_id.clone(),
            memory: req.new_content.clone(),
            summary: None,
            mem_type: target.mem_type.clone(),
            space_container_tag: target.space_container_tag.clone(),
            version: target.version + 1,
            is_latest: true,
            parent_memory_id: Some(target.id.clone()),
            root_memory_id: Some(root),
            relation: Some(MemoryRelation::Updates),
            source_count: target.source_count,
            is_static: target.is_static,
            is_inference: false,
            review_status: None,
            is_forgotten: false,
            forget_reason: None,
            forget_after: None,
            forget_batch_id: None,
            event_date: target.event_date,
            metadata: req
                .metadata
                .clone()
                .unwrap_or_else(|| target.metadata.clone()),
            created_at: now,
            updated_at: now,
        };
        self.db
            .replace_latest_memory(&target.id, &mem, &embedding_index.id, &embedding)
            .await?;
        Ok(mem)
    }
}

/// Extract the first {...} JSON object substring from a noisy LLM response.
fn extract_json_block(s: &str) -> &str {
    match (s.find('{'), s.rfind('}')) {
        (Some(a), Some(b)) if b > a => &s[a..=b],
        _ => "{}",
    }
}

/// Parse a batched bucket-assignment response into one entry per fact (aligned by order).
/// Returns `None` — signalling the caller to fall back to per-fact classification — when
/// the response is unparseable or does not contain exactly `n` assignments. Within a valid
/// response, `"none"` and any key not in `valid_keys` map to `None` (unbucketed).
fn parse_bucket_assignments(
    raw: &str,
    valid_keys: &[&str],
    n: usize,
) -> Option<Vec<Option<String>>> {
    let value: serde_json::Value = serde_json::from_str(raw.trim())
        .or_else(|_| serde_json::from_str(extract_json_block(raw)))
        .ok()?;
    let arr = value.get("assignments")?.as_array()?;
    if arr.len() != n {
        return None;
    }
    Some(
        arr.iter()
            .map(|entry| {
                entry.as_str().and_then(|key| {
                    (key != "none" && valid_keys.contains(&key)).then(|| key.to_string())
                })
            })
            .collect(),
    )
}

pub(crate) fn parse_iso_date(s: &str) -> Option<Timestamp> {
    // Accept full RFC3339 or a bare date.
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&chrono::Utc));
    }
    if let Ok(d) = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return Some(d.and_hms_opt(0, 0, 0)?.and_utc());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::parse_bucket_assignments;

    const KEYS: &[&str] = &["preferences", "work"];

    #[test]
    fn valid_batch_maps_keys_and_none() {
        let raw = r#"{"assignments":["preferences","none","work"]}"#;
        let got = parse_bucket_assignments(raw, KEYS, 3).expect("valid batch parses");
        assert_eq!(
            got,
            vec![
                Some("preferences".to_string()),
                None,
                Some("work".to_string())
            ]
        );
    }

    #[test]
    fn unknown_keys_become_unbucketed() {
        // A hallucinated bucket key must not be assigned; it maps to None.
        let raw = r#"{"assignments":["made_up","work"]}"#;
        let got = parse_bucket_assignments(raw, KEYS, 2).expect("parses");
        assert_eq!(got, vec![None, Some("work".to_string())]);
    }

    #[test]
    fn wrong_length_signals_fallback() {
        // Two assignments returned for three facts -> None, so the caller falls back
        // to per-fact classification rather than misaligning buckets to facts.
        let raw = r#"{"assignments":["work","preferences"]}"#;
        assert!(parse_bucket_assignments(raw, KEYS, 3).is_none());
    }

    #[test]
    fn malformed_json_signals_fallback() {
        assert!(parse_bucket_assignments("not json at all", KEYS, 1).is_none());
        assert!(parse_bucket_assignments(r#"{"other":[]}"#, KEYS, 1).is_none());
    }

    #[test]
    fn tolerates_surrounding_prose() {
        // extract_json_block salvages the object from a chatty response.
        let raw = "Here you go: {\"assignments\":[\"work\"]} hope that helps";
        let got = parse_bucket_assignments(raw, KEYS, 1).expect("salvaged");
        assert_eq!(got, vec![Some("work".to_string())]);
    }
}
