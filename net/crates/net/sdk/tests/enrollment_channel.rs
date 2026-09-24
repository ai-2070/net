// SPDX-License-Identifier: MIT OR Apache-2.0
//! V3-2A C2: the channel relation in invites and bundles, and the chain
//! minted at redemption. The invite signs exactly one channel offer
//! (canonical name + its `u64` hash, token root, publish/subscribe rights);
//! the enrollment node mints `root → node → device` for exactly the
//! redeeming device under its root grant for that channel; the device refuses
//! anything but what it was offered; and two names sharing a `u16` wire hint
//! never stand in for each other.
#![cfg(feature = "net")]

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use net::adapter::net::channel::ChannelName;
use net::adapter::net::identity::{
    EntityKeypair, PermissionToken, RevocationRegistry, TokenChain, TokenScope,
};
use net_sdk::bootstrap_credential::Psk;
use net_sdk::channel_issuer::ChannelLeafIssuer;
use net_sdk::enrollment::bundle::{
    channel_chain_matches, BundleError, MembershipBundle, MembershipIssuer, MembershipReceipt,
    MeshContact,
};
use net_sdk::enrollment::invite::{
    ChannelOffer, EnrollmentEndpoint, EnrollmentKey, InviteError, InviteSpec, MembershipInvite,
    RedemptionIntent, Relation,
};
use net_sdk::enrollment::policy::{ApprovalMode, InvitationPolicy};
use net_sdk::enrollment::redeem::Refusal;
use net_sdk::enrollment::service::BundleIssuer;
use net_sdk::identity::Identity;

const PSK: [u8; 32] = [0x45; 32];
const DAY: u64 = 24 * 60 * 60;
const CHANNEL: &str = "fleet.telemetry";

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn root() -> EntityKeypair {
    EntityKeypair::from_bytes([0xC1; 32])
}

fn channel(name: &str) -> ChannelName {
    ChannelName::new(name).unwrap()
}

fn rights(bits: &[TokenScope]) -> TokenScope {
    bits.iter().fold(TokenScope::NONE, |acc, s| acc.union(*s))
}

fn offer(name: &str, r: TokenScope) -> ChannelOffer {
    ChannelOffer {
        channel: channel(name),
        root: root().entity_id().clone(),
        rights: r,
    }
}

fn spec(relations: Vec<Relation>, channel: Option<ChannelOffer>) -> InviteSpec {
    InviteSpec {
        trust_domain_name: "lab".into(),
        trust_domain: Psk::new(PSK).trust_domain(),
        endpoint: Some(EnrollmentEndpoint::parse("127.0.0.1:9").unwrap()),
        relay: None,
        enrollment_key: EnrollmentKey([6; 32]),
        subnet: None,
        org: None,
        channel,
        relations,
        intended_subject: None,
        policy: InvitationPolicy::with_options(
            now(),
            Duration::from_secs(3600),
            ApprovalMode::Preauthorized,
        )
        .unwrap(),
    }
}

/// The root's DELEGATE grant on `name` to the operator's identity.
fn leaf_issuer(operator: &Identity, name: &str, r: TokenScope) -> ChannelLeafIssuer {
    let grant = PermissionToken::try_issue(
        &root(),
        operator.entity_id().clone(),
        r.union(TokenScope::DELEGATE),
        channel(name).hash(),
        30 * DAY,
        1,
    )
    .unwrap();
    ChannelLeafIssuer::new(grant, (**operator.keypair()).clone()).unwrap()
}

fn contact() -> MeshContact {
    MeshContact {
        addr: Some("127.0.0.1:9".parse().unwrap()),
        noise_pubkey: [1; 32],
        node_id: 7,
        relay: None,
    }
}

fn issuer(operator: &Identity, issuers: Vec<ChannelLeafIssuer>) -> MembershipIssuer {
    MembershipIssuer::new(operator.clone(), Psk::new(PSK), contact()).with_channel_issuers(issuers)
}

/// Two distinct canonical names that share a `u16` wire hint.
fn same_hint_pair() -> (String, String) {
    let mut seen = std::collections::HashMap::new();
    for i in 0u32.. {
        let name = format!("fleet.c{i}");
        let hint = channel(&name).wire_hash();
        if let Some(prior) = seen.insert(hint, name.clone()) {
            return (prior, name);
        }
    }
    unreachable!()
}

