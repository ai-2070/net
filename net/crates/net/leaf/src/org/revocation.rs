//! The verify-time revocation view: what the admission engine
//! actually reads from core's filesystem-backed revocation store.
//!
//! Core's `org_revocation.rs` is NOT portable (a `std::fs`
//! interprocess store with `Condvar`s) and stays native. What the
//! admission engine reads at verify time is exactly one query — the
//! merged raise-only per-member floor maxima — plus the two store
//! health facts the §9.5 stability hook compares. The leaf is FED
//! those over its control channel as [`RevocationFacts`]; the merge
//! rule is ported verbatim: raise-only (a lower floor never rolls
//! back a higher one) and floor 0 is skipped (it is the "no floor"
//! default, not a revocation).

use std::collections::BTreeMap;

use crate::org::cert::OrgId;
use crate::org::entity::EntityId;
use crate::org::grant_revocation::{
    revocation_entry_expired, OrgGrantRevocationBundle, RevokedGrantKind,
};

/// A revoked grant as the facts key it: the issuing org, the grant
/// family, and the revocation id.
pub type RevokedGrantKey = (OrgId, RevokedGrantKind, [u8; 32]);

/// The revocation facts one admission verifies against — the leaf's
/// stand-in for core's `OrgRevocationState` snapshot.
///
/// Fed from the control plane (verified [`crate::org::cert::OrgRevocationBundle`]s
/// merged in via [`Self::merge_floors`]): merged raise-only maxima,
/// the publish epoch, and store health.
#[derive(Debug, Clone, Default)]
pub struct RevocationFacts {
    /// Merged raise-only maxima keyed `(org, member)`; a missing key
    /// means floor 0 — every generation admissible.
    pub floors: BTreeMap<(OrgId, EntityId), u32>,
    /// Grants revoked by their issuing org, with each revoked grant's
    /// `not_after` (entries are dropped once that has passed).
    pub revoked_grants: BTreeMap<RevokedGrantKey, u64>,
    /// Publish epoch (core's `BarrieredGeneration::get`); bumps on
    /// every raise — `stability_recheck` uses it to detect "floor
    /// rose mid-admission".
    pub epoch: u64,
    /// Core's `OrgRevocationStore::is_poisoned()` — store health.
    /// `true` ⇒ deny `ProviderAuthorityUnavailable` / fail
    /// `stability_recheck`.
    pub poisoned: bool,
}

impl RevocationFacts {
    /// "Every cert `member` holds from `org` below this generation
    /// is revoked." Absent key ⇒ 0 ⇒ every generation admissible.
    pub fn floor_for(&self, org: &OrgId, member: &EntityId) -> u32 {
        self.floors
            .get(&(*org, member.clone()))
            .copied()
            .unwrap_or(0)
    }

    /// `true` iff `issuer` revoked the `kind` grant named `grant_id`.
    pub fn is_grant_revoked(
        &self,
        issuer: &OrgId,
        kind: RevokedGrantKind,
        grant_id: &[u8; 32],
    ) -> bool {
        self.revoked_grants
            .contains_key(&(*issuer, kind, *grant_id))
    }

    /// Merge a VERIFIED grant-revocation bundle (mirrors core's
    /// `OrgRevocationState::merge_grant_revocations`): entries already
    /// expired at `now_secs` are skipped, a `not_after` past the longest
    /// lifetime any genuine grant can have is clamped to it, a present
    /// entry keeps the later `not_after`, and expired entries are pruned.
    /// Returns how many entries were newly added; the epoch bumps once if
    /// any were.
    pub fn merge_grant_revocations(
        &mut self,
        bundle: &OrgGrantRevocationBundle,
        now_secs: u64,
    ) -> usize {
        // Core's clamp: no genuine grant is valid past now + the grant TTL
        // ceiling (plus skew), so a later `not_after` revokes exactly as
        // much once clamped but can no longer pin the entry indefinitely.
        let horizon = now_secs
            .saturating_add(crate::org::grant::MAX_ORG_GRANT_TTL_SECS)
            .saturating_add(crate::org::MAX_TOKEN_CLOCK_SKEW_SECS);
        let mut added = 0;
        for entry in bundle.entries() {
            if revocation_entry_expired(entry.not_after, now_secs) {
                continue;
            }
            let entry_not_after = entry.not_after.min(horizon);
            let key = (bundle.org_id, entry.kind, entry.grant_id);
            match self.revoked_grants.get_mut(&key) {
                Some(not_after) => *not_after = (*not_after).max(entry_not_after),
                None => {
                    self.revoked_grants.insert(key, entry_not_after);
                    added += 1;
                }
            }
        }
        self.revoked_grants
            .retain(|_, not_after| !revocation_entry_expired(*not_after, now_secs));
        if added > 0 {
            self.epoch = self.epoch.saturating_add(1);
        }
        added
    }

    /// Raise-only merge (mirrors core's
    /// `OrgRevocationState::merge_bundle`): floor 0 entries are
    /// skipped, a strictly-higher floor raises the stored maximum,
    /// lower floors never roll back. Returns the count of raised
    /// floors; the epoch bumps once if anything raised — the publish
    /// generation advances with every raise, which is exactly what
    /// `stability_recheck` watches.
    pub fn merge_floors(&mut self, org: OrgId, floors: &[(EntityId, u32)]) -> usize {
        let mut raised = 0;
        for (member, floor) in floors {
            if *floor == 0 {
                continue;
            }
            let entry = self.floors.entry((org, member.clone())).or_insert(0);
            if *floor > *entry {
                *entry = *floor;
                raised += 1;
            }
        }
        if raised > 0 {
            self.epoch = self.epoch.saturating_add(1);
        }
        raised
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::org::cert::OrgKeypair;
    use crate::org::grant_revocation::RevokedGrant;

    /// A `not_after` far past any genuine grant's lifetime is clamped the
    /// way core clamps it, so the entry expires on core's schedule instead
    /// of staying in the leaf's memory forever.
    #[test]
    fn an_oversized_not_after_is_clamped_like_core() {
        let now = 1_000_000;
        let bundle = OrgGrantRevocationBundle::issue_at(
            &OrgKeypair::from_bytes([0x5au8; 32]),
            [RevokedGrant {
                kind: RevokedGrantKind::Capability,
                grant_id: [0x01u8; 32],
                not_after: u64::MAX,
            }],
            now,
        )
        .expect("bundle");
        let mut facts = RevocationFacts::default();
        assert_eq!(facts.merge_grant_revocations(&bundle, now), 1);
        let horizon =
            now + crate::org::grant::MAX_ORG_GRANT_TTL_SECS + crate::org::MAX_TOKEN_CLOCK_SKEW_SECS;
        let stored = facts
            .revoked_grants
            .values()
            .copied()
            .next()
            .expect("entry");
        assert_eq!(stored, horizon);
        // Re-merging the same bundle later never extends it past the
        // horizon computed then, and once that horizon has passed the
        // entry is pruned.
        let later = horizon + crate::org::MAX_TOKEN_CLOCK_SKEW_SECS + 1;
        let empty =
            OrgGrantRevocationBundle::issue_at(&OrgKeypair::from_bytes([0x5au8; 32]), [], later)
                .expect("empty bundle");
        facts.merge_grant_revocations(&empty, later);
        assert!(facts.revoked_grants.is_empty());
    }
}
