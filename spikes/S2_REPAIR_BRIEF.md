# Stage 1–2 repairs — Kyra's HOLD on `b6e522bb5`

Kyra's review of the submitted head `b6e522bb5` **holds** the candidate on
five required repairs, all Stage 2. Stage 1 UDP preservation has no
identified source blocker and its Linux/binding CI jobs passed; keep that
credit — do not touch Stage 1 code except where a repair below names it.
Stage 3 is not authorized. Exact-head CI:
https://github.com/ai-2070/net/actions/runs/34597594248 (47 jobs green,
2 failed — R3 and R4 below).

Work on `LZL0/webrtc-transport`, prefix `fix(net): stage 2 repair Rn —`,
one commit per repair (R1–R5) plus one for the report. Additive to Stage 1
as before. Do not touch plan documents; append to `S2_REPORT.md` only in
the new §9 described at the end.

## R1 — Preserve the opaque `Stream` handle contract (P2)

`net/crates/net/wire/src/stream.rs:238–250` made `Stream`'s
`peer_node_id`, `stream_id`, `epoch`, `config` **public**. Kyra's runtime
probe: open a `FireAndForget` stream, assign
`handle.config.reliability = Reliable`, `send_on_stream` → the packet
goes out with `wire_reliable=true` but **no retransmit entry is
retained** (`mesh.rs:38980–39027, 39106–39136, 39154–39158, 39359–39370`
choose flags from the handle while bookkeeping uses the live
`StreamState`). At Stage 1 the same assignment was a compile error
(E0616).

Repair (the factoring Kyra offered, and the smallest): **move the
`Stream` handle back into the core.** Nothing in `wire/` uses it
(`grep -rn "Stream {" wire/src` → only `stream.rs` itself). Keep
`StreamConfig`, `Reliability`, `CloseBehavior`, `StreamError`,
`StreamStats`, `DEFAULT_STREAM_WINDOW_BYTES` etc. in `net_wire::stream`.
In the core, define `Stream` with **private fields** and read-only
accessors (`peer_node_id()`, `stream_id()`, `epoch()`, `config()` →
`&StreamConfig`), constructed only by the core (`pub(crate)` constructor
or the `open_stream` path). Re-export so `net::adapter::net::stream::Stream`
and every existing consumer path still resolve — SDK/bindings must not
change (they use the handle through `MeshNode` methods; verify with the
consumer-diff check). **No public constructor** that lets an application
mint a handle with arbitrary config for an existing stream id.

Evidence required, both directions:
- an external-consumer compile probe (a tiny throwaway crate or a
  `compile_fail` doctest on the core) showing `handle.config.reliability
  = …` and `handle.epoch = …` are rejected again;
- a **live-send witness** in the core's tests: open `FireAndForget`, try
  the only mutation path an application still has (there should be none —
  if the accessor returns `&StreamConfig`, prove it cannot be written);
  and the positive control: a `Reliable` open sends `wire_reliable=true`
  **and** retains its retransmit entry. Use the real two-node
  connect/accept path as Kyra's probe did; do not start the ACK workers.

The inherited "conflicting-config idempotent reopen" issue is **not**
yours to fix; mention it in §9 as inherited.

## R2 — Restore gating execution of the 196 native wire tests (P1)

`ci.yml:103–104, 3540–3549, 4326`: the root unit job runs only
`net-mesh`; nothing executes `net-mesh-wire`'s ordinary `#[test]` suite
(clippy compiles, wasm runs three cases). 196 real tests stopped gating.

Repair:
- In the unit-test job, after the root `cargo test --lib --features
  "$UNIT_FEATURES"` (keep that surface exactly as is), add
  `cargo test --locked -p net-mesh-wire --features json` — explicit,
  locked.
- Add a **non-vacuous inventory/count guard** in the existing style
  (`MIN=` + `REQUIRED` exact names, like the witness floors): the moved
  suite must run **≥ 196** tests and the roster must include, by exact
  name, the two `should_panic` cases plus at least one test per moved
  module (`protocol`, `crypto`, `pool`, `batch`, `stream`, `reliability`,
  `session`, `route_codec`, `route_hop`). A filter that matches nothing
  must fail the job (`--no-tests=fail` with nextest, or count the
  `test result:` line).
- Demonstrate, in §9, that a planted failing wire test makes the exact
  job command exit non-zero (plant, run, show, revert — the revert must
  be in the same commit, i.e. the planted test never lands).

Count wording for §9: 196 tests moved; the two new drift witnesses mean
the core decreased by 194; the old heartbeat drift module had **four**
tests, not five.

## R3 — Install the wasm target for the *selected* toolchain (P1, CI red)

