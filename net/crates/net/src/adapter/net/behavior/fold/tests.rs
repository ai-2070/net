//! Phase 1 unit + property tests.
//!
//! Each test exercises one runtime contract of the generic fold:
//! - apply / query round-trip for a trivial `FoldKind`
//! - generation ordering: stale apply is rejected
//! - `merge` override path: routing-style lower-metric-wins
//! - `evict_node` removes every entry attached to a node and
//!   updates the secondary index
//! - snapshot → restore round-trips the full state
//! - restore over a live fold without `force` is refused
//! - metric counters track outcomes
//!
//! Concrete folds (capability / routing / reservation) land in
//! later phases — these tests use synthetic `TestFold` /
//! `RoutingTestFold` shapes scoped to the test module.

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
        0,
        None,
        0,
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
        0,
        None,
        0,
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
// Phase 2: wire codec + sign/verify + dispatch routing
// ---------------------------------------------------------------------

use std::sync::Arc;

use crate::adapter::net::identity::EntityKeypair;

use super::announcement::placeholder_signature;
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
        0,
        None,
        0,
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
        0,
        None,
        0,
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
// Phase 2B: channel-router trait wiring (in-process publisher → router
// → registry → fold roundtrip). The mesh-level dispatch arm is exercised
// by integration tests once the channel layer adopters land; here we
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
