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

## Provider lifecycle

A provider serves readiness for a capability by installing one
`ReadinessEvaluator`. The trait is deliberately the whole contract: one
cheap, non-blocking, synchronous `evaluate` call. It runs on the
emission path (at the aggregated cadence plus on state edges) but
always OUTSIDE every sensing lock, so an evaluator may safely call back
into `MeshNode`. Expensive state acquisition stays **outside** the
evaluator — publish into an atomic or an `ArcSwap` snapshot the
evaluator merely reads.

### Supported surface vs. internal plumbing

The **supported** provider surface is the Rust SDK's
`net_sdk::sensing`: `mesh.sensing()?.provide(..)` and the
`ReadinessRegistration` it returns. That is what applications should
use, and it is the only part of this lifecycle covered by the usual
compatibility expectations.

Everything below it is **internal plumbing**. The registry itself
(`behavior/sensing/evaluator.rs`) is crate-private; what remains `pub`
is only what a separate crate — the SDK, or the workspace's own test
suites — cannot otherwise reach. Every one of those items is
`#[doc(hidden)]` and says in words that it is not supported API.

There are two such inventories, and a `--lib` guard test walks both,
requiring the marker attribute and the exact wording on each entry.

**Production bridges** — `pub` in every build, so any dependent can
reach them. Each carries `#[doc(hidden)]` and the sentence *"Unstable,
workspace-internal SDK bridge; not supported core API."*

| Production bridge | Meaning |
|---|---|
| `MeshNode::register_readiness_evaluator` | vacancy-required install; issues an `EvaluatorRegistrationId`, or refuses with `EvaluatorInstallRefusal::Occupied` |
| `MeshNode::replace_readiness_evaluator` | explicit supersession; issues a fresh id, so the superseded id is non-current the instant it returns |
| `MeshNode::unregister_readiness_evaluator` | removes only if the supplied id is still the installed one; `true` at most once per registration |
| `MeshNode::notify_sensing_state_changed_owned` | the ownership-aware state edge: pokes only while the supplied id is still installed |
| `MeshNode::sensing_enabled` / `sensing_identity_is_durable` | the two prerequisite bits `Mesh::sensing` refuses by name |
| `EvaluatorRegistrationId` / `EvaluatorInstallRefusal` | the opaque id the SDK handle holds, and the refusal it maps |

**Fixtures-only bridges** — gated on `cfg(test)` or the `fixtures`
feature, but still `pub` whenever a *dependency* enables that feature,
which means they appear in all-features builds and in rustdoc. So they
are hidden too, and carry a distinct sentence: *"Unstable fixtures-only
test bridge; not supported core API."* The guard additionally requires
the cfg gate to still be present, so one cannot quietly become
unconditionally public.

| Fixtures-only bridge | Purpose |
|---|---|
| `MeshNode::sensing_evaluator_count` | how many capabilities have an evaluator installed — lets a witness assert a refusal was total |
| `MeshNode::sensing_evaluator_identities_exhausted` | whether the registration-identity space has reached its terminal state |
| `MeshNode::set_sensing_evaluator_next_id_for_test` | force the allocator's resting value, to reach the boundary without 2^64 registrations; deliberately bypasses monotonicity |
| `MeshNode::sensing_max_registration_id_for_test` | the largest issuable id, so a witness names the boundary without duplicating the constant |
| `MeshNode::set_sensing_commit_pause_hook_for_test` | park the emitter at the END of the publication section, to prove the section is retained across signing and publication |
| `MeshNode::set_sensing_ownership_contention_hook_for_test` | acknowledge that an ownership transition found the commit mutex HELD, so contention is proved rather than inferred from a timeout |

Neither inventory is supported API. The only supported provider surface
remains `net_sdk::sensing`.

One method in this area is **not** a bridge:
`MeshNode::notify_sensing_state_changed` is the pre-existing
capability-scoped state edge for low-level callers that own their node
outright, and it keeps its original `-> ()` signature and semantics.
`MeshNode::sensing_origin_active` likewise predates this slice and is
read by the crate's own integration suites as a plain observability
query.

