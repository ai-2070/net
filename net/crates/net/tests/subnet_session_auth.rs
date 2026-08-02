//! S3 of `docs/internal/plans/SUBNET_AUTH_PLAN.md` — session
//! admission and the compiled `VerifiedSubnetContext`.
//!
//! The AEAD session does not by itself prove the leaf `EntityId`
//! (Noise `NKpsk0` leaves the initiator anonymous), so a protected
//! subnet session requires a fresh verifier challenge signed by the
//! subject's Ed25519 key. These witnesses pin that the proof is
//! non-transferable across entity, session, verifier, challenge,
//! credential set, target, and rights; that the routing-id pin is
//! compared atomically and never overwritten; and that the compiled
//! context dies on expiry, epoch movement, and withdrawal.

#![cfg(feature = "net")]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::identity::EntityKeypair;
use net::adapter::net::subnet::{
    admission::unix_now_secs, SubnetAuthError, SubnetAuthPresentation, SubnetAuthorityConfig,
    SubnetCredentialSet, SubnetGrant, SubnetRef, SubnetRevocationFloor, SubnetRights,
    TopologySubnetId,
};
use net::adapter::net::{MeshNode, MeshNodeConfig, SocketBufferConfig};

const PSK: [u8; 32] = [0x73u8; 32];
const DAY: u64 = 24 * 60 * 60;
const SCOPE: &[u8] = &[3, 7];

fn authority_root() -> EntityKeypair {
    EntityKeypair::from_bytes([0xA1; 32])
}

fn base_config() -> MeshNodeConfig {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let mut cfg = MeshNodeConfig::new(addr, PSK)
        .with_heartbeat_interval(Duration::from_millis(200))
        .with_session_timeout(Duration::from_secs(5))
        .with_handshake(3, Duration::from_secs(2));
    cfg.socket_buffers = SocketBufferConfig {
        send_buffer_size: 256 * 1024,
        recv_buffer_size: 256 * 1024,
    };
    cfg
}

/// Verifier node anchoring the authority; the subject node's keypair
/// is the credential subject, so `subject.node_id()` matches the
/// routing id the verifier sees.
struct Fixture {
    verifier: Arc<MeshNode>,
    subject: Arc<MeshNode>,
    subject_kp: EntityKeypair,
    root: EntityKeypair,
}

async fn fixture() -> Fixture {
    let root = authority_root();
    let cfg = base_config().with_subnet_authority(SubnetAuthorityConfig {
        authority: root.entity_id().clone(),
        roots: vec![root.entity_id().clone()],
        maximum_grant_lifetime_secs: 7 * DAY,
    });
    let verifier = Arc::new(
        MeshNode::new(EntityKeypair::generate(), cfg)
            .await
            .expect("verifier"),
    );
    let subject_kp = EntityKeypair::generate();
    let subject = Arc::new(
        MeshNode::new(subject_kp.clone(), base_config())
            .await
            .expect("subject"),
    );
    handshake(&subject, &verifier).await;
    Fixture {
        verifier,
        subject,
        subject_kp,
        root,
    }
}

async fn handshake(a: &Arc<MeshNode>, b: &Arc<MeshNode>) {
    let a_id = a.node_id();
    let b_pub = *b.public_key();
    let b_addr = b.local_addr();
    let b_clone = b.clone();
    let accept = tokio::spawn(async move { b_clone.accept(a_id).await });
    a.connect(b_addr, &b_pub, b.node_id())
        .await
        .expect("connect");
    accept.await.expect("accept task").expect("accept");
    a.start();
    b.start();
}

fn grant_for(
    root: &EntityKeypair,
    subject: &EntityKeypair,
    scope: &[u8],
    rights: SubnetRights,
    duration: u64,
) -> SubnetCredentialSet {
    SubnetCredentialSet::Direct(
        SubnetGrant::try_issue(
            root,
            root.entity_id().clone(),
            TopologySubnetId::new(scope),
            0,
            subject.entity_id().clone(),
            rights,
            1,
            unix_now_secs() - 60,
            duration,
        )
        .expect("issue grant"),
    )
}

