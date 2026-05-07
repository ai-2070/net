# Daemon identity transport on migration

## Context

[`SDK_COMPUTE_SURFACE_PLAN.md`](SDK_COMPUTE_SURFACE_PLAN.md) ships daemon spawn + migration, promising that "each daemon has its own keypair" (line 112) and that migration "moves the daemon from source to target" (line 193). The plan never says whether the keypair travels. The current core code enforces that it must — `restore_snapshot` at `compute/migration_target.rs:143` takes `keypair: EntityKeypair` as a caller-supplied parameter, and `DaemonHost::from_snapshot` asserts `chain.origin_hash() == keypair.origin_hash()` (`host.rs:66`). There is no wire slot for the key in `MigrationMessage` and no mechanism for the target to produce a matching keypair; every attempt to migrate today either panics on the origin-hash assert or silently constructs a daemon with a fresh key that receivers will refuse to recognise.

Two load-bearing facts from the current code shape the plan:

- **Causal events are not individually signed.** `CausalLink` carries `origin_hash: u32` + `parent_hash` (xxh3, structural). Tamper resistance is session-scoped via Noise AEAD. A migrated daemon's new causal events don't need a signing ceremony; they need to stamp the same `origin_hash`.
- **The keypair's signing role is narrower than "everything the daemon says."** Current uses:
  - Signing `CapabilityAnnouncement`s if the daemon advertises its own caps (via `MeshNode::announce_capabilities`).
  - Issuing `PermissionToken`s as an authority.
  - Delegating tokens (signing child tokens from a parent).
  - *Not* signing outbound payloads, not signing subscribe/unsubscribe requests, not authenticating reads from storage.

Pure compute daemons (state machines consuming events, emitting payloads) don't use the private key at all — the keypair sits in `DaemonHost::keypair` purely to keep `origin_hash` stable. Token-issuer daemons and caps-announcing daemons do use it. The plan must support both.

## Scope

**In scope**

- Transporting the daemon's ed25519 private key from source to target, wrapped under a key only the target can unwrap.
- Source-side key destruction after target acknowledges possession.
- A "public identity only" opt-out for daemons that don't need signing capability post-migration.
- Failure handling: partial transport, source crash mid-migration, target crash before activation.
- Wire format for the key envelope — new field on `StateSnapshot` with a version byte bump (same bump `DAEMON_CHANNEL_REBIND_PLAN.md` and `SDK_COMPUTE_SURFACE_PLAN.md` § *API stability* are already calling for — land together).

**Out of scope**

- **Delegation-chain identity rotation.** A design where each migration generation gets a fresh sub-keypair signed by the previous generation, eliminating key transport entirely, is tracked as a v3 proposal. The protocol impact (signature chains on capability announcements + permission tokens) is too broad for this plan.
- **Multi-party-compute / threshold-signed daemon identities.** Same v3 bucket.
- **Persistent at-rest key storage on either node.** Daemons are in-memory only today; this plan keeps it that way.
- **Identity recovery after dual node loss.** If both source and target lose the daemon's private key simultaneously, the identity is gone. This is the same property as any single-custodian key model; not degraded by this plan.

## Design invariants

1. **The private key is never in plaintext on the wire.** Source encrypts under a target-specific pubkey before handing to `MigrationMessage::SnapshotReady`. The Noise session's AEAD is defense in depth, not the primary confidentiality layer — tying confidentiality to the session only would leave the key exposed to any middlebox that can log post-decryption payloads.
2. **Two machines hold the key for exactly one interval.** After target sends `ActivateAck`, source zeroizes and drops. Before `ActivateAck`, only source holds it. The target-holds-only window is after source zeroize but before any further migration — "steady state" post-migration is target-only.
3. **Losing the key fails cleanly.** If target can't decrypt, migration surfaces `MigrationError::IdentityTransportFailed` and rolls back; source keeps its key. No half-migrated state where target has a crippled daemon.
4. **Opt-out exists.** A caller that can prove it doesn't need signing capability (no caps announcements, no token issuance) can choose "public-identity migration" — the snapshot carries `entity_id` only, target is constructed with a *read-only* identity that rejects every `sign()` call. Documented failure surface.

