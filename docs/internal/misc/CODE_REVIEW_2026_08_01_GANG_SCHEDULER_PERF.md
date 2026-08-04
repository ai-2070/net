# CODE REVIEW 2026-08-01 — Gang-claim scheduler hot path (`performance-gang-scheduler`)

> **STATUS: ADDRESSED at `5f22babee`, not signed off.** Every finding below has a
> landed fix — see [Closure](#closure) for the commit against each. Originally
> reviewed at `1da5327d2`.
>
> Nothing in the original pass was a correctness defect — all four optimizations
> do what `PERF_AUDIT_2026_07_31_GANG_SCHEDULER.md` claims, and the two
> equivalence-sensitive rewrites check out against their originals by hand. §1
> was the only blocking finding, and it was a three-line move.
>
> **The fix pass turned up two things the review missed**, both filed below in
> place rather than as new sections:
>
> - **§3's first fix was flaky.** The strengthened fixture used node ids from
>   generated keypairs, and whether the two limit orderings diverge at a given
>   limit depends on where the candidate set iterates one particular key — so it
>   was a coin flip per run. Fixed ids make the iteration order a property of the
>   fixture. See §3.
> - **Four `clippy::undocumented_unsafe_blocks` hits**, branch-introduced, in the
>   allocation witness's `GlobalAlloc` impl. They only surface under `--tests`,
>   which the original pass did not run clippy against. Folded into §5. Writing
>   the impl-level comment surfaced the load-bearing soundness argument that was
>   stated nowhere: a counting `GlobalAlloc` is unsound if the counter can
>   allocate or re-enter, and what rules that out is the *const-initialized* TLS
>   `Cell`.
>
> Every fix that could be red-coupled was: §1, §3 and §8 each have a recorded
> failure against the pre-fix code, and §3's additionally has a recorded *pass*
> of the old test under the mutation, which is the claim that made §3 worth
> filing.

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
`match_islands`". `MeshNode::claim_island_sensed` (`mesh.rs:27242`) pays it once
per claim attempt, and that surface is itself retried on contention — so, like
the costs the audit opened against, it is charged per attempt rather than per
job. Removing a two-snapshot read and reintroducing a per-attempt allocation on
the more common branch is a poor trade even though the snapshot is the larger
term.

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

**Disposition: FIXED (`b0b61d6e1`).** Guard hoisted as above.
`sensed_match_with_no_evidence_allocates_exactly_what_the_plain_matcher_does`
pins it under the counting allocator — **161 allocations either way, against
162 pre-fix** (recorded by running the new test against the old ordering). The
assertion is equality rather than a ratio: with `sensed_non_viable` empty the
prune set is `Cow::Borrowed`, so both matchers do literally the same work
through the same calls, and any delta at all is a discarded allocation.

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

**Disposition: FIXED (`1b6a34fd3`).** Collapsed to one `pub FxU64Hasher` in
`fold/state.rs`, with `BuildU64Hasher` and `BuildU64TupleHasher` as
`BuildHasherDefault` aliases over it.
`both_index_hasher_aliases_are_the_same_mixer` pins it *behaviorally* rather
than structurally — both aliases fed the same `write_u64` sequence must produce
the same digest — so a future re-fork that changed the constant or the step
fails, while a faithful copy (which cannot drift by definition) does not. Plus a
guard that the mixer is not a pass-through. The rename is confined to this
branch, so no released name is affected; recorded in the audit's new "Public
surface changes" section anyway.

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

**Disposition: FIXED (`5a48d4789`).** The premise was verified rather than
argued: with `candidate_hosts_for` mutated to dedup first and limit after, **the
old test still passed** and the new one fails at limit 2. New
`fold_with_a_multi_class_host` fixture — host A in five classes, host B in one,
6 keys over 2 hosts — and the assertion was upgraded from count equality to
**set** equality against the untouched `query` + `candidate_hosts` oracle, which
is strictly better than the count-only assertion suggested above.

Two things the fix pass had to add that this section did not anticipate:

- **Fixed node ids.** The first attempt used `entity_id().node_id()` from
  generated keypairs. Whether the two orderings diverge at a given limit depends
  on where the candidate set iterates B's key, and the set is hashed over the
  ids themselves — so it was a coin flip per run, and the first run failed the
  `discriminated` guard. `Fold::apply` trusts the caller for identity
  (verification is the dispatch layer's job), so the announcements now carry
  chosen ids and the iteration order is a fixed property of the fixture.
- **A `discriminated` guard.** Under limit-after-dedup the host count would be
  exactly `min(limit, hosts)` at *every* limit, so at least one limit coming in
  under that is the signature of the ordering under test. Without it the fixture
  could silently degenerate back into the state this finding is about — which is
  precisely how the original got there.

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

**Disposition: FIXED (`1c2b0706e`).** There is no `CHANGELOG.md` in this
repository and no `docs/releases/` yet, so the record went into the audit doc
these commits discharge — a new "Public surface changes (read this before
cutting a release)" section in
`PERF_AUDIT_2026_07_31_GANG_SCHEDULER.md`. It carries the before/after table,
the one-line migration, the three additive surfaces, and the reason the type
changed rather than converting at the call boundary (the set is probed once per
topology entry — a boundary conversion reintroduces the per-call rebuild audit
§7 exists to avoid). If a changelog is added later this section is what it
should be seeded from.

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

**Disposition: FIXED (`5f22babee`).** Two diffs by the time this ran — the
import was already resolved by §1's commit, which added `match_islands_sensed`
to it.

**Found during the fix pass:** four `clippy::undocumented_unsafe_blocks` hits,
also branch-introduced, on the witness's `unsafe impl GlobalAlloc` and its three
forwarding blocks. The original pass ran clippy against `--lib` only, where they
do not appear; under `--tests` the suite has 14, of which these 4 are new (the
other 10, mostly `ffi_shutdown_race.rs`, are pre-existing). Fixed in the same
commit.

Writing the impl-level comment was worth more than silencing the lint: a
counting `GlobalAlloc` is unsound if the counter can allocate or re-enter the
allocator, and what rules that out here is that `ALLOCS` / `COUNTING` are
**const-initialized** TLS `Cell`s — no lazy init on first touch. The module
header mentions const-initialization as a measurement-hygiene detail; that it is
also the soundness argument was stated nowhere.

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

**Disposition: FIXED (`0529bf521`).** It was one edit, §2 having landed first.
The doc now carries a "What a key type must satisfy to use this" section stating
the requirement as low-bit distribution specifically, why (no finalizer;
multiplication propagates entropy only upward; hashbrown indexes on low bits),
which key types qualify (`NodeId`, `IslandId` — both already digests) and which
would not (counters, left-shifted composites, pointers — anything with
structural zeroes low down).

### §7 — `U64Hasher::write` mixes nothing on an empty slice, and collides with `write_u64(0)`

`fold/state.rs:71`. `bytes.chunks(8)` yields nothing for an empty slice, so the
hash stays at its `Default` value of 0; and `write(&[0u8])` produces exactly the
state `write_u64(0)` does, since the short chunk is zero-padded. Both are
unreachable today — the fallback exists only against a future `Hash` impl
routing through `write` — and the comment says as much. Noted so a later pass
does not have to re-derive it; no change requested.

**Disposition: FIXED anyway (`0529bf521`).** Both edges are now stated on
`write`'s doc comment and pinned by
`fx_u64_hasher_byte_fallback_matches_its_documented_edges`, alongside the
property that makes the fallback a fallback rather than a second algorithm:
`write(&v.to_le_bytes())` equals `write_u64(v)`. The reasoning for going past
"noted": the fallback exists precisely for the day another key type routes
through it, and that caller needs the behavior stated *and stable*, which prose
alone does not give.

### §8 — `count_allocs` leaves counting enabled if the closure panics

`tests/gang_alloc_witness.rs:66`. `COUNTING.set(false)` is not on an unwind path,
so a panic inside `f()` leaves the thread's counter armed. Blast radius is nil
today: each `#[test]` runs on its own thread, both tests call `count_allocs`
once, and a panic fails the test regardless. If the file grows a third
measurement that shares a thread with a fallible one, a drop guard makes it
airtight.

**Disposition: FIXED (`ab1677bb2`).** The file grew a third and fourth
measurement in this very pass, so the hypothetical stopped being one. Drop
guard, unconditional.
`a_panicking_measurement_does_not_leave_counting_armed` red-couples it (the
post-unwind assertion fails without the guard, verified) and additionally checks
that the *next* measurement starts from a zeroed counter — "disarmed" and
"reusable" being separate properties. The default panic hook is swapped out
around the deliberate panic so the suite does not print a backtrace note for an
expected failure.

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
  and correctly retained, and
  `candidate_hosts_for_drops_index_keys_with_no_live_entry` is the right witness
  for it (eviction is the only constructible divergence between index and
  primary store).
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

## Follow-on work landed on this branch (not a review finding)

Recorded here because it lands in the same range and touches code this review
covers, but it did **not** come out of this pass — it is the answer to the
protocol decision audit §4 was blocked on, and it is a correctness fix rather
than a review finding.

**`eb4d270b5` — a locally rejected reservation is never broadcast.**
`MeshNode::apply_and_broadcast_reservation` published every signed transition
regardless of the local CAS outcome, so a node emitted reservation state it did
not itself believe and an observer could install a claimant the claimant knew
had lost. The broadcast now sits inside the `Inserted | Replaced` arms, under
the invariant *only locally accepted reservation-fold transitions are
publishable*. Deliberately **not** the pre-read audit §4 sketched: the local CAS
stays the sole decision point, so the rejected-apply metric, the generation
consumption and the expiry semantics are all untouched, and only the wire output
changes. Full reasoning and the declined half in the audit's §4, "The decision
taken".

Two things from that work are worth carrying back into how this review's
material reads:

- **ICB-4's W4 witness was inverted.** It asserted the divergence as an
  "orthogonal ICB-3-style" property and disclaimed it. My original pass read
  ICB-4 and did not question that framing — a disclaimed defect is still a
  defect, and "witnessed, not hidden" is not the same as "correct".
- **`ReservationFold` emits no audit events.** Neither does any other production
  fold; only a test fold does. Anywhere this branch's comments or the audit say
  a transition is "audited", that is aspirational — `Fold::audit_event` defaults
  to `None`. Nothing asserts it, and the one place a fix-pass test tried to,
  it failed. Filed in audit §4 rather than fixed, because making the reservation
  fold the first audited production fold puts a per-apply `String` allocation on
  the CAS path.

---

## Verification

Run on Windows 11 from `net/crates/net`.

**Original pass (`1da5327d2`):**

| check | result |
|---|---|
| `cargo test --lib adapter::net::behavior::gang::` | **65 passed**, 0 failed |
| `cargo test --lib adapter::net::behavior::fold::` | **186 passed**, 0 failed |
| `cargo test --test gang_alloc_witness -- --nocapture` | **2 passed**, 0 failed — `§1 matcher allocations over 50 hosts: 161 (1 tag) vs 161 (40 tags)` |
| `cargo clippy --lib --all-features` | clean — no new lint classes |
| `cargo fmt --check` | **3 diffs** — see §5 |
| `cargo fmt --check` on `master` (same host) | clean — confirms §5 is branch-introduced |

**Fix pass (`5f22babee`):**

| check | result |
|---|---|
| `cargo test --lib` | **5368 passed**, 0 failed, 1 ignored — the whole crate, not just the touched modules |
| `cargo test --test gang_alloc_witness` | **4 passed**, 0 failed |
| `cargo clippy --lib --tests --benches --all-features` | **0 errors.** The ~12k `unwrap`/`expect` restriction-lint hits are pre-existing repo-wide. The one new class the original pass had missed — 4 `undocumented_unsafe_blocks` in the witness — is fixed; the remaining 10 in the suite are pre-existing |
| `cargo fmt --check` | clean |
| `cargo test --lib candidate_hosts_for_applies_limit` ×5 | ok every run — the flakiness §3's first fix introduced is gone |

**Not exercised in either pass.** No ICB-1 run: `icb1_interleave.py` needs
per-slice release arm binaries built from separate commits, and the audit's
acceptance rule is a human comparison of deltas against ranges, not a threshold
check either pass could discharge. The branch's own measured-results claims in
`PERF_AUDIT_2026_07_31_GANG_SCHEDULER.md` are therefore taken as reported, not
reproduced.

**Why the fix pass does not invalidate them.** No published ICB-1 cell moves.
That bench drives `match_islands`, which §1 did not touch — the hoisted guard is
inside `match_islands_sensed`. §2 is a rename plus one deleted duplicate type
with an identical mixer, so hashing behavior is unchanged by construction (and
`both_index_hasher_aliases_are_the_same_mixer` is the check). §3, §5, §7 and §8
are tests, formatting and comments. §4 and §6 are documentation. The one
measured number that *does* change is the §3 allocation witness, which is
reported above and in the audit.

**Still unexercised, carried forward from the branch's own notes.**
`tests/sensing_origin_emitter.rs` does not compile under `--features net` alone
(pre-existing, confirmed by the branch author with all slice changes stashed).
Neither pass ran it, and neither pass made it worse.

---

## Closure

| # | disposition | commit |
|---|---|---|
| §1 | fixed — guard hoisted, red-coupled at 162 → 161 allocs | `b0b61d6e1` |
| §2 | fixed — one `FxU64Hasher`, two aliases | `1b6a34fd3` |
| §3 | fixed — non-injective fixture, set equality, fixed ids, degeneracy guard | `5a48d4789` |
| §4 | fixed — recorded in the audit doc; no changelog exists to put it in | `1c2b0706e` |
| §5 | fixed — 2 fmt diffs, plus 4 branch-added unsafe-comment lints found in the fix pass | `5f22babee` |
| §6 | fixed — requirement restated as low-bit distribution | `0529bf521` |
| §7 | fixed, though only noted — both edges documented and pinned | `0529bf521` |
| §8 | fixed — drop guard, red-coupled | `ab1677bb2` |

Four new tests plus one rewritten:

| test | pins |
|---|---|
| `sensed_match_with_no_evidence_allocates_exactly_what_the_plain_matcher_does` | §1 — no-evidence path costs exactly `match_islands` |
| `both_index_hasher_aliases_are_the_same_mixer` | §2 — the two aliases cannot drift |
| `fx_u64_hasher_byte_fallback_matches_its_documented_edges` | §7 — the fallback's stated behavior |
| `a_panicking_measurement_does_not_leave_counting_armed` | §8 — unwind safety of the measurement harness |
| `candidate_hosts_for_applies_limit_before_host_dedup` (rewritten) | §3 — the limit ordering, now falsifiably |

Three of these were red-coupled — run against the pre-fix code and observed to
fail (§1, §8) or, for §3, run in both directions: the old test **passes** under a
limit-after-dedup mutation and the new one fails at limit 2.
