# Stage 1 — `PeerAddr` endpoint generalization + UDP-preserving `PeerSink`

**Authorized by Kyra (2026-09-11) from `ad874ff43` — implementation only;
not acceptance, not merge, not Stage 2.** Source of truth, in this order:

1. `docs/internal/plans/BROWSER_NATIVE_WEBRTC_TRANSPORT_PLAN.md` §Stage 1
   (the authorization text, the three decisions, scope, stop condition,
   exit criteria) and §1 ("What S0d established", the seam contract).
2. `docs/internal/spikes/S0D_SEND_INGRESS_INVENTORY.md` — §1 the 56-row
   outbound table, §2 ingress, §3.1 the UDP-preserving contract and the
   row→entry-point mapping, §3.3 the drain split, §3.5 the 10 leak rows,
   §5 the caveats. **Every row is a preservation check.**
3. `AGENTS.md` — pre-push checklist, the feature-flag trap, witness
   floors, nextest flags.

The first Stage 1 commit already landed (`3f73e04f4`): the libnet export
baseline (`.github/scripts/check-ffi-exports.py`,
`bindings/go/net-ffi/exports.baseline`, 568 names, pinned to `ad874ff43`).
Your work must keep it green.

## Goal

Every peer endpoint the mesh keys on becomes `PeerAddr`; every outbound
packet leaves through one submission surface with three entry points; and
**nothing observable changes**: not a byte on the wire, not a deadline, not
an error mapping, not a batching decision, not a shed, not the exported
symbol set. This is the behaviour-neutral floor that Stage 3 builds the RTC
variant on.

## Target

`net/crates/net/src/adapter/net/`: `transport.rs`, `mesh.rs`, `mod.rs`,
`route.rs`, `reroute.rs`, `failure.rs`, `router.rs`, `session.rs`,
`swarm.rs`, `behavior/proximity.rs`, `behavior/fold/{routing,capability}.rs`
and whatever else the compiler drags in. Plus mechanical test edits, and the
FFI/bindings crates only if a signature they call changes (it should not —
`MeshNodeConfig::bind_addr` / `peer_addr` stay `SocketAddr`).

**Frozen — do not generalize (Kyra, decision 1):**

- `proxy.rs` (`NetProxy`): own socket, own next-hop map, own send path.
  Unchanged. Shared-type/import compatibility edits only. Rows 48 and F7
  are preservation checks, not instructions.
- Standalone UDP primitives: `transport.rs`'s connected-socket `send()`
  variants (`:265`, `:705`, no production caller) and the traversal
  sockets (`traversal/**`) keep `SocketAddr`. Convert at the boundary
  where a traversal result becomes a peer endpoint
  (`PeerAddr::Udp(addr)`), not inside traversal.
- Wire: nothing serializes a `PeerAddr`. `reflex_addr: Option<SocketAddr>`
  on the announcement stays. `ReflexMsg` / `RendezvousMsg` stay.
- Config: `MeshNodeConfig::{bind_addr, peer_addr}`, `reflex_override`,
  anything an operator types — `SocketAddr`.

## Change

### 1. `PeerAddr` (in `transport.rs`)

```rust
/// Where a peer is reached. Only `Udp` exists in this stage; the RTC
/// variant is Stage 3's and is feature-gated there.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum PeerAddr {
    Udp(SocketAddr),
}
```

Add `Display` (renders the inner `SocketAddr` unchanged, so every log line
and error string that formats a peer address is byte-identical), `From<SocketAddr>`,
and whatever accessor the code needs (`fn udp(&self) -> Option<SocketAddr>`
— keep the set small; do not add helpers speculatively). Do **not** implement
`FromStr` or serde.

### 2. `PeerSink` (in `transport.rs`) — the submission surface

One type, three entry points, UDP behaviour identical to today, per
S0d §3.1:

```rust
#[derive(Clone)]
pub struct PeerSink { udp: Arc<NetSocket> }   // Stage 3 adds the RTC half

impl PeerSink {
    /// Awaited. UDP: exactly today's `socket.send_to(..).await`.
    pub async fn send(&self, packet: &[u8], to: PeerAddr) -> io::Result<usize>;
    /// Non-blocking shed. UDP: exactly today's `try_send_to`.
    pub fn try_send(&self, packet: &[u8], to: PeerAddr) -> io::Result<usize>;
    /// Awaited under a caller-chosen deadline. UDP: today's
    /// `bound_datagram_send(socket.send_to(..), addr, deadline)`.
    pub async fn send_bounded(&self, packet: &[u8], to: PeerAddr, deadline: Duration)
        -> Result<(), AdapterError>;
}
```

