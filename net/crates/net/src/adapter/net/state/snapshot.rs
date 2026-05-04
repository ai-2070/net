//! State snapshots for daemon migration and catchup.
//!
//! A snapshot captures an entity's accumulated state at a point in the
//! causal chain. New nodes receive the snapshot + replay events after it,
//! avoiding full log replay.
//!
//! # Wire versioning
//!
//! v0 was the pre-identity-migration layout: a bare header + state
//! payload, no hint of which version the decoder is looking at. v1
//! (introduced by `DAEMON_IDENTITY_MIGRATION_PLAN.md` + shared with
//! `DAEMON_CHANNEL_REBIND_PLAN.md`) prepends a 4-byte magic +
//! version byte so readers can unambiguously distinguish the two
//! and so future bumps can introduce new trailing fields without a
//! guessing game. v1 readers still decode v0 bytes for rolling-
//! upgrade compatibility: v0 content is surfaced with empty
//! `bindings_bytes` + `identity_envelope: None`, the same defaults
//! a fresh v1 snapshot with no extras would produce. Writers always
//! emit v1.

use bytes::{Buf, Bytes};

use super::causal::{CausalLink, CAUSAL_LINK_SIZE};
use super::horizon::ObservedHorizon;
use crate::adapter::net::identity::{
    EntityId, EntityKeypair, IdentityEnvelope, IDENTITY_ENVELOPE_SIZE,
};

/// 4-byte magic prefix for v1 snapshots. v0's first 4 bytes are the
/// first 32 bytes of an `EntityId` (arbitrary); this ASCII marker is
/// a ~1/2^32 collision with any given v0 snapshot and lets the
/// decoder branch unambiguously. `CDS` = *Compute-Daemon Snapshot*;
/// the `1` is the version digit, bumped when an on-wire field
/// changes shape.
const V1_MAGIC: [u8; 4] = *b"CDS1";

/// Current snapshot wire version. Bumped from 1 → 2 in the
/// audit-#102 envelope wire-bump (embedded
/// `IdentityEnvelope` grew from 208 → 209 bytes for the new
/// version byte). v1 readers cannot consume v2 bytes (the
/// envelope offsets shift); v2 readers reject v1 bytes via the
/// version-byte check below. Rolling-upgrade compat from v1 was
/// removed deliberately — see the audit doc and the project
/// release notes for the migration cliff.
pub const SNAPSHOT_VERSION: u8 = 2;

/// Errors from snapshot serialization.
///
/// `to_bytes` is on the migration / snapshot-send path, so a
/// panic on `u32` length-prefix overflow (`state.len()` or
/// `bindings_bytes.len()` exceeding 4 GiB) would crash the
/// dispatch task without releasing locks. The fallible
/// counterpart is [`StateSnapshot::try_to_bytes`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnapshotError {
    /// The snapshot's `state` or `bindings_bytes` exceeds the
    /// `u32::MAX` (4 GiB) wire-format cap.
    ExceedsWireFormat {
        /// `self.state.len()` at the time of the failure.
        state_len: usize,
        /// `self.bindings_bytes.len()` at the time of the failure.
        bindings_len: usize,
    },
}

impl std::fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ExceedsWireFormat {
                state_len,
                bindings_len,
            } => write!(
                f,
                "snapshot exceeds wire-format cap (state_len={}, bindings_len={}, max=u32::MAX)",
                state_len, bindings_len
            ),
        }
    }
}

impl std::error::Error for SnapshotError {}

/// A serializable state snapshot at a point in the causal chain.
#[derive(Debug, Clone)]
pub struct StateSnapshot {
    /// Wire version this snapshot was produced under. Writers
    /// always stamp [`SNAPSHOT_VERSION`]; readers accept v0 bytes
    /// by surfacing them with the v1 defaults populated.
    pub version: u8,
    /// Entity this snapshot belongs to.
    pub entity_id: EntityId,
    /// Sequence number this snapshot is valid through.
    pub through_seq: u64,
    /// CausalLink at the snapshot point (for chain verification).
    pub chain_link: CausalLink,
    /// Serialized daemon state (opaque bytes).
    pub state: Bytes,
    /// The entity's observed horizon at snapshot time.
    pub horizon: ObservedHorizon,
    /// Timestamp when snapshot was taken (unix nanos).
    pub created_at: u64,
    /// Opaque wire slot for channel-re-bind metadata populated by
    /// [`DAEMON_CHANNEL_REBIND_PLAN.md`](../../../../docs/DAEMON_CHANNEL_REBIND_PLAN.md).
    /// Stage 1 of the identity-migration plan lands this as an
    /// always-empty `Vec` so the wire format is forward-compatible
    /// with the channel-re-bind work even though the typed
    /// `DaemonBindings` decoder isn't yet present. Plan #1 will
    /// decode these bytes into its own struct at restore time.
    pub bindings_bytes: Vec<u8>,
    /// Encrypted ed25519 seed + attestation for cross-node identity
    /// transport. Populated by
    /// [`DAEMON_IDENTITY_MIGRATION_PLAN.md`](../../../../docs/DAEMON_IDENTITY_MIGRATION_PLAN.md)
    /// Stage 3; Stage 1 always emits `None`. A `None` envelope on
    /// restore means "public-identity migration" — the target gets
    /// a read-only keypair that can still serve `entity_id` /
    /// `origin_hash` queries but refuses to sign anything new.
    pub identity_envelope: Option<IdentityEnvelope>,
    /// Runtime-only: payload bytes of the event at
    /// `chain_link.sequence`. Required by
    /// [`super::log::EntityLog::from_snapshot`] to validate the
    /// next event's `parent_hash` after restore (the chain validator
    /// computes `xxh3(prev_link_bytes ++ prev_payload)`).
    ///
    /// **Not serialized** — `to_bytes` / `from_bytes` skip this
    /// field, so the wire format is unchanged. Callers reconstructing
    /// a snapshot from the log have the head event in hand and
    /// populate this via `with_head_payload` before passing the
    /// snapshot to restore. Cross-node migration carries the head
    /// event through the migration message itself, paired with the
    /// snapshot bytes.
    ///
    /// `Option<Bytes>` so the "no head_payload context attached"
    /// case is structurally distinct from "head event genuinely
    /// had an empty payload." An empty-Bytes sentinel would
    /// conflate both: `assess_continuity` would reject legitimate
    /// non-genesis snapshots whose head event happens to carry an
    /// empty payload as if they were missing-context. With the
    /// Option, `Some(Bytes::new())` is "head event payload is
    /// empty" (legitimate) and `None` is "caller hasn't populated
    /// this field" (verification can't proceed).
    ///
    /// Default for snapshots deserialized from wire bytes is
    /// `None`; callers populate it from the head event via
    /// `with_head_payload` before `EntityLog::from_snapshot` can
    /// validate subsequent events.
    pub head_payload: Option<Bytes>,
}

impl StateSnapshot {
    /// Create a new snapshot stamped with the current wire version
    /// and empty v1 extension fields.
    pub fn new(
        entity_id: EntityId,
        chain_link: CausalLink,
        state: Bytes,
        horizon: ObservedHorizon,
    ) -> Self {
        Self {
            version: SNAPSHOT_VERSION,
            entity_id,
            through_seq: chain_link.sequence,
            chain_link,
            state,
            horizon,
            created_at: current_timestamp(),
            bindings_bytes: Vec::new(),
            identity_envelope: None,
            head_payload: None,
        }
    }

