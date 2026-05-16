## Deck SDK — implementation plan

> Operator-side bindings: live `MeshOsSnapshot` subscription, signed admin-chain commits, audit-chain queries, log-stream subscription, and the **ICE** (break-glass) surface. The dual of [`MESHOS_SDK_PLAN.md`](MESHOS_SDK_PLAN.md) — that one ships the daemon-author trait, this one ships the cluster-operator surface. Five languages mirroring the precedent: Rust (canonical, also powers the Deck binary), Python (pyo3), Node / TypeScript (napi-rs), Go (cgo), C (raw FFI). Companion to [`MESHOS_PLAN.md`](MESHOS_PLAN.md) (the substrate it commands against), [`MESHOS_SDK_PLAN.md`](MESHOS_SDK_PLAN.md) (the type-shape parent), [`MESHDB_PLAN.md`](MESHDB_PLAN.md) (the federated-query plane Deck composes for snapshot / audit reads), and [`DECK_FEATURES.md`](DECK_FEATURES.md) (the product brief this plan turns into shippable phases). **Atomic Playboys release** per [`RELEASE_ROADMAP.md`](RELEASE_ROADMAP.md); follows the MeshOS SDK.

## Status

Design only. Substrate prereqs all in code as of v0.17: MeshOS pipeline (`MESHOS_PLAN.md` Phases A–G + executor + snapshot reader + scheduler + chain integration), the admin-chain commit path (`behavior::meshos::event::AdminEvent`), MeshDB federated query plane (`MESHDB_PLAN.md`), channel-auth guards (`CHANNEL_AUTH_GUARD_PLAN.md`), and the operator-identity layer at `behavior::safety::AuditSink`. This plan binds against those surfaces.

Activation gate: a Deck consumer driving the operator workflows enumerated in [`DECK_FEATURES.md`](DECK_FEATURES.md) — the Rust SDK + a working Deck binary land together as the first slice, with the other four bindings following as tenant tooling requires. Without a Deck consumer the SDK is a surface looking for a UI; with it, it's the cyberdeck.

**Substrate prereqs** (all in code today, v0.17):

- **`AdminEvent` enum + admin chain** at `src/adapter/net/behavior/meshos/event.rs`. Every operator command this SDK exposes already rides this enum; the SDK adds the *issuer* side (signing + commit) without touching the consumer side (the MeshOS fold).
- **`MeshOsSnapshot` + `RedexFold<MeshOsSnapshot>`** at `src/adapter/net/behavior/meshos/{snapshot, chain}.rs`. The serializable projection Deck renders; subscription rides MeshDB's federated executor against the snapshot chain.
- **`ActionChainRecord`** at `src/adapter/net/behavior/meshos/chain.rs`. Postcard-versioned per-action wire form. Deck reads the historical action chain through this to drive the Behavior Timeline + audit views.
- **MeshDB federated executor** at `src/adapter/net/behavior/meshdb/`. The SDK's snapshot / audit queries compile to `MeshQuery::{Latest, Between, Filter}` and ride the existing executor — no new wire protocol.
- **Channel-auth + `OperatorIdentity`** at `src/adapter/net/identity/` and `behavior::safety`. Operator-key loading, per-action signing, signature verification.
- **MeshOS SDK type shapes** at `MESHOS_SDK_PLAN.md`. `MetadataView`, `DaemonHealth`, `DaemonControl`, `MeshOsSnapshot`, `<<…-sdk-kind:KIND>>MSG` error discriminator — the Deck SDK re-uses them verbatim rather than redefining.

**Substrate gaps this plan introduces:**

- **No "force" variants on `AdminEvent` today.** ICE force-drain / force-evict / freeze-cluster / kill-migration need new chain-committable variants. The SDK design pins their shape; the substrate slice that adds them (with channel-auth multi-operator gating) lands alongside Phase 2.
- **No live `MeshOsSnapshot` subscription path today.** The substrate ships point-in-time `MeshOsSnapshotReader::read()` + the `MeshOsSnapshotFold` over the action chain; what's missing is the streaming "tail with replay" path Deck needs. The SDK design pins the API; the substrate adds a `tail()` over the action chain that the snapshot fold can attach to.
- **No blast-radius simulation surface.** ICE actions need a pre-commit "what does this touch" preview against the current snapshot. The SDK's `IceProposal::simulate()` is the entry point; the substrate-side simulator is a new module that re-runs the relevant reconcile arms against a hypothetical state diff.

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

