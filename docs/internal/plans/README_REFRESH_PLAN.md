# README Refresh Plan

**Status:** EXECUTED (2026-09-18) — Stages 0–4. Stage 0 (root `README.md`) landed; Stages 1–4
applied across the accuracy workstream's files — the 27 non-root READMEs minus the two audited
clean, plus `sdk-py/pyproject.toml`. Stage 5 is **partial**: the checker is committed at
`.github/scripts/check-readmes.py` and passes (29 files, offline checks), but it is **not wired
into CI** while D2 stays open, so the verification goal is not yet closed. The one-shot run
reported 69 relative links resolving, 0 residual overclaims, license uniform, CLI/Deck sibling
blocks byte-identical, and 55 docs-site URLs returning 200.

This plan covers the `README.md` layer of the documentation surface — the files rendered as
package landing pages on crates.io, npm, PyPI and pkg.go.dev, and read from the repository
tree. It is adjacent to, and does not overlap, the docs-site work:
`DOCS_POLYGLOT_LENS_PLAN.md` owns `web/src/content/docs/` and never touches these files.

> **Framing — there are two different jobs here, and the first draft treated them as one.**
> The accuracy pass is bounded: the audit found **six** READMEs shipping the wrong license,
> **four** documents whose tables contradict the CLI/Deck they describe, **two** code samples
> that do not compile, and a set of counts, paths and feature names that no longer resolve.
> None of that is a rewrite. But the repository **front door is a different problem**: the
> root README asks a cold reader to adjudicate a sweeping claim about networking before it
> shows them one thing they can build, and it spends its strongest evidence (nanoseconds vs
> milliseconds) in the opening line, where it is least defensible and most likely to be
> rejected. That is positioning work, not hygiene — and `DOCS_STRATEGY_PLAN.md:41–55`
> already froze a layered positioning decision whose *Reconciliation obligation* explicitly
> names the root README: *"Do not let the docs quietly re-position the company while the
> README says something else."* This plan honors that obligation instead of deferring it.

**Review disposition (2026-09-18).** A positioning review of this plan returned
*approve as a maintenance pass, not as the refresh Net needs from a positioning
standpoint*, with six changes. All are applied:

| Review item | Verdict | Where it landed |
|---|---|---|
| The plan excludes the highest-value work (root positioning) | **accept** | new Stage 0; the blanket "no restructure/voice" non-goal is scoped to non-root files |
| Lead with federation; architecture is the reason to believe | **accept** | Stage 0 — sequence + copy direction |
| The opening spends credibility (impossible-claim, ms vs ns, ns failure detection) | **accept** | Stage 0 — scope claims at first appearance |
| Show one concrete system before the component catalogue | **accept** | Stage 0 — the worked example requirement |
| D1: enforce shared *facts*, not whole-presentation byte-match | **modify** | D1 rewritten |
| Stage 4: reject "every README links the union of guides" | **modify** | Stage 4 rewritten to a prioritized route |

## Goals

**Accuracy (Stages 1–5):**

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
- A verification harness exists so the next drift is caught mechanically.

**Positioning (Stage 0):**

- A cold reader understands what Net enables and why they might need it **before** being asked
  to accept an architectural argument.
- One concrete working system carries the central value — caller receives a typed result, the
  failure path is shown, and the provider's owner retains access control.
- Performance and recovery claims are scoped where they **first appear**, not qualified several
  screens below.
- The product reads as something that fits an existing estate, not a demand to replace TCP,
  brokers, registries and cloud wholesale.
- Every package README offers one clear next action.
- The brand stays **Net**, the identity stays **the mesh**; the agentic use case is the entry
  point, not a rename.

## Non-goals

- **`web/`.** The site, its content pages, and its 14 content READMEs are out of scope. This
  plan links *to* those pages; it does not edit them.
- **Non-README files.** Two adjacent defects surfaced and are filed separately (end of plan).
- **Restructuring non-root READMEs.** Stage 0 restructures the root front door only. The other
  27 files get corrections, not re-authoring.
- **Renaming Net or reframing it as an MCP utility.** The mesh stays the identity; agent
  capability federation is the wedge, per `DOCS_STRATEGY_PLAN.md:41–55`.
- **Adding new documentation content** beyond the Stage 0 example. If a README omits a feature,
  this plan does not invent a section for it.
- **Translating READMEs into the docs spine.** `DOCS_SDK_SPINE_PLAN.md` owns the site spine.
- **Badge/CI-status policy.** Where a badge exists it must point at the right package; adding
  badges where none exist is not proposed.

