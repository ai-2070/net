//! Selective subnet removal: the subject floor (NET_CLI_PLAN_V3 §6.1a).
//!
//! The decisive witness: B loses access with its old credentials — on
//! its live session and on a real reconnect — while C stays admitted in
//! the same subnet on its SAME context (no re-admission churn), and the
//! removal survives a verifier restart. The remaining witnesses pin the
//! pinned semantics: ancestor-scoped and delegated credentials cannot
//! carry B back in, rights are exact, generations never lower, and only
//! root-direct issuance at or above the floor re-admits.

#![cfg(feature = "net")]

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::identity::EntityKeypair;
use net::adapter::net::subnet::{
    admission::unix_now_secs, SubnetAuthError, SubnetAuthPresentation, SubnetAuthorityConfig,
    SubnetControlFact, SubnetCredentialSet, SubnetGrant, SubnetIssuerGrant, SubnetRef,
    SubnetRights, SubnetSubjectFloor, TopologySubnetId,
};
use net::adapter::net::{MeshNode, MeshNodeConfig, SocketBufferConfig};
use net::adapter::Adapter;

const PSK: [u8; 32] = [0x5Eu8; 32];
const DAY: u64 = 24 * 60 * 60;
/// The floored subtree S.
const S: &[u8] = &[3, 7];

fn root() -> EntityKeypair {
    EntityKeypair::from_bytes([0xB2; 32])
}

fn verifier_kp() -> EntityKeypair {
    EntityKeypair::from_bytes([0xB3; 32])
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

fn verifier_config(store: Option<&Path>) -> MeshNodeConfig {
    let root = root();
    let cfg = base_config().with_subnet_authority(SubnetAuthorityConfig {
        authority: root.entity_id().clone(),
        roots: vec![root.entity_id().clone()],
        maximum_grant_lifetime_secs: 7 * DAY,
    });
    match store {
        Some(dir) => cfg.with_subnet_floor_store(dir),
        None => cfg,
    }
}

async fn verifier(store: Option<&Path>) -> Arc<MeshNode> {
    // The in-process "restart" reopens the store the previous verifier
    // owned; its background tasks may hold the owner lock a moment
    // after shutdown. A real restart is a new process.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        match MeshNode::new(verifier_kp(), verifier_config(store)).await {
            Ok(node) => return Arc::new(node),
            Err(e) if std::time::Instant::now() < deadline && e.to_string().contains("Busy") => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(e) => panic!("verifier: {e}"),
        }
    }
}

async fn node(kp: &EntityKeypair) -> Arc<MeshNode> {
    Arc::new(
        MeshNode::new(kp.clone(), base_config())
            .await
            .expect("node"),
    )
}

