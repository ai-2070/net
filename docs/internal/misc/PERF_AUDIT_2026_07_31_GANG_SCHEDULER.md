# Performance Audit — Gang-Claim Scheduler Hot Path (2026-07-31)

Source: code inspection of the gang-claim scheduler
(`net/crates/net/src/adapter/net/behavior/gang/`) and the fold surfaces its hot path
reads and writes — `fold/capability.rs` (step 1), `fold/island.rs` (step 2),
`fold/reservation.rs` (step 4), plus the shared `fold/mod.rs` apply path and
`fold/wire.rs` envelope codec — the node-level claim surface in `adapter/net/mesh.rs`,
the Cortex claim pipeline in `adapter/net/cortex/workflow/step.rs`, and the existing
island-claim bench suite (`benches/island_claim_*.rs`).

**Status: slices 1–3 implemented (see "Recommended order of attack"); slice 4 held, slice 5
blocked. Nothing here is measured** — the implemented slices landed on correctness and
mechanism witnesses, not on benchmark evidence.
Every cost claim below is derived from reading the code; the one absolute latency figure
used (Ed25519 sign ≈ 50 µs) is quoted from this repo's own
`PERF_AUDIT_2026_06_10_FULL_CRATE.md`, not re-measured. Treat the ranking as a hypothesis
ordered by expected leverage, not as a result.

> **Review disposition (2026-07-31, Kyra), first pass against `performance-gang-scheduler`
> @ `1c53f800a`.** §5, §6, §1 and the lookup portions of §7 approved subject to the scope
> and test corrections now folded into each section. **§2 is held behind read *and* write
> evidence** — it is the only scaling-curve change and the only one that can regress
> heartbeat writes. **§4 is returned for architectural revision and must not be coded as
> an optimization** — it is a protocol/observability decision, and §4 below has been
> rewritten as a decision point rather than a proposal. The benchmark mapping and the
> `cd` for every command were also corrected; the original revision pointed §4 at two
> benches that cannot characterize it.
>
> **Second pass, against `15ae0befe`.** Three conditions closed in this revision: §3's
> snapshot semantics are now a **locked T1 decision** rather than a claim of mechanical
> equivalence (§3, "Snapshot decision"); §7's borrowed holder read must **preserve query
> accounting** rather than using unmetered `with_state`; and the evidence requirements are
> now a **measurement contract** with an explicit acceptance rule, not "take a
> before/after". Final disposition: §1 ready for `Composite`; §2 held behind evidence; §3
> ready under the locked snapshot contract; §4 blocked on the protocol decision; §5 ready
> as an isolated broad-fold change; §6 ready; §7 ready under query-count parity.

The hot path in question is the read pipeline plus the commit — plan §2 steps 1–4:

```text
[1] capability match  → candidate hosts   (capability fold, read)
[2] numeric filter    → tightened islands (island topology fold, read)
[3] select            → ordered island list (pure fn)
[4] ReservationFold CAS                    (the only commit)
```

Steps 1–3 (`match_islands`) run on **every retry round** of the synchronous
`schedule_single` loop (`gang/schedule.rs:136`), and once per claim attempt on the node
and Cortex paths — so constant-factor waste there is paid per round, not per job.

**Inspection hypothesis:** a substantial part of matcher cost appears to be work whose
results are immediately discarded. This is a reading of the code, not a profile — no
component measurement exists yet to say what fraction, and the measurement contract below
is what would establish it.
Step 1 deep-clones capability payloads to extract a `u64` from each; step 2 scans the
entire topology regardless of how few hosts matched; the sensed variant scans it a second
time and clones all of it. Those three are ordinary optimizations. The fourth candidate —
skipping predictably-losing claim attempts — turned out **not** to be an optimization at
all, and is written up as such.

Recommended order of attack is at the bottom.

---

## 1. (Largest discarded-work term by inspection) `CapabilityQuery::Composite` matching deep-clones every matched payload, then discards it

