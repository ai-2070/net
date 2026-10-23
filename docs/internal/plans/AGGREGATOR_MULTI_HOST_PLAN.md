# Aggregators across hosts: correct summaries, a live query path, and replicas that leave the box

## Status

**Planned. Revised after review 1** (both 2026-10-23), on branch
`LZL0/aggregators` (forked from `master` at `541cc0078`). No release has been
chosen.

- **Phases 0 and 1 are cleared to start.**
- **Phases 2–3 were revised** around the daemon's actual boot path,
  configuration and publication scope.
- **Phase 4 is gated** on the identity, member-lifecycle and receiver contracts
  (D1, D3, D7, D8) being implemented as written.

Predecessors, both closed: `AGGREGATOR_LIFECYCLE_DEFERRED_2026_05_23.md`
(AL-1..AL-8) and `AGGREGATOR_CLI_REMOTE_ATTACH_AND_SCALE_RPC.md` (A, B, C).
Phase 0 corrects claims in them, and in `SCALING_SUBNET_SPEC.md`, that turned
out to be wrong.

[Review 1](#review-1-2026-10-23) lists every finding and where this revision
answers it.

---

## The gap

The single-host aggregator ships and works. The `net-aggregator-daemon` binary
runs groups from a TOML config. It has template-based Spawn/Scale/Unregister/List
over `aggregator.registry`, auto-respawn, and an `operators` allowlist. The CLI
verbs and the Rust/TS/Python/Go/C clients exist. Each item below was checked
against the tree at `541cc0078`.

### G1. Replicas never leave the host (the original item 1)

- **Placement is recorded, not acted on.** `LifecycleGroup::spawn_with_placement`
  records a host per replica and then starts every replica locally
  (`src/adapter/net/behavior/lifecycle/group.rs:191-200`).
- **No remote-spawn path exists.** Migration (`compute/orchestrator.rs`,
  `migration_source.rs`, `migration_target.rs`) moves `MeshDaemon`s, but
  aggregators implement the sibling `LifecycleDaemon` trait (AL-1, direction B).
- **Scale-up ignores the scheduler.** `add_replica` never consults it
  (`lifecycle/group.rs:380-387`).
- **The daemon doesn't use placement at all.** It calls plain
  `LifecycleGroup::spawn` (`aggregator-daemon/src/lib.rs:680`).

### G2. In-process replicas duplicate each other

Every replica the daemon builds gets the same `Arc<MeshNode>`
(`aggregator-daemon/src/lib.rs:680-692`, plus the monitor and scale factories).
`AggregatorDaemon::produce_summaries` reads `self.mesh.capability_fold()`
(`behavior/aggregator/daemon.rs:385`). N replicas on one host therefore read the
same fold and publish the same summary N times from the same node. They protect
only against one replica's task dying. The derived replica keypairs are unused
(`registry_service.rs:137-143`: they "authorize nothing in the aggregator path").

### G3. Summaries ignore their subnet, and the daemon has no subnet policy

- **The subnet is only a label.** `CapabilityFoldSummarizer::summarize`
  (`behavior/aggregator/summarizer.rs:109-150`) counts every entry in the node's
  fold. A daemon running groups for subnets A and B publishes identical counts
  under two labels.
- **The classifier exists, but the daemon never configures it.**
  `SubnetPolicy::assign_from_rendered_tags` (`subnet/assignment.rs:232`) is the
  classifier. The daemon config has no policy field: its fields are `listen`,
  `psk_hex`, `groups`, `templates`, `operators`, `operators_any_admitted_peer`,
  `max_groups` and `max_replica_count` (`aggregator-daemon/src/lib.rs:111-176`).
  Boot builds plain `MeshNodeConfig::new(listen_addr, psk)` (`:464-466`), whose
  policy is `None`. An empty policy would assign everything to `GLOBAL`, so a
  non-global group would read zero rather than its real count.
- **`peer_subnets` can't fill the gap.** It is written only for direct (hop 0)
  announcements (`mesh.rs:42126-42142`, witnessed by
  `tests/capability_broadcast.rs::forwarded_announcement_does_not_write_relay_peer_subnet`).

### G4. Announcement relay through an aggregator (the original item 2)

Commit `caee075ac` (2026-09-25) added hub replay (`mesh.rs:42587-42670`), and
`tests/capability_multihop.rs` covers bare-`MeshNode` hubs. Two cases remain
unwitnessed:

1. The hub is the real `net-aggregator-daemon` binary.
2. Both clients attach by routed handshake **before** the provider announces,
   so discovery comes from the live flood (`forward_capability_announcement`,
   `mesh.rs:42470`), not from replay.

### G5. The daemon serves no `fold.query`, and summary visibility is not enforced

- **No `fold.query` handler.** The daemon installs only the registry service
  (`aggregator-daemon/src/lib.rs:495`). Nothing in `aggregator-daemon/` calls
  `install_query_service` (`query_service.rs:234`); only the SDK helper does
  (`sdk/src/aggregator.rs:148-149`). The ignore note on
  `cli/tests/aggregator_remote.rs:335` names the wrong cause.
- **No group selector.** `FoldQueryRequest { kind, op }` (`query_service.rs:45-52`)
  cannot name one of several groups.
- **Registering summary channels does nothing today.** `register_summary_channels`
  returns `Ok(0)` when the node has no `ChannelConfigRegistry`
  (`aggregator/daemon.rs:483-487`), and the daemon installs none. Only tests
  call it.
- **Visibility is per node, not per group.** It is evaluated against the node's
  own subnet: `set_channel_configs` builds the `SubnetGateway` from
  `self.local_subnet` (`mesh.rs:35524-35527`), and must run before `start`.
  Summary channels are named `summary/<kind>` with no subnet. One node hosting
  groups for A and B therefore cannot give them different publication
  boundaries, and registering the same name twice replaces the config.

### G6. Operator auth, and the daemon's identity

The registry is gated by the `operators` allowlist
(`aggregator-daemon/src/lib.rs:128-157`), and unknown callers are refused before
decoding (`registry_service.rs:359-377`). But:

- **The daemon's identity changes at every boot.** It is
  `EntityKeypair::generate()` (`aggregator-daemon/src/lib.rs:464-466`). Any
  allowlist that names a daemon goes stale when that daemon restarts.
- **The identity-file format belongs to the CLI binary.**
  `IdentityFile`/`read_identity_file` are `pub(crate)` in `net-cli`
  (`cli/src/commands/identity.rs:457, 541`), which has no lib target.
- **Daemons can't find each other.** The config has no peers to dial. Only
  inbound attaches work, so daemons that start isolated stay isolated.

### G7. Capacity (the original item 5)

`SCALING_SUBNET_SPEC.md:522` caps an aggregator at "~10K detailed entries". The
v0.42 fold measurements (`net/crates/net/docs/releases/RELEASE_v0.42_BACK_IN_BLACK.md`,
harness `benches/fold_scale_report.rs`, i9-14900K, Windows 11):

| measure | v0.40 | v0.42 |
|---|---|---|
| retained per entry, 1M | 4,184 B (4.2 GB) | 1,875 B (~1.9 GB) |
| expiry sweep, 1M, nothing due | 8.7 ms, 1M visited | 0.00 ms, 0 visited |
| expiry sweep, 1M, whole fleet expired | 14.1 s | 10.5 s (held in 1,024-entry chunks, `fold/expiry.rs:50`) |
| insert / refresh / changed replace¹ | 5.99 / 1.40 / 16.4 µs | 2.86 / 1.21 / 10.4 µs |
| translate an announcement¹ | 9.79 µs | 3.7 µs |
| mixed workload: broad query p50 (~500k matches) | 40.5 ms | 17.7 ms |

¹ Last measured with v0.41's work. The release notes' caveats apply: µs deltas
of ±20–30% are noise, and the final review round (8,192-tag cap, linear budget
check, expiry-wheel and restore limits) was not re-measured.

Still open:

- **The summary tick scans everything under the fold's read lock.** It is
  O(entries) and blocks writers for the scan (`with_state` takes `state.read()`,
  `fold/mod.rs:824-827`). At 1M entries that is probably tens of ms per tick
  (inferred, not measured).
- **Nothing measures a whole aggregator** (ingest + verify + translate + apply +
  tick).

### G8. Spawn and Unregister aren't safe for coordinating remote members

- **Retries are indistinguishable from conflicts.** A repeated Spawn of the same
  name returns `DuplicateGroupName` (`registry_service.rs:606`), so a lost reply
  is an uncertain outcome the caller cannot resolve.
- **Unregister is local and keyed by name.** It removes and stops one local
  `LifecycleGroup`. A delayed cleanup could stop a replacement that reused the
  name.

### G9. Appended wire fields do not fail closed

`postcard::from_bytes` does not require all input to be consumed. Review 1 ran
a probe against the repository's postcard 1.1.3 with an optional `group`
appended to `FoldQueryRequest`:

- **New request, old decoder:** accepted, with the 15 extra bytes silently
  ignored. An old server would answer as if no group had been named.
- **Old request, new decoder:** `DeserializeUnexpectedEnd`.

Renaming a field changes no bytes; changing its type or order does.

### G10. Stale planning docs

- **`SDK_AGGREGATOR_SUBNET_PLAN.md` is out of date.** Its "Current state" table
  shows ✗ for clients that have shipped.
- **`SCALING_SUBNET_SPEC.md` promises things the code never built.** It still
  promises:
  - aggregators as `MeshDaemon`s deployed by `ReplicaGroup` (lines 5, 118, 130,
    141-287, 439, 484);
  - identity continuity across re-placement (151);
  - term/epoch fencing (79, 161);
  - replica discovery via `role:aggregator` and `route_event` (352);
  - `ReplicaGroup::on_node_failure` re-spawn (498).

  AL-1 chose the `LifecycleDaemon` sibling instead, so none of these exist.

---

## The design

Each decision has a recommendation. Phase 4 starts only when D1, D3, D7 and D8
are signed off.

**D0. Publication scope: one source subnet per daemon.** *(New in review 1;
finding 3.)*

The daemon config gains `subnet = "<SubnetId>"`, installed with
`MeshNodeConfig::with_subnet`, so the node's `local_subnet` is the source
subnet. Every `[[group]]` and `[[template]]` must name the same `source_subnet`.
Boot and Spawn refuse a mismatch; omitting it defaults to the daemon's subnet.

Before `start`, boot installs a `ChannelConfigRegistry` with one
`summary/<kind>` entry for every fold kind any group **or template** uses, so
dynamically spawned groups are covered. Each entry carries the daemon's
`summary_visibility`. Groups on one daemon must agree on visibility; boot
refuses a conflict.

Then publication scope is something the existing gateway can enforce.

`fold.query` returns the same summaries the channel carries, so it enforces the
same boundary. It admits a caller only if the summary channel's visibility would
deliver to that caller's subnet. A caller whose subnet is unknown (no direct
announcement, G3) is refused unless the visibility is `Global`. Callers on the
`operators` list are always admitted.

