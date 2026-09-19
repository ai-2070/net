# Stage 5 round 3 — AbiEvidence lane receipts

## `anchorless-fixture-causality.log`

§11.8 item (2), re-captured. Bounded inverse diff, exact command,
exit 1 with the verbatim `duplicate_sequence (1, 3)` failure,
`git checkout --` restore with an empty `git status --porcelain`,
and the restored run at exit 0 / 2 passed. Command is quoted in the
file header; it runs in ~6 s from `net/crates/net/leaf`.

## `browser-matrix-chromium-run1.log`, `browser-matrix-chromium-run2.log`

Two consecutive Chromium browser-matrix runs at this head, with the
five new Stage 5 stream/ABI witnesses. Build and command:

    cargo build --release \
      --manifest-path net/crates/net/tests/rtc_browser/runner/Cargo.toml

    CHROMEDRIVER="<repo>/.tools/cd/chromedriver-win64/chromedriver.exe" \
      ./net/crates/net/tests/rtc_browser/runner/target/release/rtc-browser-harness.exe \
      --engine chromium

Both runs: **26 witnesses, 25 PASS, 1 FAIL, exit 1.** The same single
witness fails in both, for the same reason and with the same
numbers — it is a reproducible counterexample, not a flake:

    stage5_reliable_stream_recovers_injected_loss_and_reorder

40 natively encoded nRPC REQUEST events on ONE stream opened
`reliability: 'reliable'` through the built `@net-mesh/browser`; the
page elides every 5th outbound datagram and submits every 3rd out of
order; the ANCHOR holds its own `open_stream(..., Reliable)` for the
same id, so the receiver has reliability state to acknowledge and
NACK against. Observed, both runs:

  * 8 datagrams elided, 10 pairs swapped — both hooks fired;
  * the anchor's real handler saw **32 of 40** invocations in 45 s;
  * the 8 missing bodies are exactly the 8 elided ones;
  * the leaf's own counters report **`stream_failed: 0`** — no typed
    terminal disposition was raised for the stream either.

So a reliable browser-leaf → native stream neither recovered the
loss nor reported giving up. That is a NEW executable counterexample
in the same family as P1 (an acknowledged fragment's expiry silently
abandons delivery): silent abandonment with no terminal disposition.
It is reported to the owner rather than assertion-weakened; the
witness asserts recovery because recovery is the contract.

**Disposition (owner).** Repaired, not deferred. The witness stays
exactly as written and stays in the roster; the browser floor stays
at 21 until it is green and is raised to 26 only then, so CI never
asserts something untrue in the interim.

A second defect in the same family was measured by the
large-message witness and is recorded, not gated, in its leg 3b: a
32 KiB native → leaf stream send returns `Ok` at the native sender
and nothing arrives at the leaf. The absence of a written contract
for that direction makes it worse rather than excusable — a caller
cannot discover the limit except by losing data. It goes to the
same lane: fragment it as the leaf → native direction does, or
refuse it typed at the sender; never `Ok` plus nothing. The leg
becomes a gate once the behaviour is decided.

Firefox cannot run on this host (NSS `certutil` unusable; the
harness refuses rather than writing a platform trust store), so
these are Chromium only.

## `../S5_R3_NATIVE_RECEIPTS/`

NativeX's ACK-oracle inverses A and B, cited by §11.8 item (1).