---

## Scope — 28 files

| Tier | Files | Count | Workstream |
|---|---|---|---|
| A. Root + meta | `README.md`; `net/crates/net/README.md`; `docs/internal/README.md`; `docs/data/README.md` | 4 | **Positioning** (root) + accuracy |
| B. SDK surfaces | `sdk/`; `sdk-ts/`; `sdk-py/`; `sdk-macros/`; `bindings/python/`; `bindings/python/src/` | 6 | accuracy |
| C. CLI / Deck + packaging | `cli/`; `cli/npm/`; `cli/python/`; `deck/`; `deck/npm/`; `deck/python/` | 6 | accuracy |
| D. Bindings / C / Go / integrations / skills | `go/`; `include/`; `bindings/node/`; `bindings/go/net/`; `integrations/hermes/`; `.claude/skills/`; `.claude/skills/net-event-bus/examples/` | 7 | accuracy |
| E. Tests / tools / demos | `tools/binary-size/`; `tests/natsim/`; `tests/cross_lang_payments/`; `tests/cross_lang_capability/`; `docs/demos/dir_transfer/` | 5 | accuracy |

## Pre-flight — complete

The audit ran as four read-only reconnaissance passes over the five tiers, each verifying
claims against the working tree rather than against other prose: `Cargo.toml` `[features]` and
package names, `package.json` / `pyproject.toml` metadata, `go.mod`, the `include/*.h` tree,
the `cli/` and `deck/` clap/enum sources, the fixtures and example dirs on disk, and the
`web/src/content/docs/` page tree (including the adaptive-route projection in
`web/src/lib/docs.ts`). Findings carry `path:line` evidence.

The positioning review separately read the root README, `DOCS_STRATEGY_PLAN.md:31–55`, and
representative root/package READMEs. Its six changes are dispositioned above.

Confirmed invariants the plan relies on:

- All published surfaces are lockstep **0.36.0**.
- Dual license `MIT OR Apache-2.0` is declared in every manifest.
- `net/crates/net/include/` holds **11** `.h` files; `net/crates/net/examples/` holds **8** `.c`
  examples.
- The only pinned version in any README is `sdk-macros/README.md`'s `0.24` — stale.

**Baseline size.** 28 files, ≈231 KB, last touched between 2026-05-17 and 2026-08-10 — none
refreshed since before the current cut.

---

## Decisions

**D1 — Registry-page self-containment.** The packaging READMEs (`cli/npm|python`, `deck/npm|python`)
currently duplicate the parent README's shared prose byte-for-byte with only the install section
swapped. The first draft proposed keeping the full duplication and enforcing a whole-file
byte-match; the review rejected that: *readers need a complete package page, but every registry
does not need the parent's entire presentation.*

**Choose (modified): shared facts, correct per registry — not equal across registries.** Registry
pages stay self-contained. The enforced invariant is that each page is **correct for its own
registry**: versions, licensing, product facts and the designated shared blocks (e.g. the
subcommand/tab table) *agree* across the family, while package identifiers and install commands
**differ by design** — npm, PyPI and Cargo have different commands and possibly different
identifiers — and are checked against that registry's own manifest. The capability-federation
one-liner is not compulsory identical prose for the CLI and Deck pages; those lead with their
operator-facing purpose. Stage 5 checks facts per registry, not command equality.

**D2 — Whether the Stage 5 checkers become a CI job.** Options: *(a)* throwaway scripts;
*(b)* a permanent `.github/scripts/check-readmes.py` pinned in `ci.yml`.

**Recommend (b), scoped.** The repository already treats drift as a CI problem
(`check-spine-symbols.py`, the routing-witness floors). Pin only the *link-resolution* and
*facts-vs-source* checks; do not gate on prose or presentation. Left open; Stages 0–4 proceed
either way.

---

## Stage 0 — Root README positioning rewrite

**Cost:** 1–1½ days (copy plus one executive review pass).
**Output:** a restructured `README.md` front door that leads with federation, shows one working
system, and scopes its claims where they first appear.

**Evidence the rewrite is answering** (line refs are on the **pre-refresh** root README —
baseline commit `49abe7203`; the rewrite has since renumbered the file):

