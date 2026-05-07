# Daemon channel re-bind on migration

## Context

[`SDK_COMPUTE_SURFACE_PLAN.md`](SDK_COMPUTE_SURFACE_PLAN.md) ships daemon spawn + migration, but explicitly defers "channel re-bind on migration" to V2 (§ *Cross-cutting with the other plans*). In practice most useful daemons subscribe to or publish on channels — a migration that drops subscriptions is a migration that loses messages. The footnote-sized hole turns into a user-visible cliff the first time anyone migrates a non-trivial daemon.

Today:

- A daemon running on node `A` calls `mesh.subscribe_channel(publisher=P, channel=X, token)` — `P.roster` now maps `X → [A.node_id]`.
- Migration moves the daemon's state from `A` to `B`. `P.roster` is unchanged because `P` has no signal that the daemon moved.
- Post-cutover `P` keeps publishing to `A.node_id`. `A`'s session may still be alive, but the daemon is gone; events hit `/dev/null`.
- Nothing tears the stale `A` entry down until the publisher's session timeout fires (default 30 s) or the token sweep evicts it on expiry (default 30 s). During that window the daemon on `B` sees zero inbound events on every subscription it held.

Outbound publishes survive migration on their own — the publisher side already keys fan-out on the roster at send time — so this plan scopes to **inbound subscriptions only**. Publishes are covered by the usual subscribe-on-target paths.

## Scope

**In scope**

- Per-daemon subscription bookkeeping on the source side.
- Transfer of the binding list inside `StateSnapshot`.
- Target-side replay before cutover completes.
- Source-side unsubscribe at cutover.
- Error surface: a per-channel failure doesn't kill the migration, but it's visible on the phase stream.

**Out of scope**

- **Cross-node `ChannelConfig` transfer.** Publishing a channel with `publish_caps` / `require_token` still requires the app to register the config on every node the daemon might land on. That's a policy surface the app owns.
- **Zero-drop delivery.** A small duplicate-delivery window at cutover is accepted. Daemons dedupe by causal `seq` (which is already the `MeshDaemon` contract). Zero-duplicate is a harder concurrency problem this plan does not solve.
- **Subscriptions the daemon initiates during migration.** The binding set is frozen at snapshot time. A subscribe that races the migration is user error.
- **Non-daemon callers of `subscribe_channel`.** A plain `Mesh` user that happens to migrate its process between machines is out of scope; daemons have a well-defined identity + lifecycle, arbitrary user code does not.

## Design invariants

1. **Subscriptions are daemon state.** They snapshot with the daemon, transfer with it, and restore on the target. No out-of-band coordination channel.
2. **Target re-subscribes before source unsubscribes.** A duplicate-delivery window is strictly preferable to a drop window — daemons dedupe on causal `seq` by construction, droppers can't recover.
3. **Per-channel failures are visible, not fatal.** If a publisher is offline or the token expired in transit, the migration completes with a `ReplayPartial` event naming the failed channels. The app decides whether to retry or accept the partial state.
4. **The binding format is versioned.** `StateSnapshot` gets a version byte (plan-wide pre-condition from `SDK_COMPUTE_SURFACE_PLAN.md` § *API stability*). Bindings piggyback on that bump — a v0 snapshot decodes as empty bindings, and a v1 reader rejects v2 cleanly.

## Current behavior — concrete trace

```text
t=0    daemon (origin=O) on A subscribes to channel X on P with token T
       P.roster[X] = [A.node_id]
       P.auth_guard.allow_channel(subscriber_origin_hash(A), X)
       P.token_cache.insert(T)            # keyed by (O.entity_id, X.hash)

t=1    migration: A → B
       A.daemon_host.snapshot(O)   →   bytes
       B.daemon_host.restore(O, bytes)  →  daemon alive on B

t=2    P.publish(&X, payload)
       P.roster[X] = [A.node_id]   →   fan-out to A
       A forwards to ... nothing.  daemon is gone.

t=3    session timeout on P ≈ 30 s later → A.node_id evicted
t=3'   or token_sweep on P fires and clears (O, X) → roster GC
```

The window between `t=1` and `t=3` is the drop window. Can be tens of seconds in default config.

## Surface

### Core — per-daemon subscription ledger

New state on `DaemonHost`:

