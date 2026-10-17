//! Carrying an org's signed revocation material to providers in other orgs.
//!
//! A provider in org B checks a cross-org caller from org A against A's
//! membership floors and A's dispatcher-grant revocations. Both are signed by
//! A's root and arrive at B only if something carries them there; without
//! that, every one of A's floors reads as zero at B ("absent means zero").
//!
//! This module holds the two halves of that carriage:
//!
//! - **Retention.** Every node keeps the signed bundles it has applied, as a
//!   minimal covering set per org ([`RetainedMaterial`]): a bundle stays only
//!   while it is the newest source of some floor the node still enforces, or
//!   of some grant revocation that has not expired, plus the single newest
//!   bundle of each org (its freshness evidence). Superseded bundles are
//!   dropped, so the set is bounded by the floors and revocations themselves,
//!   and further by a byte cap per org.
//! - **Relay.** A caller attaches its org's retained bundles to a cross-org
//!   protected call in the [`ORG_REVOCATIONS_HEADER`], within a byte budget
//!   ([`encode_relay_header`] / [`decode_relay_header`]). The provider verifies
//!   each bundle's signature and merges it before admitting the call.
//!
//! Bundles authenticate themselves (root-signed) and can only raise floors or
//! add revocations, so any carrier may carry them: a caller relaying its own
//! org's material cannot weaken anything. A revoked member simply relays
//! nothing useful; other members of its org, and the org's operator pushing
//! to the provider directly, still deliver. A provider that must not depend on
//! that can require recent material from an org, which turns "nothing
//! arrived" into a refusal instead of a silent pass (see
//! `OrgRevocationState::latest_issued_at`).

use std::collections::{BTreeMap, BTreeSet};

use super::org::{OrgError, OrgId, OrgRevocationBundle};
use super::org_grant_revocation::{revocation_entry_expired, OrgGrantRevocationBundle};
use super::org_revocation::{OrgRevocationState, RevokedGrantKey};
use crate::adapter::net::identity::EntityId;

/// The request header a cross-org caller relays its org's revocation material
/// in. Stripped before the handler runs, like the admission header.
pub const ORG_REVOCATIONS_HEADER: &str = "net-org-revocations";

/// Most bytes one relay header may carry (the whole header value). One nRPC
/// request rides one packet, so the relay shares that budget with the proof.
pub const MAX_RELAY_HEADER_BYTES: usize = 4 * 1024;

/// Most bundles one relay header may carry.
pub const MAX_RELAY_ITEMS: usize = 16;

/// Byte cap on the retained material of one org. Past it the oldest
/// retained bundles go first (never the newest), and the drop is logged.
pub const MAX_RETAINED_BYTES_PER_ORG: usize = 64 * 1024;

const MATERIAL_DIGEST_CONTEXT: &str = "net-org-revocation-material-v1";

/// Which signed object a [`RevocationMaterial`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum RevocationMaterialKind {
    /// An `OrgRevocationBundle` (membership floors).
    Floors = 0x01,
    /// An `OrgGrantRevocationBundle`.
    Grants = 0x02,
}

impl RevocationMaterialKind {
    fn from_tag(tag: u8) -> Option<Self> {
        match tag {
            0x01 => Some(Self::Floors),
            0x02 => Some(Self::Grants),
            _ => None,
        }
    }
}

/// One signed revocation bundle, carried verbatim.
#[derive(Clone, PartialEq, Eq)]
pub struct RevocationMaterial {
    /// Which bundle type `bytes` holds.
    pub kind: RevocationMaterialKind,
    /// The bundle's canonical wire form.
    pub bytes: Vec<u8>,
}

impl std::fmt::Debug for RevocationMaterial {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RevocationMaterial")
            .field("kind", &self.kind)
            .field("bytes", &self.bytes.len())
            .finish()
    }
}

