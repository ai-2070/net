# CODE REVIEW 2026-08-01 — Gang-claim scheduler hot path (`performance-gang-scheduler`)

> **STATUS: OPEN, not signed off.** Reviews the branch as it stands at
> `1da5327d2`. Nothing below is a correctness defect — all four optimizations do
> what `PERF_AUDIT_2026_07_31_GANG_SCHEDULER.md` claims, and I verified the two
> equivalence-sensitive rewrites against their originals by hand. §1 is the only
> finding I would call blocking, and it is a three-line move.

**Scope:** the full branch diff `master...1da5327d2` (merge base `fb0f5803e`) —
10 commits, 14 files, +2166/−72. This discharges slices §1, §3, §5, §6 and the
lookup portions of §7 of `PERF_AUDIT_2026_07_31_GANG_SCHEDULER.md`; audit §2 and
§4 remain held/blocked there and are out of scope here.

| file | what |
|---|---|
| `behavior/gang/mod.rs` | `match_island_records` split; single-snapshot sensed banding; `provider_rank` map |
| `behavior/gang/filter.rs` | `candidate_hosts_for` — clone-free step-1 host resolution |
| `behavior/gang/claim.rs` | `release_island` borrowed holder read |
| `behavior/fold/mod.rs` | `build_entry` by value; `with_state_query` |
| `behavior/fold/state.rs` | `U64Hasher` / `BuildU64Hasher` / `NodeIdSet` |
| `behavior/fold/reservation.rs` | `holder_of` |
| `behavior/fold/island.rs` | `HostedByAny` retyped to `NodeIdSet` |
| `behavior/fold/wire.rs` | allocation-free placeholder-signature check |
| `adapter/net/mesh.rs` | `release_island` holder read |
| `fold/tests.rs`, `gang/*` tests, `tests/gang_alloc_witness.rs` | witnesses |
| `benches/icb1_interleave.py` | interleaved N-arm ICB-1 runner |

**Overall.** Good work, and the evidence discipline is the best part of it. Each
slice lands with a test that measures the *mechanism* rather than wall-clock —
the fold query counter in `sensed_match_takes_exactly_one_topology_snapshot`, the
counting global allocator in `gang_alloc_witness.rs` — so none of them can be
satisfied by "make the discarded work faster" instead of not doing it. The
findings below are predominantly about *residual scope*: one optimization that
moved a cost rather than removing it, one duplicated type whose stated
justification does not survive reading it, and one test that cannot fail.

Per the review-tracking rule, the `§N` labels are for this document only — they
do not belong in code or commit messages. They are numbered independently of the
audit's own `§N`; where this document means the audit's, it says so.

---

## Blocking

### §1 — `match_islands_sensed` builds the band map before the early return, adding an allocation to the path that does not need it

`gang/mod.rs:224`:

```rust
let candidates = match_island_records(capability_fold, topology_fold, criteria, &pruned);
let hosts: HashMap<IslandId, NodeId> =
    candidates.iter().map(|r| (r.id, r.host)).collect();          // always built
let mut ordered = select_with_affinity(candidates, criteria.selection,
                                       criteria.prefer_capability.clone());
if sensed_viable_order.is_empty() || ordered.len() < 2 {
    return ordered;                                               // ...frequently unused
}
```

On `master` the equivalent map was built from the second `IslandQuery::All`
snapshot, which was taken **after** this early return. So the no-sensing path
allocated nothing. It now allocates a `HashMap` sized to the whole candidate set
and drops it untouched.

That is the path this branch's own doc comment (`gang/mod.rs:213-215`) calls
"the common path, and the one the contract above calls byte-identical to
`match_islands`", and `MeshNode::claim_first_sensed` (`mesh.rs:27242`) drives it
inside the claim retry loop — so, exactly like the costs the audit opened
against, it is paid per round rather than per job. Removing a two-snapshot read
and reintroducing a per-round allocation on the more common branch is a poor
trade even though the snapshot is the larger term.

`select_with_affinity` (`gang/filter.rs:215`) is a 1:1 projection —
`select_islands` sorts and maps, and the affinity arm partitions and
concatenates, neither of which drops a record — so `candidates.len()` is
`ordered.len()` and the guard can simply move up:

```rust
let candidates = match_island_records(capability_fold, topology_fold, criteria, &pruned);
// `select_with_affinity` is a 1:1 projection, so this is `ordered.len() < 2`.
if sensed_viable_order.is_empty() || candidates.len() < 2 {
    return select_with_affinity(candidates, criteria.selection,
                                criteria.prefer_capability.clone());
}
let hosts: HashMap<IslandId, NodeId> = candidates.iter().map(|r| (r.id, r.host)).collect();
```

