# Performance Audit — Gang-Claim Scheduler Hot Path (2026-07-31)

Source: code inspection of the gang-claim scheduler
(`net/crates/net/src/adapter/net/behavior/gang/`) and the fold surfaces its hot path
reads and writes — `fold/capability.rs` (step 1), `fold/island.rs` (step 2),
`fold/reservation.rs` (step 4), plus the shared `fold/mod.rs` apply path and
`fold/wire.rs` envelope codec — and the existing island-claim bench suite
(`benches/island_claim_*.rs`) to establish what is already characterized.

**Status: findings only. Nothing here is implemented and nothing here is measured.**
Every cost claim below is derived from reading the code; the one absolute latency figure
used (Ed25519 sign ≈ 50 µs) is quoted from this repo's own
`PERF_AUDIT_2026_06_10_FULL_CRATE.md` §"Impact: medium", not re-measured. Treat the
ranking as a hypothesis ordered by expected leverage, not as a result. The
`island_claim_match` bench (ICB-1) is the instrument for validating §1–§3; see
**Reproduce** at the bottom.

The hot path in question is the read pipeline plus the commit — plan §2 steps 1–4:

```text
[1] capability match  → candidate hosts   (capability fold, read)
[2] numeric filter    → tightened islands (island topology fold, read)
[3] select            → ordered island list (pure fn)
[4] ReservationFold CAS                    (the only commit)
```

Steps 1–3 (`match_islands`) run on **every retry round** of `schedule_single`
(`gang/schedule.rs:136`) — the loop deliberately re-matches because the world moves
between attempts — so constant-factor waste there is paid per round, not per job.

The headline conclusion: **the matcher's cost is dominated by work it throws away.**
Step 1 deep-clones capability payloads to extract a `u64` from each; step 2 scans the
entire topology regardless of how few hosts matched; the sensed variant scans it a second
time and clones all of it. Separately, step 4 pays full Ed25519 signing for claim attempts
whose rejection is already determined by state the caller could have read.

Recommended order of attack is at the bottom.

---

## 1. (Highest constant-factor leverage) Step 1 deep-clones every matched capability payload, then discards it

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
~1–3 µs and ~100 allocations *each* — so the discarded work is plausibly the largest
single term in step 1, and it is paid again on every retry round.

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
        // Non-composite shapes keep the existing path.
        other => candidate_hosts(&CapabilityFold::query(state, index, other.clone())),
    }
});
```

**Two correctness notes for whoever implements this.**

- The `state.entries.contains_key` guard is not optional. `composite_query` uses
  `filter_map(|k| state.entries.get(&k)…)`, so a key present in the index but absent from
  the primary store is silently dropped today. Collecting node ids straight off the index
  would resurrect it.
- `filter.limit` must stay applied to *keys*, before host dedup, to match today's
  semantics (`composite_query` takes `limit` matches and `candidate_hosts` dedups after).
  Iteration order of the candidate `HashSet` is already unspecified, so which `limit`
  matches survive is already unspecified — this change does not make it worse, but it
  should not silently change the *count* either.

**Same line, second win.** `criteria.capability.clone()` (`gang/mod.rs:117`) clones the
`CapabilityFilter` — its `tags_all`, `tags_any`, `tag_groups_all` String Vecs — on every
call, only because `Fold::query` takes `K::Query` by value. The sketch above borrows it
instead. This is per retry round.

---

## 2. (Only structural finding) Step 2 scans the whole topology; the island fold has no index

**The gap.** `IslandQuery::HostedByAny` (`fold/island.rs:251-256`) iterates **every entry
in the topology fold** and hashes each entry's `host` against the candidate set:

```rust
IslandQuery::HostedByAny(hosts) => state.entries.iter()
    .filter(|(_, e)| hosts.contains(&e.payload.host))
    .map(|(k, e)| (*k, e.payload.clone()))
    .collect(),
