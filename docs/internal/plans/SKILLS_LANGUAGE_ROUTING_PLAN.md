# Agent Skills — language routing

> Make the skills route by binding instead of presenting five API shapes at
> once. Adopts the reviewed recommendation (shared doctrine + thin binding
> companions + checked per-language examples), **scoped by measurement** — the
> uniform version would churn files that have nothing to gain.
> Companion to [`SKILLS_VERIFICATION_PLAN.md`](SKILLS_VERIFICATION_PLAN.md),
> which proves the skills match the tree; this one decides what an agent loads.

## Status

**Not started.** The architecture is agreed: shared conceptual skill, explicit
language routing, thin binding companions, checked examples per binding — not
five cloned skills, and not one giant multilingual file.

What follows is where the measurement changes the shape of the work.

## The measurement that scopes it

Per-language content is **not** evenly spread. Splitting every subsystem file
would be churn for most of them:

| File | Lines | Shared | Per-language | Worth splitting? |
|---|---|---|---|---|
| `apis.md` | 270 | 31% | **68%** | **Yes — this is the whole problem** |
| `runtime.md` | 344 | 63% | 36% | Partly |
| `nrpc.md` | 582 | 64% | 35% | No — see below |
| `scheduler.md` | 209 | **94%** | 5% | No |

86 per-language sections are spread across 12 files. But the 35% in `nrpc.md`
is **operation-specific** — "how Python does nRPC" — and under the review's own
definition that belongs in the subsystem file, not in `bindings/python.md`
("binding companions explain how that language expresses Net *generally*").
Moving it would scatter one subsystem across five files and lose the grouping
that makes the chapter readable.

`apis.md` is the opposite: its 68% *is* generic-language material — package
name, sync vs async construction, error shape, iterator semantics, lifecycle.
That is precisely a binding companion, currently welded into one file that a
Python reader loads to get 34 relevant lines out of 270.

**So the refactor is narrow: `apis.md` becomes the binding companions.** Most
subsystem files keep their per-language sections.

## Phase 1 — the binding capability matrix — **Priority 1, S**

The one genuinely new artifact, and the one the review is most right about:
curated, mechanically checked, more useful than any generated type inventory.

**Why it earns its place:** parity claims are currently prose scattered across
at least 8 files (`SKILL.md`, `bindings.md`, `nrpc.md`, `org.md`, `runtime.md`,
`observability.md`, `README.md`, …). They drift, and one already had:
`net-payments/bindings.md` said billing was Rust + Python when Node has
`readBilling` too — fixed during the 2c audit, but only because someone read it.

A single source with a check makes that class impossible.

**Shape** — statuses must distinguish absence from unsupported:

```
full | partial | verify-only | core-only | poll-only | not-exposed | n/a
```

`n/a` is load-bearing: "C has no A2A because A2A is not a C-shaped API" is a
different fact from "Go's A2A is missing," and a matrix that conflates them
tells an agent to attempt the wrong thing.

**Checked, not generated.** Generation cannot distinguish public-but-experimental
from supported, verifier-only from full, or shipped-dark from available. The
matrix is curated; the check asserts the *falsifiable* half — that a cell
claiming `full`/`partial` has a resolvable symbol behind it, and that a cell
claiming `not-exposed` has none.

**Done when:** one file is the source of truth, the skills' prose parity claims
point at it rather than restating it, and a check fails when a cell disagrees
with the tree.

## Phase 2 — `apis.md` → `bindings/*.md` — **Priority 1, M**

Split the 68% by language; keep the 31% shared as the routing page plus the
capability matrix. Each companion carries exactly what changes generated code:

1. package / import (they differ — `net-mesh-sdk` publishes, `net_sdk` imports)
2. construction + configuration, and whether it is async
3. runtime model, including blocking hidden behind an async surface
4. exact names and argument shapes (positional / kwargs / options object)
5. return and error behaviour — throws vs returns false vs encodes status
6. resource ownership and shutdown obligations
7. feature availability + known gaps, pointing at the matrix
8. one checked minimal example
9. where the authoritative binding source is
10. what must never be inferred from another binding

**Done when:** `apis.md` is a routing page, five companions exist, and the
existing checks still pass over the new layout.

## Phase 3 — routing rules in `SKILL.md` — **Priority 2, S**

The skill already says "identify the language" as workflow step 1. Make it
binding-selection rather than a passing instruction:

1. inspect the project manifest to determine the actual binding;
2. load that companion before generating;
3. consult the matrix before promising a surface exists;
4. never default to Rust because Rust is the substrate;
5. when the surface is still uncertain, read the binding's own source/types;
6. with no project context and no language named, **ask** — this is one of the
   few questions worth blocking on.

## Phase 4 — per-language example coverage — **Priority 2, M**

Today: one hello-world per language, all compile-checked (four gate `publish`,
TypeScript gates via `ci.yml`). The review wants a canonical checked example per
*important operation* per binding, with docs and skills pointing at them rather
than keeping unrelated copies.

Report coverage with explicit `n/a`, so a missing example is never mistaken for
an unsupported binding:

```
announce:  Rust ✓  TS ✓  Python ✓  Go ✓   C ✓
nRPC:      Rust ✓  TS ✓  Python ✓  Go ✓   C n/a
A2A:       Rust ✓  TS ✓  Python ✓  Go n/a C n/a
```

Sequenced last: it is the largest, and it is worth far more once Phase 2 gives
each example an obvious home.

## Explicitly not doing

- **Five skills per language.** Duplicates doctrine, drifts on security and
  failure semantics, breaks cross-language projects, and misrepresents the
  domain — the domain is the event bus, not the language.
- **Auto-extracted multilingual type inventories.** A second synthetic SDK
  representation, noisy, and unable to express the things that actually break
  generated code: that Node's `close()` must precede mesh shutdown, that
  Python's async iteration calls blocking FFI, that a payment gate returns a
  status object rather than throwing. Native compilers and type-checkers are
  already the authority and are already wired into CI.
- **Splitting subsystem files by language.** `scheduler.md` is 94% shared;
  `nrpc.md`'s per-language content is operation-specific. Only `apis.md` earns
  it.

## Appendix — what CI already proves

Relevant because it decides how much the companions must assert rather than
demonstrate: cited paths exist and are tracked; documented symbols resolve; enum
variants belong to their enum *and* match its shape; metric/config identifiers
exist; cross-binding vocabularies agree; five hello-worlds compile or
type-check; marked snippets compile; publication is blocked when verification is
red; and the docs site has the same path/variant/identifier/CLI checks.

The gap this plan closes is not correctness. It is that an agent currently loads
five API shapes to write one.
