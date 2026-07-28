# Agent Skills — verification plan

> Closing the gap between "the skills are structurally checked" and "the skills are
> correct." [`.github/scripts/check-skills.sh`](../../../.github/scripts/check-skills.sh)
> gates nine classes of drift in CI, and `publish` is blocked behind it
> ([`skills.yml`](../../../.github/workflows/skills.yml)). What it cannot see is the
> content that matters most: whether the code compiles, whether the payments skill is
> as accurate as the event-bus one, and whether the behavioural prose is true.
> Companion to [`DOCS_STRATEGY_PLAN.md`](DOCS_STRATEGY_PLAN.md) (the docs site) and
> [`TEST_COVERAGE_PLAN_2.md`](TEST_COVERAGE_PLAN_2.md) (the same "pin the invariant
> with a named test" method, applied to the crate).

## Status

**Revision 2 — not started.** Revision 1 was reviewed and held. Every finding was
verified against the tree and is incorporated below; the three that changed the
shape of the plan were:

- **The gate did not gate.** R1 listed "blocking the publish mirror on anything
  new" as a non-goal, which contradicts the plan's purpose: `publish` needs only
  `drift`, so a red `examples` job would mirror a broken hello-world in the same
  run that proved it broken. Now Phase 0, and the non-goal is restated correctly.
- **The triggers miss most of what the skills document.** Measured: **36 of 66**
  cited-and-tracked source paths fall outside the workflow's path globs. Gap 4
  could have been implemented in full and still never run on the changes it
  targets. Now Phase 0.
- **Several "done when" conditions claimed more than the machinery delivers.**
  Compile-checking is not execution; a resolving test name is not proof; a
  finished audit with no artifact is indistinguishable from a skim. Acceptance
  criteria are now split by what each mechanism actually proves.

Prerequisites: Phase 0 gates everything else. Phases 1–5 are independently
shippable after it.

## Context — why this work

Four audit passes over the skills found 16 defects. The rate did not decline:

| Pass | Area opened | Defects |
|---|---|---|
| 1 | Cited paths, symbol existence, CLI verb names | 6 |
| 2 | Enum variants, error tables, config defaults | 6 |
| 3 | A CI failure on a clean checkout | 1 (+ a bug in the checker itself) |
| 4 | Metric names and config knobs (~10 min probe) | 3 |

Every newly-opened area contained something. The worst was not a typo but a
correct-sounding recommendation: `dataforts.md` told operators to use
`dataforts_greedy_admit_throttled_bandwidth_total` to separate "NIC saturated"
from "cache full." That metric does not exist — rejections are one counter with a
`reason` label — *and* the real label cannot answer that question, because
`BandwidthExhausted` bumps the `capacity` reason
([`greedy/runtime.rs:136`](../../../net/crates/net/src/adapter/net/dataforts/greedy/runtime.rs)).

Name-checking is a floor. The phases below are ordered by how much of the
un-floored space each one covers, after first making the floor actually load-bearing.

The corpus, for sizing:

| Skill | Files | Lines | Fenced blocks |
|---|---|---|---|
| `net-event-bus` | 24 | 6,028 | 161 |
| `net-payments` | 17 | 3,201 | 79 |

Language-tagged blocks: **113 rust, 31 python, 30 ts, 9 go, 4 c** (187 total; the
remaining 273 fences are output, wire dumps, and diagrams).

---

## Phase 0 — make the gate load-bearing — **Priority 0, S**

Prerequisite for every other phase. Neither item is a "nice to have": without
them, later phases can be fully implemented and still fail to run, or run and
fail to block.

### 0a. Publication must fail closed on content checks

**Surface:** [`skills.yml`](../../../.github/workflows/skills.yml), `publish.needs`.

**Gap:** `publish` declares `needs: drift` only. Any job added by Phases 1–4 is
advisory by default — it can go red while the same workflow run mirrors the
invalid skill to `ai-2070/net-claude-skill`. This is sharpest for Phase 1, whose
own justification is that the examples are the highest-exposure content in either
skill.

**Work:** every job that verifies content the mirror carries joins `publish.needs`:

```yaml
publish:
  needs: [drift, examples, snippets]
```

**Policy, stated so later phases inherit it:** a job that verifies published
content blocks publication. A job may be slow, or PR-only, or manually
dispatched — but a *known-red* verification of the artifact must never publish
that artifact. Where a check is too slow to run per-branch, run it PR-only and
have the master publish depend on the PR having passed, rather than dropping the
dependency.

**Done when:** deleting a symbol referenced by a marked snippet makes `publish`
skip, demonstrated on a scratch branch.

### 0b. Trigger paths must cover the documented surface

**Surface:** the `push` and `pull_request` `paths:` lists in `skills.yml`.

**Gap:** measured against the paths the skills actually cite, **36 of 66
cited-and-tracked source paths are unwatched**. The current list reaches
`net/crates/net/{src,sdk/src,cli/src,payments/src}` and `bindings/*/src/**`, and
misses:

| Unwatched surface | Tracked files | Why it matters |
|---|---|---|
| `go/**` | 71 | The whole Go binding; `apis.md`, `nrpc.md`, `capabilities.md` document it |
| `net/crates/net/sdk-ts/**` | 50 | The TypeScript SDK; `apis.md` templates target it |
| `net/crates/net/sdk-py/**` | 52 | The Python SDK; ditto |
| `net/crates/net/bindings/node/*.ts` | 81 (dir) | `bindings/*/src/**` misses these — they sit at the package root |
| `net/crates/net/bindings/python/python/**` | 6 | The public Python wrapper surface |
| `net/crates/net/include/**` | 11 | The C API; `hello.c` and `apis.md` depend on it |
| `net/crates/net/adapters/mcp/**` | — | `mcp.md` cites `serve/gated.rs` directly |

The `bindings/node/*.ts` case is the clearest demonstration: two commits before
this plan was written, `nrpc.md` was corrected to cite `bindings/node/mesh_rpc.ts`
and `errors.ts` instead of their gitignored `.js` build output — and the workflow
does not watch either file.

**Work:** replace the partial subdirectory list with correct broad roots.

```yaml
- "go/**"
- "net/crates/net/src/**"
- "net/crates/net/sdk/**"
- "net/crates/net/sdk-ts/**"
- "net/crates/net/sdk-py/**"
- "net/crates/net/cli/**"
- "net/crates/net/payments/**"
- "net/crates/net/bindings/**"
- "net/crates/net/adapters/**"
- "net/crates/net/include/**"
```

Plus the manifests where dependency or API resolution can shift without a source
edit: `Cargo.lock`, `go/go.mod`, `net/crates/net/sdk-ts/package.json`,
`net/crates/net/sdk-py/pyproject.toml`, and the binding package manifests.

Broad roots over another brittle partial list — the check is ~13s and a
false-positive run costs nothing, while a missed run costs a published defect.

**Done when:** a script asserts every path cited by the skills falls under at
least one trigger glob, and it runs inside `check-skills.sh` so the two lists
cannot drift apart. (This is itself a tenth check class, and the cheapest one in
the plan.)

### 0c. Correct the stale examples-job sketch

The commented `examples:` block in `skills.yml` references
`cargo build -p net-sdk --example hello`, which will not work — see Phase 1. Fix
or delete it when Phase 1 lands so nobody implements the sketch.

**Why S:** three edits and one assertion script. Half a day.

---

## Phase 1 — the `examples/` never build — **Priority 1, M**

**Surface:** `.claude/skills/net-event-bus/examples/{hello.rs,hello.ts,hello.py,hello.go,hello.c}`
plus its `README.md`.

**Gap:** the README presents these as "the first thing a developer runs after
`npm install` / `pip install` / `cargo add`," and gives an exact command per
language. Nothing compiles or runs any of them.

### The two acceptance levels, kept distinct

R1 conflated these. Compile-checking and executing prove different things and
must not share a "done" condition.

**Level 1 — PR compile floor.** Runs on every PR, gates `publish`.

| Lang | Command | Proves |
|---|---|---|
| Rust | `cargo build` in a generated scratch crate | compiles against the workspace SDK |
| Go | `go build -o /dev/null` in the `go/` module | compiles against the current module |
| TS | `tsc --noEmit` with a generated `tsconfig` | type-checks against the current package surface |
| Python | type-check against the installed/stubbed package | members and signatures resolve |
| C | `gcc -fsyntax-only -I net/crates/net/include` | compiles against the current public headers |

**`python -m py_compile` is not acceptable as the Python floor.** It proves
syntax and nothing else — not that `net_sdk` imports, that referenced members
exist, that signatures match, or that the example reaches the SDK at all. Use a
type checker against the local package or its stubs. `py_compile` may stay as a
cheap precheck, never as the acceptance proof.

**Level 2 — release execution proof.** Runs where built artifacts already exist
(the `release-*` workflows), not on every PR.

- Execute the exact command printed in `examples/README.md`.
- Assert exit 0, **exactly one line** of output, and that the emitted payload
  round-trips to the expected value.

Until Level 2 exists, the honest claim is "compile-checked," not "verified
runnable," and the README should say so.

### The Rust harness — decided, not offered

R1 offered two options. Choosing: **keep the canonical `hello.rs` in the skill**
and generate a scratch crate at `target/skill-examples/rust/` that pulls it in,
depending on `net-sdk` by exact workspace path, building only that example.

Moving it under `net/crates/net/sdk/examples/` was the other candidate and is
rejected: it would either remove the file from the published skill or duplicate
it, and `cargo build --examples` would silently widen the job to every unrelated
SDK example. The scratch crate proves the exact file users receive.

### Harness detail each language needs before implementation

"Against the package" is not specific enough to implement. Each needs:

- **TS** — generated `tsconfig.json`, module resolution mode, and whether it
  resolves `@net-mesh/sdk` via workspace link or built `.d.ts`.
- **Go** — module vs workspace context, and whether the example compiles against
  `go/` in-tree or a published module path.
- **Python** — package install (editable) vs stub-only resolution, and which type
  checker.
- **C** — feature defines, header include path, and whether the floor is
  syntax-only or a real link against the cdylib.

**Done when:** Level 1 covers all five languages, `publish` needs the job, and
`examples/README.md` distinguishes "CI compiles this" from "CI runs this." Level 2
is tracked as a separate item with its own done condition.

**Why M, not S:** five toolchains, five harnesses, and the Python floor is a real
piece of work rather than a one-liner.

---

## Phase 2 — payments verification, as signed audit slices — **Priority 1, L**

**Surface:** `.claude/skills/net-payments/` — 17 files, 3,201 lines.

**Gap:** passes 1–4 were weighted heavily toward `net-event-bus`. Verified in
payments: three constructor signatures (`PricingTerms::new`,
`InProcessProvider::new`, `serve_payments`), the Rust/Python/Node/Go parity
claims, `payment_gate`'s non-existence, the failure-schematic scope, and the
plan-vocabulary sweep. **Not** verified: the five envelope field tables in
`object-model.md`, `x402.md`'s byte-preservation claims, the tier transitions in
`verification.md`, the whole of `signer.md`, `spend-policy.md`'s decision matrix,
and `networks.md`'s ladder.

### Split into three independently-signable slices

R1 estimated gap 1 and this "a day between them," which was wrong and would have
pressured the implementer into exactly the name-checking pass this plan exists to
replace. No time estimate is given here; the slices are the unit of progress.

**2a — the signer boundary.** The sharpest surface: `signer.md` documents a
security invariant ("keys never cross the boundary"), and a wrong claim there is
worse than a wrong claim about a metric. Comparing method names does not prove
it. The audit must trace:

- public binding signatures across Rust, Python, and Node;
- request/intent serializers — what actually goes on the wire;
- error and `Debug`/`Display` paths, which are a classic leak;
- callback payloads and FFI structures;
- whether any raw-key representation is *constructible* or *returnable* at all;
- the exact contents of each signing intent (`SvmTransferIntent`,
  `XrplPaymentIntent`, the EIP-3009 authoring path).

**2a requires an independent reviewer** — not the person who wrote the audit.

**2b — object model and verification state machine.** `object-model.md`'s five
envelope tables field-by-field against `payments/src/core/`, including optionality
and types; `verification.md`'s tier transitions and reorg handling against
`payments/src/checker/` and `core/verification.rs`.

**2c — remaining surfaces.** `spend-policy.md`, `x402.md`, `networks.md`,
`facilitator.md`, `billing.md`, `http402.md`, `caller.md`, `provider.md`,
`bindings.md`, `testing.md`.

The crate maps almost 1:1 onto the skill's files (`engine/`, `flow/`,
`facilitator/`, `core/`, `x402/`, `policy/`, `checker/`, `billing/` all exist),
which is what makes slicing clean.