/// Connect `a` to `b` by the routed handshake, which needs no pre-`accept`
/// on the responder — so an already-started verifier can take new and
/// reconnecting peers.
async fn handshake(a: &Arc<MeshNode>, b: &Arc<MeshNode>) {
    a.start();
    b.start();
    a.connect_via(b.local_addr(), b.public_key(), b.node_id())
        .await
        .expect("connect");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while b.peer_session_id(a.node_id()).is_none() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "responder never installed the session"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// A real reconnect: `a` (a fresh node for a returning identity, or a
/// peer of a restarted responder) re-handshakes with `b` until `b` holds
/// a NEW session for it. The returning identity is deferred while `b`
/// still holds its previous session, so the attempt is retried.
async fn reconnect(a: &Arc<MeshNode>, b: &Arc<MeshNode>) {
    let old = b.peer_session_id(a.node_id());
    a.start();
    b.start();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let _ = a
            .connect_via(b.local_addr(), b.public_key(), b.node_id())
            .await;
        let now = b.peer_session_id(a.node_id());
        if now.is_some() && now != old {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "reconnect never produced a new session"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

fn scope(path: &[u8]) -> SubnetRef {
    SubnetRef {
        authority: root().entity_id().clone(),
        path: TopologySubnetId::new(path),
    }
}

fn direct(
    subject: &EntityKeypair,
    at: &[u8],
    rights: SubnetRights,
    generation: u32,
) -> SubnetCredentialSet {
    let root = root();
    SubnetCredentialSet::Direct(
        SubnetGrant::try_issue(
            &root,
            root.entity_id().clone(),
            TopologySubnetId::new(at),
            0,
            subject.entity_id().clone(),
            rights,
            generation,
            unix_now_secs() - 60,
            DAY,
        )
        .expect("grant"),
    )
}

/// A leaf for `subject` signed by a root-authorized delegated issuer.
fn delegated(
    subject: &EntityKeypair,
    issuer: &EntityKeypair,
    generation: u32,
) -> SubnetCredentialSet {
    let root = root();
    let issuer_grant = SubnetIssuerGrant::try_issue(
        &root,
        root.entity_id().clone(),
        TopologySubnetId::new(&[3]),
        0,
        issuer.entity_id().clone(),
        SubnetRights::ALL,
        1,
        unix_now_secs() - 120,
        2 * DAY,
    )
    .expect("issuer grant");
    let leaf = SubnetGrant::try_issue(
        issuer,
        root.entity_id().clone(),
        TopologySubnetId::new(S),
        0,
        subject.entity_id().clone(),
        SubnetRights::ATTACH,
        generation,
        unix_now_secs() - 60,
        DAY,
    )
    .expect("leaf");
    SubnetCredentialSet::OneHop { issuer_grant, leaf }
}

fn subject_floor(
    subject: &EntityKeypair,
    rights: SubnetRights,
    generation: u32,
    revision: u64,
) -> SubnetSubjectFloor {
    SubnetSubjectFloor::try_issue(
        &root(),
        scope(S),
        0,
        subject.entity_id().clone(),
        rights,
        generation,
        revision,
        unix_now_secs(),
    )
    .expect("subject floor")
}

/// Challenge, sign and admit `subject` at `target` for `rights`.
fn admit(
    verifier: &Arc<MeshNode>,
    subject: &EntityKeypair,
    set: &SubnetCredentialSet,
    target: &[u8],
    rights: SubnetRights,
) -> Result<u64, SubnetAuthError> {
    let id = subject.node_id();
    let nonce = verifier.issue_subnet_challenge(id).expect("challenge");
    let session_id = verifier.peer_session_id(id).expect("session");
    let presentation = SubnetAuthPresentation::try_issue(
        subject,
        set.credential_set_hash(),
        session_id,
        verifier.entity_id().clone(),
        nonce,
        scope(target),
        rights,
    )
    .expect("presentation");
    verifier
        .admit_subnet_session(id, &presentation, set)
        .map(|ctx| ctx.session_id)
}

/// The decisive witness (§6.1a).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn b_is_removed_c_is_untouched_across_reconnect_and_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let store = tmp.path().join("subnet-floors");
    let v = verifier(Some(&store)).await;
    let (b_kp, c_kp) = (EntityKeypair::generate(), EntityKeypair::generate());
    let (b, c) = (node(&b_kp).await, node(&c_kp).await);
    handshake(&b, &v).await;
    handshake(&c, &v).await;
    let b_set = direct(&b_kp, S, SubnetRights::ATTACH, 1);
    let c_set = direct(&c_kp, S, SubnetRights::ATTACH, 1);
    admit(&v, &b_kp, &b_set, S, SubnetRights::ATTACH).expect("B admitted");
    admit(&v, &c_kp, &c_set, S, SubnetRights::ATTACH).expect("C admitted");
    let c_before = v.subnet_context_for(c_kp.node_id()).expect("C context");
    let authority = root().entity_id().clone();
    let epoch_before = v.subnet_floor_registry().auth_epoch(&authority);

    // Remove B (ATTACH) inside S.
    assert!(v
        .apply_subnet_subject_floor(&subject_floor(&b_kp, SubnetRights::ATTACH, 2, 1))
        .expect("apply"));

    // B's live context is gone; C keeps the SAME context — no epoch
    // move, no re-admission.
    assert!(
        v.subnet_context_for(b_kp.node_id()).is_none(),
        "B's context must drop"
    );
    let c_after = v
        .subnet_context_for(c_kp.node_id())
        .expect("C keeps its context");
    assert_eq!(
        c_after, c_before,
        "C's context is untouched, not re-admitted"
    );
    assert_eq!(
        v.subnet_floor_registry().auth_epoch(&authority),
        epoch_before,
        "a subject floor must not advance the authority-wide epoch"
    );

    // B's old credentials fail on its live session...
    assert_eq!(
        admit(&v, &b_kp, &b_set, S, SubnetRights::ATTACH),
        Err(SubnetAuthError::Revoked)
    );
    // ...and on a real reconnect: a fresh node, same identity, new session.
    b.shutdown().await.expect("B shutdown");
    drop(b);
    let b = node(&b_kp).await;
    reconnect(&b, &v).await;
    assert_eq!(
        admit(&v, &b_kp, &b_set, S, SubnetRights::ATTACH),
        Err(SubnetAuthError::Revoked),
        "reconnecting with the old credentials must not restore access"
    );

    // Restart the verifier on the same store: the removal survives.
    let v_id = v.node_id();
    v.shutdown().await.expect("verifier shutdown");
    drop(v);
    let v = verifier(Some(&store)).await;
    assert_eq!(v.node_id(), v_id, "same verifier identity after restart");
    reconnect(&b, &v).await;
    reconnect(&c, &v).await;
    assert_eq!(
        admit(&v, &b_kp, &b_set, S, SubnetRights::ATTACH),
        Err(SubnetAuthError::Revoked),
        "a restarted verifier must not re-admit B"
    );
    admit(&v, &c_kp, &c_set, S, SubnetRights::ATTACH).expect("C still admitted after restart");

    // Retrying with a delegated leaf — however high its generation —
    // does not re-admit; only root-direct issuance at or above the
    // floor does.
    let issuer = EntityKeypair::generate();
    assert_eq!(
        admit(
            &v,
            &b_kp,
            &delegated(&b_kp, &issuer, 99),
            S,
            SubnetRights::ATTACH
        ),
        Err(SubnetAuthError::Revoked)
    );
    admit(
        &v,
        &b_kp,
        &direct(&b_kp, S, SubnetRights::ATTACH, 2),
        S,
        SubnetRights::ATTACH,
    )
    .expect("root-direct issuance at the floor re-admits B");
}

/// A grant scoped at an ancestor of S cannot carry B back into S; B's
/// authority outside S is untouched.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_ancestor_scoped_grant_cannot_defeat_the_removal() {
    let v = verifier(None).await;
    let b_kp = EntityKeypair::generate();
    let b = node(&b_kp).await;
    handshake(&b, &v).await;
    let wide = direct(&b_kp, &[3], SubnetRights::ATTACH, 1);
    v.apply_subnet_subject_floor(&subject_floor(&b_kp, SubnetRights::ATTACH, 2, 1))
        .expect("apply");
    assert_eq!(
        admit(&v, &b_kp, &wide, S, SubnetRights::ATTACH),
        Err(SubnetAuthError::Revoked),
        "inside S via the parent grant"
    );
    assert_eq!(
        admit(&v, &b_kp, &wide, &[3, 7, 4], SubnetRights::ATTACH),
        Err(SubnetAuthError::Revoked),
        "below S via the parent grant"
    );
    admit(&v, &b_kp, &wide, &[3, 8], SubnetRights::ATTACH)
        .expect("outside S the parent grant still works");
}

/// Rights are exact (an ATTACH floor does not remove ROUTE), stale or
/// reordered facts are no-ops, and no later fact lowers a generation.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rights_are_exact_and_generations_never_lower() {
    let v = verifier(None).await;
    let b_kp = EntityKeypair::generate();
    let b = node(&b_kp).await;
    handshake(&b, &v).await;
    let both = direct(&b_kp, S, SubnetRights::ATTACH.union(SubnetRights::ROUTE), 2);
    assert!(v
        .apply_subnet_subject_floor(&subject_floor(&b_kp, SubnetRights::ATTACH, 3, 5))
        .unwrap());
    assert_eq!(
        admit(&v, &b_kp, &both, S, SubnetRights::ATTACH),
        Err(SubnetAuthError::Revoked)
    );
    admit(&v, &b_kp, &both, S, SubnetRights::ROUTE)
        .expect("ROUTE was not named, so it is not removed");

    // Replay / reorder: an older revision is a no-op even if it asks more.
    assert!(!v
        .apply_subnet_subject_floor(&subject_floor(&b_kp, SubnetRights::ALL, 9, 4))
        .unwrap());
    admit(&v, &b_kp, &both, S, SubnetRights::ROUTE).expect("the stale fact changed nothing");
    // A newer revision with a LOWER generation cannot lower ATTACH's floor.
    assert!(!v
        .apply_subnet_subject_floor(&subject_floor(&b_kp, SubnetRights::ATTACH, 1, 6))
        .unwrap());
    assert_eq!(
        admit(&v, &b_kp, &both, S, SubnetRights::ATTACH),
        Err(SubnetAuthError::Revoked)
    );
}

