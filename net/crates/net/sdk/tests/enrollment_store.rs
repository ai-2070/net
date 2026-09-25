// SPDX-License-Identifier: MIT OR Apache-2.0
//! V3 durable invitation ledger. Inputs are trusted fixtures: these witnesses
//! prove single-use/ordering/persistence transitions, not signature or proof
//! verification, and no credential delivery, listener or CLI is implied.
#![cfg(feature = "net")]

use std::path::{Path, PathBuf};
use std::time::Duration;

use net_sdk::enrollment::policy::{ApprovalMode, InvitationPolicy, PolicyError};
use net_sdk::enrollment::store::{
    ClaimOutcome, Claimant, CompactReport, EnrollmentLedger, InvitationId, LedgerError,
    LedgerLimits, OfferSpec, OfferState, RECEIPT_RECOVERY_WINDOW_SECS,
};
use net_sdk::identity::EntityId;

const T0: u64 = 1_000;
const TTL: u64 = 86_400;

fn entity(b: u8) -> EntityId {
    EntityId::from_bytes([b; 32])
}

fn issuer() -> EntityId {
    entity(0xAA)
}

fn claimant(subject: u8, intent: u8) -> Claimant {
    Claimant {
        subject: entity(subject),
        intent_digest: [intent; 32],
    }
}

fn spec(seed: u8, mode: ApprovalMode, intended: Option<EntityId>) -> OfferSpec {
    OfferSpec {
        invitation_id: InvitationId::from_bytes([seed; 16]),
        invite_digest: [seed; 32],
        scope_digest: [seed ^ 0x55; 32],
        intended_subject: intended,
        policy: InvitationPolicy::with_options(T0, Duration::from_secs(TTL), mode).unwrap(),
    }
}

fn fresh(tmp: &tempfile::TempDir) -> (PathBuf, EnrollmentLedger) {
    let dir = tmp.path().join("ledger");
    let ledger = EnrollmentLedger::create(&dir, issuer(), LedgerLimits::default()).unwrap();
    (dir, ledger)
}

fn reopen(dir: &Path) -> EnrollmentLedger {
    EnrollmentLedger::open(dir, issuer(), LedgerLimits::default()).unwrap()
}

#[test]
fn preauthorized_claim_issues_once_and_recovers_identical_bytes_after_reopen() {
    let tmp = tempfile::tempdir().unwrap();
    let (dir, mut ledger) = fresh(&tmp);
    let s = spec(1, ApprovalMode::Preauthorized, None);
    let id = s.invitation_id;
    let offer = ledger.offer(s, T0).unwrap();
    let b = claimant(0xB0, 1);

    // No approver callback: the default claim is immediately ready.
    assert_eq!(ledger.claim(&id, &b, T0 + 5).unwrap(), ClaimOutcome::Ready);
    let receipt = ledger.issue(&id, &b, b"bundle-v1", T0 + 6).unwrap();
    drop(ledger);

    let mut ledger = reopen(&dir);
    let got = ledger.recover(&id, &b, T0 + 7).unwrap();
    assert_eq!(got.receipt_id, receipt);
    assert_eq!(got.payload, b"bundle-v1");
    // A retried claim reports the committed issuance; issue never mints again.
    assert_eq!(
        ledger.claim(&id, &b, T0 + 8).unwrap(),
        ClaimOutcome::AlreadyIssued(receipt)
    );
    let rev = ledger.revision();
    assert!(matches!(
        ledger.issue(&id, &b, b"bundle-v2", T0 + 9),
        Err(LedgerError::AlreadyIssued(r)) if r == receipt
    ));
    assert_eq!(ledger.revision(), rev);
    assert!(matches!(
        ledger.status(&offer).unwrap().state,
        OfferState::Issued { receipt_id, recoverable: true, .. } if receipt_id == receipt
    ));
}