| Current material | Positioning consequence |
|---|---|
| `README.md:12` "…systems engineers said was impossible" | asks the reader to accept an unnamed adversary and an extraordinary victory |
| `README.md:12` "Existing networks operate in milliseconds (10⁻³). Net operates in nanoseconds (10⁻⁹)." | invites an apples-to-oranges objection before the benchmark scope is stated |
| `README.md:183–193` nanosecond-scale failure detection / sub-microsecond fail-and-recover | reads as a distributed operational promise; in fact it is local decision computation |
| `README.md:542` "All numbers below measure **packet scheduling** … they do not include NIC transfer, wire latency, or speed-of-light propagation" | the correct qualification — buried several screens below the claim it qualifies |
| repeated arguments against TCP, brokers, registries, cloud | reads as a demand to replace everything, rather than a system that fits an estate |

**Required outcomes (from the review):**

1. **Lead with capability federation; let architecture be the reason to believe.** Preserve the
   layered positioning frozen in `DOCS_STRATEGY_PLAN.md:41–55` — immediate agentic use case on
   top, broader substrate underneath — without compressing Net into "just discovery." The
   differentiator is that discovery, invocation, identity, authority and artifact movement are
   **one coherent system**, not adapters the customer assembles.

2. **One concrete system that demonstrates federation — not merely remote RPC.** A provider
   returning a typed result is something many systems can do; the example must make the
   distinction visible: the caller **selects a capability rather than hardcoding a machine**, the
   provider **decides whether the caller is authorized**, the work **runs where the resource or
   credential lives**, and a useful result — ideally an artifact — comes back. One coherent
   scenario, not a feature obstacle course; a document-processing capability on another machine
   is preferred if it demonstrates these through shipped APIs without elaborate setup (a GPU
   example is valuable only if GPU setup does not become the reader's first problem). The
   failure path shows a real, understandable outcome — **access denied** or **provider
   unavailable** — not a manufactured transparent-recovery promise. No channels/nRPC/subnets/
   daemons/Dataforts catalogue before this.

3. **Correct the performance claims, don't relabel them.** Processing a heartbeat, evaluating a
   timeout, selecting an alternate, detecting a *remote* failure, and completing recovery are
   **different measurements**; the writer must identify what each benchmark actually exercises
   and name that operation. Move the `README.md:542` scheduling-vs-wire qualification up to the
   first performance claim (`README.md:12`). The rerouting passage (`README.md:183–193`) must not
   present local decision latency as distributed recovery. The "Net wins by 100x or more"
   composed-workflow claim needs its own supporting comparison — the packet-scheduling
   disclaimer does not establish it; **unsupported headline claims are removed pending
   evidence**. Keep the performance story; precision makes it stronger.

4. **Reposition the Cyberpunk origin and the affiliation disclaimer** below the product
   explanation. Keep the aesthetic; stop making the licensing disclaimer the first substantive
   introduction.

**Proposed sequence** (a reading of the review's preferred order; the exact ordering is still
open and should be confirmed before copy is finalized). **The writer has permission to select,
compress, and link out** — the root demonstrates the breadth of the system without explaining
every subsystem at reference depth. The test for each retained section is not *did we preserve
it?* but *does it help someone understand, evaluate, or start using Net?*

1. One-sentence what + the capability-federation lead.
2. The concrete worked system (outcome, failure path, authority).
3. Install / quickstart per language.
4. Why the pieces compose — identity, discovery, typed RPC, streams, durable state, artifacts on
   one substrate.
5. The architecture / why the mesh — the reason to believe, with claims scoped inline.
6. **Selected breadth** — a compressed tour of the subsystem surface that links out to the docs
   for detail, not a reference-depth catalogue.
7. **A small set of meaningful performance evidence**, scoped to the operations actually
   measured, linking to `BENCHMARKS.md` for the full material.
8. Origin + Cyberpunk character + disclaimer.
9. License.

**Copy direction** (from the review; a direction, not final copy):

> Net connects agents, services, and devices into a capability mesh.
>
> Discover what another machine can do, invoke it through typed RPC, and move artifacts between
> participants while resource owners retain control over access.
>
> Underneath is a latency-first encrypted mesh, with shared primitives for identity, discovery,
> streams, durable state, and execution across heterogeneous machines.

**Acceptance.** A cold reader understands what Net enables and why they might need it; one
working example demonstrates the central value; no performance or recovery claim appears before
its scope; no passage demands wholesale replacement of an existing stack; the brand and mesh
identity are intact.

**Risk.** Medium — this is judgment work, not a lookup, and it touches the repository's most
public file. Mitigation: one review pass with the same reviewer before merge, and the accuracy
stages keep the factual base clean underneath it.

---

## Stage 1 — Conventions and mechanical corrections

**Cost:** ½ day.
**Output:** license and link treatment made uniform; two duplicate blocks removed; one stale
version fixed.

**Deliverables:**

1. **License (6 files).** Replace bare `Apache-2.0` with `MIT OR Apache-2.0` + license links:
   `cli/npm/`, `cli/python/`, `deck/npm/`, `deck/python/`, `bindings/node/`, `bindings/go/net/`.

2. **Broken license links (1 file).** `.claude/skills/README.md` links `LICENSE-APACHE` /
   `LICENSE-MIT` relative to a directory that contains neither. Add the files, or point at the
   repo-root copies.

3. **Duplicate blocks (1 file).** `README.md` repeats the `opensrc` fetch block — inline and
   again under `### Give the agent the source too`. Delete one. (Stage 0 rewrites this region;
   land the dedupe there if Stage 0 precedes it.)

4. **Link style (1 file).** `README.md:14` links a GitHub `/blob/…/worldview` **directory**
   (404). Replace with a relative path or the docs-site URL.

5. **Stale version (1 file).** `sdk-macros/README.md` pins `net-mesh-sdk = "0.24"` → `0.36`.

6. **Wrong in-repo path (1 file).** `README.md:243` cites `bindings/coverage.md`, which exists
   only under `.claude/skills/{net-event-bus,net-payments}/bindings/`.

**Acceptance.** No README claims bare `Apache-2.0`; no in-repo link in a touched file points at
a missing path.
**Risk.** Low — string substitutions with a manifest or directory listing as the oracle.

---

## Stage 2 — Factual corrections to code-facing claims

**Cost:** 1–1½ days.
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

**Acceptance.** CLI/Deck tables diff cleanly against the clap `Command` enum, the `Cli`
global-flags struct, `ExitCodeKind`, and the Deck `Tab` enum/labels; feature tables against
`Cargo.toml` / `package.json`.
**Risk.** Medium for CLI/Deck — the mirror rule means each parent edit is replicated into two
siblings. Do the parent first and propagate in the same commit (Stage 5 checks the shared facts).

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

**Acceptance.** Go and Python snippets compile/run against `go/` and `bindings/python/`; the TS
`napi` feature set is accepted by cargo; the pyproject metadata reports a non-empty description.
**Risk.** Low–medium. `sdk-py` and `go` need a real build to prove.

---

## Stage 4 — Per-package next action, not a link dump

**Cost:** ½ day.
**Output:** each SDK-facing README presents a short, prioritized route; the last two
relative-path defects are fixed.

The first draft set "every README links the union of all guides" as the goal. The review
rejected that: *give readers a short, prioritized route; complete navigation belongs on the
documentation site.* So this stage **curates**, it does not enumerate.

1. **Prioritized route (each SDK-facing README: `sdk/`, `sdk-ts/`, `sdk-py/`, `bindings/python/`).**
   A four-step route: **quickstart → the next useful task → deployment → reference.** Concretely,
   link the lens quickstart, one task guide appropriate to that lens, `guides/production-deployment`,
   and the lens's `errors` / reference page. `production-deployment` is added everywhere (linked
   by none today); `task-lifecycle` / `gang-scheduler` are added only to the lenses that expose
   the scheduler. `bindings/python/` gets a short `## Links` section in the same shape (it is the
   only SDK README without one). **Do not** add the full guide set; a link list that mirrors the
   site is not the goal.