#[test]
fn the_channel_offer_is_signed_canonical_and_must_match_its_relation() {
    let operator = Identity::generate();
    let o = offer(CHANNEL, TokenScope::SUBSCRIBE);
    let invite = MembershipInvite::sign(
        &operator,
        spec(vec![Relation::Mesh, Relation::Channel], Some(o.clone())),
    )
    .unwrap();
    let back = MembershipInvite::decode(&invite.encode()).unwrap();
    assert_eq!(back.channel(), Some(&o));
    assert_eq!(back.relations(), &[Relation::Mesh, Relation::Channel]);

    // Relation and offer together or not at all; mesh membership required;
    // rights are publish/subscribe only and never empty.
    let refused = |relations: Vec<Relation>, channel: Option<ChannelOffer>| {
        matches!(
            MembershipInvite::sign(&operator, spec(relations, channel)),
            Err(InviteError::Relations(_))
        )
    };
    assert!(refused(vec![Relation::Mesh, Relation::Channel], None));
    assert!(refused(vec![Relation::Mesh], Some(o.clone())));
    // A channel relation alone is the standalone form (for a device already
    // on the mesh); with anything but mesh membership it is refused.
    assert!(
        MembershipInvite::sign(&operator, spec(vec![Relation::Channel], Some(o.clone()))).is_ok()
    );
    assert!(refused(
        vec![Relation::Subnet, Relation::Channel],
        Some(o.clone())
    ));
    for bad in [
        TokenScope::NONE,
        TokenScope::ADMIN,
        TokenScope::WILDCARD,
        TokenScope::DELEGATE,
        TokenScope::PUBLISH.union(TokenScope::ADMIN),
    ] {
        assert!(
            refused(
                vec![Relation::Mesh, Relation::Channel],
                Some(offer(CHANNEL, bad))
            ),
            "{bad:?}"
        );
    }

    // The encoded u64 must be the name's: a swapped hash is refused before
    // the signature is even consulted, and widening the rights byte breaks
    // the signature.
    let hash = channel(CHANNEL).hash().to_le_bytes();
    let at = invite
        .to_bytes()
        .windows(8)
        .position(|w| w == hash)
        .unwrap();
    let mut swapped = invite.to_bytes().to_vec();
    swapped[at..at + 8].copy_from_slice(&channel("fleet.other").hash().to_le_bytes());
    assert_eq!(
        MembershipInvite::from_bytes(&swapped).unwrap_err(),
        InviteError::Malformed("channel hash is not the name's")
    );
    let mut widened = invite.to_bytes().to_vec();
    widened[at + 8 + 32] = rights(&[TokenScope::PUBLISH, TokenScope::SUBSCRIBE]).bits() as u8;
    assert_eq!(
        MembershipInvite::from_bytes(&widened).unwrap_err(),
        InviteError::BadSignature
    );
    let mut admin = invite.to_bytes().to_vec();
    admin[at + 8 + 32] = TokenScope::ADMIN.bits() as u8;
    assert!(MembershipInvite::from_bytes(&admin).is_err());
}

#[test]
fn redemption_mints_exactly_the_offered_chain_for_the_redeeming_device() {
    let operator = Identity::generate();
    let device = Identity::generate();
    let full = rights(&[TokenScope::PUBLISH, TokenScope::SUBSCRIBE]);
    let o = offer(CHANNEL, TokenScope::PUBLISH);
    let invite = MembershipInvite::sign(
        &operator,
        spec(vec![Relation::Mesh, Relation::Channel], Some(o.clone())),
    )
    .unwrap();
    let intent = RedemptionIntent::for_invite(&invite, device.entity_id().clone()).unwrap();

    let bytes = issuer(&operator, vec![leaf_issuer(&operator, CHANNEL, full)])
        .issue(&invite, &intent)
        .unwrap();
    let bundle = MembershipBundle::from_bytes(&bytes).unwrap();
    bundle.verify_for(&invite, &intent).unwrap();
    let chain = bundle.channel_chain().unwrap();
    assert_eq!(chain.tokens.len(), 2, "root → node → device");
    assert_eq!(&chain.tokens[0].issuer, root().entity_id());
    assert_eq!(&chain.tokens[1].subject, device.entity_id());
    assert_eq!(
        chain.tokens[1].scope,
        TokenScope::PUBLISH,
        "exactly the offer"
    );
    chain
        .verify_authorizes(
            TokenScope::PUBLISH,
            channel(CHANNEL).hash(),
            device.entity_id(),
            &[root().entity_id().clone()],
            &RevocationRegistry::new(),
            0,
        )
        .unwrap();

    // No grant for the channel, or one that does not cover the rights, or
    // one from another root: the invite cannot be honoured.
    let subscribe_only = leaf_issuer(&operator, CHANNEL, TokenScope::SUBSCRIBE);
    for issuers in [vec![], vec![subscribe_only]] {
        assert_eq!(
            issuer(&operator, issuers)
                .issue(&invite, &intent)
                .unwrap_err(),
            Refusal::Unavailable
        );
    }
    let other_root = PermissionToken::try_issue(
        &EntityKeypair::generate(),
        operator.entity_id().clone(),
        full.union(TokenScope::DELEGATE),
        channel(CHANNEL).hash(),
        DAY,
        1,
    )
    .unwrap();
    let other_root = ChannelLeafIssuer::new(other_root, (**operator.keypair()).clone()).unwrap();
    assert_eq!(
        issuer(&operator, vec![other_root])
            .issue(&invite, &intent)
            .unwrap_err(),
        Refusal::Unavailable
    );
}

