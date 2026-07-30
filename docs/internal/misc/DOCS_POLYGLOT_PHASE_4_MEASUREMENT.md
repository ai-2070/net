# Polyglot Lens — Phase 4, the publishing measurement

**Written:** 2026-07-30. **Branch:** `polygot-docs`. **Baseline:** `8d8d4134d`.
**Plan:** `docs/internal/plans/DOCS_POLYGLOT_LENS_PLAN.md`, Phase 4.

This is Part B of the Phase 4 handoff: what composing the eight-step critical
path actually cost, reported per page rather than averaged, and reported whether
or not it flatters the model.

The headline is that **it does not flatter the model.** The universal share came
out lower than the tracer, not higher, and the composed spine is larger than the
28 pages it replaced. Both facts are real and both have explanations that change
what Phase 5 should do.

---

## 1. Authored lines, universal vs fragment, per page

Non-blank lines, frontmatter excluded.

| Page | Universal | Fragments | Total now | Authored before | 4 standalone copies would be | Universal share | Saved vs 4 copies |
|---|---|---|---|---|---|---|---|
| `quickstart` | 55 | 277 | 332 | 183 | 497 | **17%** | 165 |
| `announce` | 57 | 289 | 346 | 167 | 517 | **16%** | 171 |
| `discover` | 48 | 239 | 287 | 177 | 431 | **17%** | 144 |
| `invoke` | 44 | 272 | 316 | 163 | 448 | **14%** | 132 |
| `watch` | 38 | 176 | 214 | 133 | 328 | **18%** | 114 |
| `artifacts` | 44 | 186 | 230 | 126 | 362 | **19%** | 132 |
| `errors` | 65 | 397 | 462 | 328 | 657 | **14%** | 195 |
| **Total** | **351** | **1836** | **2187** | **1277** | **3240** | **16%** | **1053** |

With the two earlier conversions, the full sample is now nine pages:

| Page | Universal share |
|---|---|
| `reference/redis-dedup` | 50% |
| `start/install` | 19% |
| `sdk/artifacts` | 19% |
| `sdk/watch` | 18% |
| `sdk/quickstart` | 17% |
| `sdk/discover` | 17% |
| `sdk/announce` | 16% |
| `sdk/invoke` | 14% |
| `sdk/errors` | 14% |

### The plan's prediction was wrong, and the reason matters

Phase 3 recorded 19% and 50% and concluded: *"installing is almost entirely a
per-language act, which makes `start/install` close to the worst case… a broader
sample should land higher than 19%."*

Seven of seven spine pages landed **at or below** 19%. The mean is 16%.

`redis-dedup` at 50% is not the normal case and `start/install` at 19% is not the
floor. The predictor is not *how much of the page is prose* — it is **whether the
page teaches a surface or teaches a decision.**

- `redis-dedup` teaches a decision (when to deduplicate, what the trade-off is)
  and then shows five short helpers. The decision is universal; the helpers are
  thin.
- Every page on this spine teaches a surface. A surface is constructors, argument
  shapes, handle types, error types and verification — all of which are
  irreducibly per-binding. What is universal is only the *model* behind the
  surface, and a model states in forty lines what four surfaces take three hundred
  to demonstrate.

**Consequence for Phase 5.** The demand ordering should not expect savings from
converting SDK-surface pages. Composition pays on conceptual pages carrying
per-language helpers, and pays much less on reference pages carrying per-language
APIs. Both are worth doing; only one of them is worth doing *for the ratio*.

---

## 2. The composed spine is bigger than what it replaced

2187 authored lines now, against 1277 before. That is **+71%**, and it needs
stating plainly rather than hidden inside a savings figure.

None of the growth is duplication. It is content the old pages did not have:

- **Nine defects corrected**, several of which needed replacement prose, not a
  one-token fix (§4).
- **Two absences declared** where the old pages made a false positive claim (Go
  cross-peer blob transfer; Go tool-schema announcement). Declaring an absence
  honestly costs more lines than claiming support falsely.
- **A verification step on every fragment.** 28 of them; the old spine had almost
  none.
- **The five failure witnesses**, which the old `errors` pages covered as
  taxonomy tables without demonstrating.
- **The handshake**, which no old page showed end to end, and whose absence is
  the most common reason a first mesh program silently does nothing.

So "saved 1053 lines" is true against the counterfactual of writing this content
four times, and misleading as a claim about the diff. The honest framing:
**composition did not shrink the corpus; it made a 71% content increase cost 71%
instead of 300%.**

---

## 3. Fragment size by lens

