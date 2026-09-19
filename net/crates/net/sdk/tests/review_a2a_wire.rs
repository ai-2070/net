//! Review-only wire witness; not part of the implementation branch.
#![cfg(all(feature = "net", feature = "cortex"))]
use net_sdk::a2a::{A2aBounds, A2aOffer, CancelToken, TaskBrief, TaskExecutor, TaskRegistry};
use net_sdk::mesh::{Mesh, MeshBuilder};
use net_sdk::mesh_a2a::{A2aServiceConfig, A2aServicePolicy};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

struct Exec;
#[async_trait::async_trait]
impl TaskExecutor for Exec {
    async fn run(&self, _: TaskBrief, _: CancelToken) -> Result<String, String> {
        Ok("artifact:done".into())
    }
}
async fn mesh() -> Mesh {
    MeshBuilder::new("127.0.0.1:0", &[0x5a; 32])
        .unwrap()
        .build()
        .await
        .unwrap()
}
async fn check(description_len: usize) {
    let host = mesh().await;
    let caller = mesh().await;
    let offer = A2aOffer {
        service_id: "summarize".into(),
        revision: "r1".into(),
        description: Some("x".repeat(description_len)),
        pricing_terms: None,
        bounds: A2aBounds {
            max_prompt_bytes: 512,
            max_context_refs: 4,
            max_tags: 4,
            max_tag_bytes: 32,
            max_in_flight: 2,
        },
        reservation_ttl_secs: 600,
        reservation_retention_secs: 604800,
        retention_secs: 3600,
    };
    let _serving = host
        .serve_a2a_configured(
            TaskRegistry::new(),
            Arc::new(Exec),
            A2aServiceConfig::new(BTreeMap::from([(
                "summarize".into(),
                A2aServicePolicy::Free(offer),
            )])),
        )
        .unwrap();
    let addr = host.inner().local_addr();
    let key = *host.inner().public_key();
    let target = host.inner().node_id();
    let cid = caller.inner().node_id();
    let (a, c) = tokio::join!(host.inner().accept(cid), async {
        tokio::time::sleep(Duration::from_millis(50)).await;
        caller.inner().connect(addr, &key, target).await
    });
    a.unwrap();
    c.unwrap();
    caller.start();
    host.start();
    let result = tokio::time::timeout(Duration::from_secs(35), caller.describe_a2a(target)).await;
    assert!(
        result.is_ok(),
        "accepted offer description {description_len} bytes was not delivered: {result:?}"
    );
    let offers = result.unwrap().expect("describe reply");
    assert_eq!(offers.len(), 1);
    assert_eq!(
        offers[0].description.as_ref().unwrap().len(),
        description_len
    );
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn review_short_description_control() {
    check(64).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn review_accepted_description_is_discoverable() {
    check(4096).await;
}
