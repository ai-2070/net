//! Durable issuer state, end to end (decision 4b).
//!
//! `issuer_generation` existed on the wire and in the revocation
//! registry, but no SDK could set it: `Identity` had no epoch, every
//! token it minted was generation zero, and the only way to rotate was
//! to hand-assemble a `PermissionToken` and re-sign it. So the field
//! was inert, and the one operational property it exists for —
//! retiring an issuer's outstanding credentials without a CRL — was
//! not reachable from any supported API.
//!
//! These walk the rotation the decision specifies, and the ways it can
//! go wrong: key-only restoration silently returning generation zero,
//! a generation going backwards, and the ceiling at `u32::MAX`.

use std::time::Duration;

use net::adapter::net::identity::RevocationRegistry;
use net::adapter::net::ChannelName;

use crate::identity::{Identity, IdentityStateError, IDENTITY_STATE_SIZE};
use crate::TokenScope;

fn channel() -> ChannelName {
    ChannelName::new("issuer/rotation").unwrap()
}

fn issue(id: &Identity, subject: &Identity) -> crate::PermissionToken {
    id.issue_token(
        subject.entity_id().clone(),
        TokenScope::PUBLISH.union(TokenScope::DELEGATE),
        &channel(),
        Duration::from_secs(3600),
        3,
    )
}

/// The full rotation: mint at zero, rotate to one, raise the floor,
/// watch the old token die and the replacement live.
#[test]
fn rotation_retires_the_old_generation_and_admits_the_new() {
    let issuer = Identity::generate();
    let subject = Identity::generate();
    let registry = RevocationRegistry::new();

    // 1. Generation-zero token accepted.
    let old = issue(&issuer, &subject);
    assert_eq!(old.issuer_generation, 0);
    assert!(!registry.is_revoked(&old));

    // 2. Generation-one state persisted — before any floor moves.
    let rotated = issuer.at_generation(1).expect("rotate 0 -> 1");
    let persisted = rotated.to_state_bytes();
    assert_eq!(rotated.issuer_generation(), 1);
    assert_eq!(
        issuer.issuer_generation(),
        0,
        "rotation must not mutate the identity it came from, nor its clones"
    );

    // 3. Floor raised to one.
    registry.revoke_below(issuer.entity_id(), 1);

    // 4. Old token rejected.
    assert!(
        registry.is_revoked(&old),
        "the generation-zero token must fall below floor 1"
    );

    // 5. Same key restored at generation one.
    let restored = Identity::from_state_bytes(&persisted).expect("restore issuer state");
    assert_eq!(restored.entity_id(), issuer.entity_id());
    assert_eq!(restored.issuer_generation(), 1);

    // 6. Replacement token accepted.
    let fresh = issue(&restored, &subject);
    assert_eq!(fresh.issuer_generation, 1);
    assert!(
        !registry.is_revoked(&fresh),
        "a token at the current generation must survive its own floor"
    );
    fresh.verify().expect("replacement must verify");
}

/// Restart from versioned state preserves the generation.
#[test]
fn restart_from_versioned_state_preserves_the_generation() {
    let issuer = Identity::generate().at_generation(9).expect("rotate to 9");
    let bytes = issuer.to_state_bytes();
    assert_eq!(bytes.len(), IDENTITY_STATE_SIZE);

    let back = Identity::from_state_bytes(&bytes).expect("restore");
    assert_eq!(back.issuer_generation(), 9);
    assert_eq!(back.entity_id(), issuer.entity_id());
    assert_eq!(
        back.to_state_bytes(),
        bytes,
        "state must round-trip byte-for-byte"
    );
}

/// Key-only restoration demonstrably returns generation zero.
///
/// This is the trap the docs on `to_bytes` warn about, pinned so the
/// warning cannot drift away from the behaviour. An issuer that
/// rotated to 4, published floor 4, then came back through the seed
/// path mints at 0 — below its own floor, so nothing it signs is
/// accepted, and it has no way to climb back without knowing 4.
#[test]
fn key_only_restoration_returns_generation_zero() {
    let issuer = Identity::generate().at_generation(4).expect("rotate to 4");
    let subject = Identity::generate();
    let registry = RevocationRegistry::new();
    registry.revoke_below(issuer.entity_id(), 4);

    let seed_only = Identity::from_bytes(&issuer.to_bytes()).expect("restore from seed");
    assert_eq!(seed_only.entity_id(), issuer.entity_id());
    assert_eq!(
        seed_only.issuer_generation(),
        0,
        "the seed carries no epoch"
    );

    let token = issue(&seed_only, &subject);
    assert!(
        registry.is_revoked(&token),
        "a key-only restore mints below its own published floor — this \
         is why `to_state_bytes` exists"
    );
}

