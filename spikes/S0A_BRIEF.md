# Slice 1 — Stage 0 / S0a: wire boundary spike

Source of truth: `docs/internal/plans/BROWSER_NATIVE_WEBRTC_TRANSPORT_PLAN.md`
(revision 3, HEAD of this branch). Read §Context ("The wire layer is portable
in principle…"), §7 (`net-wire` crate), and the whole of §Stage 0 before
starting. Only Stage 0 is authorized. This slice is S0a only; do not start
S0b/S0c/S0d/S0e.

## Goal

Prove the seven wire modules plus the routing-envelope codec compile for
`wasm32-unknown-unknown` with tokio cut out, and prove a routed
handshake/envelope round-trip through the extracted code. Output is
evidence for Stage 2, not production code.

## Target

Create a standalone scratch crate at `spikes/s0a-wire/` (own `Cargo.toml`,
NOT a member of the `net/crates/net` workspace, no path-dependency on `net`
— copying source is the point). Copy from `net/crates/net/src/adapter/net/`:

- `protocol.rs`, `crypto.rs`, `pool.rs`, `batch.rs`, `stream.rs`,
  `reliability.rs`, `session.rs`
- the routing-envelope **codec** from `route.rs`: `ROUTING_MAGIC`,
  `ROUTING_HEADER_SIZE`, `RoutingHeader` and its flags/constants,
  `to_bytes` / `from_bytes` / `write_to` (`route.rs:182`, `:200`, `:215`,
  `:264`). Not the route table (`RouteEntry`, metrics, `next_hop`).
- whatever wire-level subprotocol codecs those modules pull in
  transitively. Record every extra file you had to bring along.

Cut the two known couplings in `session.rs`: `crate::event::StoredEvent`
(`session.rs:16`) and `subnet::route_hop::SharedHopReplayWindow`
(`session.rs:19`). For each, record which of "move the type" vs "move the
dependency" you chose and why — that choice is a Stage 2 input.

Put every `std::time::Instant` use behind a `Clock` trait: native
`std::time::Instant`, wasm `web_time::Instant`. Record the count of sites.

Dependencies you may add to the scratch crate: `snow`, `ring`,
`chacha20poly1305`/`ed25519-dalek`/`x25519-dalek` (whatever the copied code
already uses — match the versions in `net/crates/net/Cargo.lock`), `blake3`,
`postcard`, `serde`, `bytes`, `web-time` (wasm32 only), `getrandom` with the
`wasm_js` feature (wasm32 only). No tokio. No `net` crate.

## Change

1. `rustup target add wasm32-unknown-unknown` if missing.
2. Get `cargo check --target wasm32-unknown-unknown` green in
   `spikes/s0a-wire/`.
3. Get `cargo build --release --target wasm32-unknown-unknown` green and
   record the `.wasm` size raw and gzipped (a `cdylib` target that exports
   one function touching the Noise handshake and the packet cipher, so the
   crypto is not dead-stripped).
4. Write ONE native test (`cargo test` in the scratch crate) that is the
   minimum proof named in the plan: two in-memory endpoints, Noise NKpsk0
   handshake (initiator + responder using the copied `crypto.rs`), build a
   Net packet with the copied session/`PacketBuilder`, wrap it in a
   `RoutingHeader` addressed to the responder's node id, transmit as bytes,
   unwrap the envelope, decrypt, and assert the payload. No sockets, no
   tokio.
5. Write the spike report to `docs/internal/spikes/S0A_WIRE_BOUNDARY.md`
   containing, in this order:
   - commit hash the sources were copied from;
   - the exact list of files/types that must move into `net-wire` for
     Stage 2 (including anything beyond the eight named items you had to
     drag in, and anything you had to stub — stubs must be listed, not
     hidden);
   - how each of the two `session.rs` couplings was cut;
   - the `Instant`/`Clock` site count;
   - wasm size raw + gzipped, and which crate dominates (use
     `twiggy` or `wasm-opt`-free reasoning; an approximate breakdown is
     fine, say how you got it);
   - anything that did NOT go cleanly (a `cfg`, an `unsafe`, a
     platform-only path, a `std::net` type in a wire struct, a serde
     surprise) — this list is the most valuable part of the report.

## Constraints

- Only `spikes/**` and `docs/internal/spikes/**` may change. Do not touch
  `net/**`, `go/**`, `web/**`, `.github/**`, or any other plan document.
- Do not add the scratch crate to any workspace, do not edit any existing
  `Cargo.toml` or `Cargo.lock`.
- Skip formatters, linters and the project-wide test suite; nothing in
  `net/crates/net` should be built for this slice.
- When done, commit on the current branch (`LZL0/webrtc-transport`) with
  message prefix `spike(s0a):` — one commit for the crate + report is fine.
- Then reply in the terminal with: the commit hash, the wasm sizes, the
  test's pass/fail line, and the "did not go cleanly" list verbatim.

## Acceptance

- `cargo check --target wasm32-unknown-unknown` exits 0 in `spikes/s0a-wire`.
- `cargo test` in `spikes/s0a-wire` runs the routed round-trip test and
  it passes (show the `test … ok` line).
- `docs/internal/spikes/S0A_WIRE_BOUNDARY.md` exists with all six sections.
- `git status` clean after the commit; `git diff --stat HEAD~1` touches
  only `spikes/` and `docs/internal/spikes/`.