Four rules follow, and they are what the ids exist for:

- **No silent theft.** A second integration cannot take a served
  capability by accident; it is refused and the incumbent keeps
  evaluating. Supersession has to be spelled out.
- **A superseded holder is inert on every edge.** Once replaced or
  closed, an old holder's removal changes nothing, its state-edge
  notification moves nothing, and a readiness result it was already
  computing can no longer be published.
- **Close/drop are idempotent.** The removal is reported once, so an
  explicit close followed by a drop is safe.
- **Identity exhaustion is terminal, not wrapping.** Ids are issued at
  most once each; the allocator saturates at a reserved sentinel rather
  than reusing a value a long-closed handle still holds. Past that
  point every install — vacancy-required and replacing alike — is
  refused with `EvaluatorInstallRefusal::IdentityExhausted`, incumbents
  keep serving, and removal keeps working.
- **No user code runs under the ownership mutex — including
  destructors.** `Drop` on an evaluator is user code just like
  `evaluate`, and it may legitimately re-enter provide/replace/close.
  The mutex is not reentrant, so a displaced or removed slot is moved
  out as a value, the section is released, and only then is the slot
  dropped. The map mutation itself stays fully serialized.

### The publication fence

`evaluate` is arbitrary user code and may take arbitrarily long, so a
close or a replacement can land while a result is still being computed.
Publishing that result afterwards would let a retired integration's
verdict become the latest observation.

The registry therefore owns a commit mutex that is the linearization
point for ownership transfer. Install, replace, and remove hold it for
their whole operation; the emitter holds it across the currentness test
**and** the publication. So:

```text
snapshot (id, evaluator)      ← no lock
run evaluate                  ← no lock; may block, may re-enter MeshNode
begin_commit(cap, id)         ┐ one critical section:
  sign                        │ the currentness decision and
  insert into latest          │ the publication cannot be split
  feed the consumer cell      ┘
release, then fan out to peers ← no lock held for network I/O
```

If the capability's installed registration is no longer the one the
evaluation ran under, the section does not open and the beat is dropped
— the emitter re-arms and the successor's own beat publishes the
truthful answer. The test is total in both directions: an "unevaluable"
beat produced with no evaluator installed is likewise dropped if an
evaluator has appeared since, because publishing "cannot answer" about a
capability that now can is a false negative.

**Lock order.** `commit_mu` is strictly outermost among the sensing
locks. It may be held while taking
`sensing_local_projection_mu` → `sensing_interest_table` →
`sensing_observations` (the frozen order), or `sensing_emitter`; nothing
acquires it while holding any of those. No user evaluator, no user
`Drop`, no `.await`, and no network I/O runs inside it — the displaced
and removed slots that carry the final `Arc<dyn ReadinessEvaluator>` are
released after the section, and the emitter's own clone is released
before it enters the section.

**Scope, stated honestly.** The fence covers the LOCAL commit points —
the wire cache and the consumer cell. Peer fan-out happens after the
section releases, because holding a registry lock across network I/O
would let a slow send block every registration on the node. A frame
already handed to the socket is not recalled, which is the same
soft-state honesty §4.3 of the SDK integration plan applies to the
lease's wire leg: the plan explicitly declines to linearize installation
ownership across the wire.

A capability with no evaluator is not silence: interests targeting this
node for it stream `ProviderUnknown { TemporarilyUnevaluable }`, which
projects `Unknown`. "Targeted but cannot answer" beats both a false
`Ready` and a global `NotReady`.

The state edge is a **wake, never a value**. A woken beat carries
whatever the evaluator reads at beat time, so publish the new state
*before* announcing the edge — an edge announced first simply re-signs
the old answer. That obligation is the caller's; nothing in the node can
enforce it.

### Rust SDK

`net_sdk::sensing` wraps the provider lifecycle and nothing else:
`mesh.sensing()?.provide(capability, evaluator)` returns a
`ReadinessRegistration` that owns its registration and releases it on
`close()` or drop. `changed()` routes through the ownership-aware seam,
so a superseded-but-still-open handle cannot move its successor's
schedule.

