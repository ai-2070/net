# Aggregators across hosts: correct summaries, a live query path, and replicas that leave the box

## Status

**Planned**, 2026-10-23, on branch `LZL0/aggregators` (forked from `master` at
`541cc0078`). No release has been chosen. Phases 0–3 are small and could ship
together; Phase 4 is the multi-week part.

Predecessors, both closed: `AGGREGATOR_LIFECYCLE_DEFERRED_2026_05_23.md`
(AL-1..AL-8) and `AGGREGATOR_CLI_REMOTE_ATTACH_AND_SCALE_RPC.md` (A, B, C).
Phase 0 corrects two claims in them that turned out to be wrong.

---

## The gap

The single-host aggregator ships and works. The `net-aggregator-daemon` binary
runs groups from a TOML config. It has template-based Spawn/Scale/Unregister/List
over `aggregator.registry`, auto-respawn, and an `operators` allowlist. The CLI
verbs and the Rust/TS/Python/Go/C clients all exist. What the spec
(`SCALING_SUBNET_SPEC.md`) promises and the code does not do is listed below.
Each item was checked against the tree at `541cc0078`.

### G1. Replicas never leave the host (the original item 1)

- `LifecycleGroup::spawn_with_placement` asks the scheduler for one host per
  replica, records the answer, and then starts every replica locally
  (`src/adapter/net/behavior/lifecycle/group.rs:191-200`: "The group does not
  actually move daemons across nodes; that is the substrate's remote-spawn
  responsibility").
- No remote-spawn path exists. The migration machinery (`compute/orchestrator.rs`,
  `migration_source.rs`, `migration_target.rs`) moves `MeshDaemon`s, but
  aggregators implement the sibling `LifecycleDaemon` trait (AL-1, direction B).
- `add_replica` does not consult the scheduler at all
  (`lifecycle/group.rs:380-387`).
- The daemon binary does not use placement in any form. It calls plain
  `LifecycleGroup::spawn` (`aggregator-daemon/src/lib.rs:680`).

### G2. In-process replicas duplicate each other (found while checking G1)

Every replica the daemon builds gets the same `Arc<MeshNode>`
(`aggregator-daemon/src/lib.rs:680-692`, and the monitor and scale factories
below it). `AggregatorDaemon::produce_summaries` reads `self.mesh.capability_fold()`
(`behavior/aggregator/daemon.rs:385`). So N replicas on one host:

- read the same fold,
- compute the same summary,
- publish it N times on the same channel from the same node.

They protect only against one replica's task dying. The derived replica keypairs
are never used for anything. `registry_service.rs:137-143` already notes they
"authorize nothing in the aggregator path".

### G3. Summaries ignore their subnet (found while checking G2)

`source_subnet` is only a label. `CapabilityFoldSummarizer::summarize`
(`behavior/aggregator/summarizer.rs:109-150`) counts every entry in the node's
fold. A daemon running groups for subnets A and B publishes the same counts
twice, labelled A and B. That is wrong output, not just slow output. The
subnet-scaling story (`SCALING_SUBNET_SPEC.md` §1: aggregators bridge one
source subnet to its parent) depends on this filter.

The mesh's `peer_subnets` map cannot supply the filter by itself. It is written
only for direct (hop 0) announcements (`mesh.rs:42126-42142`, witnessed by
`tests/capability_broadcast.rs::forwarded_announcement_does_not_write_relay_peer_subnet`),
so origins learned by forwarding have no entry.

### G4. Announcement relay through an aggregator (the original item 2)

The July Hermes work saw two clients attached to one `net-aggregator-daemon`
fail to see each other's capability tags. Commit `caee075ac` (2026-09-25, V3 S6)
then added hub replay of held announcements (`mesh.rs:42587-42670`), and
`tests/capability_multihop.rs` now covers hubs:

- `a_late_attacher_discovers_and_reaches_a_provider_through_its_hub`
- `devices_attached_to_a_hub_by_routed_handshakes_reach_each_other`

Two cases are still unwitnessed:

1. The hub is the real `net-aggregator-daemon` binary, not a bare `MeshNode`.
2. Both clients are attached by routed handshake **before** the provider
   announces, so discovery comes from the live flood
   (`forward_capability_announcement`, `mesh.rs:42470`) rather than from replay.
   Every routed-hub test today announces before the second client attaches.

### G5. The daemon does not serve `fold.query` (the original item 3)

`cli/tests/aggregator_remote.rs:335` is `#[ignore]`d. Its note blames replica
node-id discovery, but the real cause is simpler: the daemon binary installs
only the registry service (`aggregator-daemon/src/lib.rs:495`). Nothing in
`aggregator-daemon/` calls `install_query_service` (`query_service.rs:234`). The
only production caller is the Rust SDK helper (`sdk/src/aggregator.rs:148-149`).
So `net aggregator query` against a real daemon cannot succeed.
`AGGREGATOR_LIFECYCLE_DEFERRED_2026_05_23.md`'s "Remaining gap" section says the
query verb is closed, which is not true for the binary.

A second problem shows up once it is installed. `fold.query` is one service name
per node, while a daemon can host several groups, and the request
(`FoldQueryRequest { kind, op }`, `query_service.rs:45-52`) has no way to name
one.

### G6. Operator auth (the original item 4): mostly closed

- The daemon's `operators` list (`aggregator-daemon/src/lib.rs:128-145`) bounds
  who may drive `aggregator.registry`. Empty, the default, refuses every remote
  request.
- The CLI attaches with the operator's persistent identity (`cli/src/context.rs:193-203`).
- `registry_service.rs:359-377` refuses unknown callers before decoding.

What is left: the PSK is still the mesh-membership secret, and changing
operators means editing config and restarting. That becomes a real cost in
Phase 4, where every daemon must also list every peer daemon.

### G7. Capacity (the original item 5)

`SCALING_SUBNET_SPEC.md:522` caps an aggregator at "~10K detailed entries". That
figure predates the fold-scale work. The v0.42 measurements
(`net/crates/net/docs/releases/RELEASE_v0.42_BACK_IN_BLACK.md`, harness
`benches/fold_scale_report.rs`, i9-14900K, Windows 11):

| measure | v0.40 | v0.42 |
|---|---|---|
| retained per entry, 1M | 4,184 B (4.2 GB) | 1,875 B (~1.9 GB) |
| expiry sweep, 1M, nothing due | 8.7 ms, 1M visited | 0.00 ms, 0 visited |
| expiry sweep, 1M, whole fleet expired | 14.1 s | 10.5 s (held in 1,024-entry chunks, `fold/expiry.rs:50`) |
| insert / refresh / changed replace¹ | 5.99 / 1.40 / 16.4 µs | 2.86 / 1.21 / 10.4 µs |
| translate an announcement¹ | 9.79 µs | 3.7 µs |
| mixed workload: broad query p50 (~500k matches) | 40.5 ms | 17.7 ms |

¹ last measured with v0.41's work. The release notes' caveats apply. µs deltas
of ±20–30% are noise on this machine. The final review round (the 8,192-tag cap,
the linear budget check, the expiry-wheel and restore limits) was not
re-measured.

So the fold alone holds 1M entries in memory. Two things are still unknown or
wrong:

- **The summarizer is O(entries) under the fold's read lock on every tick.**
  `with_state` takes `state.read()` (`fold/mod.rs:824-827`), which blocks
  writers for the whole scan. At 1M entries that is probably tens of ms per
  tick. This is inferred from the broad-query row, not measured.
- **Nothing measures a whole aggregator.** That means UDP ingest, signature
  verification, translate, apply and the summary tick together.
  `fold_scale_report` measures the fold in isolation, and signature
  verification is likely the largest per-announcement cost. At 1M nodes
  re-announcing once a minute, the ingest rate is about 17K/s.

### G8. Stale planning docs

`SDK_AGGREGATOR_SUBNET_PLAN.md`'s "Current state" table still shows ✗ for the
clients that have since shipped (`sdk/src/aggregator.rs`, `sdk-ts/src/aggregator.ts`,
the PyO3/Go/C surfaces, `tests/c_sdk/aggregator.c`).

---

## The design

### Decisions

Each decision has a recommendation. Phase 4 should not start until D1–D4 are
signed off.

**D1. How a replica starts on another host.** *Recommend: daemon-to-daemon
Spawn.*

Each host runs `net-aggregator-daemon`. The coordinating daemon places a member,
then calls the existing `RegistryRequest::Spawn { template_name, group_name,
replica_count: 1 }` on the chosen host's daemon.

- The remote host runs the member as an ordinary local group.
- The trust boundary stays where it is: the remote host only runs templates its
  own operator configured, and only for callers on its own `operators` list.
- No new substrate primitive is needed.

Rejected:

- (a) **Make `AggregatorDaemon` a migratable `MeshDaemon`.** This reverses AL-1
  and pulls snapshot/restore into a daemon whose state is a cache of the fold.
  The state is rebuilt from announcements anyway, so migration buys nothing.
- (b) **A general remote-spawn substrate primitive.** It is the right
  long-term home, but it is far larger. It can absorb D1 later without changing
  the wire seen by operators.

**D2. What a replica is.** *Recommend: one replica per host. `replica_count`
counts hosts.*

G2 shows that in-process replicas only duplicate. A group of N becomes N members
on N distinct hosts. On a daemon with no peers, `replica_count > 1` is refused at
spawn with a clear error. Config compatibility is decided in slice M0 (below).
The group seed is **not** shipped to remote hosts: replica keypairs authorize
nothing (G2), each host runs its member under its own node identity, and the
seed stays a correlation handle (`SeedFingerprint`).

Rejected: keeping in-process replicas as a cheap HA mode. It only covers a
panicking task, which the existing `HealthMonitor` respawn already covers with
one replica.

**D3. How receivers handle several publishers.** *Recommend: every member
publishes; summaries gain a publisher-local epoch; receivers keep the newest per
publisher and merge with a fixed rule.*

`SummaryAnnouncement` today is `{ source_subnet, fold_kind, generation, buckets }`
(`summarizer.rs:27-40`). Each member's `generation` starts at 0, so a receiver
cannot order two members' summaries. Add:

- `epoch: u64`, the member's start time in ms, fixed for its lifetime.
- The publisher's identity, which already arrives as the authenticated channel
  sender. Expose it to the receiver rather than adding it to the payload.

A receiver keeps the latest `(epoch, generation)` per `(publisher, subnet, kind)`
and reports the most recently received one. The SDK supplies this as a small
`SummaryView` helper so each binding does not reinvent it.

Rejected: a single active publisher chosen by the coordinator. It halves the
traffic but makes the coordinator a single point of failure for publishing, not
just for re-placement.

**D4. How summaries pick their subnet (G3).** *Recommend: classify by the
aggregator's own `SubnetPolicy` over the origin's signed tags.*

The capability payload carries the origin's tags. Those tags are covered by the
origin's signature, so deriving the subnet from them locally is safe. The
hazard `forwarded_announcement_does_not_write_relay_peer_subnet` guards against
is binding the *relay's* session to a subnet, which this does not do. Counts are
kept incrementally, per `(subnet, state)`, updated on apply / replace / expire /
evict, so a summary tick reads counters instead of scanning (closes the G7 lock
hold too).

To confirm in slice S1: the fold's capability payload keeps every tag the policy
rules read. Slice 6's tag interning (`fold/tag_dictionary.rs`) and the 8,192-tag
cap must not drop one.

Rejected: filtering at summary time via `peer_subnets`. It misses forwarded
origins (G3) and keeps the O(entries) scan.

**D5. Auth between daemons.** *Recommend: node-id `operators` allowlists first,
org capability grants later.*

Phase 4 needs each daemon to accept Spawn/List/Unregister from the coordinator.
The existing allowlist does that today with one config line per peer. Org grants
(revocable, no restart; `ORG_CAPABILITY_AUTH_PLAN.md`) are slice M8, optional,
and only worth it once fleets get past a handful of daemons.

**D6. Which group a `fold.query` reaches (G5).** *Recommend: one daemon-level
handler; the request gains an optional group name.*

`FoldQueryRequest` gains `group: Option<String>`. `None` answers with every
local group's latest summaries for `kind`. Postcard is positional, so this is a
wire change, but every binding wraps the Rust client (TS via NAPI —
`sdk-ts/src/aggregator.ts:40`; Python via PyO3; Go/C via the FFI). The encoding
change is therefore Rust-only. Each binding only exposes the new optional
argument.

Rejected: one service name per group (`fold.query/<group>`). Clients would have
to know group names before they could ask anything.

### Shape after Phase 4

```
            operator CLI ──registry RPC──▶ daemon A (coordinator for group "g")
                                              │  placement: Scheduler over hosts
                                              │  announcing `aggregator-host`
                                 Spawn(tpl,"g",1)
                         ┌────────────────────┼────────────────────┐
                         ▼                    ▼                    ▼
                     daemon A             daemon B             daemon C
                    member g/0           member g/1           member g/2
                         │ publish summary/<kind> (epoch, generation)
                         └──────────────▶ receivers: newest per publisher (D3)
```

The coordinator's registry holds the logical group: members are
`(host node id, local group name)`. Each host holds an ordinary one-member local
group. Coordinator health checks use the remote daemon's `List` (already
authorized by D5). When a host stops answering for the miss budget, the
coordinator places that member elsewhere.

