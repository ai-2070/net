# Port Mapping Plan

Opportunistically request a port mapping on the operator's home router via UPnP-IGD and NAT-PMP / PCP so a NATed mesh node can advertise itself as `NatType::Open` + a stable public `ip:port`, cutting relay hops out of the data plane when the gateway cooperates. Implements stage 4b of [`NAT_TRAVERSAL_PLAN.md`](NAT_TRAVERSAL_PLAN.md); references §4 as the parent contract.

> **Framing.** Same as the parent plan: port mapping is a **latency / throughput optimization**, not a correctness requirement. A node whose router doesn't speak UPnP / NAT-PMP / PCP still reaches every peer through the routed-handshake path. A failed install, a refused mapping, or a renewal timeout all mean "stayed on the relay" — none of them is a connectivity failure. Docstrings + READMEs written as part of this stage must not imply port mapping is required for anything; it only makes the already-working mesh faster when the gateway allows it.

## Context

Stage 4a landed the `reflex_override` surface: both config-level (`MeshNodeConfig::reflex_override`) and runtime (`MeshNode::set_reflex_override` / `clear_reflex_override`) setters are live across all four bindings. Setting an override forces `NatClass::Open` and pins `reflex_addr` to the supplied external `SocketAddr`; the classifier sweep short-circuits.

**Stage 4b is the consumer of that surface.** A port-mapper task, when enabled, calls `set_reflex_override(external)` on a successful `AddPortMapping` and `clear_reflex_override()` on renewal failure or shutdown. No core changes outside the new module; everything the task needs at the mesh boundary already exists.

### What the parent plan specifies

Parent §4 sketches the behavior in 6 bullets (probe gateway, try NAT-PMP first with 1 s deadline, fall back to UPnP with 2 s, 30 min renewal, `DeletePortMapping` on shutdown) and mentions two deps (`igd-next`, `rust-natpmp`). Decision 10 below resolved the NAT-PMP side by inlining the ~100-line wire codec directly — `rust-natpmp` is *not* a dep; only `igd-next` is pulled in for UPnP. The implementation-level decisions below document the remaining choices.

## Goals

- Spawn a `PortMapperTask` from `MeshNode::start()` when `MeshNodeConfig::try_port_mapping(true)` is set.
- Discover the gateway, install a mapping, and flip the node to `NatClass::Open` with the mapped external address via the stage-4a override hook.
- Periodically renew (30 min cadence; router TTLs are typically 1 hour).
- Revoke (DeletePortMapping) + `clear_reflex_override()` on clean shutdown.
- Graceful degradation when the router doesn't cooperate — continue through the classifier path, never hang boot.
- Mockable task lifecycle so the state machine is fully unit-tested without a real router in the loop.
- Parity across bindings: `try_port_mapping` flag on all four SDK surfaces.

## Non-goals

- **Install-side retry / exponential backoff.** Parent decision 5 ("best-effort side quest") applies — one shot per protocol on startup, no retry loop. If both fail, the task logs + exits and the classifier takes over.
- **Multi-mapping.** We install one mapping for the mesh's bind port. No per-subprotocol granularity; no per-peer mapping.
- **PCP (RFC 6887).** Out of scope entirely for this plan. NAT-PMP (RFC 6886) and PCP share UDP port 5351 and the first version byte is how routers distinguish them, but the wire formats diverge past that — PCP uses a 24-byte common header with opcode-specific payloads (e.g., MAP requests are 60 bytes total), while NAT-PMP uses 2/12/16-byte packets. Our inlined `natpmp.rs` codec implements RFC 6886 only; `rust-natpmp` (mentioned in the parent plan) is similarly NAT-PMP-only and does *not* speak PCP despite the shared port. Advanced PCP features (`PEER` opcodes, `ANNOUNCE`, filtering) require a separate client and are punted to a follow-up if practical deployments need them.
- **Persistent mapping state across restarts.** Each mesh boot re-probes and re-installs. Parent decision 12 ("accept the crash leak") covers the case where shutdown skips revoke; the router cleans up on TTL expiry.
- **Windows NAT-PMP.** Parent plan assumed `rust-natpmp`'s default-gateway helper; 4b-2 inlined the wire codec instead (decision 10 below), so gateway discovery on Windows needs its own path and is not load-bearing. If only UPnP works on Windows in practice, that's fine.

---

## Design decisions

