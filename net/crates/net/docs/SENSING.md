# Capability Sensing (Interest Coalescing)

Capability sensing answers one existential question a mesh could not
ask before: **"is *someone* who can do C ready for me right now?"** —
without the asker naming a provider. It sits on top of the behavior
plane's capability announcements (which say *who can* do C) and the
proximity plane (which says *how far* they are), and adds the missing
dimension: *are they ready, and will they stay ready long enough to
be worth committing to.*

The readiness signal is **advisory**. It informs a scheduler's
candidate ranking; the authoritative decision — claim, admission,
gang formation — stays with the scheduler (see
`MESH_SCHEDULER_GANG_CLAIM_PLAN.md`). A provider signs what it
evaluates about itself; each consumer judges path viability against
its own latency budget locally. Nothing here is a global truth
oracle.

Design and rationale live in `plans/SENSING_INTEREST_COALESCING_PLAN.md`
(v4.3). This document is the operator/consumer view: the model, the
config surface, and the observability surface.

## The model in one picture

There is no capability-name routing in v1 — a provider-free interest
has no `next_hop` of its own. Interests reach providers in two routed
legs:

```
provider-free interest (AnyAuthorized / Group / Tags):
    consumer ── route to the scope-local sensing LEADER R ──▶ R
    R resolves candidates, then:
    R ── route toward each selected provider P ──▶ P
    P signs readiness attestations; they fan back R ──▶ consumer

provider-targeted sensing (Node(X) / Nodes):
    consumer ── route straight to the named provider ──▶ P
    (no leader, no resolver — the explicit-surveillance path)
```

The **leader** is elected by reusing the RedEX election
(`redex::replication_election::elect`) over a shared
closeness-centrality key — the same code path as replication, never a
second election subsystem. At the leader, identical interests
coalesce into one row, resolve candidates once, open one set of
provider-targeted branches, and fan identical signed proofs to every
registered consumer. The leader is rendezvous, deduplicator, bounded
resolver, and fan-out point — nothing more.

Leader failure is cheap because interests are per-downstream soft
state: the same election yields the next-ranked healthy node,
consumers re-register their still-live interests there, and the old
leader's branches expire. Partitions are **deliberately tolerated** —
each reachable island may elect its own leader; duplicate provider
streams are bounded, expiring, and *measured* (see merge-miss below).
Do not "fix" this with consensus; blocking sensing on global leader
agreement is the failure mode.

## Coalescing surfaces

- **Local, pre-selection.** Every consumer on one node asking the
  same `(interest, capability, latency, selector, mode)` shares one
  interest before anything leaves the node.
- **Scope-wide, pre-selection.** Equivalent interests from different
  nodes meet at the elected leader and coalesce *before* provider
  selection — divergent local provider rankings no longer fragment
  demand. N consumers become one upstream registration and one signed
  provider stream, fanned back N ways. Signing — the expensive part —
  is paid once, not per watcher.
- **Residual divergence.** Distinct islands during partitions, and
  the window while an election result propagates, can leave two
  leaders each sensing the same provider. Bounded, expiring, and
  surfaced as the merge-miss metric.

## Config surface

The plane ships **dark**: `enable_sensing_coalescing` defaults to
`false`, and a node with it off does zero sensing work — inbound
frames drop like an unknown subprotocol, local registration is
refused, and the (empty) table is skipped by the heartbeat sweep.

| Knob | Default | Meaning |
|------|---------|---------|
| `enable_sensing_coalescing` | `false` | master switch; off = fully inert |
| `sensing_interest_ttl` | 30 s | soft-state lifetime; rows refresh at ttl/2, drop after 2 misses. Also the ceiling on what an inbound registration may request — a peer cannot pin rows past this |
| `max_interests_per_peer` | 512 | per-downstream cap on `(interest, provider)` rows; over-cap registrations are refused, refreshes never are |
| `attestation_cadence_floor` | 50 ms | sample intervals below this get a structured cadence refusal, not a stream |
| `continuity_factor` (`k`) | 3 | `continuity_window = k × max(promised_cadence, own D)` (plan §4.5) |
| `sensing_owner_root` | `None` (self) | the owner scope this node serves (plan §4.10). Set every fleet member to the owner's commitment so they accept each other's registrations; setting it explicitly also opts into fleet-membership admission for multi-hop coalescing |
| `sensing_incarnation` | `None` (dark) | the §4.6 epoch this node signs under. `None` is **fail-closed**: the node registers table rows but never signs/emits — a non-persisted epoch could replay `(incarnation, seq)` after a restart and be poisoned as equivocation. Derive it with `next_incarnation` over real persistence *before* constructing the node |

Being an origin (signing readiness for yourself) needs BOTH
`enable_sensing_coalescing = true` AND a persisted `sensing_incarnation`.
Being a relay/leader/consumer needs only the master switch.

## Observability

