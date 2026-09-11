# S1 — `PeerAddr` endpoint generalization + UDP-preserving `PeerSink`

Stage 1 of [`BROWSER_NATIVE_WEBRTC_TRANSPORT_PLAN.md`](../plans/BROWSER_NATIVE_WEBRTC_TRANSPORT_PLAN.md)
(§Stage 1, §1), implementing the seam contract of
[`S0D_SEND_INGRESS_INVENTORY.md`](S0D_SEND_INGRESS_INVENTORY.md).

**Implementation only** — not acceptance, not merge, no continuation to
Stage 2.

## 1. Commit range and feature set

Base: `ad874ff43` (Stage 0 complete). The export-baseline commit
`3f73e04f4` was already on the branch and is untouched.

| Commit | What |
|---|---|
| `2db4406aa` | `PeerAddr` + `PeerSink` introduced in `transport.rs`, re-exported from `adapter/net/mod.rs`; no callers |
| `a55b8c78b` | Peer-keyed state, `dispatch_packet` source typing, every send site on `PeerSink`, scheduler/`router.rs` plumbing |
| `bb34926d0` | `cargo fmt -p net-mesh` reflow + `MeshNode::set_peer_addr_for_test` (production-side) |
| `57654b685` | **Test and witness edits only** |
| `96dc9fdf4` | Linux `batched-ingress` argument fix + rustdoc private-link fix (production) |
| `d65731727` | Witness field shorthand after the `relay` rename (test-side) |

Candidate: **`d65731727`**.

`UNIT_FEATURES` as pinned in `.github/workflows/ci.yml` and used verbatim
for every `--features "$UNIT_FEATURES"` command below:

```
net redex redex-disk cortex netdb meshdb meshos dataforts nat-traversal port-mapping tool batched-ingress cli regex
```

## 2. `PeerSink` as implemented

`net/crates/net/src/adapter/net/transport.rs`:

```rust
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum PeerAddr {
    Udp(SocketAddr),
}
impl PeerAddr { pub fn udp(&self) -> Option<SocketAddr>; }
impl From<SocketAddr> for PeerAddr;
impl std::fmt::Display for PeerAddr;   // renders the inner SocketAddr unchanged

#[derive(Clone)]
pub struct PeerSink { udp: Arc<NetSocket> }

impl PeerSink {
    pub fn new(udp: Arc<NetSocket>) -> Self;
    pub fn udp_socket(&self) -> &Arc<NetSocket>;
    pub async fn send(&self, packet: &[u8], to: PeerAddr) -> io::Result<usize>;
    pub fn try_send(&self, packet: &[u8], to: PeerAddr) -> io::Result<usize>;
    pub async fn send_bounded(&self, packet: &[u8], to: PeerAddr, deadline: Duration)
        -> Result<(), AdapterError>;
}
```