/// A [`RevocationMaterial`] decoded and signature-checked.
#[derive(Debug, Clone)]
pub enum VerifiedMaterial {
    /// A verified floor bundle.
    Floors(OrgRevocationBundle),
    /// A verified grant-revocation bundle.
    Grants(OrgGrantRevocationBundle),
}

impl VerifiedMaterial {
    /// The org that signed it.
    pub fn org_id(&self) -> OrgId {
        match self {
            Self::Floors(b) => b.org_id,
            Self::Grants(b) => b.org_id,
        }
    }

    /// When it was issued (unix seconds, as signed).
    pub fn issued_at(&self) -> u64 {
        match self {
            Self::Floors(b) => b.issued_at,
            Self::Grants(b) => b.issued_at,
        }
    }
}

impl RevocationMaterial {
    /// Wrap a floor bundle.
    pub fn floors(bundle: &OrgRevocationBundle) -> Self {
        Self {
            kind: RevocationMaterialKind::Floors,
            bytes: bundle.to_bytes(),
        }
    }

    /// Wrap a grant-revocation bundle.
    pub fn grants(bundle: &OrgGrantRevocationBundle) -> Self {
        Self {
            kind: RevocationMaterialKind::Grants,
            bytes: bundle.to_bytes(),
        }
    }

    /// Identity of this exact bundle (kind and bytes), for de-duplication.
    pub fn digest(&self) -> [u8; 32] {
        let mut hasher = blake3::Hasher::new_derive_key(MATERIAL_DIGEST_CONTEXT);
        hasher.update(&[self.kind as u8]);
        hasher.update(&self.bytes);
        *hasher.finalize().as_bytes()
    }

    /// The org the bundle names as its signer, from a strict decode with
    /// NO signature check: a cheap filter before the signature work (the
    /// org id is signed, so [`Self::verify`] still decides). `None` when
    /// the bytes do not decode.
    pub fn claimed_org(&self) -> Option<OrgId> {
        match self.kind {
            RevocationMaterialKind::Floors => OrgRevocationBundle::from_bytes(&self.bytes)
                .ok()
                .map(|b| b.org_id),
            RevocationMaterialKind::Grants => OrgGrantRevocationBundle::from_bytes(&self.bytes)
                .ok()
                .map(|b| b.org_id),
        }
    }

    /// Decode strictly and verify the org signature.
    pub fn verify(&self) -> Result<VerifiedMaterial, OrgError> {
        match self.kind {
            RevocationMaterialKind::Floors => {
                let bundle = OrgRevocationBundle::from_bytes(&self.bytes)?;
                bundle.verify()?;
                Ok(VerifiedMaterial::Floors(bundle))
            }
            RevocationMaterialKind::Grants => {
                let bundle = OrgGrantRevocationBundle::from_bytes(&self.bytes)?;
                bundle.verify()?;
                Ok(VerifiedMaterial::Grants(bundle))
            }
        }
    }

    fn encoded_len(&self) -> usize {
        1 + 4 + self.bytes.len()
    }
}

/// Encode relay items as one header value: `kind (1) ‖ len (4, LE) ‖ bytes`,
/// repeated. Items that do not fit in `budget` (or past
/// [`MAX_RELAY_ITEMS`]) are left out, in order: callers pass the most
/// important first. Returns `None` when nothing fits.
pub fn encode_relay_header(items: &[RevocationMaterial], budget: usize) -> Option<Vec<u8>> {
    let budget = budget.min(MAX_RELAY_HEADER_BYTES);
    let mut out = Vec::new();
    let mut count = 0;
    for item in items {
        if count == MAX_RELAY_ITEMS || out.len() + item.encoded_len() > budget {
            continue;
        }
        out.push(item.kind as u8);
        out.extend_from_slice(&(item.bytes.len() as u32).to_le_bytes());
        out.extend_from_slice(&item.bytes);
        count += 1;
    }
    (!out.is_empty()).then_some(out)
}