### Durable evidence, not a transient audit

R1's done condition — "diffed against source and the findings fixed" — leaves no
artifact. Six months on, nobody can distinguish a source-pinned field-by-field
review from someone reading it and saying it looked fine.

Each slice writes a ledger at `docs/internal/audits/SKILLS_PAYMENTS_<slice>_<YYYY-MM>.md`,
one row per claim:

| Field | Meaning |
|---|---|
| Claim | The specific assertion, quoted from the skill |
| Authoritative source | Exact module/file — the thing that decides |
| Source SHA | The commit audited, so staleness is computable |
| Method | field-by-field \| boundary trace \| transition review |
| Result | pass \| fail |
| Finding / fix | Commit that fixed it, or "no change needed" |

Anything mechanical that falls out feeds back into `check-skill-refs.py`.

**Done when:** all three slices have a ledger pinned at a source SHA, 2a carries
an independent reviewer's sign-off, findings are fixed, and the check is green.

---

## Phase 3 — non-Rust structural membership — **Priority 2, M** — ⚠️ **narrowed on evidence, shipped**

> **Outcome: the inventory step changed the answer, so the phase shipped smaller
> than specified.** Step 1 (inventory before implementing) was the right
> instruction and it paid: measuring the actual citation surface showed three
> parsers would have guarded almost nothing.
>
> | Language | Surface in source | Cited by the skills |
> |---|---|---|
> | Python | **0** `Enum`/`StrEnum` classes in `sdk-py` | nothing to check |
> | TypeScript | 10 string-literal union types | **2** quoted-literal citations, both in one line of `streams.md`, both correct |
> | Go | 5 typed-const families | concentrated in **one** family, the `nrpc:` wire kinds |
>
> A citation convention plus three parsers plus per-language fixtures, to guard
> roughly a dozen citations, is not a good trade. What *does* carry risk is the
> narrow case those citations cluster in: **frozen cross-language vocabularies**
> — a value single-sourced across four bindings and reproduced in the skills as
> a table a reader pattern-matches on.
>
> Shipped `check-skill-vocab.py` for exactly that. It immediately found the nRPC
> kind table missing `cancelled` and `capability_denied` — both real wire kinds
> present in Rust, Node, Python and Go, both absent from the skill, so a
> cross-language catch site written from that table would have missed them.
>
> **Not done, and deliberately:** the qualification grammar, the TS/Python/Go
> parsers, and wrong-owner fault injection. If the skills later grow a table of
> TS union members or Python enum members, that work becomes worth doing and
> this section is the starting point. Adding a vocabulary to the shipped check is
> four lines.