## Current failure mode — concrete trace

```text
t=0    caller-on-A: rt.start_migration(origin=O, A → B)
       A builds StateSnapshot { entity_id: O_pub, chain_link, state, horizon }
       A sends MigrationMessage::SnapshotReady { snapshot_bytes }

t=1    B receives; B's migration target handler calls restore_snapshot(ctx, keypair, ...)
       ?? where does `keypair` come from ??
       — today: no wire carries it. Caller on B must supply.
       — if caller fabricates a fresh EntityKeypair: keypair.origin_hash() != O
         → assert_eq! in DaemonHost::from_snapshot panics, OR
         → the restore_snapshot explicit check at :162 returns StateFailed.

t=2    Migration is stuck. No path to target with the correct identity.
```

## Surface

### Wire — `StateSnapshot` v1

Same version bump that `DAEMON_CHANNEL_REBIND_PLAN.md` uses. Adds one field:

```rust
pub struct StateSnapshot {
    pub version: u8,                                    // v1
    pub entity_id: EntityId,
    pub through_seq: u64,
    pub chain_link: CausalLink,
    pub state: Bytes,
    pub horizon: ObservedHorizon,
    pub created_at: u64,

    // new in v1
    pub bindings: DaemonBindings,                       // per the channel-rebind plan
    pub identity_envelope: Option<IdentityEnvelope>,    // new this plan
}

/// Encrypted ed25519 seed + metadata for cross-node identity transport.
/// `None` = public-identity migration; target gets a read-only identity.
pub struct IdentityEnvelope {
    /// X25519 public key the payload is sealed to. Matches the target
    /// node's static X25519 key advertised via `SUBPROTOCOL_IDENTITY_ADVERT`
    /// (see "Target-key discovery" below).
    pub target_static_pub: [u8; 32],
    /// `crypto_box_seal` of the 32-byte ed25519 seed, under
    /// `target_static_pub`. 48 bytes: 32-byte ephemeral pubkey + 16-byte
    /// MAC + 32-byte ciphertext = 80 total. Libsodium-compatible layout.
    pub sealed_seed: [u8; 80],
    /// Source's ed25519 public key + signature over
    /// `(target_static_pub || snapshot.chain_link.to_bytes())`. The
    /// target verifies this before unsealing — a tampered envelope
    /// that swaps `target_static_pub` to an attacker-controlled key
    /// would fail the signature check.
    pub source_attestation: Attestation,
}

pub struct Attestation {
    pub signer_pub: [u8; 32],     // source node's ed25519 pub
    pub signature:  [u8; 64],     // ed25519
}
```

The `sealed_seed` format is `crypto_box_seal` so any libsodium-compatible implementation can decrypt it. Rationale for `crypto_box_seal` specifically (not `crypto_box`): the source doesn't need the target's ed25519 pubkey to correspond to the envelope — a single-shot "to-pubkey" encryption is enough, and we don't want to carry ephemeral public keys in the orchestrator protocol.

### Target-key discovery — new subprotocol

The source needs the target's X25519 static public key to seal against. Options:

- **Piggyback on the Noise handshake.** Noise-IK already exchanges X25519 static keys during session establishment — we can expose the peer's static key via a new `MeshNode::peer_static_x25519(node_id) -> Option<[u8; 32]>` accessor. The Noise static key lives per-session; reading it back out is a read-only accessor.
- **Separate identity advert.** A new `SUBPROTOCOL_IDENTITY_ADVERT` (TBD id) carrying the node's X25519 pubkey. Redundant but cleanly separated.

Go with piggyback. Noise-IK is already the trust anchor between the two nodes; deriving the seal pubkey from the same session avoids a second-key-discovery step.

### Core — `restore_snapshot` updated