/// Strictly decode a relay header value. Signatures are NOT checked here.
/// A malformed value is refused whole: relay is best-effort, so the caller
/// ignores it rather than guessing at a prefix.
pub fn decode_relay_header(value: &[u8]) -> Option<Vec<RevocationMaterial>> {
    if value.len() > MAX_RELAY_HEADER_BYTES {
        return None;
    }
    let mut items = Vec::new();
    let mut rest = value;
    while !rest.is_empty() {
        if items.len() == MAX_RELAY_ITEMS || rest.len() < 5 {
            return None;
        }
        let kind = RevocationMaterialKind::from_tag(rest[0])?;
        let len = u32::from_le_bytes([rest[1], rest[2], rest[3], rest[4]]) as usize;
        let body = rest.get(5..5usize.checked_add(len)?)?;
        items.push(RevocationMaterial {
            kind,
            bytes: body.to_vec(),
        });
        rest = &rest[5 + len..];
    }
    Some(items)
}

/// One retained bundle and what it contributes.
#[derive(Clone)]
struct Retained {
    material: RevocationMaterial,
    /// `material.digest()`, computed once.
    digest: [u8; 32],
    issued_at: u64,
    /// Insertion order, the tie-break between equal `issued_at`.
    seq: u64,
    floors: Vec<(EntityId, u32)>,
    grants: Vec<(RevokedGrantKey, u64)>,
}

/// The signed bundles a node keeps for relay, as a minimal covering set per
/// org. Not security state: losing it only means relaying less, so it is
/// persisted best-effort beside the revocation state.
#[derive(Default, Clone)]
pub struct RetainedMaterial {
    by_org: BTreeMap<OrgId, Vec<Retained>>,
    next_seq: u64,
}

impl std::fmt::Debug for RetainedMaterial {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_map()
            .entries(self.by_org.iter().map(|(org, r)| (org, r.len())))
            .finish()
    }
}

impl RetainedMaterial {
    /// Keep a VERIFIED bundle the node has just applied, then prune the
    /// org's set against `view` (the state after the apply). Returns whether
    /// the retained set changed.
    pub fn retain(
        &mut self,
        material: &RevocationMaterial,
        verified: &VerifiedMaterial,
        view: &OrgRevocationState,
        now_secs: u64,
    ) -> bool {
        let org = verified.org_id();
        let digest = material.digest();
        let entries = self.by_org.entry(org).or_default();
        if entries.iter().any(|r| r.digest == digest) {
            return false;
        }
        let (floors, grants) = match verified {
            VerifiedMaterial::Floors(b) => (b.floors().to_vec(), Vec::new()),
            VerifiedMaterial::Grants(b) => (
                Vec::new(),
                b.entries()
                    .iter()
                    .map(|e| ((b.org_id, e.kind, e.grant_id), e.not_after))
                    .collect(),
            ),
        };
        let seq = self.next_seq;
        self.next_seq += 1;
        entries.push(Retained {
            material: material.clone(),
            digest,
            issued_at: verified.issued_at(),
            seq,
            floors,
            grants,
        });
        self.prune(&org, view, now_secs);
        true
    }

