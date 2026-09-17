# README Refresh Plan

**Status:** DRAFT (2026-09-18). A read-only audit of all **28** `README.md` files outside
`web/` is complete and its findings are folded in below. **Nothing is edited yet.** Two
decisions are open (D1, D2), each with a recommendation; the rest of the plan is
independent of them.

This plan covers the `README.md` layer of the documentation surface — the files that are
rendered as package landing pages on crates.io, npm, PyPI and pkg.go.dev, and read from the
repository tree. It is adjacent to, and does not overlap, the docs-site work:
`DOCS_POLYGLOT_LENS_PLAN.md` owns `web/src/content/docs/` and never touches these files;
`DOCS_STRATEGY_PLAN.md` and `DOCS_SDK_SPINE_PLAN.md` own positioning and the SDK spine on the
site. Where a README and the site disagree about a fact, the **code** is the source of truth
and both sides are wrong independently.

> **Framing — the READMEs are not stale by neglect, they are stale by construction.** Each was
> written against the surface at the time its component landed and then diverged as the code
> moved underneath it. The audit found **six** READMEs that ship the wrong license, **four**
> documents whose tables contradict the CLI/Deck they describe, **two** code samples that do
> not compile as written, and a small set of counts, paths and feature names that no longer
> resolve. None of this is a rewrite: every file has a correct skeleton and a bounded list of
> corrections. The work is a consistency pass with a verification harness, not a re-authoring.

## Goals

- Every factual claim in a README — package name, install command, feature name, command flag,
  subcommand, tab label, file path, tool count, version — matches the code or manifest it
  describes, or is removed.
- License treatment is uniform: every crate/package/binding README states `MIT OR Apache-2.0`
  and links the repo-root license files at the correct relative depth. No README says bare
  `Apache-2.0`.
- Every link resolves: doc-site links have a matching page under `web/src/content/docs/`;
  in-repo links resolve from the file's own directory.
- Every executable sample (Go, Python, TypeScript, Rust) is valid against the exported surface
  of the package it names.
- The CLI and Deck README families (`cli/` + `cli/npm/` + `cli/python/`, `deck/` + `deck/npm/`
  + `deck/python/`) are in lockstep — no shared block drifts between siblings.
- A verification harness exists that checks the above mechanically, so the next drift is caught
  rather than re-discovered by a read-through.

## Non-goals

- **`web/`.** The site, its content pages (`web/src/content/docs/`), and its 18 content
  READMEs are out of scope. This plan links *to* those pages; it does not edit them.
- **Non-README files.** Two adjacent defects surfaced during the audit and are filed
  separately (see end of plan): `integrations/hermes/plugin.yaml` at version `0.35.0`, and a
  stale fixture count in `web/src/content/docs/releases/RELEASE_v0.13_CHIPPIN_IN.md` (web/).
- **Re-authoring or re-positioning.** No README is restructured, re-titled, or rewritten for
  voice. Corrections only, plus the deduplication named in Stage 1.
- **Adding new documentation content.** If a README omits a feature, this plan does not
  invent a section for it; it only fixes claims that are present and wrong.
- **Translating READMEs into the docs spine.** `DOCS_SDK_SPINE_PLAN.md` owns the site's SDK
  spine; READMEs stay READMEs.
- **Badge/CI-status policy.** Where a badge is present it must point at the right package;
  adding badges where none exist is not proposed.

---

## Scope — 28 files

| Tier | Files | Count |
|---|---|---|
| A. Root + meta | `README.md`; `net/crates/net/README.md`; `docs/internal/README.md`; `docs/data/README.md` | 4 |
| B. SDK surfaces | `sdk/`; `sdk-ts/`; `sdk-py/`; `sdk-macros/`; `bindings/python/`; `bindings/python/src/` | 6 |
| C. CLI / Deck + packaging | `cli/`; `cli/npm/`; `cli/python/`; `deck/`; `deck/npm/`; `deck/python/` | 6 |
| D. Bindings / C / Go / integrations / skills | `go/`; `include/`; `bindings/node/`; `bindings/go/net/`; `integrations/hermes/`; `.claude/skills/`; `.claude/skills/net-event-bus/examples/` | 7 |
| E. Tests / tools / demos | `tools/binary-size/`; `tests/natsim/`; `tests/cross_lang_payments/`; `tests/cross_lang_capability/`; `docs/demos/dir_transfer/` | 5 |

## Pre-flight — complete