The original scope follows, kept as the record of what was decided against.

**Surface:** `check-skill-refs.py` — `read_sources()` brace-matches `pub enum` for
`.rs` only. `.ts`, `.py`, and `.go` feed the identifier corpus but get no
structural check.

**Gap:** the enum-variant check caught the worst defect in the skills (four
invented `RpcError` variants plus a one-letter misspelling that would not
compile). There is no equivalent for a TS union, a Python enum, or a Go const
block — and the skills document all three.

### The blocker R1 missed: documentation must claim ownership

Parsing `export type State = "ready" | "failed"` tells the checker what exists.
It does not tell it that a backticked `` `'ready'` `` in a Markdown table claims
membership in `State`. Rust works today only because the reference names its
owner: `RpcError::NoRoute`.

Measured across the skills:

| Reference shape | Count | Checkable today |
|---|---|---|
| `Owner::Member` (Rust-style) | 229 | yes |
| `Owner.member` (dotted) | 48 | ambiguous — member or method call |
| Bare quoted literal (`'reliable'`, `'fire_and_forget'`) | 18 | **no owner named** |

Without a convention, a checker can confirm `'reliable'` appears somewhere in the
TS corpus and learn nothing about whether it is a valid `Reliability` value.

**Work, in order:**

1. Inventory the reference shapes actually used (the table above is the start).
2. Choose a qualified citation convention and apply it: `State.ready` for TS,
   `State.READY` for Python, `StateReady` bound to an identified const block for
   Go, `Type.field` for struct fields.
