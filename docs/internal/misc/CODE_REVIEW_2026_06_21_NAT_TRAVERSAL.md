# Code Review — NAT Traversal subsystem (2026-06-21)

Scope: `crates/net/src/adapter/net/traversal/**` (pure codecs, NAT-type
classification FSM, port-mapping clients) plus the runtime wiring in
`crates/net/src/adapter/net/mesh.rs` (receive-loop dispatch, `connect_direct`,
the coordinator/responder hole-punch flow, reflex probing, and the
reflex-override / capability-publication machinery).

Reference design: `docs/plans/NAT_TRAVERSAL_PLAN.md`,
`docs/plans/PORT_MAPPING_PLAN.md`.

---

## Overall assessment

High-quality, defensively-written code. The leaf modules are pure logic with
exhaustive tests (the 16-cell `pair_action` matrix is pinned cell-by-cell;
decoders are property/fuzz-tested for panic-freedom; many tests are explicit
regressions for previously-found bugs). The runtime wiring is careful about
concurrency (generation-stamped waiter cleanup; a publication mutex keeping
`(nat_class, reflex_addr, override_flag)` coherent against
`announce_capabilities_with`) and already closes several real attack vectors
(forged `PunchIntroduce`/`PunchAck` are bound to the recorded coordinator;
NAT-PMP enforces RFC 6886 §3.1 source filtering; `install` refuses to publish
the gateway's private IP).

The "NAT traversal is an optimization, not correctness" framing is consistently
maintained — every failure path falls back to the routed handshake, so **none
of the findings below break the correctness contract.**

All findings below live in the **rendezvous runtime path**, where the
implementation is weaker than both the surrounding code's posture and the plan.

---

## Resolution status — 2026-06-21 hardening pass (branch `nat-traversal-hardening`)

The original three findings were addressed in this branch. A verification pass
(compile + run of every new/changed test) confirms the fixes land and pass. Two
residual items that surfaced during that pass are tracked below as Findings 4
and 5, plus a set of non-blocking minor notes.

| Finding | Status | Commit | Test |
|---|---|---|---|
| 1 — UDP reflector | ✅ **Resolved** — coordinator path (`b54329d88`) + direct path (Stage 1, see Finding 4) | `b54329d88` | `rendezvous_coordinator.rs::request_punch_with_spoofed_self_reflex_ip_is_dropped`, `…_port_shifted_self_reflex_is_accepted` |
| 2 — `sender_node_id` unvalidated | ✅ **Resolved** (validation moved to receive loop after a cubic-flagged DoS in the first cut) | `fb151b4af` + follow-up | `punch_keepalive.rs::observer_acks_only_on_matching_sender_node_id`, `…::stray_keepalive_with_wrong_sender_does_not_burn_the_observer` |
| 3 — unbounded `fire_at_ms` | ✅ **Resolved** | `9b56e6d41` | `mesh.rs::keepalive_offset_tests` (4 cases) |
| 4 — direct unsolicited-introduce reflector | ✅ **Resolved** — `NAT_TRAVERSAL_V2_PLAN.md` Stage 1 | this branch | `rendezvous_introduce_validation.rs` (4 cases) |
| 5 — rate-limit budgets / `RendezvousRejected` | ✅ **Resolved** — `NAT_TRAVERSAL_V2_PLAN.md` Stage 2 | this branch | `rendezvous_coordinator.rs` (rate-limit + reject), `rendezvous_introduce_validation.rs` (train caps), codec + `error_kind` unit tests |

### Finding 1 fix — bind `self_reflex` to A's session source IP

`handle_punch_request` now resolves A's session up-front and drops the request
when `req.self_reflex.ip() != a_addr.ip()` (`mesh.rs:7772`), *before* `a_reflex`
is ever read. This closes the **coordinator-mediated** reflection path — an
attacker can no longer name a third-party victim IP. Symmetric-NAT port shifts
stay honoured (the guard keys on IP only), verified by the port-shifted
acceptance test. The drop is silent (→ A's `request_punch` times out as
`PunchFailed`); `RendezvousRejected` is still not constructed (Finding 5).

### Finding 2 fix — validate keep-alive `sender_node_id`

The sender check runs in the **receive loop, before the observer is removed**.
`punch_observers` values now carry the expected counterpart `node_id` alongside
the oneshot, and `dispatch_packet` consumes the observer via a `remove_if` that
fires only when `ka.sender_node_id` matches that id. The wire path is consistent
end-to-end: the keep-alive sender stamps its own `local_node_id` and the
counterpart's observer expects exactly that id.

The first cut put the check *after* the observer fired (inside the scheduler
task). cubic flagged that as a P2 DoS: the receive loop removed the observer on
the first keep-alive from `peer_reflex` regardless of sender, so a single
stray/spoofed packet consumed the observer and the late check then failed the
punch permanently — even if a valid keep-alive arrived moments later. Moving the
check ahead of removal means a wrong-sender packet is dropped without consuming
the observer, so a subsequent valid keep-alive still completes the punch.

Tests: `observer_acks_only_on_matching_sender_node_id` (self-validating —
a **control phase** with a matching id proves the injection path drives an ack,
so the reject phase can't pass vacuously) and
`stray_keepalive_with_wrong_sender_does_not_burn_the_observer` (the DoS
regression — stray-then-valid still acks).

### Finding 3 fix — clamp `fire_at_ms` lead

The inline offset math was extracted into a pure, unit-tested
`keepalive_send_offsets(fire_at_ms, now_ms, deadline)` (`mesh.rs:1404`) that
clamps `base_lead` to `punch_deadline` and uses saturating adds for the
`+100/+250 ms` spacing. Far-future and `u64::MAX` inputs are covered for both
clamping and panic-freedom.

---

## Finding 1 — Rendezvous is an unauthenticated UDP reflector to attacker-chosen addresses (Medium)

> **Status: resolved** — coordinator path closed in `b54329d88`; the direct
> unsolicited-introduce path (tracked as Finding 4) closed in
> `NAT_TRAVERSAL_V2_PLAN.md` Stage 1. The original analysis below is retained as
> the historical record.

The punch responder fires UDP keep-alives at a **wire-supplied** address with no
validation and no rate limit.

### Mechanism

- In `handle_punch_request` (`mesh.rs:7714`), the coordinator takes A's reflex
  verbatim from the *unsigned* `PunchRequest` body
  (`a_reflex = req.self_reflex`) and forwards it to B as `peer_reflex`. A can
  put a victim's `ip:port` here. (B's reflex, by contrast, comes from B's
  *signed* capability announcement via `reflex_addr_for`.)
- In the `PunchIntroduce` dispatch arm (`mesh.rs:5071-5090`), when **no waiter
  exists** — the legitimate responder role, since B never called
  `request_punch` — control falls straight through to `schedule_punch` on an
  *unsolicited* introduce. The forged-introduce binding only guards the case
  where a waiter *does* exist.
- `schedule_punch` (`mesh.rs:7903-7915`) then spawns a task that sends three
  keep-alives to `intro.peer_reflex`.

### Reachability

Two paths, both requiring only an authenticated mesh session:

1. **Direct:** a peer X with a session to B sends B a
   `PunchIntroduce{ peer: <any>, peer_reflex: <victim>, fire_at: now }`. B has
   no waiter for `<any>` → `schedule_punch` → B emits UDP at `<victim>`, with
   X's identity hidden behind B.
2. **Coordinator-mediated (honest relay):** a malicious initiator A sends
   `PunchRequest{ target: B, self_reflex: <victim> }` to an honest coordinator
   R. R fans out `PunchIntroduce{ peer: A, peer_reflex: <victim> }` to B; B
   fires at `<victim>`.

There is no per-requester budget on `PunchRequest` and no per-peer budget on
responder keep-alive trains. Fan-out is modest (3 × 14-byte packets per
message), but it is multiplied across every target an attacker names, and the
primary value to an attacker is source obfuscation / reflection rather than
bandwidth amplification.

### Why this is a gap, not a design choice

The plan explicitly calls for "two rate-limit budgets" for rendezvous
coordination (`NAT_TRAVERSAL_PLAN.md:181`) and defines a
`rendezvous-rejected (rate-limit / unknown target)` outcome
(`NAT_TRAVERSAL_PLAN.md:513`). But `TraversalError::RendezvousRejected` and
`RendezvousNoRelay` are **defined and mapped to FFI error codes
(`ffi/mesh.rs:144-145`) yet never constructed anywhere** — the entire
coordinator policy/rate-limit layer is unimplemented. Contrast the asymmetry:
the reflex handler correctly replies only to the peer's *known* address
(`peer.addr`, `mesh.rs:4967`), but rendezvous sends to a wire-supplied one.

### Recommended mitigations (in order of value)

1. **Bind `self_reflex` to A's session source IP at the coordinator.** In
   `handle_punch_request`, reject when `req.self_reflex.ip() != a_addr.ip()`
   (return / surface `RendezvousRejected`). A is genuinely at `a_addr`, so its
   real reflex IP matches (only the port varies, for symmetric NAT); this kills
   arbitrary-victim targeting at near-zero cost.
2. **Add the planned rate-limit budgets** — per-requester on `PunchRequest`
   (coordinator) and per-peer on responder keep-alive trains. The
   `is_auth_throttled` infrastructure used for subscribe auth is a model.
   Surface rejections as `RendezvousRejected`.
3. **Optionally** validate `intro.peer_reflex` against the announced reflex of
   `intro.peer` when one is cached in the fold.

---

## Finding 2 — Keep-alive `sender_node_id` is decoded but never validated (Low)

> **Status: resolved** (`fb151b4af`). The original analysis below is retained as
> the historical record.

`Keepalive.sender_node_id` is documented as **load-bearing**, specifically to
stop "a stray packet on the right source addr [from] falsely signal[ing] 'punch
succeeded'" (`rendezvous.rs:180-185`).

But the receive loop matches purely on the UDP `source` `SocketAddr` and
forwards the decoded `ka` (`mesh.rs:3668-3674`), and
`await_punch_observer_outcome` discards it (`mesh.rs:1362`). `ka.sender_node_id`
is never compared to the expected counterpart (`intro.peer`); `punch_id` is
likewise hardwired to `0` everywhere (`mesh.rs:7905,7934`). The field is
effectively dead.

### Impact

Low. The real session is still gated by the authenticated Noise handshake, so a
false observer firing only causes a wasted direct-handshake attempt before relay
fallback. But the documented guarantee is not delivered.

### Recommended fix

Thread the received `Keepalive` out of `await_punch_observer_outcome` and, in
`schedule_punch`'s observer task, compare `ka.sender_node_id == intro.peer`
before emitting the `PunchAck`. Alternatively, drop the "load-bearing" claim
from the doc to match reality.

---

## Finding 3 — `fire_at_ms` from `PunchIntroduce` is trusted unbounded (Low)

> **Status: resolved** (`9b56e6d41`). The original analysis below is retained as
> the historical record.

In `schedule_punch` (`mesh.rs:7871-7915`):

```rust
let base_lead_ms = intro.fire_at_ms.saturating_sub(now_ms);
let base_lead = Duration::from_millis(base_lead_ms);
// ...
tokio::time::sleep_until(start + offset).await;
```

`base_lead` has no upper clamp. A malicious or buggy coordinator can set
`fire_at_ms` far in the future, which makes the keep-alive sender task park for
an unbounded duration (holding a socket `Arc` + payload), and `start + offset`
(a tokio `Instant + Duration`) risks an overflow panic in the spawned task. The
observer task self-limits via `punch_deadline` (5 s), but the sender task does
not. Combined with the missing rate limit from Finding 1, flooding far-future
introduces accumulates parked tasks (memory / task-handle pressure).

### Recommended fix

Clamp `base_lead` to a small multiple of `TraversalConfig::punch_fire_lead`
(e.g. a few seconds) and drop introduces whose `fire_at` is implausibly distant.

---

## Finding 4 — Residual: the *direct* unsolicited `PunchIntroduce` reflector (Low–Medium)

> **Status: resolved** — `NAT_TRAVERSAL_V2_PLAN.md` Stage 1. The
> unsolicited (no-waiter) dispatch branch now gates every introduce through
> `Mesh::unsolicited_introduce_permitted` before it reaches `schedule_punch`:
> when a signed reflex for `intro.peer` is cached, the introduce's
> `peer_reflex` must share its IP (only the port may differ, for symmetric
> NAT) or the introduce is dropped; when no reflex is cached, a temporary
> per-source fixed-window cap (`unsolicited_introduce_rate`, 4 / 10 s keyed by
> the introducing session peer) admits only a trickle so an attacker naming a
> nonexistent counterpart can't flood. Stage 2 replaces the temporary cap with
> the full rendezvous responder budget + typed `RendezvousRejected`. Tests:
> `tests/rendezvous_introduce_validation.rs` — `…_mismatched_ip_fires_no_train`
> (drop), `…_port_shifted_same_ip_fires_train` (accept), `…_uncached_counterpart_still_punches`
> (legitimate race), `…_flood_from_one_source_is_capped` (cap). The original
> analysis below is retained as the historical record.

Surfaced during the 2026-06-21 verification pass. Finding 1's fix guards the
**coordinator-mediated** path only; the **direct** path from Finding 1's
reachability analysis is untouched:

- An authenticated session peer X sends responder B an unsolicited
  `PunchIntroduce{ peer: <any>, peer_reflex: <victim>, fire_at: now }`. B has no
  waiter for `<any>`, so the dispatch arm falls through to `schedule_punch`
  (`mesh.rs:5114→5126`).
- `schedule_punch`'s keep-alive sender task fires three packets to
  `intro.peer_reflex` **unconditionally** (`mesh.rs:7974-7982`); `peer_reflex`
  is wire-supplied and bound to nothing X-related.

The Finding 2 `sender_node_id` check does **not** close this — it gates only
whether B emits the *return* `PunchAck`, not whether B sends the keep-alive
*train*. So B still emits `3 × 14` bytes at an attacker-named address, source
obfuscated behind B.

### Why it's lower-severity than the headline

Reachable only by an authenticated mesh member; payload is tiny and there is no
amplification (the value is reflection / source-hiding). Finding 3's clamp now
bounds each parked sender task to ≤ `punch_deadline + 250 ms`, so flooding
far-future introduces no longer accumulates unbounded parked tasks — only
bounded-lifetime churn (still uncapped in *rate*; see Finding 5).

### Recommended fix

Finding 1's mitigation #3, now promoted from optional: in the unsolicited
branch, drop when `intro.peer_reflex` disagrees with `reflex_addr_for(intro.peer)`
if a reflex is cached for `intro.peer` in the fold. When no reflex is cached,
fall back to the Finding 5 rate-limit budget rather than firing blind.

---

## Finding 5 — Rate-limit budgets and `RendezvousRejected` (Low)

> **Status: resolved** — `NAT_TRAVERSAL_V2_PLAN.md` Stage 2. Added
> `RendezvousBudgets`: a coordinator per-requester fixed-window budget on
> `PunchRequest` (`charge_request`, default 4 / 10 s), a responder per-source
> budget on unsolicited keep-alive trains (`charge_train`, default 4 / 10 s,
> replacing Stage 1's temporary cap), and a global concurrent-train ceiling
> (`try_train_slot` → RAII `TrainSlot` held for the observer's lifetime, default
> 8). Knobs live on `TraversalConfig` (`punch_budget_window`,
> `punch_requests_per_window`, `punch_trains_per_window`,
> `punch_trains_concurrent_max`). A new `PunchReject { target, reason }` wire
> message (`RendezvousMsg` discriminator `0x04`, `RejectReason`) carries the
> coordinator's refusal — rate-limited, unknown-target-reflex,
> no-session-with-target, or reflex-mismatch (Finding 1's previously-silent
> drop) — back to the requester, whose `request_punch` now resolves
> *immediately* with `TraversalError::RendezvousRejected(reason)` instead of
> waiting out `punch_deadline`. The FFI-mapped error code (`ffi/mesh.rs:144-145`)
> and the Node/Python/Go `traversal_err` mappers, previously dead, are now
> reachable. Tests: `rendezvous.rs` codec unit tests (`punch_reject_roundtrip`,
> `…_unknown_reason_decodes_to_unspecified`, `…_wrong_length_rejects`,
> `reject_reason_kind_strings_are_stable`); `mod.rs::error_kind_tests`;
> `rendezvous_coordinator.rs` (`coordinator_rate_limits_requests_from_one_requester`
> + the three former-timeout tests now asserting fast typed rejection);
> `rendezvous_introduce_validation.rs` (`…_flood_from_one_source_is_capped`,
> `…_globally_capped_across_sources`). `RendezvousNoRelay` construction lands in
> Stage 3 (coordinator selection). The original analysis below is retained as the
> historical record.

Carried forward from Finding 1's mitigation #2 and its "why this is a gap" note;
still true after this branch:

- There is still **no per-requester budget on `PunchRequest`** and **no per-peer
  budget on responder keep-alive trains**. Volume-based abuse over the
  still-open direct path (Finding 4) is unbounded in *count*.
- `TraversalError::RendezvousRejected` / `RendezvousNoRelay` are still **never
  constructed**. Finding 1's guard surfaces as a silent drop → `PunchFailed`
  timeout, so the FFI-mapped error codes (`ffi/mesh.rs:144-145`) stay dead.

### Recommended fix

Add the planned budgets (the `is_auth_throttled` subscribe-auth infrastructure
is the model) and surface both the rate-limit rejection and the Finding 1 IP
mismatch as `RendezvousRejected` — so the error path stops being dead and A gets
a fast, typed failure instead of waiting out `punch_deadline`.

---

## Minor notes (non-blocking)

1. ✅ **Resolved (Stage 1).** The Finding 1 guard's direct-session assumption is
   now stated in the `handle_punch_request` guard comment. (Was: the guard
   compares `self_reflex.ip()` against `PeerInfo::addr`, which is the relay's
   address for relay-reached peers — correctness-preserving since A↔R is direct
   in the normal topology, but undocumented.) The same IP-only rationale is
   mirrored in the new unsolicited-introduce guard.
2. **IP-only binding has two acknowledged edges.** (a) IPv4-mapped-IPv6 or
   multi-public-IP CGNAT pools can drop a valid A (→ relay fallback); (b) under
   CGNAT a malicious A can still name `self_reflex = <shared IP>:<co-tenant port>`
   and reflect at a co-tenant behind the same public IP. Both are inherent to
   "bind IP, allow any port for symmetric NAT" and are accepted tradeoffs. The
   Stage 1 unsolicited-introduce guard inherits edge (b) identically.
3. ✅ **Resolved (Stage 1).** The `keepalive_send_offsets` doc now says the clamp
   bounds the sender task's lifetime "to within ~250 ms of the observer's",
   accounting for the post-clamp `+100/+250 ms` spacing.

---

## Notable positives (no action needed)

- Pure codecs (`reflex.rs`, `rendezvous.rs`, `natpmp.rs`) and the
  classification FSM (`classify.rs`) are exhaustively unit/property tested,
  including panic-freedom over malformed input and the full 16-cell pair-type
  matrix pinned cell-by-cell.
- Forged `PunchIntroduce` / `PunchAck` are bound to the recorded coordinator
  (`mesh.rs:5071-5124`); waiter maps use generation stamps so timeout cleanup
  never evicts a racing replacement (`probe_reflex`, `request_punch`,
  `connect_direct`).
- `(nat_class, reflex_addr, reflex_override_active)` are published as a coherent
  triple under `traversal_publish_mu`, with the mid-sweep override race closed
  in `commit_reclassify_observations` (`mesh.rs:12401-12446`).
- NAT-PMP enforces the RFC 6886 §3.1 source-address filter via
  `UdpSocket::connect` (`natpmp.rs:403-414`), refuses `ttl=0` installs, and
  refuses to publish the gateway's private IP when the external cache is empty.
- `SequentialMapper` correctly invalidates its cached protocol on install
  failure and re-probes before a cross-protocol fallback install
  (`sequential.rs:133-213`).
- `connect_direct` stats accounting is precise — `punches_attempted`,
  `punches_succeeded`, and `relay_fallbacks` only bump after the corresponding
  wire activity actually lands.

---

## Suggested follow-up

The 2026-06-21 hardening pass landed Finding 1 (mitigation 1), Finding 2, and
Finding 3. `NAT_TRAVERSAL_V2_PLAN.md` Stage 1 then landed Finding 4 and minor
notes 1 & 3; Stage 2 landed Finding 5 (rendezvous budgets + `PunchReject` +
typed `RendezvousRejected`).

**All findings from this review are now resolved.** Remaining NAT-traversal work
(coordinator auto-selection, background direct-path upgrade + session-migration
contract, the netns NAT-simulator harness, and stats/observability parity) is
tracked in `NAT_TRAVERSAL_V2_PLAN.md` Stages 3–5, not here.
