# CODE REVIEW 2026-07-15 — Sensing Interest Coalescing (`sensing-interest-plan`)

> **Status: all 8 findings RESOLVED on this branch** (commits `a52973c9b`
> §1, `82cedcf7b` §3, `02ee2590d` §4, `cb31e59c8` §6, `83f5d17a8` §7,
> `0dc22dc88` §8, `720536c47` §2+§5). Each Rust fix ships a regression test;
> the crate passes `cargo fmt`, the strict `--lib --bins` and the
> `--all-targets` clippy gates, and the full sensing unit + integration
> suites. The Go fixes (§2, §5) are gofmt-clean and manually verified — the
> Go binding's `.go` files are not built in CI (only the Rust `net-rpc-ffi`
> shim is linted) and the CGO harness is not wired locally.

Review of branch `sensing-interest-plan` against `master`: 57 files, +28.5k/−481.
The change lands the SI (Sensing Interest Coalescing) subsystem — a new
`adapter/net/behavior/sensing/` module tree, ~5.7k lines of `mesh.rs`
integration, the SDK `tool.watch` streaming surface, Go bindings, and
cortex/scheduler-bridge glue (plan: `docs/plans/SENSING_INTEREST_COALESCING_PLAN.md`).

## Method

The core state-machine, wire, crypto, interest-table, and evaluator modules
(`incarnation.rs`, `continuity.rs`, `wire.rs`, `frames.rs`, `table.rs`,
`evaluator.rs`) were read line-by-line. The integration glue (`mesh.rs`
+5.7k), delivery/rendezvous, controller/emitter, identity crypto, and the
SDK/Go/cortex surfaces were swept with focused deep-readers, and **every
finding below was re-verified against source**.

The cryptographic core (injective, domain-separated digests; a
tamper-evident attestation transcript), the wire codec, the epoch/continuity
state machines, the interest table, and the entire `mesh.rs` integration are
clean — no lock-across-await, saturating time math on origin/peer-controlled
inputs, bounded/reclaimed maps, and exhaustive tamper/eviction/projection
tests. Findings concentrate at the **wire → leader boundary** and in the **Go
binding**.

Severity summary:

| # | Sev | Location | One-line |
|---|-----|----------|----------|
| 1 | **High** | `rendezvous.rs:530` / `table.rs:231` | Uncapped `soft_state_ttl` panics the leader — single-frame DoS |
| 2 | Medium | `bindings/go/net/tool.go:922` | `WatchTools` baseline TOCTOU → permanently stale tool entry |
| 3 | Medium | `sensing/controller.rs:180,347` | `Quorum(k)` with `k > maximum_fanout` silently unsatisfiable, no refusal |
| 4 | Medium | `sensing/rendezvous.rs:841` | Reconcile leaves a provider in both `active` and `standby`; `expand_to_standby` then duplicates it |
| 5 | Low | `bindings/go/net/tool.go:930` | Sub-millisecond `WatchOptions.Interval` truncates to 0 |
| 6 | Low | `sensing/rendezvous.rs:802` | Consumer on a torn-down branch loses proofs until its ttl/2 refresh |
| 7 | Low | `sensing/continuity.rs:229` | `update_interval` mis-anchors an Unestablished cell whose warm-start `promised_cadence > own_interval` |
| 8 | Low | `sensing/identity.rs:~568` | `ProviderSelector` derived `Eq`/`Hash` disagree with the canonical digest identity |

All paths are relative to `net/crates/net/src/adapter/net/behavior/` unless
otherwise noted (`mesh.rs`, `bindings/`, and `sdk/` are relative to
`net/crates/net/`).

---

## 1. High — Uncapped `soft_state_ttl` panics (crashes) the sensing leader

**Location:** `sensing/rendezvous.rs:530` (`register_from_frame`) →
`register_capability_interest` (`rendezvous.rs:377`) →
`delivery.rs` `register_downstream` → `table.rs:231`
(`expires_at = now + soft_state_ttl`).
**Reachable on:** the `redex`/leader build (the `CapabilityRegistration`
dispatch arm is `#[cfg(feature = "redex")]`).

