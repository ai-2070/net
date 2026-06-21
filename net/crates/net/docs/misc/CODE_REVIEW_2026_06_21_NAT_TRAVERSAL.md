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

All three findings live in the **rendezvous runtime path**, where the
implementation is weaker than both the surrounding code's posture and the plan.

---

## Finding 1 — Rendezvous is an unauthenticated UDP reflector to attacker-chosen addresses (Medium)

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

Findings 1 (mitigation 1) and 3 are small, localized changes. A natural single
patch:

- `handle_punch_request`: reject `PunchRequest` whose `self_reflex.ip()` does
  not match the requester's session source IP.
- `schedule_punch`: clamp `base_lead`.
- Wire `RendezvousRejected` into the coordinator so the FFI-mapped error code
  stops being dead.

Finding 2 is a doc-vs-implementation reconciliation that can ride along or be
deferred.
