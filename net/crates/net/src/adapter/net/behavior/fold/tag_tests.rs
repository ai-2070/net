//! Slice 6 (tag interning) witnesses at the fold level:
//! CAPABILITY_FOLD_SCALE_PLAN.md "Owner rulings and corrected B1
//! contract". The dictionary's own unit tests live in
//! `tag_dictionary.rs`, the type's in `tag_str.rs`.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;

use super::audit::{FoldAuditSink, RingFoldAuditSink};
use super::capability::{CapabilityIndexInner, CapabilityMembership, HardwareSummary};
use super::capability_bridge::{translate_announcement, CapabilitySetCache};
use super::dispatch::FoldRegistry;
use super::snapshot::{FoldSnapshot, FoldSnapshotEntry};
use super::{
    ApplyOutcome, AuditKind, CapabilityFold, EnvelopeMeta, Fold, FoldError, FoldKind, NodeId,
    NodeState, PayloadRejection, SignedAnnouncement, TagBudget, TagStr, MAX_CAPABILITY_TAGS,
};
use crate::adapter::net::behavior::capability::{CapabilityAnnouncement, CapabilitySet};
use crate::adapter::net::behavior::group::GroupId;
use crate::adapter::net::behavior::subnet::SubnetId;
use crate::adapter::net::identity::EntityKeypair;

fn new_fold() -> Fold<CapabilityFold> {
    Fold::with_sweep_interval(Duration::ZERO)
}

fn fold_with_budget(max_tags: usize) -> Fold<CapabilityFold> {
    Fold::with_sweep_interval_and_index(
        Duration::ZERO,
        CapabilityIndexInner::with_tag_budget(TagBudget {
            max_tags,
            max_bytes: 1 << 20,
        }),
    )
}

fn membership(class: u64, tags: &[&str]) -> CapabilityMembership {
    CapabilityMembership {
        class_hash: class,
        tags: tags.iter().map(|t| TagStr::from(*t)).collect(),
        hardware: None,
        state: NodeState::Idle,
        region: None,
        price_quote: None,
        reflex_addr: None,
        noise_pubkey: None,
        rtc_bootstrap: None,
        rtc_addr: None,
        rtc_stun_addr: None,
        allowed_nodes: Vec::new(),
        allowed_subnets: Vec::new(),
        allowed_groups: Vec::new(),
        metadata: BTreeMap::new(),
        owner: None,
    }
}

fn signed(
    kp: &EntityKeypair,
    node: NodeId,
    generation: u64,
    payload: CapabilityMembership,
) -> SignedAnnouncement<CapabilityMembership> {
    let class = payload.class_hash;
    SignedAnnouncement::sign(
        kp,
        CapabilityFold::KIND_ID,
        class,
        node,
        generation,
        EnvelopeMeta::default(),
        payload,
    )
    .expect("sign")
}

fn interned(fold: &Fold<CapabilityFold>) -> (u64, u64) {
    let stats = fold.stats();
    (stats.interned, stats.interned_bytes)
}

// ---------------------------------------------------------------------
// Golden oracle: the pre-Slice-6 `Vec<String>` payload.
// ---------------------------------------------------------------------

/// `CapabilityMembership` as it was declared before Slice 6: the same
/// serialized fields in the same order, `tags` as `Vec<String>`.
#[derive(Serialize, serde::Deserialize, Clone)]
struct LegacyMembership {
    class_hash: u64,
    tags: Vec<String>,
    hardware: Option<HardwareSummary>,
    state: NodeState,
    region: Option<String>,
    price_quote: Option<u64>,
    reflex_addr: Option<SocketAddr>,
    allowed_nodes: Vec<u64>,
    allowed_subnets: Vec<SubnetId>,
    allowed_groups: Vec<GroupId>,
    metadata: BTreeMap<String, String>,
}

fn legacy(class: u64, tags: &[&str]) -> LegacyMembership {
    LegacyMembership {
        class_hash: class,
        tags: tags.iter().map(|t| (*t).to_owned()).collect(),
        hardware: None,
        state: NodeState::Idle,
        region: Some("eu-west".into()),
        price_quote: Some(7),
        reflex_addr: None,
        allowed_nodes: vec![3, 4],
        allowed_subnets: Vec::new(),
        allowed_groups: Vec::new(),
        metadata: BTreeMap::from([("k".to_owned(), "v".to_owned())]),
    }
}