Read a snapshot through `MeshNode::sensing_counters()` (an
`Arc<SensingCounters>`; use `SensingCounters::get(&counter)` for one
value). All counters are relaxed, monotonic, and **diagnostics only** —
never load-bearing for any decision.

### Refusals by kind

| Counter | Fires when |
|---------|-----------|
| `invalid_constraints` | any constraint parse/validate rejection |
| `protocol_invalid` | the security-relevant subset: digest mismatch, or a wire scope claim the session does not back |
| `cadence_refusals` | a requested interval below the cadence floor was refused |
| `scope_refusals` | a §4.10 scope-validation refusal (any kind) |
| `broad_selector_refusals` | an `Each`-mode selector matched more providers than `each_mode_max_providers` (the §4.7 amplification guard) |

### Coalescing + delivery lifecycle

| Counter | Meaning |
|---------|---------|
| `interests_registered` | consumer registrations admitted at this node's leader role (the coalescing-ratio denominator) |
| `interests_coalesced` | the subset that JOINED an existing interest row — demand that merged at the leader. `interests_coalesced / interests_registered` is the local coalescing efficacy |
| `candidate_fanout_total` | sum of resolved active-branch counts across fresh resolutions (the fan-out the leader opened) |
| `attestations_emitted` | signed origin beats this node's emitter produced — one per branch per tick, **not** multiplied by watchers |
| `attestations_forwarded` | signed attestations relayed verbatim to downstreams, counted per forward (fan-out volume) |
| `attestations_gated` | attestations dropped at the §4.6 observer gate (stale/rewound sequence, duplicate) |
| `attestations_superseded` | attestations dropped because their `(incarnation, generation)` epoch was globally superseded (a delayed obsolete beat) |

### Coalescing efficacy — the merge-miss rate

The headline. `divergent_resolution_merge_miss / provider_free_registrations`
is the **residual-divergence rate** measured at a provider:

- `provider_free_registrations` — provider-free registrations this
  node admitted as the target provider (the denominator).
  `Node`/`Nodes` direct registrations are excluded: multiple direct
  surveillants of one provider are *intended*, not a coalescing
  failure.
- `divergent_resolution_merge_miss` — the subset admitted while the
  branch already carried another distinct upstream. Two independent
  leaders resolved the same interest to this provider — the
  split-brain / election-propagation residual §4.1 tolerates.

A materially non-zero rate justifies a future convergence refinement
(leader anti-entropy / a per-digest spread); a rate near zero shows
the split-brain tolerance is empirically cheap. This is the number
that feeds the plan's §4.1 future gate.

### Leader load

`MeshNode::sensing_leader_load()` returns a `SensingLeaderLoad`
(`interests`, `branches`, `downstream_rows`), or `None` when the role
is not installed. The leader concentrates a scope's demand — bounded
by scope size, per-downstream caps, and coalescing — so watch these
three to spot a hot leader before it is a problem. A per-digest
leader spread is a possible later refinement, not v1.

### Benchmarks — capability propagation latency (CPB)

The counters above answer *how much* diverges; the **CPB benchmark suite**
answers *how fast* a capability change reaches a remote scheduling decision —
from publication through remote visibility, never quoting serialization or
in-process overhead as end-to-end latency. Poll-free throughout (each sample
stops on an exact-state read after a `capability_fold().subscribe_changes()`
wake, not the wake alone). Under `net/crates/net/benches/`:

- `capability_propagation` — publication → remote exact-state visibility
  (warm update / add / remove / cold; direct + routed; small + GPU manifests),
  plus RT-3 registry-driven convergence (`--features "net tool"`, debounce-only
  vs default-policy — the latter is rate-limit-dominated, ~100× the debounce),
  and fan-out (A→16) batch completion.
- `capability_scheduler_reaction` (`--features "net redex"`) — publication →
  scheduler-input wake, and → a real `match_islands` decision change.
- `capability_burst` (`--features "net tool"`) — coalescing efficiency: an RT-3
  registry burst collapses to one publication; an RT-1 explicit-announce burst
  to one leading + one trailing broadcast.

See `plans/CAPABILITY_PROPAGATION_BENCHMARK_PLAN.md` §7 for the reference
baselines and the data-derived regression thresholds.

## Related

- `plans/SENSING_INTEREST_COALESCING_PLAN.md` — full design + rationale.
- `plans/CAPABILITY_PROPAGATION_BENCHMARK_PLAN.md` — the CPB latency suite (above).
- `CONTINUITY.md` — the §4.5 continuity state machine the readiness
  projection rides.
- `BEHAVIOR.md` — capability announcements (the *who can* input the
  leader's candidate resolution reads).
- `MESH_SCHEDULER_GANG_CLAIM_PLAN.md` — the authoritative consumer of
  the advisory readiness signal.
- SDK `discover.md` (Go / Python / TypeScript / Rust) — the tool
  discovery + watch-loop surface this complements.