/// Sign a presentation as `signer` for the given challenge/target.
#[allow(clippy::too_many_arguments)]
fn present(
    signer: &EntityKeypair,
    set: &SubnetCredentialSet,
    session_id: u64,
    verifier: &Arc<MeshNode>,
    nonce: [u8; 32],
    root: &EntityKeypair,
    target: &[u8],
    rights: SubnetRights,
) -> SubnetAuthPresentation {
    SubnetAuthPresentation::try_issue(
        signer,
        set.credential_set_hash(),
        session_id,
        verifier.entity_id().clone(),
        nonce,
        SubnetRef {
            authority: root.entity_id().clone(),
            path: TopologySubnetId::new(target),
        },
        rights,
    )
    .expect("issue presentation")
}

fn session_id_of(verifier: &Arc<MeshNode>, node: u64) -> u64 {
    verifier
        .peer_session_id(node)
        .expect("peer session must exist")
}

// ---------------------------------------------------------------------------
// Happy path
// ---------------------------------------------------------------------------

#[tokio::test]
async fn valid_presentation_compiles_a_context() {
    let f = fixture().await;
    let sub_id = f.subject.node_id();
    let set = grant_for(&f.root, &f.subject_kp, SCOPE, SubnetRights::ATTACH, DAY);
    let nonce = f
        .verifier
        .issue_subnet_challenge(sub_id)
        .expect("issue challenge");
    let sid = session_id_of(&f.verifier, sub_id);
    let p = present(
        &f.subject_kp,
        &set,
        sid,
        &f.verifier,
        nonce,
        &f.root,
        &[3, 7, 2],
        SubnetRights::ATTACH,
    );

    let ctx = f
        .verifier
        .admit_subnet_session(sub_id, &p, &set)
        .expect("admission must succeed");
    assert_eq!(ctx.scope, TopologySubnetId::new(SCOPE));
    assert_eq!(ctx.rights, SubnetRights::ATTACH);
    assert_eq!(&ctx.subject, f.subject_kp.entity_id());
    assert_eq!(ctx.subject_node, sub_id);
    assert_eq!(ctx.session_id, sid);

    // Readable from the forwarding path, and it authorizes the
    // granted subtree only.
    let live = f
        .verifier
        .subnet_context_for(sub_id)
        .expect("context must be live");
    assert!(live.allows(
        0,
        0,
        unix_now_secs(),
        TopologySubnetId::new(&[3, 7, 2]),
        SubnetRights::ATTACH
    ));
    assert!(!live.allows(
        0,
        0,
        unix_now_secs(),
        TopologySubnetId::new(&[4]),
        SubnetRights::ATTACH
    ));
    assert!(!live.allows(
        0,
        0,
        unix_now_secs(),
        TopologySubnetId::new(&[3, 7]),
        SubnetRights::ROUTE
    ));
}

/// A parent-scoped grant presented for a child admits the child: the
/// compiled context records that exact child as `attachment` while
/// retaining the parent grant root separately as `scope`. Substituting
/// `scope` for `attachment` would place this one peer everywhere
/// beneath the parent (SUBNET_AUTH_PLAN.md D5/D6).
#[tokio::test]
async fn parent_grant_attached_at_a_child_records_both_points() {
    let f = fixture().await;
    let sub_id = f.subject.node_id();
    // Grant scoped at the vehicle root [3].
    let set = grant_for(&f.root, &f.subject_kp, &[3], SubnetRights::ATTACH, DAY);
    let nonce = f
        .verifier
        .issue_subnet_challenge(sub_id)
        .expect("challenge");
    let sid = session_id_of(&f.verifier, sub_id);
    // …presented for the camera domain [3, 7, 2].
    let p = present(
        &f.subject_kp,
        &set,
        sid,
        &f.verifier,
        nonce,
        &f.root,
        &[3, 7, 2],
        SubnetRights::ATTACH,
    );
    let ctx = f
        .verifier
        .admit_subnet_session(sub_id, &p, &set)
        .expect("a parent grant admits a child attachment");

    assert_eq!(
        ctx.attachment,
        TopologySubnetId::new(&[3, 7, 2]),
        "attachment is the exact presented target",
    );
    assert_eq!(
        ctx.scope,
        TopologySubnetId::new(&[3]),
        "scope remains the credential's broader ceiling",
    );
    assert_ne!(
        ctx.attachment, ctx.scope,
        "the two must stay distinguishable — forwarding reads attachment",
    );
}

