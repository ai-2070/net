//! Integration tests for the capability-broadcast subprotocol
//! (`SUBPROTOCOL_CAPABILITY_ANN = 0x0C00`).
//!
//! Covers the four load-bearing guarantees of Stage C-1:
//! - Two-node announce → find round-trip
//! - TTL expiry: post-TTL queries no longer return the peer
//! - Late joiner: session-open push catches new peers up
//! - Version skip: older announcements from the same peer are ignored
//!
//! Run: `cargo test --features net,cortex,fixtures --test capability_broadcast`

// Uses the `fixtures`-gated `apply_legacy_announcement` helper and cortex
// types, so it needs all three features — CI runs it under default features +
// `fixtures`. Skips cleanly otherwise instead of failing to compile.
#![cfg(all(feature = "net", feature = "cortex", feature = "fixtures"))]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::behavior::capability::{
    CapabilityAnnouncement, CapabilityFilter, CapabilitySet,
};
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig, SocketBufferConfig};

const TEST_BUFFER_SIZE: usize = 256 * 1024;
const PSK: [u8; 32] = [0x42u8; 32];

fn test_config() -> MeshNodeConfig {
    // Bind via `127.0.0.1:0` so the OS picks a free port — no
    // pre-bind reservation, no TOCTOU race with parallel tests.
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let mut cfg = MeshNodeConfig::new(addr, PSK)
        .with_heartbeat_interval(Duration::from_millis(200))
        .with_session_timeout(Duration::from_secs(5))
        .with_handshake(3, Duration::from_secs(2))
        .with_capability_gc_interval(Duration::from_millis(250));
    cfg.socket_buffers = SocketBufferConfig {
        send_buffer_size: TEST_BUFFER_SIZE,
        recv_buffer_size: TEST_BUFFER_SIZE,
    };
    cfg
}

/// Build an unstarted MeshNode and return it alongside its node_id.
async fn build_node() -> Arc<MeshNode> {
    build_node_with(|cfg| cfg).await
}

/// Build a MeshNode with a caller-supplied tweak to the test config.
async fn build_node_with<F>(tweak: F) -> Arc<MeshNode>
where
    F: FnOnce(MeshNodeConfig) -> MeshNodeConfig,
{
    let cfg = tweak(test_config());
    let keypair = EntityKeypair::generate();
    Arc::new(MeshNode::new(keypair, cfg).await.expect("MeshNode::new"))
}

/// Handshake two nodes (A initiator, B responder) and `start()` both.
async fn handshake(a: &Arc<MeshNode>, b: &Arc<MeshNode>) {
    let a_id = a.node_id();
    let b_id = b.node_id();
    let b_pub = *b.public_key();
    let b_addr = b.local_addr();

    let b_clone = b.clone();
    let accept = tokio::spawn(async move { b_clone.accept(a_id).await });
    // No pre-connect sleep — `handshake_initiator` and
    // `handshake_responder` each have internal retry-with-backoff.
    a.connect(b_addr, &b_pub, b_id)
        .await
        .expect("connect failed");
    accept
        .await
        .expect("accept task panicked")
        .expect("accept failed");
    a.start();
    b.start();
}

/// Poll `cond` on `node` every 25ms for up to 2s; returns `true` on
/// match. Callers use this instead of a fixed `sleep` so slow CI
/// boxes don't flake.
async fn wait_until<F>(node: &Arc<MeshNode>, cond: F) -> bool
where
    F: FnMut(&MeshNode) -> bool,
{
    wait_until_for(node, Duration::from_secs(2), cond).await
}

/// `wait_until` with a caller-chosen deadline — for conditions gated
/// on a configured window (e.g. the RT-1 trailing-edge flush) rather
/// than the fixed 2s.
async fn wait_until_for<F>(node: &Arc<MeshNode>, timeout: Duration, mut cond: F) -> bool
where
    F: FnMut(&MeshNode) -> bool,
{
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        if cond(node) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    cond(node)
}

/// After an NKpsk0 handshake, only the initiator learns the peer's
/// Noise static pubkey — the pattern has no `-> s` leg, so the
/// responder never sees the initiator's static. `peer_static_x25519`
/// reflects exactly what snow exposes: `Some(pub)` on the initiator
/// side, `None` on the responder side. The identity-envelope path
/// uses this to seal to the target when the source was the
/// initiator; the Stage 5 wiring handles the responder-side case
/// by refusing to transport identity when the static is unknown
/// (migration falls back to `public_only` identity).
#[tokio::test]
async fn peer_static_x25519_returns_peer_noise_pubkey_after_handshake() {
    let a = build_node().await;
    let b = build_node().await;
    handshake(&a, &b).await;

    let a_id = a.node_id();
    let b_id = b.node_id();

    // Initiator side: A learns B's static from the out-of-band
    // pubkey it handed to `connect()`, surfaced post-handshake by
    // snow's `get_remote_static`.
    assert_eq!(
        a.peer_static_x25519(b_id),
        Some(*b.public_key()),
        "initiator (A) should recover B's Noise static pubkey",
    );

    // Responder side: NKpsk0 has no `-> s`, so snow has no remote
    // static to return. Documented limitation of the current
    // pattern; Stage 5 of the identity-migration plan plans around
    // this by requiring the migration source to have initiated the
    // session to the target (or by falling back to public-only
    // identity transport).
    assert!(
        b.peer_static_x25519(a_id).is_none(),
        "responder (B) should see None under NKpsk0 — pattern discloses only -> e",
    );

    // No session with an unknown node_id — return None, not zeros.
    assert!(a.peer_static_x25519(0xDEAD_BEEF_CAFE_F00D).is_none());
}