/// The wire form and the signature transcript do not depend on how the
/// receiver stores tags: the payload and the whole signed envelope encode
/// to the old type's exact bytes, and a signature made over the OLD type
/// verifies when decoded as the new one.
#[test]
fn tag_str_encoding_matches_the_string_oracle() {
    let tags = [
        "hardware.gpu",
        "héllo-ünïcode",
        "",
        "software.model.0.id=llama3",
    ];
    let old = legacy(0x51, &tags);
    let mut new = membership(0x51, &tags);
    new.region = old.region.clone();
    new.price_quote = old.price_quote;
    new.allowed_nodes = old.allowed_nodes.clone();
    new.metadata = old.metadata.clone();

    assert_eq!(
        postcard::to_allocvec(&new).expect("new"),
        postcard::to_allocvec(&old).expect("old"),
        "payload bytes"
    );

    let kp = EntityKeypair::generate();
    // Verification binds the envelope's node id to the signing key.
    let node = kp.entity_id().node_id();
    let old_env = SignedAnnouncement::sign(
        &kp,
        CapabilityFold::KIND_ID,
        0x51,
        node,
        3,
        EnvelopeMeta::default(),
        old.clone(),
    )
    .expect("sign old");
    let new_env = signed(&kp, node, 3, new.clone());
    let old_bytes = old_env.encode().expect("encode old");
    assert_eq!(
        new_env.encode().expect("encode new"),
        old_bytes,
        "envelope bytes"
    );

    let decoded =
        SignedAnnouncement::<CapabilityMembership>::decode_and_verify(&old_bytes, kp.entity_id())
            .expect("an old-type signature verifies on the new type");
    assert_eq!(decoded.payload.tags, new.tags);
}

#[derive(Serialize)]
struct LegacySnapshotEntry {
    key: (u64, NodeId),
    payload: LegacyMembership,
    node_id: NodeId,
    generation: u64,
    received_offset_ns: u64,
    expires_offset_ns: u64,
}

#[derive(Serialize)]
struct LegacySnapshot {
    kind: u16,
    taken_at_unix_us: u64,
    entries: Vec<LegacySnapshotEntry>,
}

/// The current snapshot's serialized shape, field for field, with the
/// current payload. `FoldSnapshot<K>` itself is not deserializable for a
/// concrete fold (its derived serde bound asks `K: Deserialize`), and no
/// production path decodes one, so the test decodes into this mirror.
#[derive(serde::Deserialize)]
struct CurrentSnapshotEntry {
    key: (u64, NodeId),
    payload: CapabilityMembership,
    node_id: NodeId,
    generation: u64,
    received_offset_ns: u64,
    expires_offset_ns: u64,
}

#[derive(serde::Deserialize)]
struct CurrentSnapshot {
    kind: u16,
    taken_at_unix_us: u64,
    entries: Vec<CurrentSnapshotEntry>,
}

/// A snapshot written with the old `Vec<String>` payload decodes into the
/// current snapshot shape and restores, with no format bump.
#[test]
fn old_snapshot_restores_without_a_format_bump() {
    let tags = ["b-tag", "a-tag", "b-tag"];
    let old = LegacySnapshot {
        kind: CapabilityFold::KIND_ID,
        taken_at_unix_us: crate::adapter::net::current_timestamp_micros(),
        entries: vec![LegacySnapshotEntry {
            key: (0x51, 0xA),
            payload: legacy(0x51, &tags),
            node_id: 0xA,
            generation: 4,
            received_offset_ns: 0,
            expires_offset_ns: 60_000_000_000,
        }],
    };
    let bytes = postcard::to_allocvec(&old).expect("encode old snapshot");
    let decoded: CurrentSnapshot =
        postcard::from_bytes(&bytes).expect("decode as the current snapshot");
    let snap = FoldSnapshot::<CapabilityFold> {
        kind: decoded.kind,
        taken_at_unix_us: decoded.taken_at_unix_us,
        entries: decoded
            .entries
            .into_iter()
            .map(|e| FoldSnapshotEntry {
                key: e.key,
                payload: e.payload,
                node_id: e.node_id,
                generation: e.generation,
                received_offset_ns: e.received_offset_ns,
                expires_offset_ns: e.expires_offset_ns,
            })
            .collect(),
    };
    let fold = new_fold();
    fold.restore(snap, false).expect("restore");
    fold.with_state(|s| {
        let entry = s.get(&(0x51, 0xA)).expect("restored");
        let restored: Vec<&str> = entry.payload.tags.iter().map(TagStr::as_str).collect();
        assert_eq!(restored, tags, "content and order, duplicates kept");
        s.assert_expiry_index();
    });
    assert_eq!(interned(&fold), (2, 10), "two distinct tags");
}