```rust
impl MigrationTargetHandler {
    pub fn restore_snapshot(
        &self,
        ctx: RestoreContext<'_>,
        target_x25519_priv: &X25519PrivateKey,   // new — owned by the node
        daemon_factory: F,
        config: DaemonHostConfig,
    ) -> Result<(), MigrationError>
    where F: FnOnce() -> Box<dyn MeshDaemon>;
}
```

The caller no longer hands the keypair in. The target node's static X25519 private key replaces it, and the handler derives the daemon's keypair from `snapshot.identity_envelope`:

```rust
// Sketch of the new interior
let kp = match &snapshot.identity_envelope {
    Some(env) => {
        // 1. Verify attestation signs (target_x25519_pub || chain_link).
        let transcript = transcript_bytes(env.target_static_pub, &snapshot.chain_link);
        if !env.source_attestation.verify(&transcript) {
            return Err(MigrationError::IdentityTransportFailed(
                "attestation signature did not verify",
            ));
        }

        // 2. Seal-open with target's X25519 secret.
        let seed = crypto_box_seal_open(&env.sealed_seed, target_x25519_priv)
            .map_err(|_| MigrationError::IdentityTransportFailed("seal_open failed"))?;

        // 3. Reconstruct keypair from seed. origin_hash must match.
        let kp = EntityKeypair::from_bytes(seed);
        if kp.origin_hash() != snapshot.entity_id.origin_hash() {
            return Err(MigrationError::IdentityTransportFailed(
                "seed produced mismatched origin_hash",
            ));
        }
        kp
    }
    None => {
        // Public-identity mode — read-only keypair.
        EntityKeypair::public_only(snapshot.entity_id.clone())
    }
};
```

`EntityKeypair::public_only` is new — wraps an `EntityId` with a signing half that returns `Err(KeypairError::ReadOnly)` on every `sign`. The daemon host accepts it, but any code path that expects to sign (capability advertisement, token issuance) fails with a typed error.

### Source — wipe after `ActivateAck`

Source-side today already drops the daemon at cleanup. Add one explicit zeroize step before drop:

```rust
fn handle_activate_ack(&self, daemon_origin: u32) {
    let host = self.daemon_registry.unregister(daemon_origin).ok();
    if let Some(host) = host {
        host.keypair.zeroize();   // new — existing ed25519-dalek supports zeroize
        drop(host);
    }
}
```

`EntityKeypair::zeroize()` is a pass-through to `ed25519_dalek::SigningKey::zeroize()`. Requires a bound on the keypair type that it already has (via `zeroize` crate).

### SDK — opt-in / opt-out flag

```rust
impl DaemonRuntime {
    pub async fn start_migration_with(
        &self,
        origin_hash: u32,
        source_node: NodeId,
        target_node: NodeId,
        opts: MigrationOpts,
    ) -> Result<MigrationHandle, MigrationError>;
}

pub struct MigrationOpts {
    /// If `false`, the snapshot's `identity_envelope` is `None` and the
    /// target receives a read-only identity. Appropriate for pure compute
    /// daemons (consumes events, emits payloads) that don't announce caps
    /// or mint tokens. Default: `true`.
    pub transport_identity: bool,
    /// Default 60 s. Expiry on the orchestrator side; if the envelope
    /// hasn't been consumed by then, source keeps its key and the
    /// migration aborts.
    pub identity_transport_timeout: Duration,
}
```

`start_migration` (bare) keeps default `MigrationOpts` for ergonomics.

## Failure handling

Three concrete failure modes, each with a defined outcome:

### Source crash between `SnapshotReady` and `ActivateAck`

- The envelope is in flight / on target. Source's key hasn't been zeroized.
- On source restart: the daemon host is reconstructed from in-memory state, which is gone. Source has no record it was migrating. Its keypair is gone with the process.
- Outcome: target holds the key. Source has no identity copy. **Key is now single-holder on target** — exactly the end state we wanted, just without the explicit zeroize step.
- Side effect: if source had external tokens/caps signed with this key that expire and need re-signing, those stop working until target re-issues.