`ci.yml:4285–4293` installs `wasm32-unknown-unknown` for the
`dtolnay/rust-toolchain@stable` installation, but every cargo command
under `net/crates/net` resolves `rust-toolchain.toml`'s 1.98.1
installation, whose target set is separate → `E0463` missing core/std.
Failing job:
https://github.com/ai-2070/net/actions/runs/34597594248/job/103256898011

Repair: one toolchain for install and every command. Either run
`rustup target add wasm32-unknown-unknown` **from `net/crates/net`** after
the toolchain action (so the pinned toolchain receives it), or point the
action at the pinned toolchain explicitly — read how the other jobs in
`ci.yml` handle the pin and match them. Keep the executed wasm test and
its per-test grep. Also extend `wasm-wire-no-native-deps` to check **both**
graphs: the host graph and
`cargo tree -p net-mesh-wire --target wasm32-unknown-unknown --edges normal,build`.

Prove locally by reproducing the failure mode (a toolchain without the
target must fail with the same E0463) and the fix (the pinned toolchain
with the target passes); `rustup target list --installed
--toolchain <pinned>` output in §9.

## R4 — Refresh the fixtures-off probe lockfile (P2, CI red)

`net/crates/net/guards/fixtures_off_probe/Cargo.lock` was not regenerated
after `net-mesh-wire` entered the graph; the probe's locked negative leg
now fails at dependency resolution instead of the intended
E0432/E0433 refusal, and CI correctly rejects the wrong-reason failure.
Failing job:
https://github.com/ai-2070/net/actions/runs/34597454936/job/103256898095
(the CortEX-family failure is this, not a runtime assertion).

Repair: regenerate the probe's own lockfile under its actual
dependency/feature configuration; keep `--locked` and both legs
(fixtures-off → the intended resolution refusal; fixtures-on → compiles).
Reproduce first: `cargo check --locked --offline --message-format short`
in the probe dir exits 101 today; after the refresh, the negative leg
fails for the **intended** reason and the positive leg passes. Show both
in §9.

## R5 — Package-safe test inputs (P2)

- `wire/tests/wasm_wire.rs:172` includes
  `../../tests/cross_lang_wire/aead_vector.json` — outside the wire
  package; a real `cargo package` of `net-mesh-wire` unpacked and built
  fails to compile the wasm test.
- `src/adapter/net/heartbeat_api_drift_check.rs:204–217` reads the
  sibling workspace path `wire/src/session.rs`, which does not exist for
  a registry dependency.

Repair:
- Make the AEAD vector **package-owned and shared without an
  out-of-package include**. Recommended: the wire crate carries the
  canonical fixture under `wire/tests/fixtures/` (listed in
  `[package] include` if the manifest restricts files) and exposes it as
  `net_wire::test_vectors::AEAD_VECTOR: &str` behind a `test-vectors`
  feature that only tests enable; both the wasm test and the core's
  `cross_lang_wire` consumer read that constant. The JSON under
  `net/crates/net/tests/cross_lang_wire/` then becomes a copy checked
  byte-equal by the core test (repository-only assertion), or is removed
  in favour of the constant — your call, say which.
- Make the callee drift guard **explicitly repository-only** without
  weakening it: detect a repository checkout (e.g. the workspace root's
  `.git`), run the guard there, and in a packaged build **skip with a
  printed reason** — never a silent pass. CI is always a checkout, so the
  guard never goes vacuous where it matters; the negative witness stays.
- Verify with an actual `cargo package -p net-mesh-wire --allow-dirty`
  (no publish), unpack the `.crate`, and show the native library and the
  wasm test target compile from the unpacked tree. Record in §9.

## Non-blocking wording corrections (the reviewer will apply these)

Not yours: the reviewer edits `S2_REPORT.md` §3/§7 wording on
`PacketBuilder::new` (not a new crypto vulnerability; the drift check is
a lexical guard for two files), the "JSON-free test graph" claim
(`serde_json` is a dev-dependency), and narrows the S0d/S1
`DATAGRAM_SEND_DEADLINE` claim (ordinary org-egress uses it; the queue
deadline is fixture-only). Do not edit those passages.

## Validation before the repair candidate

Everything from `S2_BRIEF.md` §Validation, plus: `cargo test --locked -p
net-mesh-wire --features json` (count ≥ 196), the new inventory guard's
exact command, the probe's two legs, the packaged-tree compile, the wasm
executed test, and the export checker on a fresh cdylib (`exports.baseline`
untouched). Six floors 93/24/62/41/60/67 with REQUIRED names.

## Report

Append **§9 "Repairs after Kyra's HOLD on `b6e522bb5`"** to
`docs/internal/spikes/S2_REPORT.md`: per repair, what changed, the
reproduction before and the proof after (commands + output lines), and
the three-heads table for the repair candidate. Reply in the terminal
with the repair-candidate hash, the per-repair proof lines, and the
validation pass/fail list. Then stop — **no Stage 3**.
