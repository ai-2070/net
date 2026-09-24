//! Subnet admission over the wire (NET_CLI_PLAN_V3 V3-2 S1): subprotocol
//! `0x0A02`. A device presents its credentials to a verifier across the
//! session — request a challenge, sign the session/verifier/challenge-bound
//! presentation, get a verdict — and the verifier runs the unchanged
//! `admit_subnet_session`, so floors (including subject floors) and the
//! routing-id pin apply exactly as for local admission.

#![cfg(feature = "net")]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::identity::EntityKeypair;
use net::adapter::net::subnet::admission_wire::SubnetAdmissionError;
use net::adapter::net::subnet::{
    admission::unix_now_secs, SubnetAuthError, SubnetAuthorityConfig, SubnetCredentialSet,
    SubnetGrant, SubnetIssuerGrant, SubnetRef, SubnetRights, SubnetSubjectFloor, TopologySubnetId,
};
use net::adapter::net::{MeshNode, MeshNodeConfig, SocketBufferConfig};

const PSK: [u8; 32] = [0x2A; 32];
const DAY: u64 = 24 * 60 * 60;
const S: &[u8] = &[4, 2];
const T: Duration = Duration::from_secs(6);

fn root() -> EntityKeypair {
    EntityKeypair::from_bytes([0xC1; 32])
}

fn config(verifier: bool) -> MeshNodeConfig {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let mut cfg = MeshNodeConfig::new(addr, PSK)
        .with_heartbeat_interval(Duration::from_millis(200))
        .with_session_timeout(Duration::from_secs(5))
        .with_handshake(3, Duration::from_secs(2));
    cfg.socket_buffers = SocketBufferConfig {
        send_buffer_size: 256 * 1024,
        recv_buffer_size: 256 * 1024,
    };
    if verifier {
        let root = root();
        cfg = cfg.with_subnet_authority(SubnetAuthorityConfig {
            authority: root.entity_id().clone(),
            roots: vec![root.entity_id().clone()],
            maximum_grant_lifetime_secs: 7 * DAY,
        });
    }
    cfg
}

async fn node(kp: &EntityKeypair, verifier: bool) -> Arc<MeshNode> {
    let node = Arc::new(MeshNode::new(kp.clone(), config(verifier)).await.unwrap());
    node.start();
    node
}

async fn connect(device: &Arc<MeshNode>, verifier: &Arc<MeshNode>) {
    device
        .connect_via(
            verifier.local_addr(),
            verifier.public_key(),
            verifier.node_id(),
        )
        .await
        .expect("connect");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while verifier.peer_session_id(device.node_id()).is_none() {
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn scope() -> SubnetRef {
    SubnetRef {
        authority: root().entity_id().clone(),
        path: TopologySubnetId::new(S),
    }
}

fn direct(subject: &EntityKeypair, generation: u32) -> SubnetCredentialSet {
    let root = root();
    SubnetCredentialSet::Direct(
        SubnetGrant::try_issue(
            &root,
            root.entity_id().clone(),
            TopologySubnetId::new(S),
            0,
            subject.entity_id().clone(),
            SubnetRights::ATTACH,
            generation,
            unix_now_secs() - 60,
            DAY,
        )
        .unwrap(),
    )
}

/// A device is admitted over the wire; after a subject floor it is
/// refused on re-presentation while a sibling keeps its context.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_device_is_admitted_over_the_wire_and_refused_after_removal() {
    let v = node(&EntityKeypair::generate(), true).await;
    let (b_kp, c_kp) = (EntityKeypair::generate(), EntityKeypair::generate());
    let (b, c) = (node(&b_kp, false).await, node(&c_kp, false).await);
    connect(&b, &v).await;
    connect(&c, &v).await;

    b.present_subnet_credentials(
        v.node_id(),
        &direct(&b_kp, 1),
        scope(),
        SubnetRights::ATTACH,
        T,
    )
    .await
    .expect("B admitted over the wire");
    c.present_subnet_credentials(
        v.node_id(),
        &direct(&c_kp, 1),
        scope(),
        SubnetRights::ATTACH,
        T,
    )
    .await
    .expect("C admitted over the wire");
    let ctx = v
        .subnet_context_for(b_kp.node_id())
        .expect("B's context installed");
    assert_eq!(ctx.subject, *b_kp.entity_id());
    let c_ctx = v.subnet_context_for(c_kp.node_id()).expect("C's context");

    let floor = SubnetSubjectFloor::try_issue(
        &root(),
        scope(),
        0,
        b_kp.entity_id().clone(),
        SubnetRights::ATTACH,
        2,
        1,
        unix_now_secs(),
    )
    .unwrap();
    assert!(v.apply_subnet_subject_floor(&floor).unwrap());
    assert_eq!(
        b.present_subnet_credentials(
            v.node_id(),
            &direct(&b_kp, 1),
            scope(),
            SubnetRights::ATTACH,
            T
        )
        .await,
        Err(SubnetAdmissionError::Refused(SubnetAuthError::Revoked))
    );
    assert!(v.subnet_context_for(b_kp.node_id()).is_none());
    assert_eq!(v.subnet_context_for(c_kp.node_id()), Some(c_ctx));
}

/// Delegated (one-hop) credentials are admitted over the wire too.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn delegated_credentials_are_admitted_over_the_wire() {
    let v = node(&EntityKeypair::generate(), true).await;
    let d_kp = EntityKeypair::generate();
    let d = node(&d_kp, false).await;
    connect(&d, &v).await;
    let root = root();
    let issuer = EntityKeypair::generate();
    let issuer_grant = SubnetIssuerGrant::try_issue(
        &root,
        root.entity_id().clone(),
        TopologySubnetId::new(&[4]),
        0,
        issuer.entity_id().clone(),
        SubnetRights::ATTACH,
        1,
        unix_now_secs() - 120,
        2 * DAY,
    )
    .unwrap();
    let leaf = SubnetGrant::try_issue(
        &issuer,
        root.entity_id().clone(),
        TopologySubnetId::new(S),
        0,
        d_kp.entity_id().clone(),
        SubnetRights::ATTACH,
        1,
        unix_now_secs() - 60,
        DAY,
    )
    .unwrap();
    d.present_subnet_credentials(
        v.node_id(),
        &SubnetCredentialSet::OneHop { issuer_grant, leaf },
        scope(),
        SubnetRights::ATTACH,
        T,
    )
    .await
    .expect("delegated credentials admitted");
    assert!(v.subnet_context_for(d_kp.node_id()).is_some());
}