#[test]
fn require_approval_cannot_issue_until_that_exact_claim_is_approved() {
    let tmp = tempfile::tempdir().unwrap();
    let (_dir, mut ledger) = fresh(&tmp);
    let s = spec(2, ApprovalMode::RequireApproval, None);
    let id = s.invitation_id;
    let offer = ledger.offer(s, T0).unwrap();
    let b = claimant(0xB0, 1);

    assert_eq!(
        ledger.claim(&id, &b, T0 + 1).unwrap(),
        ClaimOutcome::PendingApproval
    );
    assert!(matches!(
        ledger.issue(&id, &b, b"x", T0 + 2),
        Err(LedgerError::ApprovalRequired)
    ));
    // An approval naming another subject or intent cannot replace the claim.
    for wrong in [claimant(0xC0, 1), claimant(0xB0, 2)] {
        assert!(matches!(
            ledger.approve(&offer, &wrong, T0 + 3),
            Err(LedgerError::ClaimConflict)
        ));
    }
    assert!(matches!(
        ledger.status(&offer).unwrap().state,
        OfferState::PendingApproval { .. }
    ));
    ledger.approve(&offer, &b, T0 + 4).unwrap();
    ledger.approve(&offer, &b, T0 + 4).unwrap();
    assert_eq!(ledger.claim(&id, &b, T0 + 5).unwrap(), ClaimOutcome::Ready);
    ledger.issue(&id, &b, b"x", T0 + 6).unwrap();
}

#[test]
fn refused_claims_leave_the_invitation_unclaimed_and_the_revision_unchanged() {
    let tmp = tempfile::tempdir().unwrap();
    let (_dir, mut ledger) = fresh(&tmp);
    let s = spec(3, ApprovalMode::Preauthorized, Some(entity(0xB0)));
    let id = s.invitation_id;
    let offer = ledger.offer(s, T0).unwrap();
    let rev = ledger.revision();

    assert!(matches!(
        ledger.claim(&id, &claimant(0xC0, 1), T0 + 1),
        Err(LedgerError::WrongSubject)
    ));
    assert!(matches!(
        ledger.claim(
            &InvitationId::from_bytes([9; 16]),
            &claimant(0xB0, 1),
            T0 + 1
        ),
        Err(LedgerError::UnknownInvitation)
    ));
    assert!(matches!(
        ledger.claim(&id, &claimant(0xB0, 1), T0 - 1),
        Err(LedgerError::Policy(PolicyError::NotYetValid))
    ));
    assert_eq!(ledger.revision(), rev);
    assert_eq!(ledger.status(&offer).unwrap().state, OfferState::Offered);
    assert_eq!(
        ledger.claim(&id, &claimant(0xB0, 1), T0 + 2).unwrap(),
        ClaimOutcome::Ready
    );
}

#[test]
fn the_first_committed_claimant_and_intent_win() {
    let tmp = tempfile::tempdir().unwrap();
    let (dir, mut ledger) = fresh(&tmp);
    let s = spec(4, ApprovalMode::Preauthorized, None);
    let id = s.invitation_id;
    ledger.offer(s, T0).unwrap();
    let winner = claimant(0xB0, 1);
    assert_eq!(
        ledger.claim(&id, &winner, T0 + 1).unwrap(),
        ClaimOutcome::Ready
    );
    drop(ledger);

    // The winner survives restart; neither another subject nor a changed
    // intent from the same subject can claim, issue or recover.
    let mut ledger = reopen(&dir);
    for loser in [claimant(0xC0, 1), claimant(0xB0, 2)] {
        assert!(matches!(
            ledger.claim(&id, &loser, T0 + 2),
            Err(LedgerError::ClaimConflict)
        ));
        assert!(matches!(
            ledger.issue(&id, &loser, b"x", T0 + 2),
            Err(LedgerError::ClaimConflict)
        ));
    }
    assert_eq!(
        ledger.claim(&id, &winner, T0 + 3).unwrap(),
        ClaimOutcome::Ready
    );
    ledger.issue(&id, &winner, b"x", T0 + 4).unwrap();
    assert!(matches!(
        ledger.recover(&id, &claimant(0xC0, 1), T0 + 5),
        Err(LedgerError::ClaimConflict)
    ));
}