---

## The slices

Each slice leaves the tree green. "Proof" names the test or measurement that
would fail if the slice were wrong. Sizes are rough working days.

### Phase 0 — Correct the record (½ day)

- **P0-1. Amend the two predecessor plans and the spec.**
  - `AGGREGATOR_LIFECYCLE_DEFERRED_2026_05_23.md`, "Remaining gap": note that
    `query` is not live against the binary (G5) and point here.
  - `SDK_AGGREGATOR_SUBNET_PLAN.md`: replace the "Current state" table with
    today's (G8).
  - `SCALING_SUBNET_SPEC.md:522`: mark the 10K figure superseded, pending S3.

  *Proof:* docs only; `bun run check` in `web/` if any mirrored page changes.

### Phase 1 — Relay through a real daemon (G4, 1–2 days)

- **R1. Daemon-hub relay witnesses.** New tests in `aggregator-daemon/tests/`
  (auto-discovered, `cargo test -p net-aggregator-daemon`) that boot the daemon
  via `boot()` and attach two SDK nodes by `connect_via`, covering:
  1. the provider announces before the consumer attaches (replay);
  2. both attached, then the provider announces (flood);
  3. the provider re-announces with a changed tag, and the consumer sees the
     change (newer version supersedes in `relay_announcements`).

  *Proof:* each test asserts `find_nodes_by_filter` contains the provider within
  the deadline. Case 2 is the one that may fail.