The audit ran as four read-only reconnaissance passes over the five tiers, each verifying
claims against the working tree rather than against other prose: `Cargo.toml` `[features]` and
package names, `package.json` / `pyproject.toml` metadata, `go.mod`, the `include/*.h` tree,
the `cli/` and `deck/` clap/enum sources, the fixtures and example dirs on disk, and the
`web/src/content/docs/` page tree (including the adaptive-route projection in
`web/src/lib/docs.ts`). Findings carry `path:line` evidence.

Confirmed invariants the plan relies on:

- All published surfaces are lockstep **0.36.0** (`sdk/Cargo.toml`, `sdk-ts/package.json`,
  `sdk-py/pyproject.toml`, `bindings/node/package.json`, `cli/npm|python`, `deck/npm|python`,
  `bindings/python/pyproject.toml`).
- Dual license `MIT OR Apache-2.0` is declared in every crate/package/pyproject manifest.
- `net/crates/net/include/` holds **11** `.h` files; `net/crates/net/examples/` holds **8**
  `.c` examples.
- Version strings and cross-links in READMEs are unqualified by release; the only pinned
  version in any README is `sdk-macros/README.md`'s `0.24` — stale (see Stage 3).

**Baseline size.** 28 files, ≈231 KB, last touched between 2026-05-17 and 2026-08-10 — none
refreshed since before the current cut.

---

## Decisions

**D1 — Until now, packaging READMEs duplicate the parent README's shared prose (byte-identical
blocks), with only the install section swapped.** Options: *(a)* keep the duplication and
enforce a mirror rule; *(b)* slim `cli/npm|python` and `deck/npm|python` to install + a short
header, deleting the shared tables.

**Choose (a).** The registry README *is* the package page on npm/PyPI; a reader there never
sees the parent README, so deleting the tables removes the only copy they will read. Keep the
duplication, but make it enforceable: the two CLI siblings must byte-match the parent outside
the install block, and likewise the two Deck siblings. Stage 1's checker asserts this, which is
what makes the duplication safe rather than a standing drift hazard.

**D2 — Whether the Stage 5 checkers become a CI job.** Options: *(a)* throwaway scripts run at
the end of this work; *(b)* a permanent `.github/scripts/check-readmes.py` pinned in `ci.yml`.

**Recommend (b), scoped.** The repository already treats drift as a CI problem
(`check-spine-symbols.py`, the routing-witness floors, the integration-test pin guard), and the
link/table checks are cheap and deterministic. Pin the *link-resolution* and *table-vs-source*
checks only; do not gate on prose. Because `ci.yml` is path-filtered on `net/**`, a README
change already triggers the relevant jobs. Left open for the owner; Stages 1–4 proceed either
way.

---

## Stage 1 — Conventions and mechanical corrections

**Cost:** ½ day.
**Output:** license and link treatment made uniform; two duplicate blocks removed; one stale
version fixed.

**Deliverables:**

1. **License (6 files).** Replace the bare `Apache-2.0` statement with `MIT OR Apache-2.0` and
   license links, matching the package metadata:
   - `net/crates/net/cli/npm/README.md` (package.json: `MIT OR Apache-2.0`)
   - `net/crates/net/cli/python/README.md` (pyproject: `MIT OR Apache-2.0`)
   - `net/crates/net/deck/npm/README.md`
   - `net/crates/net/deck/python/README.md`
   - `net/crates/net/bindings/node/README.md`
   - `net/crates/net/bindings/go/net/README.md`

2. **Broken license links (1 file).** `.claude/skills/README.md` links `LICENSE-APACHE` /
   `LICENSE-MIT` relative to its own directory, where neither exists (the dir holds only
   `README.md`, `net-event-bus/`, `net-payments/`). Add the two files, or point at the
   repo-root copies.

3. **Duplicate blocks (1 file).** `README.md` repeats the `opensrc` fetch block — once inline
   after the skills block, once under `### Give the agent the source too`. Delete one.

4. **Link style (1 file).** `README.md:14` links
   `https://github.com/ai-2070/net/blob/master/web/src/content/docs/worldview` — a directory,
   which `/blob/` cannot serve. Replace with a relative path or the docs-site URL.

5. **Stale version (1 file).** `net/crates/net/sdk-macros/README.md` pins
   `net-mesh-sdk = { version = "0.24", … }`; the workspace is `0.36.0`. Update.

