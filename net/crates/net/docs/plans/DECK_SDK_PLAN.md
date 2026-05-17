## Deck SDK — implementation plan

> Operator-side bindings: live `MeshOsSnapshot` subscription, signed admin-chain commits, audit-chain queries, log-stream subscription, and the **ICE** (break-glass) surface. The dual of [`MESHOS_SDK_PLAN.md`](MESHOS_SDK_PLAN.md) — that one ships the daemon-author trait, this one ships the cluster-operator surface. Five languages mirroring the precedent: Rust (canonical, also powers the Deck binary), Python (pyo3), Node / TypeScript (napi-rs), Go (cgo), C (raw FFI). Companion to [`MESHOS_PLAN.md`](MESHOS_PLAN.md) (the substrate it commands against), [`MESHOS_SDK_PLAN.md`](MESHOS_SDK_PLAN.md) (the type-shape parent), [`MESHDB_PLAN.md`](MESHDB_PLAN.md) (the federated-query plane Deck composes for snapshot / audit reads), and [`DECK_FEATURES.md`](DECK_FEATURES.md) (the product brief this plan turns into shippable phases). **Atomic Playboys release** per [`RELEASE_ROADMAP.md`](RELEASE_ROADMAP.md); follows the MeshOS SDK.

## Status

**Phases 1–3 (Rust SDK + ICE substrate + ICE Rust surface) shipped.** The canonical surface lives at `crates/net/sdk/src/deck.rs` (re-export curtain) over `crates/net/src/adapter/net/behavior/deck.rs` (implementation). What landed against the design below:

