# MeshOS Snapshot Change-Gating (E-10) — Design Record

Status: **Path A implemented** (2026-05-29); Path B documented, not taken.
Owner: TBD
Related: `POLLING_TO_EVENT_DRIVEN_SDK_PLAN.md` (this is the E-10 slice spun out
into its own record), `DECK_PLAN.md`, `MESHOS_PLAN.md`.

This is a keepsake: it records both the route we took and the one we didn't, with
enough detail that a future reader can pick up Path B without re-deriving the
analysis.

## 1. Problem

The MeshOS loop runs `publish_snapshot()` at the end of every reconcile pass —
once per `tick_interval` (default **500 ms**). Before E-10 that call
unconditionally stored a fresh `MeshOsSnapshot` into the `ArcSwap` **and** fired
the change signal (`Notify::notify_waiters`). So every deck consumer parked on the
signal (`DeckClient::watch`, `SnapshotStream`, `StatusSummaryStream`) woke ~2×/sec
even on a totally quiet cluster, re-read, and usually found nothing new. A
`SnapshotStream` with no dedup re-emitted an identical snapshot every tick forever.

Goal: stop waking/emitting on ticks where nothing meaningful changed, without
losing prompt delivery of real changes and without breaking any consumer.

## 2. The trap that shapes everything

`MeshOsSnapshot` derives `PartialEq` and carries no raw `Instant`, so a naive
"only publish when `new != last`" gate looks obvious. **It does not work.** The
snapshot is pervaded by *server-projected relative-time fields*, all computed as
`now - event_time` where `now = actual.last_tick` (which advances every tick):

- `DaemonSnapshot.age_ms` — advances for any running/stopped daemon
- `freeze_remaining_ms` — counts down during a freeze
- `RestartStateSnapshot::{BackingOff,CrashLooping}.until_ms`
- migration `elapsed_ms` / `age_in_phase_ms`, peer `since_ms`, avoid-list TTLs,
  `recently_emitted[].age_ms`

On any real node (which essentially always has a running daemon), consecutive
snapshots therefore **differ every tick** even when nothing structural changed. A
full-snapshot equality gate would never suppress.

Worse, the obvious "fix" — gate the *store* (skip publishing when only time
advanced) — **freezes those fields** at their last-stored value. That breaks any
consumer that reads them: e.g. `watch(|s| s.freeze_remaining_ms < 1000)` would
never fire if the countdown is frozen. So gating the store is off the table.

This tension is fundamental: **a live-ticking counter is, by definition,
activity.** You cannot have both smoothly-ticking server-side counters and a
truly idle (zero-work) loop. The two routes below differ in how they resolve it.

## 3. Path A — structural-view signal gate (TAKEN, implemented)

Keep storing a fresh snapshot every tick (counters stay live; the store is a cheap
atomic swap), but **gate only the change *signal*** on whether the *structural*
content changed.

### Mechanism

- **`MeshOsSnapshot::from_state_at(now, ..)`** (`snapshot.rs`) — the existing
  builder with an injectable reference time. `from_state` delegates with
  `now = last_tick` (unchanged behavior).
- Each publish, the loop builds a **structural view** via `from_state_at(structural_ref, ..)`
  where `structural_ref` is a fixed instant captured at loop construction. Because
  both the reference and every event time are fixed, every relative-time field is a
  *constant* across ticks — so the structural view is invariant to tick progression,
  and differs only on a genuine structural change (daemon set, lifecycle, replica
  holders, freeze active/inactive, emitted actions, audit/log rings, …).
  - **The reference is one-sided, by construction.** It is captured at startup, so
    it *precedes* every event the loop later records. That makes the projection
    asymmetric — and the asymmetry is what we want:
    - *Past*-relative fields (`age_ms`, peer/maintenance `since_ms`,
      `recently_emitted[].age_ms`) project to `now - event` with `now` before the
      event, so they **saturate to 0** and drop out of the comparison entirely.
      Uptime/age is cosmetic; erasing it is the point.
    - *Future*-relative fields (`freeze_remaining_ms`, backoff/crashloop `until_ms`,
      maintenance `deadline_remaining_ms`, avoid-list TTLs) project to
      `event - now` with the event after `now`, so they stay **positive constants**
      that encode the absolute deadline. A real change to a freeze/backoff/deadline
      window therefore *does* move the view and signal.