Rejected:

- **One `MeshNode` per group inside a daemon.** Several listeners and
  identities per process, for a deployment shape nobody has asked for.
- **Putting the subnet in the channel name.** The gateway still evaluates the
  node's `local_subnet`, so the name would change nothing.

**D1. How a replica starts on another host.** *Recommend: daemon-to-daemon
Spawn under the D7 protocol.*

The coordinating daemon places a member and calls Spawn on the chosen host's
daemon. The remote host runs only templates its own operator configured, and
only for callers on its own `operators` list. No new substrate primitive is
needed.

Rejected:

- (a) **Make `AggregatorDaemon` a migratable `MeshDaemon`.** This reverses AL-1;
  the state is a cache rebuilt from announcements, so migration buys nothing.
- (b) **A general remote-spawn primitive.** It is the right long-term home but
  far larger. It can absorb D1 later.

**D2. What a replica is.** *Recommend: one member per host. `replica_count`
counts hosts.*

G2 shows that in-process replicas only duplicate. The group seed is never
shipped to a remote host. Each host runs its member under its own persistent
identity (D6).

Rejected: keeping in-process replicas as cheap HA. `HealthMonitor` respawn
already covers a dead task with one replica.

**D3. Ordering a publisher's summaries across restarts.** *(Revised after review
1, finding 5. It was "epoch = start time in ms", which is not monotonic under
clock correction or two starts in the same millisecond.)* *Recommend: a
persisted boot counter plus an in-memory start sequence.*

