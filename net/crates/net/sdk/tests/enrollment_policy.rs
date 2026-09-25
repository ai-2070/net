// SPDX-License-Identifier: MIT OR Apache-2.0
//! V3 policy only: no wire format, credentials, store or listener is implied.
#![cfg(feature = "net")]

use net_sdk::enrollment::policy::{ApprovalMode, InvitationPolicy, PolicyError};
use std::time::Duration;

#[test]
fn default_invitation_is_preauthorized_for_exactly_24_hours() {
    let policy = InvitationPolicy::new(100).unwrap();
    assert_eq!(policy.created_at(), 100);
    assert_eq!(policy.expires_at(), 86_500);
    assert_eq!(policy.approval_mode(), ApprovalMode::Preauthorized);
    assert_eq!(
        policy.check_redemption_at(100),
        Ok(ApprovalMode::Preauthorized)
    );
    assert_eq!(
        policy.check_redemption_at(86_499),
        Ok(ApprovalMode::Preauthorized)
    );
}

#[test]
fn exact_expiry_and_later_times_refuse_in_both_modes() {
    for mode in [ApprovalMode::Preauthorized, ApprovalMode::RequireApproval] {
        let policy = InvitationPolicy::with_options(100, Duration::from_secs(900), mode).unwrap();
        assert_eq!(policy.check_redemption_at(999), Ok(mode));
        for now in [1000, 1001, u64::MAX] {
            assert_eq!(policy.check_redemption_at(now), Err(PolicyError::Expired));
        }
    }
}

#[test]
fn explicit_approval_is_preserved_not_implicitly_authorized() {
    let policy = InvitationPolicy::with_options(
        100,
        Duration::from_secs(3600),
        ApprovalMode::RequireApproval,
    )
    .unwrap();
    assert_eq!(policy.expires_at(), 3700);
    assert_eq!(policy.approval_mode(), ApprovalMode::RequireApproval);
    assert_eq!(
        policy.check_redemption_at(101),
        Ok(ApprovalMode::RequireApproval)
    );
}

#[test]
fn inspection_and_repeated_checks_never_extend_expiry() {
    let policy = InvitationPolicy::new(100).unwrap();
    let original = policy;
    for now in [100, 500, 86_499, 86_500, 86_501] {
        let _ = policy.check_redemption_at(now);
        let _ = (
            policy.created_at(),
            policy.expires_at(),
            policy.approval_mode(),
        );
        assert_eq!(policy, original);
    }
}

#[test]
fn zero_and_fractional_ttls_are_rejected_without_rounding() {
    for ttl in [
        Duration::ZERO,
        Duration::from_nanos(1),
        Duration::from_millis(1500),
    ] {
        assert_eq!(
            InvitationPolicy::with_options(100, ttl, ApprovalMode::Preauthorized),
            Err(PolicyError::InvalidTtl)
        );
    }
}

#[test]
fn timestamp_overflow_is_rejected_not_saturated() {
    assert_eq!(
        InvitationPolicy::new(u64::MAX),
        Err(PolicyError::ExpiryOverflow)
    );
    assert_eq!(
        InvitationPolicy::with_options(
            1,
            Duration::from_secs(u64::MAX),
            ApprovalMode::Preauthorized
        ),
        Err(PolicyError::ExpiryOverflow)
    );
    let policy = InvitationPolicy::with_options(
        u64::MAX - 1,
        Duration::from_secs(1),
        ApprovalMode::Preauthorized,
    )
    .unwrap();
    assert_eq!(
        policy.check_redemption_at(u64::MAX),
        Err(PolicyError::Expired)
    );
}

#[test]
fn future_creation_refuses_redemption() {
    let policy = InvitationPolicy::new(100).unwrap();
    assert_eq!(
        policy.check_redemption_at(99),
        Err(PolicyError::NotYetValid)
    );
}