- **R2. Fix, only if R1 case 2 fails.** Likely suspects, in order:
  - The split-horizon lookup (`mesh.rs:42516`) resolving both routed clients to
    the same address.
  - The recipient snapshot excluding a routed session.
  - The webrtc-gated forward admission at the top of
    `forward_capability_announcement` on the daemon's feature set.

  *Proof:* R1 case 2 green. A focused `tests/capability_multihop.rs` test pins
  the cause at the `MeshNode` level.

### Phase 2 — Serve `fold.query` from the daemon (G5, 2–3 days)

- **Q1. Group selector on the wire (D6).**
  - `FoldQueryRequest.group: Option<String>`; `FoldQueryError::UnknownGroup`.
  - Handler dispatch over the registry's groups instead of one daemon.
  - `FoldQueryClient::query_group` plus the bound variant.

  *Proof:* round-trip tests in `tests/aggregator_fold_query.rs` for `None` (all
  groups), a named group, and an unknown group.
- **Q2. The daemon installs it.** `boot()` installs the daemon-level `fold.query`
  handler next to the registry service and keeps the `ServeHandle` on
  `BootedDaemon`. It also calls `register_summary_channels` for each group, so
  the configured `summary_visibility` is enforced; today the binary never
  registers them.

  *Proof:* un-ignore `cli/tests/aggregator_remote.rs::query_returns_summary_for_configured_group`.
  Rewrite its ignore note's premise: the target is the daemon's node id, not a
  replica's. Add a `boot_and_query.rs` case that the summary channel's
  visibility is registered.