### 1. One `PortMapperClient` trait; implementations chain in sequence

Keep one async trait per installation-step verb (`probe`, `install`, `renew`, `remove`). One implementation per protocol (`NatPmpMapper`, `UpnpMapper`) plus two test fixtures (`NullPortMapper` that always returns `Unavailable`, a mock for controlled lifecycle tests). A composing `SequentialMapper` tries NAT-PMP first, falls back to UPnP — it's the one `PortMapperTask` holds in production.

```rust
#[async_trait]
pub trait PortMapperClient: Send + Sync {
    /// Short-deadline reachability check. Returns `Ok(())` if the
    /// protocol is available on the gateway.
    async fn probe(&self) -> Result<(), PortMappingError>;

    /// Request a mapping for `internal_port` with the given TTL.
    /// Returns the external `SocketAddr` the router gave us.
    async fn install(
        &self,
        internal_port: u16,
        ttl: Duration,
    ) -> Result<PortMapping, PortMappingError>;

    /// Refresh an existing mapping. Re-installs under most
    /// protocols; semantically "keep my mapping alive."
    async fn renew(
        &self,
        mapping: &PortMapping,
    ) -> Result<PortMapping, PortMappingError>;

    /// Ask the router to drop the mapping. Best-effort —
    /// failures here are logged + ignored.
    async fn remove(&self, mapping: &PortMapping);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol { NatPmp, Upnp }

#[derive(Debug, Clone, Copy)]
pub struct PortMapping {
    pub external: SocketAddr,
    pub internal_port: u16,
    pub ttl: Duration,
    pub protocol: Protocol,
}
```

**Alternative considered:** single unified client that tries protocols internally. Rejected — couples the protocols, harder to mock individual protocol failures for the fallback test.

### 2. Gateway discovery: lean on the libs; optional `default-net` for NAT-PMP

UPnP-IGD uses SSDP multicast for discovery — no gateway IP needed upfront. NAT-PMP needs the default gateway's IPv4 address to send the 2-byte probe packet.

Because we inlined the wire codec (decision 10), there's no `rust-natpmp` helper to lean on for gateway discovery; stage-4b-4's sequencer parses the OS routing table itself (unix: `netstat -rn` / `ip route`; macOS: `route get default`) or accepts an operator-supplied gateway when auto-detection fails. On platforms where auto-detection fails we degrade gracefully by skipping NAT-PMP and falling through to UPnP.

**Decision:** No explicit `default-net` dep in stage 4b. The sequencer's own routing-table reader is sufficient for unix + macOS, and Windows can rely on UPnP. Revisit if we see a non-trivial number of Windows-only deployments where UPnP is disabled but NAT-PMP would work.

### 3. Task lifecycle: 5-state machine with single-attempt install

```text
Idle ──start()──▶ Probing ──ok──▶ Installed ──tick──▶ Renewing
 ▲                   │                │                   │
 │                   │fail            │fail×3             │ok
 │                   ▼                ▼                   ▼
 └────(no retry)── Exited ◀─────── Revoked ◀──────── Installed
                    ▲                │                    │
                    └────shutdown────┴────────────────────┘
```

- **Idle** — task not yet spawned.
- **Probing** — run `probe()` against NAT-PMP, then UPnP. On success move to Installing (elided in the diagram) then Installed. On both failing, log + transition to Exited (no further attempts).
- **Installed** — mapping is live; `set_reflex_override(external)` has been called. A 30-min renewal timer is armed.
- **Renewing** — every tick, call `renew()`. On success back to Installed. On 3 consecutive failures, transition to Revoked.
- **Revoked** — mapping is dead (either renewal failure or shutdown). Call `remove()` + `clear_reflex_override()`. Task exits.

**Decision:** Install + probe are a one-shot. The parent plan's "best-effort side quest" framing (decision 5) locks this in — operators who want the mesh to hang until a mapping lands can implement that at the application layer; the traversal module treats mapping as a discretionary upgrade.

### 4. Deadline policy: per-call, no per-task

`probe` and `install` each respect a per-protocol deadline (1 s NAT-PMP, 2 s UPnP — parent §4 values). `renew` uses the same deadlines. No aggregate task deadline: the task runs for the mesh's lifetime.

**Decision:** Single tunable `TraversalConfig::port_mapping_renewal` (already exists, default 30 min). No separate install deadline knob in stage 4b — the per-protocol defaults are inside the clients.