### 1. Rust SDK (canonical)

Lives in `src/adapter/net/sdk/deck/` (sibling to `sdk/meshos/` from the MeshOS SDK plan).

```rust
pub struct DeckClient {
    mesh: Arc<MeshNode>,
    identity: OperatorIdentity,
    config: DeckClientConfig,
}

impl DeckClient {
    pub fn new(mesh: Arc<MeshNode>, identity: OperatorIdentity) -> Self;

    /// Live snapshot stream. Tail over the action chain;
    /// surfaces the freshest `MeshOsSnapshot` per Tick.
    pub fn snapshots(&self) -> SnapshotStream;

    /// Subscribe to per-node / per-daemon log chains.
    pub fn subscribe_logs(&self, filter: LogFilter) -> LogStream;

    /// Signed admin-event commits (ordinary, single-signature).
    pub fn admin(&self) -> AdminCommands<'_>;

    /// Break-glass surface (multi-signature, blast-radius gated).
    pub fn ice(&self) -> IceCommands<'_>;

    /// Audit chain queries (composes against MeshDB).
    pub fn audit(&self) -> AuditQuery<'_>;
}

pub struct AdminCommands<'a> { /* ... */ }

impl<'a> AdminCommands<'a> {
    pub async fn drain(&self, node: NodeId, deadline: Instant)
        -> Result<ChainCommit, AdminError>;
    pub async fn enter_maintenance(&self, node: NodeId, deadline: Option<Instant>)
        -> Result<ChainCommit, AdminError>;
    pub async fn exit_maintenance(&self, node: NodeId)
        -> Result<ChainCommit, AdminError>;
    pub async fn cordon(&self, node: NodeId) -> Result<ChainCommit, AdminError>;
    pub async fn uncordon(&self, node: NodeId) -> Result<ChainCommit, AdminError>;
    pub async fn drop_replicas(&self, node: NodeId, chains: Vec<ChainId>)
        -> Result<ChainCommit, AdminError>;
    pub async fn invalidate_placement(&self, node: NodeId)
        -> Result<ChainCommit, AdminError>;
    pub async fn restart_all_daemons(&self, node: NodeId)
        -> Result<ChainCommit, AdminError>;
    pub async fn clear_avoid_list(&self, node: NodeId)
        -> Result<ChainCommit, AdminError>;
}

pub struct IceCommands<'a> { /* ... */ }

impl<'a> IceCommands<'a> {
    /// Propose an ICE operation. Returns a proposal that must
    /// be simulated + multi-signed before commit.
    pub fn force_drain(&self, node: NodeId) -> IceProposal;
    pub fn force_evict_replica(&self, chain: ChainId, victim: NodeId) -> IceProposal;
    pub fn force_restart_daemon(&self, daemon: DaemonRef) -> IceProposal;
    pub fn force_cutover(&self, chain: ChainId, target: NodeId) -> IceProposal;
    pub fn kill_migration(&self, migration: MigrationId) -> IceProposal;
    pub fn freeze_cluster(&self, ttl: Duration) -> IceProposal;
    pub fn thaw_cluster(&self) -> IceProposal;
    pub fn flush_avoid_lists(&self, scope: AvoidScope) -> IceProposal;
}

pub struct IceProposal { /* opaque */ }

impl IceProposal {
    /// Pre-execution preview. Runs the relevant reconcile arms
    /// against a hypothetical post-action state diff; reports
    /// the affected nodes / replicas / daemons.
    pub async fn simulate(&self) -> Result<BlastRadius, IceError>;

    /// Commit with the supplied signatures. Substrate-side
    /// verification enforces the configured M-of-N threshold;
    /// fewer signatures than required surfaces
    /// `IceError::InsufficientSignatures`.
    pub async fn commit(self, signatures: &[OperatorSignature])
        -> Result<ChainCommit, IceError>;
}

pub struct BlastRadius {
    pub affected_nodes: Vec<NodeId>,
    pub affected_replicas: Vec<ChainId>,
    pub affected_daemons: Vec<DaemonRef>,
    pub estimated_drain_delay: Option<Duration>,
    pub placement_stability_delta: f32,
    pub warnings: Vec<BlastWarning>,
}
```

