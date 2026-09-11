# S2 — `net-mesh-wire` crate + `Clock` + AEAD backend seam

Stage 2 of [`BROWSER_NATIVE_WEBRTC_TRANSPORT_PLAN.md`](../plans/BROWSER_NATIVE_WEBRTC_TRANSPORT_PLAN.md)
(§7, §Stage 2), along the boundary
[`S0A_WIRE_BOUNDARY.md`](S0A_WIRE_BOUNDARY.md) proved, stacked on the
Stage 1 candidate `10302333a`.

**Implementation only** — not acceptance, not merge, no Stage 3.

## 1. Heads, commits, toolchain

| Head | Hash | What it is |
|---|---|---|
| Stage 1 candidate (base) | `10302333a` | what Stage 2 stacks on |
| Validated head | `2a9a1d7c5` | the tree every number below was produced from |
| Implementation candidate | `a9e7f223c` | validated head + this file; no code difference |
| Submitted head | `a9e7f223c` + the reviewer's docs-only record commit | branch tip at handoff; `git diff a9e7f223c..HEAD -- net/ .github/` is a one-line trailing-newline fix in `ci.yml` |

`cea1def23` (the reviewer's Stage 1 fix — `DispatchCtx.socket` was
dead under the `meshdb`/`meshos`/`deck-ffi` feature sets) landed in the
middle of this stack. It touches `mesh.rs` only and is included in
every number below; Stage 2 did not touch it.

| Commit | What |
|---|---|
| `d7174a930` | crate skeleton, moved modules, re-exports, AEAD + Clock seam files |
| `ee257c215` | drift-check negative witness + the callee pin |
| `f88243bc5` | `cross_lang_wire` fixtures + the executed wasm test |
| `5a85aff47` | CI wasm jobs, lint lists, test pin, release order |
| `bb16dba76` | the 13 monotonic reads onto the `Clock` seam |
| `2a9a1d7c5` | fmt + two rustdoc links (last code commit) |
| *(this commit)* | this report — the candidate |

Every commit is additive with respect to Stage 1 code: a new crate,
`pub use` lines, moved files, new CI jobs. No Stage 1 logic reshaped.

```
UNIT_FEATURES = net redex redex-disk cortex netdb meshdb meshos dataforts
                nat-traversal port-mapping tool batched-ingress cli regex
```