/// Regression (Cubic-AI P1: leaking Noise static private key).
///
/// `MeshNode::static_x25519_priv()` used to return a raw
/// `[u8; 32]` copy of the node's long-term Noise static private
/// key. Any SDK caller with an `Arc<Mesh>` could call it and
/// exfiltrate the key — the key that backs this node's *identity*
/// in the mesh, not just one migration's envelope-open.
///
/// The fix deletes that method and replaces it with
/// [`MeshNode::migration_identity_context`], which returns a
/// `MigrationIdentityContext` whose closures capture the key
/// internally. Callers get the *functionality* they need
/// (open a sealed envelope) without ever touching the raw bytes.
///
/// This test exercises the new surface end-to-end: build an
/// identity envelope on A sealed against B's public static, then
/// unseal it via B's context. Functional regression — if the
/// context's closure is wired incorrectly, the keypair won't
/// round-trip.
#[tokio::test]
async fn migration_identity_context_unseals_envelope_without_exposing_key() {
    use net::adapter::net::identity::{EntityKeypair, IdentityEnvelope};
    use net::adapter::net::state::causal::CausalLink;
    use net::adapter::net::state::snapshot::StateSnapshot;

    let a = build_node().await;
    let b = build_node().await;
    handshake(&a, &b).await;

    // Source-side daemon identity (what an envelope would carry).
    let daemon_kp = EntityKeypair::generate();

    // Build an envelope on A sealed against B's public static. We
    // read B's public half the normal way (it's public key
    // material, not secret).
    let b_static_pub = *b.public_key();
    let chain_link = CausalLink {
        origin_hash: daemon_kp.origin_hash(),
        horizon_encoded: 0,
        sequence: 0,
        parent_hash: 0,
    };
    let envelope =
        IdentityEnvelope::new(&daemon_kp, b_static_pub, &chain_link).expect("seal envelope");

    // Wrap into a minimal StateSnapshot carrying only the envelope
    // (the rest of the fields are stubbed for the open path).
    let snapshot = StateSnapshot {
        version: net::adapter::net::state::snapshot::SNAPSHOT_VERSION,
        entity_id: daemon_kp.entity_id().clone(),
        chain_link,
        through_seq: 0,
        state: bytes::Bytes::new(),
        horizon: Default::default(),
        created_at: 0,
        bindings_bytes: Vec::new(),
        identity_envelope: Some(envelope),
        head_payload: None,
    };

    // Unseal via B's migration-identity context. The private key
    // never leaves B's closures — this call is the only surface.
    let ctx = b.migration_identity_context();
    let opened = (ctx.unseal_snapshot)(&snapshot)
        .expect("unseal succeeds")
        .expect("envelope present → Some(keypair)");

    // Round-trip: the opened keypair matches the source.
    assert_eq!(opened.entity_id(), daemon_kp.entity_id());
    assert_eq!(opened.origin_hash(), daemon_kp.origin_hash());

    // peer_static_lookup on the context finds B's view of A's
    // static (initiator side — A initiated the handshake to B).
    let a_from_b_via_ctx = (a.migration_identity_context().peer_static_lookup)(b.node_id());
    assert_eq!(
        a_from_b_via_ctx,
        Some(b_static_pub),
        "peer_static_lookup on A's context must find B's static \
         (A initiated to B; initiator learns responder's static)",
    );

    // Size canary — pinned here as an integration-level
    // regression alongside the unit-level one in
    // `migration_handler::tests`. If the context ever grows
    // (e.g. re-adding `local_x25519_priv: [u8; 32]`), this
    // assertion fires.
    use std::mem::size_of;
    let fat_ptr = 2 * size_of::<usize>();
    assert_eq!(
        size_of::<net::adapter::net::subprotocol::MigrationIdentityContext>(),
        2 * fat_ptr,
        "MigrationIdentityContext must stay two Arc<dyn Fn ...> — a \
         size bump means a new field, most likely the Noise static \
         private key leaking back as a readable [u8; 32]",
    );
}

#[tokio::test]
async fn two_node_announce_is_visible() {
    let a = build_node().await;
    let b = build_node().await;
    handshake(&a, &b).await;

    let caps = CapabilitySet::new().add_tag("gpu").add_tag("inference");
    a.announce_capabilities(caps)
        .await
        .expect("announce failed");

    let filter = CapabilityFilter::new().require_tag("gpu");
    let a_id = a.node_id();
    let arrived = wait_until(&b, |node| {
        node.find_nodes_by_filter(&filter).contains(&a_id)
    })
    .await;
    assert!(arrived, "B did not observe A's capability announcement");
}