**`SnapshotStream`** is an `impl Stream<Item = Result<MeshOsSnapshot, StreamError>>`. The implementation tails the MeshOS action chain via MeshDB's `MeshQuery::Latest` plus a continuation token; on every new commit the fold re-runs and the next snapshot publishes. Tests use a `MockSnapshotStream` that emits fixture snapshots without a real chain.

**`LogStream`** is similarly `impl Stream<Item = Result<LogLine, StreamError>>`. Composes against the existing RedEX `tail()` API; each daemon's log lines live on its own chain, multiplexed into a single stream by the `LogFilter`.

**`AuditQuery`** is a fluent builder that compiles to `MeshQuery` against the admin-event chain. `recent(limit)`, `by_operator(op_id)`, `between(start, end)`, `for_node(node_id)`, `force_only()`. Returns `Stream<Item = AdminCommit>`.

**`OperatorIdentity`** is loaded from a maintenance node's identity store at `DeckClient::new`. The SDK never creates identities; it loads + uses them. Key rotation goes through the same chain commit path (`AdminEvent::OperatorKeyRotation` — a new variant added in Phase 2 of this plan).

### 2. Python SDK (pyo3)

Sync-first, matching the MeshDB + MeshOS SDK precedent.

```python
import net.deck as deck

client = deck.DeckClient(node, operator_identity)

# Live snapshot subscription
for snap in client.snapshots():
    render_topology(snap.peers)
    if snap.local_maintenance.kind == "DrainFailed":
        alert(f"Drain failed: {snap.local_maintenance.reason}")

# Ordinary admin commit
commit = client.admin.enter_maintenance(
    node=0xABCD,
    deadline_ms=600_000,
)
print(f"committed at seq {commit.seq}")

# ICE — break-glass
proposal = client.ice.force_drain(0xABCD)
blast = proposal.simulate()
print(f"affects {len(blast.affected_nodes)} nodes, "
      f"{len(blast.affected_replicas)} replicas")
if confirm("Continue?"):
    commit = proposal.commit([sig_op1, sig_op2])

# Audit
for entry in client.audit.recent(limit=100):
    print(f"{entry.committed_at} {entry.operator} {entry.event_kind}")

# Logs
for line in client.subscribe_logs(level="warn", daemon="telemetry"):
    print(line)
```

`SnapshotStream` is a Python iterator (`__iter__` + `__next__`). `LogStream` is the same. `AuditQuery` returns an iterator over `AdminCommit` objects. Async wrappers (`asnapshots()`, `asubscribe_logs(...)`) land in a slice 2 if a consumer asks for pyo3-asyncio.

### 3. Node / TypeScript SDK (napi-rs)

AsyncIterable for streams; promise-based for one-shots.

```ts
import { DeckClient, type MeshOsSnapshot, AvoidScope } from '@ai2070/net/deck';

const client = new DeckClient(node, operatorIdentity);

// Live snapshot
for await (const snap of client.snapshots()) {
    renderTopology(snap.peers);
    if (snap.localMaintenance.kind === 'DrainFailed') {
        alert(`Drain failed: ${snap.localMaintenance.reason}`);
    }
}

// Ordinary admin commit
const commit = await client.admin.enterMaintenance({
    node: 0xABCDn,
    deadlineMs: 600_000n,
});

// ICE
const proposal = await client.ice.forceDrain(0xABCDn);
const blast = await proposal.simulate();
console.log(`affects ${blast.affectedNodes.length} nodes, ${blast.affectedReplicas.length} replicas`);
if (await confirm('Continue?')) {
    const commit = await proposal.commit([sigOp1, sigOp2]);
}

// Audit
for await (const entry of client.audit.recent({ limit: 100 })) {
    console.log(`${entry.committedAt} ${entry.operator} ${entry.eventKind}`);
}

// Logs
for await (const line of client.subscribeLogs({ level: 'warn', daemon: 'telemetry' })) {
    console.log(line);
}
```