    /// Attach the head event's payload bytes — needed by
    /// `EntityLog::from_snapshot` to validate the next event's
    /// chain link after restore. Genesis snapshots
    /// (`chain_link.sequence == 0`) carry empty bytes; subsequent
    /// snapshots carry the payload of the event at
    /// `chain_link.sequence`.
    pub fn with_head_payload(mut self, head_payload: Bytes) -> Self {
        self.head_payload = Some(head_payload);
        self
    }

    /// Attach an identity envelope sealed to `target_static_pub`,
    /// returning `self` by value so the call chains cleanly off
    /// [`Self::new`] / the source's snapshot-build path.
    ///
    /// Fails with
    /// [`EnvelopeError::SourceReadOnly`](crate::adapter::net::identity::EnvelopeError::SourceReadOnly)
    /// when `source_kp` is public-only — a public-only caller can't
    /// produce the attestation signature the target needs to verify.
    /// The attestation transcript binds to `self.chain_link`, so the
    /// resulting envelope is non-replayable at a different migration
    /// point.
    pub fn with_identity_envelope(
        mut self,
        source_kp: &EntityKeypair,
        target_static_pub: [u8; 32],
    ) -> Result<Self, crate::adapter::net::identity::EnvelopeError> {
        let env = IdentityEnvelope::new(source_kp, target_static_pub, &self.chain_link)?;
        self.identity_envelope = Some(env);
        Ok(self)
    }

    /// Open the attached identity envelope (if any) using the
    /// target's X25519 static private key. Returns the daemon's
    /// fully-keyed [`EntityKeypair`], which the target-side
    /// restore path uses instead of the caller-supplied fallback.
    ///
    /// Returns `Ok(None)` when the snapshot has no envelope —
    /// callers interpret this as "public-identity migration, target
    /// gets a read-only keypair." Returns `Err` if the envelope is
    /// present but fails to verify / unseal; callers must treat
    /// that as a terminal error, not a fallback trigger, or an
    /// attacker could downgrade identity transport by tampering.
    pub fn open_identity_envelope(
        &self,
        target_x25519_priv: &x25519_dalek::StaticSecret,
    ) -> Result<Option<EntityKeypair>, crate::adapter::net::identity::EnvelopeError> {
        match &self.identity_envelope {
            None => Ok(None),
            Some(env) => {
                // Pass the snapshot's `entity_id` as
                // `expected_signer_pub` so a substituted envelope
                // (built by an attacker with the correct
                // `target_static_pub` but a different signer
                // identity) is rejected EARLY — before any
                // cryptographic work. The post-decrypt
                // `kp.entity_id() != self.entity_id` check is
                // retained as defense-in-depth.
                let kp = env.open(
                    target_x25519_priv,
                    &self.chain_link,
                    Some(self.entity_id.as_bytes()),
                )?;
                // Belt-and-braces: the decrypted keypair's
                // `origin_hash` must match the snapshot's
                // `entity_id`. The early-reject above catches the
                // common case where signer_pub differs; this
                // covers the (now-vanishingly-rare) case where
                // the decrypted seed produces a derived pub that
                // doesn't match what the envelope claimed.
                if kp.entity_id() != &self.entity_id {
                    return Err(crate::adapter::net::identity::EnvelopeError::OriginHashMismatch);
                }
                Ok(Some(kp))
            }
        }
    }

    /// Serialize to bytes for transfer.
    ///
    /// # v1 wire format
    ///
    /// ```text
    /// magic:             4 bytes (b"CDS1")
    /// version:           1 byte  (SNAPSHOT_VERSION)
    /// entity_id:        32 bytes
    /// through_seq:       8 bytes
    /// chain_link:       CAUSAL_LINK_SIZE bytes (28)
    /// created_at:        8 bytes
    /// state_len:         4 bytes (u32)
    /// state:             state_len bytes
    /// bindings_len:      4 bytes (u32)
    /// bindings:          bindings_len bytes (opaque; see `bindings_bytes`)
    /// envelope_flag:     1 byte (0 = none, 1 = present)
    /// [envelope:       208 bytes]  (if envelope_flag == 1)
    /// ```
    ///
    /// Horizon and `head_payload` are not serialized in the compact
    /// format — `head_payload` is a runtime-only field populated by
    /// the caller from the head event before invoking restore (see
    /// the field's doc).
    pub fn to_bytes(&self) -> Vec<u8> {
        // Tests and well-known internal callers know their state is
        // bounded; production callers (compute orchestrator, migration
        // handler) should use `try_to_bytes` so an oversized snapshot
        // surfaces as a `MigrationError::StateFailed(...)` rather than a
        // panic that unwinds across the dispatch task.
        self.try_to_bytes()
            .expect("StateSnapshot::to_bytes — call try_to_bytes for fallible serialization")
    }

    /// Fallible serialization to bytes.
    ///
    /// Returns `SnapshotError::ExceedsWireFormat { .. }` when
    /// `state.len()` or `bindings_bytes.len()` exceeds the
    /// `u32::MAX` wire-format cap (4 GiB). The wire format encodes
    /// each as a `u32` length prefix; a payload that overflows
    /// would be permanently un-decodable.
    ///
    /// `to_bytes` is on the migration / snapshot-send path, where
    /// a panic on `u32` length-prefix overflow would crash the
    /// dispatch task without releasing locks. `state` is opaque
    /// caller-supplied bytes (compute orchestrator, FFI clients)
    /// and `bindings_bytes` is opaque externally-controlled
    /// migration metadata, so the `>4 GiB` case is reachable from
    /// outside-controlled inputs. Production callers should use
    /// `try_to_bytes` and surface the error; the legacy `to_bytes`
    /// wrapper is kept for well-known-bounded test callers.
    pub fn try_to_bytes(&self) -> Result<Vec<u8>, SnapshotError> {
        // Validate state and bindings sizes BEFORE allocating the
        // output buffer — a 4+ GiB heap allocation on a known-bad
        // input would itself be a (smaller) availability hit.
        let state_len =
            u32::try_from(self.state.len()).map_err(|_| SnapshotError::ExceedsWireFormat {
                state_len: self.state.len(),
                bindings_len: self.bindings_bytes.len(),
            })?;
        let bindings_len = u32::try_from(self.bindings_bytes.len()).map_err(|_| {
            SnapshotError::ExceedsWireFormat {
                state_len: self.state.len(),
                bindings_len: self.bindings_bytes.len(),
            }
        })?;

        let envelope_bytes_len = if self.identity_envelope.is_some() {
            IDENTITY_ENVELOPE_SIZE
        } else {
            0
        };
        let header_size = V1_MAGIC.len() + 1 + 32 + 8 + CAUSAL_LINK_SIZE + 8 + 4;
        let trailer_size = 4 + self.bindings_bytes.len() + 1 + envelope_bytes_len;
        let mut buf = Vec::with_capacity(header_size + self.state.len() + trailer_size);

        buf.extend_from_slice(&V1_MAGIC);
        buf.push(SNAPSHOT_VERSION);
        buf.extend_from_slice(self.entity_id.as_bytes());
        buf.extend_from_slice(&self.through_seq.to_le_bytes());
        buf.extend_from_slice(&self.chain_link.to_bytes());
        buf.extend_from_slice(&self.created_at.to_le_bytes());
        buf.extend_from_slice(&state_len.to_le_bytes());
        buf.extend_from_slice(&self.state);

        buf.extend_from_slice(&bindings_len.to_le_bytes());
        buf.extend_from_slice(&self.bindings_bytes);

        match &self.identity_envelope {
            None => buf.push(0),
            Some(env) => {
                buf.push(1);
                buf.extend_from_slice(&env.to_bytes());
            }
        }

        Ok(buf)
    }