3. Define exactly which source shapes each parser supports — TS string-literal
   unions and `as const` objects; Python `Enum`/`StrEnum` and dataclass fields;
   Go `const (...)` blocks and exported struct fields.
4. Add positive and negative fixtures per language.
5. Fault-inject **wrong-owner membership**, not merely a globally-absent name. A
   name existing somewhere in the corpus must not satisfy membership in the wrong
   type — that is the whole point of the check.

Output shape matches the Rust check ("X is not a member of Y — actual: ..."),
since that message is what made the Rust findings actionable.

**Done when:** a documented-but-nonexistent TS union member, Python enum member,
and Go const each fail; and a *real* member cited against the *wrong* type also
fails.

---

## Phase 4 — opt-in snippet compilation — **Priority 2, L** — ⚠️ **shipped; ratchet started**

> **It found the worst defect of the whole programme on its first real run.**
> `capabilities.md` — the file the skill itself calls "the differentiator vs
> Kafka/NATS/Redis" — documented a predicate API that has never existed:
>
> ```rust
> use net_sdk::capabilities::{p, evaluate_predicate, predicate_to_rpc_header,
>                             validate_capabilities, tag_key};
> let pred = p.and(&[ p.exists(&tag_key("hardware", "gpu")), ... ]);
> ```
>
> Of those five imports: `p` and `evaluate_predicate` and `tag_key` do not
> exist anywhere; `predicate_to_rpc_header` and `validate_capabilities` are real
> but live in `capabilities::predicate::` and `capabilities::schema::`
> respectively, not the parent module. The real API is a **`pred!` macro** with a
> parse-time DSL and dotted string keys — a different shape entirely, documented
> correctly in the SDK's own doc comment the whole time. No name-level check
> could have caught this: every symbol was plausible and the prose around it was
> accurate.
>
> **Harness lessons worth keeping**, each found by a failing run rather than
> reasoning:
>
> - *Always wrap in a function.* An "emit items as-is, wrap statements"
>   heuristic split exactly wrong on the mixed snippets docs are full of
>   (declare a type, then use it). Rust allows items inside fn bodies, so one
>   rule covers all three shapes.
> - *Hoist whole `use` statements, not lines.* The multi-line `use foo::{ ... };`
>   form is everywhere in these files; a line-based split silently breaks it.
> - *The preamble must be empty.* It started with `use std::sync::Arc;` as a
>   convenience and immediately collided with the snippets that (correctly)
>   import it themselves — and worse, would have let a snippet pass while
>   missing an import its reader needs. It is now empty by policy, with the
>   reasoning recorded in the file.
> - *`cargo check`, not `cargo build`.* Linking adds nothing here and drags in
>   build-script link failures unrelated to any snippet.
>
> **Ratchet state:** the marker is `<!-- skill-check: compile -->`; unmarked
> blocks are skipped and the checked/total ratio is always printed, so a green
> run never implies coverage it does not have. Only Rust is wired up — a marker
> on any other language is a hard error rather than a silent pass, so extending
> it is a deliberate act. `ratchet-skill-snippets.py` unmarks failures during
> maintenance and is deliberately NOT run in CI, where it would let coverage
> quietly fall.

