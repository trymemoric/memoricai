//! Retrieval: `/v1` chunk RAG, `/v1` memory/hybrid search, profile fast path,
//! and bulk semantic forget. Supports query rewriting (Phase 2) and cross-encoder
//! reranking (Phase 2).

use crate::Engine;
use futures::StreamExt;
use memoricai_core::dto::*;
use memoricai_core::error::{Error, Result};
use memoricai_core::filter::MetadataFilter;
use memoricai_core::ports::{ChatMessage, ChatOptions};
use memoricai_db::MemoryHit;
use std::collections::HashMap;
use std::time::Instant;

impl Engine {
    fn validate_query(query: &str) -> Result<()> {
        if query.trim().is_empty() || query.len() > 4096 {
            return Err(Error::BadRequest(
                "query must contain between 1 and 4096 bytes".into(),
            ));
        }
        Ok(())
    }

    fn validate_threshold(value: f32, name: &str) -> Result<()> {
        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
            return Err(Error::BadRequest(format!(
                "{name} must be a finite number between 0 and 1"
            )));
        }
        Ok(())
    }

    /// Generate query variations for recall expansion (empty on failure).
    async fn query_variations(&self, q: &str) -> Vec<String> {
        let system = ChatMessage::system(
            "Rewrite the user's search query into 2 alternative phrasings that expand \
             synonyms and abbreviations. Respond ONLY as JSON {\"queries\":[string,string]}.",
        );
        let raw = self
            .models
            .llm
            .complete(
                vec![system, ChatMessage::user(q.to_string())],
                ChatOptions {
                    json: true,
                    temperature: Some(0.3),
                    ..Default::default()
                },
            )
            .await;
        let Ok(raw) = raw else { return vec![] };
        serde_json::from_str::<serde_json::Value>(raw.trim())
            .ok()
            .and_then(|v| {
                v["queries"].as_array().map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str())
                        .map(str::trim)
                        .filter(|query| !query.is_empty() && query.len() <= 4096)
                        .take(2)
                        .map(str::to_string)
                        .collect::<Vec<_>>()
                })
            })
            .unwrap_or_default()
    }

    /// Embed the query plus (optionally) rewritten variations. The base
    /// query's embedding is served from the process-local cache when
    /// possible — remote embedding round-trips dominate search latency.
    async fn query_embeddings(&self, q: &str, rewrite: bool) -> Result<Vec<Vec<f32>>> {
        let cached = self
            .query_cache
            .lock()
            .map(|cache| cache.get(q))
            .unwrap_or(None);
        let mut queries: Vec<String> = Vec::new();
        if cached.is_none() {
            queries.push(q.to_string());
        }
        if rewrite {
            queries.extend(self.query_variations(q).await);
        }
        let embeddings = if queries.is_empty() {
            Vec::new()
        } else {
            // Query-side embedding: asymmetric local models apply their
            // query task prefix here (no-op for symmetric providers).
            self.models.embedder.embed_query_batch(&queries).await?
        };
        crate::validate_embedding_batch(&queries, &embeddings, self.models.dim())?;
        match cached {
            Some(vec) => {
                let mut out = Vec::with_capacity(embeddings.len() + 1);
                out.push(vec);
                out.extend(embeddings);
                Ok(out)
            }
            None => {
                if let (Some(first), Ok(mut cache)) = (embeddings.first(), self.query_cache.lock())
                {
                    cache.put(q.to_string(), first.clone());
                }
                Ok(embeddings)
            }
        }
    }

    /// Apply the reranker over result passages, replacing similarity with the rerank score.
    async fn apply_rerank(
        &self,
        query: &str,
        mut results: Vec<MemorySearchResult>,
    ) -> Vec<MemorySearchResult> {
        if results.is_empty() {
            return results;
        }
        let passages: Vec<String> = results
            .iter()
            .map(|r| {
                r.memory
                    .clone()
                    .or_else(|| r.chunk.clone())
                    .unwrap_or_default()
            })
            .collect();
        if let Ok(scores) = self.models.reranker.rerank(query, &passages).await {
            if scores.len() != results.len() || scores.iter().any(|score| !score.is_finite()) {
                return results;
            }
            for (r, s) in results.iter_mut().zip(scores) {
                r.similarity = s;
            }
            results.sort_by(|a, b| {
                b.similarity
                    .partial_cmp(&a.similarity)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        }
        results
    }

    /// `/v1/documents/search` — document/chunk RAG.
    pub async fn search_documents(
        &self,
        org_id: &str,
        req: &DocumentSearchRequest,
    ) -> Result<DocumentSearchResponse> {
        Self::validate_query(&req.q)?;
        if req.limit == 0 || req.limit > 100 {
            return Err(Error::BadRequest("limit must be between 1 and 100".into()));
        }
        Self::validate_threshold(req.chunk_threshold, "chunkThreshold")?;
        Self::validate_threshold(req.document_threshold, "documentThreshold")?;
        if req
            .filters
            .as_ref()
            .is_some_and(|filter| MetadataFilter::from_value(filter).is_none())
        {
            return Err(Error::BadRequest("invalid metadata filter".into()));
        }
        let start = Instant::now();
        let qvecs = self.query_embeddings(&req.q, req.rewrite_query).await?;
        let tags = req.container_tags.as_deref();

        // Gather chunks across all query embeddings, keeping the best score per (doc, content).
        let mut best: HashMap<(String, String), f32> = HashMap::new();
        let hit_batches = futures::future::try_join_all(qvecs.iter().map(|qvec| {
            self.db.search_chunks(
                org_id,
                tags,
                qvec,
                (req.limit as i64) * 4,
                req.chunk_threshold,
                req.doc_id.as_deref(),
            )
        }))
        .await?;
        for hits in hit_batches {
            for h in hits {
                let key = (h.document_id, h.content);
                let e = best.entry(key).or_insert(0.0);
                *e = e.max(h.similarity);
            }
        }

        // Group by document.
        let mut by_doc: HashMap<String, Vec<(String, f32)>> = HashMap::new();
        for ((doc_id, content), score) in best {
            by_doc.entry(doc_id).or_default().push((content, score));
        }

        let filter = req.filters.as_ref().and_then(MetadataFilter::from_value);
        let by_doc: Vec<(String, Vec<(String, f32)>)> = by_doc.into_iter().collect();
        let doc_futs: Vec<_> = by_doc
            .iter()
            .map(|(doc_id, _)| self.db.get_document(org_id, doc_id))
            .collect();
        let docs = futures::stream::iter(doc_futs)
            .buffered(8)
            .collect::<Vec<_>>()
            .await;
        let mut results = Vec::new();
        for ((_, chunks), doc) in by_doc.into_iter().zip(docs) {
            let doc = match doc {
                Ok(d) => d,
                Err(_) => continue,
            };
            if let Some(f) = &filter {
                if !f.matches(&doc.metadata) {
                    continue;
                }
            }
            let best_score = chunks.iter().map(|(_, s)| *s).fold(0.0_f32, f32::max);
            if best_score < req.document_threshold {
                continue;
            }
            let chunk_hits = chunks
                .into_iter()
                .map(|(content, score)| ChunkHit {
                    content,
                    score,
                    is_relevant: score >= req.chunk_threshold,
                })
                .collect();
            results.push(DocumentSearchResult {
                document_id: doc.id,
                title: doc.title,
                doc_type: doc.doc_type,
                score: best_score,
                chunks: chunk_hits,
                metadata: doc.metadata,
                created_at: doc.created_at,
                updated_at: doc.updated_at,
                content: req.include_full_docs.then_some(doc.content).flatten(),
                summary: req.include_summary.then_some(doc.summary).flatten(),
            });
        }
        results.sort_unstable_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        results.truncate(req.limit as usize);
        let total = results.len();
        Ok(DocumentSearchResponse {
            results,
            timing: start.elapsed().as_millis() as u64,
            total,
        })
    }

    /// `/v1/search` — memory / hybrid search.
    pub async fn search_memories(
        &self,
        org_id: &str,
        req: &MemorySearchRequest,
        default_tag: Option<&str>,
    ) -> Result<MemorySearchResponse> {
        Self::validate_query(&req.q)?;
        if req.limit == 0 || req.limit > 100 {
            return Err(Error::BadRequest("limit must be between 1 and 100".into()));
        }
        Self::validate_threshold(req.threshold, "threshold")?;
        if !matches!(
            req.search_mode.as_str(),
            "memories" | "hybrid" | "documents"
        ) {
            return Err(Error::BadRequest(
                "searchMode must be memories, hybrid, or documents".into(),
            ));
        }
        if let Some(tag) = req.container_tag.as_deref().or(default_tag) {
            if !memoricai_core::is_valid_container_tag(tag) {
                return Err(Error::BadRequest("invalid container tag".into()));
            }
        }
        if req
            .filters
            .as_ref()
            .is_some_and(|filter| MetadataFilter::from_value(filter).is_none())
        {
            return Err(Error::BadRequest("invalid metadata filter".into()));
        }
        let start = Instant::now();
        let qvecs = self.query_embeddings(&req.q, req.rewrite_query).await?;
        let tag = req.container_tag.as_deref().or(default_tag);
        let filter = req.filters.as_ref().and_then(MetadataFilter::from_value);
        let mode = req.search_mode.as_str();

        let mut results: Vec<MemorySearchResult> = Vec::new();
        let mut digest: Option<String> = None;

        if mode == "memories" || mode == "hybrid" {
            // Merge hits across query embeddings by memory id (max similarity).
            // A digest draws on a wider slice of matches than the result page;
            // aggregation-shaped queries ("how many…", "list all…") need
            // completeness, not top-k relevance, so they fetch wider still.
            let aggregate = req.digest && is_aggregation_query(&req.q);
            let mut fetch = (req.limit as i64) * 3;
            if req.digest {
                fetch = fetch.max(if aggregate { 200 } else { 60 });
            }
            let mut merged: HashMap<String, MemoryHit> = HashMap::new();
            let hit_batches = futures::future::try_join_all(qvecs.iter().map(|qvec| {
                self.db.search_memories(
                    org_id,
                    tag,
                    qvec,
                    fetch,
                    req.threshold,
                    req.include.forgotten_memories,
                )
            }))
            .await?;
            for hits in hit_batches {
                for mut hit in hits {
                    // Preserve the previous 0.0 floor on first insert.
                    hit.similarity = hit.similarity.max(0.0);
                    let s = hit.similarity;
                    merged
                        .entry(hit.memory.id.clone())
                        .and_modify(|e| e.similarity = e.similarity.max(s))
                        .or_insert(hit);
                }
            }
            let mut hits: Vec<MemoryHit> = merged.into_values().collect();
            hits.sort_unstable_by(|a, b| {
                b.similarity
                    .partial_cmp(&a.similarity)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });

            if req.digest {
                digest = self.build_digest(org_id, &hits, aggregate).await?;
            }

            for hit in hits {
                let m = hit.memory;
                if let Some(f) = &filter {
                    if !f.matches(&m.metadata) {
                        continue;
                    }
                }
                let context = if req.include.related_memories {
                    Some(self.memory_context(&m.id).await?)
                } else {
                    None
                };
                let documents = if req.include.documents {
                    self.memory_documents(org_id, &m).await
                } else {
                    None
                };
                results.push(MemorySearchResult {
                    id: m.id,
                    memory: Some(m.memory),
                    chunk: None,
                    similarity: hit.similarity,
                    metadata: m.metadata,
                    updated_at: m.updated_at,
                    version: m.version,
                    root_memory_id: m.root_memory_id,
                    context,
                    documents,
                });
            }
        }

        // Fall back to / include document chunks.
        if (mode == "hybrid" && results.len() < req.limit as usize) || mode == "documents" {
            let tags = tag.map(|t| vec![t.to_string()]);
            let mut seen: HashMap<String, f32> = HashMap::new();
            let chunk_batches = futures::future::try_join_all(qvecs.iter().map(|qvec| {
                self.db.search_chunks(
                    org_id,
                    tags.as_deref(),
                    qvec,
                    req.limit as i64,
                    req.threshold,
                    None,
                )
            }))
            .await?;
            for chunk_hits in chunk_batches {
                for ch in chunk_hits {
                    let e = seen.entry(ch.content).or_insert(0.0);
                    *e = e.max(ch.similarity);
                }
            }
            for (content, sim) in seen {
                results.push(MemorySearchResult {
                    id: memoricai_core::ids::token(10),
                    memory: None,
                    chunk: Some(content),
                    similarity: sim,
                    metadata: serde_json::json!({}),
                    updated_at: chrono::Utc::now(),
                    version: 0,
                    root_memory_id: None,
                    context: None,
                    documents: None,
                });
            }
        }

        if req.rerank {
            results = self.apply_rerank(&req.q, results).await;
        } else {
            results.sort_by(|a, b| {
                b.similarity
                    .partial_cmp(&a.similarity)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        }
        results.truncate(req.limit as usize);
        let total = results.len();
        Ok(MemorySearchResponse {
            results,
            timing: start.elapsed().as_millis() as u64,
            total,
            digest,
        })
    }

    /// Compose a compact, ready-to-inject context digest from memory hits:
    /// facts grouped by source document, stamped with the document's `date`
    /// metadata (falling back to its creation date), groups ordered by best
    /// similarity — or chronologically with a larger budget for
    /// aggregation-shaped queries, where completeness beats relevance.
    /// Facts carrying an `event_date` are prefixed with it. Latest memory
    /// versions only (superseded facts never reach search). Pure composition
    /// over already-extracted data — no model calls.
    async fn build_digest(
        &self,
        org_id: &str,
        hits: &[MemoryHit],
        aggregate: bool,
    ) -> Result<Option<String>> {
        let budget: usize = if aggregate { 8000 } else { 4000 };
        if hits.is_empty() {
            return Ok(None);
        }

        // Group hits by source document, keeping group order by best hit.
        let mut group_order: Vec<Option<String>> = Vec::new();
        let mut groups: HashMap<Option<String>, Vec<&MemoryHit>> = HashMap::new();
        for hit in hits {
            let key = hit.memory.document_id.clone();
            let entry = groups.entry(key.clone()).or_default();
            if entry.is_empty() {
                group_order.push(key);
            }
            entry.push(hit);
        }

        // Resolve document dates (metadata `date` string, else creation date).
        let doc_ids: Vec<String> = group_order.iter().flatten().cloned().collect();
        let doc_futs: Vec<_> = doc_ids
            .iter()
            .map(|id| self.db.get_document(org_id, id))
            .collect();
        let docs = futures::stream::iter(doc_futs)
            .buffered(8)
            .collect::<Vec<_>>()
            .await;
        let mut doc_dates: HashMap<String, String> = HashMap::new();
        for (id, doc) in doc_ids.iter().zip(docs) {
            if let Ok(doc) = doc {
                let date = doc
                    .metadata
                    .get("date")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| doc.created_at.format("%Y-%m-%d").to_string());
                doc_dates.insert(id.clone(), date);
            }
        }

        let group_date = |key: &Option<String>| -> Option<String> {
            match key {
                Some(id) => doc_dates.get(id).cloned(),
                None => None,
            }
            .or_else(|| {
                groups
                    .get(key)
                    .and_then(|group| group.first())
                    .map(|h| h.memory.updated_at.format("%Y-%m-%d").to_string())
            })
        };
        // Groups stay in relevance order even for aggregation queries:
        // chronological ordering buries the strongest matches mid-timeline
        // and measurably hurt multi-session accuracy (74.4% -> 66.9% on the
        // LongMemEval multi-session slice). Aggregation queries differ only
        // in fetch width, budget, and the completeness instruction.
        let mut out = String::from(if aggregate {
            "Facts from memory (most relevant first; check the full list before counting or summarizing):\n"
        } else {
            "Facts from memory (most relevant first):\n"
        });
        'groups: for key in &group_order {
            let Some(group) = groups.get(key) else {
                continue;
            };
            let header = match group_date(key) {
                Some(d) => format!("\n## {d}\n"),
                None => "\n##\n".to_string(),
            };
            if out.len() + header.len() > budget {
                break;
            }
            out.push_str(&header);
            for hit in group {
                let line = match hit.memory.event_date {
                    Some(when) => {
                        format!("- [{}] {}\n", when.format("%Y-%m-%d"), hit.memory.memory)
                    }
                    None => format!("- {}\n", hit.memory.memory),
                };
                if out.len() + line.len() > budget {
                    break 'groups;
                }
                out.push_str(&line);
            }
        }
        Ok(Some(out))
    }

    async fn memory_context(&self, memory_id: &str) -> Result<MemoryContext> {
        let parents = self.db.memory_parents(memory_id).await?;
        let children = self.db.memory_children(memory_id).await?;
        Ok(MemoryContext {
            parents: parents
                .into_iter()
                .map(|(m, rel)| ContextEntry {
                    memory: m.memory,
                    relation: rel.as_str().to_string(),
                    version: m.version,
                    updated_at: m.updated_at,
                })
                .collect(),
            children: children
                .into_iter()
                .map(|(m, rel)| ContextEntry {
                    memory: m.memory,
                    relation: rel.as_str().to_string(),
                    version: m.version,
                    updated_at: m.updated_at,
                })
                .collect(),
        })
    }

    async fn memory_documents(
        &self,
        org_id: &str,
        mem: &memoricai_core::model::Memory,
    ) -> Option<Vec<memoricai_core::model::Document>> {
        let doc_id = mem.document_id.as_deref()?;
        self.db
            .get_document(org_id, doc_id)
            .await
            .ok()
            .map(|d| vec![d])
    }

    /// `/v1/profile` — profile fast path (+ optional combined search).
    pub async fn profile(&self, org_id: &str, req: &ProfileRequest) -> Result<ProfileResponse> {
        if !memoricai_core::is_valid_container_tag(&req.container_tag) {
            return Err(Error::BadRequest("invalid container tag".into()));
        }
        if let Some(query) = &req.q {
            Self::validate_query(query)?;
        }
        if let Some(threshold) = req.threshold {
            Self::validate_threshold(threshold, "threshold")?;
        }
        if req.include.as_ref().is_some_and(|items| {
            items.len() > 3
                || items
                    .iter()
                    .any(|item| !matches!(item.as_str(), "static" | "dynamic" | "buckets"))
        }) {
            return Err(Error::BadRequest(
                "include accepts only static, dynamic, and buckets".into(),
            ));
        }
        if req.buckets.as_ref().is_some_and(|buckets| {
            buckets.len() > 100
                || buckets
                    .iter()
                    .any(|bucket| bucket.trim().is_empty() || bucket.len() > 100)
        }) {
            return Err(Error::BadRequest(
                "buckets accepts at most 100 non-empty keys of at most 100 bytes".into(),
            ));
        }
        let mut profile = self.build_profile(org_id, &req.container_tag).await?;
        if let Some(include) = &req.include {
            if !include.iter().any(|s| s == "static") {
                profile.r#static = None;
            }
            if !include.iter().any(|s| s == "dynamic") {
                profile.dynamic = None;
            }
            if !include.iter().any(|s| s == "buckets") {
                profile.buckets = None;
            }
        }
        if let (Some(requested), Some(buckets)) = (&req.buckets, &mut profile.buckets) {
            buckets.retain(|key, _| requested.iter().any(|requested| requested == key));
            if buckets.is_empty() {
                profile.buckets = None;
            }
        }
        let search_results = if let Some(q) = &req.q {
            let sreq = MemorySearchRequest {
                q: q.clone(),
                container_tag: Some(req.container_tag.clone()),
                search_mode: "hybrid".into(),
                limit: 10,
                threshold: req.threshold.unwrap_or(0.5),
                rerank: false,
                rewrite_query: false,
                filters: req.filters.clone(),
                include: SearchInclude::default(),
                digest: false,
            };
            Some(
                self.search_memories(org_id, &sreq, Some(&req.container_tag))
                    .await?,
            )
        } else {
            None
        };
        Ok(ProfileResponse {
            profile,
            search_results,
        })
    }

    /// `/v1/memories/forget-matching` — semantic bulk forget.
    pub async fn forget_matching(
        &self,
        org_id: &str,
        req: &ForgetMatchingRequest,
    ) -> Result<ForgetMatchingResponse> {
        Self::validate_query(&req.query)?;
        if !memoricai_core::is_valid_container_tag(&req.container_tag) {
            return Err(Error::BadRequest("invalid container tag".into()));
        }
        Self::validate_threshold(req.threshold, "threshold")?;
        if req.max_forget == 0 || req.max_forget > 1000 {
            return Err(Error::BadRequest(
                "maxForget must be between 1 and 1000".into(),
            ));
        }
        let qvec = self
            .models
            .embedder
            .embed_query_batch(std::slice::from_ref(&req.query))
            .await?
            .pop()
            .ok_or_else(|| Error::Model("empty embedding response".into()))?;
        crate::validate_embedding(&qvec, self.models.dim())?;
        let hits = self
            .db
            .search_memories(
                org_id,
                Some(&req.container_tag),
                &qvec,
                req.max_forget as i64,
                req.threshold,
                false,
            )
            .await?;
        let candidates: Vec<ForgetCandidate> = hits
            .iter()
            .map(|h| ForgetCandidate {
                id: h.memory.id.clone(),
                memory: h.memory.memory.clone(),
                similarity: h.similarity,
            })
            .collect();

        if req.dry_run {
            return Ok(ForgetMatchingResponse {
                dry_run: true,
                count: candidates.len(),
                forget_batch_id: None,
                summary: format!("{} memories would be forgotten", candidates.len()),
                candidates: Some(candidates),
                forgotten: None,
            });
        }

        let batch = memoricai_core::ids::batch_id();
        let reason = req.reason.as_deref().unwrap_or("bulk-forget");
        let mut forgotten = Vec::new();
        for c in &candidates {
            if self
                .db
                .forget_memory_by_id(org_id, &c.id, Some(reason), Some(&batch))
                .await?
                .is_some()
            {
                forgotten.push(c.clone());
            }
        }
        Ok(ForgetMatchingResponse {
            dry_run: false,
            count: forgotten.len(),
            forget_batch_id: Some(batch),
            summary: format!("forgot {} memories", forgotten.len()),
            candidates: None,
            forgotten: Some(forgotten),
        })
    }
}

/// Deterministic detector for aggregation/enumeration-shaped queries, where
/// digest completeness matters more than top-k relevance ("how many…",
/// "list all…", "which of the…"). False positives only widen the digest.
fn is_aggregation_query(q: &str) -> bool {
    let q = q.to_lowercase();
    const PHRASES: &[&str] = &[
        "how many",
        "how much",
        "how often",
        "how frequently",
        "count",
        "list all",
        "list the",
        "list every",
        "name all",
        "what are all",
        "what are the",
        "which of",
        "all the times",
        "every time",
        "each time",
        "in total",
        "total number",
        "altogether",
        "so far",
    ];
    PHRASES.iter().any(|p| q.contains(p))
}

#[cfg(test)]
mod tests {
    use super::is_aggregation_query;

    #[test]
    fn aggregation_queries_are_detected() {
        for q in [
            "How many art-related events did I attend in the past month?",
            "list all the restaurants I mentioned",
            "What are all the books I've read this year?",
            "how often did I go to the gym",
            "In total, how much did I spend on travel?",
        ] {
            assert!(is_aggregation_query(q), "should detect: {q}");
        }
    }

    #[test]
    fn lookup_queries_are_not_detected() {
        for q in [
            "What is my name?",
            "When did I attend the photography workshop?",
            "What laptop did the assistant recommend?",
            "Where does my sister live?",
        ] {
            assert!(!is_aggregation_query(q), "should not detect: {q}");
        }
    }
}