The exact shape (free fns vs methods, where `bound_datagram_send`'s body
moves, whether `MeshNode.socket` becomes a `PeerSink` or sits beside one)
is yours, subject to: **one surface, three entry points, and the row
mapping in S0d §3.1** — `send` for rows 1–3, 6, 11–32, 34–37, 43–48,
51–53 (and primitives 49/50/55/56); `try_send` for rows 8, 9, 10 (the
three deliberate sheds — their comments reject queuing; keep the
comments); `send_bounded` for rows 4, 5, 7 (and via #7 rows 38–42).
Rows 4/5 use the org-egress queue's own deadline variable, row 7 uses
`DATAGRAM_SEND_DEADLINE` — do not unify them.

The 14 rows that spawn a task around the send keep that structure
(deadline/error handling/ordering live inside the task; row 11's rollback
guard travels into it). Row 33's scheduler `enqueue` stays the only
`Backpressure` producer; row 32's unscheduled arm still maps to
`StreamError::Transport`.

### 3. Peer-keyed state → `PeerAddr`

`PeerTransport::{Direct { owned: PeerAddr }, Routed { relay: PeerAddr, adjacent_relay_identity }}`
with `send_addr()` / `owned_addr()` / `is_direct()` unchanged in meaning;
`addr_to_node: DashMap<PeerAddr, u64>`; `RouteEntry::next_hop`;
`NetSession::peer_addr`; `ParsedPacket::source`; `NodeInfo::addr`;
`pending_direct_initiators`; the failure detector and reroute keys; the
partition filter; `dispatch_packet(data, source: PeerAddr, ctx)`;
`QueuedPacket::dest: PeerAddr` and the scheduler drain partitioned by
variant with only the `Udp` arm live (S0d §3.3: `group_by_dest` and the
`sendmmsg` flush unchanged for `Udp`; the depth-0 fast path unchanged;
`MAX_DRAIN` and the drain instrumentation stay whole-drain). The 10 leak
rows of S0d §3.5 are where `PeerAddr` crosses into `route.rs` /
`reroute.rs` / `failure.rs` — budget for them, and list them in your
report with what each became.

Where a `SocketAddr` is genuinely required at a boundary (binding a
socket, `is_loopback` / partition checks on a UDP tuple, traversal input,
reflex publication), match on `PeerAddr::Udp(a)` at that boundary; do not
add a lossy `to_socket_addr()` that silently invents a tuple.

### 4. What must survive, exactly

- Direct/routed installation rules, routed→direct migration, teardown
  ordering, stale-session and address-reuse protections, "the withdrawing
  hop is the resolved sender" — all re-exercised on `PeerAddr` by the
  existing witnesses.
- **No new guards held across awaits.** S0d §4 found zero; keep it zero.
  The witness `a_send_in_flight_retains_no_peer_shard`
  (`org_routing_wiring_tests.rs:7807`) must keep passing.
- The exported symbol set: `python3 .github/scripts/check-ffi-exports.py`
  after a `cargo build --release -p net-ffi --features net-ffi/test-helpers`
  must print "matches the baseline". Do not touch `exports.baseline`.
- Zero wire change: `tests/cross_lang_*` pass unmodified.

### 5. Tests and witnesses

Mechanical signature edits (`SocketAddr` → `PeerAddr::Udp(..)` at call
sites, type annotations) are allowed. **Assertions, coverage and named
witnesses are not weakened, renamed or deleted.** Witness floors in
`ci.yml` — `org_routing_wiring_tests >= 93` (`:175`),
`behavior::org_routing:: >= 24`, `behavior::org_routing_registry:: >= 62`,
routing state `>= 41`, org gate `60` / mesh `67` — stay met with their
REQUIRED names. **Commit test/witness edits separately from production
edits** (see Commits) so the witness diff can be reviewed line by line.

### 6. Linux-only code on a Windows host

`router.rs`'s `sendmmsg` drain and the batched-ingress path are
`cfg(target_os = "linux")`; `#[cfg(unix)]` code does not compile here.
Install the target and type-check it without linking:

```
rustup target add x86_64-unknown-linux-gnu
cargo check --target x86_64-unknown-linux-gnu --workspace --all-targets --features "$UNIT_FEATURES"
cargo check --target x86_64-unknown-linux-gnu --workspace --all-targets --all-features
```

(`cargo check` needs no linker.) Do this before every checkpoint commit;
CI is the only place that *runs* those paths.

### 7. Validation (before the candidate commit)

From `net/crates/net`, with `UNIT_FEATURES` as pinned in
`.github/workflows/ci.yml` (read it; do not trust AGENTS.md's copy):

```
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo check --target x86_64-unknown-linux-gnu --workspace --all-targets --features "$UNIT_FEATURES"
cargo clippy --all-features --lib --bins -- -D warnings
cargo clippy --lib --bins -- -D warnings
cargo clippy --no-default-features --lib --bins -- -D warnings
cargo clippy --all-features --all-targets -- -D warnings -A clippy::unwrap_used -A clippy::expect_used -A clippy::undocumented_unsafe_blocks -A clippy::multiple_unsafe_ops_per_block
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features
cargo test --lib --features "$UNIT_FEATURES"
cargo test --doc --features "$UNIT_FEATURES"
# the six witness floors, with the exact filters and REQUIRED names from ci.yml (count them; a filter that matches nothing is a silent pass)
# every tests/*.rs CI pins by name that touches routing/transport/session (cargo test --test <file>), plus every tests/cross_lang_*
cargo build --release -p net-ffi --features net-ffi/test-helpers && python3 ../../../.github/scripts/check-ffi-exports.py
# per-member clippy/doc for every member you touched, with the feature list from ci.yml
```

`cargo clippy -p <member>` / `cargo doc -p <member>` for every touched
member. If a Go toolchain with cgo is available, `go test ./...` from
`go/`; if not, say so — CI runs it.

## Constraints

- Only what the scope names. No RTC variant, no feature flag, no
  admission state, nothing from §12, no crypto/backend change, no
  `net-wire` extraction, no `proxy.rs` redesign, no new abstractions
  beyond `PeerAddr` and `PeerSink`.
- Do not "fix" unrelated things you notice; note them in the report.
- Do not edit `exports.baseline`, any plan document, or `spikes/**`
  (other than reading briefs).
- Skip project-wide formatters *until* the validation step, then run
  `cargo fmt --all` once and include it in the candidate commit.

## Commits

On `LZL0/webrtc-transport`, prefix `refactor(net): stage 1 —`. Green
checkpoints are encouraged (each must pass `cargo check --workspace
--all-targets` natively and for the Linux target), in this order:

1. `PeerAddr` + `PeerSink` introduced, no callers yet.
2. Peer-keyed state and `dispatch_packet` source typing.
3. Send sites through `PeerSink` (by S0d row ranges; one or more commits).
4. Scheduler / `router.rs` plumbing.
5. **Test and witness edits — their own commit(s), never mixed with
   production edits.**
6. Candidate: `cargo fmt`, validation, and the report.

## Report

`docs/internal/spikes/S1_REPORT.md` (same discipline as S0a–S0e):

1. commit range; the exact `UNIT_FEATURES` string used;
2. `PeerSink` as implemented (final signatures; where
   `bound_datagram_send` went; how `MeshNode` holds it);
3. the S0d row table **re-stated with a "became" column** — for every row
   (1–56) the entry point it now uses or "frozen" — and the 10 leak rows
   with what each became;
4. every witness/test file touched, with the edit class (signature-only /
   type-annotation / other — "other" needs a sentence each);
5. validation: the full command list above with pass/fail and the witness
   counts against their floors; the export-checker line;
6. deviations from S0d or from this brief, and anything noticed but not
   fixed;
7. "did not go cleanly".

Reply in the terminal with: the candidate commit hash, the validation
pass/fail list, the six witness counts, the export-checker line, and §7
verbatim. Then stop — **no continuation to Stage 2.**

## Acceptance (what the review will re-run)

- Candidate commit is clean and green on the validation list.
- `git diff ad874ff43..<candidate> -- net/` contains no `Rtc`, no
  `#[cfg(feature = "webrtc")]`, no admission/provisional state, no change
  under `proxy.rs` beyond imports/types, no change under `traversal/`
  beyond boundary conversions.
- `exports.baseline` unchanged; checker green on a fresh cdylib build.
- Witness diffs are signature/annotation-only; floors met with REQUIRED
  names.
- The report's row table has no blanks.