// ---------------------------------------------------------------------------
// No grant / no proof
// ---------------------------------------------------------------------------

#[tokio::test]
async fn session_without_admission_has_no_context() {
    let f = fixture().await;
    assert!(
        f.verifier.subnet_context_for(f.subject.node_id()).is_none(),
        "a handshaken peer holds no subnet authority until it proves a grant",
    );
}

/// A grant is not enough: without a signed presentation there is no
/// proof the presenter holds the subject key. A presentation signed
/// by a DIFFERENT entity over a valid grant is refused even though
/// the routing id is the attacker's own.
#[tokio::test]
async fn presentation_signed_by_another_entity_is_refused() {
    let f = fixture().await;
    let sub_id = f.subject.node_id();
    let set = grant_for(&f.root, &f.subject_kp, SCOPE, SubnetRights::ATTACH, DAY);
    let nonce = f
        .verifier
        .issue_subnet_challenge(sub_id)
        .expect("challenge");
    let sid = session_id_of(&f.verifier, sub_id);

    let impostor = EntityKeypair::generate();
    let p = present(
        &impostor,
        &set,
        sid,
        &f.verifier,
        nonce,
        &f.root,
        SCOPE,
        SubnetRights::ATTACH,
    );
    assert_eq!(
        f.verifier
            .admit_subnet_session(sub_id, &p, &set)
            .unwrap_err(),
        SubnetAuthError::WrongSubject
    );
    assert!(f.verifier.subnet_context_for(sub_id).is_none());
}

// ---------------------------------------------------------------------------
// Challenge binding
// ---------------------------------------------------------------------------

#[tokio::test]
async fn wrong_or_replayed_challenge_is_refused() {
    let f = fixture().await;
    let sub_id = f.subject.node_id();
    let set = grant_for(&f.root, &f.subject_kp, SCOPE, SubnetRights::ATTACH, DAY);
    let sid = session_id_of(&f.verifier, sub_id);

    // A nonce the verifier never issued.
    let p = present(
        &f.subject_kp,
        &set,
        sid,
        &f.verifier,
        [0x11; 32],
        &f.root,
        SCOPE,
        SubnetRights::ATTACH,
    );
    assert_eq!(
        f.verifier
            .admit_subnet_session(sub_id, &p, &set)
            .unwrap_err(),
        SubnetAuthError::WrongChallenge
    );

    // A real nonce works once, then is spent — the identical
    // presentation replayed is refused.
    let nonce = f
        .verifier
        .issue_subnet_challenge(sub_id)
        .expect("challenge");
    let p = present(
        &f.subject_kp,
        &set,
        sid,
        &f.verifier,
        nonce,
        &f.root,
        SCOPE,
        SubnetRights::ATTACH,
    );
    f.verifier
        .admit_subnet_session(sub_id, &p, &set)
        .expect("first use succeeds");
    assert_eq!(
        f.verifier
            .admit_subnet_session(sub_id, &p, &set)
            .unwrap_err(),
        SubnetAuthError::WrongChallenge,
        "a consumed challenge cannot be replayed",
    );
}

