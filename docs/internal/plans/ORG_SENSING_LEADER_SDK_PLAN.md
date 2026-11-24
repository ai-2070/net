# LS-7: an SDK surface for leader-backed organization sensing

**Status: AUTHORIZED FOR BUILD (owner, 2026-11-24).** This is the "separate
public-API decision" that
[`ORG_SENSING_LEADER_SUBSTRATE_PLAN.md`](ORG_SENSING_LEADER_SUBSTRATE_PLAN.md)
§0 deferred as LS-7. It builds on the leader track as it stands on PR #1471
(LS-1..LS-6 plus D-LS13).

- **Branch:** `LZL0/sensing-leader-sdk`, stacked on `LZL0/sensing-leader-track`.
- **Cadence:** one commit pushed per phase (LS-7a, LS-7b, LS-7c), and one PR
  at the end, the same as the leader track.
- **The current record** is §0. The later sections are the design as
  proposed, kept so the decisions have their evidence beside them.

---

## 0. Owner decisions and build decisions

### 0.1 Owner decisions (2026-11-24)

The five questions of the proposal, all answered as recommended:

1. **Build LS-7 now.** The value is fan-in and fan-out reduction, not reach
   (§1). LS-7a also makes the lit leader arm reachable in production.
2. **Shape B:** an explicit opt-in on the existing query,
   `SensingQuery::coalesced(Coalesced)` (§4).
3. **Result modes:** `Coalesced::Best` (`ResultMode::Any`) and
   `Coalesced::Top(k)` (`ResultMode::TopK`, `1 <= k <= 32`) only.
4. **No leader:** a typed error, `SensingError::NoSensingLeader`
   (kind `no_sensing_leader`).
5. **Bindings:** all of them in LS-7c, in one change, with the shared
   cross-language error fixture.

### 0.2 Build decisions (from re-reading the tree, 2026-11-24)

These are numbered after the leader plan's D-LS1..D-LS13. Each one settles
something the proposal left open, or corrects it.

- **D-LS14 (who holds the org leader role).** Every node with sensing
  coalescing enabled holds the role: it is installed at `start()`. The
  election (D-LS15) decides where a consumer SENDS a registration, not who may
  serve one.
  - This is what election by consumers requires: a node any consumer can
    elect must already hold the role.
  - Serving a registration that arrives at a non-winner costs coalescing,
    not correctness. That is the split-brain tolerance the original design
    accepts (`SENSING_INTEREST_COALESCING_PLAN.md` §4.1).
  - The org intake is still gated by the org gate and bounded by D-LS4
    (`MAX_ORG_LEADER_INTERESTS` = 512) and the per-peer caps.
  - **Legacy intake does not light as a side effect.** The legacy
    `CapabilityRegistration` arm only checks whether the role is present, so
    it moves behind its own flag (`legacy_sensing_leader`), which only
    `MeshNode::assume_sensing_leader` sets. Legacy behavior is unchanged.
- **D-LS15 (the election).** The leader is chosen per capability, by every
  node computing the same answer from its own view.
  - **Electorate:** the owner-private announcers this node's verified
    discovery holds, under its own owner org, unexpired and floor-current,
    that a pin maps to a node id. This node is included when it announces
    owner-scoped services itself. Members that provide the watched
    capability are then REMOVED.
  - **Why per capability and minus providers:** a leader excludes itself
    from its own candidate projection (leader plan Q2, v1). A whole-org
    leader would often be a provider of the watched capability, and in a
    small org a consumer would then sense nothing through it. Electing among
    non-providers avoids that without building the deferred self-candidate.
  - **Bound:** at most `MAX_ORG_LEADER_ELECTORATE` = 32 members. If there
    are more, the electorate is the 32 with the lowest rendezvous hash
    `H(capability, node_id)`. That is deterministic for every observer and
    spreads leaders across capabilities.
  - **Rank:** `sensing::rendezvous::sensing_leader`, the existing RedEX
    election. Its key is closeness over the shared, pingwave-flooded
    proximity view (`ProximityGraph::edge_latency`, undirected), so no
    second election subsystem is introduced. Health means the member is
    reachable from this node: it has a route.
  - **A consumer never registers with itself.** If this node wins, it uses
    the next-ranked member. Other consumers still send to it, and it serves
    them under D-LS14.
  - An empty electorate means there is no leader: `NoSensingLeader`.