    /// Deserialize from bytes.
    ///
    /// Accepts only v2 (post-audit-#102 wire bump) layouts. v1
    /// and pre-magic v0 bytes are rejected — see the project
    /// release notes for the migration cliff. The audit-#102 bump
    /// changed `IDENTITY_ENVELOPE_SIZE` (208 → 209), shifting
    /// every offset in the v1 trailer; a v1 reader cannot
    /// consume v2 bytes correctly, and conversely. v1 bytes
    /// reach this function with the current magic but a stale
    /// version byte; the version-byte check inside `from_bytes_v2`
    /// rejects them.
    ///
    /// `head_payload` is runtime-only and always defaults to empty
    /// after deserialize; callers must populate it from the head
    /// event before passing the snapshot to
    /// [`super::log::EntityLog::from_snapshot`].
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() >= V1_MAGIC.len() && data[..V1_MAGIC.len()] == V1_MAGIC {
            Self::from_bytes_v2(&data[V1_MAGIC.len()..])
        } else {
            // No magic prefix → pre-magic-era v0 layout. Rejected
            // post-bump; rolling upgrade from that era is no
            // longer supported.
            None
        }
    }

    fn from_bytes_v2(data: &[u8]) -> Option<Self> {
        let mut cursor = data;
        if cursor.remaining() < 1 {
            return None;
        }
        let version = cursor.get_u8();
        if version != SNAPSHOT_VERSION {
            // A future v2 reader can match on this byte; today we
            // reject cleanly instead of mis-parsing.
            return None;
        }

        // Core header — same layout as v0 past this point, except
        // the reader branches to the v1 trailer at the end.
        let header_remaining = 32 + 8 + CAUSAL_LINK_SIZE + 8 + 4;
        if cursor.remaining() < header_remaining {
            return None;
        }

        let mut entity_bytes = [0u8; 32];
        cursor.copy_to_slice(&mut entity_bytes);
        let entity_id = EntityId::from_bytes(entity_bytes);

        let through_seq = cursor.get_u64_le();

        let mut link_bytes = [0u8; CAUSAL_LINK_SIZE];
        cursor.copy_to_slice(&mut link_bytes);
        let chain_link = CausalLink::from_bytes(&link_bytes)?;

        let created_at = cursor.get_u64_le();

        let state_len = cursor.get_u32_le() as usize;
        if cursor.remaining() < state_len {
            return None;
        }
        let state = Bytes::copy_from_slice(&cursor[..state_len]);
        cursor = &cursor[state_len..];

        // v1 trailer — bindings (length-prefixed opaque bytes) then
        // optional envelope.
        if cursor.remaining() < 4 {
            return None;
        }
        let bindings_len = cursor.get_u32_le() as usize;
        if cursor.remaining() < bindings_len {
            return None;
        }
        let bindings_bytes = cursor[..bindings_len].to_vec();
        cursor = &cursor[bindings_len..];

        if cursor.remaining() < 1 {
            return None;
        }
        let envelope_flag = cursor.get_u8();
        let identity_envelope = match envelope_flag {
            0 => None,
            1 => {
                if cursor.remaining() < IDENTITY_ENVELOPE_SIZE {
                    return None;
                }
                let env = IdentityEnvelope::from_bytes(&cursor[..IDENTITY_ENVELOPE_SIZE])?;
                cursor = &cursor[IDENTITY_ENVELOPE_SIZE..];
                Some(env)
            }
            _ => return None,
        };

        // Strict length match — trailing garbage after the envelope
        // is a framing bug on the source, not forward-compat.
        if !cursor.is_empty() {
            return None;
        }

        // Consistency checks, same set as v0.
        if chain_link.sequence != through_seq {
            return None;
        }
        if chain_link.origin_hash != entity_id.origin_hash() {
            return None;
        }

        Some(Self {
            version: SNAPSHOT_VERSION,
            entity_id,
            through_seq,
            chain_link,
            state,
            horizon: ObservedHorizon::new(),
            created_at,
            bindings_bytes,
            identity_envelope,
            // Runtime-only: not on the wire. Caller populates from
            // the head event before invoking `EntityLog::from_snapshot`.
            head_payload: None,
        })
    }

    /// Compact header size (excluding state payload). Historical
    /// constant from the v0 compat path — kept for any external
    /// caller that may still reference it for sizing math.
    pub const HEADER_SIZE: usize = 32 + 8 + CAUSAL_LINK_SIZE + 8 + 4; // 80 bytes (was 76 pre-#130)

    /// Age of this snapshot in seconds.
    pub fn age_secs(&self) -> u64 {
        let now = current_timestamp();
        (now.saturating_sub(self.created_at)) / 1_000_000_000
    }
}

/// Snapshot store — holds the latest snapshot per entity.
///
/// Keyed by full EntityId (32 bytes) to avoid origin_hash collisions.
pub struct SnapshotStore {
    snapshots: dashmap::DashMap<[u8; 32], StateSnapshot>,
    /// Per-entity highest `through_seq` ever observed. Survives
    /// `remove` so a stale producer that races AFTER retention
    /// drops the live entry cannot rewind state by re-storing an
    /// older snapshot.
    ///
    /// Pre-fix the store had no such record. After
    /// `remove`, ANY snapshot — including one with a
    /// `through_seq` lower than the just-removed value — was
    /// accepted. A stale producer racing retention could
    /// re-store an older snapshot under the same `entity_id`
    /// and downstream readers observed a state rollback (ABA on
    /// the snapshot lineage). Now `store` rejects any snapshot
    /// whose `through_seq` is `<=` the high-water mark even when
    /// no live entry exists.
    ///
    /// Callers that legitimately need to rebind the entity at a
    /// lower `through_seq` (e.g. wiping for a fresh
    /// reconstruction) must call `forget` to clear the high-water
    /// mark before storing.
    high_water: dashmap::DashMap<[u8; 32], u64>,
}

impl SnapshotStore {
    /// Create an empty store.
    pub fn new() -> Self {
        Self {
            snapshots: dashmap::DashMap::new(),
            high_water: dashmap::DashMap::new(),
        }
    }