### Current shape

The wire intake gate bounds the sample interval and rejects a zero ttl, but
**never bounds `soft_state_ttl` above** (`mesh.rs:12916`):

```rust
if !sensing_interval_in_bounds(*requested_sample_interval, ctx.sensing_interest_ttl)
    || soft_state_ttl.is_zero()
{
    // ... dropped
    return;
}
// ... later, at mesh.rs:12968:
let registration = match leader.register_from_frame(&frame, from_node, ...) { ... };
```

`register_from_frame` forwards the **raw** frame value
(`rendezvous.rs:530`), and neither `register_capability_interest`
(`rendezvous.rs:377`) nor `register_downstream` caps it; it reaches
`InterestTable::register`, which computes `expires_at: now + soft_state_ttl`
(`table.rs:231`). `Instant + Duration` panics on overflow
(`expect("overflow when adding duration to instant")`).

Every **local** registration path already caps it —
`let ttl = soft_state_ttl.min(self.config.sensing_interest_ttl)` at
`mesh.rs:5491` and `mesh.rs:5696` — and even the mesh Leader-row uses the
capped value at `mesh.rs:13084`. The leader-internal relay registration is
the one leg that skips the cap.

`sensing_interval_in_bounds` (`mesh.rs:2950`) protects the *interval* side
(`0 < D ≤ sensing_interest_ttl`, default 30s), so `requested_sample_interval`
cannot trigger this — only `soft_state_ttl` is unguarded.

### Failure scenario

An authenticated peer inside the owner-root boundary sends a valid
`CapabilityRegistration` (correct consumer-binding, constraints digest, and
scope) with `soft_state_ttl = Duration::from_secs(u64::MAX)`. `soft_state_ttl`
is **not** part of `interest_digest`, so it is entirely unvalidated. The
leader reaches `now + Duration::from_secs(u64::MAX)` and panics, before ever
reaching the mesh-row cap at `mesh.rs:13084`. One frame crashes the leader
node. Note this is a robustness failure independent of the v1
inside-the-owner-root trust assumption: a trusted-but-buggy peer, or a ttl
computed wrong, has the same effect.

### Fix

Cap on the leader path exactly as the local paths do. Cleanest at intake —
add the upper-bound to the gate (`mesh.rs:12916`), or clamp inside
`register_from_frame` before forwarding:

```rust
let soft_state_ttl = (*soft_state_ttl).min(ctx.sensing_interest_ttl);
```

Optionally harden `InterestTable::register`/`ObservationCell::register` to use
`now.checked_add(...)` and drop the row rather than panic, as defence in depth
(the `ObservationCell` time math is already saturating; the `Instant + ttl` in
the table is the exposed edge).

---

## 2. Medium — Go `WatchTools` baseline TOCTOU → permanently stale tool entry

**Location:** `bindings/go/net/tool.go:922` (new in this branch).

### Current shape

`WatchTools` snapshots its returned baseline with a **separate, earlier**
call than the substrate watch's own baseline:

```go
baseline, err = rpc.raw.ListTools()          // line 922  (snapshot T1)
// ...
code = C.net_rpc_watch_tools(h, intervalMs, &wh, &openErr)   // line 937 → substrate snapshot T2 > T1
```

The substrate `watch_tools` (`mesh.rs:11750`) takes its **own**
`initial_snapshot` synchronously inside `net_rpc_watch_tools` and emits deltas
only relative to that snapshot — it does **not** replay the current tool set
as `Added` events.

### Failure scenario

A tool `B` is added (or removed) in the window between T1 and T2. `B` is
present in the substrate's `initial_snapshot` (so no `Added(B)` delta is ever
emitted) but absent from the Go `baseline` (taken before). The consumer
reconstructs state as `baseline + deltas` and never learns `B` exists until
`B` next changes — a permanently stale view. Symmetric for removal (a removed
`B` appears available forever). The old polling code used **one** snapshot for
both the returned baseline and the diff basis and was internally consistent;
this is a regression.