The original scope follows.

**Surface:** 187 language-tagged fenced blocks.

**Gap:** the checker validates names, never a signature, arity, or type. The
signature errors found by hand in pass 2 were all name-shaped and would now be
caught; a wrong *argument order* still would not be.

**Why L:** most snippets are fragments.

| Lang | Blocks | With imports | Contains elision |
|---|---|---|---|
| rust | 113 | 37 | 20 |
| python | 31 | 19 | 5 |
| ts | 30 | 14 | 7 |
| go | 9 | 4 | 7 |
| c | 4 | 3 | 1 |

About a third are self-contained. Wholesale extraction produces a wall of
failures that says nothing; rewriting 187 blocks to stand alone would bloat the
skills and hurt the reader.

### Marker grammar — decided

R1 offered ```` ```rust,check ```` or an HTML comment. Choosing the **HTML
comment**: an info string of `rust,check` is treated as a literal language name
by several renderers and loses Rust highlighting in the published skill.

```
<!-- skill-check: compile -->
```

Rules: exactly one marker per fence, immediately adjacent; a malformed or
orphaned marker is an **error**, not a silent skip. Reserve room to extend
deliberately — `no-run`, `compile-fail`, `ignore reason="fragment"` — but ship
`compile` alone first.

### Preamble discipline

A generated preamble supplying common imports is necessary and dangerous: a
snippet can compile only because the harness imported something the skill never
tells the reader to import, and then the job proves the harness rather than the
documentation. Constraints:

- one checked-in preamble per language, reviewed as source;
- imports and harness scaffolding only — **no helper behaviour**;
- no preamble may define a symbol the public SDK is supposed to provide;
- each marked snippet declares which preamble it expects;
- preamble content is printed in CI diagnostics on failure.

### Rollout

- Extractor writes marked blocks into generated packages under
  `target/skill-snippets/<lang>/`.
- Mark the already-self-contained blocks first (~30 rust, ~15 python, ~10 ts),
  then ratchet.
- Report `checked/skipped` totals explicitly — "41/113 rust blocks checked" —
  never a bare pass that implies full coverage.
- Runs as its own job, not inside the ~13s `drift` path, and per Phase 0a it
  joins `publish.needs`.

**Done when:** the marked subset compiles in CI, totals are printed, orphaned
markers fail, the marker is documented in `.claude/skills/README.md`, and a red
snippet job blocks the mirror.

---

## Phase 5 — behavioural claims traced to tests — **Priority 3, L**

**Surface:** the prose — the highest-value content in the skills and the only
kind no mechanical check can evaluate.

**Gap:** pass 4 showed the ceiling directly. A name check would now catch the
wrong metric name, but the *advice* attached to it was independently wrong, and
only reading `runtime.rs` revealed that.

### Proof tiers — because a resolving test name proves nothing

R1 said claims should point at "the test that proves it," but a name check
establishes only that a test with that name exists. A vacuous test, a stale name,
or an assertion on the wrong transition all pass. Tiers:

| Tier | Establishes |
|---|---|
| `exists` | the named test resolves |
| `ci` | the test is selected by a required CI gate (reuse `integration-guard`'s pin logic in [`ci.yml`](../../../.github/workflows/ci.yml)) |
| `reviewed` | a reviewer confirmed its assertions cover the cited claim |
| `mutation` | a claim-negating mutation kills the test |

### First-wave claims — named, not "top ~20"

R1's "top ~20" would have let the implementer pick the twenty easiest to
annotate. The first wave is exactly these, with required tier:

| Claim | Skill file | Tier |
|---|---|---|
| After a conflicted heal the losing side's writes survive as a fork, not a merge | `runtime.md` | mutation |
| A facilitator receipt is `observed` only; `confirmed(n)`/`final` come solely from `ChainChecker` | `verification.md` | mutation |
| Keys never cross the signer boundary | `signer.md` | mutation |
| A missing path and an empty `$and`/`$or` both match nothing | `filter-dsl.md` | mutation |
| Cluster-cap eviction withdraws the `causal:` announcement inline | `dataforts.md` | mutation |
| Capability filters are advisory; `require_token` + `token_roots` is the only real boundary | `mesh.md` | mutation |
| `match_islands` is advisory and holds nothing; only the CAS commits | `scheduler.md` | reviewed |
| Backpressure is silent under `drop_*`; visible only via `events_dropped` | `concepts.md` | reviewed |
| Replication `UnderCapacity` / `SyncNack` retry behaviour | `redex.md` | reviewed |
| Consent runs before payment; no configured flow fails closed | `provider.md` | reviewed |

### Mechanism

A hidden marker, not visible parenthetical prose — this ships publicly and the
reader should not see audit scaffolding:

```
<!-- net-claim: redex-conflicted-heal
     test: net/crates/net/tests/<file>.rs::<test>
     proof: mutation -->
