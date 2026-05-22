//! Framework-level tests for the generic fold runtime.
//!
//! Concrete-fold tests live alongside the impls in
//! `capability.rs` / `routing.rs` / `reservation.rs`; this module
//! uses synthetic `CapFold` / `RoutingTestFold` shapes to pin the
//! runtime contract (apply, query, merge, evict, snapshot,
//! restore, metrics, wire codec, sign / verify, dispatch, audit,
//! TTL sweep).

use std::collections::HashSet;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::*;

/// Synthetic "capability-shaped" fold for the simple-runtime
/// tests. Key is `(class, node_id)`; payload is a small struct
/// carrying tags so the secondary-index hook has something to
/// see; query is "all entries in class C tagged with T".
struct CapFold;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct CapPayload {
    class_hash: u64,
    tags: Vec<String>,
}

#[derive(Debug, Clone)]
struct CapQuery {
    class: u64,
    required_tag: Option<String>,
}

#[derive(Default)]
struct CapIndex {
    /// tag → set of (class, node_id) keys carrying that tag.
    by_tag: std::collections::HashMap<String, HashSet<(u64, NodeId)>>,
}

impl FoldIndex<CapFold> for CapIndex {
    fn on_insert(&mut self, key: &(u64, NodeId), payload: &CapPayload) {
        for tag in &payload.tags {
            self.by_tag.entry(tag.clone()).or_default().insert(*key);
        }
    }

    fn on_remove(&mut self, key: &(u64, NodeId), payload: &CapPayload) {
        for tag in &payload.tags {
            if let Some(set) = self.by_tag.get_mut(tag) {
                set.remove(key);
                if set.is_empty() {
                    self.by_tag.remove(tag);
                }
            }
        }
    }

    fn clear(&mut self) {
        self.by_tag.clear();
    }
}

impl FoldKind for CapFold {
    const KIND_ID: u16 = 0x0F00;
    const CHANNEL_PREFIX: &'static str = "test:cap:";
    const DEFAULT_TTL: Duration = Duration::from_secs(60);
    type Key = (u64, NodeId);
    type Payload = CapPayload;
    type Query = CapQuery;
    type Result = Vec<(u64, NodeId)>;
    type Index = CapIndex;

    fn key_for(node_id: NodeId, payload: &CapPayload) -> Self::Key {
        (payload.class_hash, node_id)
    }

    fn build_index() -> CapIndex {
        CapIndex::default()
    }

    fn query(state: &FoldState<Self>, index: &CapIndex, q: CapQuery) -> Vec<(u64, NodeId)> {
        match &q.required_tag {
            Some(tag) => {
                // Use the inverted index for tag selectivity, then
                // filter by class against the primary store.
                index
                    .by_tag
                    .get(tag)
                    .into_iter()
                    .flat_map(|set| set.iter())
                    .filter(|(class, _)| *class == q.class)
                    .copied()
                    .collect()
            }
            None => state
                .entries
                .iter()
                .filter(|((class, _), _)| *class == q.class)
                .map(|(k, _)| *k)
                .collect(),
        }
    }
}

fn cap_announcement(
    node_id: NodeId,
    class: u64,
    generation: u64,
    tags: Vec<&str>,
) -> SignedAnnouncement<CapPayload> {
    SignedAnnouncement::placeholder(
        CapFold::KIND_ID,
        class,
        node_id,
        generation,
        EnvelopeMeta::default(),
        CapPayload {
            class_hash: class,
            tags: tags.into_iter().map(String::from).collect(),
        },
    )
}

#[test]
fn apply_then_query_round_trips_a_single_announcement() {
    let fold: Fold<CapFold> = Fold::new();
    let outcome = fold
        .apply(cap_announcement(0x42, 0x1000, 1, vec!["gpu", "h100"]))
        .expect("apply succeeds");
    assert_eq!(outcome, ApplyOutcome::Inserted);
    assert_eq!(fold.metrics().applies_inserted(), 1);
    assert_eq!(fold.metrics().entries(), 1);

    let hits = fold.query(CapQuery {
        class: 0x1000,
        required_tag: Some("h100".into()),
    });
    assert_eq!(hits, vec![(0x1000, 0x42)]);
}

#[test]
fn stale_generation_is_rejected_by_default_merge() {
    let fold: Fold<CapFold> = Fold::new();
    fold.apply(cap_announcement(0x42, 0x1000, 5, vec!["gpu"]))
        .expect("gen=5 accepted");

    // Equal-generation: rejected.
    let outcome = fold
        .apply(cap_announcement(0x42, 0x1000, 5, vec!["different-tags"]))
        .expect("apply returns Ok with Rejected outcome");
    assert_eq!(outcome, ApplyOutcome::Rejected);

    // Lower-generation: rejected.
    let outcome = fold
        .apply(cap_announcement(0x42, 0x1000, 3, vec!["even-different"]))
        .expect("apply returns Ok with Rejected outcome");
    assert_eq!(outcome, ApplyOutcome::Rejected);

    assert_eq!(fold.metrics().applies_inserted(), 1);
    assert_eq!(fold.metrics().applies_rejected(), 2);

    // Original entry's tags are intact (the rejected announcements
    // never reached the primary store).
    fold.with_state(|state| {
        let entry = state.entries.get(&(0x1000, 0x42)).expect("entry present");
        assert_eq!(entry.generation, 5);
        assert_eq!(entry.payload.tags, vec!["gpu".to_string()]);
    });
}

#[test]
fn higher_generation_replaces_existing_entry_and_index() {
    let fold: Fold<CapFold> = Fold::new();
    fold.apply(cap_announcement(0x42, 0x1000, 1, vec!["old-tag"]))
        .expect("gen=1 accepted");
    let outcome = fold
        .apply(cap_announcement(0x42, 0x1000, 2, vec!["new-tag"]))
        .expect("gen=2 accepted");
    assert_eq!(outcome, ApplyOutcome::Replaced);
    assert_eq!(fold.metrics().applies_replaced(), 1);

    // Old tag must NOT match anymore (index was rebuilt on replace).
    let old_hits = fold.query(CapQuery {
        class: 0x1000,
        required_tag: Some("old-tag".into()),
    });
    assert!(old_hits.is_empty(), "stale tag must be evicted from index");

    let new_hits = fold.query(CapQuery {
        class: 0x1000,
        required_tag: Some("new-tag".into()),
    });
    assert_eq!(new_hits, vec![(0x1000, 0x42)]);
}

#[test]
fn generation_zero_is_refused_with_invalid_generation_error() {
    let fold: Fold<CapFold> = Fold::new();
    let result = fold.apply(cap_announcement(0x42, 0x1000, 0, vec!["gpu"]));
    match result {
        Err(FoldError::InvalidGeneration { node_id }) => assert_eq!(node_id, 0x42),
        other => panic!("expected InvalidGeneration, got {other:?}"),
    }
    assert_eq!(fold.metrics().applies_rejected(), 1);
    assert_eq!(fold.metrics().entries(), 0);
}