#[test]
fn expiry_boundary_refuses_claim_late_approval_and_issue_without_clock_reset() {
    let tmp = tempfile::tempdir().unwrap();
    let (_dir, mut ledger) = fresh(&tmp);
    let exp = T0 + TTL;
    let b = claimant(0xB0, 1);

    let unclaimed = spec(5, ApprovalMode::Preauthorized, None);
    let unclaimed_id = unclaimed.invitation_id;
    ledger.offer(unclaimed, T0).unwrap();
    assert!(matches!(
        ledger.claim(&unclaimed_id, &b, exp),
        Err(LedgerError::Policy(PolicyError::Expired))
    ));

    let pending = spec(6, ApprovalMode::RequireApproval, None);
    let pending_id = pending.invitation_id;
    let pending_offer = ledger.offer(pending, T0).unwrap();
    ledger.claim(&pending_id, &b, exp - 10).unwrap();
    assert!(matches!(
        ledger.approve(&pending_offer, &b, exp),
        Err(LedgerError::Policy(PolicyError::Expired))
    ));

    let ready = spec(7, ApprovalMode::Preauthorized, None);
    let ready_id = ready.invitation_id;
    ledger.offer(ready, T0).unwrap();
    ledger.claim(&ready_id, &b, exp - 1).unwrap();
    assert!(matches!(
        ledger.issue(&ready_id, &b, b"x", exp),
        Err(LedgerError::Policy(PolicyError::Expired))
    ));
    ledger.issue(&ready_id, &b, b"x", exp - 1).unwrap();

    assert!(matches!(
        ledger.offer(spec(8, ApprovalMode::Preauthorized, None), exp),
        Err(LedgerError::Policy(PolicyError::Expired))
    ));
}

#[test]
fn revoke_and_deny_are_terminal_before_issue_and_never_undo_an_issuance() {
    let tmp = tempfile::tempdir().unwrap();
    let (dir, mut ledger) = fresh(&tmp);
    let b = claimant(0xB0, 1);

    let s = spec(10, ApprovalMode::RequireApproval, None);
    let id = s.invitation_id;
    let offer = ledger.offer(s, T0).unwrap();
    ledger.claim(&id, &b, T0 + 1).unwrap();
    ledger.revoke(&offer, T0 + 2).unwrap();
    ledger.revoke(&offer, T0 + 3).unwrap();
    assert!(matches!(
        ledger.approve(&offer, &b, T0 + 4),
        Err(LedgerError::Revoked)
    ));
    assert!(matches!(
        ledger.issue(&id, &b, b"x", T0 + 4),
        Err(LedgerError::Revoked)
    ));
    assert!(matches!(
        ledger.claim(&id, &b, T0 + 4),
        Err(LedgerError::Revoked)
    ));

    let s = spec(11, ApprovalMode::RequireApproval, None);
    let denied_id = s.invitation_id;
    let denied = ledger.offer(s, T0).unwrap();
    ledger.claim(&denied_id, &b, T0 + 1).unwrap();
    ledger.deny(&denied, &b, T0 + 2).unwrap();
    // A racing claimant does not take over a denied invitation.
    assert!(matches!(
        ledger.claim(&denied_id, &claimant(0xC0, 1), T0 + 3),
        Err(LedgerError::Denied)
    ));

    let s = spec(12, ApprovalMode::Preauthorized, None);
    let issued_id = s.invitation_id;
    let issued = ledger.offer(s, T0).unwrap();
    ledger.claim(&issued_id, &b, T0 + 1).unwrap();
    assert!(matches!(
        ledger.deny(&issued, &b, T0 + 2),
        Err(LedgerError::NotPending)
    ));
    let receipt = ledger.issue(&issued_id, &b, b"x", T0 + 2).unwrap();
    assert!(matches!(
        ledger.revoke(&issued, T0 + 3),
        Err(LedgerError::AlreadyIssued(r)) if r == receipt
    ));
    drop(ledger);

    let ledger = reopen(&dir);
    assert_eq!(
        ledger.status(&offer).unwrap().state,
        OfferState::Revoked {
            subject: Some(entity(0xB0))
        }
    );
    assert_eq!(
        ledger.status(&denied).unwrap().state,
        OfferState::Denied {
            subject: entity(0xB0)
        }
    );
    assert_eq!(
        ledger.recover(&issued_id, &b, T0 + 4).unwrap().payload,
        b"x"
    );
}