**Scope, stated up front:** this finding and its fix cover the **`Composite`** query shape
only. That is the shape every in-repo `MatchCriteria` uses, but `match_islands` accepts
any `CapabilityQuery`, and `InClass` / `HasAllTags` / `HasAnyTag` / `InState` / `InRegion`
(`fold/capability.rs:536-573`) all remain clone-heavy under the sketch below. This section
does not fix "the scheduler's capability matching"; it fixes the composite path and leaves
the others on today's route.

**The gap.** `match_islands` (`gang/mod.rs:117-118`) does:

```rust
let matches = capability_fold.query(criteria.capability.clone());
let mut hosts = candidate_hosts(&matches);
```

`Fold::query` → `CapabilityFold::query` → `composite_query` (`fold/capability.rs:844-868`)
materializes `Vec<CapabilityMatch>`, cloning a full `CapabilityMembership` per matched
host: the `tags` Vec of Strings, the `metadata` BTreeMap, three allow-list Vecs
(`allowed_nodes` / `allowed_subnets` / `allowed_groups`), `hardware`, `region`,
`price_quote`, `reflex_addr`.

`candidate_hosts` (`gang/filter.rs:21-23`) is then:

```rust
matches.iter().map(|((_class, node), _)| *node).collect()
```

Every cloned byte is dropped on the next line. On a 100-matching-host mesh with ~30 tags
per host, `PERF_AUDIT_2026_06_10_FULL_CRATE.md` models a comparable membership clone at
~1–3 µs and ~100 allocations *each*.

**Why it is straightforward.** The clone-free entry point already exists and is already
`pub(crate)`. `resolve_candidate_keys` (`fold/capability.rs:661`) resolves the same
indexed-axis candidate set, and its own doc comment names this exact caller shape:

> Does NOT clone any payload […] Callers that only need keys — or that post-filter against
> borrowed payloads — use this directly via `Fold::with_state_and_index`.