- **Q3. Bindings expose the optional group.** These add the argument only; no
  encoder changes.
  - Node: `bindings/node` + `sdk-ts/src/aggregator.ts`.
  - Python: `bindings/python`.
  - Go: `go/` + `bindings/go/*-ffi`.
  - C: `include/` header + `tests/c_sdk/aggregator.c`.

  The C ABI change follows the single-commit rule (Rust, headers, Go
  `abi_stability_*_test.go`).

  *Proof:* each binding's existing aggregator test gains a named-group case, and
  `aggregator.c` keeps its NET-CHECKS line.

### Phase 3 — Summaries that are right, and cheap at 1M (G3, G7, about 1 week)

- **S1. Subnet-classified incremental counts (D4).**
  - Maintain `(SubnetId, NodeState) → u64` counters on the capability fold,
    updated wherever an entry is inserted, replaced, expired or evicted.
  - The subnet comes from the aggregator node's `SubnetPolicy` applied to the
    entry's tags.
  - The capability summarizer reads the counters for its `source_subnet`.
  - Do the same for `ReservationFoldSummarizer` if its fold can be classified;
    otherwise record that it stays mesh-wide and why.

  *Proof:*
  - Unit: two subnets' worth of entries yield different counts per group. This
    test fails on today's code.
  - Property: after a random apply/replace/expire/evict sequence, the counters
    equal a full scan.
  - Fold-scale bench: the summary tick at 1M is O(1) and takes no state lock.