```

The checker rejects duplicate claim IDs and unresolved tests, and verifies the
declared tier is satisfied as far as it can (resolution and CI selection
mechanically; `reviewed` and `mutation` by requiring a ledger entry).

**A claim with no test to point at is itself a finding** — either the behaviour is
untested, or the claim is speculative. Both outcomes are useful, and the first
turns part of this phase into crate work rather than docs work.

**Done when:** every first-wave claim carries a marker at its required tier,
unresolvable references fail the check, and claims that could not be pinned are
recorded here as crate testing gaps.

---

## Sequencing

```
Phase 0  fix the gate            ── prerequisite for everything
   │
   ├── Phase 1  examples          ── highest exposure; needs 0a to block publish
   ├── Phase 2  payments audit    ── largest unaudited surface; 2a needs a reviewer
   │
   └── Phase 3  non-Rust members  ── needs 0b to trigger at all
          │
          └── Phase 4  snippets   ── reuses Phase 3's parsers
                 │
                 └── Phase 5  claims ── partly crate work
```

Phase 0 first, always — it is half a day and without it Phases 1–4 are advisory.
Phases 1 and 2 are independent and can run in parallel. Phase 3 must not start
before 0b, or it will be tested only by paths that never fire.

## Non-goals

- **Expensive execution inside the fast `drift` job.** The ~13s structural path
  stays fast. This is the *correct* form of R1's non-goal — it is about job
  placement, not about whether verification blocks publication. Per Phase 0a, a
  red content check always blocks the mirror.
- **Rewriting snippets to be standalone.** It would bloat the skills and hurt the
  reader. The opt-in marker exists to avoid exactly this.
- **Checking prose with a model.** Phase 5 buys traceability to tests, not
  semantic verification.
- **Extending this to the docs site.** `web/` has its own link checker and
  `doc_link_guard` covers the crate's docs; this plan is scoped to
  `.claude/skills/`.

## Appendix — what the current check already covers

`check-skills.sh`, ~13s, no toolchain. Nine classes, reported as eight sections
(enum variants and identifiers share one pass over the tree):

| Class | Catches |
|---|---|
| Frontmatter | missing keys, `net-version` skew, description over 3,000 chars |
| Cross-file links | a `*.md` reference with no such file |
| Cited repo paths | resolved via `git ls-files`, so gitignored build output cannot mask a bad citation |
| API canaries | `emit*` count, `SdkError` variant count |
| Documented symbols | a named function with no Rust definition |
| Enum variants | a variant that does not belong to its enum, with the real list in the message |
| Metric/config identifiers | a snake_case identifier appearing nowhere in source, with an `ABSENT_OK` list for deliberately-absent names |
| CLI verbs | bare `net <verb>` when the binary is `net-mesh` |
| Plan leakage | roadmap vocabulary (`P0`, `Mode E`, internal branch names) in a publicly-shipped skill |

Every class has been fault-injected. Phase 0b adds a tenth: trigger-path coverage
of every cited source path.

## What each mechanism proves, and does not

Stated explicitly, because R1's acceptance criteria overclaimed.

| Mechanism | Proves | Does not prove |
|---|---|---|
| `check-skills.sh` | names, paths, variants, identifiers resolve | signatures, arity, semantics |
| Phase 1 Level 1 | examples compile against current surfaces | that they run, or print what the README says |
| Phase 1 Level 2 | exact README command exits 0 with one correct line | anything about other snippets |
| Phase 2 ledger | a human checked these claims at a named SHA | that they stay true after that SHA |
| Phase 3 | a documented member belongs to the type it is cited under | that the member behaves as described |
| Phase 4 | marked snippets compile with a reviewed preamble | unmarked snippets, or runtime behaviour |
| Phase 5 `exists`/`ci` | a named test resolves and is gated | that its assertions cover the claim |
| Phase 5 `mutation` | negating the claim breaks a test | claims not in the first wave |
