//! Review-only fixture setup for the actual Python recovery boundary.
//! Archives a real Paid attempt through production store APIs. Removing its
//! live entry is explicit fixture supersession, not a claim of Python reachability.
use net_payments::flow::a2a::{A2aPurchaseFile, A2aPurchaseStore, PurchaseState, RefusalRecord};
use net_payments::policy::store::mutate_json_if_changed;
#[tokio::test]
async fn review_seed_python_history() {
    let path = std::env::var("NET_REVIEW_PURCHASE_PATH").expect("test fixture store path required");
    let store = A2aPurchaseStore::new(&path);
    let attempts = store.attempts().await.unwrap();
    assert_eq!(attempts.len(), 1);
    let old = attempts[0].clone();
    let (proof, billing) = match &old.state {
        PurchaseState::Paid { proof, billing } => (proof.clone(), billing.clone()),
        _ => panic!("fixture must originate in actual paid flow"),
    };
    let archived = PurchaseState::PaidUnexecutable {
        proof,
        billing,
        refusal: RefusalRecord {
            at_ns: old.updated_at_ns,
            message: "review fixture supersession".into(),
            reason: Some("superseded_attempt".into()),
            safe_to_retry: false,
            safe_to_requote: false,
        },
    };
    store
        .retain_superseded(&old, archived, old.updated_at_ns)
        .await
        .unwrap();
    let key = old.key.id();
    mutate_json_if_changed::<A2aPurchaseFile, _, _>(std::path::Path::new(&path), move |file| {
        assert_eq!(file.attempts.get(&key), Some(&old));
        file.attempts.remove(&key);
        ((), true)
    })
    .await
    .unwrap();
    assert!(store.attempt(&attempts[0].key).await.unwrap().is_none());
    assert_eq!(store.retained_attempts().await.unwrap().len(), 1);
}