### 5. Renewal failure: 3-strike revoke matches the failure detector

Transient renewal failures happen — Wi-Fi blip, router CPU spike, UPnP's `ERROR_CODE_204` on a busy gateway. Don't revoke on the first miss.

**Decision:** Count consecutive renewal failures. On the third, transition to Revoked — call `clear_reflex_override()` so the mesh stops advertising a possibly-dead external address, then call `remove()` (best-effort) and exit. The classifier takes over from there.

Rationale: matches the `FailureDetector`'s miss_threshold = 3 pattern used elsewhere in the mesh. Symmetric operator intuition.

### 6. Race with the stage-4a runtime setter: last-writer-wins

The port-mapper task calls `set_reflex_override(x)` on install. An operator can concurrently call `set_reflex_override(y)` from any binding. Whose value wins?

**Decision:** Last-writer-wins. The stage-4a setter is a pure atomic update, so there's no state corruption — whichever call lands last pins the reflex. The port-mapper's next renewal tick will re-stamp its own value, but that's correct behavior: the operator set `y` knowing the port-mapper was running; the router's lease still says the port-mapper's mapping is the real one.

**Non-decision:** don't track an "override source" enum (`Operator | PortMapper`). Adds complexity for no user-visible benefit — if the operator doesn't want the port-mapper scribbling over their override, they disable `try_port_mapping`.

### 7. Shutdown ordering: revoke before mesh exit

`MeshNode::shutdown` signals the port-mapper task via the existing `shutdown_notify`. The task:

1. Cancels its renewal timer.
2. Calls `remove(&mapping)` with a short deadline (same as probe — 1 s / 2 s).
3. Calls `clear_reflex_override()` unconditionally.
4. Exits.

Per parent decision 12, a crash leak is acceptable: if step 2 times out or the process dies before reaching it, the router keeps the mapping for its TTL (3600 s) and reclaims automatically.

### 8. Observability: three new `TraversalStats` fields

Add to `TraversalStatsSnapshot`:

```rust
pub struct TraversalStatsSnapshot {
    // ... existing fields ...
    /// True when a port mapping is currently installed and
    /// advertised via `set_reflex_override`.
    pub port_mapping_active: bool,
    /// The external `SocketAddr` a successful port mapping
    /// installed, or `None` when no mapping is active.
    pub port_mapping_external: Option<SocketAddr>,
    /// Count of successful renewal ticks. Distinguishes a
    /// freshly-installed mapping from a long-lived one.
    pub port_mapping_renewals: u64,
}
```

Behind `#[cfg(feature = "nat-traversal")]` (not `port-mapping`) so code compiled with traversal but not port-mapping still gets the struct, just with `port_mapping_active = false` always.

### 9. Testing: mock client + no-router graceful degradation + `#[ignore]` real-router e2e

**Unit tests (mockable):**

- `MockPortMapperClient` with pre-programmed response queue per verb. Tests verify:
  - On install success: `set_reflex_override(external)` is called; stats flip.
  - On renewal success: renewal counter bumps; override stays pinned.
  - On 3 consecutive renewal failures: `clear_reflex_override()` is called; task exits.
  - On shutdown: `remove()` is called before `clear_reflex_override()`.
  - On install failure (NAT-PMP + UPnP both unavailable): no override set; classifier runs normally.

**Integration tests (CI-safe):**

- `NullPortMapper` always returns `Unavailable`. A mesh built with `try_port_mapping(true)` + the null mapper proceeds through boot in under 1 s, never calls `set_reflex_override`, and exposes `port_mapping_active = false`.

**Real-router manual tests (`#[ignore]`d):**

- `tests/port_mapping_real_router.rs` with tests marked `#[ignore]`. Runs locally by `cargo test -- --ignored` against the developer's own UPnP-capable router. Asserts: install succeeds, external IP matches the router's, revoke on shutdown removes the mapping (verified via a second probe).

### 10. Dependency review at implementation time

The parent plan names `igd-next` (MIT, actively maintained, ≈150 KB) and `rust-natpmp` (MIT, sparse maintenance, ≈50 KB, stable wire format). Resolved outcomes at landing time:

- **`igd-next`:** kept as-is; `port-mapping` feature adds `dep:igd-next`.
- **`rust-natpmp`:** **rejected.** NAT-PMP wire format (RFC 6886) is ~100 lines — we inlined it in `portmap/natpmp.rs` with its own `encode_request` / `decode_response` + unit tests. Benefits: one less dep (no other user in-tree), explicit control over the kernel-level source-address filter (RFC 6886 §3.1 mandate, implemented via `UdpSocket::connect`), and no coupling to a sparsely-maintained external crate. Alternatives considered and not needed: (1) pinning to a last-known-good version, (2) vendoring into `crates/net/third-party/natpmp/`.

---

## Implementation stages

Break stage 4b into five sub-stages — same shape as stage 3 (3a/3b/3c/3d) so each is independently shippable + testable.

### Stage 4b-1 — Trait + task scaffolding + null mapper

- `traversal/portmap.rs` module with the trait + struct + error enum from decision 1.
- `NullPortMapper` impl (always `Unavailable`).
- `PortMapperTask` state machine (decision 3). Driven by a `tokio::select!` loop over the renewal timer + shutdown_notify.
- `MeshNodeConfig::try_port_mapping: bool` field (default `false`).
- Spawn the task from `MeshNode::start()` when the flag is set, holding a `Box<dyn PortMapperClient>` (starts as `NullPortMapper`; stage 4b-4 swaps in the real sequencer).
- `TraversalStatsSnapshot` extensions (decision 8).
- Mock-based unit tests covering the full lifecycle per decision 9.
- Integration test: `try_port_mapping(true)` with null mapper boots cleanly.

No new external deps. Testable entirely in-process.

### Stage 4b-2 — NAT-PMP client

- Wire format inlined in `portmap/natpmp.rs` (decision 10 resolved): `encode_request` / `decode_response` covering the RFC 6886 external-address + map-UDP shapes, plus a `ResultCode` enum.
- `NatPmpMapper` impls `PortMapperClient`, using `UdpSocket::connect` to pin the kernel-side source-address filter per RFC 6886 §3.1 (rejects spoofed responses from non-gateway hosts).
- Gateway discovery handled by the sequencer at 4b-4 time — the mapper accepts a known gateway `Ipv4Addr` at construction.
- Manual test against a NAT-PMP-capable router (most consumer routers support one of the two; test setup: pfSense or a Unifi Dream Router).

Feature-gate: behind `port-mapping` cargo feature.

### Stage 4b-3 — UPnP client

- Add `igd-next` dep.
- `UpnpMapper` impls `PortMapperClient`. `install` → `AddPortMapping`; `remove` → `RemovePortMapping`; `renew` → re-run `AddPortMapping` (IGD spec allows it as a refresh).
- Manual test against a UPnP-capable router.

Feature-gate: `port-mapping`.

### Stage 4b-4 — Sequencer + task integration

- `SequentialMapper` that owns a `NatPmpMapper` + `UpnpMapper` and probes each in order. On first success, subsequent calls route to the winning protocol; no mid-session protocol switch.
- Wire it into `MeshNode::start()` instead of `NullPortMapper` when `try_port_mapping(true)`.
- Real-router integration test (gated `#[ignore]`).

### Stage 4b-5 — Binding surface parity

Mirror the stage-4a pattern — expose `try_port_mapping` across all four surfaces:

- **SDK:** `MeshBuilder::try_port_mapping(bool)`.
- **NAPI:** `tryPortMapping?: boolean` on `MeshOptions`.
- **PyO3:** `try_port_mapping=None` kwarg.
- **Go:** `TryPortMapping bool` on `MeshConfig` (JSON: `try_port_mapping`).

No new methods on the mesh — the flag is a constructor-time decision. Stage-4a's runtime override setters are reused for install/revoke signalling.

---

## Critical files

- `adapter/net/traversal/portmap.rs` — new module. Trait, task, state machine, error enum, null + mock mappers.
- `adapter/net/traversal/portmap/natpmp.rs` — NAT-PMP client (inlined RFC 6886 wire codec + `NatPmpMapper`).
- `adapter/net/traversal/portmap/upnp.rs` — UPnP client.
- `adapter/net/traversal/mod.rs` — re-export `PortMapperClient`, `PortMapping`, `PortMappingError`, `Protocol`.
- `adapter/net/traversal/config.rs` — no change (`port_mapping_renewal` already exists).
- `adapter/net/mesh.rs` — `MeshNodeConfig::try_port_mapping` field + `with_try_port_mapping` setter; task spawn in `start()`; `TraversalStatsSnapshot` fields.
- `Cargo.toml` — `port-mapping` feature gains `dep:igd-next`. NAT-PMP is inlined (decision 10), so no `rust-natpmp` dep.
- `sdk/src/mesh.rs`, `bindings/node/src/lib.rs`, `bindings/python/src/lib.rs`, `bindings/go/net/mesh.go` — `try_port_mapping` parity (stage 4b-5).

