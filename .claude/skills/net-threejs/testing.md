# Testing, and what each surface actually establishes

The units and the witnesses live in different places, and confusing them is how
a green bar comes to mean nothing.

## In the package

From `net/crates/net/browser-ts/`:

```bash
npm install
npm run build     # tsc + bundle + copy the leaf wasm beside dist/
npm test          # tsc -p tsconfig.test.json && vitest run
```

The unit tests run against `test/fake-wasm.ts`, a fake module satisfying the
interfaces declared in `src/wasm.ts` — so if the Rust surface changes, `src/`
stops compiling against the declared boundary and the fake stops satisfying it.
Both are compile-time failures, not silent drift.

The store's public behaviour is pinned by transport-double tests:

- `test/store/hosted.test.ts` — host and joiner end to end over a transport
  double: readiness and projection, action round trip, input coalescing, lease
  renewal and expiry, audience scoping, deltas.
- `test/store/audience.test.ts` — audience transition timing (immediate clear,
  owner acceptance allocates, installation publishes) and refusal of denied
  audiences.
- `test/three/binding.test.ts` — create/update/remove, the reference-identity
  skip, dispose, error isolation, and the pull `apply()`.
- `tests/abi_real_package.mjs` loads the **built** package output and the
  wasm-bindgen artifact beside it and asserts the decode, the ids, the ICE parse
  and the effective stream options against the real artifacts.

**Reference stability gets explicit witnesses**, because "unchanged subtrees
retain identity" is an implementation property a naive snapshot apply silently
breaks:

- updating one entity preserves unrelated entity references;
- applying an *equivalent* resynchronization snapshot preserves the root
  reference;
- changing part of a snapshot preserves unchanged subtrees;
- an unchanged selector result does not notify its listener.

Audience clearing is **not** subject to these: removing visibility is a genuine
observable change and must notify.

## In a real browser

`net/crates/net/tests/rtc_browser/run.ps1` (`run.sh`) is the CI gate and the
supported path. It builds the wasm leaf, issues its own CA and a `localhost`
leaf, pins trust per engine, starts the anchor and the bootstrap listeners,
serves the page on `http://localhost`, launches the engine via Playwright, and
prints one `RTCB PASS`/`RTCB FAIL` line per witness. Use `-Engine chromium|firefox`
and `-Stage7` for the opt-in store witnesses. Nothing in it uses
`--ignore-certificate-errors`.

Witness rosters are constants in `net/crates/net/tests/rtc_browser/runner/src/`
— read them rather than trusting a prose count. The store witnesses (snapshot
install over a real stream, correlated action + delta, duplicate-message
idempotence, relay forwarding counter) sit behind `--stage7` and are **not in any
floor**.

`net/crates/net/examples/browser-demo/run.ps1` is the three-tab direct-path demo;
`-Check` runs it headless and asserts its rows.

Two rules that come up constantly:

- **Independent participants need distinct identities, asserted.** Use isolated
  browser contexts/profiles *and* assert distinct authenticated node ids —
  isolation alone is insufficient if the harness provisions the same identity
  twice. Multiple same-origin tabs sharing one identity cover leader replacement
  and subscription ownership, which is a **separate** witness and not a
  substitute.
- **Linux netns evidence is CI-only on a workstation; browser execution is not.**
  A Chromium/Firefox run needs no netns.

## The two demo modes, and their very different weight

The in-package demo (`net/crates/net/browser-ts/demo/`) has two modes:

- `?mode=local` (default) — host and two joiners in one page over a development
  bus. The store, codec, chunker, assembler, ledger and both state machines are
  **real**; the mesh is not. Delivery is a function call and the authenticated
  peer is **assigned by the bus instead of proved by a handshake**. A screenshot
  of it is **not evidence that two browsers can play**.
- `?mode=mesh` — this page's node from the built package over a real anchor.
  It is written and, at the time of writing, run against nothing: it establishes
  neither Chromium **and** Firefox on the supported path, nor direct-versus-
  forced-fallback delivery with a per-pair forwarding counter, nor reliable
  transfer, nor the leader-proxy lifecycle. Those are transport properties.

The page exposes `globalThis.__demo` (`state()`, `scene()`, `steer()`, `fire()`,
`hostState()`) so a harness can drive it without scraping pixels — prefer that to
a screenshot.

## Witness discipline

- Write the failing witness **before** the fix, run the narrow family, then the
  broader gate.
- An oracle must be able to distinguish the outcomes. Counting a replica's
  publications is stronger than checking its final value (a duplicated message
  that moves the view once is invisible in the final value).
- A "did not throw" or "field is non-empty" assertion is not a witness. Assert
  what a consumer observes: a channel bearer never receives the waypoint; an
  action at self is refused with a code; the same install over injected loss and
  reorder still lands byte-identical.
- When a witness is excluded or skipped, **name it** so a short ledger is never
  silent.