#[tokio::test]
async fn announcement_expires_after_ttl() {
    let a = build_node().await;
    let b = build_node().await;
    handshake(&a, &b).await;

    let caps = CapabilitySet::new().add_tag("ephemeral");
    // TTL = 1s; GC tick from `test_config` is 250ms, so two or three
    // sweeps land before we re-query at 1.5s. Signed — B's default
    // now drops unsigned announcements, and this test is exercising
    // TTL + GC, not the sign-gate.
    a.announce_capabilities_with(caps, Duration::from_secs(1), true)
        .await
        .expect("announce failed");

    let filter = CapabilityFilter::new().require_tag("ephemeral");
    let a_id = a.node_id();
    assert!(
        wait_until(&b, |n| n.find_nodes_by_filter(&filter).contains(&a_id)).await,
        "B never indexed A's announcement in the first place"
    );

    // Wait beyond TTL; GC should evict.
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    let still_present = b.find_nodes_by_filter(&filter).contains(&a_id);
    assert!(
        !still_present,
        "B still returns A after TTL expiry (GC not running?)"
    );
}

#[tokio::test]
async fn late_joiner_receives_session_open_push() {
    let a = build_node().await;

    // A announces *before* B exists.
    let caps = CapabilitySet::new().add_tag("preannounced");
    a.announce_capabilities(caps)
        .await
        .expect("announce failed");

    // B joins the party.
    let b = build_node().await;
    handshake(&a, &b).await;

    let filter = CapabilityFilter::new().require_tag("preannounced");
    let a_id = a.node_id();
    let arrived = wait_until(&b, |n| n.find_nodes_by_filter(&filter).contains(&a_id)).await;
    assert!(
        arrived,
        "session-open push did not deliver the pre-announcement"
    );
}

#[tokio::test]
async fn require_signed_capabilities_drops_unsigned_announcements() {
    // Post-E-1, `announce_capabilities` signs by default. Test the
    // policy knob by explicitly calling `announce_capabilities_with`
    // with `sign = false` — receiver B's flag must drop those.
    // Receiver B has the flag on; sender A announces unsigned.
    // A self-indexes its own announcement (local path bypasses
    // receive), so a self-query on A still matches — only B's view
    // should be blank.
    let a = build_node().await;
    let b = build_node_with(|cfg| cfg.with_require_signed_capabilities(true)).await;
    handshake(&a, &b).await;

    a.announce_capabilities_with(
        CapabilitySet::new().add_tag("classified"),
        Duration::from_secs(60),
        false, // unsigned
    )
    .await
    .expect("announce failed");

    // A sees itself (local self-index isn't subject to the flag).
    let filter = CapabilityFilter::new().require_tag("classified");
    assert!(
        a.find_nodes_by_filter(&filter).contains(&a.node_id()),
        "sender lost its own self-index"
    );

    // Give the receive path a few ms to process (or drop).
    tokio::time::sleep(Duration::from_millis(100)).await;

    // B must not have indexed A's unsigned announcement.
    assert!(
        !b.find_nodes_by_filter(&filter).contains(&a.node_id()),
        "receiver accepted an unsigned announcement despite require_signed_capabilities=true"
    );
}

#[tokio::test]
async fn stale_versions_are_ignored_by_index() {
    // Dodges the mesh entirely — the version-skip invariant is a
    // property of the fold-backed capability state itself, not the
    // subprotocol. Keeping the test here so the whole "capability
    // pipeline" suite lives together and this regression catches
    // anyone who alters fold-apply semantics.
    use net::adapter::net::behavior::fold::{capability_bridge, CapabilityFold, Fold};
    use net::adapter::net::EntityId;

    let fold = Fold::<CapabilityFold>::with_sweep_interval(std::time::Duration::ZERO);
    let caps_v1 = CapabilitySet::new().add_tag("v1");
    let caps_v2 = CapabilitySet::new().add_tag("v2");

    // Direct fold test — no mesh, no signature verification.
    // A zero-byte EntityId is a valid data-structure input even
    // though it's not a valid ed25519 public key.
    let eid = EntityId::from_bytes([0u8; 32]);
    let v1 = CapabilityAnnouncement::new(/* node_id */ 0xAA, eid.clone(), 1, caps_v1);
    let v2 = CapabilityAnnouncement::new(0xAA, eid, 2, caps_v2);

    capability_bridge::apply_legacy_announcement(&fold, v2, None, 0)
        .expect("apply legacy announcement in fixture");
    capability_bridge::apply_legacy_announcement(&fold, v1, None, 0)
        .expect("apply legacy announcement in fixture"); // older — must be a no-op

    let v2_filter = CapabilityFilter::new().require_tag("v2");
    assert_eq!(
        capability_bridge::find_nodes_matching(&fold, &v2_filter),
        vec![0xAA]
    );

    let v1_filter = CapabilityFilter::new().require_tag("v1");
    assert!(
        capability_bridge::find_nodes_matching(&fold, &v1_filter).is_empty(),
        "older version overwrote the newer one"
    );
}