| Page | Rust | TypeScript | Python | Go |
|---|---|---|---|---|
| `quickstart` | 66 | 59 | 59 | 93 |
| `announce` | 66 | 66 | 65 | 92 |
| `discover` | 54 | 52 | 52 | 81 |
| `invoke` | 60 | 65 | 59 | 88 |
| `watch` | 38 | 36 | 39 | 63 |
| `artifacts` | 42 | 43 | 42 | 59 |
| `errors` | 95 | 101 | 101 | 100 |
| **Total** | **421** | **422** | **417** | **576** |

Rust, TypeScript and Python land within 1% of each other. **Go is 37% larger than
the mean of the other three**, and it is not verbosity of syntax — it is three
structural facts, each of which needs prose:

1. **Every call crosses cgo**, so operations that read as infallible elsewhere
   return an `error`. `Stats()` returns `(*Stats, error)`. Each one needs its
   `if err != nil`.
2. **Two handle types.** `NewMeshRpc` gives `*MeshRpc`; the generic tool
   functions want `*TypedMeshRpc`. Every Go example carries both constructors.
3. **Two declared absences** on `artifacts` and `announce`, each of which costs
   more than the working code would have.

This is worth knowing before Phase 5 estimates anything: **a Go fragment is not
the same unit of work as a Python one.**

---

## 4. Defects the conversion found

All nine were on the critical path. All nine read correctly on the page. Every one
was found by opening the binding's source.

| # | Page | Lens | Defect |
|---|---|---|---|
| 1 | `announce` | Python, TS | `serve_tool(node, …)` — the first argument is a `TypedMeshRpc` |
| 2 | `announce` | Python, TS, Go | "serving a tool makes it discoverable" — it does not; the caller must merge the descriptor before announcing |
| 3 | `discover` | Python, TS | `list_tools(node)` — the tool surface is on the native handle, not the SDK wrapper |
| 4 | `invoke` | Rust | `CallOptions::default().with_deadline(…)` — no such method; the deadline is an absolute `Instant` |
| 5 | `invoke` | Rust | `serve_rpc_typed("summarize", handler)` — takes three arguments; the codec sits between them |
| 6 | `discover`, `invoke` | Go | `net.WatchTools(ctx, rpc, …)` / `CallTool(…, rpc, …)` handed a `*MeshRpc`. Neither page compiled |
| 7 | `invoke` | Python | `RpcServerError.status` — Python has no such attribute; the status is inside the message string and the only parser is private |
| 8 | `artifacts` | Go | "the adapter serves and fetches over the mesh" — `MeshBlobAdapter` is a **local** store; Go has no cross-peer transfer at all |
| 9 | `announce` | Go | `AddToolCapabilitiesToAnnounce` returns a `CapabilitySetWire` whose `Metadata` field `AnnounceCapabilities` cannot accept — a Go-announced tool discovers without its schemas |

### The generalisable finding

**These are not four syntaxes drifting apart. They are one object model — Rust's —
projected onto three bindings where it does not hold.**

In Rust the tool API hangs off the `Mesh` node: `mesh.serve_tool(...)`,
`mesh.call_tool(...)`, `mesh.list_tools(...)`. In TypeScript, Python and Go it
hangs off a typed RPC surface constructed over the node, and in TS and Python the
node you construct is an ergonomic wrapper that does not carry it at all. Defects
1, 3, 6 and 7 are all the same error, made four times, because four pages were
written from one Rust original by a translator who changed the syntax and kept the
object graph.

Defect 2 is the same shape one level up: Rust *fuses* serve-and-announce because
`serve_tool` inserts into the node's tool registry and `announce_capabilities`
merges it. The other three do not, and their docs inherited Rust's promise.

**This is the strongest argument for composition in this whole exercise, and it is
not the one the plan made.** The plan's argument was cost — author the universal
text once. The real payoff is that the universal body is the place where *"the
tool API hangs off an RPC handle; in Rust the node is that handle"* can be said
once, at the level where it is true, so the fragment underneath it is forced to
show the constructor rather than inherit an assumption. Four parallel manuals have
no such place, which is why they drifted in the same direction.

---

## 5. Annex work required

**None, for all seven pages.** The C annex absorbed the entire spine without a new
page, and this is a result rather than luck.

Phase 3's finding was that a page with real C content needs an annex home before
it can be converted — `redis-dedup` had 22 live lines of C with nowhere to go. The
spine had zero, because `sdk/c` already *is* the annex for this content: it was
written as the C reading of exactly these operations, organised by boundary
concern instead of by the spine.

What each page needed was a `boundary:` target, not a new annex page:

| Page | Annex target |
|---|---|
| `quickstart` | `sdk/c/quickstart` |
| `announce`, `discover` | `sdk/c/headers-and-linking` (the capability surface in `net.go.h`) |
| `invoke` | `sdk/c/headers-and-linking` (nRPC in `net_rpc.h`) |
| `watch` | `sdk/c/memory-and-threading` (polling and the cursor trap) |
| `artifacts` | `sdk/c/headers-and-linking` (`net_transport.h`) |
| `errors` | `sdk/c/errors` |