**Suggested disposition:** hoist the guard as above. Worth a witness in the same
style as the rest of the branch — the existing
`sensed_match_with_empty_delta_is_identical_and_potential_is_never_pruned` case
run under `gang_alloc_witness.rs`'s counting allocator would pin that the
empty-`sensed_viable_order` path allocates no more than `match_islands` does,
which is the property the doc comment already asserts in prose.

---

## Findings

### §2 — `U64Hasher` is a verbatim copy of `U64TupleHasher`, and the comment explaining why is not true of the code

`fold/state.rs:51` and `fold/capability.rs:323` are byte-identical in body:
same `finish`, same `write_u64` with the same `FX_SEED`
(`0x51_7c_c1_b7_27_22_0a_95`), same `write` chunking fallback. Only the doc
comments differ. The stated reason for the fork (`fold/state.rs:47-49`):

> Deliberately distinct from the tuple hasher rather than reusing it: that one is
> named and shaped for `(u64, u64)` keys and is private to the capability module.

Neither half holds. Nothing in `U64TupleHasher`'s body is tuple-shaped — it mixes
`u64` writes and knows nothing about arity; the `(u64, u64)` framing lives
entirely in its *name* and in the `BuildU64TupleHasher` alias. And "private to
the capability module" is a fact about where it is declared, not a constraint:
it is `pub(crate)`, and both call sites are in the same crate.

The cost is divergence risk. There is now one algorithm in two places with no
link between them, so a future correction to either — the low-bit finalizer in
§6 being the obvious candidate — silently will not reach the other, and the
comment actively discourages the reader from looking for the second copy.

**Suggested disposition:** one `pub(crate) struct FxU64Hasher` in `fold/state.rs`
carrying the shared rationale, with `BuildU64TupleHasher` and `BuildU64Hasher`
as two `BuildHasherDefault` aliases over it and `NodeIdSet` unchanged. The
per-site rationale (which keys, why the probe count makes it worth removing
SipHash) stays on the aliases, where it is actually site-specific. If the fork is
kept deliberately, the comment needs to give the real reason, because the one
there now will not survive the next reader checking it.

### §3 — `candidate_hosts_for_applies_limit_before_host_dedup` cannot fail

`gang/filter.rs:445`. The docstring states the invariant precisely:

> §1 invariant 2: `limit` applies to KEYS, before host dedup — the same order
> `composite_query` + `candidate_hosts` used.

But the fixture cannot distinguish the two orders. `populated_fold`
(`gang/filter.rs:300`) announces `nodes[0]` and `nodes[1]` into class 1 and
`nodes[2]` into class 2 — each node into exactly **one** class. Key → host is
therefore injective across the whole fold, so "take *N* keys then dedup" and
"dedup then take *N*" yield the same count for every *N*, and the assertions
(`got.len() <= limit`, and equality of `len()` against the query path) hold under
either implementation. The test passes today for a reason unrelated to what it
claims to pin.

This matters more than a normal weak-fixture note, because the ordering is
load-bearing for the §1 rewrite: applying `limit` after dedup would let
`candidate_hosts_for` return *more* hosts than the path it replaced, from the
same fold, for a mesh where one host publishes into several capability classes —
which is the ordinary shape for a host that serves more than one model.