// ---------------------------------------------------------------------
// Per-advertisement caps, on every intake path.
// ---------------------------------------------------------------------

fn n_tags(n: usize) -> Vec<String> {
    (0..n).map(|i| format!("t{i}")).collect()
}

#[test]
fn tag_caps_apply_on_every_intake_path() {
    let kp = EntityKeypair::generate();
    let fold = Arc::new(new_fold());

    // Exactly at the cap: accepted.
    let at_cap = n_tags(MAX_CAPABILITY_TAGS);
    let at_cap_refs: Vec<&str> = at_cap.iter().map(String::as_str).collect();
    assert_eq!(
        fold.apply(signed(&kp, 0xA, 1, membership(1, &at_cap_refs)))
            .expect("exactly at the cap is accepted"),
        ApplyOutcome::Inserted
    );

    // One over, by duplicates: refused whole, typed.
    let mut over = vec!["dup"; MAX_CAPABILITY_TAGS];
    over.push("dup");
    match fold.apply(signed(&kp, 0xB, 1, membership(1, &over))) {
        Err(FoldError::PayloadRejected {
            node_id: 0xB,
            reason:
                PayloadRejection::TooManyTags {
                    count,
                    max: MAX_CAPABILITY_TAGS,
                },
        }) if count == MAX_CAPABILITY_TAGS + 1 => {}
        other => panic!("expected a too-many-tags refusal, got {other:?}"),
    }

    // Overlong by UTF-8 bytes: 129 two-byte chars is 258 bytes.
    let long = "é".repeat(129);
    match fold.apply(signed(&kp, 0xC, 1, membership(1, &["ok", &long]))) {
        Err(FoldError::PayloadRejected {
            reason:
                PayloadRejection::TagTooLong {
                    index: 1, len: 258, ..
                },
            ..
        }) => {}
        other => panic!("expected a tag-too-long refusal, got {other:?}"),
    }

    // Typed signed dispatch: the same refusal through the registry. The
    // envelope's node id must be the signer's, or verification refuses
    // it before the payload is ever looked at.
    let registry = FoldRegistry::new();
    registry.register(Arc::clone(&fold));
    let dispatcher = EntityKeypair::generate();
    let dispatch_node = dispatcher.entity_id().node_id();
    let bytes = signed(&dispatcher, dispatch_node, 1, membership(1, &over))
        .encode()
        .expect("encode");
    assert!(registry.dispatch(&bytes, dispatcher.entity_id()).is_err());

    // Legacy intake: an announcement translated with one tag over the cap.
    let mut caps = CapabilitySet::new();
    for tag in n_tags(MAX_CAPABILITY_TAGS + 1) {
        caps = caps.add_tag(tag);
    }
    let ann = CapabilityAnnouncement::new(0xE, kp.entity_id().clone(), 1, caps);
    assert!(matches!(
        fold.apply(translate_announcement(&ann, None)),
        Err(FoldError::PayloadRejected {
            reason: PayloadRejection::TooManyTags { .. },
            ..
        })
    ));

    fold.with_state(|s| {
        assert_eq!(s.len(), 1, "only the at-cap advertisement is stored");
        assert!(s.keys_for(0xB).is_none() && s.keys_for(0xC).is_none());
        assert!(s.keys_for(dispatch_node).is_none() && s.keys_for(0xE).is_none());
    });
    let stats = fold.stats();
    assert_eq!(stats.limit_rejections, 4);
    assert_eq!(stats.applies_rejected, 4);
    assert_eq!(stats.interned, MAX_CAPABILITY_TAGS as u64);
}

// ---------------------------------------------------------------------
// Budget: fail-closed, net, and a refusal changes nothing.
// ---------------------------------------------------------------------