- `DeckClient::new(handle, snapshot_reader, identity, config)` + `DeckClient::from_runtime(&runtime, identity)` + `with_operator_registry(registry)`. `DeckClientConfig { snapshot_poll_interval, ice_signature_threshold }` ships with default threshold = 1; substrate verifier still enforces M-of-N once the threshold is raised.
- `AdminCommands` — all 9 methods: `drain`, `enter_maintenance`, `exit_maintenance`, `cordon`, `uncordon`, `drop_replicas`, `invalidate_placement`, `restart_all_daemons`, `clear_avoid_list`.
- `IceCommands` — 7 of 8 planned factories: `freeze_cluster`, `thaw_cluster`, `flush_avoid_lists`, `force_evict_replica`, `force_restart_daemon`, `force_cutover`, `kill_migration`. **`force_drain` deferred** (annotated in substrate at `behavior::meshos::ice` as "future slices add ForceDrain"); routine `Drain` lives in `AdminCommands` already.
- Typestate `IceProposal::simulate() -> SimulatedIceProposal::commit(&[OperatorSignature])`. `commit()` does not exist on `IceProposal` — `simulate()` is a compile-time prerequisite. Bindings without compile-time typestate (Python, dynamic JS via plain class) reproduce this by hiding `commit` on `IceProposal` and only surfacing it on the type returned by `simulate()`.
- `BlastRadius` carries `affected_nodes`, `affected_replicas`, `affected_daemons`, `estimated_drain_delay`, `warnings: Vec<BlastWarning>`. **`placement_stability_delta` is absent** — drop it from the binding shape until the substrate signals churn deltas. `BlastWarning` enum variants today: `ClusterFreezeBlocksOperatorActions`, `ThawResumesPendingReconciles`, `ThawHasNoFreezeToCancel`, `GlobalAvoidFlushMayReEmit`.
- Streams: `SnapshotStream`, `StatusSummaryStream`, `LogStream`, `FailureStream`, `AuditStream` — all `impl Stream`. `LogFilter { min_level, daemon_id, node_id }`; `AuditQuery::{recent, by_operator, between, force_only, since, collect, stream}`. The `subscribe_failures(since_seq)` and `status_summary{,_stream}()` surfaces are extras beyond the original §1 design and must be exposed by every binding.
- `OperatorIdentity::{from_keypair, generate, operator_id, keypair}`, `OperatorRegistry::{insert, register, verify, verify_bundle}` (distinct-operator dedup via `BTreeSet<u64>`), `AdminVerifier::{new, with_freshness, with_full_policy}`, `VerifyError` (`not_authorized`, `signature_invalid`, `insufficient_signatures`, `envelope_expired`, `envelope_from_future`, `simulation_required`, `ice_cooldown_active`).
- Substrate-side: `IceActionProposal` (the 7 ICE variants), `simulate_ice_proposal(&snapshot, &proposal) -> Result<BlastRadius, _>` (pure, exposed for the deck binary's pre-confirmation preview), the `AdminEvent::Force*` chain landings, ICE cooldown timer (`IceCooldownActive` error path), domain-tagged signing payload.

**Phase 1 deliberate constraints** (call out in every binding's README so consumers don't write against a contract the substrate doesn't yet enforce):

- **Non-signing today.** `DeckClient` records the operator ID on every admin commit but does not yet sign through channel-auth — the signing seam is plumbed (`OperatorIdentity::keypair()` exposes the keypair; ICE multi-op helpers like `sign_proposal()` exist) and lands when the substrate's operator-key channel-auth gate ships. Bindings expose the same identity-loading API today; consumers benefit transparently when the substrate cuts over.
- **Admin-audit ring, not signed admin chain, today.** `AuditQuery` reads the in-memory `admin_audit` ring on the local snapshot, not a federated MeshDB query against a signed admin chain. The API shape is final; the durable backing lands in a follow-up substrate slice.
- **Log stream is a local subscription, not RedEX `tail()`.** `subscribe_logs(LogFilter)` works against the runtime's log ring; the per-daemon RedEX `tail()` multiplex lands when the failure / audit chains move to RedEX-backed.

**Phases 4–7 underway, near full parity.** All four bindings exist and ship slices 1+2+3 (admin / audit-log-failure streams / ICE break-glass with typestate-enforced simulate→commit). What's left across every language: the `OperatorRegistry` + `AdminVerifier` + `VerifyError` surface and the `OperatorIdentity::sign_proposal()` helper — both shipped in the canonical Rust SDK but not yet exposed in any binding. Plus the cross-cutting offline-signing primitives + runnable examples + parity test enumerated under **Remaining work** below.

**Two-tier packaging.** Same split as MeshOS-SDK: Python = `bindings/python` cdylib + `sdk-py` pure-Python wrapper; Node = `bindings/node` cdylib + `sdk-ts` pure-TypeScript wrapper; Go = `bindings/go/deck-ffi` cdylib + `bindings/go/net/deck.go` (single-tier, no `sdk-go`); C = `include/net_deck.h` header + shared cdylib.

**Activation gate for Phases 4–7:** tenant operator tooling (custom dashboards, ChatOps bots, automation) outside the Rust deck binary. Deck-the-binary is already a Rust SDK consumer at `crates/net/deck/src/{app,lineage}.rs` (imports `DeckClient`, `MeshOsSnapshot`, `DaemonSnapshot`).

**Substrate prereqs** (all in code today, v0.17):

- **`AdminEvent` enum + admin chain** at `src/adapter/net/behavior/meshos/event.rs`. Every operator command this SDK exposes already rides this enum; the SDK adds the *issuer* side (signing + commit) without touching the consumer side (the MeshOS fold).
- **`MeshOsSnapshot` + `RedexFold<MeshOsSnapshot>`** at `src/adapter/net/behavior/meshos/{snapshot, chain}.rs`. The serializable projection Deck renders; subscription rides MeshDB's federated executor against the snapshot chain.
- **`ActionChainRecord`** at `src/adapter/net/behavior/meshos/chain.rs`. Postcard-versioned per-action wire form. Deck reads the historical action chain through this to drive the Behavior Timeline + audit views.
- **MeshDB federated executor** at `src/adapter/net/behavior/meshdb/`. The SDK's snapshot / audit queries compile to `MeshQuery::{Latest, Between, Filter}` and ride the existing executor — no new wire protocol.
- **Channel-auth + `OperatorIdentity`** at `src/adapter/net/identity/` and `behavior::safety`. Operator-key loading, per-action signing, signature verification.
- **MeshOS SDK type shapes** at `MESHOS_SDK_PLAN.md`. `MetadataView`, `DaemonHealth`, `DaemonControl`, `MeshOsSnapshot`, `<<…-sdk-kind:KIND>>MSG` error discriminator — the Deck SDK re-uses them verbatim rather than redefining.

**Substrate gaps this plan introduces** — all closed except as noted:

- ~~No "force" variants on `AdminEvent` today.~~ **Shipped.** `IceActionProposal` carries 7 variants (`FreezeCluster`, `ThawCluster`, `FlushAvoidLists`, `ForceEvictReplica`, `ForceRestartDaemon`, `ForceCutover`, `KillMigration`); chain-committable through `AdminEvent::Force*`. **`ForceDrain` deferred** — annotated as "future slices" in `behavior::meshos::ice`.
- ~~No live `MeshOsSnapshot` subscription path today.~~ **Shipped.** `SnapshotStream` polls the runtime's snapshot reader at `DeckClientConfig::snapshot_poll_interval` (default 100ms); the underlying fold replays the action chain on every snapshot publish.
- ~~No blast-radius simulation surface.~~ **Shipped.** `simulate_ice_proposal(&snapshot, &proposal) -> Result<BlastRadius, _>` is a pure function exposed both through `SimulatedIceProposal` (the typestate path) and as a free function for the deck binary's pre-confirmation preview. The simulator runs the affected reconcile arms against a hypothetical post-action state diff.

## Frame

Daemons author `MeshDaemon` implementations and are *subjects* of cluster behavior. Operators *command* cluster behavior. Two consumers, two SDKs, sharing the type shapes (`MetadataView`, `DaemonHealth`, `DaemonControl`, `MeshOsSnapshot`, error-kind discriminator format) without sharing the action surface.

The MeshOS SDK refuses every operator-side action by design — its locked decision #1 is "daemon-side only." The Deck SDK is the explicit counterpart: every action the MeshOS SDK refused lives here, gated by operator-key signing + channel-auth verification + admin-chain commit. Nothing in the Deck SDK bypasses MeshOS — even ICE force-operations commit to the admin chain so MeshOS reconcile sees them and acts. The only difference between an "ordinary" admin event and an ICE force-event is that the latter carries a `force = true` flag the substrate honors at the relevant gate (rate-limit, hysteresis, cooldown) and requires a multi-operator signature bundle.

Deck the *binary* is the canonical Rust SDK consumer — the terminal-UI cyberdeck that Net ships. Tenant tools (custom dashboards, ChatOps bots, automation scripts in Python / Node / Go / C) reach the same surface through the language bindings.

## Why this exists

Three reasons this needs a written plan rather than "we'll add admin-chain commits when we need them":

1. **The non-goals are load-bearing — different ones than MeshOS SDK.** Deck SDK *does* expose cluster control; MeshOS SDK does not. What Deck SDK refuses is a different list — no direct chain mutation outside the signed admin commit path, no bypass of channel-auth verification for any action, no daemon-authoring surface, no topology / identity management (adding / removing nodes is substrate-level identity work). Calling these out up front keeps later contributors from accidentally widening either side.
2. **ICE is high-authority — needs its own discipline.** Force-drain, freeze-cluster, force-evict, kill-migration, flush-avoid-list aren't "AdminEvent with an extra bit." They need: blast-radius simulation before commit; 2-of-N signing; lockout timers after execution; visual confirmation gates in the UI; a dedicated audit subchain so security review can replay every force-operation across the cluster's lifetime. The SDK is the contract surface that disciplines all of that; the UI enforces the workflow.
3. **Signing semantics matter.** Channel-auth signs an event; that signature commits to the admin chain; that commit propagates via RedEX to every node; every node's MeshOS reconcile observes the event identically. The SDK's job is to be the signing seam — load the operator key, build the event payload, sign cleanly, hand to the substrate's chain-commit path. Getting the seam wrong in any binding produces silent-bypass bugs (operator A's identity signing operator B's action; expired keys signing future-dated events; race conditions between key rotation and pending commits).

## What ships

Five operator-facing surfaces, in dependency order:

1. **Live `MeshOsSnapshot` subscription** — `SnapshotStream` pulls the freshest published snapshot per tick from a node, with replay-from-history via the action chain. Read-only; the entire Cluster Topology Map / Replica Inspector / Daemon Supervision Panel / Behavior Timeline / Node Inventory feature set composes against this single stream.
2. **Signed admin-chain commits** — every existing `AdminEvent` variant exposed as a typed method (`drain`, `enter_maintenance`, `exit_maintenance`, `cordon`, `uncordon`, `drop_replicas`, `invalidate_placement`, `restart_all_daemons`, `clear_avoid_list`). Each method signs with the operator key, commits to the admin chain, and returns a `ChainCommit` handle for audit correlation.
3. **Audit-chain queries** — `AuditQuery` composes against MeshDB to answer "every admin event by operator X between time T1 and T2," "every force-operation in the last 24h," "the audit trail for chain Y's last three migrations." Read-only; rides the existing MeshDB query plane.
4. **Log-stream subscription** — `LogStream` subscribes to per-node / per-daemon log chains via RedEX `tail()`. Filter by level / daemon-id / node-id; follow-mode or seek-by-time. Powers the Log Matrix feature.
5. **ICE — break-glass surface** — new `AdminEvent::Force*` variants (`ForceDrain`, `ForceEvictReplica`, `ForceRestartDaemon`, `ForceCutover`, `KillMigration`, `FreezeCluster { ttl }`, `ThawCluster`, `FlushAvoidLists`). Each goes through `IceProposal` → `simulate() -> BlastRadius` → `commit(signatures: &[Signature])` with multi-operator signing (default 2-of-N, configurable). Powers the Operator ICE feature.

Each surface ships in five languages. The Rust SDK is the canonical surface; the Deck binary itself is the largest Rust SDK consumer.

What this doc does NOT ship:

🚫 **No daemon-authoring surface.** That's [`MESHOS_SDK_PLAN.md`](MESHOS_SDK_PLAN.md). Operators command daemons; daemons implement themselves.

🚫 **No MeshDB query construction.** Audit queries compose against MeshDB but the SDK doesn't re-expose `MeshQuery::*`. Consumers that need raw MeshDB go through the MeshDB SDK.

🚫 **No topology / identity management.** Adding / removing nodes, key generation for new operators, ed25519 keypair management — those are substrate-level identity concerns covered in `KEY_MIGRATION_PLAN.md` and `DAEMON_IDENTITY_MIGRATION_PLAN.md`. The Deck SDK loads existing operator identities; it does not create them.

🚫 **No direct chain mutation outside signed admin commits.** Every action this SDK exposes routes through the admin-chain commit path with operator-key signing. No "skip the admin chain" hook in any binding, ever.

🚫 **No bypass of channel-auth verification.** Every signed event passes through the existing channel-auth guard; the SDK is the *issuer* side, not the *verifier* side. Verification stays substrate-internal.

🚫 **No UI rendering, layout, or terminal-control logic.** That's Deck the binary. The SDK is the data + action plane; the UI composes on top.

🚫 **No generic mesh administration outside the admin chain.** If an operation isn't representable as an `AdminEvent` variant or a chain commit, it's not in the SDK. Tenant-side workflows that need richer semantics build them on top of the SDK; we don't extend the SDK to cover them.

---

## Design

### 1. Rust SDK (canonical) — shipped

Lives at `crates/net/sdk/src/deck.rs` (re-export curtain) over `crates/net/src/adapter/net/behavior/deck.rs` (implementation). Sibling to `crates/net/sdk/src/meshos.rs` from the MeshOS SDK plan.

```rust
// Re-exports from substrate (actual import path)
pub use net::adapter::net::behavior::deck::{
    DeckClient, DeckClientConfig, DeckError,
    AdminCommands, AdminError, ChainCommit,
    IceCommands, IceError, IceProposal, SimulatedIceProposal,
    SnapshotStream, StatusSummary, StatusSummaryStream,
    LogStream, LogFilter, FailureStream, AuditQuery, AuditStream,
    OperatorIdentity, OperatorRegistry, OperatorSignature,
    DaemonCounts, PeerCounts,
};
pub use net::adapter::net::behavior::meshos::{
    AdminAuditRecord, AdminEvent, AdminVerifier, AvoidScope,
    BlastRadius, BlastWarning, FailureRecord, IceActionProposal,
    LogLevel, LogRecord, MeshOsSnapshot,
    VerificationOutcome, VerifyError, simulate_ice_proposal,
};

impl DeckClient {
    pub fn new(handle: MeshOsHandle, snapshot_reader: MeshOsSnapshotReader,
               identity: OperatorIdentity, config: DeckClientConfig) -> Self;
    pub fn from_runtime(runtime: &MeshOsRuntime, identity: OperatorIdentity) -> Self;
    pub fn with_operator_registry(self, registry: OperatorRegistry) -> Self;

    /// Live snapshot stream — polls the snapshot reader at
    /// `config.snapshot_poll_interval` (default 100ms).
    pub fn snapshots(&self) -> SnapshotStream;
    pub fn status(&self) -> StatusSummary;
    pub fn status_summary(&self) -> StatusSummary;
    pub fn status_summary_stream(&self) -> StatusSummaryStream;

    pub fn subscribe_logs(&self, filter: LogFilter) -> LogStream;
    pub fn subscribe_failures(&self, since_seq: u64) -> FailureStream;

    pub fn admin(&self) -> AdminCommands<'_>;
    pub fn ice(&self) -> IceCommands<'_>;
    pub fn audit(&self) -> AuditQuery<'_>;
}

pub struct DeckClientConfig {
    pub snapshot_poll_interval: Duration,    // default 100ms
    pub ice_signature_threshold: usize,      // default 1; substrate enforces
}

impl<'a> AdminCommands<'a> {
    pub async fn drain(&self, node: NodeId, drain_for: Duration)               -> Result<ChainCommit, AdminError>;
    pub async fn enter_maintenance(&self, node: NodeId, drain_for: Option<Duration>) -> Result<ChainCommit, AdminError>;
    pub async fn exit_maintenance(&self, node: NodeId)                         -> Result<ChainCommit, AdminError>;
    pub async fn cordon(&self, node: NodeId)                                   -> Result<ChainCommit, AdminError>;
    pub async fn uncordon(&self, node: NodeId)                                 -> Result<ChainCommit, AdminError>;
    pub async fn drop_replicas(&self, node: NodeId, chains: Vec<ChainId>)      -> Result<ChainCommit, AdminError>;
    pub async fn invalidate_placement(&self, node: NodeId)                     -> Result<ChainCommit, AdminError>;
    pub async fn restart_all_daemons(&self, node: NodeId)                      -> Result<ChainCommit, AdminError>;
    pub async fn clear_avoid_list(&self, node: NodeId)                         -> Result<ChainCommit, AdminError>;
}

impl<'a> IceCommands<'a> {
    pub fn freeze_cluster(&self, ttl: Duration)                                -> IceProposal;
    pub fn thaw_cluster(&self)                                                 -> IceProposal;
    pub fn flush_avoid_lists(&self, scope: AvoidScope)                         -> IceProposal;
    pub fn force_evict_replica(&self, chain: ChainId, victim: NodeId)          -> IceProposal;
    pub fn force_restart_daemon(&self, daemon: DaemonRef)                      -> IceProposal;
    pub fn force_cutover(&self, chain: ChainId, target: NodeId)                -> IceProposal;
    pub fn kill_migration(&self, migration: MigrationId)                       -> IceProposal;
    // force_drain — deferred (substrate annotation: "future slices add ForceDrain").
}

/// Typestate: `IceProposal` has no `commit` — `simulate()` is a
/// compile-time prerequisite. Bindings without compile-time
/// typestate reproduce by hiding `commit` on the proposal class
/// and only surfacing it on the simulated form.
pub struct IceProposal { /* opaque */ }
impl IceProposal {
    pub async fn simulate(self) -> Result<SimulatedIceProposal, IceError>;
}

pub struct SimulatedIceProposal { /* opaque — carries BlastRadius + payload */ }
impl SimulatedIceProposal {
    pub fn blast_radius(&self) -> &BlastRadius;
    pub fn signing_payload(&self) -> Vec<u8>;
    pub async fn commit(self, signatures: &[OperatorSignature])
        -> Result<ChainCommit, IceError>;
}

pub struct BlastRadius {
    pub affected_nodes: Vec<NodeId>,
    pub affected_replicas: Vec<ChainId>,
    pub affected_daemons: Vec<DaemonRef>,
    pub estimated_drain_delay: Option<Duration>,
    pub warnings: Vec<BlastWarning>,
    // placement_stability_delta — absent from substrate; dropped from bindings.
}

pub struct OperatorSignature {
    pub operator_id: u64,
    pub signature: Vec<u8>,   // 64-byte ed25519
}

impl OperatorRegistry {
    pub fn new() -> Self;
    pub fn insert(&mut self, operator_id: u64, public_key: EntityId);
    pub fn register(&mut self, keypair: &EntityKeypair);
    pub fn verify(&self, sig: &OperatorSignature, payload: &[u8]) -> Result<(), VerifyError>;
    /// Distinct-operator dedup via `BTreeSet<u64>`; same operator
    /// signing twice does not satisfy the threshold.
    pub fn verify_bundle(&self, sigs: &[OperatorSignature], payload: &[u8],
                          threshold: usize) -> Result<(), VerifyError>;
}
```

**Stream types** all implement `impl Stream<Item = T>` for their respective payloads:
- `SnapshotStream` → `MeshOsSnapshot`
- `StatusSummaryStream` → `StatusSummary` (`{ peer_counts, daemon_counts, … }`)
- `LogStream` → `LogRecord` (per `LogFilter { min_level, daemon_id, node_id }`)
- `FailureStream` → `FailureRecord` (paginated via `subscribe_failures(since_seq)`)
- `AuditStream` → `AdminAuditRecord` (fluent builder: `audit().recent(n) | by_operator(id) | between(s,e) | force_only() | since(seq)` then `.collect()` or `.stream()`)

**`OperatorIdentity`** is constructed from an `EntityKeypair` (`from_keypair`) or generated for tests (`generate`); the SDK never derives identities from a configuration file — operator key loading happens outside the SDK, in the deck binary's startup path. Key rotation requires a new `DeckClient` (Locked decision #9).

### 2. Python SDK (pyo3) — design

**Tier split.** pyo3 cdylib at `bindings/python/src/deck.rs` (raw classes); pure-Python wrapper at `sdk-py/src/net_sdk/deck.py` (iterator protocol + ergonomic context manager). Precedent: `bindings/python/src/compute.rs` for daemon-trait wrapping; `bindings/python/src/meshdb.rs` for query-builder + stream patterns.

```python
# sdk-py/src/net_sdk/deck.py (ergonomic wrapper)
import net_sdk.deck as deck
from net._net import (
    DeckClient as _RawClient,
    OperatorIdentity, OperatorSignature, AvoidScope,
)

class DeckClient:
    def __init__(self, runtime, identity: OperatorIdentity,
                 config: "DeckClientConfig | None" = None): ...

    # Streams — Python iterators (__iter__ + __next__)
    def snapshots(self) -> "Iterator[MeshOsSnapshot]": ...
    def status_summary_stream(self) -> "Iterator[StatusSummary]": ...
    def subscribe_logs(self, filter: "LogFilter | dict") -> "Iterator[LogRecord]": ...
    def subscribe_failures(self, since_seq: int = 0) -> "Iterator[FailureRecord]": ...

    # One-shots
    def status(self) -> "StatusSummary": ...
    def status_summary(self) -> "StatusSummary": ...

    @property
    def admin(self) -> "AdminCommands": ...
    @property
    def ice(self) -> "IceCommands": ...
    def audit(self) -> "AuditQuery": ...

class AdminCommands:
    def drain(self, node: int, drain_for_ms: int) -> "ChainCommit": ...
    def enter_maintenance(self, node: int,
                           drain_for_ms: int | None = None) -> "ChainCommit": ...
    def exit_maintenance(self, node: int) -> "ChainCommit": ...
    def cordon(self, node: int) -> "ChainCommit": ...
    def uncordon(self, node: int) -> "ChainCommit": ...
    def drop_replicas(self, node: int, chains: list[int]) -> "ChainCommit": ...
    def invalidate_placement(self, node: int) -> "ChainCommit": ...
    def restart_all_daemons(self, node: int) -> "ChainCommit": ...
    def clear_avoid_list(self, node: int) -> "ChainCommit": ...

class IceCommands:
    def freeze_cluster(self, ttl_ms: int) -> "IceProposal": ...
    def thaw_cluster(self) -> "IceProposal": ...
    def flush_avoid_lists(self, scope: AvoidScope) -> "IceProposal": ...
    def force_evict_replica(self, chain: int, victim: int) -> "IceProposal": ...
    def force_restart_daemon(self, daemon: "DaemonRef") -> "IceProposal": ...
    def force_cutover(self, chain: int, target: int) -> "IceProposal": ...
    def kill_migration(self, migration: int) -> "IceProposal": ...
    # No force_drain — substrate-deferred.

class IceProposal:
    def simulate(self) -> "SimulatedIceProposal": ...
    # No commit() — the binding hides it to mirror Rust typestate.

class SimulatedIceProposal:
    @property
    def blast_radius(self) -> "BlastRadius": ...
    def signing_payload(self) -> bytes: ...
    def commit(self, signatures: list[OperatorSignature]) -> "ChainCommit": ...

class AuditQuery:
    def recent(self, limit: int) -> "AuditQuery": ...
    def by_operator(self, op_id: int) -> "AuditQuery": ...
    def between(self, start_ms: int, end_ms: int) -> "AuditQuery": ...
    def force_only(self) -> "AuditQuery": ...
    def since(self, since_seq: int) -> "AuditQuery": ...
    def collect(self) -> "list[AdminAuditRecord]": ...
    def stream(self) -> "Iterator[AdminAuditRecord]": ...
```

```python
import net_sdk.deck as deck

client = deck.DeckClient(runtime, op_identity, deck.DeckClientConfig(
    snapshot_poll_interval_ms=100, ice_signature_threshold=1))

# Live snapshot subscription
for snap in client.snapshots():
    render_topology(snap.peers)
    if snap.local_maintenance.kind == "DrainFailed":
        alert(f"Drain failed: {snap.local_maintenance.reason}")
        break

# Ordinary admin commit
commit = client.admin.enter_maintenance(node=0xABCD, drain_for_ms=600_000)
print(f"committed at seq {commit.seq}")

# ICE — typestate enforced
proposal = client.ice.freeze_cluster(ttl_ms=300_000)
simulated = proposal.simulate()        # required step
print(f"affects {len(simulated.blast_radius.affected_nodes)} nodes")
if confirm("Continue?"):
    commit = simulated.commit([sig_op1, sig_op2])

# Audit
for entry in client.audit().recent(100).stream():
    print(f"{entry.committed_at} {entry.operator_id} {entry.event_kind}")

# Logs
for record in client.subscribe_logs(deck.LogFilter(min_level="warn", daemon_id=42)):
    print(record)
```

**Error surface.** Errors raise `DeckSdkError(kind: str, message: str)` for admin/snapshot paths; `IceError(kind, message)` for ICE — both parse `<<deck-sdk-kind:KIND>>MSG`. Kinds today: `loop_closed`, `queue_full`, `unknown_node`, `unknown_chain`, `unknown_daemon`, `freeze_in_effect`, `not_authorized`, `signature_invalid`, `insufficient_signatures`, `envelope_expired`, `envelope_from_future`, `simulation_required`, `ice_cooldown_active`. Bindings tolerate unknown kinds.

### 3. Node / TypeScript SDK (napi-rs) — design

**Tier split.** napi-rs cdylib at `bindings/node/src/deck.rs` (raw classes + napi-callable stream iterators); pure-TS wrapper at `sdk-ts/src/deck.ts` (AsyncIterable shim, typed builders, error classes). Precedent: `bindings/node/meshdb.ts` for query-builder + AsyncIterable streaming.

```ts
// sdk-ts/src/deck.ts
export class DeckClient {
  constructor(runtime: MeshOsRuntime, identity: OperatorIdentity,
              config?: DeckClientConfig);

  // Streams — AsyncIterable wrappers over a raw nextXxx() napi method
  snapshots(): AsyncIterable<MeshOsSnapshot>;
  statusSummaryStream(): AsyncIterable<StatusSummary>;
  subscribeLogs(filter: LogFilter): AsyncIterable<LogRecord>;
  subscribeFailures(sinceSeq?: bigint): AsyncIterable<FailureRecord>;

  // One-shots
  status(): StatusSummary;
  statusSummary(): StatusSummary;

  readonly admin: AdminCommands;
  readonly ice: IceCommands;
  audit(): AuditQuery;
}

export class AdminCommands {
  drain(node: bigint, drainForMs: bigint): Promise<ChainCommit>;
  enterMaintenance(node: bigint, drainForMs?: bigint): Promise<ChainCommit>;
  exitMaintenance(node: bigint): Promise<ChainCommit>;
  cordon(node: bigint): Promise<ChainCommit>;
  uncordon(node: bigint): Promise<ChainCommit>;
  dropReplicas(node: bigint, chains: bigint[]): Promise<ChainCommit>;
  invalidatePlacement(node: bigint): Promise<ChainCommit>;
  restartAllDaemons(node: bigint): Promise<ChainCommit>;
  clearAvoidList(node: bigint): Promise<ChainCommit>;
}

export class IceCommands {
  freezeCluster(ttlMs: bigint): IceProposal;
  thawCluster(): IceProposal;
  flushAvoidLists(scope: AvoidScope): IceProposal;
  forceEvictReplica(chain: bigint, victim: bigint): IceProposal;
  forceRestartDaemon(daemon: DaemonRef): IceProposal;
  forceCutover(chain: bigint, target: bigint): IceProposal;
  killMigration(migration: bigint): IceProposal;
  // No forceDrain — substrate-deferred.
}

export class IceProposal {
  simulate(): Promise<SimulatedIceProposal>;
  // No commit() — typestate enforced by class split.
}

export class SimulatedIceProposal {
  readonly blastRadius: BlastRadius;
  signingPayload(): Buffer;
  commit(signatures: OperatorSignature[]): Promise<ChainCommit>;
}

export class AuditQuery {
  recent(limit: number): this;
  byOperator(opId: bigint): this;
  between(startMs: bigint, endMs: bigint): this;
  forceOnly(): this;
  since(seq: bigint): this;
  collect(): Promise<AdminAuditRecord[]>;
  stream(): AsyncIterable<AdminAuditRecord>;
}
```

```ts
import { DeckClient, AvoidScope } from '@ai2070/net-sdk/deck';

const client = new DeckClient(runtime, opIdentity, {
  snapshotPollIntervalMs: 100, iceSignatureThreshold: 1,
});

for await (const snap of client.snapshots()) {
  renderTopology(snap.peers);
  if (snap.localMaintenance.kind === 'DrainFailed') {
    alert(`Drain failed: ${snap.localMaintenance.reason}`);
    break;
  }
}

const commit = await client.admin.enterMaintenance(0xABCDn, 600_000n);

const proposal = client.ice.freezeCluster(300_000n);
const simulated = await proposal.simulate();
console.log(`affects ${simulated.blastRadius.affectedNodes.length} nodes`);
if (await confirm('Continue?')) {
  const c = await simulated.commit([sigOp1, sigOp2]);
}

for await (const entry of client.audit().recent(100).stream()) {
  console.log(`${entry.committedAt} ${entry.operatorId} ${entry.eventKind}`);
}

for await (const r of client.subscribeLogs({ minLevel: 'warn', daemonId: 42n })) {
  console.log(r);
}
```

**AsyncIterable shim** lives in the wrapper tier — the cdylib exposes `nextSnapshot(streamHandle, timeoutMs?) → Promise<T | null>` and the TS shim adds `Symbol.asyncIterator`. Same pattern `bindings/node/meshdb.ts` already uses for query streams. Errors throw `DeckSdkError extends Error { kind: string }` / `IceError extends Error { kind: string }` with the discriminator parsed; `bindings/node/errors.ts`'s `classifyError` matcher extends to the deck kinds.

### 4. Go SDK (cgo) — design

**Tier layout.** Cdylib at `bindings/go/deck-ffi/` exporting `net_deck_*` C functions; Go wrapper at `bindings/go/net/deck.go`. No `sdk-go/` tier. Precedent: `bindings/go/meshdb-ffi/` for query/stream FFI; `bindings/go/net/meshdb.go` for the `context.Context`-cancellable channel pattern.

```go
// bindings/go/net/deck.go
package net

type DeckClient struct{ /* opaque */ }

func NewDeckClient(runtime *MeshOsRuntime, identity *OperatorIdentity,
                    config DeckClientConfig) (*DeckClient, error)

func (c *DeckClient) Snapshots(ctx context.Context) <-chan MeshOsSnapshot
func (c *DeckClient) StatusSummaryStream(ctx context.Context) <-chan StatusSummary
func (c *DeckClient) SubscribeLogs(ctx context.Context, filter LogFilter) <-chan LogRecord
func (c *DeckClient) SubscribeFailures(ctx context.Context, sinceSeq uint64) <-chan FailureRecord

func (c *DeckClient) Status() StatusSummary
func (c *DeckClient) StatusSummary() StatusSummary

func (c *DeckClient) Admin() *AdminCommands
func (c *DeckClient) ICE() *IceCommands
func (c *DeckClient) Audit() *AuditQuery

type AdminCommands struct{ /* opaque */ }
func (a *AdminCommands) Drain(ctx context.Context, node NodeId, drainFor time.Duration) (*ChainCommit, error)
func (a *AdminCommands) EnterMaintenance(ctx context.Context, node NodeId,
                                          drainFor *time.Duration) (*ChainCommit, error)
// ... etc — one method per AdminEvent variant.

type IceCommands struct{ /* opaque */ }
func (i *IceCommands) FreezeCluster(ttl time.Duration) *IceProposal
func (i *IceCommands) ThawCluster() *IceProposal
func (i *IceCommands) FlushAvoidLists(scope AvoidScope) *IceProposal
func (i *IceCommands) ForceEvictReplica(chain ChainId, victim NodeId) *IceProposal
func (i *IceCommands) ForceRestartDaemon(daemon DaemonRef) *IceProposal
func (i *IceCommands) ForceCutover(chain ChainId, target NodeId) *IceProposal
func (i *IceCommands) KillMigration(migration MigrationId) *IceProposal
// No ForceDrain — substrate-deferred.

type IceProposal struct{ /* opaque */ }
func (p *IceProposal) Simulate(ctx context.Context) (*SimulatedIceProposal, error)

type SimulatedIceProposal struct{ /* opaque */ }
func (s *SimulatedIceProposal) BlastRadius() BlastRadius
func (s *SimulatedIceProposal) SigningPayload() []byte
func (s *SimulatedIceProposal) Commit(ctx context.Context, sigs []OperatorSignature) (*ChainCommit, error)

type AuditQuery struct{ /* opaque */ }
func (q *AuditQuery) Recent(n int) *AuditQuery
func (q *AuditQuery) ByOperator(opId uint64) *AuditQuery
func (q *AuditQuery) Between(startMs, endMs uint64) *AuditQuery
func (q *AuditQuery) ForceOnly() *AuditQuery
func (q *AuditQuery) Since(seq uint64) *AuditQuery
func (q *AuditQuery) Collect(ctx context.Context) ([]AdminAuditRecord, error)
func (q *AuditQuery) Stream(ctx context.Context) <-chan AdminAuditRecord
```

```go
client, _ := net.NewDeckClient(runtime, opIdentity, net.DeckClientConfig{
    SnapshotPollInterval: 100 * time.Millisecond, IceSignatureThreshold: 1,
})

for snap := range client.Snapshots(ctx) {
    renderTopology(snap.Peers)
}

commit, _ := client.Admin().EnterMaintenance(ctx, 0xABCD,
    durPtr(10*time.Minute))

proposal := client.ICE().FreezeCluster(5 * time.Minute)
simulated, _ := proposal.Simulate(ctx)
if confirm() {
    c, _ := simulated.Commit(ctx, []net.OperatorSignature{sigOp1, sigOp2})
}

for entry := range client.Audit().Recent(100).Stream(ctx) {
    fmt.Printf("%d %d %s\n", entry.CommittedAt, entry.OperatorId, entry.EventKind)
}

for r := range client.SubscribeLogs(ctx, net.LogFilter{MinLevel: net.LogWarn, DaemonId: 42}) {
    fmt.Println(r)
}
```

Channels close on `ctx.Done()` or stream end. The cdylib spawns one goroutine per stream that pumps from the Rust side; `bindings/go/net/meshdb.go` is the structural template. Errors return `*DeckSdkError{Kind, Message}` (or `*IceError{Kind, Message}`) parsed from `<<deck-sdk-kind:KIND>>MSG`.

### 5. C SDK (raw FFI) — design

Header at `include/net_deck.h` (joins the existing `net.h` / `net_meshdb.h` / `net_meshos.h` set); cdylib shared with Go at `bindings/go/deck-ffi/`. Function-pointer-table callbacks for stream consumption; opaque pointers for `DeckClient` / `IceProposal` / `SimulatedIceProposal` / `AuditQuery`.

```c
// Lifecycle
NetDeckClient* net_deck_client_new(const NetMeshOsRuntime*,
                                    const NetOperatorIdentity*,
                                    const NetDeckClientConfig*);
void net_deck_client_free(NetDeckClient*);
int  net_deck_client_with_operator_registry(NetDeckClient*, const NetOperatorRegistry*);

// Snapshot stream — callback-driven; return 0 to continue, non-zero to stop.
typedef int (*NetSnapshotCallback)(void* ctx, const NetMeshOsSnapshot*);
NetSnapshotStream* net_deck_subscribe_snapshots(NetDeckClient*,
                                                 NetSnapshotCallback, void* ctx);
void net_deck_snapshot_stream_close(NetSnapshotStream*);

// Log / failure / audit streams — same callback pattern.
typedef int (*NetLogCallback)(void* ctx, const NetLogRecord*);
NetLogStream* net_deck_subscribe_logs(NetDeckClient*, const NetLogFilter*,
                                       NetLogCallback, void* ctx);
void net_deck_log_stream_close(NetLogStream*);
// ... net_deck_subscribe_failures, net_deck_audit_stream — identical shape.

// One-shots
int net_deck_status(NetDeckClient*, NetStatusSummary* out);

// Admin commits — one per variant.
int net_deck_admin_drain(NetDeckClient*, uint64_t node_id, uint64_t drain_for_ms,
                          NetChainCommit* out);
int net_deck_admin_enter_maintenance(NetDeckClient*, uint64_t node_id,
                                       uint64_t drain_for_ms, int has_drain_for,
                                       NetChainCommit* out);
int net_deck_admin_exit_maintenance(NetDeckClient*, uint64_t node_id, NetChainCommit* out);
int net_deck_admin_cordon(NetDeckClient*, uint64_t node_id, NetChainCommit* out);
int net_deck_admin_uncordon(NetDeckClient*, uint64_t node_id, NetChainCommit* out);
int net_deck_admin_drop_replicas(NetDeckClient*, uint64_t node_id,
                                  const uint64_t* chains, size_t chain_count,
                                  NetChainCommit* out);
int net_deck_admin_invalidate_placement(NetDeckClient*, uint64_t node_id, NetChainCommit* out);
int net_deck_admin_restart_all_daemons(NetDeckClient*, uint64_t node_id, NetChainCommit* out);
int net_deck_admin_clear_avoid_list(NetDeckClient*, uint64_t node_id, NetChainCommit* out);

// ICE — typestate via two opaque types. Caller MUST call
// net_deck_ice_simulate before net_deck_ice_commit; commit
// without a prior simulate yields IceError::SimulationRequired.
NetIceProposal* net_deck_ice_freeze_cluster(NetDeckClient*, uint64_t ttl_ms);
NetIceProposal* net_deck_ice_thaw_cluster(NetDeckClient*);
NetIceProposal* net_deck_ice_flush_avoid_lists(NetDeckClient*, NetAvoidScope);
NetIceProposal* net_deck_ice_force_evict_replica(NetDeckClient*, uint64_t chain, uint64_t victim);
NetIceProposal* net_deck_ice_force_restart_daemon(NetDeckClient*, const NetDaemonRef*);
NetIceProposal* net_deck_ice_force_cutover(NetDeckClient*, uint64_t chain, uint64_t target);
NetIceProposal* net_deck_ice_kill_migration(NetDeckClient*, uint64_t migration_id);
// No net_deck_ice_force_drain — substrate-deferred.

// simulate consumes the proposal and returns a simulated handle.
int net_deck_ice_simulate(NetIceProposal* /*consumed*/, NetSimulatedIceProposal** out);
void net_deck_ice_proposal_free(NetIceProposal*);

const NetBlastRadius* net_deck_simulated_blast_radius(const NetSimulatedIceProposal*);
int net_deck_simulated_signing_payload(const NetSimulatedIceProposal*,
                                        uint8_t** out, size_t* out_len);
int net_deck_ice_commit(NetSimulatedIceProposal* /*consumed*/,
                         const NetOperatorSignature* sigs, size_t sig_count,
                         NetChainCommit* out);
void net_deck_simulated_free(NetSimulatedIceProposal*);

// Audit query — fluent builder.
NetAuditQuery* net_deck_audit_query(NetDeckClient*);
void net_deck_audit_query_recent(NetAuditQuery*, size_t limit);
void net_deck_audit_query_by_operator(NetAuditQuery*, uint64_t op_id);
void net_deck_audit_query_between(NetAuditQuery*, uint64_t start_ms, uint64_t end_ms);
void net_deck_audit_query_force_only(NetAuditQuery*);
void net_deck_audit_query_since(NetAuditQuery*, uint64_t seq);
int  net_deck_audit_query_collect(NetAuditQuery*,
                                   NetAdminAuditRecord** out, size_t* out_count);
void net_deck_audit_query_free(NetAuditQuery*);

// Last-error surface (mirrors include/net_meshdb.h:467-480)
const char* net_deck_last_error_message(void);
const char* net_deck_last_error_kind(void);
void net_deck_clear_last_error(void);
```

Same `ffi_guard!` macro + thread-local `LAST_ERROR_*` surface as `net.h` / `net_meshdb.h` / (forthcoming) `net_meshos.h`. The last-error kind string is the substrate `<<deck-sdk-kind:KIND>>MSG` discriminator (or `<<meshos-sdk-kind:KIND>>MSG` when the underlying failure is a verifier rejection — bindings parse either prefix transparently). The example `examples/deck.c` walks one realistic operator workflow (load identity → subscribe snapshots → commit `enter_maintenance` → propose freeze → simulate → abort) analogous to `examples/meshdb.c`.

### 6. Wire / FFI contracts (shared across bindings)

- **`AdminEvent` serialization** — the existing postcard form. Deck SDK encodes operator-signed envelopes wrapping the event payload; substrate-side channel-auth verifies on commit.
- **`MeshOsSnapshot` serialization** — already public per `MESHOS_PLAN.md`'s Phase F snapshot work. Bindings deserialize directly; no Deck-specific transform.
- **`ActionChainRecord` serialization** — postcard, version-byte-prefixed per `chain.rs:WIRE_FORMAT_VERSION`. Audit queries decode existing records.
- **`IceProposal` shape** — postcard-encoded payload carrying `proposal_id` + `action_variant` + `proposed_at_ms` + `required_signatures`. Each binding's `IceProposal` is a thin wrapper that holds the payload + accumulates signatures before commit.
- **`OperatorSignature`** — `(operator_id, ed25519_signature_over_proposal_payload)` pair. Substrate-side verification re-encodes the payload deterministically + verifies each signature against the operator's known public key.
- **`BlastRadius`** — serializable; same `Serialize + Deserialize` story as `MeshOsSnapshot`. Bindings deserialize into language-native shapes.
- **`<<deck-sdk-kind:KIND>>MSG` error discriminator** — same regex MeshDB / MeshOS SDKs use. Kinds actually emitted by the Rust SDK today (parse-by-substring; bindings tolerate unknown kinds): admin/snapshot paths — `loop_closed`, `queue_full`, `unknown_node`, `unknown_chain`, `unknown_daemon`, `freeze_in_effect`. ICE verifier paths (under `<<meshos-sdk-kind:>>` since the verifier lives in `behavior::meshos::ice`) — `not_authorized`, `signature_invalid`, `insufficient_signatures`, `envelope_expired`, `envelope_from_future`, `simulation_required`, `ice_cooldown_active`. The original plan named `auth_failed` / `chain_commit_failed` / `simulate_failed` / `stream_closed` / `ice_locked_out` — those are NOT in code; bindings must wire the actual list.

### 7. ICE-specific safeguards

Phase 2 of this plan ships the ICE surface. It carries five disciplines beyond ordinary admin commits:

1. **Multi-operator signing.** `SimulatedIceProposal::commit(signatures)` requires `signatures.len() >= ice_threshold` (`DeckClientConfig::ice_signature_threshold`, default 1 today; bumped per-cluster via the operator-policy chain the substrate reads on startup). The substrate-side verifier re-checks the threshold at commit time — an SDK that lies about the threshold can't commit because the substrate validates.

   **Why M-of-N matters.** Routine admin commits (cordon, drain, etc.) are reversible and tolerate single-operator authority. ICE actions are not: `force_cutover` to a node that can't host the workload, a misfired `force_evict_replica` on the last healthy holder, a `kill_migration` mid-cutover, or a panic-fired `freeze_cluster` during reconcile can wedge or damage the cluster in ways an operator can't roll back without another break-glass action. Requiring `>= ice_threshold` distinct operator signatures over the same `(action, issued_at_ms, blast_hash)` tuple closes three concrete threats: (a) a single compromised keypair can't fire ICE alone; (b) a single panicked or mistaken operator has to convince a co-operator before commit; (c) insider unilateral action shows up in audit as sub-threshold sig bundles that the verifier rejected. The signing payload is domain-tagged (`ICE_SIGNING_DOMAIN`) so an ICE signature can't cross-validate as a routine admin commit, and the verifier deduplicates by `OperatorSignature::operator_id` so the same operator can't satisfy the threshold by signing twice.

   **What the threat model does not cover.** Both operator keypairs compromised in the same window, or operators colluding — neither is mitigated by M-of-N. Those require key-rotation cadence + operator-policy auditing, which live outside this plan.
2. **Blast-radius simulation.** `IceProposal::simulate() -> SimulatedIceProposal` is mandatory before commit — `commit` lives only on `SimulatedIceProposal`, so the typestate makes "commit without simulate" structurally impossible in typed languages. Dynamic-language bindings (Python, plain JS) reproduce the constraint by hiding `commit` on the proposal class and only surfacing it on the simulated class. Substrate-side `IceError::SimulationRequired` remains as a defense-in-depth check (returned through the binding's error envelope) for any binding that fails to enforce the split.
3. **Lockout timer.** After an ICE force-operation commits on a node, that node enters a 5-minute "ICE cooldown" during which subsequent ICE operations targeting the same node require an extra signature. The cooldown rides chain metadata; every node observes it identically.
4. **Dedicated audit subchain.** ICE force-operations land on a `ice.admin.<cluster>` subchain in addition to the main admin chain. The audit query has a `.force_only()` filter that reads only the ICE subchain, so security review can replay every break-glass event without scanning the full admin history.
5. **Visual confirmation gates (UI-side, not SDK-side).** Deck-the-binary requires a typed confirmation matching the proposed action's identifier before invoking `commit()`. The SDK doesn't enforce this — tenant tools written against the SDK can skip the confirmation if they have their own gating — but the Deck binary's UI layer ships the friction.

### 8. Tests

Per-binding unit tests + integration tests, mirroring the MeshDB / MeshOS SDK structure:

- **Per-binding unit tests** — language-native runner (`cargo test` / `pytest` / `vitest` / `go test` / a small C harness).
- **Per-binding integration tests** — register a real `DeckClient` against an in-process MeshOS runtime; commit a synthetic admin event; verify it lands on the admin chain + the snapshot reflects the post-commit state.
- **Cross-binding parity test** — one minimal Deck client per language commits a `clear_avoid_list(node)` admin event against a shared substrate fixture; the substrate verifies the commit landed identically regardless of source binding.
- **ICE-specific tests** — `commit()` without `simulate()` fails; `commit()` with fewer than `ice_threshold` signatures fails; `commit()` during lockout fails; blast-radius simulation matches the executor's actual behavior under a controlled fixture.
- **Non-goal enforcement** — compile-time for typed languages (no method exists for daemon registration; no surface for direct chain mutation; no surface for topology editing); runtime "no such attribute" for Python; "no exported function" greps for C. Same allowlist-checked-in-CI approach as MeshOS SDK.

### 9. Documentation

Per-binding README walking one realistic operator workflow end-to-end:

1. Load operator identity from the local maintenance node.
2. Subscribe to the snapshot stream + render a sample topology view.
3. Commit an `enter_maintenance` against a target node.
4. Watch the snapshot stream surface the `EnteringMaintenance` state transition.
5. Propose an ICE action (`freeze_cluster` is the canonical low-blast example since it's reversible via `thaw_cluster`); run `simulate()`; print the blast radius; abort. (`force_drain` is substrate-deferred — bindings should NOT include it in the v1 walkthrough.)
6. Query the audit chain for the last 10 admin commits; verify the `enter_maintenance` is there.

Each README matches the MeshDB / MeshOS SDK README format (slice-based, explicit "what ships in v1 vs deferred"). The C SDK ships a runnable `examples/deck.c` analogous to the existing `examples/meshdb.c`.

---

## Deferred work

### Cross-deck co-signing workflow

§7 #1 specifies *that* `IceProposal::commit` requires `signatures.len() >= ice_threshold` and *what* the substrate verifies. It does **not** specify *how* a second operator's signature reaches the originating deck. Today the substrate accepts the bundle and the SDK exposes the per-operator `OperatorIdentity::sign_proposal`, but there's no defined operator-facing workflow for the two (or more) deck instances to exchange signatures over an unsigned proposal.

Status: **indefinitely deferred**. The demo runtime ships with `ice_signature_threshold = 1` and the single-operator path is sufficient for current and foreseeable cluster operations; no near-term roadmap item drives multi-op coordination. The notes below capture the design surface so a future reviewer doesn't have to re-derive it — this is reference material, not a queued slice.

If a consumer ever revives the workflow the SDK additions are bounded:

- **Expose offline-signing primitives.** Re-export `BlastRadiusHash`, `blast_radius_hash(&BlastRadius) -> BlastRadiusHash`, and `ice_proposal_signing_payload(&IceActionProposal, issued_at_ms, &BlastRadiusHash) -> Vec<u8>` from `net_sdk::deck::*`. The substrate already implements all three at `behavior::meshos::ice`; they're internal-only today because no consumer needs them.
- **Add a serializable proposal bundle.** A new `IceProposalBundle { action: IceActionProposal, issued_at_ms: u64, blast: BlastRadius }` carries everything a remote operator needs to (a) re-derive the same `blast_hash` locally and (b) sign over the same domain-tagged payload. All three fields are already `Serialize + Deserialize`; the bundle is a thin tuple type with one helper: `bundle.signing_payload() -> Vec<u8>` (delegates to `ice_proposal_signing_payload`).
- **Document the round-trip.** Operator A's deck simulates and produces a bundle; A signs locally and exports `(bundle, sig_a)` as a postcard or JSON blob (paste / file / out-of-band channel); operator B's deck imports the bundle, verifies its own re-derived `blast_hash` matches what A signed over, signs locally, exports `sig_b`; A imports `sig_b` and commits with `&[sig_a, sig_b]`. The substrate verifier rebuilds the signing payload from `(action, issued_at_ms, blast_hash)` so both signatures bind to the exact same envelope or commit fails closed.

The deck-binary surface this would unlock (export bundle, import signature, commit modal that shows collected-so-far vs required) is **not a planned feature**. This section exists to pin the design if anyone picks it up later.

### Things explicitly not deferred to this section

- Substrate-side multi-signature verification — already implemented and tested at `behavior::deck::SimulatedIceProposal::commit`.
- Threshold configuration — already plumbed via `DeckClientConfig::ice_signature_threshold`.
- Distinct-operator deduplication — already enforced (`unique_operators` set in the verifier).

What's deferred is the **operator-facing exchange protocol**, not the substrate's verification semantics.

---

## Locked decisions

Lock these so phase implementations don't relitigate:

1. **Deck SDK is operator-side only.** No daemon-authoring surface in any binding. Daemons author against the MeshOS SDK; operators command against this one.
2. **Every action signed with the operator key.** No "unsigned commit" path, no "trust the client" override. The substrate's channel-auth guard is the single verification point; the SDK is the issuer side.
3. **ICE commits require multi-operator signing.** Threshold is `DeckClientConfig::ice_signature_threshold` (default 1 today, raised per-cluster via the operator-policy chain). The substrate-side verifier enforces with distinct-operator dedup; the SDK builds the proposal envelope.
4. **Blast-radius simulation is mandatory before ICE commit.** Substrate-side contract; SDK plumbs the API. No "skip the preview" shortcut in any binding.
5. **Snapshot subscription is read-only.** The stream type carries no mutation surface; it's purely a tail over the action chain via MeshDB.
6. **The SDK never bypasses MeshOS.** Even ICE force-operations commit to the admin chain so reconcile observes them. The force-* variants set bits the substrate honors at the relevant gate (rate-limit, hysteresis, cooldown); they don't reach around the loop.
7. **Per-language async model is native.** Sync Python (with async wrappers as a slice 2). Async-iterable Node. Channels + `context.Context` Go. Vtable + blocking polls C. Tokio async on the Rust handle methods.
8. **Error kinds use `<<deck-sdk-kind:KIND>>MSG`.** Same parsing approach as MeshDB / MeshOS SDKs.
9. **Operator-identity loading is one-shot per `DeckClient`.** No re-load mid-lifetime; rotation requires a new client. Forces clean lifecycle around key rotation.
10. **No surface for adding / removing nodes.** Topology editing is substrate-level identity work (`KEY_MIGRATION_PLAN.md`, future `NODE_LIFECYCLE_PLAN.md`); the Deck SDK loads / commands against existing nodes.
11. **Audit queries are read-only and ride MeshDB.** No "delete audit entry" surface in any binding, ever. The audit chain is append-only by substrate contract; the SDK respects that.
12. **The Rust SDK is the canonical surface; Deck-the-binary is its first consumer.** Other-language bindings ride the wire / FFI contracts the Rust SDK pins. Drift between bindings is closed by cross-binding parity tests in CI.

---

## Phases

Activation order, dependency-driven:

- **Phase 1 — Rust SDK: snapshot subscription + ordinary admin commits + audit queries + log stream. SHIPPED.** `DeckClient`, all 9 `AdminCommands`, `SnapshotStream` / `LogStream` / `FailureStream` / `AuditStream` / `StatusSummaryStream`, `AuditQuery` fluent builder. Deck-the-binary at `crates/net/deck/src/{app,lineage}.rs` is the first consumer. Phase 1 ships **non-signing** (operator ID recorded, channel-auth seam not yet engaged) and reads admin audit from the local in-memory ring rather than a signed MeshDB-backed admin chain — both are deliberate constraints documented in the Status section.
- **Phase 2 — Substrate-side: ICE `IceActionProposal` variants + multi-operator signing + blast-radius simulator + ICE subchain. SHIPPED** (except `ForceDrain`). `behavior::meshos::ice` ships: 7 `IceActionProposal` variants, `OperatorRegistry::verify_bundle` with distinct-operator dedup, `simulate_ice_proposal(&snapshot, &proposal) → BlastRadius`, ICE cooldown timer (`IceCooldownActive`), domain-tagged signing payload, freshness/skew checks (`EnvelopeExpired` / `EnvelopeFromFuture`). **`ForceDrain` deferred** — noted as "future slices" in the substrate annotation; routine `Drain` remains in `AdminCommands`.
- **Phase 3 — Rust SDK: ICE surface. SHIPPED.** `IceCommands` (7 factories), `IceProposal::simulate() → SimulatedIceProposal::commit(&[OperatorSignature])` typestate, re-exported `BlastRadius` + `BlastWarning` + `simulate_ice_proposal` for the deck binary's pre-confirmation preview.
- **Phase 4 — Python SDK. SLICES 1+2+3 SHIPPED; OPERATOR-VERIFIER SURFACE PENDING.** `bindings/python/src/deck.rs` + `sdk-py/src/net_sdk/deck.py`. Slice 1 — `DeckClient` + `snapshots()` iterator + `status_summary_stream` + `AdminCommands` (9 methods) **shipped**. Slice 2 — `AuditQuery` fluent builder + `subscribe_logs` + `subscribe_failures` **shipped**. Slice 3 — `IceCommands` (7 factories) + typestate via separate `IceProposal` / `SimulatedIceProposal` classes (`commit` hidden on `IceProposal`) + `OperatorIdentity` (generate / from_seed / from_identity / operator_id) **shipped**. **Pending:** `OperatorRegistry` (`insert` / `register` / `verify` / `verify_bundle`), `AdminVerifier` (`new` / `with_freshness` / `with_full_policy`), `VerifyError` enum, `OperatorIdentity.sign_proposal()` helper.
- **Phase 5 — Node / TypeScript SDK. SLICES 1+2+3 SHIPPED; OPERATOR-VERIFIER SURFACE PENDING.** `bindings/node/src/deck.rs` + `sdk-ts/src/deck.ts`. Slice 1 — `DeckClient` + `snapshots()` AsyncIterable + `statusSummaryStream` + `AdminCommands` (9 methods) **shipped**. Slice 2 — `AuditQuery` (`recent` / `byOperator` / `between` / `forceOnly` / `since` / `collect` / `stream`) + `subscribeLogs` + `subscribeFailures` **shipped**. Slice 3 — `IceCommands` (7 factories) + typestate via separate `IceProposal` / `SimulatedIceProposal` napi classes + `OperatorIdentity` (generate / fromSeed / fromIdentity / operatorId) **shipped**. **Pending:** same operator-verifier surface as Python.
- **Phase 6 — Go SDK. SLICES 1+2+3 SHIPPED; OPERATOR-VERIFIER SURFACE PENDING.** `bindings/go/deck-ffi/src/lib.rs` + `bindings/go/net/deck.go`. Slice 1 — `DeckClient` + `Snapshots(ctx)` channel + admin methods **shipped**. Slice 2 — `AuditQuery` fluent builder + `SubscribeLogs(ctx, filter)` + `SubscribeFailures(ctx, sinceSeq)` **shipped**. Slice 3 — `IceCommands` (7 factories) + separate `IceProposal` / `SimulatedIceProposal` opaque types + `OperatorIdentity` **shipped**. **Pending:** `OperatorRegistry` + `AdminVerifier` + `VerifyError` Go types + `SignProposal` helper.
- **Phase 7 — C SDK. SLICES 1+2+3 SHIPPED; OPERATOR-VERIFIER SURFACE PENDING.** `include/net_deck.h` + the shared `deck-ffi` cdylib. Slice 1 — `net_deck_client_new` / admin commits / `net_deck_subscribe_snapshots` + last-error trio **shipped**. Slice 2 — `net_deck_audit_query_*` builder + `net_deck_subscribe_logs` + `net_deck_subscribe_failures` callback APIs **shipped**. Slice 3 — ICE typestate via two distinct opaque handles (`NetDeckIceProposal` + `NetDeckSimulatedIceProposal`) + 7 factory functions + simulate/commit/blast_radius/blast_hash **shipped**. **Pending:** `net_deck_operator_registry_*` + `net_deck_admin_verifier_*` + `net_deck_operator_sign_proposal` exports + the C declarations for `NetDeckOperatorRegistry` / `NetDeckAdminVerifier` / `NetDeckVerifyError`.

Phases 4–7 land independently of each other; Phases 1–3 (all shipped) were a hard prereq chain. Each per-language slice can ship partial surface (e.g., Python ships snapshot + admin in slice 1, ICE + audit in slice 2) as long as the slice list converges on the full operator-command surface before declaring "v1 done."

### Remaining work — concrete punch list

**Phases 4–7 — operator-verifier surface (consistent across all four languages):**
- `OperatorRegistry` — `new` / `insert(op_id, public_key)` / `register(&keypair)` / `verify(sig, payload)` / `verify_bundle(sigs, payload, threshold)`. Distinct-operator dedup is substrate-enforced.
- `AdminVerifier` — `new(registry)` / `with_freshness(window)` / `with_full_policy(policy)` constructors plus the verify entry point.
- `VerifyError` — surface all 7 variants (`not_authorized` / `signature_invalid` / `insufficient_signatures` / `envelope_expired` / `envelope_from_future` / `simulation_required` / `ice_cooldown_active`) as a typed enum on each language.
- `OperatorIdentity.sign_proposal(payload) -> OperatorSignature` — the helper that signs an ICE proposal's domain-tagged payload bytes with the operator's keypair (substrate ships it on `OperatorIdentity::keypair()`; the binding adds the convenience wrapper).

**Cross-cutting — offline signing primitives** (per the **Deferred work / Cross-deck co-signing workflow** section; status indefinitely deferred unless a consumer revives the workflow):
- Re-export `BlastRadiusHash`, `blast_radius_hash(&BlastRadius) -> BlastRadiusHash`, `ice_proposal_signing_payload(&IceActionProposal, issued_at_ms, &BlastRadiusHash) -> Vec<u8>` from each binding.
- `IceProposalBundle { action, issued_at_ms, blast }` (Serialize + Deserialize) + `bundle.signing_payload()` helper for paste/file/out-of-band exchange between operator decks.

**Cross-cutting — examples + parity:**
- Runnable per-binding example (`examples/deck.{c,py,ts,go}`) walking the §9 workflow: load identity → snapshots → enter_maintenance → ICE freeze simulate → audit recent(10) → abort.
- Cross-binding parity test (§7 / §8) — one minimal Deck client per language commits a `clear_avoid_list(node)` against a shared substrate fixture; the substrate verifies the commit landed identically regardless of source binding.

---

## Non-goals

Per the scope brief, the SDK is not:

- A daemon-authoring surface (that's [`MESHOS_SDK_PLAN.md`](MESHOS_SDK_PLAN.md)).
- A MeshDB query-construction surface (that's the MeshDB SDK).
- A topology / identity management surface (substrate-level identity work).
- A generic mesh-administration surface outside what the admin chain supports.
- A direct-chain-mutation surface (everything signed + admin-chain-routed).
- A UI rendering / layout / terminal-control surface (Deck-the-binary).
- A bypass for channel-auth verification (substrate-internal).

The Deck SDK is **the operator-command contract, exposed in five languages**. Everything else stays inside the substrate, the MeshOS SDK, the MeshDB SDK, or Deck-the-binary's UI layer.

---

## Interaction surfaces

The SDK interacts with five substrate systems per binding:

- **MeshOS admin chain** — for signed action commits. Every operator command becomes an `AdminEvent` (or ICE `Force*` variant) postcard-encoded + signed + committed.
- **MeshOS snapshot chain** — for live state subscription. Tail via MeshDB's `MeshQuery::Latest` plus continuation tokens.
- **MeshDB federated executor** — for audit-chain queries. Compiles the operator's audit filter into `MeshQuery::{Filter, Between, Latest}`.
- **RedEX `tail()`** — for log-stream subscription. Per-node / per-daemon log chains; multiplexed by `LogFilter`.
- **Channel-auth + operator identity** — for action signing. Operator key loads via the existing identity-layer surface; signatures verify substrate-side at chain commit.

The SDK explicitly does NOT interact with:

- **Daemon registry directly.** Daemon supervision commands (`restart_all_daemons`, force-restart) ride the admin chain; the SDK doesn't reach into `DaemonRegistry`.
- **MeshOS internal state.** Avoid lists, backpressure flags, scheduler config, fold internals — all opaque. The SDK reads only the published snapshot.
- **The action executor's dispatcher.** Dispatcher hooks are MeshOS-internal; the SDK observes outcomes through the chain replay + snapshot, not by intercepting dispatch.

---

## Test surface

Following the MeshDB / MeshOS SDK precedent:

- **Per-binding unit tests** — language-native test runner. Type-level + shape-level coverage of every public surface.
- **Per-binding integration tests** — register a real `DeckClient` against an in-process MeshOS runtime; commit synthetic admin events; verify chain landings + snapshot reflection.
- **Cross-binding parity test** — one minimal Deck client per language commits a `clear_avoid_list(node)` admin event against a shared substrate fixture; the substrate verifies the commit landed identically regardless of source binding.
- **ICE discipline tests** — `commit()` without `simulate()` fails; sub-threshold signature bundles fail; lockout-window commits fail; blast-radius matches the controlled fixture's actual reconcile output.
- **Non-goal enforcement** — compile-time for typed languages; runtime "no such attribute" for Python; "no exported function" greps for C. CI checks an explicit allowlist of public symbols per binding so future contributors can't widen the surface without an explicit plan amendment.
- **Property tests** — round-trip every wire contract (`AdminEvent` envelope, `IceProposal` payload, `BlastRadius`, `MeshOsSnapshot`, `ActionChainRecord` via Deck-SDK reader) through postcard + JSON; pin the discriminator-stability contract Deck consumers depend on.

---

*Atomic Playboys (post-`MESHOS_SDK_PLAN.md`) release candidate. Gates on a real Deck binary consumer; the Rust SDK lands first as Deck's data + action plane, with the other four bindings following as tenant operator tooling demands. The substrate work in Phase 2 (ICE variants + multi-signing + blast-radius simulator + ICE subchain) is the only new substrate slice; everything else binds against surfaces v0.17 already shipped.*