#[test]
fn the_device_accepts_only_the_offered_chain_for_itself() {
    let operator = Identity::generate();
    let device = Identity::generate();
    let full = rights(&[TokenScope::PUBLISH, TokenScope::SUBSCRIBE]);
    let o = offer(CHANNEL, TokenScope::SUBSCRIBE);
    let invite = MembershipInvite::sign(
        &operator,
        spec(vec![Relation::Mesh, Relation::Channel], Some(o.clone())),
    )
    .unwrap();
    let intent = RedemptionIntent::for_invite(&invite, device.entity_id().clone()).unwrap();
    let grantor = leaf_issuer(&operator, CHANNEL, full);
    let deliver = |chain: Option<TokenChain>| {
        let receipt = MembershipReceipt::sign(&operator, &invite, &intent, now());
        let bundle = MembershipBundle::new(receipt, Psk::new(PSK), contact());
        let bundle = match chain {
            Some(chain) => bundle.with_channel_chain(&chain),
            None => bundle,
        };
        MembershipBundle::from_bytes(&bundle.to_bytes())
            .unwrap()
            .verify_for(&invite, &intent)
    };
    let refused = Err(BundleError::Mismatch("channel chain"));

    let exact = grantor
        .issue(device.entity_id(), TokenScope::SUBSCRIBE)
        .unwrap();
    assert_eq!(deliver(Some(exact.clone())), Ok(()));
    assert_eq!(deliver(None), refused, "a channel invite needs its chain");
    // More rights than offered, another device's chain, or the leaf alone.
    assert_eq!(
        deliver(Some(grantor.issue(device.entity_id(), full).unwrap())),
        refused
    );
    assert_eq!(
        deliver(Some(
            grantor
                .issue(Identity::generate().entity_id(), TokenScope::SUBSCRIBE)
                .unwrap()
        )),
        refused
    );
    assert_eq!(
        deliver(Some(TokenChain::single(exact.tokens[1].clone()))),
        refused
    );
    assert!(!channel_chain_matches(
        &offer("fleet.other", TokenScope::SUBSCRIBE),
        device.entity_id(),
        &exact
    ));

    // A chain on an invite with no channel relation is refused too.
    let plain = MembershipInvite::sign(&operator, spec(vec![Relation::Mesh], None)).unwrap();
    let plain_intent = RedemptionIntent::for_invite(&plain, device.entity_id().clone()).unwrap();
    let receipt = MembershipReceipt::sign(&operator, &plain, &plain_intent, now());
    let stray = MembershipBundle::new(receipt, Psk::new(PSK), contact()).with_channel_chain(&exact);
    assert_eq!(
        stray.verify_for(&plain, &plain_intent),
        Err(BundleError::Mismatch("channel chain"))
    );
}

/// Two names sharing a `u16` wire hint are distinct authorities: a grant for
/// one never mints for the other, and a chain for one is never accepted as
/// the other's.
#[test]
fn names_sharing_a_wire_hint_never_stand_in_for_each_other() {
    let (a, b) = same_hint_pair();
    assert_eq!(channel(&a).wire_hash(), channel(&b).wire_hash());
    assert_ne!(channel(&a).hash(), channel(&b).hash());

    let operator = Identity::generate();
    let device = Identity::generate();
    let invite = MembershipInvite::sign(
        &operator,
        spec(
            vec![Relation::Mesh, Relation::Channel],
            Some(offer(&a, TokenScope::PUBLISH)),
        ),
    )
    .unwrap();
    let intent = RedemptionIntent::for_invite(&invite, device.entity_id().clone()).unwrap();
    let grant_b = leaf_issuer(&operator, &b, TokenScope::PUBLISH);
    assert_eq!(
        issuer(&operator, vec![grant_b.clone()])
            .issue(&invite, &intent)
            .unwrap_err(),
        Refusal::Unavailable
    );
    let chain_b = grant_b
        .issue(device.entity_id(), TokenScope::PUBLISH)
        .unwrap();
    assert!(!channel_chain_matches(
        invite.channel().unwrap(),
        device.entity_id(),
        &chain_b
    ));
    // The grant for the right name does mint.
    let bytes = issuer(
        &operator,
        vec![grant_b, leaf_issuer(&operator, &a, TokenScope::PUBLISH)],
    )
    .issue(&invite, &intent)
    .unwrap();
    MembershipBundle::from_bytes(&bytes)
        .unwrap()
        .verify_for(&invite, &intent)
        .unwrap();
}