#[test]
fn evict_node_drops_every_entry_and_index_attachment_for_that_node() {
    let fold: Fold<CapFold> = Fold::new();
    // Node 0x42 carries TWO entries (two classes).
    fold.apply(cap_announcement(0x42, 0x1000, 1, vec!["gpu"]))
        .expect("first apply");
    fold.apply(cap_announcement(0x42, 0x2000, 1, vec!["tpu"]))
        .expect("second apply");
    // Node 0x43 carries one entry to confirm it's NOT evicted.
    fold.apply(cap_announcement(0x43, 0x1000, 1, vec!["gpu"]))
        .expect("third apply");

    assert_eq!(fold.metrics().entries(), 3);

    fold.evict_node(0x42, "test");

    assert_eq!(fold.metrics().entries(), 1);
    assert_eq!(fold.metrics().evictions(), 2);

    // Surviving entry is the 0x43 one.
    fold.with_state(|state| {
        assert!(state.entries.contains_key(&(0x1000, 0x43)));
        assert!(!state.entries.contains_key(&(0x1000, 0x42)));
        assert!(!state.entries.contains_key(&(0x2000, 0x42)));
        // by_node reverse index must be cleared for the evicted node.
        assert!(!state.by_node.contains_key(&0x42));
    });

    // Index must also be cleaned up — querying for the evicted
    // node's tags returns only 0x43.
    let gpu_hits: HashSet<_> = fold
        .query(CapQuery {
            class: 0x1000,
            required_tag: Some("gpu".into()),
        })
        .into_iter()
        .collect();
    assert_eq!(gpu_hits, [(0x1000, 0x43)].into_iter().collect());

    let tpu_hits = fold.query(CapQuery {
        class: 0x2000,
        required_tag: Some("tpu".into()),
    });
    assert!(tpu_hits.is_empty());
}

#[test]
fn snapshot_round_trips_via_restore() {
    let fold: Fold<CapFold> = Fold::new();
    fold.apply(cap_announcement(0x42, 0x1000, 1, vec!["gpu", "h100"]))
        .expect("apply #1");
    fold.apply(cap_announcement(0x43, 0x1000, 1, vec!["gpu"]))
        .expect("apply #2");
    fold.apply(cap_announcement(0x42, 0x2000, 1, vec!["tpu"]))
        .expect("apply #3");

    let snap = fold.snapshot();
    assert_eq!(snap.kind, CapFold::KIND_ID);
    assert_eq!(snap.entries.len(), 3);

    // Restore into a fresh fold.
    let restored: Fold<CapFold> = Fold::new();
    restored.restore(snap, false).expect("restore succeeds");

    assert_eq!(restored.metrics().entries(), 3);
    assert_eq!(restored.metrics().snapshots_restored(), 1);

    // Index is repopulated — tag query works against restored state.
    let h100_hits = restored.query(CapQuery {
        class: 0x1000,
        required_tag: Some("h100".into()),
    });
    assert_eq!(h100_hits, vec![(0x1000, 0x42)]);

    // Apply after restore advances generation past the restored
    // entry, exercising the "restored entries lose to newer live
    // applies" property the plan calls out.
    restored
        .apply(cap_announcement(0x42, 0x1000, 2, vec!["new-tag"]))
        .expect("post-restore apply");
    let new_tag = restored.query(CapQuery {
        class: 0x1000,
        required_tag: Some("new-tag".into()),
    });
    assert_eq!(new_tag, vec![(0x1000, 0x42)]);
}

#[test]
fn restore_over_live_state_without_force_is_refused() {
    let fold: Fold<CapFold> = Fold::new();
    fold.apply(cap_announcement(0x42, 0x1000, 1, vec!["gpu"]))
        .expect("apply");
    let snap = fold.snapshot();

    let live: Fold<CapFold> = Fold::new();
    live.apply(cap_announcement(0x43, 0x1000, 1, vec!["different"]))
        .expect("apply on live");

    match live.restore(snap, false) {
        Err(FoldError::RestoreOverLiveState { current_len }) => assert_eq!(current_len, 1),
        other => panic!("expected RestoreOverLiveState, got {other:?}"),
    }

    // Live state must NOT have been touched (the restore aborted
    // before mutating).
    live.with_state(|state| {
        assert_eq!(state.entries.len(), 1);
        assert!(state.entries.contains_key(&(0x1000, 0x43)));
    });
}

/// Routing-style fold: lower-metric wins, generation is just a
/// tiebreaker. Exercises the [`FoldKind::merge`] override path.
struct RoutingTestFold;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct RoutePayload {
    destination: NodeId,
    metric: u32,
}

impl FoldKind for RoutingTestFold {
    const KIND_ID: u16 = 0x0F01;
    const CHANNEL_PREFIX: &'static str = "test:route:";
    const DEFAULT_TTL: Duration = Duration::from_secs(300);
    type Key = NodeId;
    type Payload = RoutePayload;
    type Query = NodeId;
    type Result = Option<RoutePayload>;
    type Index = NoIndex;

    fn key_for(_node_id: NodeId, payload: &RoutePayload) -> NodeId {
        payload.destination
    }

    fn build_index() -> NoIndex {
        NoIndex
    }

    fn merge(
        existing: Option<&FoldEntry<Self>>,
        incoming: &SignedAnnouncement<RoutePayload>,
    ) -> MergeAction {
        match existing {
            None => MergeAction::Insert,
            Some(e) if incoming.payload.metric < e.payload.metric => MergeAction::Replace,
            _ => MergeAction::Reject,
        }
    }

    fn query(state: &FoldState<Self>, _index: &NoIndex, dest: NodeId) -> Option<RoutePayload> {
        state.entries.get(&dest).map(|e| e.payload.clone())
    }
}

fn route_announcement(
    publisher: NodeId,
    dest: NodeId,
    metric: u32,
    generation: u64,
) -> SignedAnnouncement<RoutePayload> {
    SignedAnnouncement::placeholder(
        RoutingTestFold::KIND_ID,
        0,
        publisher,
        generation,
        EnvelopeMeta::default(),
        RoutePayload {
            destination: dest,
            metric,
        },
    )
}

#[test]
fn routing_merge_override_picks_lower_metric_across_publishers() {
    let fold: Fold<RoutingTestFold> = Fold::new();
    fold.apply(route_announcement(0xAA, 0x99, 50, 1))
        .expect("publisher AA accepted at metric 50");
    let route = fold.query(0x99).expect("destination present");
    assert_eq!(route.metric, 50);

    // Different publisher with LOWER metric wins.
    fold.apply(route_announcement(0xBB, 0x99, 20, 1))
        .expect("publisher BB accepted at metric 20");
    let route = fold.query(0x99).expect("destination still present");
    assert_eq!(route.metric, 20);

    // Higher metric loses, even if generation advances.
    let outcome = fold
        .apply(route_announcement(0xCC, 0x99, 100, 100))
        .expect("CC rejected by metric");
    assert_eq!(outcome, ApplyOutcome::Rejected);
    let route = fold.query(0x99).expect("destination still present");
    assert_eq!(route.metric, 20, "lower-metric route must stick");
}

#[test]
fn metrics_counts_track_apply_outcomes_and_query_count() {
    let fold: Fold<CapFold> = Fold::new();

    // 3 inserts, 1 replace, 1 reject.
    fold.apply(cap_announcement(0x1, 0x100, 1, vec!["a"]))
        .unwrap();
    fold.apply(cap_announcement(0x2, 0x100, 1, vec!["b"]))
        .unwrap();
    fold.apply(cap_announcement(0x3, 0x100, 1, vec!["c"]))
        .unwrap();
    fold.apply(cap_announcement(0x1, 0x100, 2, vec!["a2"]))
        .unwrap();
    fold.apply(cap_announcement(0x2, 0x100, 1, vec!["b-stale"]))
        .unwrap();

    let m = fold.metrics();
    assert_eq!(m.applies_inserted(), 3);
    assert_eq!(m.applies_replaced(), 1);
    assert_eq!(m.applies_rejected(), 1);
    assert_eq!(m.applies_total(), 5);
    assert_eq!(m.entries(), 3);
    assert_eq!(m.queries(), 0);

    fold.query(CapQuery {
        class: 0x100,
        required_tag: None,
    });
    assert_eq!(m.queries(), 1);
}