/// Refusals come back as verdicts, not silence: an unanchored verifier,
/// someone else's credentials, and no session at all.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn refusals_are_verdicts_and_nothing_is_installed() {
    let plain = node(&EntityKeypair::generate(), false).await;
    let v = node(&EntityKeypair::generate(), true).await;
    let d_kp = EntityKeypair::generate();
    let d = node(&d_kp, false).await;
    connect(&d, &plain).await;
    connect(&d, &v).await;

    assert_eq!(
        d.present_subnet_credentials(
            plain.node_id(),
            &direct(&d_kp, 1),
            scope(),
            SubnetRights::ATTACH,
            T
        )
        .await,
        Err(SubnetAdmissionError::Refused(
            SubnetAuthError::UnknownAuthority
        ))
    );

    // Another entity's grant: the presentation is D's, the leaf is not.
    let someone_else = EntityKeypair::generate();
    assert_eq!(
        d.present_subnet_credentials(
            v.node_id(),
            &direct(&someone_else, 1),
            scope(),
            SubnetRights::ATTACH,
            T
        )
        .await,
        Err(SubnetAdmissionError::Refused(SubnetAuthError::WrongSubject))
    );
    assert!(v.subnet_context_for(d_kp.node_id()).is_none());

    let stranger = node(&EntityKeypair::generate(), true).await;
    assert_eq!(
        d.present_subnet_credentials(
            stranger.node_id(),
            &direct(&d_kp, 1),
            scope(),
            SubnetRights::ATTACH,
            T
        )
        .await,
        Err(SubnetAdmissionError::NoSession)
    );
}

/// V3-2B subnet leave: a device withdraws its OWN admission at exactly the
/// attachment it names. Another scope or authority drops nothing, a
/// sibling's admission is untouched, and repeating is acknowledged with
/// nothing held.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_device_withdraws_only_its_own_named_admission() {
    let v = node(&EntityKeypair::generate(), true).await;
    let (b_kp, c_kp) = (EntityKeypair::generate(), EntityKeypair::generate());
    let (b, c) = (node(&b_kp, false).await, node(&c_kp, false).await);
    connect(&b, &v).await;
    connect(&c, &v).await;
    for (n, kp) in [(&b, &b_kp), (&c, &c_kp)] {
        n.present_subnet_credentials(
            v.node_id(),
            &direct(kp, 1),
            scope(),
            SubnetRights::ATTACH,
            T,
        )
        .await
        .expect("admitted");
    }
    let c_ctx = v.subnet_context_for(c_kp.node_id()).expect("C's context");

    let elsewhere = SubnetRef {
        authority: root().entity_id().clone(),
        path: TopologySubnetId::new(&[4, 3]),
    };
    let other_authority = SubnetRef {
        authority: EntityKeypair::generate().entity_id().clone(),
        path: TopologySubnetId::new(S),
    };
    for target in [&elsewhere, &other_authority] {
        assert_eq!(
            b.withdraw_own_subnet_admission(v.node_id(), target, T)
                .await,
            Ok(false)
        );
        assert!(v.subnet_context_for(b_kp.node_id()).is_some());
    }
    assert_eq!(
        b.withdraw_own_subnet_admission(v.node_id(), &scope(), T)
            .await,
        Ok(true)
    );
    assert!(v.subnet_context_for(b_kp.node_id()).is_none());
    assert_eq!(v.subnet_context_for(c_kp.node_id()), Some(c_ctx));
    assert_eq!(
        b.withdraw_own_subnet_admission(v.node_id(), &scope(), T)
            .await,
        Ok(false),
        "repeated: acknowledged, nothing held"
    );
    // No session: nothing is claimed.
    let lone = node(&EntityKeypair::generate(), false).await;
    assert_eq!(
        lone.withdraw_own_subnet_admission(v.node_id(), &scope(), T)
            .await,
        Err(SubnetAdmissionError::NoSession)
    );
}