#[test]
fn recovery_window_is_fixed_and_exclusive_then_compaction_retires_state() {
    let tmp = tempfile::tempdir().unwrap();
    let (dir, mut ledger) = fresh(&tmp);
    let b = claimant(0xB0, 1);
    let s = spec(13, ApprovalMode::Preauthorized, None);
    let id = s.invitation_id;
    let offer = ledger.offer(s, T0).unwrap();
    ledger.claim(&id, &b, T0 + 1).unwrap();
    let issued_at = T0 + 2;
    ledger.issue(&id, &b, b"secret", issued_at).unwrap();
    let deadline = issued_at + RECEIPT_RECOVERY_WINDOW_SECS;

    // Recovery after invitation expiry is allowed until the fixed deadline.
    assert!(deadline > T0 + TTL);
    ledger.recover(&id, &b, T0 + TTL + 1).unwrap();
    ledger.recover(&id, &b, deadline - 1).unwrap();
    assert!(matches!(
        ledger.recover(&id, &b, deadline),
        Err(LedgerError::RecoveryClosed)
    ));

    assert_eq!(
        ledger.compact(deadline - 1).unwrap(),
        CompactReport::default()
    );
    assert_eq!(
        ledger.compact(deadline).unwrap(),
        CompactReport {
            payloads_discarded: 1,
            records_removed: 1
        }
    );
    assert!(matches!(
        ledger.status(&offer),
        Err(LedgerError::UnknownOffer)
    ));
    drop(ledger);
    let ledger = reopen(&dir);
    assert!(matches!(
        ledger.recover(&id, &b, T0 + 3),
        Err(LedgerError::UnknownInvitation)
    ));
}

#[test]
fn compaction_keeps_a_spent_tombstone_while_the_invitation_is_unexpired() {
    let tmp = tempfile::tempdir().unwrap();
    let (_dir, mut ledger) = fresh(&tmp);
    let b = claimant(0xB0, 1);
    // A one-hour invitation whose receipt recovery closes before its offer
    // could not; a 48-hour one keeps its tombstone after the payload is gone.
    let long = OfferSpec {
        policy: InvitationPolicy::with_options(
            T0,
            Duration::from_secs(2 * TTL),
            ApprovalMode::Preauthorized,
        )
        .unwrap(),
        ..spec(14, ApprovalMode::Preauthorized, None)
    };
    let id = long.invitation_id;
    let offer = ledger.offer(long, T0).unwrap();
    ledger.claim(&id, &b, T0 + 1).unwrap();
    ledger.issue(&id, &b, b"secret", T0 + 1).unwrap();
    let after = T0 + 1 + RECEIPT_RECOVERY_WINDOW_SECS;
    assert_eq!(
        ledger.compact(after).unwrap(),
        CompactReport {
            payloads_discarded: 1,
            records_removed: 0
        }
    );
    assert!(matches!(
        ledger.status(&offer).unwrap().state,
        OfferState::Issued {
            recoverable: false,
            ..
        }
    ));
    assert!(matches!(
        ledger.recover(&id, &b, T0 + 2),
        Err(LedgerError::RecoveryClosed)
    ));
    assert!(matches!(
        ledger.claim(&id, &claimant(0xC0, 1), after),
        Err(LedgerError::ClaimConflict)
    ));
}