```

`IslandTopologyFold::Index = NoIndex` (`fold/island.rs:176`), so there is nothing to
narrow with. Matcher cost is therefore proportional to **total mesh island count**, not to
how many islands the capability match actually implicated.

This is not a new observation — the ICB-1 bench header states it as measured architecture
evidence:

> Sparse-1000 is slower than dense-100 (both yield 100 candidate islands) because
> `HostedByAny` scans 1,000 topology entries vs 100 — NOT because the capability query
> scanned 1,000 hosts.

The bench frames this as "architecture evidence — NOT an optimization task", which was the
right call for a measurement doc. It is the optimization task here.

**The shape.** A `by_host: HashMap<NodeId, HashSet<IslandId>>` implementing
`FoldIndex<IslandTopologyFold>` turns both `HostedBy` and `HostedByAny` into a lookup
proportional to *matched* islands. `HostedBy` (`fold/island.rs:245-250`) has the identical
full-scan shape and gets the same win.

**The constraint that makes or breaks it.** Islands re-announce on **every heartbeat** —
that is the entire point of keeping `load` / `p50_latency_us` in this fold rather than the
capability index (module doc, `fold/island.rs:12-19`). Each heartbeat is an
`ApplyOutcome::Replaced`, and the Replace path (`fold/mod.rs:401-441`) does
remove-then-insert on the index. A naive index would churn on every heartbeat for a
mapping (`island → host`) that is quasi-static by construction — `record.host` cannot even
change, since `merge` pins the key to its first publisher (`fold/island.rs:212-229`).

The hook for this already exists: `index_payload_equivalent` (`fold/mod.rs:419`, added by
PERF_AUDIT §4.5 for exactly the capability-fold analogue of this problem) skips the index
churn when the new payload is index-equivalent to the old. An island-fold implementation
comparing only `host` would make every steady-state heartbeat skip the index entirely.
Without that, this finding is plausibly a net loss on write-heavy meshes.

**Sizing.** This is the only finding that changes the scaling curve rather than a
constant. It is also the only one that touches a fold's index invariants, and should get
its own review and its own ICB-1 sparse-1000 row before and after.

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
concurrent updates and hand the sort a mixed-time view". That reasoning is sound and the
fix does not weaken it — it strengthens it. `match_islands` *just* queried exactly the
records needed, in one scan, one moment earlier; `ordered` is by construction a subset of
those candidates. Threading the filtered records out (an internal `match_islands_records`
returning `Vec<IslandRecord>`, with the public `match_islands` projecting to ids) gives the
band map from data already in hand, from a **single** snapshot instead of two taken at
different times.

Net: one topology scan per sensed match instead of two, and zero whole-topology payload
clones.

**Two smaller ones in the same function.**

- **`sensed_viable_order.iter().position(...)`** (`gang/mod.rs:201-204`) is a linear scan
  of the provider order **per island**, inside the map that builds `bands` — O(islands ×
  providers). Building a `HashMap<NodeId, usize>` from `sensed_viable_order` once makes it
  O(islands + providers). The comment above it correctly notes there are no fold queries
  inside the sort; this is the same idea applied to the band derivation itself.
- **`down_nodes.clone()`** (`gang/mod.rs:175-179`) copies the entire down-set on the common
  path where `sensed_non_viable` is empty, only to satisfy the `&HashSet` parameter. A
  `Cow<'_, HashSet<NodeId>>` avoids the allocation whenever there is no sensed prune —
  which the function's own contract calls out as the "byte-identical to `match_islands`"
  case.

---

## 4. Contended claims pay full Ed25519 signing for attempts whose rejection is already determined

**The gap.** `claim_first_available` (`gang/contention.rs:38-45`) walks the ordered island
list calling `single_island_claim` on each. `single_island_claim` (`gang/claim.rs:159-169`)
signs first, asks second:

```rust
let ann = reserve_announcement(keypair, node_id, generation, island, until_unix_us)?;
Ok(ClaimOutcome::from_apply(reservations.apply(ann)?))
```

`reserve_announcement` → `sign_state` → `SignedAnnouncement::sign` (`fold/wire.rs:283`) is
a postcard serialization plus an Ed25519 signature — **~50 µs** per this repo's own
`PERF_AUDIT_2026_06_10_FULL_CRATE.md`. `Fold::apply` then takes the write lock and
`ReservationFold::merge` rejects it in what is effectively a couple of comparisons.

A claimant walking 8 already-held islands before winning the 9th burns on the order of
400 µs of signing to learn what a read under the same lock would have told it. The whole
premise of this module is contention — `claim_first_available`'s doc is "reserve the first
one that's free", and ICB's oversubscribed fixtures are the designed case — so the losing
attempts are the common path, not the exceptional one.

`try_acquire_gang` (`gang/multi.rs:90-93`) has the same shape and pays it twice over: the
blocked attempt signs for the blocker, then signs again per already-grabbed island to roll
it back (`gang/multi.rs:104-108`), and `acquire_gang` retries the whole thing under
backoff.