/// A refused advertisement, on Insert or on Replace, leaves the live
/// payload, the index, the expiry placement and deadline, the publisher
/// revision and cache validity exactly as they were; it bumps the normal
/// and the typed rejection counters and records one bounded audit event
/// that names no tag.
#[test]
fn a_budget_refusal_changes_nothing() {
    let kp = EntityKeypair::generate();
    let fold = fold_with_budget(3);
    let sink = Arc::new(RingFoldAuditSink::new(16));
    fold.set_audit_sink(Some(sink.clone() as Arc<dyn FoldAuditSink>));

    fold.apply(signed(&kp, 0xA, 1, membership(1, &["a", "b"])))
        .expect("A fits");
    let cache = CapabilitySetCache::new();
    let cached = cache.get_or_synthesize(&fold, 0xA);
    let key = (1, 0xA);
    let (rev, deadline, placement) = fold.with_state(|s| {
        (
            s.publisher_rev(0xA),
            s.get(&key).expect("A").expires_at,
            s.expiry_position(&key),
        )
    });
    let audit_before = sink.snapshot().len();

    // Insert path: B brings two new tags, 2 + 2 > 3.
    let secret = "zzz-secret-tag";
    match fold.apply(signed(&kp, 0xB, 1, membership(1, &["c", secret]))) {
        Err(FoldError::PayloadRejected { reason, .. }) => assert!(reason.is_budget()),
        other => panic!("expected a budget refusal, got {other:?}"),
    }
    // Replace path: A's refresh releases "b" (1 freed) but adds three:
    // 2 - 1 + 3 > 3.
    match fold.apply(signed(&kp, 0xA, 2, membership(1, &["a", "n1", "n2", "n3"]))) {
        Err(FoldError::PayloadRejected { reason, .. }) => assert!(reason.is_budget()),
        other => panic!("expected a budget refusal, got {other:?}"),
    }

    fold.with_state(|s| {
        assert!(s.keys_for(0xB).is_none(), "B not stored");
        let a = s.get(&key).expect("A");
        let tags: Vec<&str> = a.payload.tags.iter().map(TagStr::as_str).collect();
        assert_eq!(tags, ["a", "b"], "A's payload unchanged");
        assert_eq!(a.generation, 1);
        assert_eq!(a.expires_at, deadline, "deadline unchanged");
        assert_eq!(s.expiry_position(&key), placement, "placement unchanged");
        assert_eq!(s.publisher_rev(0xA), rev, "revision unchanged");
        s.assert_expiry_index();
    });
    assert!(
        Arc::ptr_eq(&cached, &cache.get_or_synthesize(&fold, 0xA)),
        "the cached set is still valid"
    );
    let index_has = |tag: &str| {
        fold.with_state_and_index(|_, index| index.dictionary().canonical(tag).is_some())
    };
    assert!(!index_has("c") && !index_has("n1"), "no tag admitted");
    assert_eq!(interned(&fold), (2, 2));

    let stats = fold.stats();
    assert_eq!(stats.budget_rejections, 2);
    assert_eq!(stats.applies_rejected, 2);
    let events: Vec<_> = sink.snapshot().into_iter().skip(audit_before).collect();
    assert_eq!(events.len(), 2);
    for event in &events {
        assert_eq!(event.kind, AuditKind::Custom("payload-budget-rejected"));
        let detail = event.detail.as_deref().unwrap_or_default();
        assert!(!detail.contains(secret), "the audit record names no tag");
        assert!(detail.len() < 256, "bounded");
    }
}

/// Budget arithmetic is net of the replaced payload: releasing a last-use
/// tag makes room for a new one, at a full dictionary.
#[test]
fn a_replacement_frees_its_last_use_tag_for_a_new_one() {
    let kp = EntityKeypair::generate();
    let fold = fold_with_budget(2);
    fold.apply(signed(&kp, 0xA, 1, membership(1, &["shared"])))
        .expect("A");
    fold.apply(signed(&kp, 0xB, 1, membership(1, &["shared", "old"])))
        .expect("B fills the budget");
    assert_eq!(
        fold.apply(signed(&kp, 0xB, 2, membership(1, &["shared", "new"])))
            .expect("fits net of the released tag"),
        ApplyOutcome::Replaced
    );
    fold.with_state_and_index(|_, index| {
        assert!(index.dictionary().canonical("old").is_none());
        assert_eq!(index.dictionary().uses_of("shared"), 2);
        assert_eq!(index.dictionary().uses_of("new"), 1);
    });
}