    /// Re-run the covering-set rule for `org` against `view`.
    fn prune(&mut self, org: &OrgId, view: &OrgRevocationState, now_secs: u64) {
        let Some(entries) = self.by_org.get_mut(org) else {
            return;
        };
        // Newest first.
        entries.sort_by_key(|r| std::cmp::Reverse((r.issued_at, r.seq)));
        let mut provided_floors: BTreeSet<(EntityId, u32)> = BTreeSet::new();
        let mut provided_grants: BTreeSet<RevokedGrantKey> = BTreeSet::new();
        let mut kept = Vec::with_capacity(entries.len());
        // The freshness evidence is the newest bundle NOT dated in the
        // future (see `latest_issued_at`); keep it even when a future-dated
        // one sorts first.
        let ceiling =
            now_secs.saturating_add(crate::adapter::net::identity::MAX_TOKEN_CLOCK_SKEW_SECS);
        let freshness_index = entries.iter().position(|r| r.issued_at <= ceiling);
        for (index, r) in entries.drain(..).enumerate() {
            let is_freshness = Some(index) == freshness_index;
            let needed_floor = r.floors.iter().any(|(member, floor)| {
                *floor > 0
                    && view.floor_for(org, member) == *floor
                    && !provided_floors.contains(&(member.clone(), *floor))
            });
            let needed_grant = r.grants.iter().any(|(key, not_after)| {
                !revocation_entry_expired(*not_after, now_secs)
                    && view.is_grant_revoked(&key.0, key.1, &key.2)
                    && !provided_grants.contains(key)
            });
            if index == 0 || is_freshness || needed_floor || needed_grant {
                provided_floors.extend(r.floors.iter().cloned());
                provided_grants.extend(r.grants.iter().map(|(k, _)| *k));
                kept.push(r);
            }
        }
        // Byte cap: drop from the oldest end, never the newest.
        let mut total: usize = kept.iter().map(|r| r.material.bytes.len()).sum();
        while total > MAX_RETAINED_BYTES_PER_ORG && kept.len() > 1 {
            if let Some(dropped) = kept.pop() {
                total -= dropped.material.bytes.len();
                tracing::warn!(
                    org = %org,
                    "org revocation relay: retained material is over its byte cap; \
                     dropped the oldest bundle from relay (enforcement is unaffected; \
                     push that org's bundles to its providers directly)",
                );
            }
        }
        *entries = kept;
        if entries.is_empty() {
            self.by_org.remove(org);
        }
    }

    /// The bundles to relay for `org`, newest first.
    pub fn for_org(&self, org: &OrgId) -> Vec<RevocationMaterial> {
        self.by_org
            .get(org)
            .map(|v| v.iter().map(|r| r.material.clone()).collect())
            .unwrap_or_default()
    }

    /// The relay set identity of `org`'s retained bundles: the same value
    /// [`relay_set_digest`] gives over [`Self::for_org`], from the cached
    /// per-bundle digests, without copying any bundle.
    pub fn set_digest_for(&self, org: &OrgId) -> Option<[u8; 32]> {
        let entries = self.by_org.get(org)?;
        Some(set_digest_of(entries.iter().map(|r| &r.digest)))
    }

    /// Whether a bundle with this [`RevocationMaterial::digest`] is
    /// retained.
    pub fn contains(&self, digest: &[u8; 32]) -> bool {
        self.by_org
            .values()
            .flat_map(|v| v.iter())
            .any(|r| r.digest == *digest)
    }

    /// The newest signed `issued_at` retained for `org`, ignoring any
    /// bundle dated beyond `now_secs` plus the skew ceiling: a future-dated
    /// bundle must not read as fresh forever.
    pub fn latest_issued_at(&self, org: &OrgId, now_secs: u64) -> Option<u64> {
        let ceiling =
            now_secs.saturating_add(crate::adapter::net::identity::MAX_TOKEN_CLOCK_SKEW_SECS);
        self.by_org
            .get(org)?
            .iter()
            .map(|r| r.issued_at)
            .filter(|t| *t <= ceiling)
            .max()
    }

    /// Number of retained bundles across all orgs.
    pub fn len(&self) -> usize {
        self.by_org.values().map(Vec::len).sum()
    }

    /// `true` iff nothing is retained.
    pub fn is_empty(&self) -> bool {
        self.by_org.is_empty()
    }

    /// Serialize for the best-effort sidecar: every retained bundle as
    /// `{kind, hex}`, newest first per org.
    pub fn to_file_bytes(&self) -> Vec<u8> {
        let items: Vec<serde_json::Value> = self
            .by_org
            .values()
            .flat_map(|v| v.iter())
            .map(|r| {
                serde_json::json!({
                    "kind": r.material.kind as u8,
                    "bundle": hex::encode(&r.material.bytes),
                })
            })
            .collect();
        serde_json::to_vec_pretty(&serde_json::json!({ "version": 1, "bundles": items }))
            .unwrap_or_default()
    }