Toolchain: `rustc 1.98.1` (pinned by `rust-toolchain.toml`),
`wasm32-unknown-unknown` target installed for it,
`wasm-bindgen 0.2.128` / `wasm-bindgen-test-runner 0.2.128` (matching
`Cargo.lock`'s `wasm-bindgen 0.2.128`), Node `v24.19.0`.

## 2. The crate

`net/crates/net/wire/` — package **`net-mesh-wire`**, lib **`net_wire`**,
version `0.36.0`, dual-licensed, publishable, workspace member. The
core depends on it by `version = "0.36.0", path = "wire"` with
`features = ["json"]`.

| Module | Lines | Origin |
|---|---|---|
| `protocol` | 955 | `adapter/net/protocol.rs` |
| `crypto` | 2074 | `adapter/net/crypto.rs` (+ AEAD seam call sites) |
| `pool` | 1847 | `adapter/net/pool.rs` |
| `batch` | 394 | `adapter/net/batch.rs` |
| `stream` | 271 | `adapter/net/stream.rs` |
| `reliability` | 2493 | `adapter/net/reliability.rs` (+ `Clock`) |
| `session` | 3354 | `adapter/net/session.rs` (+ `Clock`, − drift check) |
| `route_hop` | 1005 | `adapter/net/subnet/route_hop.rs` (whole module) |
| `route_codec` | 323 | `route.rs:1-319` — codec only |
| `parsed_packet` | 55 | `transport.rs:493-534` |
| `peer_addr` | 60 | `transport.rs:338-386` |
| `event` | 159 | `src/event.rs` — `StoredEvent` |
| `time` | 111 | `adapter/net/mod.rs` coarse clock |
| `clock` | 70 | new (S0a §2.3) |
| `aead` | 178 | new (S0a §2.3) |
| `lib` | 56 | new |

`cargo tree -p net-mesh-wire --all-features --edges normal,build`
(grep for `tokio|mio |socket2|net-mesh v` → **0 matches**):

```
net-mesh-wire v0.36.0
├── blake2 v0.11.0 → digest 0.11.3 (block-buffer, crypto-common, ctutils)
├── bytes v1.12.1
├── crossbeam-queue v0.3.14 → crossbeam-utils
├── dashmap v6.2.1 → cfg-if, crossbeam-utils, hashbrown, lock_api, once_cell, parking_lot_core
├── parking_lot v0.12.5 → lock_api, parking_lot_core
├── ring v0.17.14 → cfg-if, getrandom 0.2.17, untrusted   [build: cc]
├── serde v1.0.229 (json feature)
├── serde_json v1.0.151 (json feature)
├── snow v0.10.0
├── subtle v2.6.1
└── tracing v0.1.44
```

On `wasm32` `ring` and `snow`'s default resolver are replaced by
`chacha20poly1305 0.10.1` + `snow` without default features, plus
`web-time` and the two `getrandom` majors' browser opt-ins (S0a §6.2).
The `json` feature is off there, so no `serde_json` on the wasm side.

## 3. Re-exports added to the core, and the proof nothing moved

| Core path | Line added |
|---|---|
| `adapter/net/mod.rs` | `mod batch { pub use net_wire::batch::*; }` |
| | `mod crypto { pub use net_wire::crypto::*; }` |
| | `mod pool { pub use net_wire::pool::*; }` |
| | `mod protocol { pub use net_wire::protocol::*; }` |
| | `mod reliability { pub use net_wire::reliability::*; … }` |
| | `mod session { pub use net_wire::session::*; }` |
| | `mod stream { pub use net_wire::stream::*; }` |
| | `pub(crate) use net_wire::time::{coarse_clock_advance, current_timestamp, COARSE_CLOCK_REFRESH_NS};` |
| `adapter/net/subnet/mod.rs` | `pub mod route_hop { pub use net_wire::route_hop::*; }` |
| `adapter/net/route.rs` | `pub use net_wire::route_codec::{RouteFlags, RoutingHeader, ROUTING_HEADER_SIZE, ROUTING_MAGIC};` + `_MAX_TTL` |
| `adapter/net/transport.rs` | `pub use net_wire::peer_addr::PeerAddr;` |
| | `pub use net_wire::parsed_packet::ParsedPacket;` |
| `src/event.rs` | `pub use net_wire::event::StoredEvent;` |

**Proof.** Zero `use`-path edits outside the crate created and the two
files whose contents were split (`route.rs`, `transport.rs`):

- `git diff 10302333a..<candidate> -- go/ net/crates/net/bindings net/crates/net/sdk net/crates/net/cli net/crates/net/deck net/crates/net/adapters net/crates/net/payments` is **empty**.
- `grep -rn "adapter::net::\(protocol\|crypto\|pool\|batch\|stream\|reliability\|session\|route_hop\)"` over `net/` and `go/` still matches 19 consumer sites (`mesh.rs`, `mod.rs`, `org_routing_wiring_tests.rs`, `behavior/org_scoped_ann.rs`, `behavior/gang/schedule.rs`, `channel/publisher.rs`, `traversal/rendezvous.rs`, and four `tests/subnet_*.rs`), **none edited**, all compiling and passing.
- The exported C-ABI symbol set is unchanged (§6).

Three items had to widen from `pub(crate)` to `pub` because a crate
boundary cannot express the old ceiling:
`crypto::session_prefix_from_id`, `pool::PacketBuilder::new`,
`stream::ReliabilityMode::is_reliable` (the `Stream` handle's fields
were widened here too and reverted by R1, §9). `PacketBuilder::new`
carried a design convention — production packet building goes through
the session-owned pool path — and the relocated drift check is a
**lexical guard over two core files** for that convention. *(Corrected
after Kyra's review at `b6e522bb5`: the widening is not a newly
established crypto vulnerability — equivalent raw-key constructors were
already public and current zero-key production uses construct
unencrypted handshake packets — and the drift check is not "whole
enforcement" of nonce/key ownership.)*

## 4. Where each test module landed

| Test module | Landed | Why |
|---|---|---|
| `protocol`, `crypto`, `pool`, `batch`, `reliability` (rest), `session` (rest), `route_hop` | **moved** with their file | reach nothing in the core |
| `reliability::…::build_ack_ranges_newest_first_and_codec_valid` | **core**, `adapter/net/mod.rs` → `mod reliability::core_codec_tests` | pairs `ReliableStream` with the core's `subprotocol::stream_window` codec, which stayed |
| `session::heartbeat_api_drift_check` (5 tests) | **core**, new `adapter/net/heartbeat_api_drift_check.rs` | it `include_str!`s `mod.rs` and `mesh.rs`, both core files |
| `session`'s inbound-queue case | moved, **edited**: `StoredEvent::new` instead of `from_value` | `from_value` rides the `json` feature the wasm build does not enable; same assertions |
| `event.rs`'s `StoredEvent` tests | **stayed** in the core | they exercise `from_value` / `Serialize`, i.e. the `json` surface, and sit beside the rest of `event.rs`'s tests |

Counts: core `--lib` went 5975 → **5781** (196 moved to the wire
crate, +2 new drift witnesses); `cargo test -p net-mesh-wire` runs
**196**.

**The drift check.** Now `net::adapter::net::heartbeat_api_drift_check`,
declared *below every production item* in `mod.rs` — it cuts its own
scan at the first column-0 `#[cfg(test)] mod`, so a declaration higher
up silently shrinks the surface it inspects (this actually happened
during the move and the allowlist test caught it). Two additions:

- `wire_session_still_defines_the_heartbeat_helper` — reads
  `wire/src/session.rs` via `CARGO_MANIFEST_DIR` (`include_str!`
  cannot cross a crate) and asserts `build_heartbeat` still exists.
  Without it, renaming the callee leaves the allowlist asserting about
  a method that is gone: a tripwire that passes vacuously.
- `a_planted_production_caller_breaks_the_allowlist` — the **negative
  witness**. It plants the #97/#106 shape
  (`session.thread_local_pool().get().build_heartbeat()`) into the real
  `mesh.rs` production prefix and asserts (a) the scan sees exactly one
  more caller and (b) the comparison against the allowlist now fails.

All six pass:

```
test adapter::net::heartbeat_api_drift_check::production_prefix_is_line_ending_agnostic ... ok
test adapter::net::heartbeat_api_drift_check::production_prefix_cuts_only_column_zero_test_mods ... ok
test adapter::net::heartbeat_api_drift_check::wire_session_still_defines_the_heartbeat_helper ... ok
test adapter::net::heartbeat_api_drift_check::mod_rs_production_callers_match_allowlist ... ok
test adapter::net::heartbeat_api_drift_check::mesh_rs_production_callers_match_allowlist ... ok
test adapter::net::heartbeat_api_drift_check::a_planted_production_caller_breaks_the_allowlist ... ok
```

## 5. The wasm job

`wasm-wire` (new): `rustup` target via the toolchain action,
`cargo install wasm-bindgen-cli --locked` at the version scraped from
`cargo metadata`'s `wasm-bindgen`, then

1. `cargo check -p net-mesh-wire --target wasm32-unknown-unknown` (no
   `--features json`: the lean build is the thing being protected);
2. `cargo clippy` for the same target, all targets;
3. the **executed** tests under Node, with a grep for each test name —
   a harness that compiled to zero tests would otherwise pass.

What runs (`wire/tests/wasm_wire.rs`):

```
test routed_round_trip_runs_on_wasm ... ok
test clock_reads_do_not_panic_on_wasm ... ok
test aead_golden_vector_matches_on_the_wasm_backend ... ok
test result: ok. 3 passed; 0 failed; 0 ignored; 0 filtered out; finished in 0.06s
```

Wall time locally with a warm target dir: **4.9 s** for the whole
`cargo test` invocation (0.06 s in Node).

`wasm-wire-no-native-deps` (new, separate job): asserts `cargo tree -p
net-mesh-wire` has no `tokio` / `mio` / `socket2` / `net-mesh` edge. A
runtime edge compiles natively, so the wasm job alone cannot catch it.

**Size**, release profile (the workspace's, not S0a's `opt-level="z"`
one), the wasm-bindgen test module including the harness:

| Artifact | Raw | gzip -9 |
|---|---|---|
| `wasm_wire-*.wasm` (test module, 3 tests + harness) | 1 200 029 B | 337 155 B |
| `net_wire-*.wasm` (lib test harness, 0 wasm tests) | 1 605 040 B | — |

Neither is a leaf-bundle estimate: no `wasm-bindgen` CLI pass, no
`wasm-opt`, no size profile, and the harness itself is most of it.
S0a §5's 395 KB / 121 KB remains the closest figure for what a
`net-leaf` bundle would ship.

## 6. Validation

From `net/crates/net`, at the validated head `2a9a1d7c5`; the candidate `a9e7f223c` differs only by adding this file, and the reviewer re-ran the list below at `a9e7f223c`.

| Command | Result |
|---|---|
| `cargo fmt -p <each touched member> -- --check` | pass (`cargo fmt --all` still dies with os error 206 on Windows) |
| `cargo check --workspace --all-targets` | pass |
| `cargo check --workspace --all-targets --all-features` | pass |
| `cargo check -p net-mesh-wire --target wasm32-unknown-unknown` | pass |
| executed wasm test (locally, Node 24) | **3 passed, 0 failed** |
| `cargo test -p net-mesh-wire --features json` | **196 passed, 0 failed** |
| `cargo test --test cross_lang_wire` | **7 passed, 0 failed** |
| `cargo clippy --all-features --lib --bins -- -D warnings` | pass |
| `cargo clippy --lib --bins -- -D warnings` | pass |
| `cargo clippy --no-default-features --lib --bins -- -D warnings` | pass |
| `cargo clippy --all-features --all-targets` (CI's `-A` set) | pass |
| `cargo clippy -p net-mesh-wire --features json --all-targets -- -D warnings` | pass |
| `cargo clippy -p net-mesh-wire --target wasm32-unknown-unknown --all-targets` | pass |
| `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features` | pass |
| `RUSTDOCFLAGS="-D warnings" cargo doc -p net-mesh-wire --no-deps --features json` | pass |
| `cargo test --lib --features "$UNIT_FEATURES"` | **5781 passed; 0 failed; 1 ignored** |
| `cargo test --doc --features "$UNIT_FEATURES"` | **4 passed; 0 failed; 31 ignored** |
| `cargo tree -p net-mesh-wire` — no tokio/mio/socket2/net-mesh | pass (0 matches) |

### Witness floors (ci.yml's exact filters)

| Filter | Floor | Ran |
|---|---|---|
| `org_routing_wiring_tests` | 93 | **93** |
| `behavior::org_routing::` | 24 | **24** |
| `behavior::org_routing_registry::` | 62 | **62** |
| `behavior::org_routing_state::` | 41 | **41** |
| `…sensing::org_gate::tests::` | 60 | **60** |
| `…mesh::sensing_authority_witness_tests::` | 67 | **68** |

### Integration families (nextest, CI's own lists and features)

| Family | Features | Result |
|---|---|---|
| Net mesh / capability / subnets / migration / nRPC dispatch — **49** binaries, now including `cross_lang_wire` | `net fixtures` | **477 passed, 0 failed** |
| CortEX + nRPC + AI tools (30) | `cortex tool fixtures` | **279 passed, 0 failed** |
| Sensing (14) | `cortex tool fixtures` | **65 passed, 0 failed** |
| NAT traversal (14) | `net nat-traversal fixtures` | **84 passed, 0 failed** |
| Port mapping (2) | `net port-mapping` | **2 passed, 1 skipped** |
| RedEX (3) | `redex` | **47 passed, 1 skipped** |

All pre-existing `cross_lang_*` fixtures are **unmodified** and green.

### Export checker

```
cargo build --release -p net-ffi --features net-ffi/test-helpers
python .github/scripts/check-ffi-exports.py
  baseline generated-at: 1eb9ba7cc451a1c79c1a3b8c417773a0d11789c8
  baseline pinned-to: ad874ff433b89f20ce8813ecd8d6c93b2ec58f89 (net/ identical between pin and generated-at)
  baseline count: 568
✓ net.dll: export set matches the baseline
```

`exports.baseline` untouched. No script was added, so
`check-script-permissions.py` was not run.

## 7. Deviations, and things noticed but not fixed

1. **`StoredEvent::from_value`, `parse` and the `Serialize` impl moved
   into the wire crate behind a `json` feature**, rather than staying
   in the core as the brief's table says. They cannot stay: an inherent
   impl must live in the defining crate, and `impl Serialize for
   StoredEvent` in the core would be an orphan impl. The feature keeps
   the wasm build free of `serde_json` (the core turns it on), which is
   the property the brief was protecting. 104 `from_value` call sites
   are untouched.
2. **Commit 1 carries the two forced test relocations.** The brief puts
   test-module relocation in its own commit (3), but the build is not
   green without moving the drift check and the `stream_window` codec
   case — they do not compile inside the wire crate. Commit 3 is
   therefore the *new* witnesses (the negative one and the callee pin),
   which is the part review actually needs to read line by line.
3. **`_MAX_TTL` keeps its underscore name.** S0a §6.6 suggested
   renaming it to `MAX_TTL` in the new crate. Renaming a public
   constant is not free and is not Stage 2 scope; it is re-exported
   from `route.rs` unchanged.
4. **`PacketBuilder::new` is now `pub`.** `pub(crate)` cannot survive a
   crate split, and the brief's "no new abstractions" rules out a
   sealed-token dance. The doc comment records it and the drift check
   guards the two-file production convention lexically — which is why
   this stage added the negative witness for it. Not a new crypto
   exposure (see §3's correction).
5. **The Clock seam went in as its own commit (`bb16dba76`), after the
   move.** The seam *files* landed with the crate; the 13 call sites
   did not, so for four commits the wire crate compiled for wasm32 and
   would have panicked there. Nothing shipped in that window, but it is
   the exact failure mode S0a warned about, and only the executed test
   made it visible.
6. **The wasm test's fixture reader is hand-rolled.** *(Corrected after
   Kyra's review: this is a choice, not a necessity — `serde_json` **is**
   in the test graph as an unconditional dev-dependency and a
   `wasm-bindgen-test` dependency, so the test graph is not JSON-free.
   The hand-rolled reader keeps the wasm test's own link set minimal;
   it handles no escapes and the fixture is generated and has none.)*
7. **Noticed, not fixed:** S0a §5's two size levers (snow's
   `default-resolver-crypto` links AES-GCM/SHA-2/Blake2b that NKpsk0
   never uses; `dashmap`/`parking_lot_core`/`crossbeam-queue` exist for
   contention a single-threaded leaf does not have). Both are Stage 4
   bundle work, both would change the dependency graph, neither is
   Stage 2 scope.
8. **Noticed, not fixed:** the wire crate's `session.rs` still names
   `SocketAddr` through `PeerAddr::Udp` only; nothing in it is
   RTC-shaped. That is Stage 3's job and deliberately absent.
9. **Noticed, not fixed:** `adapter/net/subprotocol/*` did not move.
   S0a §2.4 already recorded that nothing in the seven modules'
   production code references it; the only coupling was one test, which
   is why that test is the one that stayed in the core.

## 8. Did not go cleanly

- **The disk filled mid-stage.** `cargo` failed with `os error 112`
  ("not enough space") while building the fixture emitter: 1.1 GB free
  of 1.9 TB, most of it this repo's `target/` directories plus the S0a
  spike's 877 MB. The user cleared it. Nothing was corrupted, but a
  CI-shaped lesson: this stage adds a *third* target directory
  (`wasm32-unknown-unknown`, plus the release wasm artifacts) and the
  `wasm-wire` job caches it.
- **Two mechanical edits damaged code that a compiler cannot check.**
  Moving `heartbeat_api_drift_check` out of `session.rs` meant
  de-indenting it by one level, which also de-indented the contents of
  two `const SRC: &str` fixtures *inside* it — and those fixtures
  encode indentation as their subject matter ("an INDENTED
  `#[cfg(test)] mod` must not cut the scan"). The test failed, correctly,
  and pointed straight at it. Separately, declaring the relocated module
  near the top of `mod.rs` put a column-0 `#[cfg(test)] mod` *above*
  `spawn_heartbeat`, so the tripwire cut its own scan before the one
  approved call site and reported zero callers. Both were caught by the
  witnesses themselves within one test run, which is the argument for
  those witnesses existing — but a reviewer should know the drift-check
  file arrived by script, not by hand.
- **The Clock seam was nearly shipped as a file with no callers.**
  `clock.rs` and `time.rs` landed in commit 1 and `cargo check --target
  wasm32-unknown-unknown` went green with `reliability.rs` and
  `session.rs` still calling `std::time::Instant::now()` twelve times.
  The check job cannot see it; the wasm *test* is what made it obvious.
  Exactly S0a §4's warning, reproduced accidentally.
- **`ring` is still a dependency of the native build, and its
  `build.rs` still runs `cc`.** The seam removes it from the wasm path
  only. Anyone building `net-mesh-wire` for a native target still needs
  a C toolchain, which is not new but is now true of a crate whose
  selling point is portability.
- **The `missing_docs` deny cost real time on moved code.** Several
  items were `pub(crate)` with no docs and became `pub`; `Stream`'s
  fields and the wasm `AeadKey` methods had to be documented before the
  crate would compile. Those docs are new text written during the move,
  not carried over — read them as such.
- **No cross-language consumer exists for `cross_lang_wire` yet.** The
  fixtures are shaped like the other `cross_lang_*` sets and the README
  says Go/TS/Python are out of scope, but until one of those actually
  reads them, "cross-lang" describes intent, not coverage. Today the
  fixtures pin Rust against itself, plus Rust-on-wasm for the AEAD
  vector.
- **The Linux target still cannot be checked on this host** (no cross C
  toolchain, unchanged from Stage 1), and `cargo fmt --all` still fails
  with `os error 206`. Neither is Stage 2's doing; both were worked
  around the same way.

## 9. Repairs after Kyra's HOLD on `b6e522bb5`

Kyra held the Stage 1–2 candidate on five repairs, all Stage 2 (Stage 1
UDP-preservation credit preserved; two CI jobs red). One commit each,
in order, then this section.

| Head | Hash | What it is |
|---|---|---|
| Held candidate | `b6e522bb5` | what the review ran; 47 jobs green, 2 red (R3, R4) |
| Validated head | `b9b5d0536` | R1–R5; every number below produced from it, plus the `route.rs` import cleanup that lands with this report |
| Repair candidate | the commit carrying this section | validated head + this text + one unused-import line |

`UNIT_FEATURES`, toolchain, `wasm-bindgen` 0.2.128 / Node v24.19.0:
unchanged from §1.

### R1 — the `Stream` handle is core-owned and opaque again

`512d808b2`. Stage 2 moved the handle into `net-mesh-wire`, where a
crate boundary cannot express "private outside `adapter::net`", so its
fields went `pub` — and `send_on_stream` reads the wire flags off the
**handle** while every retransmit entry comes from the live
`StreamState`. Kyra's probe put `RELIABLE` on the wire with nothing
retained to resend.

`Stream` now lives in `src/adapter/net/stream_handle.rs`: private
fields, `peer_node_id()` / `stream_id()` / `epoch()` / `config() ->
&StreamConfig`, `pub(crate) fn new` reachable only through
`open_stream`. `net_wire::stream` keeps `StreamConfig`, `Reliability`,
`CloseBehavior`, `StreamError`, `StreamStats`,
`DEFAULT_STREAM_WINDOW_BYTES`; nothing in `wire/` referenced the
handle. `adapter::net::stream::Stream` and `adapter::net::Stream`
resolve unchanged and no consumer was edited — the SDK, bindings, CLI,
deck, MCP adapter and payments diff is empty. The two `src/ffi/mesh.rs`
unit tests that built a handle by field init call `Stream::new`.

Before (Stage 2 head): the assignment compiled. After, an out-of-crate
rustc probe against the built rlib:

```
error[E0616]: field `config` of struct `net::adapter::net::Stream` is private
error[E0616]: field `epoch` of struct `net::adapter::net::Stream` is private
```

and the three `compile_fail` doctests (config write, epoch write, whole
struct literal) plus a passing read-only one:

```
test src\adapter\net\stream_handle.rs - …::Stream (line 62) - compile fail ... ok
test src\adapter\net\stream_handle.rs - …::Stream (line 72) - compile fail ... ok
test src\adapter\net\stream_handle.rs - …::Stream (line 81) - compile fail ... ok
test src\adapter\net\stream_handle.rs - …::Stream (line 47) ... ok
```

Live-send witness, real two-node connect/accept, **no `start()`** so no
ACK worker can retire an entry behind the assertions:

```
test adapter::net::mesh::stream_handle_contract_tests::a_fire_and_forget_stream_sends_unreliable_and_retains_nothing ... ok
test adapter::net::mesh::stream_handle_contract_tests::a_reliable_stream_sends_reliable_and_retains_its_descriptor ... ok
```

The fire-and-forget leg asserts the session is in `fire-and-forget`
mode with `has_pending() == false` after a send; the reliable leg
asserts `has_pending() == true` and that every retained descriptor
carries `PacketFlags::RELIABLE` — the descriptor records the exact
flags the builder put on the wire, so that is the wire bit, not a
restatement of the config. The mutation direction is now a compile
error rather than a runtime assertion, which is why there is no
"mutate and observe" case: there is no mutation path left.

Scope, stated narrowly: the repair closes the **mutation and forgery
paths Stage 2 newly exposed on the public handle**. It does not
prevent every conceivable config/state disagreement — see the
inherited item immediately below.

**Inherited, not repaired:** the conflicting-config idempotent reopen
(`open_stream` logs and ignores a config that differs from the first
call's, returning a handle whose `config` describes a stream the
session did not open that way). Kyra flagged it as inherited; it
predates Stage 2 and is untouched here.

### R2 — the moved wire suite executes and is gated

`3f22e5443`. `cargo test --lib --features "$UNIT_FEATURES"` is the root
crate; no job ran `net-mesh-wire`'s ordinary `#[test]`s. The unit job
now runs `cargo test --locked -p net-mesh-wire --features json` after
the root surface (unchanged), then an inventory guard in the
witness-floor style: `MIN=196` against the `test result:` count plus a
`REQUIRED` roster pinned by exact name — both `should_panic` cases and
one test per moved module.

The routing-envelope codec's ten tests moved from the core's `route.rs`
into `wire/src/route_codec.rs`, where the code has lived since Stage 2;
that is what gives `route_codec` a roster entry and takes the suite to
**206**. `stream` is deliberately not in REQUIRED: it is
config/error/stats vocabulary with no unit tests of its own, gated by
the core's pinned `--test stream_config_and_error_display`. The core's
`--lib` count moves 5781 → 5773 (ten codec tests out, two R1 witnesses
in).

Guard, run locally against the exact commands:

```
count=206
missing=0
```

Planted-failure demonstration (planted, run, reverted **inside**
`3f22e5443` — the planted test does not land). With
`assert_eq!(1, 2, "planted")` added to `protocol::tests`:

```
test protocol::tests::planted_failure_for_the_r2_demonstration ... FAILED
test result: FAILED. 206 passed; 1 failed; 0 ignored
error: test failed, to rerun pass `-p net-mesh-wire --lib`
exit: 101
```

after the revert: `test result: ok. 206 passed; 0 failed`, exit 0.

**Counts, stated plainly:** 196 tests moved out of the core in Stage 2;
the two new drift witnesses mean the core decreased by **194**; the old
`heartbeat_api_drift_check` module had **four** tests, not five. §4's
wording said five — corrected here.

### R3 — wasm32 installed for the toolchain the commands select

`a0d8f2158`. `dtolnay/rust-toolchain@stable` with `targets:` installs
into the *stable* installation; every cargo command in the job runs
under `net/crates/net`, where `rust-toolchain.toml` pins 1.98.1, a
separate installation with its own target set.

Reproduced locally on a toolchain without the target:

```
$ cargo +1.98.0 check -p net-mesh-wire --target wasm32-unknown-unknown
error[E0463]: can't find crate for `core`
error[E0463]: can't find crate for `std`
```

Fixed by adding the target *from* `net/crates/net`, so rustup resolves
the same override file cargo does, and asserting it landed before any
build. On the pinned toolchain:

```
$ rustup show active-toolchain
1.98.1-x86_64-pc-windows-msvc (overridden by …/net/crates/net/rust-toolchain.toml)
$ rustup target list --installed
wasm32-unknown-unknown
x86_64-pc-windows-msvc
x86_64-unknown-linux-gnu
$ cargo check -p net-mesh-wire --target wasm32-unknown-unknown
(clean)
```

`wasm-wire-no-native-deps` now checks **both** graphs — host and
`--target wasm32-unknown-unknown`, which is a different graph
(`ring`/`snow` swap for `chacha20poly1305` + `web-time` + two
`getrandom` browser backends). Zero `tokio` / `mio` / `socket2` /
`net-mesh` edges in either.

### R4 — the fixtures-off probe's lockfile

`64b0256c9`. Reproduction first, in
`net/crates/net/guards/fixtures_off_probe`:

```
$ cargo check --locked --offline --message-format short
error: cannot update the lock file …/fixtures_off_probe/Cargo.lock because
--locked was passed to prevent this
```

— a resolution refusal, not the E0432/E0433 the guard greps for, which
is exactly why CI rejected it. Regenerated under the probe's own
configuration (`cargo check --offline`, no feature change):
`+net-mesh-wire`, `+web-time`, `+rand_core` and their pins, 65
insertions. Both legs now behave:

```
$ cargo check --locked --message-format short          # negative leg
src\main.rs:10:24: error[E0432]: unresolved import `net::adapter::net::org_exact_sensing_bridge`
$ cargo check --locked --features fixtures             # positive leg
    Finished `dev` profile [unoptimized + debuginfo] target(s)
```

### R5 — package-safe test inputs

`b9b5d0536`. The AEAD vector is now package-owned:
`wire/src/test_vectors/aead_vector.json`, exposed as
`net_wire::test_vectors::AEAD_VECTOR` behind a `test-vectors` feature
that only test commands enable. **Decision on the repository copy:**
`net/crates/net/tests/cross_lang_wire/aead_vector.json` stays — it is
the file a Go / TypeScript / Python consumer would read, and the whole
fixture set is shaped for them — and a new repository-only test,
`the_repository_aead_fixture_mirrors_the_package_constant`, asserts it
is byte-identical to the constant so the two cannot drift.

On the real artifact:

```
$ cargo package -p net-mesh-wire --allow-dirty
    Packaged 23 files, 574.7KiB (154.0KiB compressed)
# contents include src/test_vectors/aead_vector.json; no tests/cross_lang_wire/
$ (unpacked) cargo check --offline --features "json test-vectors"
    Finished `dev` profile
$ (unpacked) cargo check --offline --target wasm32-unknown-unknown --all-targets --features test-vectors
    Finished `dev` profile
```

and, restoring the pre-repair include inside the unpacked tree, the
failure it was hiding:

```
tests\wasm_wire.rs:175:27: error: couldn't read `tests\../../tests/cross_lang_wire/aead_vector.json`:
The system cannot find the path specified. (os error 3)
```

The callee drift guard is now explicitly repository-only: it looks for
a `.git` above `CARGO_MANIFEST_DIR` (file or directory, so worktrees
count) and outside a checkout prints
`SKIPPED wire_session_still_defines_the_heartbeat_helper: …` naming the
reason — never a silent pass, never a broken build for a registry
consumer. Inside a checkout it is unchanged and still fails on a rename;
the negative witness is untouched. All six drift tests pass.

### Validation at the repair candidate

| Command | Result |
|---|---|
| `cargo fmt -p <each touched member> -- --check` | pass |
| `cargo check --workspace --all-targets` | pass |
| `cargo check --workspace --all-targets --all-features` | pass |
| `cargo check -p net-mesh-wire --target wasm32-unknown-unknown` | pass (pinned toolchain) |
| `cargo test --locked -p net-mesh-wire --features json` | **206 passed, 0 failed** (guard: `count=206`, `missing=0`) |
| executed wasm test (`--features test-vectors`, Node 24) | **3 passed, 0 failed** |
| `cargo test --test cross_lang_wire` | **8 passed** (7 + the mirror assertion) |
| `cargo clippy --all-features --lib --bins` / `--lib --bins` / `--no-default-features --lib --bins` | pass ×3 |
| `cargo clippy --all-features --all-targets` (CI's `-A` set) | pass |
| `cargo clippy -p net-mesh-wire --features "json test-vectors" --all-targets` / same for `--target wasm32` | pass ×2 |
| `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features` / `-p net-mesh-wire` | pass ×2 |
| `cargo test --lib --features "$UNIT_FEATURES"` | **5773 passed; 0 failed; 1 ignored** |
| `cargo test --doc --features "$UNIT_FEATURES"` | **8 passed; 31 ignored** (4 new `Stream` doctests) |
| fixtures-off probe, both legs | negative E0432, positive compiles |
| `cargo package -p net-mesh-wire` + unpacked native & wasm builds | pass |
| Integration families (CI's lists) | core **478**, cortex/nRPC **279**, sensing **65**, NAT **84**, port-mapping **2**, RedEX **47** — all pass |
| `cargo tree -p net-mesh-wire`, host and wasm32 | no tokio / mio / socket2 / net-mesh |
| Linux-target check, `go test ./...` | still not runnable here (no cross C toolchain) |

Witness floors: 93 / 24 / 62 / 41 / 60 / 68 against 93 / 24 / 62 / 41 /
60 / 67, REQUIRED names present.

Export checker, fresh cdylib, `exports.baseline` untouched:

```
  baseline count: 568
✓ net.dll: export set matches the baseline
```

### Did not go cleanly (repairs)

- **R1's witness was written against guessed API names twice.** The
  reliability mode's `name()` returns `"fire-and-forget"` /
  `"reliable"`, not the type names; the test failed on the assertion
  strings before it passed on the property. Cheap, but it is the second
  time this stage that a test asserted what the code was assumed to say.
- **R2's `route_codec` roster entry required moving tests, not writing
  them.** The moved suite had no `route_codec` or `stream` tests at all,
  because the codec's tests had stayed behind in the core's `route.rs`
  and `stream.rs` never had any. Moving the ten codec tests is the
  honest fix; for `stream` the honest answer is that it has no unit
  tests, so REQUIRED says so rather than padding one in.
- **The `route.rs` unused-import cleanup rides with this report, not
  with R2.** Moving the codec tests out left `#[cfg(test)] use
  bytes::BytesMut;` unused; the warning only surfaced on the full
  `--all-targets` check after R5. One line, no behaviour.
- **`cargo package` had to be run with `--no-verify` once** to inspect
  the file list quickly; the verification build was then done by hand on
  the unpacked tree, natively and for wasm32, which is the stronger
  check and is what §R5 records.

## 10. Closure after Kyra's second HOLD on `614b1c636`

Repair credit retained: R1 opacity, R2 execution, R3 target install and
R4 lockfile stand. Three bounded items, one commit each.

| Head | Hash | What it is |
|---|---|---|
| Held candidate | `614b1c636` | 48 CI jobs green, one red (C1) |
| Validated head | `fb31c198c` | C1–C3; every number below produced from it |
| Closure candidate | the commit carrying this section | validated head + this text |

### C1 — the wasm witnesses are linted with the feature they need

`513ea73d4`. `tests/wasm_wire.rs` reads
`net_wire::test_vectors::AEAD_VECTOR`, behind `test-vectors`; the
all-targets clippy step was featureless, so it never linted the wasm
witnesses — it failed to compile them.

**Chosen fix: pass `--features test-vectors` to the clippy step**, not
`required-features` on the test target and not a `#[cfg(feature)]` over
the file. Either alternative makes a featureless command skip the
witnesses silently, which is precisely what the executed step's grep
exists to prevent; the direct fix keeps what CI lints and what CI runs
the same build.

Reproduction and fix, the exact commands, in the repository:

```
$ cargo clippy -p net-mesh-wire --target wasm32-unknown-unknown --all-targets \
    -- -D warnings -A clippy::unwrap_used -A clippy::expect_used
error[E0433]: cannot find `test_vectors` in `net_wire`
error: could not compile `net-mesh-wire` (test "wasm_wire") due to 1 previous error

$ cargo clippy -p net-mesh-wire --target wasm32-unknown-unknown --all-targets \
    --features test-vectors -- -D warnings -A clippy::unwrap_used -A clippy::expect_used
(clean)
```

and the same pair inside an unpacked `cargo package` tarball, plus the
lean-build check there:

```
$ (unpacked) cargo clippy --offline --target wasm32-unknown-unknown --all-targets …
error[E0433]: cannot find `test_vectors` in `net_wire`
$ (unpacked) … --features test-vectors        → Finished `dev` profile
$ (unpacked) cargo check --offline --target wasm32-unknown-unknown  → Finished `dev` profile
```

All four required properties hold: the featureless portable-library
check remains (and its comment now says the lean build is **its** job,
not clippy's); `--all-targets` reaches `tests/wasm_wire.rs`; the
executed step still greps all three witnesses and they pass; both
dependency-graph guards are untouched.

### C2 — the guard recognizes the Net workspace, not any Git ancestor

`c14199bcd`. `ancestors().any(|d| d.join(".git").exists())` answers yes
for an unpacked package under a consumer's own repository, and the
guard then demanded a `wire/` that a packaged crate cannot have —
Kyra's middle placement, 5 pass 1 fail.

`is_net_workspace(manifest_dir)` now requires all three layout markers
that survive only in the real checkout: `wire/Cargo.toml` exists, the
manifest declares `members = [` containing `"wire",`, and it depends on
`net-mesh-wire = { version … path = "wire" }`. Real checkout → the
missing/renamed callee stays fatal; anything else → an explicit skip
naming the markers it did not find. Caller checks and the negative
witness untouched.

Kyra's three placements, now decided by the classifier and pinned by
`net_workspace_detection_needs_every_layout_marker`:

| Placement | Markers | Guard |
|---|---|---|
| real checkout | all three present | runs, fatal on drift — 7/7 drift tests pass |
| unpacked package, no repo | no `wire/`, no member, no path dep | skip with reason |
| unpacked package **under an unrelated `.git`** | same — a Git ancestor proves nothing | skip with reason (was: FAIL) |
| tree with a stray `wire/` but no manifest markers | one of three | skip with reason |

```
test adapter::net::heartbeat_api_drift_check::net_workspace_detection_needs_every_layout_marker ... ok
test adapter::net::heartbeat_api_drift_check::wire_session_still_defines_the_heartbeat_helper ... ok
test adapter::net::heartbeat_api_drift_check::a_planted_production_caller_breaks_the_allowlist ... ok
… 7 passed
```

The synthetic trees are built with `tempfile` (new dev-dependency), so
the classification is witnessed without a filesystem sandbox. **Not
reproduced end-to-end here:** running the real unpacked `net-mesh`
package under each placement needs `cargo package -p net-mesh`, which
fails locally with "no matching package named `net-mesh-wire` … location
searched: crates.io index" — the wire crate is unpublished, which is
exactly the ordering the R4/release work set up and which only a
publish resolves. The classifier's decision on those two shapes is what
the witness pins.

### C3 — the R1 witnesses observe the emitted packet

`fb31c198c`. Both witnesses now read the datagram off the peer's
socket: `recv_from` the raw socket (the responder's receive loop is not
started, so nothing else consumes it), skip any datagram whose
`stream_id` is not the target so a heartbeat cannot be mistaken for it,
`ParsedPacket::parse`, decrypt with the peer session's rx cipher —
reading the header alone would accept any bytes that parse — then
assert the RELIABLE bit and that the plaintext is the payload sent.

The reliable witness pins descriptor identity instead of `.all(…)` over
a possibly-empty iterator: a NACK naming the sequence the wire carried
returns **exactly one** descriptor, with that `stream_id`, that `seq`,
and the RELIABLE flag. `missing_bitmap: 0` is not an empty request —
`next_expected` is itself the missing sequence.

Kyra's production inverse, applied and reverted (never lands):

```
# builder.build(stream_id, seq, batch, flags) → …, PacketFlags::NONE)
test …::a_fire_and_forget_stream_sends_unreliable_and_retains_nothing ... ok
test …::a_reliable_stream_sends_reliable_and_retains_its_descriptor ... FAILED
panicked at src\adapter\net\mesh.rs:53191:
  regression: a reliable stream's packet must carry RELIABLE on the wire
test result: FAILED. 1 passed; 1 failed
exit 101

# reverted; preimage SHA-256 matches
test …::a_fire_and_forget_stream_sends_unreliable_and_retains_nothing ... ok
test …::a_reliable_stream_sends_reliable_and_retains_its_descriptor ... ok
test result: ok. 2 passed; 0 failed
```

The prose in `stream_handle.rs` and §9 is narrowed: the repair closes
the mutation and forgery paths **Stage 2 newly exposed on the public
handle**; it does not prevent every config/state disagreement. The
inherited conflicting-config idempotent reopen remains inherited.

### Validation at the closure candidate

| Command | Result |
|---|---|
| `cargo fmt -p <touched members> -- --check` | pass |
| `cargo check --workspace --all-targets` / `--all-features` | pass ×2 |
| `cargo check -p net-mesh-wire --target wasm32-unknown-unknown` (featureless) | pass |
| `cargo clippy -p net-mesh-wire --target wasm32 --all-targets --features test-vectors` (the new CI command) | pass |
| `cargo test --locked -p net-mesh-wire --features json` | **206 passed**; guard `count=206`, `missing=0` |
| executed wasm test (Node 24) | **3 passed** — all three witnesses |
| `cargo test --test cross_lang_wire` | **8 passed** |
| clippy: all-features lib/bins, lib/bins, no-default lib/bins, all-features all-targets, wire native all-targets | pass ×5 |
| rustdoc `-D warnings`: core all-features, `-p net-mesh-wire` | pass ×2 |
| `cargo test --lib --features "$UNIT_FEATURES"` | **5774 passed; 0 failed; 1 ignored** |
| `cargo test --doc` | **8 passed; 31 ignored** |
| fixtures-off probe | negative exit 101 with E0432 naming the bridge; positive exit 0 |
| `cargo package -p net-mesh-wire` → unpack → featureless wasm check + feature-enabled `--all-targets` | pass |
| C2 placements | classifier witness passes; real checkout runs the guard |
| C3 production inverse | reliable witness fails, revert restores the preimage, both pass |
| Integration families | core **478**, cortex/nRPC **279**, sensing **65**, NAT **84**, port-map **2**, RedEX **47** |
| Linux-target check, `go test ./...` | still not runnable here (no cross C toolchain) |

Witness floors: **93 / 24 / 62 / 41 / 60 / 68** against 93/24/62/41/60/67.

Export checker, fresh cdylib, `exports.baseline` untouched:

```
  baseline count: 568
✓ net.dll: export set matches the baseline
```

### Did not go cleanly (closure)

- **C1 was a self-inflicted feature split.** R5 added the
  `test-vectors` gate and enabled it on the *test* command while
  leaving the clippy command featureless — and the comment I wrote
  there claimed that was deliberate. It was a mistake dressed as a
  decision, which is worse than the mistake; the comment is corrected
  rather than deleted.
- **C2's end-to-end placements are not reproducible on this machine.**
  `cargo package -p net-mesh` cannot resolve the unpublished
  `net-mesh-wire`, so the real unpacked-core trees Kyra built could not
  be rebuilt here. The classifier is witnessed directly instead, on
  synthetic trees of exactly those shapes — weaker evidence than
  running the shipped test in all three placements, and stated as such.
- **C3 found nothing wrong with the production code, which is the
  point.** The inverse had to be applied by hand to prove the witnesses
  bite, and the first version of the new assertions passed against the
  inverse too — because they read the descriptor, not the datagram.
  The receive-and-decrypt path is what makes the difference, and it
  exists only because the review insisted on it.