/// Regression for a cubic-flagged P1: the announcement handler
/// verified the signature against `entity_id` but never checked
/// that `node_id` matched a derivation from `entity_id`. A signer
/// could therefore produce a valid signature claiming any
/// `node_id`, poisoning the capability index and route learning
/// for an unrelated peer.
///
/// The fix asserts `ann.entity_id.node_id() == ann.node_id`
/// after signature verification. This test constructs a forged
/// announcement — A's real entity_id, A's real signature, but a
/// made-up `node_id` — and ships it via the subprotocol channel.
/// The receiver must NOT index the forged node_id.
#[tokio::test]
async fn forged_node_id_rejected_even_with_valid_signature() {
    use net::adapter::net::behavior::SUBPROTOCOL_CAPABILITY_ANN;

    let a = build_node().await;
    let b = build_node().await;
    handshake(&a, &b).await;

    // Craft a forged announcement with a fresh keypair. The
    // signature is valid (signer == entity_id), but we deliberately
    // stamp a `node_id` that does NOT match
    // `entity_id.node_id()` — that's the spoof the fix catches.
    let attacker_kp = EntityKeypair::generate();
    let forged_node_id: u64 = 0x1234_5678_9ABC_DEF0;
    assert_ne!(
        forged_node_id,
        attacker_kp.node_id(),
        "fixture: forged_node_id must differ from the signer's real node_id",
    );

    let caps = CapabilitySet::new().add_tag("forged-node-id-probe");
    let mut ann =
        CapabilityAnnouncement::new(forged_node_id, attacker_kp.entity_id().clone(), 1, caps);
    ann.sign(&attacker_kp);
    assert!(
        ann.verify().is_ok(),
        "forged announcement still carries a valid signature",
    );

    // Ship the raw payload via A's subprotocol channel.
    let payload = ann.to_bytes();
    a.send_subprotocol(b.local_addr(), SUBPROTOCOL_CAPABILITY_ANN, &payload)
        .await
        .expect("send forged announcement");

    // B should NOT admit the forged node_id into its index.
    let filter = CapabilityFilter::new().require_tag("forged-node-id-probe");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !b.find_nodes_by_filter(&filter).contains(&forged_node_id),
        "receiver indexed a forged node_id despite derivation mismatch — \
         node_id must be bound to entity_id cryptographically",
    );
}

/// Regression for a cubic-flagged P2: an attacker that forwards a
/// victim's signed announcement through their own session used to
/// get TOFU-bound to the victim's `entity_id`. The announcement's
/// signature was valid (the victim really signed it), but the
/// `from_node` on the receiver side was the attacker, not the
/// origin — so the binding said "attacker's session belongs to
/// victim," later satisfying `authorize_subscribe`'s entity lookup
/// for the wrong peer. The fix restricts TOFU pinning to
/// announcements that arrived **directly** from the origin
/// (`hop_count == 0`); forwarded announcements still update the
/// capability index + routing but no longer touch
/// `peer_entity_ids`.
#[tokio::test]
async fn forwarded_announcement_does_not_tofu_pin_forwarder_to_victim_entity() {
    use net::adapter::net::behavior::SUBPROTOCOL_CAPABILITY_ANN;

    let attacker = build_node().await;
    let receiver = build_node().await;
    handshake(&attacker, &receiver).await;

    // The victim never joins the network — the attacker harvested
    // the victim's signed announcement bytes (say, from an earlier
    // multi-hop path) and now replays them via their own session.
    let victim_kp = EntityKeypair::generate();
    let victim_entity = victim_kp.entity_id().clone();
    let victim_node_id = victim_kp.node_id();

    let caps = CapabilitySet::new().add_tag("forwarded-tofu-probe");
    let mut ann = CapabilityAnnouncement::new(victim_node_id, victim_entity.clone(), 1, caps);
    ann.sign(&victim_kp);
    assert!(ann.verify().is_ok(), "victim's signature is valid");

    // Bump hop_count so the receiver treats this as a forwarded
    // announcement (skips the `ann.node_id == from_node` check and
    // falls into the relay path). Signature verification still
    // passes because `signed_payload` zeros hop_count.
    ann.hop_count = 1;

    let payload = ann.to_bytes();
    attacker
        .send_subprotocol(receiver.local_addr(), SUBPROTOCOL_CAPABILITY_ANN, &payload)
        .await
        .expect("send forwarded announcement");

    // The announcement may still land in the capability index for
    // the victim's node_id — that's fine, the signature is real.
    let filter = CapabilityFilter::new().require_tag("forwarded-tofu-probe");
    let arrived = wait_until(&receiver, |n| {
        n.find_nodes_by_filter(&filter).contains(&victim_node_id)
    })
    .await;
    assert!(
        arrived,
        "receiver should still index the victim by node_id — signature is valid",
    );

    // But the attacker's session must NOT be TOFU-bound to the
    // victim's entity_id. That's the core property: a forwarder
    // cannot impersonate the origin for direct-session auth.
    let attacker_node_id = attacker.node_id();
    assert!(
        receiver.peer_entity_id(attacker_node_id)
            != Some(victim_entity.clone()),
        "attacker's session got TOFU-pinned to the victim's entity_id via a forwarded announcement — \
         forwarder can now impersonate origin for channel auth",
    );
}