    /// Rebuild from the sidecar, re-verifying every bundle and re-running the
    /// covering rule against `view`. Anything unreadable is skipped: this is
    /// a cache, and a bad entry must not stop the node.
    pub fn from_file_bytes(bytes: &[u8], view: &OrgRevocationState, now_secs: u64) -> Self {
        let mut out = Self::default();
        for material in Self::parse_file_items(bytes) {
            if let Ok(verified) = material.verify() {
                out.retain(&material, &verified, view, now_secs);
            }
        }
        out
    }

    /// The bundles a sidecar holds, oldest first (so retaining them in
    /// order matches issue order). Signatures are NOT checked; anything
    /// unreadable is skipped.
    pub fn parse_file_items(bytes: &[u8]) -> Vec<RevocationMaterial> {
        let Ok(doc) = serde_json::from_slice::<serde_json::Value>(bytes) else {
            return Vec::new();
        };
        let Some(items) = doc.get("bundles").and_then(|v| v.as_array()) else {
            return Vec::new();
        };
        items
            .iter()
            .rev()
            .filter_map(|item| {
                let kind = item
                    .get("kind")
                    .and_then(|v| v.as_u64())
                    .and_then(|k| u8::try_from(k).ok())
                    .and_then(RevocationMaterialKind::from_tag)?;
                let bytes = item
                    .get("bundle")
                    .and_then(|v| v.as_str())
                    .and_then(|h| hex::decode(h).ok())?;
                Some(RevocationMaterial { kind, bytes })
            })
            .collect()
    }
}

/// How often a caller re-relays an unchanged material set to the same
/// provider. Relay is fire-and-forget (the provider never acknowledges),
/// so a periodic resend covers a lost request or a provider restart.
pub const RELAY_RESEND_INTERVAL: std::time::Duration = std::time::Duration::from_secs(300);

/// Most floors / grant revocations a FOREIGN org may hold on a provider.
/// A grantee org signs its own bundles, so without a cap it could grow a
/// provider's persisted state without bound under made-up member ids.
pub const MAX_FOREIGN_ORG_ENTRIES: usize = 4_096;

const SEEN_CAPACITY: usize = 4_096;
const SENT_CAPACITY: usize = 4_096;

/// Most relayed-material ingests a node runs off the RPC path at once.
/// Past it a relay is dropped (and forgotten, so a later one retries):
/// relay is best-effort, and the bound keeps a flood of relay headers from
/// queueing unbounded signature checks and persists on the blocking pool.
pub const MAX_BACKGROUND_INGESTS: usize = 2;

/// When a relay set last went to one provider: (set digest, when).
type SentRelay = ([u8; 32], std::time::Instant);

/// Per-node relay bookkeeping: the provider's freshness policy, which
/// relayed bundles it has already ingested, when it last relayed to each
/// provider, and the default-permissive warning state.
#[derive(Default)]
pub struct NodeRelayState {
    /// Acting org → most seconds its newest revocation material may age.
    freshness: parking_lot::RwLock<BTreeMap<OrgId, u64>>,
    /// Digests of relayed bundles already handled (bounded; cleared whole
    /// when full, which only costs a re-verify).
    seen: parking_lot::Mutex<std::collections::HashSet<[u8; 32]>>,
    /// Provider entity → (digest of the set last relayed, when).
    sent: parking_lot::Mutex<std::collections::HashMap<[u8; 32], SentRelay>>,
    /// Foreign orgs already warned about (admitted with no material).
    warned: parking_lot::Mutex<BTreeSet<OrgId>>,
    /// Cross-org admissions where this provider held no revocation material
    /// at all from the acting org and no freshness policy required it.
    unverified_admissions: std::sync::atomic::AtomicU64,
    /// Relayed or pushed bundles refused (bad signature, unaccepted org,
    /// over a cap, or a failed apply).
    refused: std::sync::atomic::AtomicU64,
    /// Background relay ingests running now (see
    /// [`MAX_BACKGROUND_INGESTS`]).
    ingests_in_flight: std::sync::atomic::AtomicUsize,
}