// ---------------------------------------------------------------------
// Wire codec + sign/verify + dispatch routing
// ---------------------------------------------------------------------

use std::sync::Arc;

use crate::adapter::net::identity::EntityKeypair;

use super::wire::placeholder_signature;
use super::dispatch::{DispatchError, FoldRegistry};
use super::wire::WireError;

fn sign_cap_ann(
    keypair: &EntityKeypair,
    node_id: NodeId,
    class: u64,
    generation: u64,
    tags: Vec<&str>,
) -> SignedAnnouncement<CapPayload> {
    SignedAnnouncement::sign(
        keypair,
        CapFold::KIND_ID,
        class,
        node_id,
        generation,
        EnvelopeMeta::default(),
        CapPayload {
            class_hash: class,
            tags: tags.into_iter().map(String::from).collect(),
        },
    )
    .expect("sign succeeds with valid payload")
}

#[test]
fn signed_announcement_round_trips_through_postcard_encode_decode() {
    let kp = EntityKeypair::generate();
    let ann = sign_cap_ann(&kp, 0x42, 0x1000, 1, vec!["gpu", "h100"]);

    let bytes = ann.encode().expect("encode");
    let decoded = SignedAnnouncement::<CapPayload>::decode(&bytes).expect("decode");

    assert_eq!(decoded.kind, ann.kind);
    assert_eq!(decoded.class, ann.class);
    assert_eq!(decoded.node_id, ann.node_id);
    assert_eq!(decoded.generation, ann.generation);
    assert_eq!(decoded.announced_at, ann.announced_at);
    assert_eq!(decoded.ttl_secs, ann.ttl_secs);
    assert_eq!(decoded.flags, ann.flags);
    assert_eq!(decoded.payload, ann.payload);
    assert_eq!(decoded.signature, ann.signature);
}

#[test]
fn signature_verifies_against_publisher_identity() {
    let kp = EntityKeypair::generate();
    let ann = sign_cap_ann(&kp, 0x42, 0x1000, 1, vec!["gpu"]);
    ann.verify(kp.entity_id()).expect("verify must accept untampered envelope");
}

#[test]
fn verify_rejects_signature_from_a_different_keypair() {
    let signer = EntityKeypair::generate();
    let imposter = EntityKeypair::generate();
    let ann = sign_cap_ann(&signer, 0x42, 0x1000, 1, vec!["gpu"]);
    // Verifying against a DIFFERENT identity must fail —
    // otherwise impersonation is trivial.
    match ann.verify(imposter.entity_id()) {
        Err(WireError::InvalidSignature) => {}
        other => panic!("expected InvalidSignature, got {other:?}"),
    }
}

#[test]
fn verify_rejects_tampered_payload() {
    let kp = EntityKeypair::generate();
    let mut ann = sign_cap_ann(&kp, 0x42, 0x1000, 1, vec!["gpu"]);
    // Tamper: swap a tag. The signing bytes change but the
    // signature doesn't — verify must reject.
    ann.payload.tags = vec!["malicious-tag".into()];
    match ann.verify(kp.entity_id()) {
        Err(WireError::InvalidSignature) => {}
        other => panic!("expected InvalidSignature, got {other:?}"),
    }
}

#[test]
fn verify_rejects_placeholder_signature_sentinel() {
    // The Phase-1 placeholder constructor stamps an all-zero
    // signature. The Phase-2 verifier must catch this BEFORE
    // running the Ed25519 algorithm so unsigned envelopes can't
    // sneak through with a coincidentally-valid zero signature
    // (vanishingly unlikely but explicitly guarded).
    let kp = EntityKeypair::generate();
    let ann = cap_announcement(0x42, 0x1000, 1, vec!["gpu"]);
    assert_eq!(ann.signature, placeholder_signature());
    match ann.verify(kp.entity_id()) {
        Err(WireError::PlaceholderSignature) => {}
        other => panic!("expected PlaceholderSignature, got {other:?}"),
    }
}

#[test]
fn verify_rejects_signature_of_wrong_length() {
    let kp = EntityKeypair::generate();
    let mut ann = sign_cap_ann(&kp, 0x42, 0x1000, 1, vec!["gpu"]);
    // Truncate the signature.
    ann.signature.pop();
    match ann.verify(kp.entity_id()) {
        Err(WireError::BadSignatureLength(len)) => assert_eq!(len, 63),
        other => panic!("expected BadSignatureLength, got {other:?}"),
    }
}

#[test]
fn decode_and_verify_drives_the_dispatch_hot_path() {
    let kp = EntityKeypair::generate();
    let ann = sign_cap_ann(&kp, 0x42, 0x1000, 1, vec!["gpu"]);
    let bytes = ann.encode().expect("encode");
    let verified = SignedAnnouncement::<CapPayload>::decode_and_verify(&bytes, kp.entity_id())
        .expect("decode + verify must succeed for a freshly-signed envelope");
    assert_eq!(verified.node_id, 0x42);
    assert_eq!(verified.payload.tags, vec!["gpu".to_string()]);
}

#[test]
fn fold_registry_routes_envelope_to_correct_fold_by_kind() {
    let registry = FoldRegistry::new();
    let cap_fold: Arc<Fold<CapFold>> = Arc::new(Fold::new());
    let route_fold: Arc<Fold<RoutingTestFold>> = Arc::new(Fold::new());
    registry.register(cap_fold.clone());
    registry.register(route_fold.clone());

    assert_eq!(registry.len(), 2);
    assert!(registry.get(CapFold::KIND_ID).is_some());
    assert!(registry.get(RoutingTestFold::KIND_ID).is_some());
    assert!(registry.get(0xBADD).is_none());

    let kp = EntityKeypair::generate();
    let cap_ann = sign_cap_ann(&kp, 0x42, 0x1000, 1, vec!["gpu", "h100"]);
    let cap_bytes = cap_ann.encode().expect("encode");

    let outcome = registry
        .dispatch(&cap_bytes, kp.entity_id())
        .expect("dispatch succeeds");
    assert_eq!(outcome, ApplyOutcome::Inserted);

    // The capability fold saw the apply; the routing fold did NOT.
    assert_eq!(cap_fold.metrics().applies_inserted(), 1);
    assert_eq!(route_fold.metrics().applies_inserted(), 0);

    // Query against the cap fold confirms the dispatch reached
    // the right typed apply path.
    let hits = cap_fold.query(CapQuery {
        class: 0x1000,
        required_tag: Some("h100".into()),
    });
    assert_eq!(hits, vec![(0x1000, 0x42)]);
}

#[test]
fn registry_rejects_envelope_for_unknown_kind() {
    let registry = FoldRegistry::new();
    let kp = EntityKeypair::generate();
    let ann = sign_cap_ann(&kp, 0x42, 0x1000, 1, vec!["gpu"]);
    let bytes = ann.encode().expect("encode");

    // No fold registered → UnknownKind.
    match registry.dispatch(&bytes, kp.entity_id()) {
        Err(DispatchError::UnknownKind(k)) => assert_eq!(k, CapFold::KIND_ID),
        other => panic!("expected UnknownKind, got {other:?}"),
    }
}

