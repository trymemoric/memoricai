//! End-to-end test for `POST /v1/admin/provision` against a real Postgres+pgvector.
//! Set `MEMORICAI_TEST_DATABASE_URL` to run it; otherwise it is skipped.
//!
//!   createdb memoricai_test
//!   psql -d memoricai_test -c 'CREATE EXTENSION IF NOT EXISTS vector;'
//!   MEMORICAI_TEST_DATABASE_URL=postgres://$USER@localhost/memoricai_test cargo test -p memoricai --test admin_provision_e2e -- --ignored

use std::sync::Arc;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use memoricai_api::{build_router, AppState};
use memoricai_auth::AuthService;
use memoricai_db::Db;
use memoricai_engine::{Engine, EngineConfig};
use memoricai_models::ModelStack;
use serde_json::{json, Value};
use tower::ServiceExt;

async fn state_with_provision_key(db: Db, provision_key: Option<&str>) -> AppState {
    let models = Arc::new(ModelStack::for_tests(64));
    let engine = Engine::new(
        db.clone(),
        models,
        EngineConfig {
            ingest_concurrency: 2,
            chunk_chars: 400,
        },
    );
    let auth = Arc::new(AuthService::new(db));
    AppState {
        engine,
        auth,
        request_body_timeout: std::time::Duration::from_secs(30),
        router_allowed_origins: Arc::new(Vec::new()),
        provision_key: provision_key.map(Arc::from),
    }
}

async fn body_json(response: axum::response::Response) -> Value {
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    serde_json::from_slice(&bytes).expect("parse json body")
}

#[tokio::test]
#[ignore = "requires MEMORICAI_TEST_DATABASE_URL pointing to Postgres with pgvector"]
async fn admin_provision_end_to_end() {
    let url = std::env::var("MEMORICAI_TEST_DATABASE_URL")
        .expect("MEMORICAI_TEST_DATABASE_URL is required for this ignored test");

    let db = Db::connect(&url).await.expect("connect");
    db.migrate().await.expect("migrate");

    const PROVISION_KEY: &str = "test-provision-key";
    let enabled_state = state_with_provision_key(db.clone(), Some(PROVISION_KEY)).await;
    let enabled_router = build_router(enabled_state);

    let provision_request = |bearer: Option<&str>, body: Value| {
        let mut builder = Request::builder()
            .method("POST")
            .uri("/v1/admin/provision")
            .header("content-type", "application/json");
        if let Some(token) = bearer {
            builder = builder.header("authorization", format!("Bearer {token}"));
        }
        builder
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap()
    };

    // (a) no bearer / wrong bearer -> 401
    let no_bearer_body = json!({ "orgName": "Acme", "email": "no-bearer@memoricai-itest.local" });
    let resp = enabled_router
        .clone()
        .oneshot(provision_request(None, no_bearer_body))
        .await
        .expect("no-bearer request");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    let wrong_bearer_body =
        json!({ "orgName": "Acme", "email": "wrong-bearer@memoricai-itest.local" });
    let resp = enabled_router
        .clone()
        .oneshot(provision_request(Some("not-the-key"), wrong_bearer_body))
        .await
        .expect("wrong-bearer request");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    // (b) provision_key None -> 404 (the route is not mounted at all, so this
    // is axum's plain fallback 404, not the handler's in-band JSON 404).
    let disabled_state = state_with_provision_key(db.clone(), None).await;
    let disabled_router = build_router(disabled_state);
    let disabled_body = json!({ "orgName": "Acme", "email": "disabled@memoricai-itest.local" });
    let resp = disabled_router
        .clone()
        .oneshot(provision_request(Some(PROVISION_KEY), disabled_body))
        .await
        .expect("disabled request");
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    // (b2) with the route unmounted, even a wrong-method probe (no body, no
    // auth) must hit the same fallback 404 rather than a routed 405 -
    // otherwise the method mismatch alone would reveal the path exists.
    let wrong_method_req = Request::builder()
        .method("GET")
        .uri("/v1/admin/provision")
        .body(Body::empty())
        .unwrap();
    let resp = disabled_router
        .oneshot(wrong_method_req)
        .await
        .expect("wrong-method request");
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    // (c) correct key -> 201, response has orgId and apiKey with prefix mc_<org_id>_
    let org_a_email = format!("org-a-{}@memoricai-itest.local", uniq());
    let org_a_body = json!({ "orgName": "Org A", "email": org_a_email });
    let resp = enabled_router
        .clone()
        .oneshot(provision_request(Some(PROVISION_KEY), org_a_body))
        .await
        .expect("org a provision request");
    assert_eq!(resp.status(), StatusCode::CREATED);
    let org_a = body_json(resp).await;
    let org_a_id = org_a["orgId"].as_str().expect("orgId").to_string();
    let org_a_key = org_a["apiKey"].as_str().expect("apiKey").to_string();
    assert!(org_a_id.starts_with("org_"));
    let org_a_short = org_a_id.strip_prefix("org_").unwrap_or(&org_a_id);
    assert!(
        org_a_key.starts_with(&format!("mc_{org_a_short}_")),
        "unexpected api key shape: {org_a_key}"
    );

    // (d) the returned api_key authenticates GET /v1/session, org in response matches
    let session_req = Request::builder()
        .method("GET")
        .uri("/v1/session")
        .header("authorization", format!("Bearer {org_a_key}"))
        .body(Body::empty())
        .unwrap();
    let resp = enabled_router
        .clone()
        .oneshot(session_req)
        .await
        .expect("session request");
    assert_eq!(resp.status(), StatusCode::OK);
    let session = body_json(resp).await;
    assert_eq!(session["org"]["id"].as_str(), Some(org_a_id.as_str()));

    // (e) a second provisioned org's key cannot read org A's data.
    let org_b_email = format!("org-b-{}@memoricai-itest.local", uniq());
    let org_b_body = json!({ "orgName": "Org B", "email": org_b_email });
    let resp = enabled_router
        .clone()
        .oneshot(provision_request(Some(PROVISION_KEY), org_b_body))
        .await
        .expect("org b provision request");
    assert_eq!(resp.status(), StatusCode::CREATED);
    let org_b = body_json(resp).await;
    let org_b_key = org_b["apiKey"].as_str().expect("apiKey").to_string();

    let shared_tag = format!("mc_isolation_probe_{}", uniq());

    // Org A creates a bucket under the shared tag.
    let create_bucket_req = Request::builder()
        .method("POST")
        .uri("/v1/buckets")
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {org_a_key}"))
        .body(Body::from(
            serde_json::to_vec(&json!({
                "containerTag": shared_tag,
                "key": "isolation_probe",
                "description": "org A only",
            }))
            .unwrap(),
        ))
        .unwrap();
    let resp = enabled_router
        .clone()
        .oneshot(create_bucket_req)
        .await
        .expect("create bucket request");
    assert_eq!(resp.status(), StatusCode::OK);

    // Org B lists buckets for the same tag string and must not see org A's bucket.
    let list_buckets_req = Request::builder()
        .method("POST")
        .uri("/v1/profile/buckets")
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {org_b_key}"))
        .body(Body::from(
            serde_json::to_vec(&json!({ "containerTag": shared_tag })).unwrap(),
        ))
        .unwrap();
    let resp = enabled_router
        .oneshot(list_buckets_req)
        .await
        .expect("list buckets request");
    assert_eq!(resp.status(), StatusCode::OK);
    let buckets = body_json(resp).await;
    // `list_buckets` always synthesizes a built-in `preferences` entry, so assert on the
    // absence of org A's specific bucket rather than requiring an empty list.
    let leaked = buckets["buckets"]
        .as_array()
        .expect("buckets array")
        .iter()
        .any(|b| b["key"] == "isolation_probe");
    assert!(
        !leaked,
        "org B must not see org A's bucket, got {buckets:?}"
    );
}