impl std::fmt::Debug for NodeRelayState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NodeRelayState")
            .field("freshness", &self.freshness.read().len())
            .finish()
    }
}

impl NodeRelayState {
    /// Require (`Some`) or stop requiring (`None`) recent material from
    /// `org`.
    pub fn set_freshness(&self, org: OrgId, max_age_secs: Option<u64>) {
        let mut policy = self.freshness.write();
        match max_age_secs {
            Some(age) => {
                policy.insert(org, age);
            }
            None => {
                policy.remove(&org);
            }
        }
    }

    /// The current policy.
    pub fn freshness(&self) -> BTreeMap<OrgId, u64> {
        self.freshness.read().clone()
    }

    /// The most seconds `org`'s newest material may age, if the policy
    /// names it.
    pub fn freshness_for(&self, org: &OrgId) -> Option<u64> {
        self.freshness.read().get(org).copied()
    }

    /// Whether `org` is named in the policy.
    pub fn has_freshness_policy(&self, org: &OrgId) -> bool {
        self.freshness.read().contains_key(org)
    }

    /// Record `digest` as handled; `false` if it already was.
    pub fn first_sighting(&self, digest: [u8; 32]) -> bool {
        let mut seen = self.seen.lock();
        if seen.contains(&digest) {
            return false;
        }
        if seen.len() >= SEEN_CAPACITY {
            seen.clear();
        }
        seen.insert(digest);
        true
    }

    /// Forget a digest, so a bundle whose apply failed is tried again.
    pub fn forget(&self, digest: &[u8; 32]) {
        self.seen.lock().remove(digest);
    }

    /// Whether `set_digest` is due to go to `provider`: it is not the set
    /// last sent there, or that send is older than
    /// [`RELAY_RESEND_INTERVAL`]. Records nothing; a caller that does send
    /// it calls [`Self::mark_relayed`] once the request is out, so a relay
    /// that was dropped or never sent stays due.
    pub fn relay_due(&self, provider: &EntityId, set_digest: [u8; 32]) -> bool {
        let sent = self.sent.lock();
        match sent.get(provider.as_bytes()) {
            Some((digest, at)) => *digest != set_digest || at.elapsed() >= RELAY_RESEND_INTERVAL,
            None => true,
        }
    }

    /// Record that `set_digest` went to `provider` just now.
    pub fn mark_relayed(&self, provider: &EntityId, set_digest: [u8; 32]) {
        let mut sent = self.sent.lock();
        if sent.len() >= SENT_CAPACITY {
            sent.clear();
        }
        sent.insert(
            *provider.as_bytes(),
            (set_digest, std::time::Instant::now()),
        );
    }

