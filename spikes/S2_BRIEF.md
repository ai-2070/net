# Stage 2 — `net-wire` crate + `Clock` + AEAD backend seam

**Authorized by the product owner (2026-09-11), stacked on the Stage 1
candidate `10302333a`.** Kyra's decision 2 said Stage 2 follows *accepted*
Stage 1; the owner chose to proceed while exact-head CI on `8605f26ec` runs
(Unit tests, the Linux batched send/recv integration job, Go bindings and
Format are already green there). Consequence for you: **every Stage 2
commit is strictly additive** — a new crate, `pub use` re-exports, CI jobs
— so that a Stage 1 fix, if CI demands one, cannot conflict. Do not
reshape Stage 1 code.

Source of truth, in this order:

1. `docs/internal/plans/BROWSER_NATIVE_WEBRTC_TRANSPORT_PLAN.md` §7
   (`net-wire`, "What S0a established"), §Stage 2 (scope + exit criteria),
   §Stage 1 decision 2 (endpoint types sit low; no `net-wire` → native
   runtime dependency), §Context ("The wire layer is portable in
   principle…", the `ring` correction).
2. `docs/internal/spikes/S0A_WIRE_BOUNDARY.md` — §2 the type list
   (eight named items + the four dragged-in), §3 the two coupling cuts,
   §4 the 13 `Clock` sites, §6 everything that did not go cleanly.
   `spikes/s0a-wire/` is the working proof of all of it: copy its
   `aead.rs`, `clock.rs`, `time.rs` shape and its `Cargo.toml` dependency
   choices (snow without default features on wasm32, both `getrandom`
   majors opted in, `chacha20poly1305` 0.10.1) rather than rediscovering
   them.
3. `docs/internal/spikes/S1_REPORT.md` §2 — `PeerAddr` now lives in
   `transport.rs` and `NetSession::peer_addr` / `ParsedPacket::source`
   are `PeerAddr`. That is the Stage 1 input S0a was waiting for.
4. `AGENTS.md` — pre-push checklist, feature-flag trap, per-member CI
   lint lists, release conventions.

## Goal

The wire layer becomes its own tokio-free crate that compiles for
`wasm32-unknown-unknown` **and is executed there in CI**, while the core
and every binding keep compiling unchanged: no `use net::adapter::net::…`
path changes, no behaviour change under default features, no export-set
change, no wire-format change.

## Target

New crate `net/crates/net/wire/` — package **`net-mesh-wire`**, lib name
**`net_wire`** (mirrors `net-mesh-sdk` / `net_sdk`), version `0.36.0`,
dual license headers where existing files carry them, `publish` allowed
(the core will depend on it, so it must be publishable; see Release
below). Workspace member. The core (`net-mesh`) depends on it by path +
version.

**Moves into `net-wire`** (S0a §2, verbatim list, now with Stage 1 types):

| Item | From |
|---|---|
| `protocol.rs`, `crypto.rs`, `pool.rs`, `batch.rs`, `stream.rs`, `reliability.rs`, `session.rs` | `src/adapter/net/` |
| routing-envelope **codec**: `ROUTING_MAGIC`, `ROUTING_HEADER_SIZE`, `RoutingHeader` + flags/consts, `to_bytes`/`from_bytes`/`write_to` | `route.rs` (the route *table* stays) |
| all of `subnet/route_hop.rs` | `src/adapter/net/subnet/` (S0a §3.2: `NetSession` calls `seal`/`open`/…; the window type alone is not enough) |
| `ParsedPacket` (+ `parse`, `expected_payload_len`, `is_valid_length`) | `transport.rs` (the socket types stay) |
| **`PeerAddr`** (enum, `udp()`, `From<SocketAddr>`, `Display`) | `transport.rs` — decision 2: the endpoint type sits in the wire crate so `NetSession`/`ParsedPacket` need nothing native. `PeerSink` stays in the core. |
| `current_timestamp` / `coarse_clock_advance` / `COARSE_CLOCK_REFRESH_NS` | `mod.rs` |
| `StoredEvent` (struct + ctors; `from_value` and its `serde_json` stay in core) | `src/event.rs` (S0a §3.1: move the type) |
| the AEAD backend seam (`aead.rs`) and the `Clock` seam (`clock.rs`) | new, per S0a §2.3 / `spikes/s0a-wire/src/` |

**Stays in the core**, untouched except for `pub use net_wire::…` lines
that keep every existing path valid: `transport.rs`'s sockets and
`PeerSink`, `route.rs`'s table, `mesh.rs`, `mod.rs`'s adapter, everything
tokio.

## Change

1. **Crate skeleton.** `wire/Cargo.toml`: no tokio, no `net`. Native
   deps: `ring` (AEAD), `snow` default features; wasm32 target deps:
   `snow` with `default-features = false, features = ["default-resolver",
   "default-resolver-crypto"]`, `chacha20poly1305 = "0.10.1"`, `web-time`,
   `getrandom` 0.3 `wasm_js` **and** 0.2 `js` (S0a §6.2 — both majors are
   in the tree). Plus `bytes`, `parking_lot`, `crossbeam-queue`, `dashmap`,
   `blake2`, `subtle`, `tracing`. Match the versions the workspace
   already resolves; no new versions of an existing dependency.
2. **Move the files** (git-tracked moves, so history follows). Cut the
   two couplings as S0a §3 records. `PeerAddr` moves with its docs; the
   core's `transport.rs` gets `pub use net_wire::PeerAddr;` and
   `adapter/net/mod.rs` keeps re-exporting whatever it did.
3. **Re-exports.** For every moved module, the core keeps a module of the
   same name at the same path that is `pub use net_wire::<module>::*;`
   (or `pub mod protocol { pub use net_wire::protocol::*; }` — whichever
   preserves `net::adapter::net::protocol::NetHeader` and friends
   exactly). Prove it: `git grep -n "adapter::net::\(protocol\|crypto\|pool\|batch\|stream\|reliability\|session\|route_hop\)" -- net/ go/` must need **zero** edits outside the crate you created.
   Bindings (`node`, `python`, `go/*-ffi`) must not change.
4. **AEAD seam.** `crypto.rs`'s `PacketCipher` goes through `aead.rs`:
   `ring::aead::LessSafeKey` natively (byte-identical), `chacha20poly1305`
   on wasm32. Add the **cross-backend golden vector**: a JSON fixture
   (key, nonce, AAD, plaintext, expected ciphertext+tag) under
   `net/crates/net/tests/cross_lang_wire/` generated on the native side,
   and a test on *both* sides that decrypts/encrypts to it.
5. **Clock seam.** `trait Clock`, `SystemClock`, the 13 sites (S0a §4).
   Native `std::time::Instant`; wasm32 `web_time::Instant`.
6. **Tests.** Every `#[cfg(test)] mod` in the moved files moves with its
   file **unless** it reaches into the core; S0a §2.4 lists those
   (`reliability.rs:2494` → `subprotocol::stream_window`, `serde_json`
   users, and `session.rs`'s `heartbeat_api_drift_check`). For each
   core-reaching test: keep it in the core under a test module that
   imports the wire type, and say so in the report. The drift check must
   still exist and still fail on drift — it greps `mesh.rs` and `mod.rs`
   for approved `PacketBuilder` call sites, so it lives in the core and
   reads `wire/src/session.rs` via `CARGO_MANIFEST_DIR`; add a negative
   witness proving it fails when a call site is planted.
7. **Golden fixtures.** `tests/cross_lang_wire/` gets JSON fixtures +
   a Rust consumer test (`tests/cross_lang_wire.rs`, pinned by name in
   CI like the other `cross_lang_*`): `NetHeader` bytes, `RoutingHeader`
   envelope, `EventFrame`, `NackPayload`, `StreamWindow`, the AEAD vector,
   and a `CapabilityAnnouncement` in its **current** form (the three
   optional fields are Stage 4's; do not add them). Go/TS/Python
   consumers are not in this stage.
8. **CI — Stage 2 owns these jobs** (`.github/workflows/ci.yml`; read
   the surrounding comments, they encode incidents):
   - `cargo check -p net-mesh-wire --target wasm32-unknown-unknown`
     (`rustup target add` in the job).
   - **An executed wasm test**: `wasm-bindgen-test` under Node (the
     runner ships Node 24; `getrandom`'s `wasm_js` backend needs
     `crypto.getRandomValues`, which Node has) — or `wasm-pack test
     --node`. It must run (a) the S0a routed round-trip through
     `net_wire` (handshake → `PacketBuilder` → `RoutingHeader` → unwrap →
     decrypt), (b) a `Clock` read that would panic under
     `std::time::Instant` on wasm32, and (c) the AEAD golden vector on the
     `chacha20poly1305` side. A check job alone cannot catch the
     `Instant::now()` runtime panic (S0a §4).
   - `net-mesh-wire` added to the per-member clippy/doc lists with its
     feature set, and to the integration-test pin guard for the new
     `tests/cross_lang_wire.rs`.
   - Keep the wasm job cheap: cache the target and the `wasm-bindgen-cli`
     install; note the S0a finding that a wire-only wasm build pays
     ~260 KB of `wasm-bindgen` describe metadata for `getrandom` — fine
     for a test job.
9. **Release.** `release-crates.yml` publishes `net-mesh` first; it must
   now publish `net-mesh-wire` **before** `net-mesh`. Add it to the
   order and to `docs/releases/RELEASE_STEPS.md` (and the mirrored copy
   under `net/crates/net/docs/releases/` if the steps file is mirrored —
   check `npm run check:releases` conventions in `web/` before touching
   anything under `web/`; if the release notes are not affected, do not
   touch `web/`).

## What must survive, exactly

- **Zero behaviour change under default features.** Same unit tests, same
  witness floors (93/24/62/41/60/67 with REQUIRED names), same
  `cross_lang_*` fixtures unmodified, same export set
  (`python3 .github/scripts/check-ffi-exports.py` green on a fresh
  `net-ffi` release build; do not touch `exports.baseline`).
- **Zero `use` path changes** in the core, bindings, SDK, CLI, deck, MCP
  adapter, payments. If a re-export cannot express a path, that is a
  finding — report it, do not edit the consumer.
- **No `net-wire` → native dependency**: `cargo tree -p net-mesh-wire`
  shows no `tokio`, no `net-mesh`, no `mio`, no `socket2`.
- **Stage 1 code unchanged** beyond the mechanical `pub use` lines and
  the moved files themselves: `git diff 10302333a..HEAD -- net/crates/net/src/adapter/net/{mesh,mod,route,reroute,failure,router,swarm}.rs`
  contains only `use`/`pub use` lines and the removal of the moved codec
  from `route.rs`.

## Constraints

- Additive only (see top). No RTC, no feature flags beyond what the crate
  split needs, nothing from §12, no leaf crate, no Stage 3.
- Do not touch `spikes/**` except to read; do not touch any plan document
  or `S1_REPORT.md`.
- Skip formatters until validation; then `cargo fmt -p <each touched
  member>` (`cargo fmt --all` fails on Windows with os error 206) and
  per-file `rustfmt --check` over everything you changed.
- Windows host: `cargo check --target wasm32-unknown-unknown -p
  net-mesh-wire` **does** work here (S0a proved it; ring is cfg'd off on
  wasm). The Linux-target check does not (no cross C toolchain); CI is
  the arbiter for `cfg(unix)` code, which this stage should not touch.
- Run the executed wasm test locally too: `wasm-bindgen-cli` matching the
  `wasm-bindgen` crate version + Node (both are on this host —
  `spikes/s0b-rtc/` used them; record versions).

## Validation (before the candidate commit)

From `net/crates/net`, `UNIT_FEATURES` from `ci.yml` verbatim:

```
cargo check --workspace --all-targets
cargo check --workspace --all-targets --all-features
cargo check -p net-mesh-wire --target wasm32-unknown-unknown
<the executed wasm test, locally>
cargo test -p net-mesh-wire
cargo test --test cross_lang_wire
cargo clippy --all-features --lib --bins -- -D warnings
cargo clippy --lib --bins -- -D warnings
cargo clippy --no-default-features --lib --bins -- -D warnings
cargo clippy --all-features --all-targets -- -D warnings -A clippy::unwrap_used -A clippy::expect_used -A clippy::undocumented_unsafe_blocks -A clippy::multiple_unsafe_ops_per_block
cargo clippy -p net-mesh-wire --all-targets -- -D warnings   # and its wasm32 clippy if the job runs one
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features
RUSTDOCFLAGS="-D warnings" cargo doc -p net-mesh-wire --no-deps
cargo test --lib --features "$UNIT_FEATURES"
cargo test --doc --features "$UNIT_FEATURES"
# six witness floors with ci.yml's exact filters, counted
# every tests/*.rs CI pins that touches session/transport/routing + all cross_lang_*
cargo build --release -p net-ffi --features net-ffi/test-helpers && python3 ../../../.github/scripts/check-ffi-exports.py
cargo clippy -p <member> / cargo doc -p <member> for every touched member, ci.yml feature lists
python3 .github/scripts/check-script-permissions.py   # if you added a script
```

## Commits

On `LZL0/webrtc-transport`, prefix `refactor(net): stage 2 —`. Green
checkpoints (each passes `cargo check --workspace --all-targets` and the
wasm check):

1. crate skeleton + moved files + re-exports (core compiles unchanged);
2. AEAD seam + Clock seam;
3. test-module relocation + drift-check negative witness — **own commit**;
4. golden fixtures + `cross_lang_wire` test;
5. CI jobs + release ordering — **own commit**;
6. candidate: fmt, validation, report.

## Report

`docs/internal/spikes/S2_REPORT.md`, same discipline as S1:

1. commit range, `UNIT_FEATURES`, toolchain + `wasm-bindgen-cli` + Node
   versions; the three-heads table (validated head / candidate / submitted
   head) from the start this time;
2. the crate's final module map and `cargo tree -p net-mesh-wire`
   (proving no native dependency);
3. every re-export line added to the core, and the `git grep` proof that
   no consumer path changed;
4. every test module and where it landed (moved / stayed in core /
   split), the drift-check's new location and its negative witness;
5. the wasm job: what runs, how long, wasm size raw+gz;
6. validation list with pass/fail, witness counts, export-checker line;
7. deviations and things noticed but not fixed;
8. "did not go cleanly".

Reply in the terminal with: candidate hash, validation pass/fail list,
the six witness counts, the export-checker line, wasm test output lines,
and §8 verbatim. Then stop — **no Stage 3**.

## Acceptance (what the review will re-run)

- `cargo tree -p net-mesh-wire` has no tokio/net-mesh/mio/socket2.
- `git diff 10302333a..<candidate> -- go/ net/crates/net/bindings net/crates/net/sdk net/crates/net/cli net/crates/net/deck net/crates/net/adapters net/crates/net/payments` is empty (or `Cargo.lock` only).
- Export checker green on a fresh cdylib; `exports.baseline` untouched.
- Unit surface + floors unchanged; `cross_lang_*` unmodified and green;
  new `cross_lang_wire` green natively **and** replayed in the wasm test.
- The drift check exists, passes, and its negative witness fails on a
  planted call site.
- CI wasm check + executed wasm test jobs present and green at the
  submitted head.