### Fix

Have the FFI return the substrate's `initial_snapshot` as the baseline (single
snapshot for both), or drop the separate `ListTools()` and derive the baseline
from the watch itself.

---

## 3. Medium — `Quorum(k)` with `k > maximum_fanout` is silently unsatisfiable

**Location:** `sensing/controller.rs:180` (`resolve_candidates`), `:347`
(`project_aggregate`).

### Current shape

`resolve_candidates` caps the active fan-out at `maximum_fanout` (default 3):

```rust
ResultMode::TopK(k) | ResultMode::Quorum(k) => (k as usize)
    .max(policy.initial_fanout)
    .min(policy.maximum_fanout),        // controller.rs:180-182
```

`project_aggregate` then requires `k` viable branches:

```rust
let required = match result_mode {
    ResultMode::Quorum(k) => k as usize,     // controller.rs:318
    _ => 1,
};
// ...
let status = if viable.len() >= required { Ready }
             else if complete && potential < required { NotReady }
             else { Unknown };               // controller.rs:347
```

With `k > maximum_fanout`, only `maximum_fanout` branches are ever sensed, so
`viable.len() >= required` is unreachable. Unlike `Each` (which raises
`ResolutionRefusal::SelectorTooBroad`), `Quorum` produces **no error**.

### Failure scenario

A consumer registers `ResultMode::Quorum(5)` with the default
`CandidatePolicy { maximum_fanout: 3 }` against 8 authorized, reachable, Ready
providers. Only 3 branches are activated; `viable.len() ≤ 3 < required(5)`, so
the interest reports `Unknown` forever (unbounded selector, where
`search_complete` is never `population_is_boundable`) or `NotReady` forever
(bounded selector) — despite an ample ready population. No refusal is
surfaced, so the misconfiguration is invisible.

### Fix

Either refuse `Quorum(k)` when `k > maximum_fanout` (mirroring `Each`'s
`SelectorTooBroad`), or raise the active bound to at least `k` for `Quorum`
so enough branches are sensed to evaluate the quorum.

---

## 4. Medium — Reconcile leaves a provider in both `active` and `standby`; `expand_to_standby` then duplicates it

**Location:** `sensing/rendezvous.rs:841` (`reconcile_with_snapshot`),
`:557-558` (`expand_to_standby`).

### Current shape

Reconcile assigns the fresh standby set without excluding retained incumbents:

```rust
if let Some(entry) = self.interests.get_mut(&key) {
    entry.active = kept;                 // rendezvous.rs:840
    entry.standby = resolved.standby;    // rendezvous.rs:841  (may overlap `kept`)
}
```

`kept` is built from `old_active` (retained incumbents), while
`resolved.standby` comes from a fresh `resolve_candidates`. The two are never
intersected. `expand_to_standby` promotes with no dedup against `active`:

```rust
let promoted = entry.standby.remove(0);
entry.active.push(promoted);             // rendezvous.rs:557-558
```

### Failure scenario

Interest with `active=[A]`, `standby=[B]`, fanout 1. The shared proximity view
shifts so `B` out-ranks `A`. Fresh resolve gives `active=[B]`, `standby=[A]`.
`A` is still eligible, so `kept=[A]`; the fill loop breaks immediately
(`kept.len()=1 >= resolved.active.len()=1`) and adds no branch; line 841 sets
`standby=[A]`. `A` is now in **both** active and standby. A subsequent
`expand_to_standby(key)` promotes `standby[0]=A` → `active=[A,A]`: a duplicated
branch that double-counts in `load()`, triggers a redundant Leader-row
re-registration, and corrupts `old_active` for the next reconcile.

### Fix

Exclude retained actives from the new standby set:

```rust
entry.standby = resolved.standby.into_iter().filter(|p| !kept.contains(p)).collect();
```

(and/or dedup against `active` inside `expand_to_standby`).

---