- `publish_snapshot` stores the real snapshot every tick, then bumps the
  change-generation **only if** the structural view differs from the last one
  (`last_published_structural`).
- The signal moved from `Notify` to **`watch::Sender<u64>`**. A consumer holding
  *one* receiver across awaits is missed-wakeup-safe (watch tracks the seen
  generation), so a bump landing between two `changed()` awaits is still observed.

### Why the failure direction is safe

The structural view compares the *whole* snapshot via derived `PartialEq` after
projecting at a fixed time. A field we forget to think about is still compared, so
the worst case is **over-signalling** (a redundant wake), never a missed edge. The
only thing that could *hide* a real change is wrongly treating a structural field
as time-relative — and we don't normalize fields explicitly; the fixed-reference
rebuild does it uniformly. (Active-migration `elapsed_ms` is pre-computed outside
`from_state`, so it is NOT cancelled — an in-flight migration over-signals every
tick. Acceptable: you're genuinely busy.)

**The one under-signal (intended).** Because past-relative fields saturate to 0
(§3 Mechanism), a change that manifests *only* as a past event-time shift — e.g. a
daemon's `last_started` moving while `lifecycle` stays `Running` (an "uptime
reset") — does not move the structural view and is not signalled. This is the
deliberate trade: age is cosmetic. A real restart also flips `lifecycle`
(`Running`→`Stopped`/`Starting`→`Running`), and each of those transitions does
signal; the only gap is a restart so fast both publishes observe `Running`, and
even then a ceiling re-read still surfaces the corrected uptime. Pinned by
`structural_view_collapses_age_but_preserves_freeze_window` (see Tests).

**Why deep-eq, not something cheaper.** Two alternatives were considered and
rejected on purpose:
- *Hashing the structural view* is impossible — `MeshOsSnapshot` carries `f32`/
  `f64` fields (`saturation`, `cpu_load_1m`, …), so it can't derive `Hash`/`Eq`,
  only `PartialEq`.
- *A reconcile-bumped "dirty" generation* that skips the build+compare on untouched
  ticks would be one missed mutation site away from a **silent under-signal**, and
  some structural transitions are time-triggered with no inbound event (an expiring
  freeze clears `freeze_until` inside `gc_freeze`), which a naive "only on inbound
  events" flag would miss outright. The deep-eq's sole failure mode is the safe one
  (over-signal), so its robustness is worth one rebuild per tick.

### What it delivers / doesn't

- ✅ A real "changed-only" signal: consumers parked purely on it (a long-ceiling
  `watch`) wake on transitions, not on cosmetic ticks.
- ✅ Closes the deck-stream **best-effort-per-edge gap** (the `watch<u64>`
  migration): a long `snapshot_poll_interval` is now safe — the ceiling becomes a
  true backstop rather than a functional poll.
- ⚠️ **Does NOT change default behavior.** At the default 100 ms ceiling the deck
  streams still tick — correctly, because that's what keeps live counters live.
  Out-of-the-box idle-quiet is *unlocked* (opt-in via a long ceiling), not
  automatic.
- Cost: one extra snapshot build + one retained snapshot per tick (the structural
  view + `last_published_structural`). Bounded, on the loop thread, 2×/sec.

### As-built commits

- `from_state_at` refactor + invariance test.
- `publish_snapshot` gate + `Notify`→`watch<u64>` + deck `watch`/`SnapshotStream`/
  `StatusSummaryStream` consumers + the quiet-on-idle/fires-on-change runtime test.
- Post-review follow-ups (2026-05-29): corrected the `from_state_at` doc to state
  the one-sided projection accurately + added the age-collapse/freeze-preserve
  test; recorded the deep-eq rationale in `publish_snapshot`; noted the
  first-`changed()` semantics on `subscribe_changes`; marked the E-8/E-9
  `changed()`/`changed_owned()` references in the parent plan as superseded.

### Tests

- `snapshot.rs::from_state_at_fixed_reference_cancels_per_tick_progression` — the
  fixed-ref view is invariant to `last_tick` yet moves on a lifecycle change. Note:
  this test places the event *before* the reference, so it exercises the
  positive-constant-delta path, not the saturate-to-0 path the loop actually hits.
- `snapshot.rs::structural_view_collapses_age_but_preserves_freeze_window` — the
  production ordering (reference *before* events): an age-only "uptime reset" must
  NOT move the structural view, while committing/extending a freeze window must.
  Pins both halves of the §3 asymmetry.
- `deck.rs::change_signal_stays_quiet_on_idle_ticks_and_fires_on_structural_change`
  — end-to-end: generation quiet across idle ticks *and* a freeze countdown, fires
  promptly on a freeze commit.
- Existing deck (60) + meshos (326) suites unchanged; net-node/net-python compile
  (streams stay `Send + Sync` for the napi/pyo3 `#[pyclass]` wrappers).

## 4. Path B — client-side time projection (NOT TAKEN)

The only way to get **automatic** idle-quiet (zero per-tick emits at the default
ceiling) *with* smoothly-ticking counters: stop projecting time server-side.

### Idea

Change the snapshot schema so time fields become **absolute base timestamps**
instead of `now`-relative deltas:

- `DaemonSnapshot.age_ms` → `lifecycle_since_epoch_ms` (or an `Instant`-equivalent
  monotonic base the client can subtract from).
- `freeze_remaining_ms` → `freeze_until_epoch_ms`.
- `until_ms`, `since_ms`, migration `elapsed_ms`/`age_in_phase_ms`, avoid TTLs →
  their absolute anchors.

Then the snapshot is **stable between structural changes** (no field advances on a
quiet tick), so:

- A real `PartialEq` gate on the snapshot works directly — no structural-view
  rebuild needed.
- The loop can gate the **store** itself (publish only on change), so an idle node
  does zero snapshot allocation + zero downstream wakeups out of the box.
- Deck renders live counters client-side as `now() - base`, so they tick smoothly
  in the UI without any server activity.

### Why we didn't take it

- **Cross-cutting wire/schema change.** Every deck consumer (Rust SDK, the
  `node`/`python`/`go` deck bindings, Deck-the-binary, any dashboard) reads these
  fields today and would need to migrate to compute-from-base. Postcard wire compat
  requires field-count/order agreement, so it's a coordinated rollout, not a local
  edit.
- **Test churn.** The `snapshot.rs` age-anchoring tests, the deck status/summary
  tests, and cross-language fixtures all assert on the projected `_ms` values.
- **Risk/benefit at current scale.** The win is real only if there are long-lived
  deck watchers at scale; Path A already makes that case safe via an opt-in long
  ceiling. Path B is the right call once Deck dashboards are a steady-state
  production load, or if we want the loop itself provably idle on a quiet node.

### If/when we pick it up — sketch

1. Add the absolute-timestamp fields alongside the existing `_ms` fields
   (`#[serde(default)]`), dual-emit for one release so consumers migrate without a
   flag day.
2. Move each binding + Deck to compute live values client-side from the base.
3. Once no consumer reads the projected `_ms` fields, remove them (a second wire
   bump).
4. With the snapshot now stable on idle, gate the **store** in `publish_snapshot`
   on a plain `PartialEq` (the structural-view rebuild from Path A becomes
   unnecessary and can be deleted — or kept as a cheap fast-path).
5. Lower/retire the deck `snapshot_poll_interval` default ceiling, since the signal
   is now the primary path and the snapshot no longer drifts on idle.

This supersedes Path A's structural-view machinery rather than building on it; keep
that in mind so the two don't accrete.

## 5. Decision

Path A shipped because it is safe, local, fully tested, and unlocks idle-quiet for
operators who want it — while leaving live counters and all existing consumers
untouched. Revisit Path B when steady-state Deck watcher load (or a hard "idle node
does zero work" requirement) justifies the cross-cutting schema migration.
