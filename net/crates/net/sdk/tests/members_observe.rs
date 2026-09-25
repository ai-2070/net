// SPDX-License-Identifier: MIT OR Apache-2.0
//! V3-3: signed membership observations. Only a holder of the authority may
//! read a node's inventory (a request signed by the subnet authority's root,
//! or by the org root), the node answers only a fresh request naming it, and
//! its signed observation binds exactly that request.
#![cfg(feature = "net")]

use std::time::{SystemTime, UNIX_EPOCH};

use net::adapter::net::behavior::org::{OrgKeypair, OrgMembershipCert};
use net::adapter::net::behavior::org_authority::NodeAuthority;
use net::adapter::net::identity::EntityKeypair;
use net::adapter::net::subnet::{SubnetAuthorityConfig, TopologySubnetId};
use net::adapter::net::{MeshNode, MeshNodeConfig};
use net_sdk::members::{
    answer_members, MembersObservation, MembersOutcome, MembersRequest, MembersTarget,
};

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

async fn node(subnet_root: Option<&EntityKeypair>) -> (MeshNode, EntityKeypair) {
    let identity = EntityKeypair::generate();
    let mut cfg = MeshNodeConfig::new("127.0.0.1:0".parse().unwrap(), [0x31u8; 32]);
    if let Some(root) = subnet_root {
        cfg = cfg.with_subnet_authority(SubnetAuthorityConfig {
            authority: root.entity_id().clone(),
            roots: vec![root.entity_id().clone()],
            maximum_grant_lifetime_secs: 30 * 24 * 60 * 60,
        });
    }
    let node = MeshNode::new(identity.clone(), cfg).await.unwrap();
    (node, identity)
}

fn subnet_target(root: &EntityKeypair) -> MembersTarget {
    MembersTarget::Subnet {
        authority: root.entity_id().clone(),
        scope: TopologySubnetId::new(&[3]),
    }
}

#[tokio::test]
async fn only_the_subnet_authority_reads_a_verifiers_admissions() {
    let root = EntityKeypair::generate();
    let (verifier, identity) = node(Some(&root)).await;

    // Signed by the authority's root, naming this node: observed.
    let request =
        MembersRequest::sign(subnet_target(&root), identity.entity_id().clone(), &root).unwrap();
    let observation = answer_members(&request.to_bytes(), &verifier, now()).unwrap();
    observation.verify_for(&request).unwrap();
    assert_eq!(observation.outcome, MembersOutcome::Observed);
    assert!(observation.admitted.is_empty(), "nobody is connected");

    // Not the authority's root: refused.
    let stranger = EntityKeypair::generate();
    let forged = MembersRequest::sign(
        subnet_target(&root),
        identity.entity_id().clone(),
        &stranger,
    )
    .unwrap();
    assert!(answer_members(&forged.to_bytes(), &verifier, now()).is_err());
    // Addressed to another node: refused.
    let elsewhere = MembersRequest::sign(
        subnet_target(&root),
        EntityKeypair::generate().entity_id().clone(),
        &root,
    )
    .unwrap();
    assert!(answer_members(&elsewhere.to_bytes(), &verifier, now()).is_err());
    // Stale: refused.
    assert!(answer_members(&request.to_bytes(), &verifier, now() + 10_000).is_err());
    // A tampered signature on the request: refused.
    let mut tampered = request.to_bytes();
    let last = tampered.len() - 1;
    tampered[last] ^= 1;
    assert!(answer_members(&tampered, &verifier, now()).is_err());

    // Another authority's scope, which this node does not verify for.
    let other_root = EntityKeypair::generate();
    let other = MembersRequest::sign(
        subnet_target(&other_root),
        identity.entity_id().clone(),
        &other_root,
    )
    .unwrap();
    let answered = answer_members(&other.to_bytes(), &verifier, now()).unwrap();
    assert_eq!(answered.outcome, MembersOutcome::NotVerifier);

    // The observation binds exactly its request and its signer.
    let again =
        MembersRequest::sign(subnet_target(&root), identity.entity_id().clone(), &root).unwrap();
    assert!(observation.verify_for(&again).is_err());
    let mut forged_obs = observation.to_bytes();
    let last = forged_obs.len() - 1;
    forged_obs[last] ^= 1;
    assert!(MembersObservation::from_bytes(&forged_obs)
        .unwrap()
        .verify_for(&request)
        .is_err());
}

#[tokio::test]
async fn only_the_org_root_reads_member_standing() {
    let org = OrgKeypair::generate();
    let org_signer = EntityKeypair::from_bytes(*org.secret_bytes());
    let member = EntityKeypair::generate();
    let target = || MembersTarget::Org {
        org: org.org_id(),
        subjects: vec![member.entity_id().clone()],
    };
    let (plain, plain_id) = node(None).await;

    // A node that is not a member of the org enforces nothing for it.
    let request =
        MembersRequest::sign(target(), plain_id.entity_id().clone(), &org_signer).unwrap();
    let answered = answer_members(&request.to_bytes(), &plain, now()).unwrap();
    assert_eq!(answered.outcome, MembersOutcome::NotMember);
    // Any other signer: refused.
    let stranger = EntityKeypair::generate();
    let forged = MembersRequest::sign(target(), plain_id.entity_id().clone(), &stranger).unwrap();
    assert!(answer_members(&forged.to_bytes(), &plain, now()).is_err());

    // A member of the org reports the named member's floor there.
    let (enforcer, enforcer_id) = node(None).await;
    let tmp = tempfile::tempdir().unwrap();
    let authority = NodeAuthority::adopt(
        tmp.path(),
        OrgMembershipCert::try_issue(&org, enforcer_id.entity_id().clone(), 1, 3600).unwrap(),
        enforcer_id.entity_id(),
        0,
        None,
    )
    .unwrap();
    enforcer
        .install_node_authority(std::sync::Arc::new(authority))
        .unwrap();
    let request =
        MembersRequest::sign(target(), enforcer_id.entity_id().clone(), &org_signer).unwrap();
    let observed = answer_members(&request.to_bytes(), &enforcer, now()).unwrap();
    observed.verify_for(&request).unwrap();
    assert_eq!(observed.outcome, MembersOutcome::Observed);
    assert_eq!(observed.floors, vec![(member.entity_id().clone(), 0)]);
}