/// Decreasing generation rejected; equal is idempotent.
#[test]
fn generation_may_not_go_backwards() {
    let issuer = Identity::generate().at_generation(5).expect("rotate to 5");

    assert_eq!(
        issuer.at_generation(4).unwrap_err(),
        IdentityStateError::GenerationWentBackwards {
            current: 5,
            requested: 4,
        }
    );

    // Re-applying the persisted generation on restart is not an error.
    assert_eq!(issuer.at_generation(5).unwrap().issuer_generation(), 5);
    assert_eq!(issuer.at_generation(6).unwrap().issuer_generation(), 6);
}

/// Maximum generation requires key rotation.
#[test]
fn generation_ceiling_demands_a_key_rotation() {
    let issuer = Identity::generate()
        .at_generation(u32::MAX)
        .expect("rotate to the ceiling");
    assert_eq!(issuer.issuer_generation(), u32::MAX);

    // The ceiling is still usable for issuance...
    let subject = Identity::generate();
    assert_eq!(issue(&issuer, &subject).issuer_generation, u32::MAX);

    // ...but there is nothing above it, so the answer is a new key.
    assert_eq!(
        issuer.at_generation(u32::MAX).unwrap_err(),
        IdentityStateError::GenerationExhausted
    );
}

/// Malformed and future state is refused, not partially parsed.
#[test]
fn state_bytes_are_validated_before_they_become_an_issuer() {
    let good = Identity::generate()
        .at_generation(2)
        .unwrap()
        .to_state_bytes();

    assert_eq!(
        Identity::from_state_bytes(&good[..IDENTITY_STATE_SIZE - 1]).unwrap_err(),
        IdentityStateError::InvalidLength {
            got: IDENTITY_STATE_SIZE - 1
        }
    );
    assert_eq!(
        Identity::from_state_bytes(&[]).unwrap_err(),
        IdentityStateError::InvalidLength { got: 0 }
    );
    // A 32-byte seed is not identity state, and must not be read as
    // one — silently accepting it would resurrect the generation-zero
    // trap through the versioned door.
    assert_eq!(
        Identity::from_state_bytes(&[0u8; 32]).unwrap_err(),
        IdentityStateError::InvalidLength { got: 32 }
    );

    let mut future = good;
    future[0] = 2;
    assert_eq!(
        Identity::from_state_bytes(&future).unwrap_err(),
        IdentityStateError::UnsupportedVersion { found: 2 }
    );
}

/// Root and machine at different generations produce a valid chain,
/// and each link is governed by its own signer's floor.
#[test]
fn a_chain_spans_issuers_at_different_generations() {
    let root = Identity::generate().at_generation(3).expect("root at 3");
    let machine = Identity::generate().at_generation(7).expect("machine at 7");
    let gateway = Identity::generate();
    let sibling = Identity::generate().at_generation(1).expect("sibling at 1");

    let root_link = issue(&root, &machine);
    let machine_link = root_link
        .delegate_with_generation(
            machine.keypair(),
            machine.issuer_generation(),
            gateway.entity_id().clone(),
            TokenScope::PUBLISH,
        )
        .expect("machine -> gateway");

    assert_eq!(root_link.issuer_generation, 3);
    assert_eq!(machine_link.issuer_generation, 7);
    root_link.verify().expect("root link verifies");
    machine_link.verify().expect("machine link verifies");

    let registry = RevocationRegistry::new();
    assert!(!registry.is_revoked(&root_link));
    assert!(!registry.is_revoked(&machine_link));

    // Revoking root invalidates the chain through the root-issued link.
    registry.revoke_below(root.entity_id(), 4);
    assert!(registry.is_revoked(&root_link));
    assert!(
        !registry.is_revoked(&machine_link),
        "machine's link answers to machine's floor; the chain breaks at \
         the root link, which is the one root signed"
    );

    // Revoking machine invalidates it through the machine-issued link.
    registry.revoke_below(machine.entity_id(), 8);
    assert!(registry.is_revoked(&machine_link));

    // A sibling issuer remains valid under both.
    let sibling_link = issue(&sibling, &gateway);
    assert!(!registry.is_revoked(&sibling_link), "floors are per-issuer");
}