#[test]
fn registry_rejects_truncated_envelope() {
    let registry = FoldRegistry::new();
    let kp = EntityKeypair::generate();

    // Empty buffer: no kind varint at all.
    match registry.dispatch(b"", kp.entity_id()) {
        Err(DispatchError::Truncated) => {}
        other => panic!("empty: expected Truncated, got {other:?}"),
    }

    // `0x80` is a varint continuation byte (high bit set)
    // promising at least one more byte that isn't there. postcard
    // refuses to take a u16 from this — Truncated.
    match registry.dispatch(b"\x80", kp.entity_id()) {
        Err(DispatchError::Truncated) => {}
        other => panic!("mid-varint: expected Truncated, got {other:?}"),
    }
}

#[test]
fn registry_rejects_envelope_whose_kind_disagrees_with_routed_fold() {
    // Construct an envelope whose wire `kind` bytes route to
    // the cap fold, but tamper the in-payload `kind` to claim a
    // different fold. After verify-and-decode the dispatch
    // adapter must catch the mismatch.
    //
    // We do this by signing two different kinds against the
    // same payload and shipping the WRONG bytes:
    //   - Envelope A: `kind = CapFold::KIND_ID`, payload tags
    //   - Decode body claims `kind = 0xFFFF` (mismatched)
    //
    // To keep verify happy we need to actually sign the
    // mismatched form. We do that by hand-constructing the
    // envelope with mismatched kind and signing over THOSE
    // bytes — then route via the cap-fold dispatcher by calling
    // its dispatch directly.
    let registry = FoldRegistry::new();
    let cap_fold: Arc<Fold<CapFold>> = Arc::new(Fold::new());
    registry.register(cap_fold.clone());

    let kp = EntityKeypair::generate();
    // Sign an envelope whose `kind` field is NOT CapFold::KIND_ID.
    let foreign = SignedAnnouncement::sign(
        &kp,
        0xFFFF, // wrong kind
        0x1000,
        0x42,
        1,
        EnvelopeMeta::default(),
        CapPayload {
            class_hash: 0x1000,
            tags: vec!["gpu".into()],
        },
    )
    .expect("sign");
    let bytes = foreign.encode().expect("encode");

    // The registry's lookup keys on the wire `kind` u16 — since
    // the envelope claims 0xFFFF, the registry doesn't find a
    // fold and returns UnknownKind. The KindMismatch path fires
    // when the adapter is invoked directly with bytes whose
    // wire kind matches the dispatcher but whose envelope kind
    // doesn't — which is only constructable via a manual
    // adapter call:
    let adapter = registry
        .get(CapFold::KIND_ID)
        .expect("cap fold registered");
    match adapter.dispatch(&bytes, kp.entity_id()) {
        Err(WireError::KindMismatch { got, expected }) => {
            assert_eq!(got, 0xFFFF);
            assert_eq!(expected, CapFold::KIND_ID);
        }
        other => panic!("expected KindMismatch, got {other:?}"),
    }
    // The cap fold's apply was NOT called (the mismatch caught
    // the envelope before handoff).
    assert_eq!(cap_fold.metrics().applies_inserted(), 0);
}

#[test]
fn registry_can_deregister_a_fold() {
    let registry = FoldRegistry::new();
    let cap_fold: Arc<Fold<CapFold>> = Arc::new(Fold::new());
    registry.register(cap_fold);
    assert_eq!(registry.len(), 1);

    let removed = registry.deregister(CapFold::KIND_ID);
    assert!(removed.is_some());
    assert!(registry.is_empty());
}

// ---------------------------------------------------------------------
// Channel-router trait wiring (in-process publisher → router →
// registry → fold roundtrip). The mesh-level dispatch arm is
// exercised by integration tests; here we
// verify the router contract that arm is built against.
// ---------------------------------------------------------------------

use super::dispatch::FoldChannelRouter;
use crate::adapter::net::identity::EntityId;

#[test]
fn fold_registry_implements_channel_router_trait() {
    // The mesh dispatch arm stores routers as
    // `Arc<dyn FoldChannelRouter>`. Confirm `FoldRegistry`
    // round-trips through the trait object — a regression that
    // breaks the blanket impl would surface as a compile error
    // here.
    let registry = FoldRegistry::new();
    let cap_fold: Arc<Fold<CapFold>> = Arc::new(Fold::new());
    registry.register(cap_fold.clone());
    let registry: Arc<dyn FoldChannelRouter> = Arc::new(registry);

    let kp = EntityKeypair::generate();
    let ann = sign_cap_ann(&kp, 0x42, 0x1000, 1, vec!["gpu"]);
    let bytes = ann.encode().expect("encode");

    let outcome = registry
        .try_route(kp.entity_id(), &bytes)
        .expect("router accepts signed envelope");
    assert_eq!(outcome, ApplyOutcome::Inserted);
    assert_eq!(cap_fold.metrics().applies_inserted(), 1);
}

#[test]
fn channel_router_surface_propagates_signature_failure() {
    // The router contract must NOT mask signature failures;
    // the mesh dispatch arm relies on the `DispatchError::Wire`
    // surface to log + drop tampered frames without crediting
    // them to a fold.
    let registry = FoldRegistry::new();
    let cap_fold: Arc<Fold<CapFold>> = Arc::new(Fold::new());
    registry.register(cap_fold.clone());
    let router: Arc<dyn FoldChannelRouter> = Arc::new(registry);

    let signer = EntityKeypair::generate();
    let imposter = EntityKeypair::generate();
    let ann = sign_cap_ann(&signer, 0x42, 0x1000, 1, vec!["gpu"]);
    let bytes = ann.encode().expect("encode");

    // Route claiming the imposter's identity → InvalidSignature.
    match router.try_route(imposter.entity_id(), &bytes) {
        Err(DispatchError::Wire(WireError::InvalidSignature)) => {}
        other => panic!("expected InvalidSignature, got {other:?}"),
    }
    // No apply credited to the fold.
    assert_eq!(cap_fold.metrics().applies_inserted(), 0);
}

#[test]
fn channel_router_drops_envelope_for_unknown_kind() {
    // The mesh dispatch arm relies on UnknownKind being a
    // recoverable error (not a panic) so a stray fold publish
    // for a kind this node doesn't host doesn't take down the
    // dispatch loop.
    let registry = FoldRegistry::new();
    let router: Arc<dyn FoldChannelRouter> = Arc::new(registry);

    let kp = EntityKeypair::generate();
    let ann = sign_cap_ann(&kp, 0x42, 0x1000, 1, vec!["gpu"]);
    let bytes = ann.encode().expect("encode");

    match router.try_route(kp.entity_id(), &bytes) {
        Err(DispatchError::UnknownKind(k)) => assert_eq!(k, CapFold::KIND_ID),
        other => panic!("expected UnknownKind, got {other:?}"),
    }
}

#[test]
fn subprotocol_fold_id_is_stable() {
    // Lock the wire-subprotocol byte. Operators trace fold
    // traffic by this value; bumping it is a wire-compat break
    // that needs an explicit migration. Catch any drift here.
    assert_eq!(super::dispatch::SUBPROTOCOL_FOLD, 0x1000);
}