/// Trust and wire strictness: a non-root signer is refused, the fact
/// travels as kind 5, and an unknown kind fails closed rather than being
/// mistaken for another fact.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn only_a_root_signs_and_the_wire_kind_is_strict() {
    let v = verifier(None).await;
    let b_kp = EntityKeypair::generate();
    let forged = SubnetSubjectFloor::try_issue(
        &EntityKeypair::generate(),
        scope(S),
        0,
        b_kp.entity_id().clone(),
        SubnetRights::ATTACH,
        5,
        1,
        unix_now_secs(),
    )
    .unwrap();
    assert_eq!(
        v.apply_subnet_subject_floor(&forged),
        Err(SubnetAuthError::IssuerNotAuthorized)
    );

    let bytes = SubnetControlFact::SubjectFloor(subject_floor(&b_kp, SubnetRights::ATTACH, 2, 1))
        .to_bytes();
    assert_eq!(bytes[0], 5, "subject floors travel as control-fact kind 5");
    let outcome = v
        .apply_subnet_control_fact(&bytes)
        .expect("apply via bytes");
    assert!(outcome.applied);
    let mut unknown = bytes.clone();
    unknown[0] = 6;
    assert_eq!(
        v.apply_subnet_control_fact(&unknown),
        Err(SubnetAuthError::InvalidFormat),
        "an unknown kind fails closed"
    );
}

