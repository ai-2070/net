# Stage 1–2 closure — Kyra's second HOLD on `614b1c636`

Kyra's repair review **holds** again on three bounded items, with repair
credit retained: R1 opacity, R2 execution, R3 target install, R4
lockfile are substantively repaired; R5 ships the fixture but has a
feature mismatch and a placement defect; R1's witnesses do not yet observe
the packet. Exact-code CI at `614b1c636`:
https://github.com/ai-2070/net/actions/runs/34615155152 — 48 green, one
failure (C1). No Stage 3, split, merge or publication authorized.

Same branch, prefix `fix(net): stage 2 closure Cn —`, one commit per
item, then append **§10 "Closure after Kyra's second HOLD on
`614b1c636`"** to `docs/internal/spikes/S2_REPORT.md`. Do not touch plan
documents.

## C1 — Feature-align the wasm integration test with every command that builds it

`ci.yml:4396–4398, 4410–4413`; `wire/tests/wasm_wire.rs:175`;
`wire/src/lib.rs:57–58`; `wire/Cargo.toml:26–28`. The wasm test
unconditionally references `net_wire::test_vectors::AEAD_VECTOR`, gated
behind `test-vectors`; the all-targets clippy step is featureless →
`E0433`, exit 101 (CI-confirmed and locally reproduced; passes with
`--features test-vectors`; an unpacked `.crate` shows the same pair).

Required outcome, all four properties kept:
1. a **default-feature portable-library check** stays
   (`cargo check -p net-mesh-wire --target wasm32-unknown-unknown`, no
   features — the lean-build guard);
2. **meaningful all-target linting** compiles the wasm witnesses: give
   the clippy step `--features test-vectors` (and any other feature the
   test target needs), so `--all-targets` actually lints
   `tests/wasm_wire.rs`;
3. all three named executed wasm witnesses still run;
4. both dependency-graph guards unchanged.

Do **not** make the test compile away without the feature (no
`#[cfg(feature = "test-vectors")]` on the whole test file; that is
"green by silently compiling the witnesses away"). If you prefer making
the test target's feature requirement explicit in `Cargo.toml`
(`[[test]] name = "wasm_wire"` + `required-features = ["test-vectors"]`),
that is acceptable **only** if the executed-test step still greps all
three names and fails when they are absent — a `required-features`
target that is skipped for a featureless command prints nothing, so the
grep is what keeps it honest. State which you chose. Fix the comment
that says clippy deliberately stays featureless.

Prove: the exact featureless clippy command as previously wired fails
(E0433) and the new command passes; the unpacked-`.crate` pair too.

## C2 — Recognize the Net workspace, not any Git ancestor

`src/adapter/net/heartbeat_api_drift_check.rs:216–239`.
`manifest_dir.ancestors().any(|d| d.join(".git").exists())` classifies
an unpacked package under *any* consumer repository (e.g.
`<consumer>/target/package/net-mesh-0.36.0`) as the Net checkout, then
demands `wire/src/session.rs`, which is absent there. Kyra's
exact-source harness: outside any repo → 6 pass with skip; under an
unrelated temp repo → 5 pass 1 fail; real checkout → 6 pass.

Required outcome: detect the **actual Net workspace layout**. The
positive signal must be something only the real checkout has and a
packaged crate cannot: e.g. `manifest_dir/wire/Cargo.toml` exists **and**
`manifest_dir/Cargo.toml` contains `members = [` with `"wire"` **and**
the crate's own `Cargo.toml` declares
`net-mesh-wire = { path = "wire" … }`. Then:
- real checkout: missing/renamed callee source stays **fatal** (the
  existing panic);
- unpacked package outside Git **and** beneath an unrelated checkout:
  explicit skip with the printed reason (never silent);
- keep the caller checks and the negative witness untouched.

Add a test-side witness for the classification itself if it can be done
without a filesystem sandbox (a pure function
`fn is_net_workspace(manifest_dir: &Path) -> bool` over a temp dir with
and without the layout markers is fine — `tempfile` is already a
dev-dependency or add it as one). Prove Kyra's three placements again
in §10.

## C3 — R1 witnesses must observe the emitted packet

`mesh.rs:53074–53145` (the two witnesses); builder call at `:39292`.
Kyra's production inverse: change only
`builder.build(stream_id, seq, batch, flags)` →
`builder.build(stream_id, seq, batch, PacketFlags::NONE)`, leaving
retransmit registration alone. Both landed witnesses still pass; the
reliable packet actually leaves without `RELIABLE`. A reviewer probe that
receives and decodes the target stream's real UDP packet fails correctly.

Required outcome — extend the **existing** two-node witnesses, no new
transport design:
- after `send_on_stream`, **receive the target stream's real UDP
  packet on the peer's socket** (the responder's receive loop is not
  started in these tests, so `recv_from` the raw socket, or drain via
  the existing `PacketReceiver`), parse with `ParsedPacket::parse`,
  decrypt with the peer session's rx cipher (the S0a round-trip and the
  wire crate's tests show the exact calls), and assert
  `header.flags` has `RELIABLE` set for the reliable stream and clear for
  fire-and-forget; also assert `stream_id` matches so the packet observed
  is the target stream's, not a heartbeat;
- where the reliable witness claims one retained descriptor, assert
  **exactly one** and its identity (`stream_id`, `seq` equal to the
  sent packet's `seq` from the decoded header) — not `.all(...)` over a
  possibly empty iterator;
- keep the retention-state assertions beside the wire-bit ones.
- **Do not** mistake a zero NACK bitmap for an empty test; the protocol
  carries `next_expected` (Kyra's note).

Prove in §10 with the production inverse: apply Kyra's exact one-line
change, run the two witnesses → the reliable one must fail on the
wire-bit assertion; revert (hash-check the preimage); run → both pass.
The inverse never lands.

Narrow the prose in `stream_handle.rs` and §9: the repair prevents the
**newly exposed public mutation/forgery paths**; it does not prevent
every conceivable config/state disagreement (the inherited
conflicting-config idempotent reopen remains inherited).

## Validation before the closure candidate

Everything from the repair brief, plus: the exact wasm clippy command
CI now runs; `cargo package -p net-mesh-wire --allow-dirty` → unpack →
featureless `cargo check --target wasm32` (portable lib) and the
feature-enabled `--all-targets` check both pass; Kyra's three C2
placements; the C3 inverse. Export checker on a fresh cdylib.

Reply in the terminal with the closure-candidate hash, the per-item proof
lines, and the validation pass/fail list. Then stop — **no Stage 3**.