- **What gets stamped.** Each daemon keeps `boot_epoch: u64` in its state
  directory (D6). At boot, before anything is published, it is incremented and
  written with an fsync. Within one boot, every member start (including a
  `HealthMonitor` respawn, whose fresh `AggregatorDaemon` restarts `generation`
  at 0) takes the next `start_seq: u32`. Each summary carries
  `incarnation = (boot_epoch, start_seq)` and `generation`.
- **How a receiver orders them.** Per publisher, order is lexicographic on
  `(boot_epoch, start_seq, generation)`. A summary that sorts below the highest
  incarnation already seen from that publisher is dropped.
- **Why this works.** The publisher's identity persists (D6), so the order holds
  across restarts.

Rejected:

- **Wall-clock time.** Not monotonic.
- **A random incarnation id.** Not ordered.
- **Coordinator-issued incarnations.** The order would depend on the coordinator
  being reachable when a member restarts.

**D4. How summaries pick their subnet.** *(Revised after review 1, finding 2:
the policy now has a production source.)*

- **Where the policy comes from.** The daemon config gains `[[subnet_rule]]`
  blocks mirroring `SubnetRule` (prefix, level, value → id map), installed with
  `MeshNodeConfig::with_subnet_policy`.
- **How entries are classified.** The fold classifies each entry with
  `SubnetPolicy::assign_from_rendered_tags` over the origin's signed tags. That
  is safe for forwarded origins: the hazard
  `forwarded_announcement_does_not_write_relay_peer_subnet` guards against is
  binding a *relay's session*, which this does not do.
- **Missing policy.** If the daemon's `subnet` is not `GLOBAL` and no rules are
  configured, boot refuses ("a non-global source subnet needs subnet rules").
  A `GLOBAL` daemon with no rules counts the whole mesh, explicitly.