/// Regression for a cubic-flagged P1/P2: TOFU used to pin the
/// `(from_node → entity_id)` mapping from the first seen
/// announcement regardless of whether the announcement was
/// authenticated. An unauthenticated peer could therefore poison
/// the binding with a victim's `entity_id`, later satisfying
/// token-based channel checks for that spoofed identity. The fix
/// restricts TOFU pinning to signature-verified announcements;
/// unauthenticated deployments that run without signatures get no
/// pin at all (channel auth fails cleanly at "missing entity").
///
/// Explicit opt-out: the receiver sets
/// `require_signed_capabilities = false` so unsigned announcements
/// still reach the dispatch path (the safer post-fix default
/// drops them up front). This test covers the "discovery without
/// signatures" deployment shape and asserts the defence-in-depth
/// state guards still hold under it.
#[tokio::test]
async fn unsigned_announcement_does_not_tofu_pin_entity() {
    let a = build_node().await;
    let b = build_node_with(|cfg| cfg.with_require_signed_capabilities(false)).await;
    handshake(&a, &b).await;

    // A announces UNSIGNED caps. B accepts (explicit opt-out
    // below) but must NOT trust `ann.entity_id` enough to pin it.
    a.announce_capabilities_with(
        CapabilitySet::new().add_tag("unsigned-tofu-probe"),
        Duration::from_secs(60),
        false, // unsigned
    )
    .await
    .expect("announce");

    // Index still admits the announcement under the opt-out.
    let filter = CapabilityFilter::new().require_tag("unsigned-tofu-probe");
    let a_id = a.node_id();
    let arrived = wait_until(&b, |n| n.find_nodes_by_filter(&filter).contains(&a_id)).await;
    assert!(arrived, "unsigned announcement should still index");

    // But the TOFU map must stay empty for this peer — no pin
    // from an unauthenticated announcement.
    assert!(
        b.peer_entity_id(a_id).is_none(),
        "TOFU pin established from an unsigned announcement — \
         unauthenticated entity_id is attacker-controlled input",
    );
}

/// Regression for a cubic-flagged P1: the subnet-assignment write
/// was gated on `signature_verified` but not on
/// `ann.hop_count == 0`, so a **signed** forwarded announcement
/// still wrote `peer_subnets[from_node]` — where `from_node` is
/// the relay, not the origin. A crafted relay could therefore
/// overwrite its own legitimate subnet binding with whatever
/// subnet the forwarded caps would derive to, or poison a
/// legitimate peer's subnet binding by being the last hop on its
/// path. Matching the TOFU-pin pattern: both writes are now gated
/// on `hop_count == 0`.
#[tokio::test]
async fn forwarded_announcement_does_not_write_relay_peer_subnet() {
    use net::adapter::net::behavior::SUBPROTOCOL_CAPABILITY_ANN;
    use net::adapter::net::{SubnetPolicy, SubnetRule};

    let attacker = build_node().await;

    let rule = SubnetRule::new("region:", 0).map("privileged", 1);
    let policy = SubnetPolicy::new().add_rule(rule);
    let receiver = build_node_with(|cfg| cfg.with_subnet_policy(Arc::new(policy))).await;
    handshake(&attacker, &receiver).await;

    // Harvested victim bytes: a real, signature-valid announcement
    // with caps that would classify the origin into the non-GLOBAL
    // subnet. The attacker replays it via its own session with
    // hop_count=1 (forwarded).
    let victim_kp = EntityKeypair::generate();
    let caps = CapabilitySet::new().add_tag("region:privileged");
    let mut ann =
        CapabilityAnnouncement::new(victim_kp.node_id(), victim_kp.entity_id().clone(), 1, caps);
    ann.sign(&victim_kp);
    assert!(ann.verify().is_ok(), "victim's signature is valid");
    ann.hop_count = 1;

    attacker
        .send_subprotocol(
            receiver.local_addr(),
            SUBPROTOCOL_CAPABILITY_ANN,
            &ann.to_bytes(),
        )
        .await
        .expect("send forwarded announcement");

    // The index may admit the victim by node_id (signature is real),
    // but the attacker's own session must NOT have been shifted into
    // the forwarded subnet — that binding belongs to the origin,
    // not the relay.
    let filter = CapabilityFilter::new().require_tag("region:privileged");
    let victim_node_id = victim_kp.node_id();
    let arrived = wait_until(&receiver, |n| {
        n.find_nodes_by_filter(&filter).contains(&victim_node_id)
    })
    .await;
    assert!(arrived, "receiver should still index the victim by node_id");

    // Give the dispatch path a beat in case the subnet write lags.
    tokio::time::sleep(Duration::from_millis(100)).await;

    assert!(
        receiver.peer_subnet(attacker.node_id()).is_none(),
        "forwarded announcement wrote the relay's subnet — a crafted last \
         hop can reshape any legitimate peer's SubnetLocal visibility",
    );
}

