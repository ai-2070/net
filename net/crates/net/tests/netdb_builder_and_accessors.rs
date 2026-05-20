//! Invariant tests for the `NetDb` / `NetDbBuilder` boundary and
//! single-model surface in `src/adapter/net/netdb/db.rs`:
//!
//!   * `build()` early-return on no models enabled.
//!   * `build_from_snapshot()` early-return on no models enabled.
//!   * `snapshot()` None branches when only one model is enabled —
//!     pins the asymmetry between enabled and disabled model slots.
//!
//! Existing `integration_netdb.rs` covers happy-path build / CRUD /
//! whole-db snapshot — these tests pin the boundary + single-model
//! contract that integration tests don't directly hit.

#![cfg(feature = "netdb")]

use net::adapter::net::netdb::{NetDb, NetDbError, NetDbSnapshot};
use net::adapter::net::redex::Redex;

const ORIGIN: u64 = 0xABCD_EF01;

#[tokio::test]
async fn build_returns_no_models_enabled_when_neither_model_selected() {
    let redex = Redex::new();
    // No `.with_tasks()` and no `.with_memories()` — the builder
    // should refuse rather than return a no-op NetDb whose accessors
    // panic on first call. Pre-fix this returned `Ok` and the panic
    // surfaced downstream; the typed error makes the misconfig
    // observable at build time.
    let result = NetDb::builder(redex).origin(ORIGIN).build().await;
    assert!(
        matches!(result, Err(NetDbError::NoModelsEnabled)),
        "expected NoModelsEnabled, got {:?}",
        result.map(|_| "Ok(NetDb)")
    );
}

#[tokio::test]
async fn build_from_snapshot_returns_no_models_enabled_when_neither_model_selected() {
    let redex = Redex::new();
    let empty = NetDbSnapshot {
        tasks: None,
        memories: None,
    };
    let result = NetDb::builder(redex)
        .origin(ORIGIN)
        .build_from_snapshot(&empty)
        .await;
    assert!(
        matches!(result, Err(NetDbError::NoModelsEnabled)),
        "expected NoModelsEnabled, got {:?}",
        result.map(|_| "Ok(NetDb)")
    );
}

#[tokio::test]
async fn snapshot_omits_memories_when_only_tasks_enabled() {
    let redex = Redex::new();
    let db = NetDb::builder(redex)
        .origin(ORIGIN)
        .with_tasks()
        .build()
        .await
        .unwrap();

    let snap = db.snapshot().expect("snapshot");
    assert!(
        snap.tasks.is_some(),
        "tasks model was enabled — snapshot must capture it"
    );
    assert!(
        snap.memories.is_none(),
        "memories model was NOT enabled — snapshot's memories slot must stay None"
    );
}

#[tokio::test]
async fn snapshot_omits_tasks_when_only_memories_enabled() {
    let redex = Redex::new();
    let db = NetDb::builder(redex)
        .origin(ORIGIN)
        .with_memories()
        .build()
        .await
        .unwrap();

    let snap = db.snapshot().expect("snapshot");
    assert!(
        snap.tasks.is_none(),
        "tasks model was NOT enabled — snapshot's tasks slot must stay None"
    );
    assert!(
        snap.memories.is_some(),
        "memories model was enabled — snapshot must capture it"
    );
}