#[test]
fn missing_corrupt_truncated_or_foreign_state_refuses_open() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(matches!(
        EnrollmentLedger::open(
            &tmp.path().join("absent"),
            issuer(),
            LedgerLimits::default()
        ),
        Err(LedgerError::Storage(_))
    ));

    let (dir, mut ledger) = fresh(&tmp);
    ledger
        .offer(spec(15, ApprovalMode::Preauthorized, None), T0)
        .unwrap();
    drop(ledger);
    assert!(matches!(
        EnrollmentLedger::open(&dir, entity(0xBB), LedgerLimits::default()),
        Err(LedgerError::IssuerMismatch)
    ));

    let snapshot = dir.join("enrollment.snapshot");
    let good = std::fs::read(&snapshot).unwrap();
    let mut flipped = good.clone();
    flipped[40] ^= 1;
    let mut future = good.clone();
    future[4] = 2;
    for (bad, expect_version) in [
        (flipped, false),
        (good[..good.len() - 1].to_vec(), false),
        (Vec::new(), false),
        (future, true),
    ] {
        // Rewrite in place so the protected file's owner/ACL are preserved.
        std::fs::write(&snapshot, &bad).unwrap();
        let err = EnrollmentLedger::open(&dir, issuer(), LedgerLimits::default()).unwrap_err();
        if expect_version {
            assert!(matches!(err, LedgerError::UnsupportedVersion), "{err:?}");
        } else {
            assert!(matches!(err, LedgerError::Corrupt), "{err:?}");
        }
    }
    std::fs::write(&snapshot, &good).unwrap();
    assert_eq!(reopen(&dir).statuses().unwrap().len(), 1);
}

#[test]
fn capacity_and_payload_limits_refuse_before_mutation() {
    let tmp = tempfile::tempdir().unwrap();
    let limits = LedgerLimits {
        max_records: 1,
        max_result_bytes: 4,
        ..LedgerLimits::default()
    };
    let mut ledger = EnrollmentLedger::create(&tmp.path().join("l"), issuer(), limits).unwrap();
    let s = spec(16, ApprovalMode::Preauthorized, None);
    let id = s.invitation_id;
    ledger.offer(s.clone(), T0).unwrap();
    assert!(matches!(ledger.offer(s, T0), Err(LedgerError::Duplicate)));
    assert!(matches!(
        ledger.offer(spec(17, ApprovalMode::Preauthorized, None), T0),
        Err(LedgerError::Capacity)
    ));
    let b = claimant(0xB0, 1);
    ledger.claim(&id, &b, T0 + 1).unwrap();
    let rev = ledger.revision();
    for bad in [&b""[..], &b"12345"[..]] {
        assert!(matches!(
            ledger.issue(&id, &b, bad, T0 + 2),
            Err(LedgerError::InvalidResult)
        ));
    }
    assert_eq!(ledger.revision(), rev);
    ledger.issue(&id, &b, b"1234", T0 + 2).unwrap();

    let zero = LedgerLimits {
        max_records: 0,
        ..LedgerLimits::default()
    };
    assert!(matches!(
        EnrollmentLedger::create(&tmp.path().join("z"), issuer(), zero),
        Err(LedgerError::InvalidLimits)
    ));
}

#[test]
fn a_second_owner_is_refused_while_the_ledger_is_open() {
    let tmp = tempfile::tempdir().unwrap();
    let (dir, ledger) = fresh(&tmp);
    assert!(matches!(
        EnrollmentLedger::open(&dir, issuer(), LedgerLimits::default()),
        Err(LedgerError::Storage(_))
    ));
    assert!(matches!(
        EnrollmentLedger::create(&dir, issuer(), LedgerLimits::default()),
        Err(LedgerError::Storage(_))
    ));
    drop(ledger);
    reopen(&dir);
}

#[test]
fn debug_output_never_contains_invitation_ids_or_receipt_payloads() {
    let tmp = tempfile::tempdir().unwrap();
    let (_dir, mut ledger) = fresh(&tmp);
    let s = spec(0x7E, ApprovalMode::Preauthorized, None);
    let id = s.invitation_id;
    ledger.offer(s, T0).unwrap();
    let b = claimant(0xB0, 1);
    ledger.claim(&id, &b, T0 + 1).unwrap();
    ledger.issue(&id, &b, b"PSK-SECRET-MARKER", T0 + 2).unwrap();
    let recovered = ledger.recover(&id, &b, T0 + 3).unwrap();
    let text = format!("{id:?} {recovered:?} {ledger:?}");
    assert!(!text.contains("PSK-SECRET-MARKER"), "{text}");
    assert!(text.contains("<redacted>"), "{text}");
    assert!(!text.contains("126, 126"), "{text}");
}