## 5. Low — Sub-millisecond `WatchOptions.Interval` truncates to 0

**Location:** `bindings/go/net/tool.go:930` (new in this branch).

### Current shape

```go
if opts.Interval > 0 {
    intervalMs = C.uint64_t(opts.Interval / time.Millisecond)   // integer division
}
```

`time.Duration` is int64 nanoseconds; `500 * time.Microsecond / time.Millisecond`
is `500000 / 1000000 = 0`. The `> 0` guard passes but `intervalMs = 0`, which
the substrate interprets as "pure event-driven, no staleness ceiling."

### Failure scenario

A caller sets `opts.Interval = 500 * time.Microsecond` to bound staleness. The
requested ceiling is silently discarded (and `1500µs` becomes `1ms`, losing
the fractional part). Low impact — sub-ms ceilings are unusual and the watch
is still event-driven — but a caller-provided value is dropped without signal.

### Fix

Round up to at least 1ms when `Interval > 0`, or reject/round with a documented
minimum: `intervalMs = (opts.Interval + time.Millisecond - 1) / time.Millisecond`.

---

## 6. Low — Consumer on a torn-down branch loses proofs until its ttl/2 refresh

**Location:** `sensing/rendezvous.rs:802` (`reconcile_with_snapshot` fill loop).

### Current shape

The surviving-consumer union captured from `old_active` (`rendezvous.rs:762`)
is applied only to **newly-added** branches (`rendezvous.rs:809-818`), never to
**kept** branches, and no branch is added when `kept` already fills
`resolved.active` (the loop breaks at `rendezvous.rs:802`).

### Failure scenario