### Target crash before `ActivateAck`

- Target had the envelope. Its process is gone — keypair wasn't persisted.
- Source sees no `ActivateAck`, times out after `identity_transport_timeout`, emits `MigrationError::IdentityTransportFailed`, keeps its key, tears down the migration state.
- Outcome: source-only, rollback clean.

### Target can't decrypt (envelope sealed to wrong key)

- Attestation verifies (source signed against `target_static_pub`) but the target's current X25519 private key doesn't match — can happen if the target rotated keys between the handshake and the migration.
- `crypto_box_seal_open` returns error. Target emits `MigrationFailed { reason: "IdentityTransportFailed: seal_open failed" }`. Source keeps its key.

### Target receives a tampered envelope

- Attestation signature fails to verify.
- Target emits `MigrationFailed`. Source is held responsible for the rollback (its key is still live).

## Migration-phase integration

No new phases — the envelope rides `SnapshotReady`. Existing phase transitions are unchanged:

```text
TakeSnapshot → SnapshotReady(bytes, envelope) → RestoreComplete → ReplayComplete
                       │                             │
                       │                             └─ Here: target's daemon host
                       │                                has the decrypted keypair.
                       │
                       └─ Here: source's keypair is STILL live. It does not zeroize
                          until ActivateAck arrives.

→ CutoverNotify → CleanupComplete → ActivateTarget → ActivateAck
                                                           │
                                                           └─ Source zeroizes here.
```

## Staged rollout

Six PRs, mergeable independently after the `StateSnapshot` version bump lands.

### Stage 1 — `StateSnapshot` v1 wire format (~1 d)

- Add `version: u8`, `bindings: DaemonBindings` (empty stub OK for this PR), `identity_envelope: Option<IdentityEnvelope>` (always `None` for now).
- Serde + `to_bytes` / `from_bytes` update. v0 bytes decode as v1 with both new fields empty.
- Existing tests round-trip unchanged.

### Stage 2 — `EntityKeypair::public_only` + zeroize hook (~1 d)

- Read-only keypair variant; `sign()` returns `Err(KeypairError::ReadOnly)`.
- `zeroize()` method on `EntityKeypair` delegating to `SigningKey::zeroize()`.
- Unit tests.

### Stage 3 — `IdentityEnvelope` — seal + open + attestation (~2 d)