- **S2. Whole-aggregator bench.** `benches/aggregator_ingest.rs` (new; pin in
  `Cargo.toml` like `fold_scale_report`). It drives N distinct signed origins
  through the daemon's real dispatch path at 100K and 1M entries. It reports
  per-announcement cost split into verify / translate / apply, sustained ingest
  rate on one core, summary-tick time, and retained memory. Each value is
  labelled measured / counted / estimated / inferred, as in
  `CAPABILITY_FOLD_SCALE_PLAN.md` §Slice 0. A smaller loopback-UDP run confirms
  the in-process numbers aren't hiding socket cost.

  *Proof:* the numbers, the command and the commit recorded in this file's
  Status section.
- **S3. Replace the spec's capacity limit.** Rewrite
  `SCALING_SUBNET_SPEC.md` "What this doesn't solve → Per-aggregator capacity"
  with S2's numbers and the resulting entries-per-aggregator figure for a stated
  announce interval. Sharding goes into Phase 4 or a follow-up plan **only if**
  S2 shows one host cannot keep up at the target scale.

  *Proof:* the doc cites S2's recorded run.

### Phase 4 — Members on distinct hosts (G1, G2, G6; about 3 weeks)

- **M0. Sign-off on D1–D5, then refuse useless local replicas (D2).**
  - The daemon refuses `replica_count > 1` when it has no peer hosts.
  - Static `[[group]]` blocks with `replica_count > 1` fail boot with an error
    that names the fix.
  - Decision inside M0: whether existing configs get one release of
    warn-and-clamp first. Recommend warn-and-clamp for one release; the
    "subcommand layout is a contract" lock in `NET_CLI_PLAN.md` covers CLI
    shape, not daemon config, but someone may have deployed it.

  *Proof:* `boot_and_query.rs` cases for refuse / clamp.
- **M1. Hosts announce themselves.** Each daemon announces an `aggregator-host`
  tag, one `aggregator-template:<name>` tag per configured template, and a
  free-member capacity figure. The template tags mean the scheduler only picks
  hosts that can actually run the template.

  *Proof:* a two-daemon test in which each finds the other with
  `find_nodes_by_filter(require_tag("aggregator-host"))` and the right template
  tag.
- **M2. Placement over hosts.** The coordinator builds its candidate set from
  M1's announcements and runs `Scheduler` placement with the spread invariant
  (`GroupCoordinator::place_with_spread`, as `spawn_with_placement` already
  does). Placement failure is a typed `PlacementFailed` on the Spawn reply.

  *Proof:* unit tests on the candidate builder; a three-host test that three
  members land on three distinct hosts.
- **M3. Remote members.**
  - The coordinator's registry entry becomes a distributed group: members are
    `(host node id, local group name)`.
  - A local member starts in-process; a remote one starts by D1's Spawn against
    that host.
  - `List` reports each member's host.
  - `RegistryReplicaSummary.placement_node_id` becomes `host_node_id` and is
    always set. That is a wire change; bindings expose the field.

  *Proof:* a multi-daemon test in `aggregator-daemon/tests/` asserting each
  host's own `List` shows its one member, and the coordinator's `List` shows all
  three.
- **M4. Health and re-placement.**
  - The coordinator polls each remote member's `List` at the existing monitor
    interval (`summary_interval × 4`).
  - Once a host misses its budget, the coordinator places that member on a new
    host. When the old host comes back, the coordinator best-effort Unregisters
    the orphan.

  *Proof:* a test that stops one of three daemons and asserts the group returns
  to three members on three live hosts within the budget, with no duplicate once
  the stopped host restarts.
- **M5. Scale with placement.**
  - `add_replica_with_placement` on `LifecycleGroup` (or the distributed-group
    equivalent) for scale-up.
  - Scale-down removes the member on the most loaded host, or the highest index
    on a tie.
  - The Scale RPC and `net aggregator scale` keep their shape.

  *Proof:* grow 2→4 and shrink 4→2 across four hosts, asserting distinct hosts
  and that surviving members keep their epoch.