**Why a pre-read is sound.** `ReservationFold::merge` (`fold/reservation.rs:222-247`)
makes foreign rejection deterministic and readable from the state alone:

| existing state | foreign `Reserved` incoming | readable today? |
|---|---|---|
| `Active { .. }` | always `Reject` (`reservation.rs:243`) | yes — `ReservationQuery::State` |
| `Reserved { holder ≠ us, not expired }` | `Reject` (`reservation.rs:231-241`) | yes — holder + `until_unix_us` are both in the state |
| `Reserved { holder ≠ us, expired }` | `Replace` — takeover | must still be attempted |
| `Free` / absent | `Replace` / `Insert` | must still be attempted |

So the filter is "skip islands whose current state is foreign-`Active`, or foreign-
`Reserved` with a live deadline" — the two rows where the CAS outcome is already fixed.
The CAS remains the sole arbiter; the pre-read only declines to do work whose result is
predetermined.

**The race is not a new one.** If an island frees between the read and the CAS we would
have made, we skip it this round and take it on the next re-match. That is
indistinguishable from having lost the race by a microsecond, which the existing design
already tolerates — `schedule_single` re-matches every round precisely because the world
moves between attempts.

**Shape.** One `with_state` pass over the candidate list (a single read lock, no per-island
`query` call — see §7) producing the skip set, then the existing walk over what survives.
Note that a fully-held list must still return `Ok(None)` rather than erroring, and the
generation counter must keep advancing exactly as documented in
`claim_first_available`'s contract (`gang/contention.rs:25-29`) — skipped islands should
simply not consume a generation.

---

## 5. `Fold::apply` deep-clones a payload it already owns

**The gap.** `Fold::apply` takes `ann: SignedAnnouncement<K::Payload>` **by value**
(`fold/mod.rs:350`), then calls `build_entry::<K>(&ann)` (`fold/mod.rs:369`,
`fold/mod.rs:411`), which does `payload: ann.payload.clone()` (`fold/mod.rs:779`).

Nothing reads `ann` after the match arms. `ann.node_id` and `ann.generation` are `Copy`;
the Reject arm's `incoming: &ann` is a different arm from the two that build an entry.
Capturing `node_id` up front and moving `ann` into `build_entry` removes one deep clone
from **every accepted apply in the crate** — every island heartbeat (`IslandRecord`:
`UnitSet` Vec + `capabilities` String Vec), every capability announce
(`CapabilityMembership`: the ~100-allocation clone from §1), every reservation CAS.

This is outside the gang module but squarely on its hot path, in both directions: the
topology fold is written on every host heartbeat and read on every match round.

Borrow-checker note: `let action = K::merge(existing, &ann)` holds an immutable borrow of
`state` in `existing`, which the Reject arm still uses; NLL already permits the
Insert/Replace arms to mutate `state`, and moving `ann` in those arms is the same
situation.

---

## 6. `verify()` heap-allocates 64 bytes per call to compare against zero

`SignedAnnouncement::verify` (`fold/wire.rs:326`):

```rust
if self.signature == placeholder_signature() {
```

`placeholder_signature()` (`fold/wire.rs:64-66`) is `vec![0u8; SIGNATURE_LEN]` — a fresh
heap allocation on **every verify**, on the inbound dispatch path, purely to compare
against zero. `self.signature.iter().all(|&b| b == 0)` is the same predicate with no
allocation.

Small in absolute terms next to the Ed25519 verify that follows it, but it is free to fix
and it is on the per-envelope inbound path, which every fold shares.

---

## 7. Two smaller ones

**`candidate_hosts` returns a SipHash `HashSet<NodeId>`** (`gang/filter.rs:21-23`), and
`HostedByAny` performs one `hosts.contains(&e.payload.host)` **per topology entry**
(`fold/island.rs:253`) — so the hash count scales with total islands, not with candidate
hosts. The repo already established both the precedent and the threat-model reasoning for
exactly this substitution: `BuildU64TupleHasher` (`fold/capability.rs:314-351`, PERF_AUDIT
§4.6) notes that these keys are already hashed identity bytes and SipHash's DoS resistance
"adds zero protection". `NodeId` is a `u64` from the same source. This matters most if §2
does *not* land; with a `by_host` index the lookup count drops to |hosts|.

