//! End-to-end engine test against a real Postgres+pgvector.
//! Set `MEMORICAI_TEST_DATABASE_URL` to run it; otherwise it is skipped.
//!
//!   createdb memoricai_test
//!   psql -d memoricai_test -c 'CREATE EXTENSION IF NOT EXISTS vector;'
//!   MEMORICAI_TEST_DATABASE_URL=postgres://$USER@localhost/memoricai_test cargo test -p memoricai

use std::sync::Arc;

use memoricai_auth::AuthService;
use memoricai_core::dto::{
    CreateMemoriesRequest, IngestRequest, MemoryInput, MemorySearchRequest, PatchMemoryRequest,
    ProfileRequest, SearchInclude,
};
use memoricai_db::Db;
use memoricai_engine::{Engine, EngineConfig};
use memoricai_models::ModelStack;

#[tokio::test]
#[ignore = "requires MEMORICAI_TEST_DATABASE_URL pointing to Postgres with pgvector"]
async fn ingest_search_profile_end_to_end() {
    let url = std::env::var("MEMORICAI_TEST_DATABASE_URL")
        .expect("MEMORICAI_TEST_DATABASE_URL is required for this ignored test");

    let db = Db::connect(&url).await.expect("connect");
    db.migrate().await.expect("migrate");

    // Unique tenant per run so repeated runs stay isolated.
    let auth = AuthService::new(db.clone());
    let (org, user, _key) = auth
        .bootstrap_org("itest", "it@memoricai.local")
        .await
        .expect("bootstrap");
    let tag = format!("mc_project_{}", &org.id[4..12]);

    let models = Arc::new(ModelStack::for_tests(64));
    let engine = Engine::new(
        db.clone(),
        models,
        EngineConfig {
            ingest_concurrency: 2,
            chunk_chars: 400,
        },
    );
    let embedding_index = db
        .ensure_embedding_index(&org.id, "hash-embedder", "test-v1", "memoricai-test", 64)
        .await
        .expect("embedding index");

    // Ingest (accept-instantly) then let the background worker finish.
    let req = IngestRequest {
        content: "My name is Grace Hopper and I invented the first compiler.".into(),
        custom_id: None,
        container_tag: Some(tag.clone()),
        container_tags: None,
        metadata: None,
        entity_context: None,
        content_type: None,
        title: None,
        raw: None,
    };
    let (id, _status) = engine
        .ingest(&org.id, Some(&user.id), &req)
        .await
        .expect("ingest");

    let mut done = false;
    for _ in 0..100 {
        let doc = db.get_document_by_id(&id).await.expect("get doc");
        match doc.status {
            memoricai_core::enums::DocumentStatus::Done => {
                done = true;
                break;
            }
            memoricai_core::enums::DocumentStatus::Failed => panic!("ingest failed"),
            _ => tokio::time::sleep(std::time::Duration::from_millis(50)).await,
        }
    }
    assert!(done, "document did not reach done status");

    // Every vector is tied to an exact model index.
    assert_eq!(embedding_index.embedding_model_id, "hash-embedder");
    assert_eq!(embedding_index.model_version, "test-v1");
    assert_eq!(embedding_index.provider, "memoricai-test");
    assert_eq!(embedding_index.dimension, 64);
    let memory_vector_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM memory_embeddings e
         JOIN memories m ON m.id=e.memory_id
         WHERE e.index_id=$1 AND m.document_id=$2",
    )
    .bind(&embedding_index.id)
    .bind(&id)
    .fetch_one(&db.pool)
    .await
    .expect("count versioned memory vectors");
    let chunk_vector_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM chunk_embeddings e
         JOIN chunks c ON c.id=e.chunk_id
         WHERE e.index_id=$1 AND c.document_id=$2",
    )
    .bind(&embedding_index.id)
    .bind(&id)
    .fetch_one(&db.pool)
    .await
    .expect("count versioned chunk vectors");
    assert!(memory_vector_count > 0);
    assert!(chunk_vector_count > 0);

    // Removing one derived vector queues a durable repair from retained text;
    // the background worker restores it without re-ingesting the document.
    let memory_id: String =
        sqlx::query_scalar("SELECT memory_id FROM memory_embeddings WHERE index_id=$1 LIMIT 1")
            .bind(&embedding_index.id)
            .fetch_one(&db.pool)
            .await
            .expect("memory vector to backfill");
    sqlx::query("DELETE FROM memory_embeddings WHERE index_id=$1 AND memory_id=$2")
        .bind(&embedding_index.id)
        .bind(&memory_id)
        .execute(&db.pool)
        .await
        .expect("delete vector for backfill test");
    db.queue_embedding_backfill(&org.id, &embedding_index.id)
        .await
        .expect("queue embedding repair");
    let mut repaired = false;
    for _ in 0..100 {
        let present: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM memory_embeddings
                           WHERE index_id=$1 AND memory_id=$2)",
        )
        .bind(&embedding_index.id)
        .bind(&memory_id)
        .fetch_one(&db.pool)
        .await
        .expect("check embedding repair");
        if present {
            repaired = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(
        repaired,
        "background re-embedding did not restore the vector"
    );

    // Memory search finds the fact.
    let sreq = MemorySearchRequest {
        q: "what is my name".into(),
        container_tag: Some(tag.clone()),
        search_mode: "hybrid".into(),
        limit: 10,
        threshold: 0.01,
        rerank: false,
        rewrite_query: false,
        filters: None,
        include: SearchInclude::default(),
        digest: true,
    };
    let res = engine
        .search_memories(&org.id, &sreq, None)
        .await
        .expect("search");
    let hit = res.results.iter().any(|r| {
        r.memory.as_deref().unwrap_or("").contains("Grace")
            || r.chunk.as_deref().unwrap_or("").contains("Grace")
    });
    assert!(hit, "expected to find the memory, got {:?}", res.results);
    let digest = res.digest.as_deref().expect("digest requested");
    assert!(
        digest.contains("Grace") && digest.contains("## "),
        "digest should contain the fact under a date header, got {digest:?}"
    );

    // A version chain remains append-only across more than one update.
    let first_memory_id = res
        .results
        .iter()
        .find_map(|result| result.memory.as_ref().map(|_| result.id.clone()))
        .expect("memory result");
    let second = engine
        .patch_memory(
            &org.id,
            &PatchMemoryRequest {
                id: Some(first_memory_id),
                content: None,
                new_content: "My name is Grace Brewster Hopper.".into(),
                metadata: None,
            },
        )
        .await
        .expect("first memory update");
    let third = engine
        .patch_memory(
            &org.id,
            &PatchMemoryRequest {
                id: Some(second.id),
                content: None,
                new_content: "My name is Rear Admiral Grace Hopper.".into(),
                metadata: None,
            },
        )
        .await
        .expect("second memory update");
    assert_eq!(third.version, 3);
    assert!(third.is_latest);

    // Profile reflects the dynamic memory.
    let preq = ProfileRequest {
        container_tag: tag.clone(),
        q: None,
        threshold: None,
        filters: None,
        include: None,
        buckets: None,
    };
    let prof = engine.profile(&org.id, &preq).await.expect("profile");
    assert!(
        prof.profile.dynamic.map(|d| !d.is_empty()).unwrap_or(false),
        "expected dynamic memories in profile"
    );

    // Deleting a document-derived update restores the surviving predecessor.
    let direct = engine
        .create_memories(
            &org.id,
            Some(&user.id),
            &CreateMemoriesRequest {
                memories: vec![MemoryInput {
                    content: "I collect vintage keyboards".into(),
                    is_static: false,
                    metadata: None,
                }],
                container_tag: tag.clone(),
            },
        )
        .await
        .expect("direct memory");
    let predecessor_id = direct.memories[0].id.clone();
    let replacement_req = IngestRequest {
        content: "I collect vintage keyboards.".into(),
        custom_id: None,
        container_tag: Some(tag),
        container_tags: None,
        metadata: None,
        entity_context: None,
        content_type: None,
        title: None,
        raw: None,
    };
    let (replacement_doc_id, _) = engine
        .ingest(&org.id, Some(&user.id), &replacement_req)
        .await
        .expect("replacement ingest");
    for _ in 0..100 {
        let document = db
            .get_document_by_id(&replacement_doc_id)
            .await
            .expect("replacement document");
        if document.status == memoricai_core::enums::DocumentStatus::Done {
            break;
        }
        if document.status == memoricai_core::enums::DocumentStatus::Failed {
            panic!("replacement ingest failed");
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(
        !db.get_memory(&org.id, &predecessor_id)
            .await
            .expect("superseded predecessor")
            .is_latest
    );
    db.delete_document(&org.id, &replacement_doc_id, None)
        .await
        .expect("delete replacement document");
    assert!(
        db.get_memory(&org.id, &predecessor_id)
            .await
            .expect("restored predecessor")
            .is_latest
    );

    // Publishing is lease-fenced and atomic even when a database write fails after chunk
    // deletion has begun. A NaN vector is intentionally rejected by pgvector.
    let old_chunk_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM chunks WHERE document_id=$1")
            .bind(&id)
            .fetch_one(&db.pool)
            .await
            .expect("count old chunks");
    let old_memory_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM memories WHERE document_id=$1")
            .bind(&id)
            .fetch_one(&db.pool)
            .await
            .expect("count old memories");
    sqlx::query(
        "UPDATE documents SET status='indexing', lease_token='atomic-test',
                lease_until=now()+interval '5 minutes' WHERE id=$1",
    )
    .bind(&id)
    .execute(&db.pool)
    .await
    .expect("prepare atomic replacement");
    let chunks = vec![(
        "replacement chunk".to_string(),
        0,
        "text".to_string(),
        vec![0.5; 64],
        serde_json::json!({}),
    )];
    let memories = vec![memoricai_db::memories::ExtractedMemoryDraft {
        user_id: Some(user.id.clone()),
        container_tag: format!("mc_project_{}", &org.id[4..12]),
        content: "replacement memory".into(),
        embedding: vec![f32::NAN; 64],
        is_static: false,
        forget_after: None,
        event_date: None,
        bucket_key: None,
    }];
    let fenced = db
        .replace_document_index(
            &id,
            "wrong-token",
            &org.id,
            &embedding_index.id,
            &[format!("mc_project_{}", &org.id[4..12])],
            &chunks,
            &[],
            None,
            None,
            1,
        )
        .await;
    assert!(fenced.is_err(), "a stale lease token must be rejected");
    let failed_publish = db
        .replace_document_index(
            &id,
            "atomic-test",
            &org.id,
            &embedding_index.id,
            &[format!("mc_project_{}", &org.id[4..12])],
            &chunks,
            &memories,
            None,
            None,
            1,
        )
        .await;
    assert!(
        failed_publish.is_err(),
        "invalid vector should fail publish"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM chunks WHERE document_id=$1")
            .bind(&id)
            .fetch_one(&db.pool)
            .await
            .expect("count chunks after rollback"),
        old_chunk_count
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM memories WHERE document_id=$1")
            .bind(&id)
            .fetch_one(&db.pool)
            .await
            .expect("count memories after rollback"),
        old_memory_count
    );
    sqlx::query("UPDATE documents SET lease_until=now()-interval '1 second' WHERE id=$1")
        .bind(&id)
        .execute(&db.pool)
        .await
        .expect("expire lease");
    assert!(
        db.renew_document_lease(&id, "atomic-test").await.is_err(),
        "an expired lease must not be revivable"
    );
    assert!(
        db.replace_document_index(
            &id,
            "atomic-test",
            &org.id,
            &embedding_index.id,
            &[format!("mc_project_{}", &org.id[4..12])],
            &chunks,
            &[],
            None,
            None,
            1,
        )
        .await
        .is_err(),
        "an expired lease must not publish"
    );

    // A model-version change creates a second isolated index and backfills all
    // retained memory/chunk text without overwriting the first version.
    let mut v2_models = ModelStack::for_tests(64);
    v2_models.embedding_model.version = "test-v2".into();
    let _v2_engine = Engine::new(
        db.clone(),
        Arc::new(v2_models),
        EngineConfig {
            ingest_concurrency: 1,
            chunk_chars: 400,
        },
    );
    let mut v2_index = None;
    for _ in 0..200 {
        v2_index = db
            .embedding_indexes(&org.id)
            .await
            .expect("list embedding indexes")
            .into_iter()
            .find(|index| index.model_version == "test-v2");
        if v2_index.is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let v2_index = v2_index.expect("second embedding index");
    assert_ne!(v2_index.id, embedding_index.id);
    let mut v2_complete = false;
    for _ in 0..200 {
        v2_complete = sqlx::query_scalar(
            "SELECT
               NOT EXISTS (
                 SELECT 1 FROM memories m WHERE m.org_id=$1 AND NOT EXISTS (
                   SELECT 1 FROM memory_embeddings e
                   WHERE e.index_id=$2 AND e.memory_id=m.id))
               AND NOT EXISTS (
                 SELECT 1 FROM chunks c WHERE c.org_id=$1 AND NOT EXISTS (
                   SELECT 1 FROM chunk_embeddings e
                   WHERE e.index_id=$2 AND e.chunk_id=c.id))",
        )
        .bind(&org.id)
        .bind(&v2_index.id)
        .fetch_one(&db.pool)
        .await
        .expect("check versioned embedding backfill");
        if v2_complete {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(v2_complete, "second model version was not fully backfilled");
}