/// A padded email (leading/trailing whitespace) must dedup to the *same*
/// user as its trimmed form, mirroring how `orgName` is already trimmed
/// before use. Regression test for the email-not-trimmed bug where a padded
/// email bypassed `bootstrap_org`'s by-email dedup and created a duplicate
/// user bound to the wrong identity.
#[tokio::test]
#[ignore = "requires MEMORICAI_TEST_DATABASE_URL pointing to Postgres with pgvector"]
async fn admin_provision_trims_email_for_dedup() {
    let url = std::env::var("MEMORICAI_TEST_DATABASE_URL")
        .expect("MEMORICAI_TEST_DATABASE_URL is required for this ignored test");

    let db = Db::connect(&url).await.expect("connect");
    db.migrate().await.expect("migrate");

    const PROVISION_KEY: &str = "test-provision-key";
    let state = state_with_provision_key(db.clone(), Some(PROVISION_KEY)).await;
    let router = build_router(state);

    let provision_request = |body: Value| {
        Request::builder()
            .method("POST")
            .uri("/v1/admin/provision")
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {PROVISION_KEY}"))
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap()
    };

    let email = format!("padded-{}@memoricai-itest.local", uniq());

    // First provision with the clean email.
    let first_body = json!({ "orgName": "Padded Org 1", "email": email });
    let resp = router
        .clone()
        .oneshot(provision_request(first_body))
        .await
        .expect("first provision request");
    assert_eq!(resp.status(), StatusCode::CREATED);
    let first = body_json(resp).await;
    let first_user_id = first["userId"].as_str().expect("userId").to_string();

    // Second provision, same email but padded with leading/trailing whitespace.
    let padded_email = format!("  {email}  ");
    let second_body = json!({ "orgName": "Padded Org 2", "email": padded_email });
    let resp = router
        .clone()
        .oneshot(provision_request(second_body))
        .await
        .expect("second provision request");
    assert_eq!(resp.status(), StatusCode::CREATED);
    let second = body_json(resp).await;
    let second_user_id = second["userId"].as_str().expect("userId").to_string();

    assert_eq!(
        first_user_id, second_user_id,
        "a padded email must dedup to the same user as its trimmed form"
    );
    // Sanity: they are indeed two distinct orgs sharing one user.
    assert_ne!(first["orgId"], second["orgId"]);
}

/// A short unique-ish suffix so repeated runs of this test don't collide on
/// unique columns (e.g. `users.email`).
fn uniq() -> String {
    format!(
        "{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    )
}