6. **Wrong in-repo path (1 file).** `README.md:243` cites `bindings/coverage.md`, which exists
   only under `.claude/skills/net-event-bus/bindings/` and `.claude/skills/net-payments/bindings/`.
   Give the qualified path.

**Acceptance.** No README contains a bare `Apache-2.0` license claim; no in-repo link in the
touched files points at a non-existent path.

**Risk.** Low. Every change is a string substitution with a manifest or a directory listing as
the oracle.

---

## Stage 2 — Factual corrections to code-facing claims

**Cost:** 1–1½ days, concentrated in the CLI/Deck and bindings tables.
**Output:** every table/list in the affected READMEs matches the code it describes.

### 2a. Crate README (`net/crates/net/README.md`)

| Claim | Reality | Fix |
|---|---|---|
| "No features are enabled by default." | `Cargo.toml:102` `default = ["net","nat-traversal","cortex","meshdb","meshos","dataforts"]` | restate the default set |
| `cargo build --release  # core only` | a plain release build uses the defaults | `--no-default-features` for core-only |
| Feature table | missing `tool`, `batched-ingress`, `fixtures`; `net`/`dataforts`/`meshos` dep chains wrong | regenerate from `[features]` |

### 2b. CLI family (`cli/README.md` first, then mirror into `cli/npm/`, `cli/python/`)

| Claim | Reality | Fix |
|---|---|---|
| Subcommand table (18 rows) | omits `org`, `node`, `transfer`, `wrap`, `mcp`, `forwarding`, `typegen` | add rows |
| `ice` = "force-drain / evict / restart / cutover" | no `force-drain`; real verbs are `freeze-cluster`, `thaw-cluster`, `flush-avoid-lists`, `force-evict-replica`, `force-restart-daemon`, `force-cutover`, `kill-migration` | rewrite row |
| `snapshot … (--watch)` | `SnapshotCommand` = Get \| Status only | delete clause |
| `log tail … --level` | flag is `--min-level` | rename |
| Global flags (7) | omit `--insecure-config-permissions` `[NET_MESH_INSECURE_CONFIG_PERMISSIONS]` | add |
| Exit-code table | contradicts `src/error.rs::ExitCodeKind` (3/4/5/6 meanings wrong; 1, 7, 8, 10–14 missing) | rewrite from the enum |

### 2c. Deck family (`deck/README.md` first, then mirror into `deck/npm/`, `deck/python/`)

| Claim | Reality | Fix |
|---|---|---|
| Tab `REPLICAS` | label is `CHAINS` (`app.rs`) | rename row |
| Tab `FAILURES` | hidden/dormant (absent from `Tab::all()`) | drop or mark hidden |
| ICE list | no `force-drain`; add `flush-avoid-lists`, `kill-migration` | correct list |
| "Multi-operator signing and lockout timers are available" | runtime wires a single operator keypair; no such code | delete or mark unimplemented |
| `$XDG_CONFIG_HOME/deck/bookmarks.toml` | source pushes `net-deck` → `net-deck/bookmarks.toml` | fix path |
| "a malformed file is surfaced via stderr" | a corrupt file is renamed aside (`<path>.corrupt-<ms>`) and an empty store returned | reword |

### 2d. Bindings / integrations

| File | Claim | Reality | Fix |
|---|---|---|---|
| `bindings/node/README.md` | "the five feature flags" (table lists 7) | mismatch | count |
| `bindings/node/README.md` | `netdb` feature in table + `napi build --features` | no `netdb` feature (`cortex` implies it) | drop |
| `bindings/node/README.md` | `libnet_meshdb` / `libnet_meshos` cdylibs | shims are rlib-only, linked into one `libnet` | remove |
| `bindings/node/README.md` | build feature list | omits `aggregator,delegation,a2a,org` | quote `package.json` |
| `bindings/go/net/README.md` | `libnet_meshdb` / `libnet_meshos` cdylibs | same single-cdylib doctrine | remove |
| `integrations/hermes/README.md` | "five `net_*` tools" | plugin registers 11 | update lede, table, test claim |
| `tests/cross_lang_capability/README.md` | "the substrate doesn't ship a redaction implementation" | C ABI ships `net_predicate_redact_metadata_keys` | reword to "no Rust-SDK-level redaction; the C ABI and bindings implement it host-side" |

### 2e. Meta docs