- **Policy fingerprint.** A BLAKE2s hash of the canonically ordered rules is
  reported in `List`. Spawn compares it (D7) so a group's hosts cannot classify
  differently.

**What the counts mean.** Buckets count **capability entries** (one publisher
may hold several classes), which keeps today's semantics. A new `publishers`
bucket counts distinct nodes.

**How counts are kept.** Counters keyed `(SubnetId, NodeState)` are updated
inside the fold's write critical section. That covers insert, replace (including
forced replacement), expire, `evict_node`, and snapshot restore. They are read
as one snapshot, so the buckets in a summary always agree with each other.

**Reservation summaries.** Reservations are not tagged nodes and cannot be
classified. A `ReservationFold` kind is therefore allowed only on a `GLOBAL`
daemon; boot and Spawn refuse it elsewhere. A subnet-labelled summary never
carries mesh-wide numbers.

Rejected: filtering at tick time via `peer_subnets`. It misses forwarded origins
and keeps the scan.

**D5. Auth between daemons.** *Recommend: `operators` allowlists naming peer
daemons' persistent node ids (D6) first; org capability grants later (slice
M8).*

**D6. Daemon identity, state and peers.** *(New in review 1, finding 1.)*

- **Identity.** `identity = "<path>"` in the daemon config, in the same file
  format `net-mesh identity` writes. The parsing moves from
  `cli/src/commands/identity.rs` into `net-mesh-sdk`, so the CLI and the daemon
  share one reader with the same permission checks.
  - Without `identity`, the daemon still boots with a fresh identity, and logs
    that it is ephemeral.
  - Every Phase 4 feature (peers, coordination, being a member) refuses to run
    on an ephemeral identity.
- **State.** `state_dir = "<path>"` holds `boot_epoch` (D3) and the
  coordinator's records (D7). Phase 4 features require it.
- **Peers.** `[[peer]]` blocks give `addr`, `public_key` and `node_id`. The
  daemon dials each with the SDK's `connect_via` (the routed handshake, which
  completes against a running responder without pre-`accept`). It redials with
  backoff when a session drops.

**D7. Member lifecycle protocol.** *(New in review 1, finding 4.)* This is a
small protocol between two daemons, not a distributed transaction.

- **Member identity.** A logical group `g` has members `(g, index)`. The
  coordinator assigns each placement an `incarnation: u64` from a counter
  persisted in its `state_dir`. On the host, the member runs as the local group
  `g#<index>`, stamped with that incarnation.
- **Idempotent spawn.** `SpawnMember { template, group, index, incarnation,
  policy_fp, template_fp }`:
  - same name and same incarnation → reply `Spawned` again (a replay);
  - same name, different incarnation → `IncarnationConflict { running }`;
  - a fingerprint mismatch → refused.

  After a lost reply, the coordinator retries the identical request until it
  gets an answer, or reconciles from `List`.
- **Conditional teardown.** `UnregisterMember { group, index, incarnation }`
  removes the member only if the running incarnation matches; otherwise it
  replies `NotRunning`. A delayed cleanup therefore cannot stop a replacement
  that reused the name.
- **Partial failure.** A logical Spawn either succeeds for all N members or is
  rolled back. Members that started are torn down with conditional Unregister,
  retried in the background, and the operator gets a typed error naming the
  member that failed.
- **Logical Unregister.** The coordinator writes a tombstone listing every
  member incarnation, then conditionally unregisters each one. It retries until
  each is acknowledged or the host has been absent past the retire budget.
  Repeated Unregisters return the same result while the tombstone exists.
- **Coordinator restart.** Logical groups, incarnations and tombstones live in
  `state_dir`. On boot the coordinator reconciles each host's `List` against
  them.
- **Partition semantics** (finding 7). There is **no fencing**. When a host
  misses its `List` budget, the coordinator places a replacement with a new
  incarnation. During a partition, the group may briefly run more than N
  members. After the partition heals, conditional cleanup removes the old
  incarnation. The extra publishers are harmless to receivers (D8). Strict
  exclusion is not promised.

**D8. Receiver freshness.** *(New in review 1, finding 6.)* This is kept
separate from D3's ordering.

- **Bounded refresh.** `filter_novel` still suppresses unchanged summaries, but
  every member republishes its current summary every `refresh_interval` even
  when nothing changed. The default is 4 × `summary_interval`. Summaries carry
  `refresh_ms`, so receivers do not guess it.
- **Expiry.** A receiver's entry for `(publisher, subnet, kind)` is fresh while
  `now − received_at < 3 × refresh_ms`.
- **Selection.** `SummaryView` reports the fresh entry received most recently.
  When it expires, the view falls back to the next fresh one. With no fresh
  publisher it returns `NoFreshPublisher { last_known, age }`, never an old
  value presented as current.