    /// Claim one of the [`MAX_BACKGROUND_INGESTS`] slots; `false` when all
    /// are taken. A `true` must be paired with [`Self::end_ingest`].
    pub fn begin_ingest(&self) -> bool {
        use std::sync::atomic::Ordering;
        self.ingests_in_flight
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < MAX_BACKGROUND_INGESTS).then_some(n + 1)
            })
            .is_ok()
    }

    /// Release a slot taken by [`Self::begin_ingest`].
    pub fn end_ingest(&self) {
        self.ingests_in_flight
            .fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    }

    /// Count (and, once per org, log) a cross-org admission made with no
    /// revocation material from the acting org.
    pub fn note_unverified_admission(&self, org: &OrgId) {
        self.unverified_admissions
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if self.warned.lock().insert(*org) {
            tracing::warn!(
                org = %org,
                "admitting a cross-org caller with NO revocation material from its org: \
                 that org's membership floors and dispatcher-grant revocations cannot \
                 apply here until its bundles arrive (relayed by its members or pushed \
                 by its operator). Require recent material with \
                 require_fresh_org_revocations to refuse instead.",
            );
        }
    }

    /// See [`Self::note_unverified_admission`].
    pub fn unverified_admissions(&self) -> u64 {
        self.unverified_admissions
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Count a refused relayed or pushed bundle.
    pub fn note_refused(&self) {
        self.refused
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// Relayed or pushed bundles refused so far.
    pub fn refused(&self) -> u64 {
        self.refused.load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// Identity of a relay set (the bundles' digests, in order).
pub fn relay_set_digest(items: &[RevocationMaterial]) -> [u8; 32] {
    let digests: Vec<[u8; 32]> = items.iter().map(RevocationMaterial::digest).collect();
    set_digest_of(digests.iter())
}

fn set_digest_of<'a>(digests: impl Iterator<Item = &'a [u8; 32]>) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new_derive_key(MATERIAL_DIGEST_CONTEXT);
    for digest in digests {
        hasher.update(digest);
    }
    *hasher.finalize().as_bytes()
}

/// Why a node would not apply a bundle from the network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MaterialRefusal {
    /// The bundle does not decode or its signature does not verify.
    Invalid(OrgError),
    /// This node has no installed authority or revocation store.
    NoAuthority,
    /// The signing org is not one this node takes revocations from: not
    /// its own org, not an org it has granted to, and not named in its
    /// freshness policy.
    NotAccepted(OrgId),
    /// Applying it would put a foreign org over [`MAX_FOREIGN_ORG_ENTRIES`].
    OverCap(OrgId),
    /// The store refused or failed the apply.
    Store(String),
}

impl std::fmt::Display for MaterialRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(e) => write!(f, "revocation material does not verify: {e}"),
            Self::NoAuthority => f.write_str("no org authority is installed on this node"),
            Self::NotAccepted(org) => write!(
                f,
                "this node takes no revocation material from org {org} (not its own \
                 org, not a grantee, not in its freshness policy)"
            ),
            Self::OverCap(org) => write!(
                f,
                "org {org} would exceed {MAX_FOREIGN_ORG_ENTRIES} revocation entries here"
            ),
            Self::Store(e) => write!(f, "revocation store: {e}"),
        }
    }
}

impl std::error::Error for MaterialRefusal {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::behavior::org::OrgKeypair;
    use crate::adapter::net::behavior::org_grant_revocation::{RevokedGrant, RevokedGrantKind};

    fn org() -> OrgKeypair {
        OrgKeypair::from_bytes([0x71u8; 32])
    }

    fn member(byte: u8) -> EntityId {
        EntityId::from_bytes([byte; 32])
    }

    fn floor_bundle(floors: &[(u8, u32)], issued_at: u64) -> OrgRevocationBundle {
        let map = floors.iter().map(|(m, f)| (member(*m), *f)).collect();
        OrgRevocationBundle::issue_at(&org(), &map, issued_at).expect("bundle")
    }

    fn apply(
        retained: &mut RetainedMaterial,
        view: &mut OrgRevocationState,
        bundle: &OrgRevocationBundle,
    ) {
        view.merge_bundle(bundle);
        let material = RevocationMaterial::floors(bundle);
        let verified = material.verify().expect("verify");
        retained.retain(&material, &verified, view, 1_000);
    }

    #[test]
    fn the_header_round_trips_and_respects_its_budget() {
        let a = RevocationMaterial::floors(&floor_bundle(&[(1, 2)], 10));
        let b = RevocationMaterial::floors(&floor_bundle(&[(2, 3)], 11));
        let both = encode_relay_header(&[a.clone(), b.clone()], 4096).expect("fits");
        assert_eq!(
            decode_relay_header(&both).expect("decode"),
            vec![a.clone(), b]
        );
        let one = encode_relay_header(std::slice::from_ref(&a), 4096).expect("fits");
        assert!(encode_relay_header(std::slice::from_ref(&a), one.len() - 1).is_none());
        let mut truncated = both.clone();
        truncated.pop();
        assert!(decode_relay_header(&truncated).is_none());
        let mut unknown = one;
        unknown[0] = 0x09;
        assert!(decode_relay_header(&unknown).is_none());
    }