/// Regression for a cubic-flagged P1: even with the default
/// (`require_signed_capabilities = true`) dropping unsigned
/// announcements up-front, we want belt-and-braces so an
/// explicit opt-out for discovery can't accidentally re-open the
/// auth surface. `peer_subnets` is populated from
/// `ann.capabilities` via the subnet policy and is later consulted
/// by `subnet_visible` on the publish / subscribe paths — an
/// unsigned announcement must not be allowed to pick the peer's
/// subnet. This test opts out of the signature requirement, sends
/// an unsigned announcement whose caps would land the peer in a
/// non-GLOBAL subnet under any plausible policy, and asserts the
/// subnet binding stays unwritten.
#[tokio::test]
async fn unsigned_announcement_does_not_write_peer_subnet() {
    use net::adapter::net::{SubnetPolicy, SubnetRule};

    let a = build_node().await;

    // Receiver opts out of require_signed AND installs a subnet
    // policy whose rule maps `region:privileged` to a non-zero
    // level. `SubnetPolicy::assign` matches on a tag prefix; a
    // peer carrying that tag lands in a non-GLOBAL subnet. If the
    // write path were live, an attacker could drop itself into
    // that subnet just by announcing the matching tag.
    let rule = SubnetRule::new("region:", 0).map("privileged", 1);
    let policy = SubnetPolicy::new().add_rule(rule);
    let b = build_node_with(|cfg| {
        cfg.with_require_signed_capabilities(false)
            .with_subnet_policy(Arc::new(policy))
    })
    .await;
    handshake(&a, &b).await;

    a.announce_capabilities_with(
        CapabilitySet::new().add_tag("region:privileged"),
        Duration::from_secs(60),
        false, // unsigned — attacker lying about caps
    )
    .await
    .expect("announce");

    // Capability index is allowed to pick it up (opt-out lets
    // discovery still work).
    let filter = CapabilityFilter::new().require_tag("region:privileged");
    let a_id = a.node_id();
    let arrived = wait_until(&b, |n| n.find_nodes_by_filter(&filter).contains(&a_id)).await;
    assert!(
        arrived,
        "unsigned announcement should still index under opt-out",
    );

    // Give the dispatch path another beat in case the subnet
    // write lags the index insert.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // `peer_subnets` must stay empty for this peer — the unsigned
    // ann doesn't write here, so `subnet_visible` decisions on
    // `SubnetLocal` channels can't be steered by attacker input.
    assert!(
        b.peer_subnet(a_id).is_none(),
        "unsigned announcement was allowed to pick the peer's subnet — \
         subnet_visible decisions become attacker-controlled",
    );
}