    /// Store a snapshot if it is newer than the existing entry.
    ///
    /// Returns `true` when the snapshot was stored, `false` when an
    /// existing snapshot with a strictly higher (or equal)
    /// `through_seq` blocked the write — i.e. an older / replayed
    /// snapshot tried to overwrite a fresher one.
    ///
    /// Uses `DashMap::entry` to make the read-compare-write
    /// atomic per shard. An unconditional
    /// `self.snapshots.insert(key, snapshot)` would let a
    /// reordered or replayed snapshot delivery silently rewrite
    /// state at sequence N over an existing one at N+M, and
    /// concurrent stores would race (whichever DashMap insert
    /// landed last would win regardless of freshness). Equal
    /// `through_seq` is also rejected so a re-emission of the
    /// *same* snapshot from a stale producer doesn't thrash the
    /// entry (refresh-with-equal must explicitly `remove` first if
    /// intentional).
    pub fn store(&self, snapshot: StateSnapshot) -> bool {
        use dashmap::mapref::entry::Entry;
        let key = *snapshot.entity_id.as_bytes();
        let new_seq = snapshot.through_seq;

        // Gate on the per-entity high-water mark first.
        // The high-water survives `remove`, so a stale producer
        // racing retention can't rewind state. Order: check
        // high_water under its own shard guard, then check the
        // live snapshot. The DashMap shard for high_water is
        // distinct from the shard for snapshots, so this is two
        // brief lock acquires rather than one held across both
        // operations — fine for a non-hot path.
        if let Some(prev_seq) = self.high_water.get(&key).map(|v| *v) {
            if new_seq <= prev_seq {
                return false;
            }
        }

        // Real linearization point: the snapshots-side entry
        // guard. Two concurrent `store(seq=N)` calls can each pass
        // the high_water check above (the read-then-write isn't
        // CAS), but only one wins the entry guard — the loser's
        // `new_seq > slot.get().through_seq` is then false (slot
        // already at N) and it returns `false`. The high_water
        // write that happens here is therefore best-effort: the
        // surviving value is whichever store committed the
        // snapshot, not the loser's identical seq. The "freshness"
        // invariant — "stored snapshot has the largest seq we ever
        // saw" — is preserved by the snapshots-side guard alone;
        // the high_water table only bounds *future* stores from
        // rewinding.
        match self.snapshots.entry(key) {
            Entry::Vacant(slot) => {
                slot.insert(snapshot);
                self.high_water.insert(key, new_seq);
                true
            }
            Entry::Occupied(mut slot) => {
                if new_seq > slot.get().through_seq {
                    slot.insert(snapshot);
                    self.high_water.insert(key, new_seq);
                    true
                } else {
                    false
                }
            }
        }
    }

    /// Clear the per-entity high-water mark.
    ///
    /// Use this when the entity is being legitimately rebound for
    /// a fresh reconstruction (e.g. wiping for a daemon migration
    /// from a known-clean snapshot at a lower `through_seq`). No
    /// effect on the snapshot itself — call `remove` separately
    /// if you also want to evict the live entry.
    ///
    /// `pub(crate)` rather than `pub`: `forget` is the escape
    /// hatch that defeats the high-water-mark anti-rewind
    /// guarantee that `store` upholds. An external caller able to
    /// invoke it arbitrarily can stage stale snapshots over fresh
    /// ones, undermining the rebind-safety invariant the
    /// high_water table exists to enforce. Internal call sites
    /// (migration / rebind paths) may use it; external SDK
    /// surfaces should not.
    ///
    /// Currently only exercised by unit tests; reserved for the
    /// migration-rebind path that the high_water mark itself was
    /// added to support. The `#[allow(dead_code)]` is intentional
    /// — removing the function entirely would force whoever
    /// wires up the rebind callsite to re-derive the threat
    /// model.
    #[allow(dead_code)]
    pub(crate) fn forget(&self, entity_id: &EntityId) {
        self.high_water.remove(entity_id.as_bytes());
    }

    /// Get the latest snapshot for an entity.
    pub fn get(
        &self,
        entity_id: &EntityId,
    ) -> Option<dashmap::mapref::one::Ref<'_, [u8; 32], StateSnapshot>> {
        self.snapshots.get(entity_id.as_bytes())
    }

    /// Remove a snapshot.
    pub fn remove(&self, entity_id: &EntityId) -> Option<StateSnapshot> {
        self.snapshots.remove(entity_id.as_bytes()).map(|(_, s)| s)
    }

    /// Number of stored snapshots.
    pub fn count(&self) -> usize {
        self.snapshots.len()
    }
}

impl Default for SnapshotStore {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for SnapshotStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SnapshotStore")
            .field("snapshots", &self.snapshots.len())
            .finish()
    }
}