    /// A heartbeat (an empty bundle) must not displace the bundle that still
    /// carries a live floor: relay would otherwise stop carrying a removal
    /// the moment the org next proved itself fresh.
    #[test]
    fn a_newer_heartbeat_keeps_the_bundle_that_carries_a_live_floor() {
        let mut retained = RetainedMaterial::default();
        let mut view = OrgRevocationState::empty();
        apply(&mut retained, &mut view, &floor_bundle(&[(1, 4)], 100));
        apply(&mut retained, &mut view, &floor_bundle(&[], 200));
        let kept = retained.for_org(&org().org_id());
        assert_eq!(kept.len(), 2, "heartbeat (freshness) + the floor's source");
        assert_eq!(kept[0], RevocationMaterial::floors(&floor_bundle(&[], 200)));
    }

    /// A bundle whose every floor has been superseded by a later, higher
    /// one is dropped; the set tracks what is enforced, not history.
    #[test]
    fn a_superseded_bundle_is_dropped() {
        let mut retained = RetainedMaterial::default();
        let mut view = OrgRevocationState::empty();
        apply(&mut retained, &mut view, &floor_bundle(&[(1, 4)], 100));
        apply(&mut retained, &mut view, &floor_bundle(&[(1, 6)], 200));
        assert_eq!(retained.for_org(&org().org_id()).len(), 1);
        // A bundle still providing a DIFFERENT member's floor stays.
        apply(&mut retained, &mut view, &floor_bundle(&[(2, 3)], 300));
        assert_eq!(retained.for_org(&org().org_id()).len(), 2);
    }

    #[test]
    fn expired_grant_revocations_stop_holding_their_bundle() {
        let mut retained = RetainedMaterial::default();
        let mut view = OrgRevocationState::empty();
        let grant = OrgGrantRevocationBundle::issue_at(
            &org(),
            [RevokedGrant {
                kind: RevokedGrantKind::Dispatcher,
                grant_id: [5u8; 32],
                not_after: 2_000,
            }],
            100,
        )
        .expect("bundle");
        view.merge_grant_revocations(&grant, 1_000);
        let material = RevocationMaterial::grants(&grant);
        let verified = material.verify().expect("verify");
        retained.retain(&material, &verified, &view, 1_000);
        apply(&mut retained, &mut view, &floor_bundle(&[], 200));
        assert_eq!(retained.for_org(&org().org_id()).len(), 2);

        // Long after the grant expired, a new heartbeat prunes it.
        let late = 2_000 + crate::adapter::net::identity::MAX_TOKEN_CLOCK_SKEW_SECS + 1;
        view.prune_expired_grants(late);
        let beat = floor_bundle(&[], 300);
        view.merge_bundle(&beat);
        let material = RevocationMaterial::floors(&beat);
        let verified = material.verify().expect("verify");
        retained.retain(&material, &verified, &view, late);
        assert_eq!(retained.for_org(&org().org_id()).len(), 1);
    }

    #[test]
    fn the_sidecar_round_trips_and_reverifies() {
        let mut retained = RetainedMaterial::default();
        let mut view = OrgRevocationState::empty();
        apply(&mut retained, &mut view, &floor_bundle(&[(1, 4)], 100));
        apply(&mut retained, &mut view, &floor_bundle(&[], 200));
        let bytes = retained.to_file_bytes();
        let back = RetainedMaterial::from_file_bytes(&bytes, &view, 1_000);
        assert_eq!(
            back.for_org(&org().org_id()),
            retained.for_org(&org().org_id())
        );
        // A tampered entry is skipped, not trusted.
        let text = String::from_utf8(bytes).expect("utf8");
        let tampered = text.replacen("\"bundle\": \"", "\"bundle\": \"ff", 1);
        let back = RetainedMaterial::from_file_bytes(tampered.as_bytes(), &view, 1_000);
        assert_eq!(back.len(), 1);
    }
}