/// Regression test for TEST_COVERAGE_PLAN §P1-6: a signed
/// announcement whose `entity_id` derivation doesn't match its
/// claimed `node_id` must be dropped by the receiver even when
/// the signature itself verifies cleanly. The origin's keypair
/// owns one `node_id`; an announcement that ships a different
/// `node_id` is an attempt to poison the capability index /
/// routing table for a peer the signer doesn't control.
///
/// The binding check (`ann.entity_id.node_id() != ann.node_id`
/// in the handler) is what blocks this — signature validity
/// alone is insufficient because the signature covers `entity_id`
/// but not `node_id`. The check fires on every code path (direct
/// hop_count==0 AND forwarded hop_count>0), so a valid-signed
/// malformed-binding announcement can't sneak through by
/// claiming to have been forwarded.
///
/// This test sends from the attacker's session. Assertions:
/// (1) receiver does NOT index the spoofed `node_id`;
/// (2) receiver's `peer_entity_id(attacker)` is not rebound to
///     the victim's entity_id (the TOFU path would be skipped
///     because the binding check rejects before TOFU runs).
///
/// Defense-in-depth with `require_signed_capabilities = true`
/// (the default): the receiver's first gate is
/// "unsigned → drop"; this test's announcement is signed, so
/// that gate passes and the binding check is the load-bearing
/// line.
#[tokio::test]
async fn signed_announcement_with_mismatched_node_id_entity_id_is_rejected() {
    use net::adapter::net::behavior::SUBPROTOCOL_CAPABILITY_ANN;

    let attacker = build_node().await;
    let receiver = build_node().await;
    handshake(&attacker, &receiver).await;

    // Victim keypair — the attacker knows victim's public key
    // (capability announcements are public) but not the
    // private key. Still, construct one to get a valid
    // `EntityId` that derives to some `node_id`.
    let victim_kp = EntityKeypair::generate();
    let victim_entity = victim_kp.entity_id().clone();
    let victim_node_id = victim_kp.node_id();

    // Malformed announcement: claim a `node_id` that differs
    // from `entity_id.node_id()`. Sign with the victim's
    // keypair so the signature is genuinely valid.
    let bogus_node_id = victim_node_id.wrapping_add(1);
    assert_ne!(
        bogus_node_id, victim_node_id,
        "bogus_node_id must differ from the legitimate derivation",
    );

    let caps = CapabilitySet::new().add_tag("binding-mismatch-probe");
    let mut ann = CapabilityAnnouncement::new(bogus_node_id, victim_entity.clone(), 1, caps);
    ann.sign(&victim_kp);
    assert!(
        ann.verify().is_ok(),
        "signature must verify — we want to isolate the binding check",
    );

    // Send as a forwarded announcement (hop_count > 0) to exercise
    // the forwarding-path code. The binding check fires on both
    // hop_count==0 and hop_count>0 paths; the plan notes the
    // forwarding path specifically because TOFU-skip on
    // hop_count>0 might be mistaken for "no validation at all."
    ann.hop_count = 1;
    let payload = ann.to_bytes();
    attacker
        .send_subprotocol(receiver.local_addr(), SUBPROTOCOL_CAPABILITY_ANN, &payload)
        .await
        .expect("send bogus forwarded announcement");

    // Give the dispatch path time to process + reject.
    tokio::time::sleep(Duration::from_millis(150)).await;

    // (1) Neither node_id was indexed. The bogus announcement
    //     must not have produced an entry for bogus_node_id or
    //     for victim_node_id (the caps tag is bogus either way).
    let filter = CapabilityFilter::new().require_tag("binding-mismatch-probe");
    let matches = receiver.find_nodes_by_filter(&filter);
    assert!(
        !matches.contains(&bogus_node_id),
        "receiver indexed the spoofed node_id despite the binding mismatch — \
         the entity_id→node_id binding check failed to reject the announcement",
    );
    assert!(
        !matches.contains(&victim_node_id),
        "receiver indexed the victim's legitimate node_id using data from the \
         spoofed announcement — binding check should reject BEFORE index runs",
    );

    // (2) Attacker's session is not TOFU-pinned to the victim's
    //     entity_id. The binding check runs before the TOFU path
    //     so a rejected announcement never reaches the pin logic.
    let attacker_node_id = attacker.node_id();
    assert!(
        receiver.peer_entity_id(attacker_node_id) != Some(victim_entity),
        "attacker's session got TOFU-pinned to victim's entity_id via a \
         binding-mismatched announcement — forwarder / sender can impersonate \
         the victim for channel-auth purposes",
    );
}

// =========================================================================
// RT-2 (REALTIME_ROUTING_AND_DISCOVERY_PLAN): echo-storm tripwire.
// The local-caps change signal must bump ONLY on local-origin
// mutations. If an inbound peer announcement ever bumps it, a
// change-driven announcer (RT-3) subscribed to the signal would
// re-announce on every peer announcement — a mesh-wide feedback
// loop. This test is the hard gate for wiring RT-3.
// =========================================================================

#[tokio::test]
async fn inbound_announcements_do_not_bump_local_caps_generation() {
    let a = build_node().await;
    let b = build_node().await;
    handshake(&a, &b).await;

    let before = b.local_caps_generation();

    a.announce_capabilities(CapabilitySet::new().add_tag("echo-probe"))
        .await
        .expect("announce");
    let filter = CapabilityFilter::new().require_tag("echo-probe");
    let a_id = a.node_id();
    assert!(
        wait_until(&b, |n| n.find_nodes_by_filter(&filter).contains(&a_id)).await,
        "B never received A's announcement",
    );

    assert_eq!(
        b.local_caps_generation(),
        before,
        "an inbound peer announcement bumped the LOCAL caps generation — \
         a change-driven announcer subscribed to this signal would echo \
         every peer announcement back to the mesh",
    );

    // A's own announce is a local-origin publication of its baseline,
    // not a registry mutation — it must not bump A's signal either
    // (announce is the OUTPUT of the signal's consumer, not an input;
    // bumping here would make RT-3 self-retrigger).
    assert_eq!(
        a.local_caps_generation(),
        0,
        "announce_capabilities itself must not bump the local-caps signal",
    );
}

// =========================================================================
// RT-1 (REALTIME_ROUTING_AND_DISCOVERY_PLAN): trailing-edge announce
// rate limiter. An announce landing inside `min_announce_interval`
// must coalesce into one flush at window end — pre-RT-1 it was
// silently dropped until the next out-of-window announce or the
// 150 s re-announce keep-alive.
// =========================================================================

