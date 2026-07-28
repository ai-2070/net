# Agent Skills — language routing

> Make the skills route by binding instead of presenting five API shapes at
> once. Shared doctrine + explicit routing + thin binding companions + checked
> examples — not five cloned skills, not one giant multilingual file.
> Companion to [`SKILLS_VERIFICATION_PLAN.md`](SKILLS_VERIFICATION_PLAN.md),
> which proves the skills match the tree; this one decides what an agent loads.

## Status

**Revision 2 — not started.** Revision 1's architecture was approved; its
verification and scope gaps were not. Every correction below was checked against
the tree rather than accepted on description, and two changed the plan
materially:

- **Nested companions would escape three of five checks.** Not a theory — I put
  a file at `net-event-bus/bindings/_probe.md` with five planted defects. The
  enum-variant and identifier checks caught theirs (they use `rglob`); the
  **CLI-invocation, cited-path, and plan-vocabulary checks silently passed**,
  because `check-skills.sh` globs `"$SKILLS"/*.md "$SKILLS"/*/*.md` and a
  companion sits one level deeper. Moving content under `bindings/` before
  fixing this would quietly retire three checks.
- **The refactor is not event-bus-only.** R1 measured `net-event-bus` and
  generalised. `net-payments/bindings.md` is **85% per-language** — worse than
  `apis.md`'s 68%.

## The measurement that scopes it

Per-language content is not evenly spread. A uniform split would churn files
with nothing to gain:

| File | Lines | Shared | Per-language | Split? |
|---|---|---|---|---|
| `net-payments/bindings.md` | 286 | 14% | **85%** | **Yes** — Python 146, Node/TS 79, Go 10, Rust 9 |
| `net-event-bus/apis.md` | 270 | 31% | **68%** | **Yes** |
| `net-event-bus/runtime.md` | 344 | 63% | 36% | Partly |
| `net-event-bus/nrpc.md` | 582 | 64% | 35% | No |
| `net-event-bus/scheduler.md` | 209 | **94%** | 5% | No |
| `net-payments/{caller,provider,testing}.md` | 592 | **100%** | 0% | No |

The 35% in `nrpc.md` is **operation-specific** — "how Python does nRPC" — and by
the companions' own definition ("how that language expresses Net *generally*")
belongs in the subsystem chapter. Splitting it would scatter one subsystem
across five files. `apis.md` and `bindings.md` are the opposite: their bulk is
generic-language material — package name, sync vs async construction, error
shape, iterator semantics, lifecycle — welded into one file a Python reader
loads to get a fraction of.

*(Method note: the first pass reported "Python 225" for `bindings.md` because
the heading `## Node / TS — … (parity with Python)` matched `Python` before
`Node`. The corrected split is above. A heading-span heuristic needs its
attributions spot-checked, not trusted.)*

## Phase 0 — make nested content verifiable — **Priority 0, S**

Prerequisite. Without it the later phases silently reduce coverage.

**0a. Recursive discovery.** Every corpus-level check in `check-skills.sh`
enumerates markdown recursively, not two levels deep. Affected: cited repo
paths, CLI invocations, internal-plan vocabulary, and the cross-reference loop
(which reads `"$dir"*.md`).

**0b. A nested fixture, not just a glob change.** A glob is one edit away from
regressing. Add a permanent fixture — a nested file with a known-bad reference
that CI asserts is *caught* — so the depth guarantee is tested, not assumed.

**0c. Resolve the TypeScript publication gate.** R1's appendix listed "five
hello-worlds compile or type-check" beside "publication is blocked when
verification is red." Those do not compose. `check-skill-examples.sh` skips
TypeScript when the napi declaration is absent (deliberately, and exempt from
`REQUIRE_ALL`), and `hello.ts` is type-checked in `ci.yml` — which
`skills.yml`'s `publish` job cannot depend on, since `needs` does not cross
workflows. So: TS CI red + skills green → the mirror still publishes.

That is the cross-workflow race the merged workflow was built to remove, still
open for one language. **Close it rather than document it** — the published
skill advertises TypeScript as first-class. Options, cheapest first: a reusable
workflow both depend on; generate the napi declaration inside the skills
workflow; or move publication behind a suite that covers everything. Whichever
is chosen, the appendix claim gets corrected either way.

## Phase 1 — domain-local capability matrices — **Priority 1, S**

One matrix **per skill**, not one global file: the publisher copies
`net-event-bus/`, `net-payments/` and `README.md` and nothing else, so a
root-level matrix would never ship, and each skill must stand alone when
installed by itself.

```
net-event-bus/bindings/coverage.md
net-payments/bindings/coverage.md
```

**Why it earns its place:** parity claims are prose in at least 8 files today,
and one had already drifted — `net-payments/bindings.md` said billing was Rust +
Python when Node has `readBilling` too. Fixed during the 2c audit, but only
because someone read it.

### Cell vocabulary — one dimension at a time

R1's list mixed completeness, access mode, layer and availability into one
column, leaving "is Go payments `partial` or `verify-only`?" undecidable. Split:

| Column | Values |
|---|---|
| **Status** | `supported` · `partial` · `experimental` · `not exposed` · `n/a` |
| **Mode** | (blank) · `poll` · `verify-only` · `core-only` |

`Mode` qualifies a status rather than competing with it: C's event bus is
`supported` + `poll`; Go payments is `partial` + `verify-only`; capability
sensing is `experimental` + `core-only`.