/// Readback (V3-4 slice 2): each named verifier answers for itself with a
/// signature over the exact request, so the caller can report applied,
/// applied-but-not-persisted, refused, and no-answer separately — and an
/// owner cannot be told "applied" by anyone but the verifier.
#[cfg(feature = "cortex")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn readback_reports_each_verifier_separately_and_cannot_be_forged() {
    use net::adapter::net::subnet::floor_status::{FloorApplyOutcome, FloorStatusRequest};
    use net::adapter::net::SubnetFloorQueryError;

    let tmp = tempfile::tempdir().unwrap();
    let operator = node(&EntityKeypair::generate()).await;
    // Durable verifier, volatile verifier, an older node with no readback
    // service, and a verifier that does not recognise this authority.
    let durable = Arc::new(
        MeshNode::new(
            EntityKeypair::generate(),
            verifier_config(Some(&tmp.path().join("floors"))),
        )
        .await
        .unwrap(),
    );
    let volatile = Arc::new(
        MeshNode::new(EntityKeypair::generate(), verifier_config(None))
            .await
            .unwrap(),
    );
    let older = Arc::new(
        MeshNode::new(EntityKeypair::generate(), verifier_config(None))
            .await
            .unwrap(),
    );
    let stranger = node(&EntityKeypair::generate()).await;
    let _serving = [
        durable.serve_subnet_floor_status().unwrap(),
        volatile.serve_subnet_floor_status().unwrap(),
        stranger.serve_subnet_floor_status().unwrap(),
    ];
    for v in [&durable, &volatile, &older, &stranger] {
        handshake(&operator, v).await;
    }

    let b = EntityKeypair::generate();
    let floor = subject_floor(&b, SubnetRights::ATTACH, 2, 1);
    let request_for = |v: &Arc<MeshNode>, apply: bool, signer: &EntityKeypair| {
        let mut nonce = [0u8; 16];
        getrandom::fill(&mut nonce).unwrap();
        FloorStatusRequest::try_issue(
            signer,
            scope(S),
            0,
            b.entity_id().clone(),
            v.entity_id().clone(),
            nonce,
            unix_now_secs(),
            apply.then_some(&floor),
        )
        .unwrap()
    };
    let t = Duration::from_secs(3);

    let a = operator
        .query_subnet_floor_status(durable.node_id(), &request_for(&durable, true, &root()), t)
        .await
        .expect("durable verifier attests");
    assert_eq!(a.apply, FloorApplyOutcome::Applied);
    assert!(a.persisted && a.covers(SubnetRights::ATTACH, 2));
    assert_eq!(a.verifier, *durable.entity_id());

    let a = operator
        .query_subnet_floor_status(
            volatile.node_id(),
            &request_for(&volatile, true, &root()),
            t,
        )
        .await
        .expect("volatile verifier attests");
    assert_eq!(a.apply, FloorApplyOutcome::Applied);
    assert!(!a.persisted, "no floor store: applied, but not durable");

    let err = operator
        .query_subnet_floor_status(older.node_id(), &request_for(&older, true, &root()), t)
        .await
        .unwrap_err();
    assert!(
        matches!(err, SubnetFloorQueryError::NoAnswer(_)),
        "a verifier without readback yields no attestation: {err:?}"
    );

    let err = operator
        .query_subnet_floor_status(
            stranger.node_id(),
            &request_for(&stranger, true, &root()),
            t,
        )
        .await
        .unwrap_err();
    assert_eq!(
        err,
        SubnetFloorQueryError::Refused("subnet:unknown_authority".into())
    );

    // A non-root cannot drive or read a verifier.
    let err = operator
        .query_subnet_floor_status(
            durable.node_id(),
            &request_for(&durable, false, &EntityKeypair::generate()),
            t,
        )
        .await
        .unwrap_err();
    assert_eq!(
        err,
        SubnetFloorQueryError::Refused("subnet:issuer_not_authorized".into())
    );

    // Status-only readback reflects the held floor; re-applying is a no-op.
    let a = operator
        .query_subnet_floor_status(durable.node_id(), &request_for(&durable, false, &root()), t)
        .await
        .unwrap();
    assert_eq!(a.apply, FloorApplyOutcome::NotRequested);
    assert_eq!((a.revision, a.generations), (1, [2, 0, 0]));
    let a = operator
        .query_subnet_floor_status(durable.node_id(), &request_for(&durable, true, &root()), t)
        .await
        .unwrap();
    assert_eq!(a.apply, FloorApplyOutcome::Unchanged);

    // A stale request is refused: the answer must be fresh.
    let stale = FloorStatusRequest::try_issue(
        &root(),
        scope(S),
        0,
        b.entity_id().clone(),
        durable.entity_id().clone(),
        [7; 16],
        unix_now_secs() - 10_000,
        None,
    )
    .unwrap();
    let err = operator
        .query_subnet_floor_status(durable.node_id(), &stale, t)
        .await
        .unwrap_err();
    assert_eq!(err, SubnetFloorQueryError::Refused("subnet:expired".into()));

    // A request addressed to one verifier is refused by another, so an
    // attestation can never be obtained under someone else's name.
    let err = operator
        .query_subnet_floor_status(
            volatile.node_id(),
            &request_for(&durable, false, &root()),
            t,
        )
        .await
        .unwrap_err();
    assert_eq!(
        err,
        SubnetFloorQueryError::Refused("subnet:wrong_verifier".into())
    );
}