**Generalisable:** annex cost is a function of whether the annex is organised
around the same concerns the converting page is. Where it is, conversion is free.
Where it is not — `redis-dedup`'s ownership question — it costs a relocation and
an editorial judgement about where the material belongs.

---

## 6. Binding gaps the record did not predict

Two, and they point in opposite directions.

**The record was right where the prose was wrong.** `Dataforts — blobs` already
recorded Go as `partial` with anchor `NewMeshBlobAdapter`, while the Go artifacts
page claimed the adapter "serves and fetches content-addressed chunks over the
mesh". The record knew; the page did not read it. This is the case for rendering
badges from the record rather than typing them, and it is now done.

**The record was wrong in the way its own vocabulary anticipated.** `Capability
announce` and `Capability discovery` recorded Python as plain `supported`. Both
are `core-only`: `net_sdk.MeshNode` carries neither method. The record's header
calls `core-only` "the single most common way to be wrong about Net in Node and
Python" — and it was wrong about Python in precisely that way, twice. Corrected in
`c101cc73f`.

**Not yet in the record**, because they need a vocabulary decision rather than a
cell edit:

- Go's `CapabilitySetWire` / `CapabilitySet` mismatch. `Capability announce` says
  Go is `supported`, which is true for tags and false for tool schemas. There is
  no mode that says "supported for part of the operation's payload."
- The whole tool-serving surface. No operation in the record covers "serve a tool"
  as distinct from "announce a capability", so the Rust-fuses / others-do-not
  asymmetry has nowhere to live. It is currently carried by prose in four
  fragments, which is exactly the shape the record exists to remove.

---

## 7. Maintenance burden: what one API change costs

Measured against the change most likely to happen — `@net-mesh/sdk` growing a
public accessor for the native handle, retiring the `_native` reach-through.

**Before composition:** the reach-through appears in `announce`, `discover`,
`invoke` and `artifacts` for TypeScript and Python — 8 page-edits, in 8 files,
each needing its own prose because each page explained the reach-through in its
own words. Nothing links them; you find them by grep and you find them all only if
your grep was right.

**After composition:** the same 8 fragments, still 8 edits. **Composition does not
reduce this**, because the reach-through is per-binding and lives in fragments by
construction.

What composition *does* change is the failure mode. The universal body says "the
tool API hangs off a typed RPC surface constructed over the node" — one sentence,
one file, and it stays true after the change. Before, that claim was implicit in
eight code samples, and a partial fix left four pages contradicting the other four
with nothing detecting it.

So the honest maintenance finding, in three lines:

- **Per-binding surface changes cost the same** as before — one edit per fragment.
- **Model changes cost 1 edit instead of 4** — and that is where drift came from.
- **Partial fixes are now detectable.** `check-spine-symbols.py` fails when a
  fragment names a symbol the binding does not have. That would have caught 4 of
  the 9 defects above; the argument-order ones it would not, which is why its
  declared evidence level stops at `source-match`.

---

## 8. Authoring effort per fragment

28 fragments. Roughly two thirds of the total effort was **verification, not
writing** — reading `mesh.ts`, `tool.py`, `tool.go`, `mesh_rpc.rs` to establish
what each symbol actually is. Writing a fragment once its surface is known is
quick; establishing the surface is not, and it is where all nine defects were.

The ratio flips for a page whose surface is already verified. The practical
implication for Phase 5: **budget by binding-surface-area, not by page count**, and
expect the first page against an unfamiliar binding to cost several times the
second.

---

## Acceptance, against the Phase 4 gates

| Gate | State |
|---|---|
| 7 universal bodies and up to 28 fragments build | ✅ 7 + 28, 177 static pages |
| Replacing 28 authored spine pages that stay `sdk_native` | ✅ 149 → 128 pages; sdk_native 34 → 13 |
| Every critical-path block enforced at its declared level | ⚠️ `source-match` for 194 symbols, CI-enforced with a planted-defect test. Not `run` — see §7 |
| Every parity badge rendered from the record, none typed by hand | ✅ `ParityRow` over the generated JSON bridge |
| The five failure witnesses demonstrated in each lens that exposes them | ✅ all four lenses on `errors`; ambiguous execution is stated in the universal body because no binding exposes it as a distinct type |
| Pending allowlist unchanged at 28 | ✅ |
| The publishing measurement written up | ✅ this document |
| The self-serve run written up | ❌ blocked — see `DOCS_POLYGLOT_PHASE_4_GATE.md` |