2. **`include/README.md`** — example paths say `examples/basic.c` etc., but that directory does
   not exist; the files are at `net/crates/net/examples/` (8 of them, not 3). Use `../examples/*.c`.

3. **`docs/demos/dir_transfer/README.md`** — `docs/cli/TRANSFER.md` resolves from
   `docs/demos/dir_transfer/`; fix to `../../cli/TRANSFER.md`, and link
   `FETCH_DIR_ATOMIC_PLAN.md` properly.

4. **`.claude/skills/README.md`** — the skill file maps omit `subnet-auth.md`,
   `source-access.md`, and each skill's `bindings/` directory. Add rows.

5. **`tools/binary-size/README.md`** — add the caveat that `measure_sizes.sh` is zsh + BSD
   `stat` + `.dylib` (macOS-only).

6. **`tests/natsim/README.md`**, **`tests/cross_lang_payments/README.md`** — audited clean;
   re-verify in Stage 5 only.

**Acceptance.** Each SDK README presents an ordered short route and every link resolves; no
README is a link dump; the two relative-path defects are fixed.
**Risk.** Low.

---

## Stage 5 — Verification harness

**Cost:** ½ day.
**Output:** the committed checker at `.github/scripts/check-readmes.py` (CI wiring per D2) that
fails loudly on drift.

