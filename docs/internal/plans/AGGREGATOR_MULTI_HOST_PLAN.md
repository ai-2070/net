# Aggregators across hosts: correct summaries, a live query path, and replicas that leave the box

## Status

**Planned. Revised after reviews 1, 2 and 3** (all 2026-10-23), on branch
`LZL0/aggregators` (forked from `master` at `541cc0078`). No release has been
chosen.

- **Phases 0 and 1 are cleared to start.**
- **Phase 2 now opens with the subnet-policy slice** (formerly S0), because its
  delivery and query-access checks cannot classify callers without it
  (review 2).
- **Phases 2 and 3 are cleared at the plan level** (review 3). Implementation
  and acceptance evidence are still owed.
- **Phase 4 is gated** on the identity, member-lifecycle and receiver contracts
  (D1, D3, D7, D8) being implemented as written.
  - Review 2 tightened D3's ordering scope and D7's member identity and
    retirement memory.
  - Review 3 replaced `ForgetGroup` with floor-based reclamation, made the
    identity lock unconditional, and split the host's desired state from
    observed liveness, with crash recovery.
  - This revision awaits re-review.

Predecessors, both closed: `AGGREGATOR_LIFECYCLE_DEFERRED_2026_05_23.md`
(AL-1..AL-8) and `AGGREGATOR_CLI_REMOTE_ATTACH_AND_SCALE_RPC.md` (A, B, C).
Phase 0 corrects claims in them, and in `SCALING_SUBNET_SPEC.md`, that turned
out to be wrong.