Every prerequisite is a typed refusal rather than a silently dark node:
`SensingError::Disabled` (plane off), `DurableIdentityRequired` (a
generated ephemeral keypair cannot sign orderable readiness across a
restart), `IncarnationRequired` (the fail-closed origin gate),
`AlreadyProviding`, and `RegistrationIdentityExhausted`. Absence of the
sensing plane at BUILD time is a compile error at the call site, not a
runtime no-op: the module rides `feature = "net"`.

**Not in the SDK.** There is no query, watch, snapshot, or readiness
projection surface here, and none of the core's exact-provider
acquisition is re-exported.

The core boundary has moved. A local-origin OWN-ORGANIZATION
exact-provider lease is implemented and dark: it plans and emits
`SensingInterestFrame::OrgProviderRegistration` from installed
authority, registers its local row under the organization-derived proven
root, and reaches an organization-authoritative peer through that peer's
ordinary registration intake.
`SensingRegistrationError::OrgAudienceUnsupported` survives with a
NARROWED meaning — the audience is an organization commitment but this
node has no live membership to speak with right now, or the captured
authority view went stale before the mutation. A FOREIGN organization's
commitment is still undetectable from the sending side (a commitment is
a one-way derivation), so it takes the legacy path unchanged. The
internal design is
`ORG_EXACT_SENSING_ACQUISITION_PROJECTION_DESIGN.md` under the
repository's `docs/internal/plans/`.

Retained demand and refresh now exist in the core, still dark. An
internal clone-shared family, BOUND to the node it was minted on,
retains exact-provider demand over the AUTHORIZED provider population of
one owner-scoped capability — the audience derived from installed
organization authority and the population derived under that same
captured view, neither ever caller-supplied; a view that moves before
the derivation is re-proved makes the retention re-derive rather than
publish facts its stamp never qualified. Each convergence is one
transaction, so two concurrent reconciliations of a capability
serialize instead of both establishing it.

One node-owned worker renews every retained installation at `ttl/2` on
an absolute deadline — one task for the whole node, never a timer per
lease. A refresh mints no holder and is identity-checked, so a retired
or re-established installation is never resurrected; a stale re-arm
cannot displace its own successor's schedule, and a holder joining a
live installation cannot postpone its renewal. An authority the node
cannot author this audience under makes a refresh refuse rather than
downgrade.

A retention takes THREE facts from one acquisition transaction: the
ticket, the installation that holder joined, and whether the wire row
was (re-)registered. None of them is re-derived afterwards by a second
read of the key. A ticket paired with a separately sampled installation
can name two different incarnations — invalidate the row, let another
holder re-establish it in the gap, and the pair describes a holder that
never existed while every later validation of it agrees with itself —
and freshness inferred from a before/after pair of reads fails the same
way, making a coalescing acquisition look establishing.

The first deadline is therefore grounded in the registry's own decided
action, not in when the arm happened. `Register`/`Reregister` put the
row on the wire here and now, so a full period is right. `Unchanged`
coalesced onto a row whose age this node does not know (the public
acquisition API arms nothing at all): unless something is already
renewing it, adopting it renews it AT ONCE and the cadence starts from
that renewal. Arming at join time would otherwise put the first renewal
after the row's own expiry whenever the row was already older than a
period.

**Schedule contract.** The refresh period is `max(ttl/2, 1 ms)`. The
floor is not a tuning choice: `sensing_interest_ttl` accepts any
positive `Duration`, and an unfloored `ttl/2` on a nanosecond horizon
makes the armed deadline elapse before the arm returns, so the worker's
due-set is continuously due and its loop never parks — on a
single-threaded executor that starves every other task on the runtime.
A horizon below 2 ms therefore cannot be renewed ahead of its own
expiry, and the floor says so instead of pretending otherwise; the
worker also yields cooperatively after every effect, so due work can
never monopolize an executor.

Retirement is per-holder and honest about shared state: a lease key is
node-global, so retiring one owner RELEASES its holder and then settles
the refresh record only if that release actually retired the
installation — a surviving holder keeps its row and its renewal.