#[test]
fn entity_id_is_what_the_router_trait_takes() {
    // The dispatch arm in `mesh.rs` looks up `EntityId` from
    // `peer_entity_ids`. Confirm the router accepts that exact
    // type so the dispatch arm can pass `&entity_id` without
    // adapter code.
    fn _accepts_entity_id<R: FoldChannelRouter>(
        r: &R,
        e: &EntityId,
        b: &[u8],
    ) -> Result<ApplyOutcome, DispatchError> {
        r.try_route(e, b)
    }
    let registry = FoldRegistry::new();
    let _registered: Arc<dyn FoldChannelRouter> = Arc::new(registry);
}

// ---------------------------------------------------------------------
// Publisher-side encode + simulated receive end-to-end.
// We can't boot real mesh sockets here, but we CAN exercise the full
// "publisher signs → encode → receive bytes → registry dispatch → fold
// apply" pipeline in-process. The mesh-level
// `publish_fold_to_peer` / `publish_fold_broadcast` helpers wrap
// `ann.encode()` + `send_subprotocol(_, SUBPROTOCOL_FOLD, &bytes)` —
// the encoding step is what this test pins, since the `send_subprotocol`
// hop is already covered by the substrate's existing transport tests.
// ---------------------------------------------------------------------

#[test]
fn publisher_to_receiver_full_pipeline_in_process() {
    // 1. Publisher side: sign an announcement with the publisher's
    //    keypair and produce the on-wire bytes.
    let publisher_kp = EntityKeypair::generate();
    let ann = sign_cap_ann(&publisher_kp, 0x42, 0x1000, 1, vec!["gpu", "h100"]);
    let wire_bytes = ann.encode().expect("publisher: encode succeeds");

    // 2. Receiver side: install a FoldRegistry as the channel
    //    router. This is the same shape that
    //    `mesh.set_fold_router(Some(Arc::new(registry)))` produces.
    let registry = FoldRegistry::new();
    let cap_fold: Arc<Fold<CapFold>> = Arc::new(Fold::new());
    registry.register(cap_fold.clone());
    let router: Arc<dyn FoldChannelRouter> = Arc::new(registry);

    // 3. Receiver dispatch arm: hands the bytes + resolved
    //    publisher EntityId to `router.try_route`. The mesh
    //    `dispatch_packet` arm at `SUBPROTOCOL_FOLD` does
    //    exactly this.
    let outcome = router
        .try_route(publisher_kp.entity_id(), &wire_bytes)
        .expect("receiver: dispatch succeeds for valid envelope");
    assert_eq!(outcome, ApplyOutcome::Inserted);

    // 4. Query against the fold confirms the apply landed and
    //    the secondary index was populated by the typed
    //    `Fold<CapFold>::apply` path.
    let hits = cap_fold.query(CapQuery {
        class: 0x1000,
        required_tag: Some("h100".into()),
    });
    assert_eq!(hits, vec![(0x1000, 0x42)]);
    assert_eq!(cap_fold.metrics().applies_inserted(), 1);
}

#[test]
fn publisher_encode_is_stable_across_calls() {
    // The wire envelope is the load-bearing identity for
    // signatures, replay-window bookkeeping, and operator
    // diagnostics. A regression that introduces nondeterminism
    // (e.g. a HashMap iterating in random order inside the
    // payload encoder) would break every cross-node verify.
    //
    // We encode the same announcement twice and assert byte
    // equality. `SignedAnnouncement::sign` is also deterministic
    // for a given (keypair, payload) under Ed25519, so the
    // wire bytes are deterministic end-to-end.
    let kp = EntityKeypair::generate();
    let ann1 = sign_cap_ann(&kp, 0x42, 0x1000, 1, vec!["gpu", "h100"]);
    let ann2 = ann1.clone();
    assert_eq!(
        ann1.encode().expect("first encode"),
        ann2.encode().expect("second encode"),
        "wire encoding must be deterministic across repeated encode() calls"
    );
}

#[test]
fn receiver_rejects_envelope_signed_for_a_different_publisher() {
    // The publisher → receiver pipeline must NOT credit an
    // apply to fold state when the inbound `EntityId` (resolved
    // by the mesh dispatch arm from `peer_entity_ids`) doesn't
    // match the key that signed the envelope. Without this
    // gate, a peer that hijacks a session_id could publish
    // announcements claiming any other peer's identity.
    let real_publisher = EntityKeypair::generate();
    let session_owner = EntityKeypair::generate(); // resolved by mesh

    let ann = sign_cap_ann(&real_publisher, 0x42, 0x1000, 1, vec!["gpu"]);
    let wire_bytes = ann.encode().expect("encode");

    let registry = FoldRegistry::new();
    let cap_fold: Arc<Fold<CapFold>> = Arc::new(Fold::new());
    registry.register(cap_fold.clone());
    let router: Arc<dyn FoldChannelRouter> = Arc::new(registry);

    // Receiver dispatches with the session owner's identity (the
    // mesh arm resolves this from `peer_entity_ids`). The
    // signature was made with a different key, so verify rejects.
    match router.try_route(session_owner.entity_id(), &wire_bytes) {
        Err(DispatchError::Wire(WireError::InvalidSignature)) => {}
        other => panic!("expected InvalidSignature, got {other:?}"),
    }
    assert_eq!(
        cap_fold.metrics().applies_inserted(),
        0,
        "no apply may be credited to the fold when verify fails"
    );
}

// ---------------------------------------------------------------------
// TTL expiry sweep + audit sink routing
// ---------------------------------------------------------------------

use super::audit::{NoopSink, VecFoldAuditSink};

/// Build a cap-fold announcement with `ttl_secs = 0` so the
/// computed `expires_at` is `now + 0s == now` — the next
/// `sweep_expired_now` call evicts it. The wire envelope's TTL
/// is at 1-second resolution; tests that need finer control
/// either drive `sweep_expired_now` synchronously or wait a beat
/// under the background sweeper.
fn sign_cap_ann_with_ttl(
    keypair: &EntityKeypair,
    node_id: NodeId,
    class: u64,
    generation: u64,
    ttl_secs: u32,
    tags: Vec<&str>,
) -> SignedAnnouncement<CapPayload> {
    SignedAnnouncement::sign(
        keypair,
        CapFold::KIND_ID,
        class,
        node_id,
        generation,
        EnvelopeMeta {
            ttl_secs: Some(ttl_secs),
            ..Default::default()
        },
        CapPayload {
            class_hash: class,
            tags: tags.into_iter().map(String::from).collect(),
        },
    )
    .expect("sign succeeds")
}