**Suggested disposition:** announce one node into both classes in a dedicated
fixture (or extend `populated_fold` with a fourth publisher that duplicates
`nodes[0]`'s node id into class 2, if `key_for` permits it), so that key count
exceeds host count. Then `limit == key_count - 1` discriminates: limit-before-
dedup can return fewer hosts than limit-after-dedup. Keep the count-only
assertion — *which* keys survive is genuinely unspecified, and the existing
docstring is right to say so.

### §4 — Breaking public API change, recorded nowhere

Two `pub` items in `net-mesh` 0.34.0 change signature:

- `IslandQuery::HostedByAny` (`fold/island.rs:154`) — payload
  `std::collections::HashSet<NodeId>` → `NodeIdSet`
- `candidate_hosts` (`gang/filter.rs:23`) — return type, same change

Both are reachable from outside the crate (the in-repo bench at
`benches/island_claim_match.rs:282` reaches them through the public path, and
had to be updated for exactly this reason). Any downstream caller constructing a
default-hasher `HashSet` to pass to `HostedByAny`, or binding the result of
`candidate_hosts` to an annotated `HashSet<NodeId>`, stops compiling.

For a 0.x crate this is a legitimate minor bump, and `NodeIdSet` is exported
alongside so the fix is one type annotation. The finding is that neither the
commit messages, the audit doc, nor a changelog entry mentions it — the branch
reads as internal-only, and it is not.

**Suggested disposition:** a changelog line naming both items and the one-line
migration. Two additive `pub` surfaces land in the same diff and want the same
treatment: `Fold::with_state_query` (`fold/mod.rs:675`) and `holder_of`
(`fold/reservation.rs:168`).

### §5 — `cargo fmt --check` fails in 3 places, all branch-added

```
cd net/crates/net && cargo fmt --check
```

- `gang/mod.rs:522` — the `announce_island(...)` call in `sensed_fixture`,
  which rustfmt collapses to one line
- `tests/gang_alloc_witness.rs:26` — the `gang::{...}` import, over width
- `tests/gang_alloc_witness.rs:80` — `fixture`'s return tuple

`master` is clean (0 diffs, verified on the same host), so all three are
introduced here.

**Fix:** `cargo fmt`. Formatting only, no behavior change.

---

## Nits

### §6 — `U64Hasher`'s low-bit distribution deserves the sentence its rationale is missing

`fold/state.rs:55`. For the single-write case (which is every real use — `Hash
for u64` goes straight to `write_u64`), `finish()` returns
`v.wrapping_mul(FX_SEED)` with no finalizer. Multiplication only propagates
entropy *upward*: bit *k* of the product depends only on bits `0..=k` of the
input. hashbrown derives the bucket index from the **low** bits of the hash, so
the quality of the low bits of the input is what carries the whole thing.

That is fine here, and it is what `rustc-hash` does. But the doc comment argues
the choice from "these ids are derived from already-hashed identity bytes, so
collision resistance exists at construction" — which is about collisions, not
about low-bit distribution, and does not tell a future reader what property a new
key type would have to have before it could reuse `NodeIdSet`'s hasher. One
sentence naming the low-bit requirement closes it. (Applies identically to
`U64TupleHasher`; if §2 is taken, it is one edit.)

### §7 — `U64Hasher::write` mixes nothing on an empty slice, and collides with `write_u64(0)`

`fold/state.rs:71`. `bytes.chunks(8)` yields nothing for an empty slice, so the
hash stays at its `Default` value of 0; and `write(&[0u8])` produces exactly the
state `write_u64(0)` does, since the short chunk is zero-padded. Both are
unreachable today — the fallback exists only against a future `Hash` impl
routing through `write` — and the comment says as much. Noted so a later pass
does not have to re-derive it; no change requested.

### §8 — `count_allocs` leaves counting enabled if the closure panics

`tests/gang_alloc_witness.rs:66`. `COUNTING.set(false)` is not on an unwind path,
so a panic inside `f()` leaves the thread's counter armed. Blast radius is nil
today: each `#[test]` runs on its own thread, both tests call `count_allocs`
once, and a panic fails the test regardless. If the file grows a third
measurement that shares a thread with a fallible one, a drop guard makes it
airtight.

---

## Verified correct

Recorded so a later pass does not re-derive them:

- **`holder_of` is an exact replacement for `ReservationQuery::State`.**
  `fold/reservation.rs:168` reads `state.entries.get(&resource_id)
  .and_then(|e| e.payload.state.holder())`; the query arm
  (`fold/reservation.rs:298`) is `state.entries.get(&resource_id).map(|e|
  vec![(resource_id, e.payload.state.clone())]).unwrap_or_default()`, which the
  callers then `.first().and_then(|(_, s)| s.holder())`. Same lookup, same
  absent-entry behavior, no expiry filtering on either side.
- **Query accounting is preserved**, which was the audit's second-pass condition
  on §7. `with_state_query` (`fold/mod.rs:675`) bumps `on_query` exactly like
  `query` (`fold/mod.rs:462`) and `with_state_and_index` (`fold/mod.rs:480`), and
  `release_holder_read_preserves_the_query_count_it_replaced` pins a delta of
  exactly 1 on both the holder and non-holder release paths. The unmetered
  `with_state` now carries a doc warning against being used for this.
- **`candidate_hosts_for`'s composite arm is order-equivalent to
  `composite_query`.** Both call `resolve_candidate_keys(state, index, filter)`
  with identical inputs — deterministic `BuildHasherDefault`, no interior
  randomization, same construction sequence — so the candidate set iterates in
  the same order; both then visit it with a primary-store existence check
  (`filter_map(get)` vs `filter(contains_key)`) before `take(limit)`. So
  `take(limit)` selects the same keys. The primary-store check is load-bearing
  and correctly retained, and `candidate_hosts_for_drops_index_keys_with_no_live_
  entry` is the right witness for it (eviction is the only constructible
  divergence between index and primary store).
- **The non-composite fallback keeps one metered query**, not two, because it
  calls `FoldKind::query` *inside* the already-metered `with_state_and_index`.
  `candidate_hosts_for_matches_the_query_path_on_every_shape` covers all 12
  non-composite shapes plus 6 composite ones, and its non-degeneracy assertion at
  the end is the right guard against the loop comparing empty to empty.
- **The single-snapshot banding is not just cheaper but strictly better
  defined.** `ordered ⊆ candidates` by construction, so `hosts.get(island)`
  cannot miss — the `unwrap_or(usize::MAX)` on `bands.get` at `gang/mod.rs:257`
  is now unreachable rather than load-bearing. The three §3 tests cover the two
  cases that actually differed between T1/T2 (eviction, and reinsertion under a
  new host) plus the negative control (same-host heartbeat), which is the right
  set.
- **`provider_rank`'s `or_insert` matches `position()`** on a duplicated
  provider — first occurrence wins in both.
- **`build_entry` by value is sound.** `Fold::apply` (`fold/mod.rs:350`) copies
  `node_id` out before the accept arms move `ann` (`NodeId: Copy`), and the
  `Reject` arm's `&ann` is on a mutually exclusive branch. Nothing reads `ann`
  after the move; the payload is not needed for rebroadcast.
  `apply_moves_the_payload_instead_of_cloning_it` asserts zero clones across all
  three merge outcomes *and* that the moved payload is intact — the second half
  matters, since a move that lost data would be worse than the clone.
- **The zero-check is an exact replacement for `== placeholder_signature()`.**
  `placeholder_signature()` (`fold/wire.rs:64`) is `vec![0u8; SIGNATURE_LEN]`,
  and the length check at `fold/wire.rs:324` runs first, so the vacuous-`all`
  hazard on an empty signature is unreachable.
  `verify_does_not_mistake_a_near_zero_signature_for_the_placeholder` probes both
  ends of the buffer, which is the right shape for a short-circuiting predicate.
- **The §1 allocation witness is genuinely payload-independent**, not merely
  under the 2× allowance: 161 allocations at 1 tag/host and 161 at 40 tags/host
  over 50 hosts. Holding host count fixed and scaling only payload bulk is the
  correct experimental design for the claim.
- **`icb1_interleave.py` enforces what the measurement contract asks for** —
  rotation by `arms[i % len:] + arms[:i % len]` so no arm systematically runs on
  a warmer machine, per-slice arms rather than before/after so the delta is
  attributable, `viable_returned` equality across arms as a precondition for the
  timing comparison being meaningful at all, and ranges reported beside medians
  so no single number is the claim. The upfront `os.path.exists` check on arm
  binaries is a good catch — that failure mode is otherwise a bare `WinError 2`
  from inside `subprocess`.

---

## Verification

Run on Windows 11 from `net/crates/net`, at `1da5327d2`.

| check | result |
|---|---|
| `cargo test --lib adapter::net::behavior::gang::` | **65 passed**, 0 failed |
| `cargo test --lib adapter::net::behavior::fold::` | **186 passed**, 0 failed |
| `cargo test --test gang_alloc_witness -- --nocapture` | **2 passed**, 0 failed — `§1 matcher allocations over 50 hosts: 161 (1 tag) vs 161 (40 tags)` |
| `cargo clippy --lib --all-features` | clean — no new lint classes |
| `cargo fmt --check` | **3 diffs** — see §5 |
| `cargo fmt --check` on `master` (same host) | clean — confirms §5 is branch-introduced |

**Not exercised here.** No ICB-1 run: `icb1_interleave.py` needs per-slice
release arm binaries built from separate commits, and the audit's acceptance
rule is a human comparison of deltas against ranges, not a threshold check this
pass could discharge. The branch's own measured-results claims in
`PERF_AUDIT_2026_07_31_GANG_SCHEDULER.md` are therefore taken as reported, not
reproduced. §1 above, if fixed, does not change any ICB-1 cell — that bench
drives `match_islands`, not `match_islands_sensed`.

---

## Closure

| # | disposition | commit |
|---|---|---|
| §1 | open — blocking | — |
| §2 | open | — |
| §3 | open | — |
| §4 | open | — |
| §5 | open | — |
| §6 | open — nit | — |
| §7 | open — nit, no change requested | — |
| §8 | open — nit | — |