An interest is expanded to `active=[A,B]` (B promoted via
`expand_to_standby`); consumer `C2` was admitted only on branch `B` (its `A`
registration was cap-refused — a real partial-admission population
`B{C1,C2}`, `A{C1}`). A fresh fold resolves `active=[A]`. Reconcile tears down
`B` (C2's only row) and captures `consumer_rows = {C1, C2}`, but `kept=[A]`
already satisfies `resolved.active.len()==1`, so the fill loop adds no branch.
The captured union is applied only to newly-added branches, so `C2` ends with
no row on any branch and stops receiving proofs. It self-heals on `C2`'s own
ttl/2 refresh (`register_capability_interest` re-registers it on `active=[A]`)
— a gap masked by soft state rather than an immediate re-registration. Adjacent
to the known SI-3 partial-admission residual.

### Fix

When a branch is torn down, re-register its surviving consumers onto a kept
branch immediately (apply the captured `consumer_rows` union to kept branches,
not only newly-added ones), rather than relying on the soft-state refresh.

---

## 7. Low — `update_interval` mis-anchors an Unestablished cell whose warm-start `promised_cadence > own_interval`

**Location:** `sensing/continuity.rs:229` (`ObservationCell::update_interval`).

### Current shape

The establishment deadline is set as `own_interval × factor`, ignoring
`promised_cadence` (`register`, `continuity.rs:206`; the generation-reset
branch, `continuity.rs:293`). But `update_interval` re-anchors by shifting the
deadline by the delta of `window(promised) = max(promised, own) × factor`:

```rust
let old_window = self.window(promised);   // max(promised, own_old) * factor
self.own_interval = own_interval;
let new_window = self.window(promised);   // max(promised, own_new) * factor
// deadline += new_window - old_window (or -= )
```

When `promised > own`, `window(promised)` diverges from the `own × factor`
basis the establishment deadline was actually set with, so the shift is wrong.

### Failure scenario

An Unestablished cell receives a warm-start beat with
`promised_cadence = 1000ms` while `own_interval = 200ms` (establishment
deadline = `t_reg + 600ms`). `update_interval` is then driven (e.g. via
`update_upstream_interval` on a relay's Unestablished upstream cell,
`mesh.rs:3152`) to a new interval that crosses/relates to `promised`; the
re-anchored deadline lands early or late relative to the correct
`own_new × factor`. Safety is preserved — an early expiry projects `Unknown`,
which is always safe — so this is a timing imperfection, not a wrong-verdict
bug.

### Fix

For an Unestablished cell, re-anchor against `own_interval × factor` (the
basis the establishment deadline actually uses), not `window(promised)`; only
the Established suspicion deadline should use `window(promised)`.

---

## 8. Low — `ProviderSelector` derived `Eq`/`Hash` disagree with the canonical digest identity

**Location:** `sensing/identity.rs:~568` (`ProviderSelector` enum + derives).

### Current shape

`canonical_bytes()` / `interest_digest()` sort + dedup the `Nodes`/`Tags`
vectors before hashing (canonical, injective), but `ProviderSelector` derives
structural `PartialEq`/`Eq`/`Hash` over the **raw** `Vec`s. Two specs that are
digest-identical can therefore compare unequal and hash to different buckets.

### Failure scenario

A caller builds an interest twice via the public tuple variants (bypassing the
canonicalizing `ProviderSelector::nodes()`/`tags()` constructors):
`Nodes(vec![7,9])` vs `Nodes(vec![9,7])`. `spec_a.interest_digest() ==
spec_b.interest_digest()` and `spec_a.key() == spec_b.key()`, yet
`spec_a != spec_b` under derived `Eq`. **Not currently reachable in-crate** —
all coalescing keys on the `Digest256` (`ProviderInterestKey` /
`CapabilityInterestKey`), and no code path compares `InterestSpec`/
`ProviderSelector` structurally or uses either as a map key (verified). It is a
latent footgun for external SDK code that compares specs structurally instead
of by digest; the digest itself stays canonical and injective.

### Fix

Either make the constructors the only way to build `Nodes`/`Tags` (seal the
variants), or hand-implement `Eq`/`Hash` over the canonicalized form so
structural equality matches digest identity.

---

## Verified clean

- **Cryptographic core / wire codec** (`identity.rs`, `wire.rs`, `frames.rs`):
  length-prefixed injective digest preimages, distinct derive-key domains
  (interest / constraints / attestation), a hand-rolled tamper-evident
  attestation transcript (all 12 fields proven malleability-free by test),
  strict size-capped postcard decode, no truncating casts or panics on peer
  bytes.
- **Epoch / continuity state machines** (`incarnation.rs`, `continuity.rs`):
  persist-then-participate boot ordering, equivocation poisoning with
  evict-last LRU bounds, the pessimism-safe/optimism-earned projection table,
  incarnation- and generation-crossing continuity rules — all exhaustively
  tested.
- **Interest table** (`table.rs`): per-downstream independent expiry,
  min-dominance aggregates, refusal partitioning with the SI-3 pending-transition
  fix, per-downstream amplification cap, structural audience separation.
- **`mesh.rs` integration (+5.7k)**: lock ordering consistent and never held
  across `.await`; the near-identical `reclaim_*`/`remove_*`/`update_*` helper
  families carry no wrong-variable/wrong-key copy-paste; all
  `spawn_sensing_frame_send` target/stream/subprotocol triples correct; every
  `UpstreamAction` handled or intentionally superseded; all maps swept or
  reclaimed on branch death; no panics on attacker-reachable paths.
- **§4.4 hop rule** (`delivery.rs`): `continuity_bearing` is set only from the
  relay's own `Continuity::Established`; warm-starts are always `false` — no
  freshness laundering.
- **Scope/audience** (`scope.rs`): the session-proven owner root is the only
  load-bearing input; the wire claim is cross-checked, never trusted.

## Recommended order of fixes

1. **#1** (High) — one-line cap, mirrors existing local-path guards; blocks a
   remotely-triggerable leader crash.
2. **#3, #4** (Medium logic) — silent wrong-verdict / state-corruption; small,
   local fixes.
3. **#2, #5** (Go binding) — the baseline race is a real correctness regression;
   the interval truncation is cosmetic.
4. **#6, #7, #8** (Low) — self-healing / timing / latent-footgun; fix
   opportunistically.