#[test]
fn sweep_expired_removes_entries_past_ttl() {
    // Disable the background task so we drive expiry
    // deterministically via `sweep_expired_now`.
    let fold: Fold<CapFold> = Fold::with_sweep_interval(std::time::Duration::ZERO);
    let kp = EntityKeypair::generate();

    // Insert three entries: two with ttl=0 (already expired by
    // the time `sweep_expired_now` runs) and one with ttl=300s
    // (still valid). The cap-fold default ttl is 60s, which the
    // ttl=0 override beats.
    fold.apply(sign_cap_ann_with_ttl(&kp, 0xA, 0x100, 1, 0, vec!["a"]))
        .expect("a accepted");
    fold.apply(sign_cap_ann_with_ttl(&kp, 0xB, 0x100, 1, 0, vec!["b"]))
        .expect("b accepted");
    fold.apply(sign_cap_ann_with_ttl(&kp, 0xC, 0x100, 1, 300, vec!["c"]))
        .expect("c accepted");
    assert_eq!(fold.metrics().entries(), 3);
    assert_eq!(fold.metrics().expiries(), 0);

    // Allow a beat so `Instant::now()` advances past the
    // `expires_at` stamped on the ttl=0 entries; the cmp inside
    // sweep is `<=`, so even on identical Instant the sweep
    // would catch them, but the explicit sleep makes the test
    // robust against monotonic clock quirks.
    std::thread::sleep(std::time::Duration::from_millis(10));

    let evicted = fold.sweep_expired_now();
    assert_eq!(evicted, 2, "two expired entries evicted, one remains");
    assert_eq!(fold.metrics().entries(), 1);
    assert_eq!(fold.metrics().expiries(), 2);

    // Surviving entry is `0xC` (ttl=300s).
    fold.with_state(|state| {
        assert!(state.entries.contains_key(&(0x100, 0xC)));
        assert!(!state.entries.contains_key(&(0x100, 0xA)));
        assert!(!state.entries.contains_key(&(0x100, 0xB)));
        // by_node reverse index cleaned up for the evicted nodes.
        assert!(!state.by_node.contains_key(&0xA));
        assert!(!state.by_node.contains_key(&0xB));
        assert!(state.by_node.contains_key(&0xC));
    });

    // Index hooks ran — querying for evicted entries' tags
    // returns nothing.
    assert!(fold
        .query(CapQuery {
            class: 0x100,
            required_tag: Some("a".into()),
        })
        .is_empty());
    assert!(fold
        .query(CapQuery {
            class: 0x100,
            required_tag: Some("b".into()),
        })
        .is_empty());
    // Surviving tag still resolves.
    assert_eq!(
        fold.query(CapQuery {
            class: 0x100,
            required_tag: Some("c".into()),
        }),
        vec![(0x100, 0xC)]
    );
}

#[test]
fn sweep_with_no_expired_entries_is_a_no_op() {
    let fold: Fold<CapFold> = Fold::with_sweep_interval(std::time::Duration::ZERO);
    let kp = EntityKeypair::generate();
    fold.apply(sign_cap_ann_with_ttl(&kp, 0xA, 0x100, 1, 300, vec!["a"]))
        .expect("a accepted");

    let evicted = fold.sweep_expired_now();
    assert_eq!(evicted, 0);
    assert_eq!(fold.metrics().expiries(), 0);
    assert_eq!(fold.metrics().entries(), 1);
}

#[test]
fn sweep_evicts_across_multiple_chunks_when_count_exceeds_chunk_size() {
    // Pin the chunked-sweep behavior: insert >SWEEP_CHUNK_SIZE
    // expired entries and confirm a single sweep call evicts all
    // of them. Earlier full-state-lock implementation would also
    // pass this; the chunked variant has to loop until the read
    // pass returns empty, which this test exercises directly.
    let fold: Fold<CapFold> = Fold::with_sweep_interval(std::time::Duration::ZERO);
    let kp = EntityKeypair::generate();
    // 1500 > SWEEP_CHUNK_SIZE (1024) — guarantees at least two
    // chunks plus a leftover batch.
    const N: u64 = 1500;
    for i in 0..N {
        fold.apply(sign_cap_ann_with_ttl(&kp, i, 0x100, 1, 0, vec!["t"]))
            .expect("apply");
    }
    assert_eq!(fold.metrics().entries(), N);

    std::thread::sleep(std::time::Duration::from_millis(10));
    let evicted = fold.sweep_expired_now();
    assert_eq!(evicted, N as usize, "all expired entries evicted across chunks");
    assert_eq!(fold.metrics().entries(), 0);
    assert_eq!(fold.metrics().expiries(), N);
}

/// Audit-emitting `FoldKind` shim: identical to `CapFold` but
/// `audit_event` returns `Some(AuditEvent)` for every transition.
/// Audit emission is opt-in via the trait so folds that don't
/// audit pay nothing on the hot path.
struct AuditingCapFold;

impl FoldKind for AuditingCapFold {
    const KIND_ID: u16 = 0x0F02;
    const CHANNEL_PREFIX: &'static str = "test:audit-cap:";
    const DEFAULT_TTL: std::time::Duration = std::time::Duration::from_secs(60);
    type Key = (u64, NodeId);
    type Payload = CapPayload;
    type Query = CapQuery;
    type Result = Vec<(u64, NodeId)>;
    type Index = NoIndex;

    fn key_for(node_id: NodeId, payload: &CapPayload) -> Self::Key {
        (payload.class_hash, node_id)
    }

    fn build_index() -> NoIndex {
        NoIndex
    }

    fn query(state: &FoldState<Self>, _index: &NoIndex, q: CapQuery) -> Vec<(u64, NodeId)> {
        state
            .entries
            .iter()
            .filter(|((class, _), _)| *class == q.class)
            .map(|(k, _)| *k)
            .collect()
    }

    fn audit_event(transition: super::EntryTransition<'_, Self>) -> Option<super::AuditEvent> {
        use super::AuditKind;
        let (kind, key_repr, detail) = match transition {
            super::EntryTransition::Created { key, .. } => {
                (AuditKind::Created, format!("{key:?}"), None)
            }
            super::EntryTransition::Replaced { key, old, new } => (
                AuditKind::Replaced,
                format!("{key:?}"),
                Some(format!("gen {} → {}", old.generation, new.generation)),
            ),
            super::EntryTransition::Rejected { key, .. } => {
                (AuditKind::Rejected, format!("{key:?}"), None)
            }
            super::EntryTransition::Evicted { key, reason, .. } => (
                AuditKind::Evicted,
                format!("{key:?}"),
                Some(reason.to_string()),
            ),
            super::EntryTransition::Expired { key, .. } => {
                (AuditKind::Expired, format!("{key:?}"), None)
            }
        };
        Some(super::AuditEvent {
            kind,
            key_repr,
            detail,
        })
    }
}

fn sign_audit_ann(
    kp: &EntityKeypair,
    node_id: NodeId,
    class: u64,
    generation: u64,
    ttl_secs: u32,
    tags: Vec<&str>,
) -> SignedAnnouncement<CapPayload> {
    SignedAnnouncement::sign(
        kp,
        AuditingCapFold::KIND_ID,
        class,
        node_id,
        generation,
        EnvelopeMeta {
            ttl_secs: Some(ttl_secs),
            ..Default::default()
        },
        CapPayload {
            class_hash: class,
            tags: tags.into_iter().map(String::from).collect(),
        },
    )
    .expect("sign")
}