AsyncIterable matches the MeshDB Node binding's `for await` ergonomics — the same TS shim that adds `Symbol.asyncIterator` over a raw `next()` napi method.

### 4. Go SDK (cgo)

Channels + `context.Context` per Go idiom.

```go
import "github.com/ai-2070/net/go/deck"

client, err := deck.NewClient(node, opIdentity)

// Live snapshot
for snap := range client.Snapshots(ctx) {
    renderTopology(snap.Peers)
}

// Ordinary admin
commit, err := client.Admin.EnterMaintenance(ctx, deck.EnterMaintenanceRequest{
    Node:        0xABCD,
    DeadlineDur: 10 * time.Minute,
})

// ICE
prop := client.ICE.ForceDrain(0xABCD)
blast, err := prop.Simulate(ctx)
if blast.AffectedNodes > 0 && confirm() {
    commit, err := prop.Commit(ctx, []deck.OperatorSignature{sigOp1, sigOp2})
}

// Audit
audit := client.Audit.Recent(ctx, 100)
for entry := range audit {
    fmt.Printf("%s %s %s\n", entry.CommittedAt, entry.Operator, entry.EventKind)
}

// Logs
logs := client.SubscribeLogs(ctx, deck.LogFilter{Level: "warn", Daemon: "telemetry"})
for line := range logs {
    fmt.Println(line)
}
```

Channels close on context cancellation. The cdylib spawns one goroutine per stream that pumps from the Rust side; the same pattern the MeshDB Go SDK uses for query result streaming.

### 5. C SDK (raw FFI)

Vtable for stream callbacks; blocking polls for synchronous operations. Header at `bindings/go/meshos-ffi/include/net_deck.h` (shared cdylib with MeshOS SDK).

```c
// Lifecycle
NetDeckClient* net_deck_client_new(const NetMeshNode*, const NetOperatorIdentity*);
void net_deck_client_free(NetDeckClient*);

// Snapshot stream
typedef int (*NetSnapshotCallback)(void* ctx, const NetMeshOsSnapshot*);
NetSnapshotStream* net_deck_subscribe_snapshots(NetDeckClient*,
                                                 NetSnapshotCallback, void* ctx);
void net_deck_snapshot_stream_close(NetSnapshotStream*);

// Ordinary admin commit
int net_deck_admin_drain(NetDeckClient*, uint64_t node_id, uint64_t deadline_ms,
                          NetChainCommit* out);
int net_deck_admin_enter_maintenance(NetDeckClient*, uint64_t node_id,
                                       uint64_t deadline_ms, int has_deadline,
                                       NetChainCommit* out);
// ... etc per admin variant

// ICE
NetIceProposal* net_deck_ice_force_drain(NetDeckClient*, uint64_t node_id);
int net_deck_ice_simulate(NetIceProposal*, NetBlastRadius* out);
int net_deck_ice_commit(NetIceProposal*, const NetOperatorSignature* sigs,
                          size_t sig_count, NetChainCommit* out);
void net_deck_ice_proposal_free(NetIceProposal*);

// Audit + logs identical pattern
// ...

// Last-error surface (mirrors MeshDB / MeshOS SDK pattern)
const char* net_deck_last_error_message(void);
const char* net_deck_last_error_kind(void);
void net_deck_clear_last_error(void);
```

Same `ffi_guard!` macro + thread-local `LAST_ERROR_*` surface MeshDB / MeshOS SDKs use. Last-error kind format `<<deck-sdk-kind:KIND>>MSG` for cross-binding parsing parity.

### 6. Wire / FFI contracts (shared across bindings)