`Fold::with_state_and_index` (`fold/mod.rs:476`) is the borrow-only counterpart to
`query`, documented for precisely this ("Bulk filter paths that discard the payload should
prefer this"). Sketch:

```rust
let hosts: HashSet<NodeId> = capability_fold.with_state_and_index(|state, index| {
    match &criteria.capability {
        CapabilityQuery::Composite(f) => {
            let keys = resolve_candidate_keys(state, index, f);
            let it = keys.as_set().iter()
                .filter(|k| state.entries.contains_key(*k))
                .map(|(_, node)| *node);
            if f.limit > 0 { it.take(f.limit).collect() } else { it.collect() }
        }
        // Every other shape keeps today's clone-heavy path.
        other => candidate_hosts(&CapabilityFold::query(state, index, other.clone())),
    }
});
```

**Two invariants the implementation must preserve.**

- **Index-then-primary-store filtering.** The `state.entries.contains_key` guard is not
  optional. `composite_query` uses `filter_map(|k| state.entries.get(&k)…)`, so a key
  present in the index but absent from the primary store is silently dropped today.
  Collecting node ids straight off the index would resurrect it.
- **Limit before host dedup.** `filter.limit` must stay applied to *keys*, matching
  today's order (`composite_query` takes `limit` matches; `candidate_hosts` dedups after).
  Iteration order of the candidate `HashSet` is already unspecified, so *which* `limit`
  matches survive is already unspecified — but the resulting host *count* must not change.

**Required tests.** Equivalence against the existing `query` path for **every**
`CapabilityQuery` shape, not just `Composite` — including the index/primary-store
divergence case and a `limit`-bounded composite.

**Same line, second win.** `criteria.capability.clone()` (`gang/mod.rs:117`) clones the
`CapabilityFilter` — its `tags_all`, `tags_any`, `tag_groups_all` String Vecs — on every
call, only because `Fold::query` takes `K::Query` by value. The sketch borrows it instead.

---

## 2. (Held behind write-side evidence) Step 2 scans the whole topology; the island fold has no index

**The gap.** `IslandQuery::HostedByAny` (`fold/island.rs:251-256`) iterates **every entry
in the topology fold** and hashes each entry's `host` against the candidate set:

```rust
IslandQuery::HostedByAny(hosts) => state.entries.iter()
    .filter(|(_, e)| hosts.contains(&e.payload.host))
    .map(|(k, e)| (*k, e.payload.clone()))
    .collect(),
```

`IslandTopologyFold::Index = NoIndex` (`fold/island.rs:176`), so there is nothing to
narrow with. Matcher cost is proportional to **total mesh island count**, not to how many
islands the capability match actually implicated. `HostedBy` (`fold/island.rs:245-250`)
has the identical full-scan shape.

This is not a new observation — the ICB-1 bench header states it as measured architecture
evidence:

> Sparse-1000 is slower than dense-100 (both yield 100 candidate islands) because
> `HostedByAny` scans 1,000 topology entries vs 100 — NOT because the capability query
> scanned 1,000 hosts.

The bench frames this as "architecture evidence — NOT an optimization task", which was the
right call for a measurement doc. It is the optimization task here.

**The shape.** A `by_host: HashMap<NodeId, HashSet<IslandId>>` implementing
`FoldIndex<IslandTopologyFold>` makes both queries proportional to *matched* islands.

**The constraint that decides whether it is a win at all.** Islands re-announce on **every
heartbeat** — the entire reason `load` / `p50_latency_us` live in this fold rather than
the capability index (`fold/island.rs:12-19`). Each heartbeat is an
`ApplyOutcome::Replaced`, and the Replace path (`fold/mod.rs:401-441`) does
remove-then-insert on the index. A naive index churns on every heartbeat for a mapping
that is quasi-static by construction — `record.host` cannot change at all, since `merge`
pins the key to its first publisher (`fold/island.rs:212-229`).

The hook exists: `index_payload_equivalent` (`fold/mod.rs:419`, added by PERF_AUDIT §4.5
for the capability-fold analogue). The correct implementation is:

```rust
fn index_payload_equivalent(old: &IslandRecord, new: &IslandRecord) -> bool {
    old.host == new.host
}
```

Without it this finding is plausibly a **net loss** on write-heavy meshes.

**Required evidence before approval — read *and* write.** The original revision asked only
for a sparse-1000 read comparison. That is insufficient. Measure:

| axis | what to capture |
|---|---|
| read | ICB-1 sparse-1000 row, before/after |
| steady-state heartbeat | same-host `Replaced` — must not touch the index at all |
| host-relevant replacement | if such a transition is constructible under the ownership gate |
| insert | first announcement of an island |
| eviction / expiry | `evict_node` and the TTL sweep both drive `on_remove` |
| snapshot restore | `Fold::restore` repopulates the index from scratch |
| index size | population and memory at representative cardinality |

**Required test.** A direct witness that a steady-state heartbeat calls neither
`on_remove` nor `on_insert` — an assertion on the hooks, not an inference from timing.

---

## 3. `match_islands_sensed` scans the topology a second time and clones all of it

**The gap.** `match_islands_sensed` (`gang/mod.rs:190-194`) runs a full
`IslandQuery::All` — cloning **every `IslandRecord` in the mesh**, each with its
`capabilities` String Vec and `UnitSet` Vec — purely to build an `island → host` map for
the band sort:

```rust
let snapshot: HashMap<IslandId, NodeId> = topology_fold
    .query(IslandQuery::All)
    .into_iter()
    .map(|(island, record)| (island, record.host))
    .collect();
```

The `.map` immediately projects each cloned record down to a single `u64` field.

**Why the existing rationale does not require it.** The comment (`gang/mod.rs:184-189`)
defends this as deriving the band map from "ONE literal topology snapshot — a single `All`
scan, not one `Get` per island", because separate fold reads "could interleave with
concurrent updates and hand the sort a mixed-time view". The concern is right; the fix
satisfies it more directly. `match_islands` *just* queried exactly the records needed, in
one scan, one moment earlier, and `ordered` is by construction a subset of those
candidates. Threading the filtered records out (an internal `match_islands_records`
returning `Vec<IslandRecord>`, with the public `match_islands` projecting to ids) gives the
band map from data already in hand.

**This is an observable semantic change, not a mechanical equivalence.** Today the two
reads land at different times:

| | selection | sensed band assignment |
|---|---|---|
| today | snapshot **T1** | snapshot **T2** (the second `All` scan) |
| proposed | snapshot **T1** | snapshot **T1** |

Banding reads **only** `record.host` (`gang/mod.rs:190-207`), so the difference is narrower
than "any topology change". If an island is **evicted or expires** between T1 and T2,
today's banding no longer finds it and `unwrap_or(usize::MAX)` drops it into the trailing
band; the proposed path retains its T1 host band for the in-progress invocation. If the
same `IslandId` is subsequently **reinserted under another host** — constructible, because
`merge`'s first-writer pin only holds while the entry exists — today observes the new host
while the proposed path retains T1. **Ordinary same-host heartbeat replacement does not
affect band assignment**, since `host` is unchanged and is the only field banding reads.

The right answer is T1 — but it has to be chosen, not asserted away:

> **Snapshot decision.** Sensed band assignment uses the same topology snapshot that
> produced the filtered and selected records. Topology replacements or evictions after
> that snapshot affect the next matcher invocation, not the in-progress invocation. This
> intentionally replaces today's mixed-time T1-selection / T2-banding behavior.

**Required tests.**

- same-host heartbeat **replacement** between selection and banding ⇒ assert **no band
  change** (the negative control — this is the case that does *not* differ)
- island **evicted / expired** between selection and banding ⇒ T1 band retained, not the
  trailing band
- island evicted **and reinserted under another host** between selection and banding ⇒ T1
  band retained
- empty sensed inputs ⇒ plain and sensed output identical (the existing contract at
  `gang/mod.rs:164-166`)
- stable within-band ordering — the selection policy's order survives inside a band
  (`gang/mod.rs:211`), so an internal records variant must not perturb what
  `select_with_affinity` produced
- unsensed / `Unknown` hosts retained in the trailing band, never pruned (§4.9)

**Two smaller ones in the same function.**

- **`sensed_viable_order.iter().position(...)`** (`gang/mod.rs:201-204`) is a linear scan
  of the provider order **per island** — O(islands × providers). Build a
  `HashMap<NodeId, usize>` once.
- **`down_nodes.clone()`** (`gang/mod.rs:175-179`) copies the entire down-set on the common
  path where `sensed_non_viable` is empty. `Cow<'_, HashSet<NodeId>>` avoids it in exactly
  the case the function's contract calls out as "byte-identical to `match_islands`".

**Benchmark gap — this needs a new fixture.** `island_claim_sensed` (ICB-5) does measure
plain-versus-sensed matcher overhead, but its fixture is a fixed, tiny named-island set
(`ISLAND_A` / `ISLAND_R` / `ISLAND_X`). It cannot validate the removal of a second
**whole-topology** scan, because at that cardinality there is no topology to speak of. Add
a sensed matcher scaling row:

- ~1 000 total islands, ~100 matched
- explicit viable provider ordering
- non-viable **and** unsensed hosts present (the trailing-band case)
- exact before/after result ordering asserted, not just timing

---

## 4. (Not an optimization — a protocol decision) Signing before reading on predictably-losing claim attempts

The original revision proposed a reservation pre-read as a transparent local optimization.
**That framing was wrong and the proposal is withdrawn in that form.** The cost is real;
the change is not transparent. What follows is the decision that has to be made first.

### The cost is real

`single_island_claim` (`gang/claim.rs:159-169`) signs first and asks second:

```rust
let ann = reserve_announcement(keypair, node_id, generation, island, until_unix_us)?;
Ok(ClaimOutcome::from_apply(reservations.apply(ann)?))
```

`SignedAnnouncement::sign` (`fold/wire.rs:283`) is a postcard serialization plus an
Ed25519 signature — **~50 µs** per this repo's own audit. `ReservationFold::merge`
(`fold/reservation.rs:222-247`) then rejects it in a couple of comparisons, and the
rejection was determined by state readable before the signature:

| existing state | foreign `Reserved` incoming | predetermined? |
|---|---|---|
| `Active { .. }` | always `Reject` (`reservation.rs:243`) | yes |
| `Reserved { holder ≠ us, not expired }` | `Reject` (`reservation.rs:231-241`) | yes |
| `Reserved { holder ≠ us, expired }` | `Replace` — takeover | no, must attempt |
| `Free` / absent | `Replace` / `Insert` | no, must attempt |

### Why it is not transparent: there are three claim paths, not one

| # | path | walk | reach |
|---|---|---|---|
| 1 | `claim_first_available` (`gang/contention.rs:38-45`), used by `schedule_single` | shared helper | **no non-test callers** outside the gang module and its proptests |
| 2 | `GangClaimPipeline::claim` (`cortex/workflow/step.rs:250-267`) | its own loop over `single_island_claim` | Cortex workflow |
| 3 | `MeshNode::claim_island` / `claim_island_sensed` (`mesh.rs:27234-27318`) | its own loop over `reserve_island` | **the public node API** |

Only path 1 uses `claim_first_available`. So restricting the change to
`claim_first_available` + `try_acquire_gang` — the conservative reading — buys almost
nothing in production today, because `schedule_single`, `schedule_gang` and `acquire_gang`
have no non-test callers.

And path 3 does not merely sign locally. `reserve_island` →
`apply_and_broadcast_reservation` (`mesh.rs:27321+`) applies **and broadcasts**, and the
broadcast happens **regardless of the local CAS outcome** — a losing reserve is signed,
rejected locally, and gossiped to peers anyway. ICB-4 documents this deliberately:

> **Honesty note (W4):** a LOSING reserve still gossips — `C`'s rejected `Reserved{C}` on
> `A` is broadcast to `O` regardless of the local CAS outcome, so `O` ends up observing
> `A` held by `C` while `C` keeps `A` held by `H`. […] It is witnessed, not hidden.

ICB-4 also asserts, per sample, that the reservation fold's **rejected-apply delta is
exactly one** (`island_claim_fallback.rs:305-306`, and again in W2 at `:400-418`). A
pre-read on path 3 would drive that delta to zero and fail the bench — correctly. The
bench is doing its job: it is pinning behavior a pre-read would silently change.

So on the public path this is not "skip equivalent CPU work". It suppresses a signed
announcement, a rejected-apply metric, an audit transition, and network fan-out. That may
well be **desirable** — it removes a known divergence source that ICB-4 currently has to
witness and disclaim — but it is a deliberate protocol and observability change.

### Withdrawn claim

The original text said skipping was "indistinguishable from having lost the race by a
microsecond". **That is too strong.** Both `acquire_gang` and `schedule_single` are
deadline-bounded, and the Cortex and node walks are single-pass. Skipping a reservation
that is moments from expiry can convert a final-round success into backpressure; the next
round is not guaranteed to exist. Any implementation must handle the expiry boundary
explicitly rather than assuming a retry will cover it.

### The decision to make

> Decide whether predictably-losing local claim attempts should remain observable,
> metered, audited, and broadcast protocol events. If they should not, implement **one**
> shared reservation-viability classifier and adopt it deliberately across all four
> surfaces — synchronous scheduler, Cortex workflow, ordinary node claim, sensed node
> claim — rather than optimizing one loop and leaving the others divergent.

Tests any implementation must carry:

- expiry crossing between the pre-read and the apply, in both directions
- final-round deadline behavior (success-vs-backpressure at the boundary)
- generation advancement — skipped islands must not consume a generation, per
  `claim_first_available`'s documented contract (`gang/contention.rs:25-29`)
- rejected-apply metric deltas (ICB-4's assertion is the reference)
- audit-event emission for suppressed rejects
- an explicit statement of whether losing announcements are still expected on the wire,
  with ICB-4's W4 note updated or removed to match

---

## 5. `Fold::apply` deep-clones a payload it already owns

**The gap.** `Fold::apply` takes `ann: SignedAnnouncement<K::Payload>` **by value**
(`fold/mod.rs:350`), then calls `build_entry::<K>(&ann)` (`fold/mod.rs:369`,
`fold/mod.rs:411`), which does `payload: ann.payload.clone()` (`fold/mod.rs:779`).

Nothing reads `ann` after the match arms. Capturing the scalar envelope fields up front —
`node_id`, `generation`, `ttl_secs`, and anything else `build_entry` reads — and moving
`ann` into `build_entry` removes one deep clone from **every accepted apply in the crate**:
every island heartbeat (`IslandRecord`: `UnitSet` Vec + `capabilities` String Vec), every
capability announce (the ~100-allocation clone from §1), every reservation CAS.

Outside the gang module, but squarely on its hot path in both directions.

Borrow-checker note: `let action = K::merge(existing, &ann)` holds an immutable borrow of
`state` in `existing`, which the Reject arm still uses; NLL already permits the
Insert/Replace arms to mutate `state`, and moving `ann` in those arms is the same
situation.

**Blast radius is every fold** — run the full fold apply / audit / index suite, not just
the gang tests.

---

## 6. `verify()` heap-allocates 64 bytes per call to compare against zero

`SignedAnnouncement::verify` (`fold/wire.rs:326`):

```rust
if self.signature == placeholder_signature() {
```

`placeholder_signature()` (`fold/wire.rs:64-66`) is `vec![0u8; SIGNATURE_LEN]` — a fresh
heap allocation on **every verify**, on the inbound dispatch path, purely to compare
against zero. `self.signature.iter().all(|&b| b == 0)` is the same predicate with no
allocation, and it sits after the length check (`fold/wire.rs:323-325`), so the predicate
is preserved exactly.

Keep the malformed-length and all-zero rejection tests.

---

## 7. Hasher and holder-lookup cleanups

**The `NodeId` set hasher.** `candidate_hosts` returns a SipHash `HashSet<NodeId>`
(`gang/filter.rs:21-23`), and `HostedByAny` performs one `hosts.contains(&e.payload.host)`
**per topology entry** (`fold/island.rs:253`) — so the hash count scales with total
islands, not candidate hosts. The precedent and the threat-model reasoning already exist:
`U64TupleHasher` (`fold/capability.rs:314-351`, PERF_AUDIT §4.6) notes these keys are
already hashed identity bytes and SipHash's DoS resistance "adds zero protection".

Do **not** reach for `BuildU64TupleHasher` directly — it is named for `(u64, u64)` keys and
is private to the capability module. Define a dedicated single-`u64` hasher (or factor the
shared mixing step both can use) in a neutral location. This matters most if §2 does not
land; with a `by_host` index the lookup count drops to |hosts|.

**The allocating holder read.** `ReservationQuery::State` allocates a `Vec` for a single
row (`fold/reservation.rs:274-277`). Three callers just want the holder:

- `release_island` (`gang/claim.rs:203-207`) — synchronous
- `try_acquire_gang`'s rollback loop, once per grabbed island (`gang/multi.rs:104-108`)
- **`MeshNode::release_island` (`mesh.rs:27282-27292`)** — the public async path, same
  allocating query, and missed by the original revision

One borrowed holder lookup should serve all three. It is also the primitive a §4
implementation would want, if §4 is ever authorized.

**It must not be `Fold::with_state`.** That would silently change query telemetry:
`Fold::query` (`fold/mod.rs:459`) and `with_state_and_index` (`fold/mod.rs:477`) both call
`metrics.on_query()`; `with_state` (`fold/mod.rs:649-652`) does not, and its own doc says
"production query paths should go through `Self::query`". Replacing a metered
`ReservationQuery::State` call with an unmetered borrow drops the query count for
synchronous release, the `try_acquire_gang` rollback, public `MeshNode::release_island`,
and any future §4 classifier — all of which are counted today.

Add a metric-counted borrowed API instead:

```rust
pub fn with_state_query<R>(&self, f: impl FnOnce(&FoldState<K>) -> R) -> R {
    self.metrics.on_query();
    let state = self.state.read();
    f(&state)
}
```

The name is negotiable; the accounting is not. **Required test: query-count deltas
identical before and after** across all three call sites.

(Note for whoever implements it: several existing lib paths — `capability_tags_for`,
`capability_tags_for_all`, `nodes_with_capability_tag` — already read production state
through unmetered `with_state`. That is a pre-existing inconsistency, not licence to add
another; those calls were never `query` calls, whereas these three are.)

---

## What came back clean

- **`select_islands` / `select_with_affinity`** (`gang/filter.rs:144-172`) are honest pure
  functions over an already-narrowed set. The `partition` allocates two Vecs, but runs over
  filtered candidates, not the population.
- **`NumericFilter::accepts`** (`gang/filter.rs:57-90`) short-circuits in the right order —
  cheap scalar comparisons before the String `contains` scans.
- **`UnitSet::intersects`** (`fold/island.rs:93-103`) is the documented O(n+m) sorted merge,
  with the sorted+deduped invariant established at construction.
- **`policy_cmp`'s `partial_cmp(...).unwrap_or(Equal)`** (`gang/filter.rs:127-140`) looks
  like a total-order hazard, but `IslandTopologyFold::merge` rejects non-finite `load` at
  the door (`fold/island.rs:209-211`) specifically to keep it total.
- **`acquire_gang`'s `claim.islands.clone()` + sort + dedup** (`gang/multi.rs:137-139`) runs
  once per acquire, outside the retry loop, and establishes the global lock order the
  deadlock-freedom argument depends on.
- **The `Fold::apply` lock discipline** (state → index, both write; `fold/mod.rs:360-361`)
  is fixed-order and documented. The `NoIndex` write lock on the reservation fold is
  nominally wasted but is an uncontended `parking_lot` write on a zero-sized type — not
  worth special-casing against the uniform-ordering safety argument.

---

## Measurement contract

"Take a before/after" is not an acceptance gate. Every slice below is measured under the
same contract, and no slice lands without it.

**Acceptance rule.** *The targeted mechanism must improve outside run-to-run noise, and
unrelated rows must not regress outside the chosen confidence interval.* No public
threshold is claimed at this stage — ICB-7 owns thresholds, and this audit deliberately
sets none.

**Environment, pinned and recorded with the results.**

| axis | requirement |
|---|---|
| machine | one pinned host for a given slice's baseline and candidate; do not compare across machines |
| toolchain | one pinned Rust version |
| profile | the bench profile as configured, recorded explicitly |
| features | the exact `--features` set per bench (they differ — ICB-5 needs `redex`) |

**Run discipline.**

- Baseline and candidate runs **interleaved**, not batched — a batched A-then-B run
  attributes machine drift to the change.
- Repeated to a stated sample count, with a stated warm-up count, both recorded.
- Report the distribution, not a single number.

**Per-finding evidence.**

| finding | required evidence beyond wall-clock |
|---|---|
| §1 | **allocation counts** — the claim is discarded allocation, so alloc delta is the direct witness; wall-clock alone under-reports it |
| §3 | scan count (two → one) plus the large sparse sensed row from §3, and exact result-ordering equality |
| §5 | **allocation counts** across every fold kind, not just the island fold |
| §6 | **allocation counts** on the verify path |
| §7 | query-count parity (see §7) — this one is a *non*-regression witness, not a speed claim |
| §2 | four separate result sets: read scaling, heartbeat-write, lifecycle (insert / evict / expiry / restore), and index memory at representative cardinality — reported separately, never averaged together |

§2's four result sets are the whole point of holding it: a read win that hides a
heartbeat-write regression inside one aggregate number is exactly the failure mode the
hold exists to prevent.

---

## Recommended order of attack

**Do not land these as one aggregate delta** — that destroys attribution. Five
separately-measured slices, each under the measurement contract above:

| slice | contents | commit | state |
|---|---|---|---|
| 1. generic fold/wire mechanics | §5 + §6 | `a2bd969ed` | **implemented** + 2 witness tests |
| 2. ordinary matcher | §1 + §7 hasher | `97e8457d2` | **implemented** + 3 equivalence tests |
| 3. sensed matcher | §3 + §7 holder lookup | `6b377f0c8` | **implemented** + 5 tests |
| 4. topology scaling | §2 | — | **held** — needs all four §2 result sets |
| 5. claim behavior | §4 | — | **blocked** on the protocol decision |

Slices 1–3 are constant-factor and mechanical. Slice 4 is the only scaling-curve change and
the only one that can regress writes. Slice 5 must not begin until the observability
question in §4 is answered.

### Correctness state of slices 1–3 (2026-07-31)

5 366 lib tests pass; benches compile; clippy clean on every touched file. Two of the new
tests were verified to **fail against the pre-fix code**, so they witness the mechanism
rather than merely passing alongside it:

| test | pre-fix result |
|---|---|
| `apply_moves_the_payload_instead_of_cloning_it` | fails — `left: 1, right: 0` clones |
| `sensed_match_takes_exactly_one_topology_snapshot` | fails — `left: 2, right: 1` topology reads |

The §3 snapshot test asserts on the fold's own query counter, so it cannot be satisfied by
a *faster* second scan — only by not taking one.

**Performance evidence is NOT yet collected.** Slices 1–3 landed on correctness and
mechanism witnesses alone. The measurement contract above still has to be run against them
before any performance claim is made about this work; nothing here has been benchmarked,
and no row of ICB-1 or ICB-5 has been compared before/after.

**Pre-existing, unrelated:** `tests/sensing_origin_emitter.rs` does not compile under
`--features net` alone (missing `sensing_consumer_cell_interval_for_test`). Confirmed
present on this branch with all slice changes stashed.

## Reproduce

There is no measurement in this document to reproduce. The instruments that should be used
to validate it are below.

**Working directory matters.** There is no workspace `Cargo.toml` at the repository root or
under `net/`; these commands fail from either. Run them from the crate:

```bash
cd net/crates/net
cargo bench --bench island_claim_match      --features net         # ICB-1  — §1 §2 §7
cargo bench --bench island_claim_sensed     --features net,redex   # ICB-5  — §3 (needs the new scaling row)
cargo bench --bench island_claim_fallback   --features net         # ICB-4  — §4 reject-walk reference
```

Or, equivalently, from anywhere:

```bash
cargo bench --manifest-path net/crates/net/Cargo.toml --bench island_claim_match --features net
```

`-p net-mesh` is unnecessary when invoking the crate manifest directly.

**Two benches the original revision mapped to §4 and should not have:**

- `island_claim_harness` (ICB-0) is a harness self-test — its own header says "This is NOT
  a measurement bench".
- `island_claim_boundaries` (ICB-2) is "ONE uncontended claimant […] a FRESH island per
  sample" — there is no reject walk in it to characterize.

The reject-walk reference is **ICB-4** (`island_claim_fallback`), which measures a
known-held first island followed by a free fallback, and **ICB-5b** (the sensed fallback
row inside `island_claim_sensed`) for the sensed equivalent.

ICB-1's population discipline (exact matched-host / candidate-island counts asserted before
and after every timed batch) will fail loudly if §1–§3 change what the matcher returns
rather than only how fast it returns it. Take the before/after on the **sparse-1000 row**
specifically — the row the bench header identifies as dominated by the full topology scan.