/// V3 S3: a standalone channel link (channel relation only) is redeemed over
/// the device's existing session. The chain goes only to the entity the
/// session proved, for exactly the offer; a retry by the same device gets back
/// the COMMITTED chain (byte-identical, never re-minted); another device gets
/// nothing; and the device's membership store refuses any chain after leave.
#[test]
fn a_standalone_channel_link_issues_one_chain_and_recovers_it_exactly() {
    use net_sdk::enrollment::service::SharedLedger;
    use net_sdk::enrollment::standalone::{
        answer_standalone_redeem, is_standalone_channel, ChannelMembership, SubnetRedeemReply,
        SubnetRedeemRequest,
    };
    use net_sdk::enrollment::store::{EnrollmentLedger, LedgerLimits};

    const NODE: u64 = 0x0C4A_0001;
    let operator = Identity::generate();
    let tmp = tempfile::tempdir().unwrap();
    let ledger: SharedLedger = std::sync::Arc::new(parking_lot::Mutex::new(
        EnrollmentLedger::create(
            &tmp.path().join("ledger"),
            operator.entity_id().clone(),
            LedgerLimits::default(),
        )
        .unwrap(),
    ));
    let link = MembershipInvite::sign(
        &operator,
        spec(
            vec![Relation::Channel],
            Some(offer(CHANNEL, TokenScope::SUBSCRIBE)),
        ),
    )
    .unwrap();
    assert!(is_standalone_channel(&link));
    ledger.lock().offer(link.offer_spec(), now()).unwrap();
    let issuers = vec![leaf_issuer(&operator, CHANNEL, TokenScope::SUBSCRIBE)];
    let device = Identity::generate();
    let answer = |who: &Identity, proven: &Identity| {
        answer_standalone_redeem(
            &SubnetRedeemRequest::sign(who, &link, NODE, now())
                .unwrap()
                .to_bytes(),
            Some(proven.entity_id()),
            NODE,
            &ledger,
            None,
            None,
            &issuers,
            now(),
        )
    };

    let first = answer(&device, &device).unwrap();
    let chain = first.channel_chain().expect("a channel chain");
    assert!(net_sdk::enrollment::bundle::channel_chain_matches(
        link.channel().unwrap(),
        device.entity_id(),
        &chain
    ));
    // Lost reply: the same device gets exactly the committed bytes back.
    std::thread::sleep(Duration::from_millis(1100));
    let again = answer(&device, &device).unwrap();
    assert_eq!(again, first, "recovered, not re-minted");
    // Another device, even over its own proven session, gets nothing.
    let other = Identity::generate();
    assert_eq!(answer(&other, &other), Err(Refusal::Conflict));
    // The session must have proven the requesting device.
    assert_eq!(answer(&device, &other), Err(Refusal::Conflict));
    // No grant for the channel at this node: the link cannot be honoured.
    let late = MembershipInvite::sign(
        &operator,
        spec(
            vec![Relation::Channel],
            Some(offer("fleet.other", TokenScope::SUBSCRIBE)),
        ),
    )
    .unwrap();
    ledger.lock().offer(late.offer_spec(), now()).unwrap();
    assert_eq!(
        answer_standalone_redeem(
            &SubnetRedeemRequest::sign(&device, &late, NODE, now())
                .unwrap()
                .to_bytes(),
            Some(device.entity_id()),
            NODE,
            &ledger,
            None,
            None,
            &issuers,
            now(),
        ),
        Err(Refusal::Unavailable)
    );
    assert!(matches!(
        SubnetRedeemReply::from_bytes(&first.to_bytes()),
        Ok(SubnetRedeemReply::ChannelIssued(_))
    ));

    // Device side: the store installs only the offered chain for itself, and
    // after leave it installs nothing again (a delayed delivery cannot
    // reinstate the departed incarnation).
    let dir = tmp.path().join("membership");
    let mut membership =
        ChannelMembership::begin(&dir, &link, device.entity_id().clone(), NODE).unwrap();
    let foreign = issuers[0]
        .issue(other.entity_id(), TokenScope::SUBSCRIBE)
        .unwrap();
    assert!(membership.install(&foreign).is_err());
    membership.install(&chain).unwrap();
    drop(membership);
    let mut membership = ChannelMembership::open(&dir).unwrap();
    assert!(membership.chain().is_some(), "survives reopen");
    assert!(membership.leave(now()).unwrap());
    assert!(!membership.leave(now()).unwrap(), "idempotent");
    assert!(membership.install(&chain).is_err(), "fenced after leave");
    drop(membership);
    let reopened = ChannelMembership::open(&dir).unwrap();
    assert!(reopened.chain().is_none() && reopened.left_at().is_some());
}