use crate::adapter::net::current_timestamp;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::identity::EntityKeypair;
    use crate::adapter::net::state::causal::CausalChainBuilder;

    #[test]
    fn test_snapshot_roundtrip() {
        let kp = EntityKeypair::generate();
        let entity_id = kp.entity_id().clone();
        let mut builder = CausalChainBuilder::new(kp.origin_hash());

        // Build a short chain
        for i in 0..5 {
            builder
                .append(Bytes::from(format!("event-{}", i)), 0)
                .unwrap();
        }

        let state_data = Bytes::from_static(b"serialized daemon state here");
        let snapshot = StateSnapshot::new(
            entity_id.clone(),
            *builder.head(),
            state_data.clone(),
            ObservedHorizon::new(),
        );

        assert_eq!(snapshot.through_seq, 5);

        let bytes = snapshot.to_bytes();
        let parsed = StateSnapshot::from_bytes(&bytes).unwrap();

        assert_eq!(parsed.entity_id, entity_id);
        assert_eq!(parsed.through_seq, 5);
        assert_eq!(parsed.chain_link, *builder.head());
        assert_eq!(parsed.state, state_data);
    }

    #[test]
    fn test_snapshot_store() {
        let store = SnapshotStore::new();

        let kp = EntityKeypair::generate();
        let entity_id = kp.entity_id().clone();
        let origin_hash = kp.origin_hash();
        let link = CausalLink::genesis(origin_hash, 0);

        let snapshot = StateSnapshot::new(
            entity_id.clone(),
            link,
            Bytes::from_static(b"state"),
            ObservedHorizon::new(),
        );

        let stored = store.store(snapshot);
        assert!(stored, "first store of an entity must succeed");
        assert_eq!(store.count(), 1);

        let retrieved = store.get(&entity_id).unwrap();
        assert_eq!(retrieved.state, Bytes::from_static(b"state"));
    }

    #[test]
    fn test_snapshot_replaces_older() {
        let store = SnapshotStore::new();
        let kp = EntityKeypair::generate();
        let entity_id = kp.entity_id().clone();
        let origin_hash = kp.origin_hash();

        let snap1 = StateSnapshot::new(
            entity_id.clone(),
            CausalLink::genesis(origin_hash, 0),
            Bytes::from_static(b"state-v1"),
            ObservedHorizon::new(),
        );
        assert!(store.store(snap1));

        let mut builder = CausalChainBuilder::new(origin_hash);
        builder.append(Bytes::from_static(b"e1"), 0).unwrap();

        let snap2 = StateSnapshot::new(
            entity_id.clone(),
            *builder.head(),
            Bytes::from_static(b"state-v2"),
            ObservedHorizon::new(),
        );
        assert!(store.store(snap2));

        assert_eq!(store.count(), 1);
        let retrieved = store.get(&entity_id).unwrap();
        assert_eq!(retrieved.state, Bytes::from_static(b"state-v2"));
        assert_eq!(retrieved.through_seq, 1);
    }

    #[test]
    fn test_from_bytes_too_short() {
        assert!(StateSnapshot::from_bytes(&[0u8; 10]).is_none());
    }

    // ========================================================================
    // store() must reject older snapshots (no rewind)
    // ========================================================================

    /// Building snapshots via the chain helper makes the
    /// `chain_link.sequence` actually-match `through_seq`, which is
    /// the wire-level invariant `from_bytes` enforces. Tests below
    /// drive the real public API rather than poking through_seq
    /// directly so the regression resembles the production failure
    /// mode (signed snapshots arriving in non-monotonic order).
    fn snap_at(
        entity_id: EntityId,
        builder: &mut CausalChainBuilder,
        state_bytes: &'static [u8],
    ) -> StateSnapshot {
        StateSnapshot::new(
            entity_id,
            *builder.head(),
            Bytes::from_static(state_bytes),
            ObservedHorizon::new(),
        )
    }

    /// An older snapshot (lower `through_seq`) arriving after a
    /// newer one must NOT overwrite the newer entry. Pre-fix
    /// `store` unconditionally inserted, so a replayed or reordered
    /// older snapshot silently rolled state back. Now `store`
    /// returns `false` and the existing entry is preserved.
    #[test]
    fn store_rejects_older_snapshot_against_newer_existing_entry() {
        let store = SnapshotStore::new();
        let kp = EntityKeypair::generate();
        let entity_id = kp.entity_id().clone();
        let mut builder = CausalChainBuilder::new(kp.origin_hash());

        // newer snapshot at seq 5
        for _ in 0..5 {
            builder.append(Bytes::from_static(b"e"), 0).unwrap();
        }
        let newer = snap_at(entity_id.clone(), &mut builder, b"v5");
        assert_eq!(newer.through_seq, 5);
        assert!(store.store(newer), "first store must succeed");

        // older snapshot at seq 2 (rebuild a fresh chain)
        let mut older_builder = CausalChainBuilder::new(kp.origin_hash());
        for _ in 0..2 {
            older_builder.append(Bytes::from_static(b"e"), 0).unwrap();
        }
        let older = snap_at(entity_id.clone(), &mut older_builder, b"v2");
        assert_eq!(older.through_seq, 2);
        let stored = store.store(older);

        assert!(!stored, "older snapshot must be rejected");
        let retrieved = store.get(&entity_id).unwrap();
        assert_eq!(
            retrieved.state,
            Bytes::from_static(b"v5"),
            "newer snapshot must be preserved despite older arrival",
        );
        assert_eq!(retrieved.through_seq, 5);
    }

    /// CR-17: pin the ABA-via-retention behavior. `store` correctly
    /// rejects an older `through_seq` against a newer one, BUT
    /// `remove` does NOT carry forward the high-water mark — once
    /// the store is `remove`d, a stale producer can re-`store` an
    /// older snapshot and the cycle starts fresh.
    ///
    /// This is a documented limitation, not a fix: callers that
    /// invoke `remove` MUST take responsibility for not letting
    /// stale producers race in afterward (typically by holding a
    /// channel-level lock, or by only calling `remove` during
    /// channel teardown when no producers are live). The test
    /// pins the behavior so a future maintainer who tries to
    /// "fix" it inadvertently doesn't break the deliberate
    /// retention-clears-the-slot semantics that operators rely on.
    ///
    /// If retention is ever wired into a multi-producer code path,
    /// the right move is to add a per-entity high-water-mark cache
    /// that survives `remove` — but that's a separate audit
    /// entry. For now: this test documents the gap.
    /// CR-17: post-fix, the store maintains a
    /// per-entity high-water mark that survives `remove`. A stale
    /// producer racing retention can no longer rewind state by
    /// re-storing an older snapshot under the same entity_id.
    ///
    /// Pre-fix this test pinned the broken behavior ("stale
    /// snapshot is accepted post-remove"); post-fix the same
    /// scenario is rejected, and the test asserts the stored
    /// snapshot stays at the high-water value.
    #[test]
    fn bug8_remove_preserves_through_seq_high_water_mark() {
        let store = SnapshotStore::new();
        let kp = EntityKeypair::generate();
        let entity_id = kp.entity_id().clone();

        // Store a snapshot at seq=3.
        let mut builder = CausalChainBuilder::new(kp.origin_hash());
        for _ in 0..3 {
            builder.append(Bytes::from_static(b"e"), 0).unwrap();
        }
        let high = snap_at(entity_id.clone(), &mut builder, b"high");
        assert_eq!(high.through_seq, 3);
        assert!(store.store(high));

        // Older snapshot at seq=1 is rejected against the live
        // high-water mark.
        let mut older_builder = CausalChainBuilder::new(kp.origin_hash());
        older_builder.append(Bytes::from_static(b"e"), 0).unwrap();
        let older = snap_at(entity_id.clone(), &mut older_builder, b"stale");
        assert_eq!(older.through_seq, 1);
        assert!(
            !store.store(older.clone()),
            "older through_seq must be rejected against the live high-water mark"
        );

        // Now retention removes the entry. `remove` returns the
        // stored snapshot but the high-water mark survives.
        let removed = store.remove(&entity_id);
        assert!(removed.is_some(), "remove must return the stored snapshot");

        // A stale producer that races AFTER retention tries to
        // re-store the older snapshot. Post-fix this is rejected
        // by the high-water gate.
        let stale_rejected = !store.store(older.clone());
        assert!(
            stale_rejected,
            "post-`remove`, an older snapshot must be rejected \
             because the high-water mark survives remove. Pre-fix this \
             would accept and rewind state."
        );

        // Live entry stays empty (we removed it and didn't accept
        // the stale write).
        assert!(
            store.get(&entity_id).is_none(),
            "live entry must remain empty — neither the original (removed) \
             nor the stale (rejected) snapshot is present"
        );
    }

    /// `forget` clears the high-water so a legitimate rebind at
    /// a lower through_seq is possible. Use this when the entity
    /// is being reconstructed from scratch.
    #[test]
    fn bug8_forget_clears_high_water_to_allow_rebind() {
        let store = SnapshotStore::new();
        let kp = EntityKeypair::generate();
        let entity_id = kp.entity_id().clone();

        let mut builder = CausalChainBuilder::new(kp.origin_hash());
        for _ in 0..3 {
            builder.append(Bytes::from_static(b"e"), 0).unwrap();
        }
        let high = snap_at(entity_id.clone(), &mut builder, b"high");
        assert!(store.store(high));
        store.remove(&entity_id);

        // Without forget, a lower-seq snapshot is rejected.
        let mut older_builder = CausalChainBuilder::new(kp.origin_hash());
        older_builder.append(Bytes::from_static(b"e"), 0).unwrap();
        let older = snap_at(entity_id.clone(), &mut older_builder, b"rebind");
        assert!(!store.store(older.clone()));

        // forget() then store() succeeds.
        store.forget(&entity_id);
        assert!(
            store.store(older),
            "after forget(), the high-water mark is cleared and an \
             older snapshot can be stored — the legitimate rebind path"
        );
        assert_eq!(
            store.get(&entity_id).unwrap().state,
            Bytes::from_static(b"rebind")
        );
    }

    /// Equal `through_seq` is rejected too — a re-emission from a
    /// stale producer shouldn't churn the entry. Callers that
    /// genuinely need to refresh-at-same-seq (e.g. legitimate
    /// rebind) must `remove` first.
    #[test]
    fn store_rejects_equal_through_seq_against_existing_entry() {
        let store = SnapshotStore::new();
        let kp = EntityKeypair::generate();
        let entity_id = kp.entity_id().clone();
        let mut builder = CausalChainBuilder::new(kp.origin_hash());
        for _ in 0..3 {
            builder.append(Bytes::from_static(b"e"), 0).unwrap();
        }
        let first = snap_at(entity_id.clone(), &mut builder, b"first");
        assert_eq!(first.through_seq, 3);
        assert!(store.store(first));

        let mut other = CausalChainBuilder::new(kp.origin_hash());
        for _ in 0..3 {
            other.append(Bytes::from_static(b"e"), 0).unwrap();
        }
        let second = snap_at(entity_id.clone(), &mut other, b"second");
        assert_eq!(second.through_seq, 3);
        let stored = store.store(second);

        assert!(!stored, "equal through_seq must be rejected");
        let retrieved = store.get(&entity_id).unwrap();
        assert_eq!(
            retrieved.state,
            Bytes::from_static(b"first"),
            "first-stored snapshot must remain authoritative on equal through_seq",
        );
    }

    // ========================================================================
    // try_to_bytes must NOT panic on oversized state / bindings
    // ========================================================================

    /// `try_to_bytes` returns `SnapshotError::ExceedsWireFormat`
    /// when `bindings_bytes` exceeds the `u32::MAX` cap, instead
    /// of panicking via `expect`. Pre-fix `to_bytes` was on the
    /// migration / snapshot-send path, so a panic crashed the
    /// dispatch task without releasing locks.
    ///
    /// Building a >4 GiB `state` payload is impractical in a unit
    /// test, but `bindings_bytes` is `Vec<u8>` and we can flip its
    /// length to overflow `u32::MAX` via the `set_len` unsafe
    /// vector trick on a zero-capacity allocation only if we have
    /// genuine memory — also impractical. Instead we use
    /// `Bytes::from_static(&[..])` for `state` and exploit the
    /// fact that `Bytes::len()` reports the slice length: we
    /// can't actually allocate 5 GiB, but we CAN exercise the
    /// guard by mocking via `Bytes::from(Vec::with_capacity(0))`
    /// and patching state with a forged length... that's also a
    /// no-go in safe Rust.
    ///
    /// What we CAN test cheaply: pin the boundary by checking
    /// that `try_to_bytes` succeeds at the largest realistic
    /// payload we can construct (a few MiB), and that a
    /// hand-constructed `SnapshotError::ExceedsWireFormat`
    /// value's `Display` impl reports the lengths so callers
    /// surfacing the error get a useful message. The actual
    /// `>4 GiB` path is exercised by the `try_from`'s contract
    /// (`u32::try_from(usize > u32::MAX)` is a documented `Err`).
    #[test]
    fn try_to_bytes_succeeds_at_realistic_payload_sizes() {
        let kp = EntityKeypair::generate();
        let entity_id = kp.entity_id().clone();
        let mut builder = CausalChainBuilder::new(kp.origin_hash());
        builder.append(Bytes::from_static(b"e"), 0).unwrap();

        // 4 MiB state — a realistic-large daemon snapshot.
        let big_state = Bytes::from(vec![0u8; 4 * 1024 * 1024]);
        let snapshot = StateSnapshot::new(
            entity_id,
            *builder.head(),
            big_state.clone(),
            ObservedHorizon::new(),
        );
        let bytes = snapshot
            .try_to_bytes()
            .expect("4 MiB state must serialize without error");
        assert!(bytes.len() > big_state.len(), "envelope adds header bytes");
    }

    /// `SnapshotError::ExceedsWireFormat` `Display` impl reports
    /// both lengths so the caller's surfaced
    /// `MigrationError::StateFailed(...)` carries enough context
    /// to debug.
    #[test]
    fn snapshot_error_exceeds_wire_format_display_includes_lengths() {
        let err = SnapshotError::ExceedsWireFormat {
            state_len: 5_000_000_000,
            bindings_len: 0,
        };
        let s = format!("{}", err);
        assert!(s.contains("5000000000"));
        assert!(s.contains("u32::MAX"));
    }

    /// `try_to_bytes` returns ExceedsWireFormat when bindings
    /// overflow u32. We construct an already-overflowing
    /// `bindings_bytes` only if memory permits; this test is
    /// gated to skip on a host that can't allocate ~5 GiB. The
    /// `try_from` contract is itself the load-bearing check —
    /// this test is included for clarity and only exercises the
    /// path opportunistically.
    #[test]
    #[ignore = "requires ~5 GiB of memory; the try_from u32 guard is the load-bearing check"]
    fn try_to_bytes_rejects_oversized_bindings() {
        let kp = EntityKeypair::generate();
        let entity_id = kp.entity_id().clone();
        let mut builder = CausalChainBuilder::new(kp.origin_hash());
        builder.append(Bytes::from_static(b"e"), 0).unwrap();

        let mut snapshot = StateSnapshot::new(
            entity_id,
            *builder.head(),
            Bytes::from_static(b"small state"),
            ObservedHorizon::new(),
        );
        // 4 GiB + 1 byte bindings_bytes — overflows u32.
        snapshot.bindings_bytes = vec![0u8; (u32::MAX as usize) + 1];

        let err = snapshot
            .try_to_bytes()
            .expect_err("oversized bindings_bytes must surface as SnapshotError, not panic");
        assert!(matches!(err, SnapshotError::ExceedsWireFormat { .. }));
    }

    /// Strictly newer `through_seq` is accepted — pins the success
    /// path so a future tightening that flips `>` to `>=` can't
    /// silently break legitimate progressive snapshots.
    #[test]
    fn store_accepts_strictly_newer_snapshot() {
        let store = SnapshotStore::new();
        let kp = EntityKeypair::generate();
        let entity_id = kp.entity_id().clone();
        let mut builder = CausalChainBuilder::new(kp.origin_hash());
        for _ in 0..2 {
            builder.append(Bytes::from_static(b"e"), 0).unwrap();
        }
        let earlier = snap_at(entity_id.clone(), &mut builder, b"v2");
        assert!(store.store(earlier));

        for _ in 0..3 {
            builder.append(Bytes::from_static(b"e"), 0).unwrap();
        }
        let later = snap_at(entity_id.clone(), &mut builder, b"v5");
        assert_eq!(later.through_seq, 5);
        assert!(store.store(later), "newer snapshot must be accepted");

        let retrieved = store.get(&entity_id).unwrap();
        assert_eq!(retrieved.through_seq, 5);
        assert_eq!(retrieved.state, Bytes::from_static(b"v5"));
    }

    // ---- Regression tests for Cubic AI findings ----

    #[test]
    fn test_regression_from_bytes_rejects_sequence_mismatch() {
        // Regression: from_bytes accepted snapshots where
        // chain_link.sequence != through_seq.
        let kp = EntityKeypair::generate();
        let entity_id = kp.entity_id().clone();
        let mut builder = CausalChainBuilder::new(kp.origin_hash());
        builder.append(Bytes::from_static(b"e1"), 0).unwrap();

        let snapshot = StateSnapshot::new(
            entity_id,
            *builder.head(),
            Bytes::from_static(b"state"),
            ObservedHorizon::new(),
        );
        let mut bytes = snapshot.to_bytes();

        // v1 layout: 4 magic + 1 version + 32 entity_id = 37 bytes
        // before through_seq starts.
        bytes[37] = 0xFF;

        assert!(
            StateSnapshot::from_bytes(&bytes).is_none(),
            "from_bytes must reject snapshot with sequence mismatch"
        );
    }

    // ---- v1 wire format tests ----

    #[test]
    fn v1_roundtrip_preserves_bindings_and_envelope() {
        let kp = EntityKeypair::generate();
        let entity_id = kp.entity_id().clone();
        let mut builder = CausalChainBuilder::new(kp.origin_hash());
        builder.append(Bytes::from_static(b"e"), 0).unwrap();

        let env = IdentityEnvelope {
            target_static_pub: [0x11; 32],
            sealed_seed: [0x22; 80],
            signer_pub: [0x33; 32],
            signature: [0x44; 64],
        };

        let mut snapshot = StateSnapshot::new(
            entity_id,
            *builder.head(),
            Bytes::from_static(b"state"),
            ObservedHorizon::new(),
        );
        snapshot.bindings_bytes = vec![0x55; 42];
        snapshot.identity_envelope = Some(env.clone());

        let bytes = snapshot.to_bytes();
        // Writers always emit v1 — the first 4 bytes are the magic.
        assert_eq!(&bytes[..4], b"CDS1");
        assert_eq!(bytes[4], SNAPSHOT_VERSION);

        let parsed = StateSnapshot::from_bytes(&bytes).expect("v1 round-trip");
        assert_eq!(parsed.version, SNAPSHOT_VERSION);
        assert_eq!(parsed.bindings_bytes, vec![0x55; 42]);
        assert_eq!(parsed.identity_envelope, Some(env));
        assert_eq!(parsed.state, Bytes::from_static(b"state"));
    }

    // The pre-magic v0 → v1 rolling-upgrade compat test was
    // removed in the audit-#102 wire bump (see
    // `from_bytes_rejects_pre_magic_v0_layout` for the
    // post-bump rejection invariant).

    #[test]
    fn v1_rejects_trailing_garbage() {
        let kp = EntityKeypair::generate();
        let snapshot = StateSnapshot::new(
            kp.entity_id().clone(),
            CausalLink::genesis(kp.origin_hash(), 0),
            Bytes::from_static(b"s"),
            ObservedHorizon::new(),
        );
        let mut bytes = snapshot.to_bytes();
        bytes.push(0xFF);
        assert!(
            StateSnapshot::from_bytes(&bytes).is_none(),
            "trailing byte after a v1 snapshot must be rejected — a short \
             snapshot plus junk is indistinguishable from a framing bug",
        );
    }

    #[test]
    fn v1_rejects_unknown_version_byte() {
        let kp = EntityKeypair::generate();
        let snapshot = StateSnapshot::new(
            kp.entity_id().clone(),
            CausalLink::genesis(kp.origin_hash(), 0),
            Bytes::from_static(b"s"),
            ObservedHorizon::new(),
        );
        let mut bytes = snapshot.to_bytes();
        // 4 magic bytes then the version. Flip to an unknown future
        // version — decoder must refuse rather than mis-parse.
        bytes[4] = 0xFE;
        assert!(StateSnapshot::from_bytes(&bytes).is_none());
    }

    // ---- Identity-envelope end-to-end (Stage 5) ----

    fn fresh_x25519() -> (x25519_dalek::StaticSecret, [u8; 32]) {
        let mut seed = [0u8; 32];
        getrandom::fill(&mut seed).unwrap();
        let sk = x25519_dalek::StaticSecret::from(seed);
        let pk = x25519_dalek::PublicKey::from(&sk);
        (sk, *pk.as_bytes())
    }

    #[test]
    fn envelope_roundtrip_seals_and_opens_through_wire() {
        // Full migration-primitive slice: source builds a snapshot,
        // seals its daemon keypair to the target's X25519 pubkey,
        // serializes, ships bytes, target deserializes, opens the
        // envelope with its X25519 private key, recovers the same
        // daemon keypair (including the ability to sign).
        let daemon_kp = EntityKeypair::generate();
        let entity_id = daemon_kp.entity_id().clone();
        let mut builder = CausalChainBuilder::new(daemon_kp.origin_hash());
        builder.append(Bytes::from_static(b"event"), 0).unwrap();

        let (target_priv, target_pub) = fresh_x25519();

        let snapshot = StateSnapshot::new(
            entity_id.clone(),
            *builder.head(),
            Bytes::from_static(b"daemon state"),
            ObservedHorizon::new(),
        )
        .with_identity_envelope(&daemon_kp, target_pub)
        .expect("seal");

        // Round-trip through bytes (simulating the wire).
        let bytes = snapshot.to_bytes();
        let received = StateSnapshot::from_bytes(&bytes).expect("decode");
        assert!(received.identity_envelope.is_some());

        // Target opens with its X25519 private key.
        let recovered = received
            .open_identity_envelope(&target_priv)
            .expect("open")
            .expect("envelope present");

        // Full round-trip: the recovered keypair has the same
        // identity AND a working signing half.
        assert_eq!(recovered.entity_id(), &entity_id);
        assert_eq!(recovered.origin_hash(), daemon_kp.origin_hash());
        assert!(!recovered.is_read_only());
        let sig = recovered.sign(b"post-migration");
        assert!(entity_id.verify(b"post-migration", &sig).is_ok());
    }

    #[test]
    fn envelope_open_on_snapshot_without_envelope_returns_none() {
        let kp = EntityKeypair::generate();
        let snapshot = StateSnapshot::new(
            kp.entity_id().clone(),
            CausalLink::genesis(kp.origin_hash(), 0),
            Bytes::from_static(b"s"),
            ObservedHorizon::new(),
        );

        let (target_priv, _) = fresh_x25519();
        let opened = snapshot
            .open_identity_envelope(&target_priv)
            .expect("no envelope is not an error");
        assert!(
            opened.is_none(),
            "public-identity migration: target gets None"
        );
    }

    #[test]
    fn envelope_open_rejects_wrong_entity_id() {
        // Belt-and-braces: the snapshot commits to a specific
        // entity_id independently of the envelope's attestation. If
        // the envelope's attested `signer_pub` doesn't match the
        // snapshot's `entity_id`, `open_identity_envelope` must
        // reject — otherwise an attacker who compromises the
        // envelope-sealing path could still be caught by the
        // snapshot-level identity commitment.
        let real_daemon = EntityKeypair::generate();
        let impostor = EntityKeypair::generate();
        let (target_priv, target_pub) = fresh_x25519();

        let mut builder = CausalChainBuilder::new(real_daemon.origin_hash());
        builder.append(Bytes::from_static(b"e"), 0).unwrap();

        // Snapshot commits to `real_daemon`'s entity_id…
        let mut snapshot = StateSnapshot::new(
            real_daemon.entity_id().clone(),
            *builder.head(),
            Bytes::from_static(b"s"),
            ObservedHorizon::new(),
        );

        // …but an envelope is built from the impostor's keypair.
        // Can't happen through `with_identity_envelope` (which uses
        // the snapshot's own daemon keypair), so we construct
        // manually to simulate a tampered wire payload.
        let env = IdentityEnvelope::new(&impostor, target_pub, &snapshot.chain_link)
            .expect("impostor can still seal their own keypair");
        snapshot.identity_envelope = Some(env);

        // Fix up chain_link's origin_hash so the snapshot's own
        // consistency check (origin_hash == entity_id.origin_hash)
        // still passes — the point of this test is the
        // envelope-vs-entity_id mismatch, not the chain check.
        assert_eq!(
            snapshot.chain_link.origin_hash,
            snapshot.entity_id.origin_hash()
        );

        let err = snapshot
            .open_identity_envelope(&target_priv)
            .expect_err("impostor envelope must be rejected");
        use crate::adapter::net::identity::EnvelopeError;
        // Post-fix the early `expected_signer_pub` check
        // fires first (before any cryptographic work) and surfaces
        // `InvalidSignerKey`. The pre-fix rejection was at the
        // post-decrypt cross-check (`OriginHashMismatch`) — same
        // outcome (envelope rejected) but the new path is faster
        // and avoids unnecessary AEAD work.
        assert_eq!(err, EnvelopeError::InvalidSignerKey);
    }

    #[test]
    fn v1_without_envelope_uses_single_zero_byte_trailer() {
        // Regression: a `None` envelope must occupy exactly one byte
        // on the wire — a stray extra byte would shift every trailing
        // field and poison the round-trip in ways the strict
        // length-match catches only coincidentally.
        let kp = EntityKeypair::generate();
        let snapshot = StateSnapshot::new(
            kp.entity_id().clone(),
            CausalLink::genesis(kp.origin_hash(), 0),
            Bytes::from_static(b"s"),
            ObservedHorizon::new(),
        );
        let bytes = snapshot.to_bytes();
        assert_eq!(
            *bytes.last().expect("at least one byte"),
            0,
            "envelope_flag must be zero when None",
        );
    }

    /// Audit #102 wire-bump: the pre-magic-era v0 reader was
    /// removed. Any input lacking the `CDS1` magic prefix is now
    /// rejected at `from_bytes`, regardless of the body shape.
    #[test]
    fn from_bytes_rejects_pre_magic_v0_layout() {
        // Construct what would have been a valid v0 body
        // (no magic prefix) — should reject post-bump.
        let kp = EntityKeypair::generate();
        let mut builder = CausalChainBuilder::new(kp.origin_hash());
        builder.append(Bytes::from_static(b"e1"), 0).unwrap();
        let head = *builder.head();
        let through_seq = head.sequence;
        let state = b"state-bytes";

        let mut buf = bytes::BytesMut::new();
        use bytes::BufMut;
        buf.put_slice(kp.entity_id().as_bytes());
        buf.put_u64_le(through_seq);
        buf.put_slice(&head.to_bytes());
        buf.put_u64_le(12345);
        buf.put_u32_le(state.len() as u32);
        buf.put_slice(state);

        assert!(
            StateSnapshot::from_bytes(&buf).is_none(),
            "post-#102 reader must reject pre-magic v0 layout (no CDS1 prefix); \
             rolling-upgrade compat from that era is gone"
        );
    }

    /// Audit #102 wire-bump: a v1-version-byte snapshot (correct
    /// magic, but version=1) is rejected because v1's embedded
    /// envelope was 208 bytes — every offset in the v1 trailer
    /// shifts under v2's 209-byte envelope, so silent acceptance
    /// would mis-parse the rest.
    #[test]
    fn from_bytes_rejects_v1_version_byte() {
        // Smallest possible v1-shaped buffer with valid magic +
        // wrong version. We don't need a full body — the
        // version-byte check rejects before any header parse.
        let mut buf = bytes::BytesMut::new();
        use bytes::BufMut;
        buf.put_slice(&V1_MAGIC);
        buf.put_u8(1); // pre-bump SNAPSHOT_VERSION
                       // 32 (entity) + 8 (seq) + CAUSAL_LINK_SIZE + 8 (created) + 4 (state_len)
        buf.put_bytes(0u8, 32 + 8 + CAUSAL_LINK_SIZE + 8 + 4);
        assert!(
            StateSnapshot::from_bytes(&buf).is_none(),
            "post-#102 reader must reject the v1 version byte; the embedded \
             envelope size shifted from 208 → 209 so v1 trailers are unparseable"
        );
    }

    /// Regression: `EntityLog::from_snapshot` requires the head
    /// event's payload bytes to validate the next event's
    /// `parent_hash`. The snapshot now carries `head_payload` as a
    /// runtime-only field (not on the wire) that callers populate
    /// from the head event before invoking restore. This test pins:
    ///
    /// 1. The default constructor leaves `head_payload` empty.
    /// 2. `with_head_payload` stores the bytes.
    /// 3. The wire format is unchanged — `head_payload` round-trips
    ///    as empty regardless of what was set in-process (since the
    ///    field isn't serialized).
    /// 4. After deserialize, the caller can populate `head_payload`
    ///    out-of-band and use the snapshot for restore.
    #[test]
    fn head_payload_is_runtime_only_not_on_wire() {
        let kp = EntityKeypair::generate();
        let mut builder = CausalChainBuilder::new(kp.origin_hash());
        let head_event_payload = Bytes::from_static(b"head-event-payload");
        builder.append(head_event_payload.clone(), 0).unwrap();

        // Default constructor: head_payload is empty.
        let mut snap = StateSnapshot::new(
            kp.entity_id().clone(),
            *builder.head(),
            Bytes::from_static(b"daemon-state-bytes"),
            ObservedHorizon::new(),
        );
        assert!(
            snap.head_payload.is_none(),
            "default constructor leaves head_payload as None (Cubic P2)"
        );

        // Pin `created_at` so the wire-byte comparison below is
        // deterministic — the field is sampled from the system
        // clock at construction.
        snap.created_at = 0;
        // with_head_payload stores the bytes wrapped in Some.
        let snap = snap.with_head_payload(head_event_payload.clone());
        assert_eq!(snap.head_payload.as_ref(), Some(&head_event_payload));

        // Wire format is unchanged: head_payload is NOT serialized.
        // We pin this two ways:
        //   (a) the round-trip yields head_payload = empty
        //       regardless of what was set in-process
        //   (b) the byte length is identical to a snapshot with
        //       empty head_payload (proves no length-prefix sneaked
        //       into the wire format)
        let bytes_with = snap.to_bytes();
        let mut snap_empty = StateSnapshot::new(
            kp.entity_id().clone(),
            *builder.head(),
            Bytes::from_static(b"daemon-state-bytes"),
            ObservedHorizon::new(),
        );
        snap_empty.created_at = 0;
        let bytes_without = snap_empty.to_bytes();
        assert_eq!(
            bytes_with.len(),
            bytes_without.len(),
            "head_payload must not appear in the wire format"
        );
        assert_eq!(
            bytes_with, bytes_without,
            "wire bytes must be identical regardless of head_payload"
        );

        // Round-trip: head_payload is None after parse (Cubic P2:
        // explicit "context missing" sentinel, not Bytes::new()).
        let parsed = StateSnapshot::from_bytes(&bytes_with).unwrap();
        assert!(
            parsed.head_payload.is_none(),
            "head_payload after round-trip must be None (runtime-only field)"
        );

        // Caller populates head_payload from the head event they
        // already have, then restore can succeed.
        let parsed = parsed.with_head_payload(head_event_payload.clone());
        assert_eq!(parsed.head_payload.as_ref(), Some(&head_event_payload));
    }
}