**`ReservationQuery::State` allocates a `Vec` for a single row**
(`fold/reservation.rs:274-277`: `.map(|e| vec![(resource_id, …)])`). `release_island`
(`gang/claim.rs:203-207`) calls it once per release just to read the holder, and
`try_acquire_gang`'s rollback loop calls `release_island` per grabbed island
(`gang/multi.rs:104-108`). A non-allocating holder lookup via `Fold::with_state` would
serve both — and is the same primitive §4's batched pre-read wants.

---

## What came back clean

- **`select_islands` / `select_with_affinity`** (`gang/filter.rs:144-172`) are honest pure
  functions over an already-narrowed set. The `partition` in the affinity path allocates
  two Vecs, but it runs over filtered candidates, not the population, and the
  sort-within-band structure is what makes the affinity ranking well-defined. Not worth
  touching.
- **`NumericFilter::accepts`** (`gang/filter.rs:57-90`) short-circuits in the right order —
  cheap scalar comparisons (`units.len()`, `load`, `p50_latency_us`) before the String
  `contains` scans. `require_all` / `require_any` do linear `Vec<String>::contains`, but
  over one island's resident capability list; a set would be slower at these cardinalities.
- **`UnitSet::intersects`** (`fold/island.rs:93-103`) is the documented O(n+m) sorted merge,
  with the sorted+deduped invariant established at construction. Correct as written.
- **`policy_cmp`'s `partial_cmp(...).unwrap_or(Equal)`** (`gang/filter.rs:127-140`) looks
  like a total-order hazard, but `IslandTopologyFold::merge` rejects non-finite `load` at
  the door (`fold/island.rs:209-211`) specifically to keep it total. Already handled.
- **`acquire_gang`'s `claim.islands.clone()` + sort + dedup** (`gang/multi.rs:137-139`)
  happens once per acquire, outside the retry loop, and establishes the global lock order
  the deadlock-freedom argument depends on. Correctly placed.
- **The `Fold::apply` lock discipline** (state → index, both write; `fold/mod.rs:360-361`)
  is fixed-order and documented. Taking the index write lock on the reservation fold where
  `Index = NoIndex` is nominally wasted, but it is an uncontended `parking_lot` write on a
  zero-sized type — not worth special-casing against the deadlock-safety argument that the
  uniform order buys.

---

## Recommended order of attack

Nothing is implemented. Suggested sequencing, by risk rather than by size:

| # | finding | blast radius | risk |
|---|---|---|---|
| §5 | move payload into `build_entry` | every fold apply | low — local, type-checked |
| §6 | non-allocating placeholder check | every inbound verify | low — pure predicate |
| §1 | keys-only step 1 via `resolve_candidate_keys` | gang matcher | low-medium — two semantics notes above |
| §3 | single-snapshot sensed band map | sensed matcher | low-medium — needs an internal records variant |
| §7 | u64 hasher + non-allocating holder read | matcher + release | low |
| §4 | pre-read filter before signing | contended claim | medium — behavioral, wants ICB coverage |
| §2 | `by_host` island index | topology fold | medium-high — index invariants + heartbeat churn |

§5, §6, §1, §3, §7 are constant-factor and mechanical; land them together and take one
ICB-1 delta. §4 changes observable claim behavior under contention (islands are skipped
rather than attempted) and deserves its own review plus a contention-fixture row. §2 is the
only one that changes the scaling curve and the only one that can regress write-heavy
meshes if `index_payload_equivalent` is not implemented alongside it.

## Reproduce

There is no measurement in this document to reproduce. The instruments that *should* be
used to validate any of it, all pre-existing:

```
cargo bench -p net-mesh --bench island_claim_match      --features net           # ICB-1, matcher scaling — §1 §2 §3 §7
cargo bench -p net-mesh --bench island_claim_sensed     --features net,redex     # sensed path — §3
cargo bench -p net-mesh --bench island_claim_harness    --features net           # end-to-end claim — §4 §5
cargo bench -p net-mesh --bench island_claim_boundaries --features net           # contention boundaries — §4
```

ICB-1's population discipline (exact matched-host / candidate-island counts asserted
before and after every timed batch) means it will fail loudly if any of §1–§3 change what
the matcher returns rather than only how fast it returns it. That is the property to lean
on: **take a before/after on the sparse-1000 row specifically**, since that is the row the
bench header identifies as dominated by the full topology scan.