---

## Exit criteria

Per parent §4 plus stage-specific additions:

- **LAN + UPnP:** `try_port_mapping(true)` boot completes with `nat_type() == "open"`, `reflex_addr() == Some(router.external_ip:bind_port)`, and `traversal_stats().port_mapping_active == true` within 3 s.
- **LAN without UPnP / NAT-PMP:** boot completes in under 5 s with `port_mapping_active == false`; classifier runs; `nat_type()` eventually reflects real classification.
- **Graceful shutdown:** `MeshNode::shutdown()` removes the mapping before exit. Verified by a second probe from a separate process — the mapping is gone from the router's table within a second of shutdown return.
- **Renewal failure:** simulate via mock mapper injecting 3 consecutive `Unavailable` on `renew()`. Task calls `clear_reflex_override()`; `reflex_addr()` returns to `None`; `port_mapping_active` flips to `false`; task exits cleanly.
- **All binding surfaces:** `try_port_mapping` can be set from SDK, NAPI, PyO3, and Go constructors. Setting to `false` (or omitting) means no port-mapper task is spawned; stage-4a override setters remain usable.

---

## Open questions

- **Does our IPv6 story change?** UPnP-IGD v2 supports IPv6 (WANIPv6FirewallControl). NAT-PMP is IPv4-only by spec; PCP (RFC 6887) supports IPv6 mappings via its MAP/PEER model, while `ANNOUNCE` is for rapid recovery. We still don't speak PCP. For the MVP we target IPv4 only; IPv6 on a dual-stack node with separate IPv6 reachability falls into the "already NatType::Open" bucket per parent §Non-goals. Flag if practical deployments surface a mismatch.
- **Should the task continue running after probe failure?** Current plan (decision 3) says no: both protocols fail → task exits. Alternative: leave it dormant, re-probe on a slow cadence (e.g., every hour) so a router that gets UPnP turned on mid-session can be picked up. Decision: no — matches "best-effort side quest" framing; operators wanting this can toggle `try_port_mapping` via a restart.
- **Do we need a metric for time-to-map?** Currently just `port_mapping_active` + a renewals counter. A histogram of probe-to-install latency could surface flaky routers. Decision: add if an operator asks; not load-bearing for correctness.

---

## Rough estimates

| Sub-stage | Scope                              | Complexity       | Estimate  |
|-----------|------------------------------------|------------------|-----------|
| 4b-1      | Trait + task + null + mock + tests | Medium           | ~1.5 days |
| 4b-2      | NAT-PMP client                     | Small–medium     | ~1 day    |
| 4b-3      | UPnP client                        | Small–medium     | ~1 day    |
| 4b-4      | Sequencer + real-router test       | Small            | ~0.5 day  |
| 4b-5      | Binding surface parity             | Small            | ~0.5 day  |

Total: ~4.5 days serial. 4b-1 is the only sub-stage that's strictly blocking — the others can proceed in parallel once the trait is locked.

---

## Dependencies

- `igd-next` ≈ 150 KB, MIT. Gated by `port-mapping` feature.
- `rust-natpmp`: **not used.** Decision 10 resolved to inline the RFC 6886 wire codec directly in `portmap/natpmp.rs` (~100 lines + unit tests), eliminating the dep entirely.

Stage 4b-1 adds no external deps — it's pure module + task scaffolding. Stage 4b-3 introduces the sole port-mapping dep (`igd-next`).

---

## Out of scope (for this plan)

- Per-subprotocol port mappings (e.g., a separate mapping for the reflex subprotocol).
- PCP-specific features (`PEER` opcodes, `ANNOUNCE`, filtering).
- Detecting gateway changes mid-session (laptop switches networks) — re-probe would be a separate design.
- `STUN`-style address refresh after a rebind — that's a stage-3 classification concern, not a port-mapping one.
- IPv6 port mapping (see Open questions).
- Operator policies for "prefer UPnP over NAT-PMP" — the sequencer's default order is NAT-PMP first (faster probe, smaller wire); operators can flip the protocols by disabling one side, a follow-up if needed.