No `FromStr`, no serde, no `to_socket_addr()`. `udp()` is the single
conversion back, used at the boundaries that genuinely need a tuple
(socket binding, `is_loopback`/partition checks, traversal input, reflex
publication, `accept`'s caller-facing return).

- **Where `bound_datagram_send` went:** it stayed in `transport.rs`
  (moved there from `mesh.rs` in `2db4406aa`) as `pub(crate)`, now taking
  `PeerAddr` for its log/error fields. `PeerSink::send_bounded` is
  exactly `bound_datagram_send(udp.send_to(..), to, deadline)`. Keeping
  it addressable matters for rows 4/5 — see §6, deviation 1.
- **How `MeshNode` holds it:** `MeshNode` keeps `socket: Arc<NetSocket>`
  **and** `sink: PeerSink` beside it (`mesh.rs:10481-10486`). The sink
  wraps the same `Arc`; the socket field remains for the receive loops,
  `local_addr`, the two handshake `recv_from` bypasses, and the
  `BatchedPacketReceiver`. `DispatchCtx` carries the sink the same way.
  Spawned tasks clone the sink (it is `Clone`), exactly as they cloned
  the socket `Arc` before.
- `NetRouter` keeps its own `Arc<UdpSocket>` (it binds its own ephemeral
  send socket); `NetRouter::send_to` and the scheduler drain take
  `PeerAddr` and match `Udp` at the call boundary. That is the UDP-only
  primitive surface the brief froze, now endpoint-typed on the way in.

## 3. The S0d row table, with what each row became

Line numbers in the "became" column are at the candidate commit.
"`send`", "`try_send`", "`send_bounded`" are `PeerSink` entry points.

### 3.1 `mesh.rs` direct socket sends (rows 1–37)

| Row | S0d site | Function | Became |
|---|---|---|---|
| 1 | `mesh.rs:515` | `emit_control_chunks` | `send` — `sink.send(&packet, addr)` (`:513`) |
| 2 | `:4827` | `flood_event_pingwave_rounds` | `send` (`:4830`), pre-await `Vec<PeerAddr>` snapshot unchanged |
| 3 | `:5070` | `run_route_withdrawal_flood` | `send` (`:5073`) |
| 4 | `:6797` | `consume` (org egress, fixtures stall arm) | `bound_datagram_send(sink.send(..), next.addr, deadline)` (`:6799`) — queue's own `deadline`; see deviation 1 |
| 5 | `:6806` | `consume` (production arm) | `bound_datagram_send(sink.send(..), next.addr, deadline)` (`:6804-6809`) |
| 6 | `:7815` | `spawn_sensing_frame_send` | `send` inside the spawned task (`:7814`) |
| 7 | `:9317` | `send_datagram` (funnel) | `send_bounded(packet, addr, DATAGRAM_SEND_DEADLINE)` (`:9286-9288`) |
| 8 | `:24568` | `dispatch_packet` — pingwave re-flood | **`try_send`** (`:24560`), shed comment kept |
| 9 | `:24751` | `dispatch_packet` — legacy routed relay | **`try_send`** (`:24743`), SEC-07 shed comment kept |
| 10 | `:25057` | `relay_protected_hop` | **`try_send`** (`:25049`), shed comment kept |
| 11 | `:25479` | `handle_routed_handshake` | `send` inside the spawned task (`:25471`); rollback guard still travels into the task |
| 12 | `:25741` | `process_local_packet` — migration | `send` (`:25733`) |
| 13 | `:25792` | `process_local_packet` — migration reply | `send` (`:25784`) |
| 14 | `:25906` | `process_local_packet` — retransmission | `send` (`:25898`), destination is still `parsed.source` |
| 15 | `:26399` | `process_local_packet` — reflex response | `send` (`:26396`) |
| 16 | `:27187` | `spawn_stream_grant_drainer_loop` | `send` (`:27184`) |
| 17 | `:27287` | `spawn_retransmit_loop` | `send` (`:27284`), pre-send `work: Vec` snapshot unchanged |
| 18 | `:27554` | `spawn_heartbeat_loop` — heartbeat | `send` (`:27551`) |
| 19 | `:27556` | `spawn_heartbeat_loop` — pingwave | `send` (`:27553`) |
| 20 | `:28397` | `send_routed` (split arm) | `send` (`:28394`) |
| 21 | `:28425` | `send_routed` (tail arm) | `send` (`:28422`) |
| 22 | `:33536` | `handle_punch_request` — rejection | `send` (`:33533`) |
| 23 | `:33706` | `handle_punch_request` — introduce → A | `send` (`:33703`) |
| 24 | `:33726` | `handle_punch_request` — introduce → B | `send` (`:33723`) |
| 25 | `:33958` | `schedule_punch` — punch train | **frozen**: raw `NetSocket::send_to(.., peer_reflex)` (`:33957`); reflex tuple, not a peer endpoint (§6). Deviation 2 |
| 26 | `:33992` | `schedule_punch` — punch train (resend) | **frozen**, same (`:33991`) |
| 27 | `:34054` | `schedule_punch` — `PunchAck` to coordinator | `send` (`:34060`) |
| 28 | `:34104` | `forward_punch_ack` | `send` (`:34110`) |
| 29 | `:34860` | `send_membership_ack` | `send` (`:34866`) |
| 30 | `:35503` | `try_publish_to_peer` | `send` (`:35509`); `guard.commit()` still on the synchronous side of the await |
| 31 | `:37543` | `send_transfer_control` | `send` (`:37559`) |
| 32 | `:39387` | `deliver_stream_packet` (unscheduled) | `send` (`:39406`), still mapped to `StreamError::Transport` |
| 33 | `:39380` | `deliver_stream_packet` (scheduled) | unchanged `scheduler().enqueue`; `QueuedPacket::dest` is now `PeerAddr` (`:39403`). Still the only `Backpressure` producer |
| 34 | `:39566` | `try_connect_via_once` | `send` (`:39585`) |
| 35 | `:40321` | `try_handshake_initiator` | `send` (`:40346`) |
| 36 | `:40377` | `try_handshake_initiator` (second arm) | `send` (`:40402`) |
| 37 | `:40747` | `try_handshake_responder` | `send` (`:40774`), destination still the packet `source` |

### 3.2 Rows that funnel through #7 (38–42)

| Row | S0d site | Function | Became |
|---|---|---|---|
| 38 | `mesh.rs:28302` | `send_to_peer_node` (split arm) | `send_datagram(&self.sink, ..)` → `send_bounded` (`:28299`) |
| 39 | `:28323` | `send_to_peer_node` (tail arm) | same (`:28320`) |
| 40 | `:33389` | `forward_scoped_announcement` | `send_datagram(&sink, ..)` (`:33386`) |
| 41 | `:33443` | `forward_capability_announcement` | `send_datagram(&sink, ..)` (`:33440`) |
| 42 | `:35820` | `send_subprotocol_to_node` | `send_datagram(&self.sink, ..)` (`:35836`) |

### 3.3 Single-peer adapter and proxy (43–48)

| Row | S0d site | Function | Became |
|---|---|---|---|
| 43 | `mod.rs:934` | `try_handshake_initiator` | **frozen** — `config.peer_addr` is operator-typed `SocketAddr`; raw `socket.send_to` |
| 44 | `mod.rs:1138` | `try_handshake_responder` | **frozen** — packet `source` from the adapter's own `recv_from`, `SocketAddr` |
| 45 | `mod.rs:1407` | `spawn_heartbeat` | **frozen** |
| 46 | `mod.rs:1553` | `on_batch` (split arm) | **frozen** |
| 47 | `mod.rs:1603` | `on_batch` (tail arm) | **frozen** |
| 48 | `proxy.rs:357` | `forward_and_send` | **frozen** (Kyra decision 1) — imports/types untouched; `NetProxy` keeps its own socket, next-hop map and `SocketAddr` |

### 3.4 Primitives and the router drain (49–56)

| Row | S0d site | Function | Became |
|---|---|---|---|
| 49 | `proxy.rs:407` | `NetProxy::send_to` | **frozen** |
| 50 | `router.rs:909` | `NetRouter::send_to` | endpoint-typed: `send_to(&self, data, dest: PeerAddr)`, matches `Udp` → `socket.send_to` (`router.rs:917-922`) |
| 51 | `router.rs:984` | scheduler drain, depth-0 fast path | `match first.dest { PeerAddr::Udp(addr) => socket.send_to(..) }` (`:997-1001`); fast path otherwise unchanged |
| 52 | `router.rs:1028` | Linux `sendmmsg` tail | `let PeerAddr::Udp(dest) = *dest;` before the cfg arms, then the unchanged `send_batch` + tail `send_to` (`:1033-1049`) |
| 53 | `router.rs:1034` | non-Linux group send | same partition, unchanged per-packet `send_to` (`:1055`) |
| 54 | `router.rs:895` | `route_packet` enqueue | `QueuedPacket { dest: next_hop }` where `next_hop: PeerAddr` (`:889-895`); `RouterError::QueueFull` + drop accounting unchanged |
| 55 | `transport.rs:259/265/280` | `NetSocket::{send_to, send, try_send_to}` | unchanged `SocketAddr` primitives — they are now `PeerSink`'s implementation |
| 56 | `transport.rs:699/705/711` | `PacketSender::{send_to, send, try_send_to}` | unchanged; no production caller, kept as the primitive surface |

`group_by_dest`, `reset_dest_groups`, `MAX_DRAIN = 64`, the depth-0 fast
path, `record_batch_flush` / `record_drain_run` and the whole-drain
instrumentation are unchanged apart from the `PeerAddr` key type
(`router.rs:129-198`).

### 3.5 The 10 leak rows

| Leak | S0d | Became |
|---|---|---|
| Route entry, row 9 | `mesh.rs:24699` lookup | `RoutingTable::lookup(dest_id) -> Option<PeerAddr>`; `try_send` consumes it directly |
| Route entry, row 11 | `:25213` lookup, else `source` | both sides are `PeerAddr` now (`RouteEntry::next_hop`, `ParsedPacket::source`); the `unwrap_or(source)` fallback is unchanged |
| Route entry, row 30 | `:35497` lookup, else `dest_addr` | same shape, `PeerAddr` throughout |
| Route entry, row 54 | `router.rs:889` | `next_hop: PeerAddr` → `QueuedPacket::dest: PeerAddr` |
| Route entry → rows 51–53 | `QueuedPacket::dest` | `PeerAddr`, variant-partitioned at the drain |
| Packet source, row 11 fallback | `:25217` | `ParsedPacket::source: PeerAddr` |
| Packet source, row 14 | `:25902` | `PeerAddr` |
| Packet source, row 37 | `mesh.rs:40747` | `PeerAddr` |
| Packet source, row 44 | `mod.rs:1138` | **stays `SocketAddr`** — the single-peer adapter is frozen; `NetAdapter::process_packet(.., source: SocketAddr, ..)` is unchanged |
| Inbox keying, I9 | `pending_direct_initiators` | `DashMap<PeerAddr, Arc<DirectHandshakeInbox>>`; `DirectHandshakeRegistry` follows |

Reflex/traversal rows 25–26 and config row 43 stayed `SocketAddr` as
S0d predicted. `route.rs` (`RouteEntry`, `RoutingTable`, every
`add_route*` / `lookup*` / `migrate_next_hop` / `rebind_*`),
`reroute.rs` (`peer_addrs`, `ReroutePolicy`), `failure.rs`
(`FailureDetector` heartbeat/eviction keys), `swarm.rs`
(`NodeInfo::addr`), `behavior/proximity.rs` and
`behavior/fold/{routing,capability}.rs` followed those values, as
budgeted.

## 4. Test and witness files touched

All in `57654b685` except the two noted; edit class per file.

| File | Class |
|---|---|
| `src/adapter/net/mesh.rs` (test modules) | signature / type-annotation, **plus three source-text pins — see below** |
| `src/adapter/net/org_routing_wiring_tests.rs` | signature / type-annotation (`seed_peer`, `flip_peer_transport`, `PeerTransport` field names) |
| `src/adapter/net/mod.rs` (tests) | type-annotation; `NetSession::new` takes `PeerAddr::Udp(source)` while `process_packet` keeps the adapter's `SocketAddr` |
| `src/adapter/net/{route,reroute,router,session,swarm,transport}.rs` (tests) | type-annotation |
| `src/adapter/net/behavior/{proximity,placement,meshos/probes}.rs` (tests) | type-annotation |
| `benches/{mesh,net}.rs` | type-annotation |
| `tests/three_node_integration.rs`, `tests/route_withdraw.rs`, `tests/capability_multihop.rs`, `tests/event_pingwave.rs`, `tests/routed_transport_availability.rs`, `tests/parsed_packet_short_input.rs`, `tests/subnet_gateway_auth.rs`, `tests/sensing_{fallback,resolver,relay_delivery,e2e_registration,origin_emitter,routed_origin}.rs` | signature / type-annotation at construction sites (`PeerAddr::Udp(..)`), `local_addr()`/`MeshNodeConfig` kept `SocketAddr` |

**"Other" — three source-text witnesses, each one sentence:**

1. `protected_forward_allocation_pins::relay_protected_hop_does_not_allocate_per_packet`
   asserted the body contains `try_send_to(`; the relay egress is now
   `ctx.sink.try_send(`, so the pin looks for `try_send(` — same
   property (non-blocking shed, never a spawn), renamed surface.
2. `protected_forward_allocation_pins::unauthenticated_forwarding_paths_shed_instead_of_spawning`
   — identical change, plus its error text now names the sink's
   non-blocking `try_send` instead of `socket.try_send_to`.
3. `heartbeat_aead_tests::connect_direct_upgrade_refreshes_addr_to_node`
   matched the literal `self.addr_to_node.insert(target_addr, peer_node_id)`;
   the conversion made that
   `.insert(PeerAddr::Udp(target_addr), peer_node_id)` (and rustfmt split
   the receiver onto its own line), so the pin matches the new literal —
   the assertion still fails if the insert is removed.

No assertion was weakened, no witness renamed or deleted, no test
removed. `d65731727` additionally replaced two `relay: relay` witness
initializers with the shorthand (`clippy::redundant_field_names` under
the all-targets lint).

## 5. Validation

From `net/crates/net`, at `d65731727`.

| Command | Result |
|---|---|
| `cargo fmt --all -- --check` | **not runnable on this host** — `cargo fmt --all` fails with `os error 206` (command line too long) on Windows, before rustfmt runs. Ran `cargo fmt -p <member> -- --check` for `net-mesh`, `net-mesh-sdk`, `net-node`, `net-python`, `net-mesh-mcp`, `net-payments`, `net-cli`: **all pass** |
| `cargo check --workspace --all-targets` | pass (only pre-existing `net-node` dead-code warnings) |
| `cargo check --workspace --all-targets --all-features` | pass |
| `cargo check --target x86_64-unknown-linux-gnu ...` | **not runnable** — `ring v0.17.14`'s build script needs `x86_64-linux-gnu-gcc`; no C toolchain (gcc/clang/zig/WSL/docker) on this host. Mitigation in §7 |
| `cargo clippy --all-features --lib --bins -- -D warnings` | pass |
| `cargo clippy --lib --bins -- -D warnings` | pass |
| `cargo clippy --no-default-features --lib --bins -- -D warnings` | pass |
| `cargo clippy --all-features --all-targets -- -D warnings -A unwrap_used -A expect_used -A undocumented_unsafe_blocks -A multiple_unsafe_ops_per_block` | pass |
| `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features` | pass |
| `cargo test --lib --features "$UNIT_FEATURES"` | **ok. 5975 passed; 0 failed; 1 ignored** |
| `cargo test --doc --features "$UNIT_FEATURES"` | **ok. 4 passed; 0 failed; 31 ignored** |
| `cargo clippy -p net-mesh-sdk --all-targets --features full -- -D warnings` | pass |
| `RUSTDOCFLAGS="-D warnings" cargo doc -p net-mesh-sdk --no-deps --features full` | pass |
| `cargo clippy -p net-node --lib -- -D warnings` | pass |
| `cargo clippy -p net-python --lib -- -D warnings` | pass |
| `go test ./...` | **not run** — no C toolchain, so cgo cannot build; CI runs it |

### Witness floors

Counted with `cargo test --lib --features "$UNIT_FEATURES" <filter>`,
the exact filters from `ci.yml`:

| Filter | Floor | Ran |
|---|---|---|
| `org_routing_wiring_tests` | 93 | **93** |
| `behavior::org_routing::` | 24 | **24** |
| `behavior::org_routing_registry::` | 62 | **62** |
| `behavior::org_routing_state::` | 41 | **41** |
| `adapter::net::behavior::sensing::org_gate::tests::` | 60 | **60** |
| `adapter::net::mesh::sensing_authority_witness_tests::` | 67 | **68** |

`a_send_in_flight_retains_no_peer_shard` and the rest of the REQUIRED
name list are inside those runs and pass.

### Integration families (nextest, CI's own `--test` lists and features)

| Family | Features | Result |
|---|---|---|
| Net mesh / capability / subnets / migration / nRPC dispatch (48 binaries, incl. `three_node_integration`, `route_withdraw`, `parsed_packet_short_input`, `capability_multihop`, `event_pingwave`, `routed_transport_availability`, `subnet_*`, `cross_lang_capability_fixtures`) | `net fixtures` | **470 passed, 0 failed** |
| CortEX + nRPC + AI tools (30 binaries, incl. `integration_nrpc_cross_lang`, `integration_nrpc_cross_lang_streaming`) | `cortex tool fixtures` | **279 passed, 0 failed** |
| Sensing (14 binaries) | `cortex tool fixtures` | **65 passed, 0 failed** |
| NAT traversal (14 binaries, incl. `direct_upgrade`, `connect_direct`, `reflex_*`, `rendezvous_*`) | `net nat-traversal fixtures` | **84 passed, 0 failed** |
| Port mapping (2 binaries) | `net port-mapping` | **2 passed, 1 skipped** |
| RedEX (3 binaries) | `redex` | **47 passed, 1 skipped** |

All three cross-language golden-vector binaries
(`cross_lang_capability_fixtures`, `integration_nrpc_cross_lang`,
`integration_nrpc_cross_lang_streaming`) pass **unmodified** — zero wire
change.

### Export checker

```
cargo build --release -p net-ffi --features net-ffi/test-helpers
python .github/scripts/check-ffi-exports.py
  baseline generated-at: 1eb9ba7cc451a1c79c1a3b8c417773a0d11789c8
  baseline pinned-to: ad874ff433b89f20ce8813ecd8d6c93b2ec58f89 (net/ identical between pin and generated-at)
  baseline count: 568
✓ net.dll: export set matches the baseline
```

`exports.baseline` untouched.

## 6. Deviations, and what was noticed but not fixed

1. **Rows 4/5 call `bound_datagram_send(sink.send(..), ..)` rather than
   `PeerSink::send_bounded`.** The ordered organization egress queue has
   a fixtures-only arm that substitutes `std::future::pending()` for the
   send future to exercise the stall path, so the queue needs the
   *wrapper* addressable, not just the composed method. `send_bounded`
   is that exact composition, so the deadline, the
   `OrgEgressCounters`/`OrgEgressSendPhase` bookkeeping and the
   "never counted as sent" rule are byte-identical either way. Rows 4/5
   still use the queue's own `deadline` variable and row 7 still uses
   `DATAGRAM_SEND_DEADLINE`; they are not unified.
2. **Rows 25/26 stayed on the raw socket**, against S0d §3.1's
   `send`-range mapping. Their destination is `peer_reflex`, a
   server-reflexive tuple owned by traversal, and the brief freezes
   traversal at `SocketAddr` (§6, "convert at the boundary where a
   traversal result becomes a peer endpoint"). A punch train is never a
   peer-endpoint send, so routing it through the sink would have meant
   inventing `PeerAddr::Udp(reflex)` for an address that is not a peer
   endpoint. Flagging it because it is a real divergence from the row
   mapping, not a silent one: if review wants every datagram on the
   sink, this is a two-line change.
3. **`NetRouter::send_to` (row 50) was endpoint-typed** rather than left
   alone. S0d lists it as a primitive with no production caller, but it
   is `pub` on `NetRouter` and its sibling drain arms take `PeerAddr`;
   leaving it `SocketAddr` would have made the router's public send
   surface disagree with its own queue type. Behaviour is a single
   `match` arm over the same `socket.send_to`.
4. **`MeshNode::set_peer_addr_for_test` keeps its `SocketAddr`
   parameter** and converts once at entry. It is a `cfg(any(test,
   feature = "fixtures"))` seam that callers drive with literal
   addresses, so it is operator-typed by the same rule as
   `MeshNodeConfig::bind_addr`.
5. **`MeshNode` holds both `socket` and `sink`.** The brief left the
   shape open. A single field was not possible without either widening
   `PeerSink` with receive-side methods or making every receive/bind
   site go through `udp_socket()`; keeping both is the smaller change
   and makes "is this a send?" a grep for `sink`.
6. **Noticed, not fixed:** `bindings/node/src/{org,subnet,lib}.rs` carry
   ~18 pre-existing dead-code warnings (`install_org_authority`,
   `serve_org`, `OrgCaller`, `subnet_exports`, …) in the `lib test`
   profile. They predate this branch and are unrelated to the seam.
7. **Noticed, not fixed:** `cargo fmt --all` is unusable on Windows
   (`os error 206`, argument list length) — it fails in cargo before
   rustfmt starts, for any tree this size. Per-package `cargo fmt -p`
   works. Worth a line in CONTRIBUTING.md for Windows contributors;
   not this slice's job.

## 7. Did not go cleanly

- **The Linux-target type-check could not be run at all.** The brief
  budgets for it (§6) and it is the one check this host cannot do:
  `rustup target add x86_64-unknown-linux-gnu` succeeds, but
  `cargo check --target x86_64-unknown-linux-gnu` dies in `ring`'s build
  script for want of `x86_64-linux-gnu-gcc`, and there is no gcc, clang,
  zig, WSL distro or docker on the machine to supply one. That is not a
  theoretical gap: the `cfg(target_os = "linux", feature =
  "batched-ingress")` arm of `spawn_receive_loop` was **broken for one
  commit** — the mechanical rename of send-only socket locals to `sink`
  also rewrote `BatchedPacketReceiver::new(socket)` into
  `::new(sink)`, which no Windows build compiles. It was caught by
  reading every `cfg(target_os = "linux")` site by hand afterwards
  (`96dc9fdf4`), not by a compiler. The remaining Linux-gated code that
  the conversion touches is `router.rs`'s drain
  (`let PeerAddr::Udp(dest) = *dest;` hoisted above both cfg arms, so
  `send_batch` still gets a `SocketAddr`) and nothing else; `linux.rs`,
  `BatchedPacketReceiver` and the recv instrumentation were not touched
  and still speak `SocketAddr` end to end. CI is the first machine that
  will actually compile any of it.
- **`cargo fmt --all -- --check` is not a command that runs here** (see
  deviation 7). The candidate is formatted, but by seven per-package
  invocations, not the one the brief names.
- **`go test ./...` was not run** — same missing C toolchain; cgo
  silently skips its files with `CGO_ENABLED=0`, which AGENTS.md
  correctly calls meaningless, so it was not faked.
- **The conversion was compiler-driven and briefly very wide.** Peak was
  ~500 type errors across `mesh.rs`/`mod.rs`/`route.rs`/`reroute.rs`; it
  converged in batches, but "convert the declaration, then chase the
  call sites" produced two self-inflicted classes of damage worth
  recording: (a) a blanket `owned_addr:` → `owned:` / `relay_addr:` →
  `relay:` rewrite over a test region also renamed three *local
  bindings* whose later uses then dangled, and (b) an
  expression-wrapping pass that inserted `PeerAddr::Udp(` at a reported
  error column produced `"127.0.0.1:0".PeerAddr::Udp(parse().unwrap())`
  and `PeerAddr::Udp(assert_ne!()` in `three_node_integration.rs`. Both
  were caught by the next compile and repaired, but a reviewer should
  know the test diff was machine-generated and re-read rather than
  hand-written line by line.
- **Three source-text witnesses had to follow the rename** (§4). They
  are the only witnesses whose text changed for a reason other than a
  type annotation, and each one still fails if the property it pins is
  removed — but they are exactly the edits the "witness diffs are
  signature/annotation-only" acceptance criterion should be read against.