```rust
pub struct DaemonHost {
    // ... existing ...
    subscriptions: DashMap<(NodeId, ChannelName), SubscriptionBinding>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct SubscriptionBinding {
    pub publisher: NodeId,
    pub channel: ChannelName,
    /// Serialized `PermissionToken` — keep the wire format (not
    /// the in-memory struct) so a snapshot round-trips through a
    /// version mismatch without needing to re-verify on the
    /// source side.
    pub token_bytes: Option<Vec<u8>>,
}
```

The key is `(publisher, channel)` so a daemon can subscribe to the same channel name on multiple publishers without collision.

### SDK — `DaemonRuntime` methods

`DaemonRuntime::subscribe_channel` / `unsubscribe_channel` replace any direct `Mesh` calls a daemon would have made. They route through the host so the ledger stays authoritative:

```rust
impl DaemonRuntime {
    pub async fn subscribe_channel(
        &self,
        origin_hash: u32,
        publisher: NodeId,
        channel: ChannelName,
        token: Option<PermissionToken>,
    ) -> Result<(), DaemonError>;

    pub async fn unsubscribe_channel(
        &self,
        origin_hash: u32,
        publisher: NodeId,
        channel: ChannelName,
    ) -> Result<(), DaemonError>;
}
```

A daemon bypassing this and calling `mesh.subscribe_channel` directly gets no re-bind on migration — documented as unsupported; `DaemonRuntime` is the only SDK-sanctioned subscribe path for daemon code.

### Snapshot — versioned

`StateSnapshot` is already on the list of types that need a version byte (see `SDK_COMPUTE_SURFACE_PLAN.md` § *API stability*). Land both changes together:

```rust
pub struct StateSnapshot {
    pub version: u8,               // v1 introduces bindings
    pub state: Bytes,              // daemon-opaque state payload (unchanged)
    pub bindings: DaemonBindings,  // new in v1
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
pub struct DaemonBindings {
    pub subscriptions: Vec<SubscriptionBinding>,
}
```

v0 readers get rejected up-front by a `VersionTooNew` error; v1 readers see a v0 snapshot as `bindings: DaemonBindings::default()` (empty) — migrations of pre-v1 snapshots silently don't re-bind, which matches today's behavior.

## Migration-phase integration

The existing six phases stay; the plan adds work inside Restore and Cutover, plus one new terminal-but-non-fatal event:

```text
Snapshot ─▶ Transfer ─▶ Restore ─▶ Replay ─▶ Cutover ─▶ Complete
                         │           │          │
                         │           │          └─ source: send Unsubscribe for every
                         │           │             binding (fire-and-forget, idempotent)
                         │           │
                         │           └─ target: emit ReplayPartial { failed } if any
                         │              re-subscribe during Restore came back with
                         │              AckReason::{Unauthorized, UnknownChannel, ...}
                         │
                         └─ target: for each binding in snapshot.bindings,
                            send Subscribe to publisher with stored token; wait for Ack
                            (bounded timeout, default 2 s per binding, parallel)
```

Rationale for putting re-subscribe in Restore (not Replay): Replay is where the orchestrator drains buffered events into the daemon's `process`. Those events are ones the source captured *between* snapshot and cutover; for them the roster on `P` still points to `A`, so they arrived at `A` and rode the replay buffer. But any events `P` publishes *after* cutover need `B` in the roster — so `B` must appear in the roster *before* cutover, which means the subscribes must complete during Restore. Replay then runs against a daemon that is both (a) caught up to the snapshot + replay buffer, and (b) present in every publisher's roster.

Source-side unsubscribe runs at Cutover, not Restore, so there's no window where `B` has unsubscribed but `A` hasn't yet — during that overlap both nodes sit in `P.roster`. Publishers fan out to both. The daemon on `B` sees duplicates; it dedupes by `seq`. The daemon on `A` is gone; that delivery hits `/dev/null` but costs nothing.

## Error surface

Add a non-fatal variant to the phase stream:

```rust
pub enum MigrationPhase {
    // ... existing variants ...
    ReplayPartial {
        succeeded: Vec<ChannelName>,
        failed: Vec<(ChannelName, RebindError)>,
    },
}

pub enum RebindError {
    PublisherUnreachable,             // session down / timeout
    TokenExpired,                     // token_bytes decoded, but not_after <= now
    Unauthorized(AckReason),          // publisher said no
    DecodeFailed,                     // token_bytes corrupt — snapshot tampered
}
```