/// A failed attempt still spends its nonce: an attacker cannot use a
/// rejected probe to keep a live challenge for later.
#[tokio::test]
async fn a_rejected_attempt_still_consumes_the_challenge() {
    let f = fixture().await;
    let sub_id = f.subject.node_id();
    let set = grant_for(&f.root, &f.subject_kp, SCOPE, SubnetRights::ATTACH, DAY);
    let nonce = f
        .verifier
        .issue_subnet_challenge(sub_id)
        .expect("challenge");
    let sid = session_id_of(&f.verifier, sub_id);

    // Reject: requests EXPORT, which the grant does not carry.
    let over = present(
        &f.subject_kp,
        &set,
        sid,
        &f.verifier,
        nonce,
        &f.root,
        SCOPE,
        SubnetRights::EXPORT,
    );
    assert_eq!(
        f.verifier
            .admit_subnet_session(sub_id, &over, &set)
            .unwrap_err(),
        SubnetAuthError::RightNotGranted
    );
    // The same nonce is now gone even for a well-formed attempt.
    let ok = present(
        &f.subject_kp,
        &set,
        sid,
        &f.verifier,
        nonce,
        &f.root,
        SCOPE,
        SubnetRights::ATTACH,
    );
    assert_eq!(
        f.verifier
            .admit_subnet_session(sub_id, &ok, &set)
            .unwrap_err(),
        SubnetAuthError::WrongChallenge
    );
}

#[tokio::test]
async fn wrong_session_verifier_credentials_or_target_are_refused() {
    let f = fixture().await;
    let sub_id = f.subject.node_id();
    let set = grant_for(&f.root, &f.subject_kp, SCOPE, SubnetRights::ATTACH, DAY);
    let sid = session_id_of(&f.verifier, sub_id);

    // Wrong session id.
    let nonce = f
        .verifier
        .issue_subnet_challenge(sub_id)
        .expect("challenge");
    let p = present(
        &f.subject_kp,
        &set,
        sid ^ 0xFFFF,
        &f.verifier,
        nonce,
        &f.root,
        SCOPE,
        SubnetRights::ATTACH,
    );
    assert_eq!(
        f.verifier
            .admit_subnet_session(sub_id, &p, &set)
            .unwrap_err(),
        SubnetAuthError::WrongSession
    );

    // Wrong verifier identity.
    let nonce = f
        .verifier
        .issue_subnet_challenge(sub_id)
        .expect("challenge");
    let elsewhere = EntityKeypair::generate();
    let p = SubnetAuthPresentation::try_issue(
        &f.subject_kp,
        set.credential_set_hash(),
        sid,
        elsewhere.entity_id().clone(),
        nonce,
        SubnetRef {
            authority: f.root.entity_id().clone(),
            path: TopologySubnetId::new(SCOPE),
        },
        SubnetRights::ATTACH,
    )
    .expect("issue");
    assert_eq!(
        f.verifier
            .admit_subnet_session(sub_id, &p, &set)
            .unwrap_err(),
        SubnetAuthError::WrongVerifier
    );

    // Presentation bound to a DIFFERENT credential set than the one
    // supplied alongside it.
    let other_set = grant_for(&f.root, &f.subject_kp, &[3, 8], SubnetRights::ATTACH, DAY);
    let nonce = f
        .verifier
        .issue_subnet_challenge(sub_id)
        .expect("challenge");
    let p = present(
        &f.subject_kp,
        &other_set,
        sid,
        &f.verifier,
        nonce,
        &f.root,
        SCOPE,
        SubnetRights::ATTACH,
    );
    assert_eq!(
        f.verifier
            .admit_subnet_session(sub_id, &p, &set)
            .unwrap_err(),
        SubnetAuthError::WrongChallenge
    );

    // Target outside the granted scope.
    let nonce = f
        .verifier
        .issue_subnet_challenge(sub_id)
        .expect("challenge");
    let p = present(
        &f.subject_kp,
        &set,
        sid,
        &f.verifier,
        nonce,
        &f.root,
        &[4],
        SubnetRights::ATTACH,
    );
    assert_eq!(
        f.verifier
            .admit_subnet_session(sub_id, &p, &set)
            .unwrap_err(),
        SubnetAuthError::ScopeNotAncestor
    );
}

// ---------------------------------------------------------------------------
// Authority anchoring
// ---------------------------------------------------------------------------