/// Dictionary admission happens only after merge accepts: a stale
/// advertisement that loses the merge never grows the dictionary.
#[test]
fn a_merge_rejected_advertisement_never_reaches_the_dictionary() {
    let kp = EntityKeypair::generate();
    let fold = new_fold();
    fold.apply(signed(&kp, 0xA, 5, membership(1, &["kept"])))
        .expect("A");
    assert_eq!(
        fold.apply(signed(&kp, 0xA, 4, membership(1, &["stale-only"])))
            .expect("apply"),
        ApplyOutcome::Rejected
    );
    assert_eq!(interned(&fold), (1, 4));
    fold.with_state_and_index(|_, index| {
        assert!(index.dictionary().canonical("stale-only").is_none());
    });
}

/// Liveness is fold-owned: a reader holding a cloned payload does not keep
/// a tag in the dictionary once the fold's last stored use is gone.
#[test]
fn a_pinned_reader_does_not_keep_a_tag_in_the_dictionary() {
    let kp = EntityKeypair::generate();
    let fold = new_fold();
    fold.apply(signed(&kp, 0xA, 1, membership(1, &["pinned"])))
        .expect("A");
    let held = fold.with_state(|s| s.get(&(1, 0xA)).expect("A").payload.clone());
    let snapshot = fold.snapshot();
    fold.evict_node(0xA, "test");
    assert_eq!(
        interned(&fold),
        (0, 0),
        "retired despite two external holders"
    );
    assert_eq!(held.tags[0], "pinned", "the holder's bytes stay readable");
    assert_eq!(snapshot.entries[0].payload.tags[0], "pinned");
}

/// Every intake path stores the same canonical allocation for a tag:
/// legacy translation, typed apply, and restore, across entries.
#[test]
fn intake_paths_share_one_canonical_allocation() {
    let kp = EntityKeypair::generate();
    let fold = new_fold();

    let caps = CapabilitySet::new()
        .add_tag("shared-tag")
        .add_tag("legacy-only");
    let ann = CapabilityAnnouncement::new(0xA, kp.entity_id().clone(), 1, caps);
    fold.apply(translate_announcement(&ann, None))
        .expect("legacy");
    fold.apply(signed(&kp, 0xB, 1, membership(7, &["shared-tag"])))
        .expect("typed");

    let tag_of = |fold: &Fold<CapabilityFold>, node: NodeId| {
        fold.with_state(|s| {
            let key = s.keys_for(node).expect("present")[0];
            s.get(&key)
                .expect("entry")
                .payload
                .tags
                .iter()
                .find(|t| *t == "shared-tag")
                .cloned()
                .expect("carries the tag")
        })
    };
    let (a, b) = (tag_of(&fold, 0xA), tag_of(&fold, 0xB));
    assert!(a.shares_allocation_with(&b), "legacy and typed share");
    assert_eq!(interned(&fold).0, 2);

    let restored = new_fold();
    restored.restore(fold.snapshot(), false).expect("restore");
    let (ra, rb) = (tag_of(&restored, 0xA), tag_of(&restored, 0xB));
    assert!(ra.shares_allocation_with(&rb), "restored entries share");
    assert_eq!(interned(&restored), interned(&fold));
}

// ---------------------------------------------------------------------
// Restore: complete or refused, never partial.
// ---------------------------------------------------------------------

fn tags_of(fold: &Fold<CapabilityFold>) -> Vec<(NodeId, Vec<String>)> {
    let mut out: Vec<(NodeId, Vec<String>)> = fold.with_state(|s| {
        s.entries
            .iter()
            .map(|(k, e)| (k.1, e.payload.tags.iter().map(|t| t.to_string()).collect()))
            .collect()
    });
    out.sort();
    out
}