A release the transaction refuses leaves a holder that is still live and
still this node's, so the ticket is retained on the node and retried on
the worker's cadence instead of being dropped. That retention is an
ownership LEDGER, not a budget, and two facts are what actually bound
it by the registry's own live-holder capacity:

* an entry is only ADMITTED while its ticket is a live holder, and that
  is checked under the SAME lock the append happens in;
* an entry only stops being live by invalidation, and the invalidating
  transition discharges it under that same lock.

So an in-flight admission cannot slip past a discharge: either the
discharge reached the lock first and the admission finds its ticket dead
(nothing to own — not a loss), or the admission holds the lock and the
discharge sees the appended entry. Without that, an invalidation between
a fullness sample and the append left a stale entry the discharge had
already looked for, and pending state could scale with caller
concurrency rather than with registry capacity. Admission still never
rejects: a retry that fails puts its ticket back unconditionally, and
going over the derived ceiling is counted loudly rather than paid for by
abandoning a live holder — a capacity rejection would have to be
justified against the state it was decided on, and a check-then-lock
pair cannot do that.

Ownership is also VALIDATED, not assumed. Production invalidates whole
installations (a refused tightening whose surviving-holder restoration
current authority will not author), which kills every holder of that
key. A convergence therefore re-checks each carried ticket against
ACTUAL HOLDER MEMBERSHIP and the installation that holder belongs to,
from ONE registry read, and re-acquires when they do not match what was
committed. One read matters: asking the two questions separately is two
observations at two instants, and an independent invalidation between
them makes them disagree about a ticket that was valid when it was
committed. A carry that was valid when observed and invalidated
immediately afterwards is legitimate movement — the next convergence
rejects the dead ticket and re-acquires.

**Terminal lifecycle.** Node shutdown closes and joins the refresh
schedule and drops any retained refused releases with it. It does NOT
release family-held tickets: those belong to the family's owners, and
shutdown is terminal — the lease registry is not a supported surface on
a stopped node and goes away with it. A holder observable in the
registry after shutdown is that retained state, not a live-node leak.

What is still genuinely absent: no query, watch, or snapshot surface, no
readiness projection and no ranking. There are also no public
`OrgClient` sensing controls or wiring — the retained-demand substrate
is not connected to call planning — no provider-free/leader sensing, no
`Granted` or cross-organization sensing, and no language bindings.
Acquisition is not a projection, so the SDK surface stays
provider-lifecycle only.

The plan's §4.5 node-authority refusal guards *owner-scoped* sensing,
and the provider surface exposes none: registering an evaluator names
only a local capability id, carries no audience, and confers no
authority. Whether a consumer's interest may reach this provider is
decided on the registration path by `validate_subscriber_scope` and —
for organization audiences — `verify_org_sensing_registration`, both of
which run before any table row exists. The evaluator is consulted only
after an admitted row produces a beat. The authority refusal for
exact-provider acquisition lives on that acquisition path, in the core,
not in the provider registry.

## Observability

Read a snapshot through `MeshNode::sensing_counters()` (an
`Arc<SensingCounters>`; use `SensingCounters::get(&counter)` for one
value). All counters are relaxed, monotonic, and **diagnostics only** —
never load-bearing for any decision.

### Refusals by kind

| Counter | Fires when |
|---------|-----------|
| `invalid_constraints` | any constraint parse/validate rejection |
| `protocol_invalid` | the security-relevant subset. Complete list of production increments: an unknown sensing subprotocol / stream id; a frame from a peer already inside its auth-failure throttle; a strict interest-frame or attestation decode failure; a constraint-digest or interest-digest mismatch; a wire scope claim the session does not back; a LEGACY registration declaring an organization-derived audience while this node holds organization authority; malformed organization-registration interval / soft-state-TTL bounds; an organization frame that is not an org registration variant, whose leader-leg routed origin does not match `from_node`, or whose interest selector does not name the frame's own `target`; a rendezvous / leader wire-intake authority mismatch; an attestation signature-verification failure; an out-of-bounds promised cadence on an interval-unsupported refusal; and attestation equivocation |
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
  to one leading + one trailing publication (observed as ~2 remote updates
  applied at the consumer).

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
