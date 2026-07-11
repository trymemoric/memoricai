//! Live integration test for the Rust SDK against a running memoricai server.
//! Set MEMORICAI_SDK_TEST_URL and MEMORICAI_SDK_TEST_KEY to run:
//!
//!   MEMORICAI_SDK_TEST_URL=http://localhost:6767 \
//!   MEMORICAI_SDK_TEST_KEY=mc_... \
//!   cargo test -p memoricai-client -- --ignored

use std::time::Duration;

use memoricai_client::{Client, MemorySearchRequest, ProfileRequest};

#[tokio::test]
#[ignore = "requires MEMORICAI_SDK_TEST_URL + MEMORICAI_SDK_TEST_KEY pointing at a live server"]
async fn sdk_roundtrip_against_live_server() {
    let url = std::env::var("MEMORICAI_SDK_TEST_URL").expect("MEMORICAI_SDK_TEST_URL");
    let key = std::env::var("MEMORICAI_SDK_TEST_KEY").expect("MEMORICAI_SDK_TEST_KEY");
    let client = Client::new(url, key);
    let tag = "mc_project_sdk_rust";

    client.health().await.expect("health");

    let doc = client
        .add_text(
            "The Rust SDK smoke fact: Ada Lovelace wrote the first program in 1843.",
            tag,
        )
        .await
        .expect("add_text");
    assert_eq!(doc.status, "queued");

    let done = client
        .wait_for_document(&doc.id, Duration::from_secs(120))
        .await
        .expect("wait_for_document");
    assert_eq!(done.id, doc.id);

    let res = client
        .search_memories(&MemorySearchRequest {
            q: "who wrote the first program".into(),
            container_tag: Some(tag.into()),
            threshold: 0.05,
            digest: true,
            ..Default::default()
        })
        .await
        .expect("search_memories");
    assert!(res.total > 0, "expected results, got {res:?}");
    let digest = res.digest.expect("digest requested");
    assert!(
        digest.contains("Ada"),
        "digest should mention Ada: {digest}"
    );

    let profile = client
        .profile(&ProfileRequest {
            container_tag: tag.into(),
            q: None,
            threshold: None,
            filters: None,
            include: None,
            buckets: None,
        })
        .await
        .expect("profile");
    let p = profile.profile;
    assert!(p.r#static.is_some() || p.dynamic.is_some(), "profile empty");
}