#[test]
fn a_restore_over_a_limit_is_refused_and_leaves_the_fold() {
    let kp = EntityKeypair::generate();
    let fold = new_fold();
    fold.apply(signed(&kp, 0xA, 1, membership(1, &["live"])))
        .expect("A");
    let before = tags_of(&fold);
    let rev = fold.with_state(|s| s.publisher_rev(0xA));

    let source = new_fold();
    source
        .apply(signed(&kp, 0xB, 1, membership(1, &["fine"])))
        .expect("B");
    let mut snap = source.snapshot();
    snap.entries[0].payload.tags = n_tags(MAX_CAPABILITY_TAGS + 1)
        .into_iter()
        .map(TagStr::from)
        .collect();
    match fold.restore(snap, true) {
        Err(FoldError::RestoreRefused {
            reason: PayloadRejection::TooManyTags { .. },
        }) => {}
        other => panic!("expected a refused restore, got {other:?}"),
    }
    assert_eq!(tags_of(&fold), before, "old fold intact");
    assert_eq!(fold.with_state(|s| s.publisher_rev(0xA)), rev);
    assert_eq!(interned(&fold), (1, 4));
    fold.with_state(|s| s.assert_expiry_index());
}

#[test]
fn a_restore_over_the_budget_is_refused_and_leaves_the_fold() {
    let kp = EntityKeypair::generate();
    let fold = fold_with_budget(2);
    fold.apply(signed(&kp, 0xA, 1, membership(1, &["live"])))
        .expect("A");
    let before = tags_of(&fold);

    let source = new_fold();
    source
        .apply(signed(&kp, 0xB, 1, membership(1, &["x", "y"])))
        .expect("B");
    source
        .apply(signed(&kp, 0xC, 1, membership(1, &["z"])))
        .expect("C");
    match fold.restore(source.snapshot(), true) {
        Err(FoldError::RestoreRefused { reason }) => assert!(reason.is_budget()),
        other => panic!("expected a refused restore, got {other:?}"),
    }
    assert_eq!(tags_of(&fold), before, "old fold intact");

    // Within budget it succeeds, revisions stay monotonic, and the
    // dictionary holds exactly the restored state.
    let rev_before = fold.with_state(|s| s.publisher_rev(0xA));
    let small = new_fold();
    small
        .apply(signed(&kp, 0xA, 1, membership(1, &["p", "q"])))
        .expect("A'");
    fold.restore(small.snapshot(), true).expect("fits");
    assert!(fold.with_state(|s| s.publisher_rev(0xA)) > rev_before);
    assert_eq!(interned(&fold), (2, 2));
    fold.with_state(|s| s.assert_expiry_index());
}

/// An audit sink may read the fold's counters from `record`. A budget
/// refusal records its audit event while the apply still holds the index
/// write lock, so a `stats()` that took the index lock would deadlock the
/// apply (review defect 4 at `50a8c60d4`). The apply runs on its own
/// thread with a deadline, so a regression fails instead of hanging.
#[test]
fn an_audit_sink_can_read_stats_during_a_refusal() {
    struct StatsReadingSink {
        fold: std::sync::OnceLock<std::sync::Weak<Fold<CapabilityFold>>>,
        seen: parking_lot::Mutex<Vec<u64>>,
    }
    impl FoldAuditSink for StatsReadingSink {
        fn record(&self, _event: super::AuditEvent) {
            if let Some(fold) = self.fold.get().and_then(std::sync::Weak::upgrade) {
                let stats = fold.stats();
                self.seen.lock().push(stats.budget_rejections);
            }
        }
    }

    let kp = EntityKeypair::generate();
    let fold = Arc::new(fold_with_budget(1));
    let sink = Arc::new(StatsReadingSink {
        fold: std::sync::OnceLock::new(),
        seen: parking_lot::Mutex::new(Vec::new()),
    });
    let _ = sink.fold.set(Arc::downgrade(&fold));
    fold.set_audit_sink(Some(sink.clone() as Arc<dyn FoldAuditSink>));
    fold.apply(signed(&kp, 0xA, 1, membership(1, &["only"])))
        .expect("fits");

    let (tx, rx) = std::sync::mpsc::channel();
    let worker = {
        let fold = Arc::clone(&fold);
        std::thread::spawn(move || {
            let refused = fold
                .apply(signed(&kp, 0xB, 1, membership(1, &["second"])))
                .is_err();
            let _ = tx.send(refused);
        })
    };
    let refused = rx
        .recv_timeout(Duration::from_secs(10))
        .expect("the refusal completed instead of deadlocking");
    worker.join().expect("worker");
    assert!(refused);
    let seen = sink.seen.lock().clone();
    assert!(!seen.is_empty(), "the sink read stats from record");
}