| File | Finding | Fix |
|---|---|---|
| `docs/internal/README.md` | folder table omits `reviews/`, `audits/`; "244 files / 7 MB" and migration-path list stale | add rows; recount or drop figures |
| `docs/data/README.md` | "`docs/` … contained only `internal/`" false (`docs/misc/` exists); record table omits `spine-symbols.yaml` | reword; add row |

**Acceptance.** For CLI/Deck, the tables diff cleanly against the clap `Command` enum, the
`Cli` global-flags struct, `ExitCodeKind`, and the Deck `Tab` enum/labels. For the feature
tables, against `Cargo.toml` / `package.json`.

**Risk.** Medium for the CLI/Deck tables — the mirror rule means each parent edit must be
replicated into two siblings. Stage 5's byte-match check is the guard; do the parent first and
propagate in the same commit.

---

## Stage 3 — Executable samples

**Cost:** ½ day.
**Output:** every code snippet in scope compiles/runs against the named package.

| File | Defect | Fix |
|---|---|---|
| `go/README.md` (`:17`, `:156-159`) | `net.CallTool[Req,Resp](ctx, rpc, …)` passes a `*MeshRpc`; `CallTool` requires `*TypedMeshRpc` | show `NewTypedMeshRpc(NewMeshRpc(node))`; keep `ListTools()` on the raw handle |
| `sdk-py/README.md` | `serve_tool(node, …)` / `call_tool(node, …)` pass a node; helpers take a `TypedMeshRpc` | construct `rpc = TypedMeshRpc(node)`; `list_tools(node)` / `watch_tools(node)` stay |
| `sdk-ts/README.md` | slim-build `napi build --features "cortex netdb redex-disk meshdb meshos"` — the Node crate has neither feature | use real features (`cortex meshdb meshos`) |
| `sdk-ts/README.md` | "all 32 classes" | drop the number or recount |
| `bindings/python/src/README.md` | names `cortex_error_to_pyerr`, which does not exist | name the real converters |
| `bindings/python/src/README.md` | async-iterator template shows a bare `future_into_py` `__anext__` with constructor-armed cancel | rewrite to thread the token via `await_with_existing_token`; fix rule 4 |
| `sdk/README.md` | Discover sample binds `host` then calls `agent.list_tools` | unify the receiver |

**Manifest defect fixed in the same stage (not a README):** `sdk-py/pyproject.toml` has no
`readme` field, so the PyPI long-description is blank. Add `readme = "README.md"`.

**Acceptance.** The Go and Python snippets compile against `go/` and `bindings/python/`
respectively (build once, run the sample); the TS `napi` invocation is accepted by cargo
(feature set exists); the pyproject metadata reports a non-empty description.

**Risk.** Low–medium. `sdk-py` and `go` samples need a real build to prove; the bindings/python
internals doc needs a source read, not a build.

---

## Stage 4 — Docs-guide link parity and remaining structure

**Cost:** ½ day.
**Output:** the SDK READMEs link a consistent, complete guide set; the last two relative-path
defects are fixed.

1. **SDK guide-line parity.** The four SDK-facing READMEs (`sdk/`, `sdk-ts/`, `sdk-py/`,
   `bindings/python/`) link a **different subset** of the guides. Canonical set per lens = the
   union of guides that exist under `web/src/content/docs/guides/` plus
   `worldview/right-and-wrong-use-cases` and `start/claude-skills`. Additions:
   - all four: `production-deployment` (linked by none today)
   - `sdk-ts/`, `sdk-py/`: `gang-scheduler`, `task-lifecycle`
   - `bindings/python/`: `gang-scheduler`, `task-lifecycle`, `wrap-mcp-server`,
     `expose-net-as-mcp`, `private-capabilities`, `right-and-wrong-use-cases`,
     `start/claude-skills`, and a `## Links` section (the only SDK README without one)
   - `bindings/python/`: note that its quickstart link documents the `net_sdk` wrapper

2. **`include/README.md`** — example paths say `examples/basic.c` etc., but that directory does
   not exist; the files are at `net/crates/net/examples/` (and there are 8, not 3). Use
   `../examples/*.c`.

3. **`docs/demos/dir_transfer/README.md`** — `docs/cli/TRANSFER.md` is written
   crate-root-relative but resolves from `docs/demos/dir_transfer/`; fix to
   `../../cli/TRANSFER.md`, and link `FETCH_DIR_ATOMIC_PLAN.md` properly.

4. **`.claude/skills/README.md`** — the skill file maps omit `subnet-auth.md`,
   `source-access.md`, and each skill's `bindings/` directory. Add rows.