**`n/a` is reserved for "this operation makes no sense here",** not "this
binding lacks it." R1 got its own example wrong: *"C has no A2A because A2A is
not a C-shaped API"* — A2A could perfectly well have a C ABI; it does not have
one **yet**. That is `not exposed`. Misfiling it as `n/a` tells an agent the
gap is permanent and stops anyone asking for it.

### What the check can honestly assert

Not completeness. A symbol proves one narrow fact: *an expected anchor exists on
this surface*. It cannot prove `supported`, and absence is worse — a binding may
alias, project under another name, expose dynamically (Python), surface through
generated declarations (TS), or carry a low-level FFI symbol the ergonomic SDK
deliberately withholds.

So: **the matrix is editorially authoritative; CI verifies its declared evidence
anchors and selected critical absences.** Each non-`n/a` cell names its own
evidence — a source path, a public symbol, a compile-checked example, or a
conformance test — and the check resolves *that*. Negative evidence is asserted
only where it is cheap and meaningful, cell by cell, never inferred globally.

## Phase 2 — split generic binding material — **Priority 1, M**

`net-event-bus/apis.md` → `bindings/{rust,typescript,python,go,c}.md`, and the
same for `net-payments/bindings.md` (option **C** from the review: keep the 14%
shared as router + matrix, split the Python 146 / Node 79 / Go 10 / Rust 9). The
shared remainder of each becomes the routing page.

Each companion carries only what changes generated code:

1. package / import — they differ (`net-mesh-sdk` publishes, `net_sdk` imports)
2. construction + configuration, and whether it is async
3. runtime model, including blocking hidden behind an async surface
4. exact names and argument shapes (positional / kwargs / options object)
5. return and error behaviour — throws vs returns `false` vs encodes status
6. resource ownership and shutdown obligations
7. feature availability + known gaps, pointing at `coverage.md`
8. a link to its canonical checked example (see Phase 4 — *link*, not a copy)
9. where the authoritative binding source is
10. what must never be inferred from another binding

Subsystem chapters keep their operation-specific language sections.

**Done when:** both files are routing pages, companions exist, and the checks
pass *with Phase 0b's fixture proving they reach the new depth*.

## Phase 3 — routing rules in `SKILL.md` — **Priority 2, S**

The skill says "identify the language" as workflow step 1. Make it binding
selection: inspect the project manifest; load exactly one companion before
generating; consult `coverage.md` before promising a surface exists; never
default to Rust because Rust is the substrate; read the binding's own
source/types when still uncertain; and with no project context and no language
named, **ask** — one of the few questions worth blocking on.

## Phase 4 — bounded checked examples — **Priority 2, L**

R1 sized this M and left "important operation" undefined, which makes it
subsystems × bindings — a design problem, not an implementation task. Two
constraints have to be settled first.

**The initial operation set is the public critical path**, nothing wider:

```
construct/start · announce · discover · invoke · observe · handle one failure · shutdown
```

Subsystem examples get added only where a binding's shape is surprising or
commercially important — not by default.

**One canonical location per example.** The runnable file under `examples/` is
canonical; the companion links to it and may quote a short excerpt only where
that materially aids comprehension. CI compiles the file that ships with the
skill, so markdown and file cannot diverge.

**The checkers need extending, and that is the real cost.**
`check-skill-snippets.py` supports Rust only *by design* — a non-Rust marker is
a hard error — and `check-skill-examples.sh` is hardcoded around the five
`hello.*` files. Either generalise to a manifest, or extend per known directory
structure. Decide before starting.

Report coverage with explicit `n/a`, so a missing example is never mistaken for
an unsupported binding:

```
announce:  Rust ✓  TS ✓  Python ✓  Go ✓   C ✓
nRPC:      Rust ✓  TS ✓  Python ✓  Go ✓   C n/a
A2A:       Rust ✓  TS ✓  Python ✓  Go not-exposed  C not-exposed
```

## Explicitly not doing

- **Five skills per language.** Duplicates doctrine, drifts on security and
  failure semantics, breaks cross-language projects, misrepresents the domain.
- **Auto-extracted multilingual type inventories.** A second synthetic SDK
  representation that cannot express what breaks generated code: that Node's
  `close()` must precede mesh shutdown, that Python's async iteration calls
  blocking FFI, that a payment gate returns a status object rather than
  throwing. Native compilers are already the authority and already in CI.
- **Splitting subsystem files by language.** `scheduler.md` is 94% shared;
  `nrpc.md`'s per-language content is operation-specific.

## Appendix — what CI proves today, precisely

Cited paths exist and are tracked; documented symbols resolve; enum variants
belong to their enum *and* match its shape; metric/config identifiers exist;
cross-binding vocabularies agree; marked Rust snippets compile; the docs site
carries the same path/variant/identifier/CLI checks.

Two honest qualifications, both corrected from R1:

- **Four hello-worlds gate publication, not five.** C, Go, Rust and Python run
  in `skills.yml`'s `examples` job, which `publish` needs. TypeScript is checked
  in `ci.yml` and therefore does **not** gate the mirror — Phase 0c closes this.
- **The checks catch *wrong*, not *incomplete*.** Only backticked, qualified
  `Enum::Variant` citations are seen, and a page listing three of four variants
  passes. That defect existed in `guides/gang-scheduler.md` and was found by
  reading.

The gap this plan closes is not correctness. It is that an agent loads five API
shapes to write one.