/// A node that anchors no authority fails closed, and a grant under
/// an unanchored authority is refused even with a perfect proof.
#[tokio::test]
async fn unanchored_authority_fails_closed() {
    let root = authority_root();
    let verifier = Arc::new(
        MeshNode::new(EntityKeypair::generate(), base_config())
            .await
            .expect("verifier"),
    );
    let subject_kp = EntityKeypair::generate();
    let subject = Arc::new(
        MeshNode::new(subject_kp.clone(), base_config())
            .await
            .expect("subject"),
    );
    handshake(&subject, &verifier).await;

    let sub_id = subject.node_id();
    let set = grant_for(&root, &subject_kp, SCOPE, SubnetRights::ATTACH, DAY);
    let nonce = verifier.issue_subnet_challenge(sub_id).expect("challenge");
    let sid = session_id_of(&verifier, sub_id);
    let p = present(
        &subject_kp,
        &set,
        sid,
        &verifier,
        nonce,
        &root,
        SCOPE,
        SubnetRights::ATTACH,
    );
    assert_eq!(
        verifier.admit_subnet_session(sub_id, &p, &set).unwrap_err(),
        SubnetAuthError::UnknownAuthority
    );
}

// ---------------------------------------------------------------------------
// Invalidation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn expiry_epoch_moves_and_withdrawal_invalidate_the_context() {
    let f = fixture().await;
    let sub_id = f.subject.node_id();
    let set = grant_for(&f.root, &f.subject_kp, SCOPE, SubnetRights::ATTACH, DAY);
    let admit = |set: &SubnetCredentialSet| {
        let nonce = f
            .verifier
            .issue_subnet_challenge(sub_id)
            .expect("challenge");
        let sid = session_id_of(&f.verifier, sub_id);
        let p = present(
            &f.subject_kp,
            set,
            sid,
            &f.verifier,
            nonce,
            &f.root,
            SCOPE,
            SubnetRights::ATTACH,
        );
        f.verifier.admit_subnet_session(sub_id, &p, set)
    };

    // Explicit withdrawal.
    admit(&set).expect("admit");
    assert!(f.verifier.subnet_context_for(sub_id).is_some());
    f.verifier.withdraw_subnet_admission(sub_id);
    assert!(f.verifier.subnet_context_for(sub_id).is_none());

    // Topology epoch movement drops contexts from the prior epoch,
    // and a grant minted under the old epoch no longer verifies.
    admit(&set).expect("re-admit");
    assert!(f.verifier.subnet_context_for(sub_id).is_some());
    f.verifier.advance_subnet_topology_epoch();
    assert!(
        f.verifier.subnet_context_for(sub_id).is_none(),
        "reparenting invalidates old ancestry authority",
    );
    assert_eq!(
        admit(&set).unwrap_err(),
        SubnetAuthError::WrongTopologyEpoch
    );
}

/// An accepted revocation floor advances the authority auth epoch and
/// drops every context pinned to the older one — the broad, infrequent
/// invalidation that keeps revocation off the packet path.
#[tokio::test]
async fn accepted_floor_invalidates_stale_contexts() {
    let f = fixture().await;
    let sub_id = f.subject.node_id();
    let set = grant_for(&f.root, &f.subject_kp, SCOPE, SubnetRights::ATTACH, DAY);
    let nonce = f
        .verifier
        .issue_subnet_challenge(sub_id)
        .expect("challenge");
    let sid = session_id_of(&f.verifier, sub_id);
    let p = present(
        &f.subject_kp,
        &set,
        sid,
        &f.verifier,
        nonce,
        &f.root,
        SCOPE,
        SubnetRights::ATTACH,
    );
    let ctx = f
        .verifier
        .admit_subnet_session(sub_id, &p, &set)
        .expect("admit");
    assert_eq!(ctx.subnet_auth_epoch, 0);
    assert!(f.verifier.subnet_context_for(sub_id).is_some());

    let floor = SubnetRevocationFloor::try_issue(
        &f.root,
        SubnetRef {
            authority: f.root.entity_id().clone(),
            path: TopologySubnetId::new(SCOPE),
        },
        0,
        5,
        1,
        unix_now_secs(),
    )
    .expect("issue floor");
    assert!(f.verifier.apply_subnet_floor(&floor).expect("apply floor"));
    assert!(
        f.verifier.subnet_context_for(sub_id).is_none(),
        "the epoch move must invalidate the compiled context",
    );

    // And the revoked generation cannot re-admit.
    let nonce = f
        .verifier
        .issue_subnet_challenge(sub_id)
        .expect("challenge");
    let sid = session_id_of(&f.verifier, sub_id);
    let p = present(
        &f.subject_kp,
        &set,
        sid,
        &f.verifier,
        nonce,
        &f.root,
        SCOPE,
        SubnetRights::ATTACH,
    );
    assert_eq!(
        f.verifier
            .admit_subnet_session(sub_id, &p, &set)
            .unwrap_err(),
        SubnetAuthError::Revoked
    );
}