- **`AdminEvent` serialization** — the existing postcard form. Deck SDK encodes operator-signed envelopes wrapping the event payload; substrate-side channel-auth verifies on commit.
- **`MeshOsSnapshot` serialization** — already public per `MESHOS_PLAN.md`'s Phase F snapshot work. Bindings deserialize directly; no Deck-specific transform.
- **`ActionChainRecord` serialization** — postcard, version-byte-prefixed per `chain.rs:WIRE_FORMAT_VERSION`. Audit queries decode existing records.
- **`IceProposal` shape** — postcard-encoded payload carrying `proposal_id` + `action_variant` + `proposed_at_ms` + `required_signatures`. Each binding's `IceProposal` is a thin wrapper that holds the payload + accumulates signatures before commit.
- **`OperatorSignature`** — `(operator_id, ed25519_signature_over_proposal_payload)` pair. Substrate-side verification re-encodes the payload deterministically + verifies each signature against the operator's known public key.
- **`BlastRadius`** — serializable; same `Serialize + Deserialize` story as `MeshOsSnapshot`. Bindings deserialize into language-native shapes.
- **`<<deck-sdk-kind:KIND>>MSG` error discriminator** — same regex MeshDB / MeshOS SDKs use. Kinds: `auth_failed`, `signature_invalid`, `insufficient_signatures`, `chain_commit_failed`, `simulate_failed`, `stream_closed`, `not_authorized`, `unknown_node`, `unknown_chain`, `unknown_daemon`, `ice_locked_out`, `freeze_in_effect`.

### 7. ICE-specific safeguards

Phase 2 of this plan ships the ICE surface. It carries five disciplines beyond ordinary admin commits:

1. **Multi-operator signing.** `IceProposal::commit(signatures)` requires `signatures.len() >= ice_threshold` (default 2; configurable per-cluster via a `safety.toml` operator-policy chain that the substrate reads on startup). The substrate-side verifier re-checks the threshold at commit time — an SDK that lies about the threshold can't commit because the substrate validates.

   **Why M-of-N matters.** Routine admin commits (cordon, drain, etc.) are reversible and tolerate single-operator authority. ICE actions are not: `force_cutover` to a node that can't host the workload, a misfired `force_evict_replica` on the last healthy holder, a `kill_migration` mid-cutover, or a panic-fired `freeze_cluster` during reconcile can wedge or damage the cluster in ways an operator can't roll back without another break-glass action. Requiring `>= ice_threshold` distinct operator signatures over the same `(action, issued_at_ms, blast_hash)` tuple closes three concrete threats: (a) a single compromised keypair can't fire ICE alone; (b) a single panicked or mistaken operator has to convince a co-operator before commit; (c) insider unilateral action shows up in audit as sub-threshold sig bundles that the verifier rejected. The signing payload is domain-tagged (`ICE_SIGNING_DOMAIN`) so an ICE signature can't cross-validate as a routine admin commit, and the verifier deduplicates by `OperatorSignature::operator_id` so the same operator can't satisfy the threshold by signing twice.

   **What the threat model does not cover.** Both operator keypairs compromised in the same window, or operators colluding — neither is mitigated by M-of-N. Those require key-rotation cadence + operator-policy auditing, which live outside this plan.
2. **Blast-radius simulation.** `IceProposal::simulate()` is mandatory before commit in the substrate-side contract — proposals committed without a prior `simulate()` invocation in the same `IceProposal` lifetime fail with `IceError::SimulationRequired`. Each binding plumbs the simulation API + makes `commit()` without a prior `simulate()` a compile-time error where the language allows.
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
5. Propose an ICE `force_drain`; run `simulate()`; print the blast radius; abort.
6. Query the audit chain for the last 10 admin commits; verify the `enter_maintenance` is there.

Each README matches the MeshDB / MeshOS SDK README format (slice-based, explicit "what ships in v1 vs deferred"). The C SDK ships a runnable `examples/deck.c` analogous to the existing `examples/meshdb.c`.

---

## Deferred work

### Cross-deck co-signing workflow