- **Late subscribers.** `SummaryView::bootstrap` calls `fold.query`
  `LatestSummary` on any known member. Otherwise the view fills within one
  `refresh_interval`.

**D9. Wire evolution.** *(New in review 1, finding 8.)* *Recommend: versioned
service and channel names, with strict decoding on the new versions.*

- **What gets versioned.** Changed shapes ship under new names:
  - `fold.query.v2` (the group selector, Q1);
  - `aggregator.registry.v2` (the D7 ops and the M3 reply shape);
  - channel `summary.v2/<kind>` (D3 incarnation, D8 `refresh_ms`).
- **Strict decoding.** v2 decoders use `postcard::take_from_bytes` and reject
  any trailing bytes.
- **Mixed versions fail closed.** A new client calling an old server gets "no
  handler", surfaced as a typed `ServerTooOld`, instead of silently dropped
  fields.
- **v1 stays.** v1 names keep their current layouts and are served for at least
  one release.

Exact v2 field layouts are frozen in the slices that introduce them (Q1, M3, M6)
and pinned by golden-byte tests.

Rejected: appending fields to v1 types (G9), and relying on release notes for
lockstep upgrades.

### Shape after Phase 4

```
          operator CLI ──registry.v2──▶ daemon A (coordinator for "g"; subnet S)
                                          │ placement over hosts announcing
                                          │ aggregator-host + template tags
                         SpawnMember(g, i, incarnation)  — idempotent, conditional teardown
                    ┌─────────────────────┼─────────────────────┐
                    ▼                     ▼                     ▼
                daemon A              daemon B              daemon C      (all subnet S,
               member g#0            member g#1            member g#2      persistent ids,
                    │ summary.v2/<kind>: incarnation, generation, refresh_ms   [[peer]] dialed)
                    └──────────▶ SummaryView: order per publisher (D3), freshness (D8)
```

---

## The slices

Each slice leaves the tree green. "Proof" names what would fail if the slice
were wrong. Sizes are rough working days.

### Phase 0 — Correct the record (1 day)

- **P0-1. Amend the predecessor plans.**
  - `AGGREGATOR_LIFECYCLE_DEFERRED_2026_05_23.md`, "Remaining gap": `query` is
    not live against the binary (G5); point here.
  - `SDK_AGGREGATOR_SUBNET_PLAN.md`: today's "Current state" table (G10).
- **P0-2. Correct `SCALING_SUBNET_SPEC.md`'s obsolete contracts** (G10, and
  review 1's smaller correction).
  - Mark the `MeshDaemon`/`ReplicaGroup` deployment, identity continuity,
    term/epoch fencing, `route_event` discovery and `on_node_failure` sections
    as superseded by AL-1 and this plan, each with a one-line pointer to D1, D2
    or D7.
  - Mark the 10K capacity figure as superseded, pending S3.

*Proof:* docs only. Run `bun run check` in `web/` if a mirrored page changes.

### Phase 1 — Relay through a real daemon (G4, 1–2 days)

- **R1. Daemon-hub relay witnesses** in `aggregator-daemon/tests/` (run by
  `cargo test -p net-aggregator-daemon`). Each test boots the daemon with
  `boot()` and attaches two SDK nodes by `connect_via`, covering:
  1. the provider announces before the consumer attaches (replay);
  2. both attached, then the provider announces (flood);
  3. the provider re-announces with a changed tag (supersede).

  *Proof:* `find_nodes_by_filter` contains the provider within the deadline.
- **R2. Fix, only if R1 case 2 fails.** Suspects:
  - the split-horizon lookup (`mesh.rs:42516`);
  - the recipient snapshot excluding a routed session;
  - the webrtc-gated forward admission in `forward_capability_announcement`.

  *Proof:* R1 green, plus a `tests/capability_multihop.rs` test that pins the
  cause at the `MeshNode` level.

### Phase 2 — Publication scope and a live query path (G5; 4–5 days)

- **Q0. Daemon publication scope (D0).**
  - Add `subnet`, and source-subnet and visibility agreement checks for groups
    and templates.
  - Install the `ChannelConfigRegistry` before `start`, covering every static
    and template fold kind.

  *Proof*, in `aggregator-daemon/tests/`, with real delivery rather than a
  registered field: a daemon in subnet S with `SubnetLocal` summaries.
  - A subscriber in S receives `summary/<kind>`.
  - A subscriber in a sibling subnet is denied, as counted by the gateway's
    dropped counter and by no delivery.
  - A Spawn-created group's channel gets the same verdicts.
  - Boot refuses a template whose subnet disagrees.
