# Code Review — "Port scanning" / port-discovery surface (2026-06-21)

Scope: the NAT-traversal **discovery / probe** surfaces, reached when reviewing
what the codebase calls "port scanning." There is **no general-purpose port
scanner** in this crate — the `net port` CLI (`cli/src/commands/port.rs`) is an
explicit design stub (`#![allow(dead_code)]`). The "scanning"-style network
discovery is split across two subsystems:

1. **Router/gateway discovery** — UPnP SSDP `M-SEARCH` multicast + NAT-PMP
   gateway probe, in `adapter/net/traversal/portmap/`. (The literal network
   scan.)
2. **Reflex-probe NAT classification sweep** — `reclassify_nat` probing peers
   to detect cone-vs-symmetric port-allocation behavior, in
   `adapter/net/mesh.rs` + `traversal/{reflex,classify}.rs`. (Scanning *our
   own* NAT's port behavior, STUN-style.)

NAT-PMP wire codec / gateway-IP discovery / the sequential mapper were already
covered in `CODE_REVIEW_2026_06_21_NAT_TRAVERSAL.md`; this document adds the
**UPnP discovery** review (Part A) and the **classification-sweep** deep dive
(Part B).

---

## Abuse-resistance summary (the dual-use "scanner" question)

None of these surfaces can be turned into a general port scanner directed at
attacker-chosen hosts/ports:

- **UPnP SSDP** targets only the fixed multicast group `239.255.255.250:1900`
  (`upnp.rs:7`).
- **NAT-PMP** targets the gateway IP resolved from the **OS routing table**
  (`gateway.rs::default_ipv4_gateway`), never a wire- or peer-supplied address.
- **Reflex probes** go only to **authenticated, already-connected peers**, and
  the response is sent back to the peer's own cached address.

No path lets a remote/untrusted input choose a scan target. (Contrast the
rendezvous reflection issue from `CODE_REVIEW_2026_06_21_NAT_TRAVERSAL.md`
Finding 1, where `peer_reflex` *was* wire-supplied — that asymmetry does not
exist here.)

---

# Part A — UPnP-IGD discovery (`portmap/upnp.rs`)

A thin, well-bounded wrapper over the `igd-next` crate. No correctness bugs
found.

## Positives

- **Bounded discovery.** `UPNP_SEARCH_TIMEOUT` (1.5 s) < `UPNP_DEADLINE` (2 s)
  (`upnp.rs:63,68`), so SSDP failures surface inside the per-call deadline;
  every op is wrapped in `tokio::time::timeout` (`upnp.rs:142,172,224`). The
  no-IGD-on-network case maps to `Unavailable` (`search_err_to_port_mapping`,
  `upnp.rs:242`) so the sequencer falls through cleanly; `probe_on_no_router_
  returns_unavailable` pins the no-hang property.
- **`add_any_port` over `add_port`** (`upnp.rs:187`) is correct and important:
  `add_port` assumes external == internal, but some IGDs silently remap and
  return success, so the mesh would advertise an unreachable external port.
  `add_any_port` returns the actually-mapped port, recorded in
  `PortMapping.external`. Well-commented.
- **Cache invalidation on error** (`invalidate_gateway` on every error/timeout
  arm) re-triggers SSDP on the next call after a router reboot / network change
  rather than sticking on a dead `Gateway` handle.
- **`remove_port` keys on `mapping.external.port()`** (`upnp.rs:226`) — the
  actual mapped external port from `add_any_port`. Correct.

## Minor notes (non-blocking)

- **A1. `UpnpMapper::new` doesn't validate `local_ip`** (`upnp.rs:96`). The doc
  says "not `0.0.0.0`, not loopback," but nothing enforces it; `0.0.0.0` would
  produce an `AddPortMapping` most routers reject. Only reachable by direct
  misuse — the wired path (`sequential_mapper_from_os` →
  `local_ipv4_for_gateway`) already supplies a validated non-unspecified IPv4.
  A `debug_assert!(!local_ip.is_unspecified() && !local_ip.is_loopback())`
  would document the contract cheaply.
- **A2. `add_port_err_to_port_mapping` is dead in production**
  (`#[allow(dead_code)]`, `upnp.rs:254`) — `install` uses `add_any_port` →
  `add_any_port_err_to_port_mapping`. It is retained only for the error-mapping
  unit tests. Harmless but misleading; add a "test-only" note or fold its
  assertions into the `AddAnyPortError` mapper's tests and drop it.
- **A3. `install` re-reads `get_external_ip` even right after `probe` did**
  (`upnp.rs:176`) — one extra SOAP round-trip. Arguably correct (the WAN IP can
  change between probe and install), so not a defect; noting it is not cached
  the way NAT-PMP caches its external IP.

---

# Part B — Reflex-probe / NAT-classification sweep

Pipeline: `reflex.rs` codec → `probe_reflex` → `reclassify_nat` sweep →
`classify` FSM → `commit_reclassify_observations` → `nat:*` capability tags.
The leaf codec (`reflex.rs`) and the FSM (`classify.rs`) are hardened with
exhaustive unit/property tests. The issues are in the **sweep orchestration**.

## Finding B1 — `reclassify_nat` has no single-flight guard, contradicting its doc (Medium)

The docstring (`mesh.rs:12435`) states: *"Runs at most one sweep at a time — a
second call while a sweep is in flight is a no-op."* The body
(`mesh.rs:12448-12505`) has **no such guard** — no atomic flag, no mutex.

Normal path is safe: `spawn_nat_classify_loop` (`mesh.rs:3258-3265`) awaits each
`reclassify_nat()` serially. But the method is `pub`, exported via FFI
(`net_mesh_reclassify_nat`, `ffi/mesh.rs:1027`) and every binding, so an
operator call concurrent with the background tick — or two operator calls — runs
two sweeps at once. They then collide on `pending_reflex_probes`, keyed by
`peer_node_id` (`probe_reflex`, `mesh.rs:11931`): the second sweep's
`insert(peer, …)` drops the first sweep's oneshot sender, so the first sweep's
`probe_reflex` resolves as `ReflexTimeout` (cancelled). The earlier sweep is
silently starved.

**Fix.** An `AtomicBool` compare-exchange at the top (`return` if already
classifying; clear on exit), which also closes the probe-map interference — or
drop the doc claim. The guard is the better fix.

## Finding B2 — A sweep with <2 successful probes downgrades a good classification to `Unknown` (Medium)

`reclassify_nat` feeds whatever probes succeeded into the FSM
(`mesh.rs:12496-12501`). `classify` returns `Unknown` for fewer than 2
observations (`classify.rs:281`). But `commit_reclassify_observations` only
guards `latest_reflex == None` (`mesh.rs:12433`) — **not** "fewer than 2
observations." So **one** successful probe → commits `nat_class = Unknown` +
that single reflex, overwriting a previously-good `Cone` / `Open`.

Reachable **without any concurrency**: two peers selected, one probe's UDP
response lost or that peer slow-but-under-`classify_deadline` → `[Ok, Err]` → 1
observation → `Unknown` committed. Packet loss is routine.

It contradicts the anti-flap rationale stated just above it, in the
deadline-expired branch (`mesh.rs:12487`): *"treating deadline-expired as
Unknown would flap state on a temporarily slow link."* The <2-observation case
has the identical flap but isn't guarded. Impact is bounded by the framing
(`pair_action` treats `Unknown` as "attempt, fall back"), but it is an avoidable
~60 s window (`classify_deadline × 12`, `mesh.rs:3220`) of mis-advertised
`nat:unknown` after every lossy sweep.

**Fix.** In `reclassify_nat` (or the commit), if successful observations < 2,
keep prior state instead of committing — mirror the deadline-branch behavior.
`commit_reclassify_observations` was already split out to be unit-testable
without a mesh, so this is straightforward to cover.

## Finding B3 — Wildcard bind + port-preserving NAT over-classifies as `Open` (Low–Medium)

`classify` treats an unspecified bind IP as a wildcard and accepts **port-only**
equality as `Open` (`classify.rs:300-306`). A node bound to `0.0.0.0:9001`
behind a port-preserving **cone / restricted-cone** NAT observes reflex
`<public>:9001` → matches → `Open`. The docstring asserts such a node "is in
fact directly reachable" (`classify.rs:296`), which holds only for *no-NAT* or
*full-cone* — a restricted-cone node is **not** reachable by an unsolicited
`Direct` connect from a peer it has not contacted.

Effect: advertises `nat:open` → peers pick `pair_action → Direct` →
restricted-cone drops the unsolicited inbound → `Direct` fails → relay fallback.
Correctness holds, optimization lost. From the reflex data alone under a
wildcard bind, "no NAT" and "port-preserving NAT" are genuinely
indistinguishable, so this may be an accepted limit — but the docstring
overstates the guarantee. At minimum, soften the comment; ideally, note the
`Direct`-then-fallback cost so operators understand why a wildcard-bound NATed
node still pays a relay round-trip on first contact.

## Finding B4 — Peer selection doesn't ensure destination diversity (Low)

`peers.iter()…take(2)` (`mesh.rs:12467`) picks two arbitrary peer *node ids*.
Symmetric-NAT detection requires two distinct **destination IPs**. Two node ids
resolving to the same public IP (two mesh processes on one host, etc.) are one
destination from the NAT's perspective; a symmetric NAT keyed on dest IP may
then hand out the same port for both → misclassified `Cone`. Low probability,
and it only affects the Cone-vs-Symmetric distinction — worth a caveat comment,
not a blocker.

## Finding B5 — Reflex echo uses the cached handshake addr, not the live packet source (Low / doc)

The Request handler echoes `PeerInfo.addr` (`mesh.rs:4967`), which is set only
at handshake / key-rotation (`mesh.rs:4314,4333`) — data packets never refresh
it. The docstring calls it "the last address our kernel saw packets from this
peer arrive on" (`mesh.rs:4963`), which overstates it; a mid-session NAT rebind
without re-handshake yields a **stale** reflex.

That said, echoing the cached *authenticated* handshake addr is spoof-resistant,
whereas echoing `dispatch_packet`'s live `source` (a UDP source address) would
be spoofable. So the current choice is a defensible security tradeoff that is
just under-documented. Either correct the comment, or switch to `source` with
eyes open about the spoofing tradeoff.

---

## Summary

| # | Finding | Severity | Fix shape |
|---|---------|----------|-----------|
| A1 | `UpnpMapper::new` doesn't validate `local_ip` | Low | `debug_assert!` the contract |
| A2 | `add_port_err_to_port_mapping` dead in prod | Low | mark test-only / drop |
| A3 | `install` re-reads `get_external_ip` | Info | none (arguably correct) |
| B1 | `reclassify_nat` no single-flight guard vs doc | **Medium** | `AtomicBool` guard |
| B2 | <2-probe sweep downgrades class to `Unknown` | **Medium** | keep prior on <2 obs |
| B3 | wildcard bind + port-preserving NAT → `Open` | Low–Med | soften doc / accept limit |
| B4 | peer selection lacks destination diversity | Low | caveat comment |
| B5 | reflex echo uses cached addr, not live source | Low | doc or switch to `source` |

**None break the correctness contract** — every miss falls back to the routed
handshake. B1 and B2 are the actionable bugs with clean, testable fixes
(single-flight `AtomicBool`; sub-2-observation guard); B3/B5 are primarily
docstring-accuracy fixes; A1/A2/B4 are minor cleanups.