5. **`tools/binary-size/README.md`** — add the caveat that `measure_sizes.sh` is zsh + BSD
   `stat` + `.dylib` (macOS-only).

6. **`tests/natsim/README.md`**, **`tests/cross_lang_payments/README.md`** — audited clean;
   re-verify in Stage 5 only.

**Acceptance.** Each SDK README's guide links are a superset of the canonical set; the link
checker in Stage 5 reports zero unresolved relative paths and zero doc-site 404s.

**Risk.** Low. Link additions only.

---

## Stage 5 — Verification harness

**Cost:** ½ day.
**Output:** a script (throwaway, or promoted to CI per D2) that fails loudly on drift.

**Checks:**

1. **Link resolution.** Extract every Markdown link from all 28 files. Absolute URLs → HEAD/GET
   (doc-site links resolve through the adaptive route projection in `web/src/lib/docs.ts`;
   spot-verified live during the audit). Relative paths → `os.path.exists` resolved against the
   *file's own* directory.
2. **CLI table.** Build `net-mesh` once; diff the README subcommand table, global-flags list and
   exit-code table against `--help`, the `Cli` struct, and `ExitCodeKind`.
3. **Deck table.** Diff the Tabs table against the `Tab` enum labels.
4. **Feature tables.** Diff each README feature matrix against `Cargo.toml [features]` /
   `package.json`.
5. **Sibling byte-match (D1).** Assert `cli/npm` + `cli/python` match `cli/` outside the install
   block, and likewise for the Deck family.
6. **Packaging.** `cargo package --list -p net-mesh -p net-mesh-sdk -p net-mesh-sdk-macros` and a
   `pyproject.toml` metadata read to confirm every README is the declared long-description.

**Acceptance.** All six checks green. If D2 is taken, `.github/scripts/check-readmes.py` is
pinned in `ci.yml` and the failing-case names are specific (per AGENTS.md's
no-silent-skip culture: `--no-tests=fail`-style — never a check that passes by matching
nothing).

---

## Sequencing and rollback

| Stage | Lands as | Reversible by |
|---|---|---|
| 1 | one PR, mechanical string edits across 8 files | revert the PR |
| 2 | two PRs: crate+meta; CLI+Deck families (+bindings/integrations) | revert per PR |
| 3 | one PR, samples + the `sdk-py` manifest field | revert the PR |
| 4 | one PR, link additions and path fixes | revert the PR |
| 5 | scripts + optional CI job (D2) | drop the job / the scripts |

Budget: **2½–3 person-days** across five stages. Stage 1 is independently valuable (it fixes
the wrong-license and broken-link surface with no judgment calls); Stage 2 is the bulk; Stages
3–4 are bounded; Stage 5 is the ratchet.

**Sibling-lockstep rule.** Within Stage 2, edit `cli/README.md` and `deck/README.md` first, then
propagate the identical shared blocks into their npm/Python siblings in the same commit. Do not
edit a sibling before its parent.

---

## What this plan does NOT address (cross-references)

- **Docs-site content.** `DOCS_POLYGLOT_LENS_PLAN.md` (composition, routing, the SDK spine),
  `DOCS_STRATEGY_PLAN.md` (positioning, worldview), `DOCS_SDK_SPINE_PLAN.md`.
- **Skill content.** `SKILLS_LANGUAGE_ROUTING_PLAN.md` and `SKILLS_VERIFICATION_PLAN.md` own the
  skill bodies; this plan only fixes the skill directory READMEs' links and maps.
- **CI lint/format discipline.** `PANIC_AUDIT_AND_LINT_HARDENING_PLAN.md` owns the lint gates;
  D2 here would add one more checker script of the `check-spine-symbols.py` kind.
- **Package publishing.** No manifest is changed except the missing `sdk-py` `readme` field,
  which is a packaging defect the audit surfaced, not a version or dependency change.
- **`web/` release-note fixture count.** `RELEASE_v0.13_CHIPPIN_IN.md` states "thirteen
  golden-vector fixtures" for `tests/cross_lang_capability/`, which holds 8. Out of scope
  (web/); file separately.

## Adjacent defects (filed separately, not part of this pass)

| File | Defect |
|---|---|
| `net/crates/net/integrations/hermes/plugin.yaml` | version pinned at `0.35.0` while the tree is `0.36.0` |
| `web/src/content/docs/releases/RELEASE_v0.13_CHIPPIN_IN.md` | fixture count stale (web/) |
