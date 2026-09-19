# Stage 7 review, round 6 — head `33ca60022`

Branch `LZL0/webrtc-transport`, worktree
`C:/Users/chief/orca/workspaces/net/webrtc-transport`.

This is the repair candidate for your round-5 HOLD at `535a9f05a`.
One commit: `33ca60022`.

## What you found, and what was done

All five of your findings reproduced against my tree before anything
was changed. Every repair is inside `host.ts`'s `close()` plus two
claim texts.

1. **K1/K9 (P1) — the batch aborted on the first permanent failure.**
   Each goodbye is now sent independently, on the stream that already
   exists, and each loss is counted on its own.
2. **K2/K2b/K12 (P1) — `close()` opened a stream and could never
   return.** The farewell no longer goes through `emit`, so the
   stale-handle reopen is off the teardown path. It is bounded on TWO
   clocks — the injected `schedule` so a witness can prove the bound
   without waiting, and real time so a caller that injects a clock
   and never advances it still gets a closed store. The deadline is
   **500 ms**, not the 2 s I first chose: your probe allowed 750 ms
   and read 2 s as unbounded, and a page's unload budget is under a
   second.
3. **K4 (P2) — concurrent close.** `close()` is latched on the
   in-flight promise.
4. **K3b/K11 (P2) — the admission window.** Serving stops
   (`unsubscribe()`) before a single goodbye is composed.
5. **K6 (P2) — the adoption clause.** Corrected in both texts rather
   than deleted, and the property is witnessed where it is real: a
   new in-process witness makes the goodbye UNDELIVERABLE, so the
   replica still believes it holds a handle, its write crosses
   (asserted: the wire moved), and the successor refuses it `closed`
   with document and handle count unmoved.
6. **K5, which you retired and did not file, was a real defect.**
   `ready()` on an already-terminal replica hung forever —
   `settleReady` runs only when a frame arrives. It now rejects
   `owner-lost` at once.

## What to attack

- The new `shutdown()` ordering. Serving stops first, then goodbyes,
  then stream teardown. Is there anything between those steps that
  can still bind, send, or leak?
- The two-clock bound. Does a stray real timer outlive a store? Can
  the deadline fire after a successful send and reject a settled
  promise?
- The latch. Does a `close()` that REJECTS leave the latch poisoned,
  so a later call returns a rejected promise rather than closing?
- The new in-process adoption witness: is it really reaching the
  successor, or have I moved the same non-discrimination one step?
- Anything the per-peer double now hides: `permanent` went from
  per-node to per-pair and two existing call sites were rewritten.

## Evidence I am claiming, all executed at `33ca60022`

- Chromium `--stage7`: **58 witnesses, 0 failed**.
- `npx vitest run`: **682 passed, exit 0**.
- `npx tsc -p tsconfig.test.json` clean; `npm run build` clean.
- `node tests/abi_real_package.mjs` 32/32; `node tests/kyra_review.mjs`
  4/4; `npm run size` PASS.
- **All 18 of your probes pass** against my tree, copied to
  `C:/Users/chief/AppData/Local/Temp/s7probes5` with the dist path
  repointed. The one FAIL is K9, your defect probe, correctly
  inverting ("B was told after all").
- Six inverses run individually, each red for its own reason, tree
  restored green: batch-aborts, farewell-reopens-a-stream, unbounded,
  not-latched, unsubscribe-after-the-goodbye, ready-hangs-when-
  owner-gone.

## Lines

Unchanged from round 5. Hold if it deserves a hold; do not edit the
tree; separate EXECUTED from SOURCE-ESTABLISHED; tell me if any
inverse above is non-discriminating.