/// The window is sized so the leading-edge delivery wait (2 s cap,
/// typically <100 ms on loopback) cannot overrun it and turn the
/// "in-window" announce into an out-of-window one.
#[tokio::test]
async fn in_window_announce_flushes_at_window_end() {
    let window = Duration::from_secs(3);
    let a = build_node_with(|cfg| cfg.with_min_announce_interval(window)).await;
    let b = build_node().await;
    handshake(&a, &b).await;
    // The flush task holds a `Weak<MeshNode>` — same constraint as
    // the re-announce loop, so upgrade the bare `start()` from
    // `handshake` to `start_arc`. Idempotent.
    a.start_arc();

    let a_id = a.node_id();

    // Leading edge: first announce broadcasts immediately.
    a.announce_capabilities(CapabilitySet::new().add_tag("rt1:v1"))
        .await
        .expect("first announce");
    let v1 = CapabilityFilter::new().require_tag("rt1:v1");
    assert!(
        wait_until(&b, |n| n.find_nodes_by_filter(&v1).contains(&a_id)).await,
        "B never saw the leading-edge announcement",
    );

    // In-window: withheld from the wire (nothing was sent, so B
    // can't know it), but scheduled for the trailing-edge flush.
    a.announce_capabilities(CapabilitySet::new().add_tag("rt1:v2"))
        .await
        .expect("second announce");
    let v2 = CapabilityFilter::new().require_tag("rt1:v2");
    assert!(
        !b.find_nodes_by_filter(&v2).contains(&a_id),
        "in-window announce hit the wire immediately — rate limit gone",
    );

    // The flush fires when the window (measured from the leading
    // edge) elapses; poll past it with CI slack.
    assert!(
        wait_until_for(&b, window + Duration::from_secs(2), |n| {
            n.find_nodes_by_filter(&v2).contains(&a_id)
        })
        .await,
        "suppressed announce was never flushed at the window end",
    );
}

/// A burst of in-window announces collapses into ONE flush carrying
/// the newest capability set — the flush reads `local_announcement`
/// at fire time, so intermediates never hit the wire.
#[tokio::test]
async fn in_window_burst_coalesces_to_newest() {
    let window = Duration::from_millis(1200);
    let a = build_node_with(|cfg| cfg.with_min_announce_interval(window)).await;
    let b = build_node().await;
    handshake(&a, &b).await;
    a.start_arc();

    let a_id = a.node_id();

    // Leading edge + three suppressed announces, back-to-back so
    // the burst is guaranteed in-window (no delivery wait between).
    a.announce_capabilities(CapabilitySet::new().add_tag("burst:v1"))
        .await
        .expect("announce v1");
    for tag in ["burst:v2", "burst:v3", "burst:v4"] {
        a.announce_capabilities(CapabilitySet::new().add_tag(tag))
            .await
            .expect("burst announce");
    }

    let v2 = CapabilityFilter::new().require_tag("burst:v2");
    let v3 = CapabilityFilter::new().require_tag("burst:v3");
    let v4 = CapabilityFilter::new().require_tag("burst:v4");
    let deadline = tokio::time::Instant::now() + window + Duration::from_secs(2);
    let mut saw_newest = false;
    while tokio::time::Instant::now() < deadline {
        // v2/v3 were never broadcast — only the flush's read of the
        // newest `local_announcement` (v4) may ever reach B.
        assert!(
            !b.find_nodes_by_filter(&v2).contains(&a_id)
                && !b.find_nodes_by_filter(&v3).contains(&a_id),
            "an intermediate suppressed announcement hit the wire",
        );
        if b.find_nodes_by_filter(&v4).contains(&a_id) {
            saw_newest = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(
        saw_newest,
        "the newest suppressed announcement was never flushed",
    );
}

// =========================================================================
// RT-5 review Finding 3: unknown-subprotocol forward-compat guard.
// A frame carrying a subprotocol id this build does not know must be
// dropped at the dispatch loop's guard, not mis-parsed as application
// events. The pre-fix fall-through created a receive-side stream (and
// charged credit / emitted a StreamWindow grant); the guard leaves no
// trace — observable here as the absence of a receive-side stream for
// the unknown id.
// =========================================================================

#[tokio::test]
async fn unknown_subprotocol_is_dropped_not_surfaced_as_events() {
    let a = build_node().await;
    let b = build_node().await;
    handshake(&a, &b).await;
    let a_id = a.node_id();
    let b_addr = b.local_addr();

    // An id no handler claims — well outside every allocated range.
    const UNKNOWN_SUBPROTOCOL: u16 = 0xFE00;
    a.send_subprotocol(b_addr, UNKNOWN_SUBPROTOCOL, b"\x01\x02\x03\x04")
        .await
        .expect("send unknown subprotocol");

    // Positive control: a real capability announce sent AFTER the
    // unknown frame. Once B has processed it, the single receive loop
    // has necessarily drained past the earlier unknown frame too — so
    // a missing stream below means "dropped", not "still in flight".
    a.announce_capabilities(CapabilitySet::new().add_tag("post-unknown"))
        .await
        .expect("announce");
    let filter = CapabilityFilter::new().require_tag("post-unknown");
    assert!(
        wait_until(&b, |n| n.find_nodes_by_filter(&filter).contains(&a_id)).await,
        "positive control: B never processed A's later capability announce",
    );

    assert!(
        b.stream_stats(a_id, UNKNOWN_SUBPROTOCOL as u64).is_none(),
        "unknown subprotocol created a receive-side stream — it fell \
         through to the application event path instead of being dropped",
    );
}