- `crypto_box_seal` / `seal_open` wrappers. (We already depend on a crypto stack; if `sodiumoxide`-compatible primitives aren't in-tree, use `crypto_box` crate from RustCrypto.)
- `IdentityEnvelope::new(source_kp, target_x25519_pub, chain_link) -> Result<Self, _>` constructor.
- `IdentityEnvelope::open(target_x25519_priv, chain_link) -> Result<EntityKeypair, _>`.
- Attestation signs `(target_x25519_pub || chain_link.to_bytes())`; target verifies before `seal_open`.
- Unit tests: valid → ok; tampered attestation → reject; wrong target key → reject; swapped target_x25519_pub → attestation fails.

### Stage 4 — `peer_static_x25519` accessor + wiring (~1 d)

- Expose the session peer's X25519 static pubkey via a read-only accessor on `MeshNode`.
- Orchestrator fetches it when building the envelope.
- Ed25519 → X25519 conversion: the project already has both key types per-node; confirm the Noise side stores the X25519 directly (most likely does).

### Stage 5 — Wire the envelope through migration (~2 d)

- Source: `take_snapshot()` populates `identity_envelope` when `MigrationOpts::transport_identity = true`.
- Target: `restore_snapshot` signature changes to take `target_x25519_priv` instead of `keypair`; derives the keypair internally.
- Source-side `handle_activate_ack`: zeroize the keypair before drop.
- Integration test: two-node, token-issuer daemon, migrate, issue a new token from target, verify signature validates under the daemon's entity_id.

### Stage 6 — `MigrationOpts` surface in every SDK (~1 d per SDK, ×4)

- Rust SDK: `start_migration_with(... opts: MigrationOpts)`.
- NAPI + TS: optional `opts?: { transportIdentity?: boolean; identityTransportTimeoutMs?: number }`.
- PyO3 + Python: keyword args `transport_identity=True, identity_transport_timeout_s=60`.
- Go: `MigrationOpts` struct added to `StartMigration(ctx, origin, source, target, opts *MigrationOpts)`.
- Per-binding tests: default path (transport identity), opt-out path (public-identity only, assert target can't sign).

## Test plan

Concrete scenarios, each a tokio integration test in `tests/daemon_identity_migration.rs`:

- `identity_transport_roundtrip` — daemon on A, migrate to B with default opts, B holds the same `entity_id`, B can sign (issue a token, verify it), A's keypair is zeroized.
- `public_identity_mode_target_cannot_sign` — `transport_identity = false`, migrate, attempt to issue a token on B → `Err(ReadOnly)`.
- `tampered_envelope_rejected` — flip a bit in `sealed_seed` on the wire, target emits `MigrationFailed { IdentityTransportFailed }`, source keeps key.
- `wrong_target_key_rejected` — target rotated its static key mid-migration; seal_open fails, source keeps key.
- `source_crash_after_snapshot_before_ack` — kill source process at phase 3; target completes migration (has key); on source restart, no stale daemon is recovered (in-memory only).
- `target_crash_before_activate_ack` — kill target at phase 4; source times out on `identity_transport_timeout`, keeps key, migration reports failed.
- `zeroize_observable` — migrate, then try to read source's keypair via a test-only accessor → all-zeros.
- `v0_snapshot_restores_as_public_identity` — feed pre-v1 snapshot bytes; restore succeeds in public-identity mode; target can't sign.

## Critical files

```
net/crates/net/src/adapter/net/identity/entity.rs       +public_only constructor,
                                                         +zeroize method
net/crates/net/src/adapter/net/identity/envelope.rs     new: IdentityEnvelope,
                                                         Attestation, seal/open
net/crates/net/src/adapter/net/state/snapshot.rs        +version, +bindings,
                                                         +identity_envelope
net/crates/net/src/adapter/net/compute/migration_target.rs
                                                         restore_snapshot signature
                                                         change, internal derivation
net/crates/net/src/adapter/net/compute/migration_source.rs
                                                         zeroize on ActivateAck,
                                                         timeout on the transport
net/crates/net/src/adapter/net/compute/orchestrator.rs  no wire change (envelope
                                                         rides the existing
                                                         snapshot_bytes field)
net/crates/net/src/adapter/net/mesh.rs                  +peer_static_x25519 accessor
net/crates/net/sdk/src/compute.rs                       MigrationOpts,
                                                         start_migration_with
net/crates/net/tests/daemon_identity_migration.rs       new integration-test file
```

No new subprotocol IDs; no new wire message types. The envelope rides inside `SnapshotReady.snapshot_bytes`, which keeps the migration protocol surface unchanged.

## Risks

- **X25519 private key on every node.** Today each node's Noise static key is a long-term secret. Using it as the seal target for daemon keypairs means a node compromise exposes every daemon identity ever migrated to that node during the key's lifetime. Mitigation: *short-lived* seal keypair rotated per some cadence (day / week), distinct from the Noise static key. Deferred to a follow-up — v1 ships with Noise-static reuse, documented explicitly.
- **Brief dual-custody window.** Between source-seal and source-zeroize, both nodes have the key. Duration bounded by phases 2–6 of migration (typically sub-second). Document. The alternative — destroy-then-send — is worse because a lost envelope is an unrecoverable identity loss.
- **Libsodium compatibility.** `crypto_box_seal` is sodiumoxide-flavored; the `crypto_box` RustCrypto crate implements the same construction. Pinning a specific dep and writing against its API. If bindings are added later (Python, Node) that want to inspect envelopes, libsodium compat matters.
- **Key-seed size assumption.** `EntityKeypair::from_bytes` today takes a 32-byte seed. If the underlying ed25519 implementation ever switches to a non-seed representation (private scalar directly), the envelope layout needs a version bump. Call it out in the envelope's field docs.
- **Opt-out path confusion.** Users who pick `transport_identity = false` and *then* try to mint a token get a runtime error, not a compile-time one. Mitigate with loud documentation + an `EntityKeypair::is_read_only() -> bool` accessor for SDK-level preflight checks.

## Sizing

| Stage | Effort |
|---|---|
| 1. `StateSnapshot` v1 wire format | 1 d |
| 2. `public_only` keypair + zeroize | 1 d |
| 3. `IdentityEnvelope` seal / open / attest | 2 d |
| 4. `peer_static_x25519` accessor | 1 d |
| 5. Wire through migration | 2 d |
| 6. SDK opt-in / opt-out surface (×4) | 4 d |

Total: ~11 d core + bindings.

Stage 1 should land together with the channel-rebind plan's Stage 2 — both bump the same `StateSnapshot` version.

## Dependencies

- Shares the `StateSnapshot` v1 bump with [`DAEMON_CHANNEL_REBIND_PLAN.md`](DAEMON_CHANNEL_REBIND_PLAN.md). Land both behind one schema change.
- Depends on [`SDK_SECURITY_SURFACE_PLAN.md`](SDK_SECURITY_SURFACE_PLAN.md) Stage A (identity — already transitive).
- No dependency on channel-rebind itself; the two plans are orthogonal and can ship in either order after the schema bump.

## Explicit follow-ups (not in this plan)

- **Rotating seal pubkey per node.** Reuse of Noise static for seal is a pragmatic v1 choice. A dedicated short-lived X25519 "migration receiver" keypair, rotated on some cadence, limits the blast radius of node compromise.
- **Delegation-chain identity.** Avoids transport entirely by signing each migration generation's new keypair under the previous one's. Requires adding signature chains to `CapabilityAnnouncement` + `PermissionToken`. v3.
- **MPC / threshold identity.** Same v3 bucket.
- **Persistent at-rest encryption of the keypair on target.** Today the keypair lives in memory; if target persists daemons (post-v2 persistent-daemon work), the persistence layer needs its own at-rest story.
- **Revocation / rotation of an in-flight daemon identity.** A daemon whose key is believed compromised should be able to mint a new identity while preserving `origin_hash` in the causal chain. Not currently possible because chain integrity is keyed on `origin_hash == entity_id.origin_hash()`. Opens the same design space as delegation-chain.

## Open questions for review

- **Noise-static-key reuse vs dedicated seal key for v1.** Reuse is simpler; dedicated seal key is safer under node compromise. Going with reuse for v1 unless we find a concrete attacker model that makes it unacceptable — interested in your read.
- **Default `transport_identity = true` vs `false`.** Defaulting to `true` is "most daemons work out of the box" (caps announcements + token issuance survive migration). Defaulting to `false` is "minimum key exposure by default; callers opt into spreading the key." Leaning `true`: the typical daemon user expects migration to be transparent, and the opt-out is available for identity-sensitive workloads. Reversible with an SDK major-version bump if we decide otherwise.
- **Attestation over `chain_link` specifically.** The transcript binds the envelope to a specific point in the causal chain — replaying an old envelope at a later chain point is rejected. Alternative: bind to `through_seq` only. Binding to the full `chain_link` bytes is tighter (no chance of sequence-number collision across daemon generations). Going with full bytes.
- **Wire placement — `identity_envelope` on the snapshot vs a sibling `MigrationMessage` variant.** Putting it on the snapshot is additive; sibling variant is explicit. Going with snapshot-field to avoid touching `MigrationMessage` encode/decode.
- **Zeroize on `Drop` vs explicit method.** ed25519-dalek supports `ZeroizeOnDrop`; if we adopt it, the explicit `zeroize()` call becomes implicit at drop time. Risk: a `.clone()` of the keypair lives past the zeroize. Audit all clone sites in the compute module before switching to `ZeroizeOnDrop`.