- **D-LS16 (coalesced demand is node-owned, beside the exact lease
  registry).** Provider-free org demand gets a small dedicated registry. It
  is NOT routed through `SensingInterestLeases`, whose machinery (local table
  rows, emitter feeding, org transition ordering, partition restoration) is
  built around exact-provider rows that a provider-free interest does not
  have at the consumer.
  - The key is the interest key. Holders are counted by token.
  - One node-owned refresh task re-elects the leader and re-registers each
    held interest at `ttl/2`. A changed leader is re-registered at once,
    which is the failover. The old leader's row expires by soft-state TTL.
  - The last release removes the consumer expectation and reconciles the
    consumer cells under it. No deregistration crosses the wire: a leader
    row expires by TTL, which is the leader's existing contract, so the
    wire stays frozen.
  - Capacity is the expectation map's existing bound
    (`max_interests_per_peer`).
- **D-LS17 (the read).** A coalesced snapshot is built in core from the
  consumer cells under the interest (`sensing_branch_projections`).
  - Rows are clamped on every read to the consumer's current owner-private
    visibility of the capability. D-LS6 guards admission; this clamp guards
    reads after a floor raise or an expiry.
  - The observer is requalified exactly as the exact watch is.
  - Ranking and viability come from the consumer's own route economics and
    the provider's signed start estimate. This uses the same classifier as
    the exact path (`project_sensed_candidates`).