- **M6. Summary epoch and receiver view (D3).**
  - `SummaryAnnouncement.epoch` (a wire change).
  - `SummaryView` in `net-mesh-sdk`, exposed in the bindings per the "new
    binding surface lands in `net-mesh-sdk` first" rule.

  *Proof:*
  - A view test with two publishers, one restarting: the restarted one's
    generation 1 outranks its own old generation 40 by epoch.
  - The view reports the freshest publisher.
- **M7. Operator surfaces.**
  - `net aggregator ls` shows each member's host.
  - `spawn` gains `--min-hosts`.
  - The Deck Aggregators tab (`deck/src/tabs/aggregators.rs`) shows hosts.
  - Docs: the CLI skill/docs pages and `web/src/content/docs/` aggregator pages.

  *Proof:* `cli/tests/aggregator_remote.rs` multi-daemon cases; the docs-sync
  checks in `web/`.
- **M8. (Optional) Org capability grants for daemon-to-daemon auth (D5).**
  - `aggregator.registry` admits callers holding an org grant for an
    `aggregator.operate` capability, in addition to the `operators` list.

  *Proof:* admission tests in the org-gate families. A revoked grant is refused
  without a restart.

---

## Risks

| Risk | Fallback |
|---|---|
| R1 case 2 fails for a reason deep in routed-session forwarding | Ship Phases 2–3 regardless (they don't depend on it). Phase 4 then requires daemons to be directly connected to each other, which the coordinator can ensure with `ensure_session`. |
| The coordinator dies, so nobody re-places members | Members keep running and publishing; only re-placement stops. Document it. A takeover where any daemon holding the group spec adopts it is a follow-up, not this plan. |
| Two hosts' templates share a name but differ in content | The Spawn reply carries a template fingerprint, and the coordinator refuses a mismatch (add to M3). |
| D4's tag-derived subnet differs from the operator's mental model when policies differ across daemons | Summaries are labelled by the classifying daemon's policy. Document that a group's hosts must share a `SubnetPolicy`; M3 compares a policy fingerprint at Spawn. |
| S2 shows ingest, not the fold, is the ceiling | S3 records the real figure. Sharding gets its own plan rather than growing this one. |
| Wire changes (Q1, M3, M6) break mixed-version fleets | Postcard is positional, so old peers fail to decode rather than misread. The registry and query replies already surface codec errors as typed `codec` kinds. Release notes call out that daemons and clients upgrade together. |
| Local verification on this Windows dev box: no `#[cfg(unix)]` coverage, tight disk | Multi-daemon tests use loopback UDP only, with nothing unix-gated. Check free disk before the pre-push checklist (single target dir). |

---

## Not in scope

- Sharding one source subnet across several aggregators. This only happens if
  S2 says it is needed, and then in a separate plan.
- Federation across meshes, and strongly consistent cross-subnet operations
  (both non-goals of `SCALING_SUBNET_SPEC.md`).
- Making aggregators migratable `MeshDaemon`s (rejected in D1).
- A general remote-spawn substrate primitive (D1, rejected alternative b).
- Custom `Summarizer`s from bindings; they stay Rust-only
  (`SDK_AGGREGATOR_SUBNET_PLAN.md` non-goal).
- Coordinator takeover after the coordinating daemon is lost.
- Replacing the mesh PSK as the membership secret.

---

## Defects found while writing this plan

| ID | Defect | Where | Disposition |
|---|---|---|---|
| F1 | Daemon binary never serves `fold.query`; `net aggregator query` cannot succeed against it | `aggregator-daemon/src/lib.rs:495` | Q2 |
| F2 | The ignore note on the CLI query test names the wrong cause | `cli/tests/aggregator_remote.rs:335` | Q2 |
| F3 | Capability summary counts the whole fold, not its `source_subnet` | `aggregator/summarizer.rs:109-150` | S1 |
| F4 | In-process replicas share one `MeshNode` and publish duplicate summaries | `aggregator-daemon/src/lib.rs:680-692` | M0 |
| F5 | Daemon never registers summary channels, so `summary_visibility` is not enforced | `aggregator/daemon.rs:478` (`register_summary_channels` has no binary caller) | Q2 |
| F6 | Predecessor plan claims CLI `query` is closed | `AGGREGATOR_LIFECYCLE_DEFERRED_2026_05_23.md` "Remaining gap" | P0-1 |