#[test]
fn audit_sink_receives_create_replace_evict_and_expire_transitions() {
    let fold: Fold<AuditingCapFold> = Fold::with_sweep_interval(std::time::Duration::ZERO);
    let sink: Arc<VecFoldAuditSink> = Arc::new(VecFoldAuditSink::new());
    fold.set_audit_sink(Some(sink.clone() as Arc<dyn super::FoldAuditSink>));
    assert!(fold.has_audit_sink());

    let kp = EntityKeypair::generate();

    // Create
    fold.apply(sign_audit_ann(&kp, 0xA, 0x100, 1, 300, vec!["a"]))
        .expect("create");
    assert_eq!(sink.snapshot().len(), 1);
    assert_eq!(sink.snapshot()[0].kind, super::AuditKind::Created);

    // Replace
    fold.apply(sign_audit_ann(&kp, 0xA, 0x100, 2, 300, vec!["a2"]))
        .expect("replace");
    assert_eq!(sink.snapshot().len(), 2);
    assert_eq!(sink.snapshot()[1].kind, super::AuditKind::Replaced);
    assert_eq!(
        sink.snapshot()[1].detail.as_deref(),
        Some("gen 1 → 2")
    );

    // Reject (stale generation)
    fold.apply(sign_audit_ann(&kp, 0xA, 0x100, 2, 300, vec!["bogus"]))
        .expect("reject");
    assert_eq!(sink.snapshot().len(), 3);
    assert_eq!(sink.snapshot()[2].kind, super::AuditKind::Rejected);

    // Evict
    fold.evict_node(0xA, "SWIM declared dead");
    assert_eq!(sink.snapshot().len(), 4);
    assert_eq!(sink.snapshot()[3].kind, super::AuditKind::Evicted);
    assert_eq!(sink.snapshot()[3].detail.as_deref(), Some("SWIM declared dead"));

    // Expire — insert a fresh entry with ttl=0 then sweep.
    fold.apply(sign_audit_ann(&kp, 0xB, 0x100, 1, 0, vec!["b"]))
        .expect("create-for-expire");
    std::thread::sleep(std::time::Duration::from_millis(10));
    let n = fold.sweep_expired_now();
    assert_eq!(n, 1);
    let trail = sink.snapshot();
    assert_eq!(
        trail.last().expect("trail non-empty").kind,
        super::AuditKind::Expired
    );
}

#[test]
fn audit_sink_can_be_uninstalled() {
    let fold: Fold<AuditingCapFold> = Fold::with_sweep_interval(std::time::Duration::ZERO);
    let sink: Arc<VecFoldAuditSink> = Arc::new(VecFoldAuditSink::new());
    fold.set_audit_sink(Some(sink.clone() as Arc<dyn super::FoldAuditSink>));

    let kp = EntityKeypair::generate();
    fold.apply(sign_audit_ann(&kp, 0xA, 0x100, 1, 300, vec!["a"]))
        .expect("create");
    assert_eq!(sink.len(), 1);

    // Uninstall — subsequent events shouldn't reach the sink.
    fold.set_audit_sink(None);
    assert!(!fold.has_audit_sink());
    fold.apply(sign_audit_ann(&kp, 0xB, 0x100, 1, 300, vec!["b"]))
        .expect("create-2");
    assert_eq!(sink.len(), 1, "post-uninstall events must not reach the sink");
}