**Checks:**

1. **Link resolution.** Extract every Markdown link from all 28 files. Absolute URLs → HEAD/GET
   (doc-site links resolve through the adaptive-route projection in `web/src/lib/docs.ts`).
   Relative paths → `os.path.exists` resolved against the *file's own* directory.
2. **CLI table.** Build `net-mesh` once; diff the README subcommand table, global-flags list and
   exit-code table against `--help`, the `Cli` struct, and `ExitCodeKind`.
3. **Deck table.** Diff the Tabs table against the `Tab` enum labels.
4. **Feature tables.** Diff each README feature matrix against `Cargo.toml [features]` /
   `package.json`.
5. **Shared facts (D1).** Per packaging family (`cli/` + `cli/npm/` + `cli/python/`; `deck/` +
   npm + Python): assert each page's install command and package identifier are **correct for
   that registry's manifest**, and that license, version and the designated shared blocks
   (subcommand / tab table) **agree** across the family. **Not** a whole-file byte-match, and not
   command-equality across registries.
6. **Packaging.** `cargo package --list -p net-mesh -p net-mesh-sdk -p net-mesh-sdk-macros` and a
   `pyproject.toml` metadata read to confirm every README is the declared long-description.

**Acceptance.** All six checks green. If D2 is taken, `.github/scripts/check-readmes.py` is
pinned in `ci.yml` with specific failing-case names — never a check that passes by matching
nothing (AGENTS.md's no-silent-skip culture).

---

## Sequencing and rollback

| Stage | Lands as | Reversible by |
|---|---|---|
| 0 | one PR touching `README.md` only; one review pass before merge | revert the PR |
| 1 | one PR, mechanical edits across 8 files | revert the PR |
| 2 | two PRs: crate+meta; CLI+Deck families (+bindings/integrations) | revert per PR |
| 3 | one PR, samples + the `sdk-py` manifest field | revert the PR |
| 4 | one PR, curated routes and path fixes | revert the PR |
| 5 | scripts + optional CI job (D2) | drop the job / the scripts |

Budget: **4–5 person-days** across six stages (Stage 5 included). Stage 0 is the positioning
deliverable and can
land first (it is `README.md`-only); Stages 1–5 are the accuracy workstream and can proceed in
parallel with its review. Stage 1 is independently valuable (wrong-license and broken-link
surface, no judgment calls); Stage 2 is the bulk; Stages 3–4 are bounded; Stage 5 is the ratchet.

**Sibling-lockstep rule.** Within Stage 2, edit `cli/README.md` and `deck/README.md` first, then
propagate the shared facts and blocks into their npm/Python siblings in the same commit. Do not
edit a sibling before its parent.

---

## Overall acceptance

- A cold reader understands what Net enables and why they might need it.
- One working example demonstrates the central value.
- Performance and recovery claims are scoped where they first appear.
- The product fits an existing stack without demanding a wholesale replacement.
- Each package README offers a clear next action.
- Every README is factually accurate against the code, uniformly licensed, and link-clean.

## What this plan does NOT address (cross-references)

- **Docs-site content.** `DOCS_POLYGLOT_LENS_PLAN.md` (composition, routing, the SDK spine),
  `DOCS_STRATEGY_PLAN.md` (positioning, worldview — and the reconciliation obligation this plan
  satisfies), `DOCS_SDK_SPINE_PLAN.md`.
- **Skill content.** `SKILLS_LANGUAGE_ROUTING_PLAN.md` and `SKILLS_VERIFICATION_PLAN.md` own the
  skill bodies; this plan only fixes skill directory README links and maps.
- **CI lint/format discipline.** `PANIC_AUDIT_AND_LINT_HARDENING_PLAN.md` owns the lint gates;
  D2 here would add one more checker of the `check-spine-symbols.py` kind.
- **Package publishing.** No manifest changes except the missing `sdk-py` `readme` field.
- **`web/` release-note fixture count.** `RELEASE_v0.13_CHIPPIN_IN.md` states "thirteen
  golden-vector fixtures" for `tests/cross_lang_capability/`, which holds 8. Out of scope (web/);
  file separately.

## Adjacent defects (filed separately, not part of this pass)

| File | Defect |
|---|---|
| `net/crates/net/integrations/hermes/plugin.yaml` | version pinned at `0.35.0` while the tree is `0.36.0` |
| `web/src/content/docs/releases/RELEASE_v0.13_CHIPPIN_IN.md` | fixture count stale (web/) |