/// An expired grant produces no context, and the context read is
/// itself expiry-aware.
#[tokio::test]
async fn expired_grant_is_refused() {
    let f = fixture().await;
    let sub_id = f.subject.node_id();
    // not_before 60s ago, 30s long ⇒ already past not_after.
    let expired = SubnetCredentialSet::Direct(
        SubnetGrant::try_issue(
            &f.root,
            f.root.entity_id().clone(),
            TopologySubnetId::new(SCOPE),
            0,
            f.subject_kp.entity_id().clone(),
            SubnetRights::ATTACH,
            1,
            unix_now_secs() - 600,
            30,
        )
        .expect("issue"),
    );
    let nonce = f
        .verifier
        .issue_subnet_challenge(sub_id)
        .expect("challenge");
    let sid = session_id_of(&f.verifier, sub_id);
    let p = present(
        &f.subject_kp,
        &expired,
        sid,
        &f.verifier,
        nonce,
        &f.root,
        SCOPE,
        SubnetRights::ATTACH,
    );
    assert_eq!(
        f.verifier
            .admit_subnet_session(sub_id, &p, &expired)
            .unwrap_err(),
        SubnetAuthError::Expired
    );
}

// ---------------------------------------------------------------------------
// A peer context is not forwarding authority
// ---------------------------------------------------------------------------

/// A peer that proves ROUTE for itself grants this node nothing. The
/// verifier's own forwarding authority comes from credentials naming
/// the verifier as subject (`install_subnet_gateway_credentials`), and
/// admitting a peer never publishes one. `subnet_gateway_local_auth.rs`
/// pins the decision rule itself; this pins the separation.
#[tokio::test]
async fn admitting_a_peer_confers_no_gateway_authority() {
    let f = fixture().await;
    let sub_id = f.subject.node_id();
    assert!(
        f.verifier.subnet_gateway_contexts().is_none(),
        "precondition: the verifier holds no gateway authority",
    );

    // The peer proves ATTACH *and* ROUTE for itself.
    let rights = SubnetRights::ATTACH.union(SubnetRights::ROUTE);
    let set = grant_for(&f.root, &f.subject_kp, SCOPE, rights, DAY);
    let nonce = f
        .verifier
        .issue_subnet_challenge(sub_id)
        .expect("challenge");
    let sid = session_id_of(&f.verifier, sub_id);
    let p = present(
        &f.subject_kp,
        &set,
        sid,
        &f.verifier,
        nonce,
        &f.root,
        SCOPE,
        rights,
    );
    let ctx = f
        .verifier
        .admit_subnet_session(sub_id, &p, &set)
        .expect("admit");
    assert!(ctx.rights.contains(SubnetRights::ROUTE));

    assert!(
        f.verifier.subnet_gateway_contexts().is_none(),
        "a peer's proven ROUTE must never become the verifier's forwarding authority",
    );

    // And the peer's own credentials cannot be installed as this
    // node's gateway authority: the subject is not this process.
    assert_eq!(
        f.verifier
            .install_subnet_gateway_credentials(&[set])
            .unwrap_err(),
        SubnetAuthError::WrongSubject,
    );
}