The migration still progresses to Cutover — partial re-bind does not abort. Apps watching the stream can choose to cancel (`MigrationHandle::cancel`) when they see `ReplayPartial`; the orchestrator treats it as an advisory.

## Staged rollout

Each stage is a standalone PR; each lands its own tests.

### Stage 1 — source-side ledger (~2 d)

- `DaemonHost::subscriptions` field + update on every `subscribe_channel` / `unsubscribe_channel` via the runtime.
- Unit: subscribe → ledger has entry; unsubscribe → gone.
- Doc: "daemons must subscribe via `DaemonRuntime`, not `Mesh` directly."

### Stage 2 — versioned `StateSnapshot` (~1 d)

- Add `version: u8` + `bindings: DaemonBindings`.
- Serde bump; old snapshot decodes as empty bindings (v0).
- Round-trip test on v0 bytes produced by current serializer.

### Stage 3 — target-side replay during Restore (~2 d)

- Orchestrator's target half, after `daemon_host.restore(state)`, iterates `bindings.subscriptions` and issues a bounded-parallel subscribe with each token.
- Per-subscribe 2 s timeout. Partial failures collected into `ReplayPartial`.
- Integration test: two-node, token-gated channel, assert target daemon receives events within 500 ms of Complete.

### Stage 4 — source-side teardown at Cutover (~1 d)

- On Cutover, source iterates the ledger, sends `MembershipMsg::Unsubscribe` to each publisher. Fire-and-forget — no ack wait. The publisher's handler is already idempotent.
- Test: publisher's `roster.members(&X)` contains only the target node-id within 100 ms of Cutover, not 30 s.

### Stage 5 — binding SDKs inherit the behavior (~0.5 d per SDK)

- No binding code changes needed for NAPI / PyO3 / CGO if `DaemonRuntime::subscribe_channel` already routes through the core method. SDK-level tests per language to assert re-bind works end-to-end (JS daemon on A → migrate → event from C arrives on B).

### Stage 6 — `RebindError` surface polish (~1 d)

- Phase stream emits `ReplayPartial`. Error types serialize cleanly across every binding.
- Cancel-on-partial tests: apply `MigrationHandle::cancel` in response to a partial; assert source re-takes ownership cleanly.

## Test plan

Concrete scenarios, each a tokio integration test in `tests/daemon_channel_rebind.rs`:

- `rebind_open_channel_delivers_post_cutover` — A hosts daemon, subscribes to X on P. Migrate A → B. P publishes one event after Complete. Daemon on B gets it within 500 ms.
- `rebind_token_channel_reuses_stored_token` — X is `require_token`. Token in snapshot is still valid at Restore. Assert delivery works.
- `rebind_token_expired_in_transit_partial_emit` — stored token's `not_after` passes between snapshot and restore. Migration completes with `ReplayPartial { failed: [X] }`.
- `publisher_offline_during_restore_partial_emit` — P is shut down before restore. Other subscriptions succeed; X surfaces as `PublisherUnreachable`.
- `source_unsubscribe_clears_roster` — publisher P's `roster.members(&X)` transitions from `[A]` → `[A, B]` (window) → `[B]` within one heartbeat of Cutover.
- `duplicate_during_cutover_is_deduped_by_seq` — P publishes 100 events concurrent with the migration. Daemon on B sees 100 distinct events, 0 duplicates in the observable output (the causal-seq dedup inside the host handles the brief `[A, B]` window).
- `v0_snapshot_restores_with_empty_bindings` — feed a pre-v1 snapshot into a v1 node; no re-bind attempted, no error.

## Critical files

```
net/crates/net/src/adapter/net/compute/host.rs        +subscriptions ledger, +on-subscribe hook
net/crates/net/src/adapter/net/compute/snapshot.rs    +version byte, +DaemonBindings
net/crates/net/src/adapter/net/compute/migration.rs   +restore-time re-subscribe,
                                                       +cutover-time unsubscribe,
                                                       +ReplayPartial emission
net/crates/net/sdk/src/compute.rs                     +subscribe_channel / unsubscribe_channel
                                                       on DaemonRuntime
net/crates/net/tests/daemon_channel_rebind.rs         new integration-test file
```