[Review 1](#review-1-2026-10-23), [review 2](#review-2-2026-10-23) and
[review 3](#review-3-2026-10-23) list every finding and where it is answered.

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

Before `start`, boot installs a `ChannelConfigRegistry`. It has one entry for
every summary channel name the daemon can publish on, for every fold kind any
group **or template** uses, so dynamically spawned groups are covered:

- `summary/<kind>` from Phase 2;
- `summary.v2/<kind>` from M6 on (both while v1 is still served).

Each entry carries the daemon's `summary_visibility`. Groups on one daemon must
agree on visibility; boot refuses a conflict.

Then publication scope is something the existing gateway can enforce.

**At most one group per fold kind per daemon** (review 2, finding 3). One
subnet per daemon means two groups of the same kind would read the same fold
and publish the same counts on the same channel. Boot and Spawn refuse a second
one (`KindOccupied`). This makes `(publisher, kind)` the unique identity of a
summary stream, which D3 relies on.

**The subnet policy is a prerequisite for D0, not a Phase 3 refinement**
(review 2, finding 1). The gateway and the `fold.query` check both need the
caller's subnet. The production announcement path records a peer's subnet only
when the node has a policy installed (`mesh.rs:42139-42143`); setting the
daemon's own `subnet` classifies nobody. D4's `[[subnet_rule]]` therefore lands
in the same phase (Q0).

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

**D3. Ordering a summary stream across restarts.** *(Revised after review 1,
finding 5: it was "epoch = start time in ms", which is not monotonic under clock
correction or two starts in the same millisecond. Revised again after review 2,
finding 3: ordering was per publisher, so one group's restart retired a sibling
group's live stream.)* *Recommend: a persisted, publisher-wide boot counter,
plus a start sequence scoped to the stream.*

- **The stream.** A stream is `(publisher, subnet, kind)`. D0 makes it unique
  per group: one subnet per daemon and at most one group per kind. A
  publisher's capability and reservation summaries are therefore separate
  streams, ordered independently.
- **`boot_epoch: u64`** is publisher-wide. It lives in the state directory (D6)
  and is allocated once per boot, before anything is published, under I3's
  exclusive lock with a crash-safe write.
- **`stream_seq: u32`** is per stream and in memory, reset each boot. A stream
  takes the next value each time its member starts within the boot, including a
  `HealthMonitor` respawn, whose fresh `AggregatorDaemon` restarts `generation`
  at 0.
- **What a summary carries:** `boot_epoch`, `stream_seq` and `generation`.
- **How a receiver orders them.** Per stream, order is lexicographic on
  `(boot_epoch, stream_seq, generation)`. A summary that sorts below the highest
  one already seen **on that stream** is dropped. A summary on a different
  stream from the same publisher never retires it.
- **Why this works.** The publisher's identity persists (D6), and `boot_epoch`
  increases strictly across that identity's boots (I3). Within a boot,
  `stream_seq` increases strictly for its stream.

Rejected:

- **Wall-clock time.** Not monotonic.
- **A random incarnation id.** Not ordered.
- **Coordinator-issued incarnations for ordering.** The order would depend on
  the coordinator being reachable when a member restarts. D7's coordinator
  incarnation is a lifecycle identity, not an ordering key.
- **A publisher-wide start sequence** (review 1's revision). It let sibling
  streams retire each other (review 2).

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
- **Policy fingerprint.** A BLAKE2s hash of the rules **in declaration order**
  is reported in `List`. Spawn compares it (D7) so a group's hosts cannot
  classify differently.
  - Order is part of the policy: `SubnetPolicy` resolves same-level matches as
    later-rule-wins (`subnet/assignment.rs:30-35`). Sorting the rules before
    hashing would give two different policies one fingerprint (review 2,
    finding 2).
  - Only each rule's value map, which is unordered, is canonicalized (entries
    sorted by key).

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
- **State.** `state_dir = "<path>"` holds `boot_epoch` (D3), the coordinator's
  records and the host's member records (D7). Phase 4 features require it.
- **Exclusive ownership** (review 2; identity lock made unconditional in
  review 3, finding 2). At boot, every daemon with a persistent identity takes
  two exclusive advisory locks and holds them for the process lifetime (the
  `fs2` crate, already a `net-mesh-sdk` dependency):
  - **`<identity>.lock`**, next to the identity file, wherever that file lives,
    found by its canonicalized path;
  - **`<state_dir>/LOCK`.**

  A second process that shares either the identity file or the state directory
  refuses to boot, whatever the layout. Locking the identity only when it sat
  outside `state_dir` let a daemon with the identity inside its own state
  directory and another daemon pointing at that same file from elsewhere hold
  disjoint lock sets (review 3).

  `state_dir` records the node id it belongs to, and refuses a different
  identity. Durable writes go to a temporary file, then fsync, rename and
  directory fsync.

  Out of reach of any lock: a *copy* of the identity file at another path. That
  is the operator's responsibility and is documented as such.
- **Peers.** `[[peer]]` blocks give `addr`, `public_key` and `node_id`. The
  daemon dials each with the SDK's `connect_via` (the routed handshake, which
  completes against a running responder without pre-`accept`). It redials with
  backoff when a session drops.

**D7. Member lifecycle protocol.** *(New in review 1, finding 4. Revised after
review 2, findings 4 and 5: member identity now includes the coordinator, and
the host keeps retirement memory so a retired member cannot come back.)* This
is bounded soft-state reconciliation between two daemons. There are no
distributed transactions and no fencing.

**Member identity** is `(coordinator, group, index)`, plus an `incarnation: u64`
from **one** counter in the coordinator's `state_dir`. It is shared by all of
that coordinator's groups and only ever increases. A recreated group therefore
always gets incarnations above every one its earlier lifetime used; the floor
below relies on this.

- **Where `coordinator` comes from.** It is the authenticated
  `RpcContext::session_peer` of the request, never a payload field.
- **Direct sessions only.** `session_peer` authenticates the deliverer
  (`registry_service.rs:314-325`), so member ops are accepted only over a
  direct session. D6's `[[peer]]` dial is a single-hop `connect_via`, so the
  deliverer is the coordinator. A member op whose session peer is a relay is
  refused.
- **Local name.** On the host, the member runs as the local group
  `<coordinator_hex>/<group>#<index>`. Two coordinators that both create `g`
  with index 0 and incarnation 1 get two distinct records; neither can replay,
  adopt or clean up the other's member. D0's one-group-per-kind rule still
  applies on the host, so the second spawn of the same kind gets `KindOccupied`
  rather than a silent replay.

**Host-side state.** *(Revised after review 3, findings 1 and 3. `ForgetGroup`
is gone, and durable desired state is now separate from observed liveness.)*

The host keeps two kinds of durable state in its `state_dir`, plus one fact
observed in memory.

- **Per coordinator:** `floor: u64`, starting at 0. It only ever increases. Any
  member op carrying an incarnation `< floor` (strictly below) is answered `Retired { floor }`
  with no state change. The floor is the retained rejection evidence that lets
  records be reclaimed without reopening resurrection.
- **Per member key `(coordinator, group, index)`:**
  `{ high_water: u64, desired: Option<Desired> }`, where
  `Desired = { incarnation, template, template_fp, policy_fp }`.
  - `high_water` is the highest incarnation ever accepted for the key.
  - `desired` is what the host has committed to run. It carries enough to
    restart the member after a crash.
- **Observed, in memory only:** whether a live task exists for the desired
  incarnation. A persisted record never claims a task is alive. Only the
  in-memory handle does.

What the host reports for a key follows from desired and observed together:

- `Running(i)`: desired is `i` and its task is live.
- `Starting(i)`: desired is `i` and no task is live yet (accepted, recovering,
  or retrying a failed start).
- `StartFailed(i, reason)`: the last attempt to start `i` failed. Desired is
  kept, and a retried Spawn tries again.
- `Retired`: nothing desired.

**Transitions.** Every one persists before it acts, so a crash at any point
leaves either the old durable state or the new one. Recovery on boot (below)
then makes the observed state match.

- **`SpawnMember { template, group, index, incarnation: i, policy_fp, template_fp }`:**
  - `i < floor`, or `i < high_water`, or (`i == high_water` and desired is not
    `i`) → `Retired`, with no change. A delayed Spawn after a teardown, or after
    a reclamation, cannot start anything.
  - desired is `i` → a replay. Reply `Spawned` **only if the task is live**;
    otherwise reply `Starting` (retryable) and make sure a start is in
    progress. An unfinished valid start is never reported `Retired`, and a
    missing task is never reported `Spawned`.
  - `i > high_water` →
    1. persist `{ high_water = i, desired = i }`;
    2. stop any task for a lower incarnation;
    3. start `i`;
    4. reply `Spawned` once the task is live, or `StartFailed` if it fails.
  - a fingerprint mismatch → refused, with no state change.
- **`UnregisterMember { group, index, incarnation: i }`:**
  - `i < floor` or `i < high_water` → `Superseded`, with no change. A delayed
    cleanup cannot stop a replacement.
  - otherwise →
    1. persist `{ high_water = i, desired = None }`;
    2. stop the task if one is live;
    3. reply `Stopped`.

    Replays get `Stopped` again.
- **`Reclaim { below: u64 }`** (replaces `ForgetGroup`):
  1. persist `floor = max(floor, below)`;
  2. delete only records that have `desired = None` and `high_water < floor`.

  Properties:
  - **It never affects a newer group lifetime.** The coordinator's counter
    only increases, so every record of a recreated group has
    `high_water ≥ below` and survives.
  - **A delayed old `Reclaim` is a no-op.** Its `below` is not above the
    current floor, so `max` leaves the floor unchanged.
  - **Deleted keys stay rejected.** A delayed `SpawnMember` for a deleted key
    has `i < floor` and gets `Retired`.

  The coordinator computes `below` as the smallest incarnation that is still
  desired, or still has a pending intent, among its members on that host. If
  there are none, it uses its next unallocated incarnation. It sends `Reclaim`
  only after writing that value to its own write-ahead log.
- **Retention.** Per-key records live until `Reclaim` passes them, never by
  age. Per-coordinator floors are kept permanently: one `u64` for each
  coordinator ever admitted, bounded by the `operators` list. Live keys are
  bounded by `max_groups × max_replica_count`. At that bound the host refuses
  new keys (`HostFull`) instead of evicting records.

**Host recovery on boot.** This happens before any member op is served.

1. Under the D6 locks, load the floors and records.
2. Start every desired member.
3. Only then register the `aggregator.registry.v2` member ops.

Details:

- A desired member whose template no longer exists, or whose fingerprint no
  longer matches, stays desired and reports `StartFailed(TemplateChanged)`.
  The coordinator sees that in `List` and in Spawn replays, and treats it as a
  member failure (re-place or roll back).
- Ordinary restarts take the same path. They lose every in-process task, so
  "desired" is all that carries over and liveness is re-observed.

**What a host crash leaves behind**, by crash point:

| Crash point | State on restart | Retry answer |
|---|---|---|
| Spawn: before persisting | Old state | Spawn proceeds normally |
| Spawn: after persisting, before start | Desired `i`; recovery starts it | `Spawned` once live; `Starting` until then. Never `Retired` |
| Spawn: after start, before reply | Desired `i`; recovery restarts it (the old task died with the process) | `Spawned`; exactly one member |
| Replacement: after persisting new desired, before stopping old | Desired is the new incarnation only; the old task died with the process | New incarnation runs; old is `Superseded`/`Retired` |
| Unregister: after persisting, before stop | Nothing desired; nothing restarted | `Stopped` |
| Reclaim: after persisting floor, before deleting records | Floor raised; recovery finishes the deletion | Delayed old Spawn → `Retired` |

**Coordinator write-ahead intent.** Before any remote effect, the coordinator
fsyncs its intent to `state_dir`:

- the allocated incarnation;
- the target host;
- the operation (`Spawning`, `Retiring`, `Reclaiming { below }`).

It updates the record only after the reply. On restart, every unresolved intent
is re-sent unchanged. That is safe because each op is idempotent under the
host record. This covers a crash after the remote success and before the reply.

- **Partial failure.** A logical Spawn either succeeds for all N members or is
  rolled back: each started member gets a `Retiring` intent, and the operator
  gets a typed error naming the member that failed.
- **Logical Unregister.** The coordinator writes a tombstone and a `Retiring`
  intent for every member incarnation. Repeated Unregisters return the same
  result while the tombstone exists.
- **Absence is not retirement.** A cleanup obligation stays pending, retried
  with backoff and on every reconnect, for as long as it takes. Elapsed time
  never discharges it. The first draft's "retire budget" is removed. A host that
  comes back after a long absence still gets its `UnregisterMember`, and its
  record makes the result exact.
- **Partition semantics** (review 1, finding 7). There is **no fencing**:
  - when a host misses its `List` budget, the coordinator places a replacement
    with a new incarnation, usually on another host;
  - during the partition, the group may run more than N members;
  - after the partition heals, the pending conditional cleanup stops the old
    incarnation, and never the replacement;
  - extra publishers are harmless to receivers (D8).

  A host never retires a member on its own because its coordinator went quiet.
  An unreachable coordinator is not a deleted group. Strict exclusion is not
  promised.

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
- **Late subscribers.** `SummaryView::bootstrap` calls `fold.query.v2`
  `LatestSummary` on any known member. Otherwise the view fills within one
  `refresh_interval`.
  - The v2 response carries the ordering and freshness fields each summary
    needs (`boot_epoch`, `stream_seq`, `generation`, `refresh_ms`). Frozen v1
    summaries cannot supply them (review 2).
  - Q2 freezes those fields in the v2 layout from the start: `refresh_ms` is
    populated from Phase 2, and `boot_epoch` and `stream_seq` are `Option`s
    filled in from I3. That way M6 needs no v3.
  - A bootstrapped summary enters the view under the same ordering rule as a
    channel one.

**D9. Wire evolution.** *(New in review 1, finding 8.)* *Recommend: versioned
service and channel names, with strict decoding on the new versions.*

- **What gets versioned.** Changed shapes ship under new names:
  - `fold.query.v2` (the group selector, Q2);
  - `aggregator.registry.v2` (the D7 ops and the M3 reply shape);
  - channel `summary.v2/<kind>` (D3 incarnation, D8 `refresh_ms`).
- **Strict decoding.** v2 decoders use `postcard::take_from_bytes` and reject
  any trailing bytes.
- **Mixed versions fail closed.** A new client calling an old server gets an
  explicit refusal instead of silently dropped fields.
- **`ServerTooOld` comes from one observable result only** (review 2):
  `RpcStatus::NotFound` for the v2 service name. The mesh answers unserved
  requests with it (`mesh_rpc.rs:933`, via `answer_unserved_rpc_requests`).
  Other failures keep their own kinds: a timeout stays `Timeout` (the server's
  version is unknown), `Unauthorized` stays `NotAuthorized`, and a missing
  capability advertisement stays a discovery result.
- **v1 stays.** v1 names keep their current layouts and are served for at least
  one release.

Exact v2 field layouts are frozen in the slices that introduce them (Q2, M3, M6)
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

### Phase 2 — Subnet policy, publication scope and a live query path (G3, G5; about 1½ weeks)

Every Phase 2 test boots the daemon entirely from production TOML. No policy,
subnet or channel registry is injected through a test-only setup path
(review 2, finding 1).

- **Q0. Subnet policy in the daemon (D4).** *(Moved from Phase 3's S0 in
  review 2: Q1 and Q2 cannot classify callers without it.)*
  - `[[subnet_rule]]` parsing, installed with
    `MeshNodeConfig::with_subnet_policy`.
  - The missing-policy refusal.
  - The declaration-order policy fingerprint in `List`.

  *Proof:*
  - Boot cases: rules installed, and a non-global subnet with no rules refused.
  - Fingerprint: inserting a rule's value-map entries in a different order
    leaves it unchanged. Reversing two conflicting same-level rules changes
    **both** the classification of a sample tag set and the fingerprint.
  - A direct peer's subnet appears in `peer_subnets` after its announcement,
    with the policy coming only from the TOML.
- **Q1. Daemon publication scope (D0).**
  - Add `subnet`, and source-subnet and visibility agreement checks for groups
    and templates, plus the one-group-per-kind refusal.
  - Install the `ChannelConfigRegistry` before `start`, covering every static
    and template fold kind.

  *Proof*, in `aggregator-daemon/tests/`, with real delivery rather than a
  registered field: a daemon in subnet S with `SubnetLocal` summaries, its
  policy from TOML.
  - A subscriber in S receives `summary/<kind>`.
  - A subscriber in a sibling subnet is denied, as counted by the gateway's
    dropped counter and by no delivery.
  - A Spawn-created group's channel gets the same verdicts.
  - Boot refuses a template whose subnet disagrees, and a second group of an
    occupied kind.
- **Q2. `fold.query.v2`** (D9).
  - Request layout `{ kind: u16, op: FoldQueryOp, group: Option<String> }`.
  - Each summary in the response carries
    `{ summary, generation: u64, refresh_ms: u32, boot_epoch: Option<u64>, stream_seq: Option<u32> }`
    (D8).
  - Errors add `UnknownGroup` and `NotAuthorized`.
  - Strict decoding.
  - The D0 access check.

  *Proof:*
  - Round trips in `tests/aggregator_fold_query.rs` for all groups, a named
    group, and an unknown group.
  - Golden-byte tests of the request and response layouts.
  - Trailing bytes rejected.
  - A same-subnet caller admitted, a sibling-subnet caller refused, and an
    `operators` caller admitted, all classified by the TOML policy.
  - A v2 client against a v1-only server gets `ServerTooOld`. That mapping is
    tested from the observed `NotFound`, and a timeout is tested to stay
    `Timeout`.
- **Q3. The daemon installs v1 and v2.** v1 answers with "all groups" semantics.
  The `ServeHandle`s are kept on `BootedDaemon`.

  *Proof:* un-ignore
  `cli/tests/aggregator_remote.rs::query_returns_summary_for_configured_group`,
  and replace its ignore note's premise: the target is the daemon's node id.
- **Q4. Bindings expose the group argument and the new error kinds.**
  - Node: `bindings/node`, `sdk-ts/src/aggregator.ts`.
  - Python: `bindings/python`.
  - Go: `go/`, `bindings/go/*-ffi`.
  - C: `include/` and `tests/c_sdk/aggregator.c`.

  C ABI changes follow the single-commit rule.

  *Proof:* each binding's aggregator test gains named-group and unknown-group
  cases, and `aggregator.c` keeps its NET-CHECKS line.

### Phase 3 — Summaries that are right, and cheap at 1M (G3, G7; about 1 week)

The subnet policy itself arrives in Q0. This phase uses it for counting.

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
- **I3. State-directory ownership and boot epoch (D3, D6).**
  - The exclusive locks on `state_dir` and the identity file, and the node-id
    binding of `state_dir`.
  - Crash-safe `boot_epoch` allocation (temporary file, fsync, rename,
    directory fsync) under the lock, before first publish.
  - `stream_seq` per stream start.

  *Proof:*
  - Restart increments `boot_epoch`.
  - A second daemon on the same `state_dir` refuses to boot while the first
    runs.
  - **Asymmetric shared identity** (review 3): daemon A has `state_dir = stateA`
    and `identity = stateA/identity`; daemon B has `state_dir = stateB` and
    `identity = stateA/identity`. B refuses to boot while A runs, and so does A
    when started second.
  - A `state_dir` written by another identity is refused.
  - **Torn writes:** killing the process between write and rename leaves either
    the old or the new value, never a torn file.
  - **No published epoch is ever reused** (review 3). Publishing starts only
    after the epoch's rename and directory fsync. A boot that crashed before
    that commit may hand out the same value again, which is safe because
    nothing was published under it. The witness kills the process at both
    points: before the commit, the next epoch may repeat the uncommitted value;
    after the first publish, it must be strictly greater.
  - A corrupt or missing epoch file is refused rather than reset, unless the
    operator passes an explicit reset flag.
- **M0. One member per host (D2).**
  - `replica_count > 1` on a daemon with no peers is refused at Spawn.
  - A static `[[group]]` with `replica_count > 1` warns and clamps to 1 for one
    release, then is refused.

  *Proof:* `boot_and_query.rs` refuse and clamp cases.
- **M1. Hosts announce themselves.** `aggregator-host`, one
  `aggregator-template:<name>` per template, the subnet, the policy fingerprint,
  free-member capacity, and the fold kinds already occupied (D0), so placement
  skips hosts that would answer `KindOccupied`.

  *Proof:* two daemons dialed via `[[peer]]` find each other by filter.
- **M2. Placement over hosts.** The candidate set comes from M1 and is limited
  to the group's subnet. Placement uses `GroupCoordinator::place_with_spread`
  and a typed `PlacementFailed`.

  *Proof:* three members land on three distinct hosts; a fourth fails with
  `PlacementFailed` when only three hosts qualify.
- **M3. Remote members under D7** (`aggregator.registry.v2`).
  - `SpawnMember`, `UnregisterMember` and `Reclaim` with frozen layouts, and
    the reply set `Spawned`, `Starting`, `StartFailed`, `Stopped`, `Retired`,
    `Superseded`, `KindOccupied` and `HostFull`.
  - Coordinator identity from `session_peer`, with relayed member ops refused.
  - Template and policy fingerprints.
  - The host's per-coordinator floors and per-key desired records, with boot
    recovery before member ops are served. Also the coordinator's write-ahead
    intents. All of it persisted in `state_dir`.
  - Host crash points for the witnesses come from a crash-injection hook that
    aborts the daemon subprocess at a named point. It is compiled only in test
    builds and never changes configuration.
  - `List` v2 reports `host_node_id: u64` (not `Option`), the coordinator and
    the incarnation per member.

  *Proof*, multi-daemon tests on loopback:
  - **Dropped reply:** a Spawn reply blocked with `MeshNode::block_peer`
    (`mesh.rs:24467`) for one exchange is retried and yields exactly one member.
  - **Coordinator crash:** the coordinator is killed after the host started the
    member and before the reply arrived. On restart it re-sends the intent,
    gets `Spawned` as a replay, and there is one member.
  - **Delayed Spawn after teardown:** a `SpawnMember` for incarnation 7
    arriving after `UnregisterMember` 7 succeeded gets `Retired`; nothing
    starts.
  - **Delayed cleanup after reuse:** an `UnregisterMember` for an old
    incarnation, arriving after a newer one started, gets `Superseded` and
    leaves the replacement running.
  - **Two coordinators** with identical group names, indices and counter values
    on one host. Neither can replay, adopt or clean up the other's member. The
    second spawn of an occupied kind gets `KindOccupied`.
  - **A relayed member op** (session peer ≠ coordinator) is refused.
  - **Partial failure:** a failure after one remote member started rolls the
    group back, and the started member is gone.
  - **Repeated logical Unregister** is idempotent.
  - **Reclamation** (review 3, finding 1):
    - A delayed `SpawnMember` arriving **after `Reclaim`** has deleted its key
      gets `Retired`, and nothing starts.
    - A delayed **old `Reclaim`** arriving after the group was recreated with
      higher incarnations leaves the new records and members untouched.
    - With a host restart between `Reclaim` and each of those delayed
      operations, the answers are the same.
  - **Host crash recovery** (review 3, finding 3), using the crash-injection
    hook on the host subprocess:
    - **After persisting, before start:** the retry gets `Starting` or
      `Spawned`, never `Retired`, and ends with exactly one member.
    - **After start, before the reply:** the retry gets `Spawned` only once the
      recovered task is live, with exactly one member.
    - **During replacement:** only the new incarnation runs afterwards.
    - **During teardown:** nothing is restarted, and the retry gets `Stopped`.
    - **Any other point:** no member op is answered before recovery finishes,
      and no `Spawned` reply is ever sent for a key with no live task.
  - **`HostFull`** at the live-key bound, with no record evicted.
  - **Golden bytes:** one test per v2 layout.
- **M4. Health and re-placement (D7 partition semantics).**

  *Proof:*
  - **Partition:** cut a **still-running** member off with `block_peer` on
    both sides. The coordinator places a replacement with a new incarnation.
    While partitioned, both publish. After healing, the pending cleanup removes
    the old incarnation and not the replacement.
  - **Long absence:** a host that returns long after any timeout the
    coordinator uses still receives its pending `UnregisterMember`, and its
    member stops.
  - **Stopped daemon:** stopping a daemon outright is a separate, narrower
    test.
- **M5. Scale with placement.** `add_replica_with_placement`, or the
  distributed-group equivalent. Scale-down retires the member on the most loaded
  host (highest index on a tie) via conditional Unregister.

  *Proof:* 2→4→2 across four hosts, with distinct hosts throughout. Surviving
  members keep their incarnation.
- **M6. `summary.v2` and `SummaryView` (D3, D8)**, built in `net-mesh-sdk` first
  and then exposed in the bindings.

  M6 also registers `summary.v2/<kind>` in the daemon's channel registry with
  the same visibility as v1 (D0). Q1's allowed/denied delivery test is repeated
  on the v2 channel.

  *Proof*, ordering witnesses:
  - A backward wall-clock step between restarts.
  - Two restarts within the same millisecond.
  - A `HealthMonitor` respawn within one boot.
  - A delayed old-incarnation summary arriving after the new one.
  - Progress by the current incarnation afterwards.

  In each case, the old incarnation is dropped and the new one wins.

  *Proof*, sibling streams (review 2, finding 3): a GLOBAL daemon runs a
  capability group and a reservation group at once. Restarting either one,
  in either order, never causes the other's later summaries to be dropped.

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
| A coordinator that is gone for good leaves member records, and possibly running members, on its hosts | Records are never evicted by age, because that would reopen resurrection. An operator on the host retires the members with the host's own operator path. That path raises the coordinator's floor past them, so their records become reclaimable and stay rejected. `HostFull` makes the bound visible rather than silent. |
| Host recovery starts a desired member whose template changed since it was accepted | It stays desired and reports `StartFailed(TemplateChanged)`. The coordinator treats it as a member failure; nothing runs under a template that no longer matches its fingerprint. |
| One group per fold kind per daemon is too tight for some deployment | Run another daemon on the host. The restriction exists because two same-kind groups on one subnet would publish identical streams (D0). |
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
| F1 | Daemon never serves `fold.query` | `aggregator-daemon/src/lib.rs:495` | Q3 |
| F2 | The CLI query test's ignore note names the wrong cause | `cli/tests/aggregator_remote.rs:335` | Q3 |
| F3 | Capability summary counts the whole fold, not its `source_subnet` | `aggregator/summarizer.rs:109-150` | Q0, S1 |
| F4 | In-process replicas share one `MeshNode` and publish duplicates | `aggregator-daemon/src/lib.rs:680-692` | M0 |
| F5 | No `ChannelConfigRegistry` is installed, so summary visibility is never enforced | `aggregator-daemon/src/lib.rs:464-466`; `aggregator/daemon.rs:483-487` | Q1 |
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
| 2 | High | S1's classification policy had no production source | config has no policy; `MeshNodeConfig::new` → `None`; classifier at `subnet/assignment.rs:232` | G3, D4, S0 (now Q0), S1's end-to-end proof |
| 3 | High | Registering summary channels was a no-op, and visibility is per node, not per summary label | `daemon.rs:483-487`; `mesh.rs:35524-35527` | G5, D0, Q0 (now Q1; real allowed/denied delivery); `fold.query` boundary in D0 |
| 4 | High | Remote Spawn lacked uncertain-outcome, partial-failure and teardown semantics | `registry_service.rs:606` (`DuplicateGroupName`) | G8, D7, M3 witnesses |
| 5 | High | A millisecond start time is not a monotonic epoch | design flaw in the first draft's D3 | D3 rewritten, I3, M6 ordering witnesses |
| 6 | High | "Most recently received" had no freshness lifecycle; `filter_novel` suppresses unchanged summaries | `daemon.rs:347-364` | D8, M6 freshness witnesses |
| 7 | Med | Missed `List` budget ≠ stopped member | design gap | D7 partition semantics (over-replication, no fencing), M4 partition witness |
| 8 | Med | The claim that appended postcard fields fail closed was false; renaming changes no bytes | reviewer's probe, postcard 1.1.3 | G9, D9 (versioned names, strict decoding), golden-byte tests in Q2 (formerly Q1), M3, M6 |
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

---

## Review 2 (2026-10-23)

Re-review of `783128bda`. The reviewer worked from source inspection plus small
executable transition models (rule ordering, sibling-stream suppression,
post-teardown resurrection); the Net test suite was not run.

Verdict: Phases 0–1 stay clear; hold Phases 2–4. The remaining problems were
dependency order, identity scope and lifecycle closure, not a need for a larger
architecture. Findings 1 and 2 and the `ServerTooOld` correction were checked
against the code before this revision. Findings 3–5 are design flaws in the
previous revision, confirmed by reading it against the review's sequences. All
were accepted.

| # | Sev | Finding | Verified at | Answered by |
|---|---|---|---|---|
| 1 | High | Phase 2's delivery and query-access checks need the subnet policy, which arrived only in Phase 3; the production path records peer subnets only when a policy is installed | `mesh.rs:42139-42143` | D0 (policy is a prerequisite); S0 moved to Q0; Phase 2 tests boot from production TOML only |
| 2 | High | Sorting rules before fingerprinting would give order-sensitive policies one fingerprint | `subnet/assignment.rs:30-35` (later rule wins) | D4 fingerprint in declaration order, value maps canonicalized; Q0 witness |
| 3 | High | A publisher-wide start sequence let one group's restart retire a sibling group's live stream | previous D3 | D3 orders per stream `(publisher, subnet, kind)`; D0's one-group-per-kind rule makes the stream unique; M6 sibling-stream witness |
| 4 | High | Member identity omitted the coordinator, so two coordinators could replay or clean up each other's members | previous D7 | D7 identity `(coordinator, group, index)`, coordinator taken from `session_peer` over a direct session; M3 two-coordinator witness |
| 5 | High | Conditional Unregister did not stop a delayed Spawn from resurrecting a retired member; the retire budget gave up on absent hosts | previous D7 | D7 host records with a high-water incarnation, `Retired`/`Superseded`, `ForgetGroup` retention (replaced by floor-based `Reclaim` in review 3), coordinator write-ahead intents, no age-based discharge; M3/M4 witnesses |
| s1 | — | v2 summary channels were never registered for visibility; bootstrap named v1, whose summaries lack ordering/freshness fields | — | D0 registers `summary.v2/<kind>`; D8 bootstraps from `fold.query.v2`, whose layout carries the fields from Q2 on; M6 repeats the delivery test on v2 |
| s2 | — | `ServerTooOld` must not be inferred from arbitrary failures | `RpcStatus::NotFound` (`cortex/rpc.rs:303`), sent for unserved services (`mesh_rpc.rs:933`) | D9 maps only `NotFound` for the v2 name; Q2 witness that a timeout stays `Timeout` |
| s3 | — | Increment-and-fsync does not stop two processes allocating the same epoch | — | D6 exclusive locks on `state_dir` and the identity file, node-id binding, crash-safe writes; I3 witnesses |

What changed, in short:

- **The policy moved to Phase 2's first slice.** Phase 2's tests use
  production TOML only.
- **Fingerprints keep rule order.**
- **Ordering is per stream, not per publisher.**
- **Member identity is scoped to the authenticated coordinator,** and hosts
  remember retirements until the coordinator says to forget them.
- **The coordinator writes its intent before acting,** and never treats a
  host's absence as proof that a member retired.

---

## Review 3 (2026-10-23)

Re-review of `6af702f0c`, based on the revision diff, relevant source checks,
and executable transition models of the counterexamples below; no production
runtime tests.

Verdict:

- **Phases 0–3 are clear to implement.** Implementation and acceptance evidence
  are still owed.
- **Phase 4 stays on hold** for three lifecycle defects in D6/D7.

Review 2's dependency, fingerprint, stream-ordering, coordinator-namespace and
v2-handoff findings were confirmed addressed. The three findings are design
flaws in the previous revision's text, confirmed by tracing the review's
sequences against it. All were accepted.

| # | Sev | Finding | Answered by |
|---|---|---|---|
| 1 | High | `ForgetGroup` deleted the high-water evidence, so a delayed Spawn after Forget was accepted again. An unversioned delayed Forget could also erase a recreated group's records. | `ForgetGroup` removed. A per-coordinator monotonic `floor` is the retained rejection evidence. `Reclaim { below }` raises it with `max` and deletes only retired records below it, so it cannot touch a newer group lifetime, because the coordinator's single counter only increases. Coordinator write-ahead `Reclaiming`. M3 witnesses: Spawn after Reclaim, old Reclaim after recreation, host restart between. |
| 2 | High | The identity lock was conditional on the file living outside `state_dir`, so an inside/outside pair held disjoint lock sets under one identity. | D6: every persistent-identity daemon locks `<identity>.lock` (canonicalized path) plus `<state_dir>/LOCK`. I3 asymmetric-layout witness. A copied identity file remains the operator's responsibility. |
| 3 | High | Host persistence `{ high_water, running }` conflated pending, live and retired. Crash points between persisting and starting gave false `Retired` or false `Spawned`, and a restart lost every task. | D7: durable `desired` (carrying enough to restart) separate from in-memory observed liveness. Reported states `Running`/`Starting`/`StartFailed`/`Retired`. `Spawned` only when a task is live. Boot recovery before member ops are served. A crash-point table. M3 host-crash witnesses via a test-only crash-injection hook on the daemon subprocess. |
| s1 | — | "The next boot allocates past both" demanded more than the write protocol provides. | I3: no *published* epoch is ever reused; reusing a never-committed, never-published value is allowed. Witness kills at both points. |

What changed, in short:

- **The host never forgets an incarnation without a floor that still rejects
  it.**
- **Every daemon locks its identity file.**
- **The host persists what it intends to run, not what it believes is
  running.** It re-observes liveness after any restart before answering member
  ops.