- **Q1. `fold.query.v2`** (D9).
  - Request layout `{ kind: u16, op: FoldQueryOp, group: Option<String> }`.
  - Response adds `UnknownGroup` and `NotAuthorized`.
  - Strict decoding.
  - The D0 access check.

  *Proof:*
  - Round trips in `tests/aggregator_fold_query.rs` for all groups, a named
    group, and an unknown group.
  - A golden-byte test of the request layout.
  - Trailing bytes rejected.
  - A sibling-subnet caller refused, and an `operators` caller admitted.
  - A v2 client against a v1-only server gets `ServerTooOld`.
- **Q2. The daemon installs v1 and v2.** v1 answers with "all groups" semantics.
  The `ServeHandle`s are kept on `BootedDaemon`.

  *Proof:* un-ignore
  `cli/tests/aggregator_remote.rs::query_returns_summary_for_configured_group`,
  and replace its ignore note's premise: the target is the daemon's node id.
- **Q3. Bindings expose the group argument and the new error kinds.**
  - Node: `bindings/node`, `sdk-ts/src/aggregator.ts`.
  - Python: `bindings/python`.
  - Go: `go/`, `bindings/go/*-ffi`.
  - C: `include/` and `tests/c_sdk/aggregator.c`.

  C ABI changes follow the single-commit rule.

  *Proof:* each binding's aggregator test gains named-group and unknown-group
  cases, and `aggregator.c` keeps its NET-CHECKS line.

### Phase 3 — Summaries that are right, and cheap at 1M (G3, G7; about 1½ weeks)

- **S0. Subnet policy in the daemon (D4).** `[[subnet_rule]]` parsing,
  installation, the missing-policy refusal, and the policy fingerprint in `List`.

  *Proof:*
  - Boot cases: rules installed; a non-global subnet with no rules refused; the
    fingerprint is stable across rule order.
- **S1. Classified incremental counters (D4).** Counters cover insert, replace,
  forced replacement, expire, `evict_node` and restore, and are read as one
  snapshot. Add the `publishers` bucket and the reservation-on-non-GLOBAL
  refusal.

  *Proof:*
  - Property test: after any sequence of apply, replace, forced replace, expire,
    evict, snapshot and restore, the counters equal a full classified scan.
  - Coherence test: a concurrent writer never produces a summary whose buckets
    come from different moments.
  - **End to end:** two real daemons booted from TOML, in subnets A and B with
    shared rules, on a mesh where some A and B nodes are reachable only through
    forwarding. Each daemon's `fold.query` returns nonzero counts that differ
    between A and B, include the forwarded origins, and match the populated
    truth.
  - A reservation kind on a non-GLOBAL daemon is refused at boot and at Spawn.
- **S2. Whole-aggregator bench**, `benches/aggregator_ingest.rs`. It drives N
  distinct signed origins through the real dispatch path at 100K and 1M. It
  reports per-announcement verify / translate / apply cost, sustained ingest on
  one core, tick time and retained memory. Values are labelled measured /
  counted / estimated / inferred, as in `CAPABILITY_FOLD_SCALE_PLAN.md`.
  Validated by a smaller loopback-UDP run.

  *Proof:* the numbers, command and commit recorded in Status.
- **S3. Replace the spec's capacity limit** with S2's figure for a stated
  announce interval. Sharding gets its own plan if S2 says one host can't keep
  up.

### Phase 4 — Members on distinct hosts (G1, G2, G6, G8; about 4 weeks)

Gated on D1, D3, D7 and D8.

- **I1. Persistent identity (D6).** Move identity-file parsing into
  `net-mesh-sdk`; the CLI switches to it with no behavior change. Add the daemon
  `identity` and `state_dir` settings and the ephemeral-identity refusal for
  Phase 4 features.

  *Proof:*
  - The existing CLI identity tests pass unchanged.
  - Restarting a real daemon with the same config keeps its node id.