- **D-LS18 (two amendments to the proposal's §5).**
  - **Ranking is the consumer's, not the leader's.** The proposal said
    "order is the leader's". A consumer receives proofs; it never learns
    the leader's internal ranking. The leader chooses WHICH providers are
    sensed (`Best`, `Top(k)`); the consumer ranks what it receives.
  - **No `LeaderRefused` kind.** A leader that refuses an intake sends
    nothing back, so the consumer has no signal to report honestly. Those
    rows stay `Unknown`, as missing evidence always does. The new kind is
    `NoSensingLeader` only. Capacity reuses `WatchesAtCapacity`.

---

## 1. The framing: the leader does not widen what a consumer can see

The intuitive pitch for a provider-free selector is "sense providers you
cannot see yourself". **The leader track does not do that, by design**, and
the SDK surface does not imply it.

- **D-LS6** (`MeshNode::org_watch_admits_signer`, `mesh.rs`) accepts a
  leader-delivered proof only when its signer is a current owner-private
  provider of the capability in the CONSUMER'S OWN scoped store: unexpired,
  floor-current, under the consumer's own owner org.
- The three-node proof pins this. In
  `a_consumer_that_cannot_see_the_provider_refuses_its_proofs`
  (`tests/sensing_org_leader_three_node.rs`), the leader senses P and P's
  proofs reach A. A cannot see P, so it refuses every proof and never
  projects.

So the set of providers a coalesced watch can ever report is the same set
the shipped exact-provider watch reports: the consumer's own verified
owner-private discovery. What the leader changes is **who opens the
streams, and how many**.

| | Exact-provider watch (shipped) | Coalesced watch (LS-7) |
|---|---|---|
| Streams opened by the consumer | One `ExactProvider` lease per provider, up to `MAX_SENSED_POPULATION` (32) | One registration, to the elected leader |
| Streams each provider serves | One per consumer watching it | One per leader, shared by every consumer of the same interest |
| Who chooses which providers are sensed | The consumer: every visible provider, lowest-id prefix of 32 | The leader: `Best` or `Top(k)` over its own projection |
| Who ranks the result | The consumer | The consumer (D-LS18) |
| Trust | Own discovery | Own discovery (D-LS6) |

---

## 2. What LS-7a adds to the core

Verified against the tree at the leader track's head.

1. **The role and the election.**
   - Before this phase, `MeshNode::assume_sensing_leader` was called only
     from tests, and nothing outside its unit tests called
     `sensing::rendezvous::sensing_leader`. Every lit registration landed on
     a node with no role and was counted `NoLeader`.
   - D-LS14 installs the role, and D-LS15 is the election.
2. **A refresh owner for provider-free org demand.**
   - Before this phase, the exact lease refresh returned
     `SensingRefreshOutcome::Absent` for `SensingLeaseKey::ProviderFree`
     ("Provider-free demand is not lit; it has no refresh owner here").
   - `register_org_capability_interest` was a one-shot send its caller
     had to repeat.
   - D-LS16 is the owner.
3. **The read.** D-LS17.

---

## 3. Constraints the build keeps

These come from `CAPABILITY_SENSING_SDK_INTEGRATION_PLAN.md` and the shipped
SDK.

- **No leader ids, audiences, digests or selectors in the public API.** "The
  SDK never accepts a caller-supplied leader for its common path" (§1.4).
  The audience is derived from installed authority, as in the exact watch.
- **Refused, not faked.** A coalesced watch never silently falls back to
  exact-provider, and vice versa. The two answer different questions (§1's
  table), and the consumer docs promise "a caller is never silently
  observing a different question".
- **A snapshot is advisory.** It is not a reservation, admission or
  authority. Unchanged.
- **Error classification is owned once in the Rust SDK**
  (`SensingError::kind`, `classify_sensing_wire`). `NoSensingLeader` gets one
  kind, and it flows to every binding through that classifier and the
  regenerated `tests/cross_lang_sensing/error_vectors.json`.
- **The guard test.**
  `the_public_surface_is_provider_lifecycle_plus_exact_consumer_observation`
  (`sdk/src/sensing.rs`) is amended in LS-7b, in the same commit, to admit
  exactly the new names. Everything it forbids today stays forbidden: no
  `ProviderSelector`, `AnyAuthorized`, `ResultMode` or `SensingLeader`
  reaches the public surface. `Coalesced` is the SDK's own two-variant
  vocabulary.

---

## 4. The API (shape B)

```rust
let mut watch = mesh.sensing()?.watch(
    SensingQuery::new("nrpc:infer")
        .start_within(Duration::from_millis(500))
        .within(Duration::from_millis(800))
        // Ask the organization's sensing leader to choose, and say how many.
        .coalesced(Coalesced::Best),         // or Coalesced::Top(3)
)?;
let snapshot = watch.snapshot()?;
assert!(snapshot.is_coalesced());
```

- **New public names:**
  - `SensingQuery::coalesced(Coalesced)`;
  - the `Coalesced { Best, Top(u16) }` enum;
  - `SensingSnapshot::is_coalesced()`;
  - `SensingError::NoSensingLeader`.
- **Unchanged:** `SensingWatch`, `SensingSnapshot`, `SensedProvider`,
  `SensedViability`, `changed()`, `close()`, and the closer.
- **Refused at `watch()`:** `Top(0)` and `Top(k > 32)`, as
  `SensingError::UnsatisfiableCoalescing`, a new typed kind. A query that
  cannot mean anything is refused before any node state is touched, like the
  existing bound refusals.
- **What it maps to:**
  - the `AnyAuthorized` selector and the owner-org audience, both fixed;
  - empty constraints, as today;
  - the D-LS16 demand.

### Semantics

- **Population:** rows are the providers whose proofs reached this consumer
  through the leader. On every read they are clamped to the consumer's own
  current owner-private visibility (D-LS17). A row the leader chose but the
  consumer cannot see is absent, never `NotReady`.
- **Ranking:** the consumer's own route economics plus the provider's signed
  start estimate, the same classifier as the exact watch (D-LS18).
- **Leader loss or move:** the watch stays open. The refresh task
  re-registers at the new leader (D-LS16). Until proofs arrive, rows are
  `Unknown`. There is no error and no fallback.
- **No leader at all:** `watch()` and `snapshot()` return
  `NoSensingLeader`. The watch stays usable, and a later snapshot succeeds
  once a leader is electable. The caller can choose to open an exact watch
  instead; the SDK never does that for it.
- **Authority:** observer qualification is identical to the exact watch.
- **Cleanup:** `close()` / `Drop` releases exactly this watch's hold. The
  last release stops refresh and drops the expectation.

Shapes A (no new API, an internal switch) and C (a separate watch type) were
rejected:
- A makes the same call answer different questions depending on mesh state
  the caller cannot see;
- C roughly doubles the consumer surface in four languages for no semantic
  gain over B's `is_coalesced()` flag.

---

## 5. Phases

| Phase | Scope | Exit witnesses (named, RED-coupled) |
|---|---|---|
| LS-7a | Core: D-LS14 (role at `start()`, legacy flag); D-LS15 (`MeshNode::org_sensing_leader_for`); D-LS16 (the coalesced demand registry, its refresh task, failover); D-LS17 (`org_coalesced_projection`) | The electorate excludes providers of the capability and is bounded; every observer of one view elects the same leader; a consumer never elects itself; legacy intake stays dark with the org role installed; a real-transport proof with NO `assume_sensing_leader` call; a leader failover re-registers at the next-ranked member within one refresh; a floor raise drops a leader-delivered row on the next read; the last release drops the expectation |
| LS-7b | Rust SDK: `SensingQuery::coalesced`, `Coalesced`, `SensingSnapshot::is_coalesced`, `NoSensingLeader`, `UnsatisfiableCoalescing`; guard test amended in the same commit; the regenerated error fixture | An SDK coalesced watch end-to-end through an elected leader; close releases only its own hold; `NoSensingLeader` with no electorate; no silent coalesced↔exact fallback; `Top(0)` / `Top(33)` refused |
| LS-7c | Node and Python bindings forward to the SDK; the TS and Python SDK option; Go through a NEW additive C function (`net_mesh_sensing_watch_coalesced`), so the existing ABI is untouched; the error vectors asserted in every language | Per-language option tests; the shared error-vector fixture; Go ABI stability for the new symbol |

The wire format does not change in any phase.

---

## 6. Build log

- **LS-7a (core).**
  - `behavior/sensing/org_coalesced.rs` (new, `redex`): the pure election
    (`bound_electorate`, `elect_org_leader`), the demand registry
    (`OrgCoalescedRegistry`, `OrgCoalescedTicket`, `OrgCoalescedRefused`),
    and `registration_due`.
  - `mesh.rs`:
    - D-LS14: `install_org_sensing_leader_role` at `start()`;
      `legacy_sensing_leader`, set only by `assume_sensing_leader` and checked
      by the legacy arm; `serves_legacy_sensing_leader_intake`.
    - D-LS15: `org_leader_members` (one owner-store scan per pass),
      `elect_org_sensing_leader`, `org_sensing_leader_for`. Proximity edges
      are read in the graph's own 32-byte id encoding
      (`node_id_to_graph_id`).
    - D-LS16: `acquire_org_coalesced_interest`,
      `release_org_coalesced_interest`, `org_coalesced_holds`,
      `org_coalesced_leader`, `org_coalesced_metrics` (`OrgCoalescedMetrics`,
      exported); the ONE refresh task (`start_org_coalesced_refresher`, tick
      `clamp(ttl/4, 50 ms, 2 s)`, joined at shutdown).
    - D-LS17: `org_coalesced_rows`, clamped by the new
      `org_sensing_current_visibility_unbounded` (the population derivation
      takes an optional cap; the exact watch keeps its 32).
  - `org_sensing_demand::project_org_coalesced`: the same classifier as the
    exact projection, over the clamped rows.
  - Witnesses: `tests/sensing_org_coalesced.rs` (new, pinned in the CI
    Sensing group), six tests over real transport with no
    `assume_sensing_leader` except the legacy positive control:
    - `a_coalesced_interest_reaches_the_elected_leader_without_assuming_the_role`
      (and the shared hold, the last release dropping the expectation and
      its cells);
    - `the_electorate_excludes_the_providers_of_the_capability`;
    - `no_electable_leader_is_a_typed_refusal_that_holds_nothing`;
    - `a_lost_leader_fails_over_to_the_next_member`;
    - `an_expired_provider_leaves_the_coalesced_rows_on_the_next_read`;
    - `legacy_intake_stays_dark_until_the_legacy_role_is_assumed`.

    Plus nine unit tests in `org_coalesced`.
  - RED, each run against its named witness and restored byte-identical:
    - M20, the legacy arm ignoring the flag;
    - M21, the electorate keeping the capability's providers;
    - M22, a consumer electing itself;
    - M23, the refresh sticking to the old leader;
    - M24, the read not clamped;
    - M25, the last release keeping the expectation;
    - M26, `start` installing no org role.

    M21 first SURVIVED: in that witness the provider was not reachable from
    the consumer, so it lost the election for health, not for exclusion.
    The witness now connects consumer and provider directly, and M21 is RED.