No new wire protocol, no new subprotocol IDs — re-subscribe rides the existing membership subprotocol (`0x0A00`). Unsubscribe rides the same. The only wire change is the `StateSnapshot` version byte.

## Risks

- **Publisher-side throttle.** A daemon with N subscriptions replays N subscribes in parallel against the same publisher. The publisher's `max_auth_failures_per_window` throttle (default 16 / 60 s) could trip if tokens are invalid; the `max_channels_per_peer` cap (default 1024) could trip if N is huge. Both are legitimate rejections — they surface as `RebindError::Unauthorized(RateLimited)` / `TooManyChannels`. Document the limit; real daemons carry ≤ 10 subscriptions in practice.
- **Token staleness.** Snapshots that sit on disk for longer than a token's TTL will fail re-bind on restore. Partial-rebind surface handles this cleanly; app must reissue and retry.
- **Concurrent subscribe during snapshot.** The ledger must be read-consistent at snapshot time. Use a `DashMap::iter().map(|e| e.value().clone()).collect()` under a host-level "quiesce" notch during snapshot — daemons are single-threaded from the host's perspective, so no lock is needed, but the timing guarantee should be explicit.
- **Snapshot size.** Each `SubscriptionBinding` with a token is ~200 B. Hundreds of subscriptions push snapshot size by ~50 KB; negligible compared to daemon state in practice. Flag if any daemon ever ships with >10 k subscriptions.
- **Cross-SDK `kind` mismatch.** If a daemon snapshotted from a Rust source doesn't find its `kind` factory on the target SDK, the migration already fails in `spawn_from_snapshot`. Re-bind piggybacks on that — the bindings live inside the snapshot so they're transferred regardless, and are only applied after a successful restore.

## Sizing

| Stage | Effort |
|---|---|
| 1. Source-side ledger | 2 d |
| 2. Versioned `StateSnapshot` | 1 d |
| 3. Target-side replay (Restore) | 2 d |
| 4. Source-side teardown (Cutover) | 1 d |
| 5. SDK-level inheritance tests (×4 SDKs) | 2 d |
| 6. Error surface polish | 1 d |

Total: ~9 d. One PR per stage; merges cleanly against the compute-surface plan's Stage 2.

## Dependencies

- Depends on `SDK_COMPUTE_SURFACE_PLAN.md` Stages 1–2 (daemon runtime + migration baseline).
- Depends on `SDK_SECURITY_SURFACE_PLAN.md` Stage A (identity — already a transitive dep).
- No dependency on NAPI / PyO3 / CGO stages; those inherit re-bind for free once core lands.

## Explicit follow-ups (not in this plan)

- **Channel-config transfer.** A daemon that *publishes* on a `require_token` channel needs the `ChannelConfig` registered on the target. Plan punts — apps handle it today by registering configs on every node in the migration target set. A future stage could piggyback configs on the snapshot.
- **Automatic re-subscribe for non-daemon callers.** If a `Mesh` user migrates their process (not via the daemon framework), they're on their own. Lifting this to a generic capability means introducing a "subscription owner" identity that outlives the session — a larger redesign.
- **Subscribe-during-migration.** Today the binding set is frozen at snapshot time. A daemon that calls `subscribe_channel` between snapshot and cutover gets its new subscription stuck on the source. Either forbid (document) or add a post-snapshot delta — defer the decision until we have a user who needs it.
- **Token refresh during migration.** If a token is about to expire and a refresh is in flight during snapshot, the in-flight token is lost. Real fix is a token-issuance handshake at restore time — more auth work than this plan wants to take on.

## Open questions for review

- **Ledger on the host or on the runtime?** The doc puts it on `DaemonHost` (per-daemon). Alternative: single `DaemonRuntime`-wide ledger keyed by `origin_hash`. Host-side is cleaner for snapshot coupling; runtime-side would be a single point of observability. Going with host-side unless we find a concrete need for the aggregate view.
- **Timeout per subscribe during Restore.** Default 2 s seems reasonable; too aggressive and a slow publisher flaps into `PublisherUnreachable`. Revisit after the Stage 3 integration tests.
- **Should the daemon see re-bind events in its own event stream?** Currently no — `process` only receives `CausalEvent`s from channels, not orchestration metadata. Keeping it that way: re-bind is runtime plumbing, invisible to the daemon itself.