- **I2. Peer bootstrap (D6).** `[[peer]]` dialing and redial with backoff.

  *Proof* (review 1's witness): a real daemon restarted with the same config
  keeps its node id, is still admitted by a peer whose `operators` list names
  it, and reconnects with no test-only peer wiring.
- **I3. Boot epoch (D3).** Persisted, fsynced, incremented before first publish;
  `start_seq` per member start.

  *Proof:*
  - Restart increments it.
  - A corrupt or missing file is refused rather than reset, unless the operator
    passes an explicit reset flag.
- **M0. One member per host (D2).**
  - `replica_count > 1` on a daemon with no peers is refused at Spawn.
  - A static `[[group]]` with `replica_count > 1` warns and clamps to 1 for one
    release, then is refused.

  *Proof:* `boot_and_query.rs` refuse and clamp cases.
- **M1. Hosts announce themselves.** `aggregator-host`, one
  `aggregator-template:<name>` per template, the subnet, the policy fingerprint,
  and free-member capacity.

  *Proof:* two daemons dialed via `[[peer]]` find each other by filter.
- **M2. Placement over hosts.** The candidate set comes from M1 and is limited
  to the group's subnet. Placement uses `GroupCoordinator::place_with_spread`
  and a typed `PlacementFailed`.

  *Proof:* three members land on three distinct hosts; a fourth fails with
  `PlacementFailed` when only three hosts qualify.
- **M3. Remote members under D7** (`aggregator.registry.v2`).
  - `SpawnMember` and `UnregisterMember` with frozen layouts.
  - Template and policy fingerprints.
  - Distributed group records persisted in `state_dir`.
  - `List` v2 reports `host_node_id: u64` (not `Option`) and the incarnation
    per member.

  *Proof*, multi-daemon tests on loopback:
  - A dropped Spawn reply, injected by blocking the reply path with
    `MeshNode::block_peer` (`mesh.rs:24467`) for one exchange, is retried and
    yields exactly one member.
  - A failure after one remote member starts rolls the group back, and the
    started member is gone.
  - Repeated logical Unregister is idempotent.
  - A delayed `UnregisterMember` for an old incarnation, arriving after the
    name was reused, leaves the replacement running.
  - A coordinator restart reconciles from `state_dir` with no duplicate members.
  - Golden-byte tests for each v2 layout.
- **M4. Health and re-placement (D7 partition semantics).**

  *Proof:* partition a **still-running** member with `block_peer` on both sides.
  The coordinator places a replacement with a new incarnation. While
  partitioned, both publish. After healing, conditional cleanup removes the old
  incarnation and not the replacement. Stopping a daemon outright is a
  separate, narrower test.
- **M5. Scale with placement.** `add_replica_with_placement`, or the
  distributed-group equivalent. Scale-down retires the member on the most loaded
  host (highest index on a tie) via conditional Unregister.

  *Proof:* 2→4→2 across four hosts, with distinct hosts throughout. Surviving
  members keep their incarnation.
- **M6. `summary.v2` and `SummaryView` (D3, D8)**, built in `net-mesh-sdk` first
  and then exposed in the bindings.

  *Proof*, ordering witnesses:
  - A backward wall-clock step between restarts.
  - Two restarts within the same millisecond.
  - A `HealthMonitor` respawn within one boot.
  - A delayed old-incarnation summary arriving after the new one.
  - Progress by the current incarnation afterwards.

  In each case, the old incarnation is dropped and the new one wins.

  *Proof*, freshness witnesses:
  - Healthy publisher B stays unchanged and is still fresh (refresh keeps it
    alive).
  - Last-selected A dies; the view falls back to B within 3 × `refresh_ms`.
  - A late subscriber bootstraps from `fold.query` before any refresh.
  - When every publisher stops, the view returns `NoFreshPublisher`.
- **M7. Operator surfaces.**
  - `net aggregator ls` shows host and incarnation per member.
  - `spawn` gains `--min-hosts`.
  - The Deck Aggregators tab (`deck/src/tabs/aggregators.rs`) shows hosts.
  - Docs pages under `web/src/content/docs/` are updated.

  *Proof:* `cli/tests/aggregator_remote.rs` multi-daemon cases, and the
  docs-sync checks in `web/`.
- **M8. (Optional) Org capability grants for daemon-to-daemon auth (D5).**

  *Proof:* a revoked grant is refused without a restart.

---

## Risks

| Risk | Fallback |
|---|---|
| R1 case 2 fails deep in routed-session forwarding | Phases 2–3 don't depend on it. Phase 4's `[[peer]]` dialing gives daemons direct sessions with each other regardless. |
| The coordinator is lost for good | Members keep running and publishing; re-placement stops. Coordinator takeover is out of scope (below). Its `state_dir` makes a restart on the same host safe. |
| Partitions cause temporary over-replication | Accepted by design (D7). Receivers tolerate extra publishers (D8). Cleanup is conditional on incarnation, so it cannot remove a replacement. |
| `boot_epoch` file lost or corrupted | Refused at boot, not silently reset (I3). An operator reset must be explicit, and the docs say receivers may drop that publisher's summaries until its epoch passes the last one they saw. |
| Operators deploy one-subnet-per-daemon and want more subnets per host | Run several daemons per host; each is cheap. Per-group nodes inside one daemon (rejected in D0) stay available as a follow-up if this hurts. |
| S2 shows ingest, not the fold, is the ceiling | S3 records the real figure. Sharding gets its own plan. |
| v1 and v2 served side by side drift apart | v1 is frozen, with golden-byte tests on the current layouts. It is removed in a named later release. |
| Windows dev box: no `#[cfg(unix)]` coverage, and the disk is tight | Multi-daemon tests use loopback UDP and `block_peer`, nothing unix-gated. Check free disk before the pre-push checklist. |

---

## Not in scope

- Sharding one source subnet across aggregators (only if S2 calls for it, and
  then in its own plan).
- Federation across meshes, and strongly consistent cross-subnet operations.
- Fencing or strict member exclusion during partitions (D7 chooses temporary
  over-replication).
- Coordinator takeover by another daemon.
- Making aggregators migratable `MeshDaemon`s, or a general remote-spawn
  primitive (D1).
- Several source subnets on one daemon (D0).
- Custom `Summarizer`s from bindings.
- Replacing the mesh PSK as the membership secret.

---

## Defects found while writing this plan

| ID | Defect | Where | Disposition |
|---|---|---|---|
| F1 | Daemon never serves `fold.query` | `aggregator-daemon/src/lib.rs:495` | Q2 |
| F2 | The CLI query test's ignore note names the wrong cause | `cli/tests/aggregator_remote.rs:335` | Q2 |
| F3 | Capability summary counts the whole fold, not its `source_subnet` | `aggregator/summarizer.rs:109-150` | S0, S1 |
| F4 | In-process replicas share one `MeshNode` and publish duplicates | `aggregator-daemon/src/lib.rs:680-692` | M0 |
| F5 | No `ChannelConfigRegistry` is installed, so summary visibility is never enforced | `aggregator-daemon/src/lib.rs:464-466`; `aggregator/daemon.rs:483-487` | Q0 |
| F6 | Predecessor plan claims CLI `query` is closed | `AGGREGATOR_LIFECYCLE_DEFERRED_2026_05_23.md` | P0-1 |
| F7 | Daemon identity is regenerated every boot | `aggregator-daemon/src/lib.rs:464-466` | I1 |
| F8 | Spec promises fencing, identity continuity and `ReplicaGroup` deployment that were never built | `SCALING_SUBNET_SPEC.md` (G10) | P0-2 |

---

## Review 1 (2026-10-23)

The reviewer worked from source inspection plus one executed postcard probe;
the Net test suite was not run. Their recommendation: proceed with Phases 0–1;
revise Phases 2–3 around boot, configuration and publication scope; gate Phase 4
on the lifecycle and receiver contracts. All findings were checked against the
code before this revision and accepted.

| # | Sev | Finding | Verified at | Answered by |
|---|---|---|---|---|
| 1 | High | No persistent daemon identity and no peer bootstrap, yet D5/M1/M4 assumed both | `aggregator-daemon/src/lib.rs:464-466`; config fields `:111-176` | G6, D6, I1, I2 |
| 2 | High | S1's classification policy had no production source | config has no policy; `MeshNodeConfig::new` → `None`; classifier at `subnet/assignment.rs:232` | G3, D4, S0, S1's end-to-end proof |
| 3 | High | Registering summary channels was a no-op, and visibility is per node, not per summary label | `daemon.rs:483-487`; `mesh.rs:35524-35527` | G5, D0, Q0 (real allowed/denied delivery); `fold.query` boundary in D0 |
| 4 | High | Remote Spawn lacked uncertain-outcome, partial-failure and teardown semantics | `registry_service.rs:606` (`DuplicateGroupName`) | G8, D7, M3 witnesses |
| 5 | High | A millisecond start time is not a monotonic epoch | design flaw in the first draft's D3 | D3 rewritten, I3, M6 ordering witnesses |
| 6 | High | "Most recently received" had no freshness lifecycle; `filter_novel` suppresses unchanged summaries | `daemon.rs:347-364` | D8, M6 freshness witnesses |
| 7 | Med | Missed `List` budget ≠ stopped member | design gap | D7 partition semantics (over-replication, no fencing), M4 partition witness |
| 8 | Med | The claim that appended postcard fields fail closed was false; renaming changes no bytes | reviewer's probe, postcard 1.1.3 | G9, D9 (versioned names, strict decoding), golden-byte tests in Q1/M3/M6 |
| s1 | — | Reservation summaries must not carry mesh-wide counts under a subnet label | — | D4 (GLOBAL-only), S1 refusal test |
| s2 | — | Counters must cover restore and forced replacement, read coherently, and say what they count | — | D4 ("what the counts mean", "how counts are kept"), S1 property and coherence tests |
| s3 | — | Phase 0 must correct the spec's obsolete contracts, not only the capacity figure | `SCALING_SUBNET_SPEC.md` lines in G10 | P0-2 |

What changed in the design, in short:

- **One source subnet per daemon (D0).** This is the smallest scope the existing
  gateway can enforce.
- **Daemons have persistent identity, state and peers (D6).**
- **Remote members follow an incarnation-tagged, idempotent protocol (D7),**
  which chooses over-replication over fencing.
- **Ordering and freshness are separate contracts (D3, D8).**
- **Wire changes go to versioned names with strict decoding (D9),** instead of
  appended fields.
