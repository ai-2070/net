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
`stream::{Stream fields, ReliabilityMode::is_reliable}`. Only
`PacketBuilder::new` carried a design invariant, and that invariant is
now enforced solely by the drift check (§4).

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
   is the enforcement — which is exactly why this stage added the
   negative witness for it.
5. **The Clock seam went in as its own commit (`bb16dba76`), after the
   move.** The seam *files* landed with the crate; the 13 call sites
   did not, so for four commits the wire crate compiled for wasm32 and
   would have panicked there. Nothing shipped in that window, but it is
   the exact failure mode S0a warned about, and only the executed test
   made it visible.
6. **The wasm test's fixture reader is hand-rolled.** `serde_json` is
   not in the wasm dependency set, and adding it as a dev-dependency to
   read six flat string fields would change what the test links. The
   reader handles no escapes; the fixture is generated and has none.
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