§7 #1 specifies *that* `IceProposal::commit` requires `signatures.len() >= ice_threshold` and *what* the substrate verifies. It does **not** specify *how* a second operator's signature reaches the originating deck. Today the substrate accepts the bundle and the SDK exposes the per-operator `OperatorIdentity::sign_proposal`, but there's no defined operator-facing workflow for the two (or more) deck instances to exchange signatures over an unsigned proposal.

Status: **deferred**. The demo runtime ships with `ice_signature_threshold = 1` so the single-operator path commits without coordination; M-of-N landings wait for a Deck consumer driving the workflow.

When this comes back the SDK additions are bounded:

- **Expose offline-signing primitives.** Re-export `BlastRadiusHash`, `blast_radius_hash(&BlastRadius) -> BlastRadiusHash`, and `ice_proposal_signing_payload(&IceActionProposal, issued_at_ms, &BlastRadiusHash) -> Vec<u8>` from `net_sdk::deck::*`. The substrate already implements all three at `behavior::meshos::ice`; they're internal-only today because no consumer needs them.
- **Add a serializable proposal bundle.** A new `IceProposalBundle { action: IceActionProposal, issued_at_ms: u64, blast: BlastRadius }` carries everything a remote operator needs to (a) re-derive the same `blast_hash` locally and (b) sign over the same domain-tagged payload. All three fields are already `Serialize + Deserialize`; the bundle is a thin tuple type with one helper: `bundle.signing_payload() -> Vec<u8>` (delegates to `ice_proposal_signing_payload`).
- **Document the round-trip.** Operator A's deck simulates and produces a bundle; A signs locally and exports `(bundle, sig_a)` as a postcard or JSON blob (paste / file / out-of-band channel); operator B's deck imports the bundle, verifies its own re-derived `blast_hash` matches what A signed over, signs locally, exports `sig_b`; A imports `sig_b` and commits with `&[sig_a, sig_b]`. The substrate verifier rebuilds the signing payload from `(action, issued_at_ms, blast_hash)` so both signatures bind to the exact same envelope or commit fails closed.

The deck-binary surface this unlocks (export bundle, import signature, commit modal that shows collected-so-far vs required) is **out of scope of this plan**. The SDK delta is the prerequisite; the UI follows whenever the workflow has a real consumer demanding it.

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
3. **ICE commits require multi-operator signing.** Default 2-of-N; configurable per-cluster via the operator-policy chain. The substrate-side verifier enforces; the SDK builds the proposal envelope.
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

- **Phase 1 — Rust SDK: snapshot subscription + ordinary admin commits + audit queries + log stream.** The core operator surface; Deck-the-binary's first slice (cluster topology map, replica/placement inspector, daemon supervision panel view-side, audit trail) composes against this. Integration tests against an in-process MeshOS runtime.
- **Phase 2 — Substrate-side: ICE `AdminEvent::Force*` variants + multi-operator signing + blast-radius simulator + ICE subchain.** The substrate work that the SDK's ICE surface binds against. Lands as a `meshos`-feature-gated extension to `behavior::meshos::event::AdminEvent` + a new `behavior::meshos::ice` module.
- **Phase 3 — Rust SDK: ICE surface.** `IceCommands` + `IceProposal::simulate` / `commit` + `BlastRadius`. The break-glass operator surface; Deck-the-binary's ICE panel composes against this.
- **Phase 4 — Python SDK.** Sync-first; full surface (snapshot subscription + admin + audit + logs + ICE). pyo3 wrapper + maturin packaging.
- **Phase 5 — Node / TypeScript SDK.** AsyncIterable streams; full surface. napi-rs + TS shim.
- **Phase 6 — Go SDK.** Channels + `context.Context`; full surface. cgo cdylib + Go wrapper.
- **Phase 7 — C SDK.** Vtable + last-error pattern; full surface. cdylib + header.

Phases 4–7 land independently of each other; Phases 1–3 are a hard prereq chain. Each per-language slice can ship partial surface (e.g., Python ships snapshot + admin in slice 1, ICE + audit in slice 2) as long as the slice list converges on the full operator-command surface before declaring "v1 done."

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
