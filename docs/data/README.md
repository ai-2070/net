# `docs/data` — public product metadata

**This directory is public product truth, not internal notes.** It is a sibling of
`docs/internal/`, not part of it. `docs/` happens to have contained only
`internal/` until now, so that has to be said explicitly rather than inferred from
the parent.

Everything here is **authored once and consumed several times**. The rule that
makes it worth having:

> One authored record per domain. Any portable copy is generated and
> equality-checked in CI.

Two hand-maintained copies of the same fact diverge; within a quarter you have two
different answers to "does Go support this" and no way to tell which is current.

| File | Is the record for | Consumed by |
|---|---|---|
| `capabilities/<domain>.yaml` | which binding supports which operation, and why not when it does not | the docs' support badges and absence states, each skill's `bindings/coverage.md`, later a public parity page |
| `examples.yaml` | which examples exist, where their source lives, and what CI proves about each | the docs' transcluded snippets, `check-skill-examples.sh`, `run-skill-examples.sh` |
| `tiers.yaml` | every docs page's migration state, plus the `adaptive_pending` allowlist | the polyglot-lens checkers, the information-model table in the plan |

**Example source does not live here.** Only the index does. Runnable examples stay
where each language's tooling expects them — a Rust example in the crate's
`examples/`, a Go example in the module — because relocating them for conceptual
symmetry costs package-local discoverability, native build integration, release
coupling and first-party provenance. `examples.yaml` points at them.

Reader-facing prose in these files (an absence `reason`, an `alternative.label`) is
product-contract prose. It is reviewed in the same change as the semantic status it
explains. Generated copies have no independent authorship or review path.

Governed by `docs/internal/plans/DOCS_POLYGLOT_LENS_PLAN.md`.