#[test]
fn noop_sink_swallows_events_without_storing() {
    let fold: Fold<AuditingCapFold> = Fold::with_sweep_interval(std::time::Duration::ZERO);
    fold.set_audit_sink(Some(Arc::new(NoopSink) as Arc<dyn super::FoldAuditSink>));
    let kp = EntityKeypair::generate();
    // Many applies, no panic, no allocation observed by the
    // (non-instrumented) sink. Effectively a smoke test that
    // NoopSink composes through the trait.
    for i in 0..16 {
        fold.apply(sign_audit_ann(&kp, i as u64, 0x100, 1, 300, vec!["t"]))
            .expect("apply");
    }
    assert_eq!(fold.metrics().applies_inserted(), 16);
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn background_sweeper_evicts_expired_entries_on_tick() {
    // Construct a fold with a tight sweep interval. `start_paused`
    // + `tokio::time::advance` lets us cross the tick boundary
    // without actually sleeping — the sweeper task wakes when
    // we advance time past its `tokio::time::interval::tick`.
    let fold: Fold<CapFold> = Fold::with_sweep_interval(std::time::Duration::from_millis(50));
    let kp = EntityKeypair::generate();

    // ttl=0 → expires_at == apply time. The next sweep evicts.
    fold.apply(sign_cap_ann_with_ttl(&kp, 0xA, 0x100, 1, 0, vec!["a"]))
        .expect("apply");
    assert_eq!(fold.metrics().entries(), 1);
    assert_eq!(fold.metrics().expiries(), 0);

    // Skip the immediate first tick (the sweeper drops it
    // deliberately) and the next scheduled tick. After the second
    // tick fires the expired entry must be gone.
    tokio::time::advance(std::time::Duration::from_millis(60)).await;
    tokio::task::yield_now().await;
    tokio::time::advance(std::time::Duration::from_millis(60)).await;
    tokio::task::yield_now().await;

    assert_eq!(fold.metrics().expiries(), 1);
    assert_eq!(fold.metrics().entries(), 0);
}

// ---------------------------------------------------------------------
// FoldStats + FoldRegistry::stats + RingFoldAuditSink
// ---------------------------------------------------------------------

use super::audit::RingFoldAuditSink;
use super::metrics::FoldStats;

#[test]
fn fold_stats_snapshot_reflects_live_counters() {
    let fold: Fold<CapFold> = Fold::with_sweep_interval(std::time::Duration::ZERO);

    // 2 inserts, 1 replace, 1 reject.
    fold.apply(cap_announcement(0x1, 0x100, 1, vec!["a"]))
        .expect("ins-1");
    fold.apply(cap_announcement(0x2, 0x100, 1, vec!["b"]))
        .expect("ins-2");
    fold.apply(cap_announcement(0x1, 0x100, 2, vec!["a2"]))
        .expect("replace");
    let _ = fold
        .apply(cap_announcement(0x1, 0x100, 1, vec!["stale"]))
        .expect("reject"); // stale gen rejected
    // Tag-filtered query bumps the query counter.
    let _ = fold.query(CapQuery {
        class: 0x100,
        required_tag: Some("a2".into()),
    });

    let snap = fold.stats();
    assert_eq!(snap.kind, CapFold::KIND_ID);
    assert_eq!(snap.channel_prefix, CapFold::CHANNEL_PREFIX);
    assert_eq!(snap.entries, 2);
    assert_eq!(snap.applies_inserted, 2);
    assert_eq!(snap.applies_replaced, 1);
    assert_eq!(snap.applies_rejected, 1);
    assert_eq!(snap.applies_total, 4);
    assert_eq!(snap.expiries, 0);
    assert_eq!(snap.evictions, 0);
    assert_eq!(snap.queries, 1);
    assert_eq!(snap.snapshots_taken, 0);
    assert_eq!(snap.snapshots_restored, 0);
    assert!(!snap.has_audit_sink);

    // Drive every counter and re-snapshot.
    fold.evict_node(0x2, "test");
    let _snap = fold.snapshot();
    let restored: Fold<CapFold> = Fold::with_sweep_interval(std::time::Duration::ZERO);
    let s2 = fold.snapshot();
    restored.restore(s2, false).expect("restore");

    fold.set_audit_sink(Some(Arc::new(NoopSink) as Arc<dyn super::FoldAuditSink>));
    let snap = fold.stats();
    assert_eq!(snap.entries, 1, "after evict_node(0x2): only 0x1 remains");
    assert_eq!(snap.evictions, 1);
    assert_eq!(snap.snapshots_taken, 2);
    assert!(snap.has_audit_sink);
    assert_eq!(restored.stats().snapshots_restored, 1);
}

#[test]
fn fold_registry_stats_aggregates_across_kinds() {
    let registry = FoldRegistry::new();
    let cap: Arc<Fold<CapFold>> = Arc::new(Fold::with_sweep_interval(std::time::Duration::ZERO));
    let route: Arc<Fold<RoutingTestFold>> =
        Arc::new(Fold::with_sweep_interval(std::time::Duration::ZERO));
    registry.register(cap.clone());
    registry.register(route.clone());

    cap.apply(cap_announcement(0x42, 0x100, 1, vec!["gpu"]))
        .unwrap();
    cap.apply(cap_announcement(0x43, 0x100, 1, vec!["gpu"]))
        .unwrap();
    route
        .apply(route_announcement(0xAA, 0x99, 50, 1))
        .unwrap();

    let stats = registry.stats();
    assert_eq!(stats.len(), 2);

    // Find each by kind — order is unspecified.
    let cap_stats = stats
        .iter()
        .find(|s| s.kind == CapFold::KIND_ID)
        .expect("cap stats present");
    assert_eq!(cap_stats.entries, 2);
    assert_eq!(cap_stats.channel_prefix, CapFold::CHANNEL_PREFIX);

    let route_stats = stats
        .iter()
        .find(|s| s.kind == RoutingTestFold::KIND_ID)
        .expect("route stats present");
    assert_eq!(route_stats.entries, 1);
    assert_eq!(route_stats.channel_prefix, RoutingTestFold::CHANNEL_PREFIX);
}

#[test]
fn fold_stats_round_trips_through_serde_json() {
    // The CLI surface (`net fold list --output json`) serializes
    // this shape via `serde_json::to_string`. Pin the round-trip
    // so a regression in field naming / order doesn't silently
    // break operator tooling.
    let stats = FoldStats {
        kind: CapFold::KIND_ID,
        channel_prefix: "test:cap:".to_string(),
        entries: 12,
        applies_inserted: 10,
        applies_replaced: 3,
        applies_rejected: 1,
        applies_total: 14,
        expiries: 2,
        evictions: 0,
        queries: 7,
        snapshots_taken: 1,
        snapshots_restored: 0,
        has_audit_sink: true,
    };
    let json = serde_json::to_string(&stats).expect("serialize");
    let parsed: FoldStats = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(parsed, stats);
}

#[test]
fn ring_audit_sink_drops_oldest_when_capacity_exceeded() {
    let sink = RingFoldAuditSink::new(3);
    for i in 0..5 {
        sink.record(super::AuditEvent {
            kind: super::AuditKind::Created,
            key_repr: format!("{}", i),
            detail: None,
        });
    }
    let snap = sink.snapshot();
    assert_eq!(snap.len(), 3);
    // Oldest two (`0`, `1`) were dropped; survivors are `2..=4`
    // in insertion order.
    let keys: Vec<&str> = snap.iter().map(|e| e.key_repr.as_str()).collect();
    assert_eq!(keys, vec!["2", "3", "4"]);
}

#[test]
fn audit_kind_custom_variant_round_trips_through_sink() {
    // Folds emit fold-specific transitions via AuditKind::Custom
    // without widening the enum. Pin that the variant compares
    // by string contents, not by reference identity.
    let sink = RingFoldAuditSink::new(2);
    sink.record(super::AuditEvent {
        kind: super::AuditKind::Custom("reservation_takeover"),
        key_repr: "0xCAFE".into(),
        detail: Some("expired holder 0xDEAD".into()),
    });
    let snap = sink.snapshot();
    assert_eq!(snap.len(), 1);
    assert_eq!(
        snap[0].kind,
        super::AuditKind::Custom("reservation_takeover")
    );
    // Custom variants with different tags compare unequal.
    assert_ne!(snap[0].kind, super::AuditKind::Custom("eviction"));
}

#[test]
fn ring_audit_sink_with_zero_capacity_stores_nothing() {
    let sink = RingFoldAuditSink::new(0);
    sink.record(super::AuditEvent {
        kind: super::AuditKind::Created,
        key_repr: "x".into(),
        detail: None,
    });
    assert!(sink.is_empty());
    assert_eq!(sink.len(), 0);
    assert!(sink.snapshot().is_empty());
}

#[test]
fn fold_channel_router_trait_object_exposes_stats() {
    // The mesh dispatch arm stores routers as
    // `Arc<dyn FoldChannelRouter>`. The CLI / Deck path reads
    // stats via the trait object — no concrete-type visibility
    // — so the `stats` method on the trait must route through
    // `FoldRegistry::stats` correctly.
    let registry = FoldRegistry::new();
    let cap_fold: Arc<Fold<CapFold>> = Arc::new(Fold::with_sweep_interval(std::time::Duration::ZERO));
    let route_fold: Arc<Fold<RoutingTestFold>> =
        Arc::new(Fold::with_sweep_interval(std::time::Duration::ZERO));
    registry.register(cap_fold.clone());
    registry.register(route_fold.clone());

    let kp = EntityKeypair::generate();
    cap_fold
        .apply(cap_announcement(0x42, 0x100, 1, vec!["gpu"]))
        .unwrap();
    route_fold
        .apply(route_announcement(0xAA, 0x99, 50, 1))
        .unwrap();
    let _ = kp;

    let router: Arc<dyn FoldChannelRouter> = Arc::new(registry);
    let stats = router.stats();
    assert_eq!(stats.len(), 2, "registry router reports both folds");

    let cap_stats = stats
        .iter()
        .find(|s| s.kind == CapFold::KIND_ID)
        .expect("cap stats present");
    assert_eq!(cap_stats.entries, 1);

    let route_stats = stats
        .iter()
        .find(|s| s.kind == RoutingTestFold::KIND_ID)
        .expect("route stats present");
    assert_eq!(route_stats.entries, 1);
}

#[test]
fn fold_channel_router_stub_returns_its_own_empty_stats() {
    // Routers that don't track per-fold stats must return an
    // empty Vec explicitly — the trait has no default impl, so
    // "no stats" is a deliberate choice the implementer makes
    // rather than something callers can silently inherit.
    struct StubRouter;
    impl FoldChannelRouter for StubRouter {
        fn try_route(
            &self,
            _publisher: &EntityId,
            _bytes: &[u8],
        ) -> Result<ApplyOutcome, DispatchError> {
            Ok(ApplyOutcome::Inserted)
        }
        fn stats(&self) -> Vec<FoldStats> {
            Vec::new()
        }
    }
    let stub: Arc<dyn FoldChannelRouter> = Arc::new(StubRouter);
    assert!(stub.stats().is_empty());
}

#[test]
fn ring_audit_sink_plugs_into_fold_and_captures_transitions() {
    let fold: Fold<AuditingCapFold> = Fold::with_sweep_interval(std::time::Duration::ZERO);
    let sink = Arc::new(RingFoldAuditSink::new(4));
    fold.set_audit_sink(Some(sink.clone() as Arc<dyn super::FoldAuditSink>));

    let kp = EntityKeypair::generate();
    // 5 distinct events — the 1st is dropped (capacity 4).
    for i in 0..5 {
        fold.apply(sign_audit_ann(
            &kp,
            0x10 + i,
            0x100,
            1,
            300,
            vec!["t"],
        ))
        .expect("apply");
    }
    let snap = sink.snapshot();
    assert_eq!(snap.len(), 4);
    // All 4 retained events are "created"; the oldest "created
    // for 0x10" was dropped.
    for e in &snap {
        assert_eq!(e.kind, super::AuditKind::Created);
    }
}
