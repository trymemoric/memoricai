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
}
