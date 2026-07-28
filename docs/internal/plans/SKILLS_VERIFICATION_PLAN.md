# Agent Skills — verification plan

> Closing the gap between "the skills are structurally checked" and "the skills are
> correct." [`.github/scripts/check-skills.sh`](../../../.github/scripts/check-skills.sh)
> now gates nine classes of drift in CI, and `publish` is blocked behind it
> ([`skills.yml`](../../../.github/workflows/skills.yml)). What it cannot see is the
> content that matters most: whether the code compiles, whether the payments skill is
> as accurate as the event-bus one, and whether the behavioural prose is true.
> Companion to [`DOCS_STRATEGY_PLAN.md`](DOCS_STRATEGY_PLAN.md) (the docs site) and
> [`TEST_COVERAGE_PLAN_2.md`](TEST_COVERAGE_PLAN_2.md) (the same "pin the invariant
> with a named test" method, applied to the crate).

## Status

**Not started.** The five gaps below are the residue of a four-pass audit of
`.claude/skills/` (2026-07-28). Nothing here is blocking: the skills are in good
shape and the structural check is green. This is the work that would let us claim
they are *verified* rather than *checked*.

Prerequisites: none. Each gap is independent and shippable on its own.

Activation gate: gaps 1 and 3 are worth doing now — they are small and they cover
the two highest-exposure surfaces (the first command a user runs, and the skill
with the least scrutiny). Gaps 2, 4, and 5 are worth doing when the skills next
take a large edit, since their value is proportional to churn.

## Context — why these five

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

That is the shape of what remains. Name-checking is a floor. The five gaps below
are ordered by how much of the un-floored space each one covers.

The corpus, for sizing:

| Skill | Files | Lines | Fenced blocks |
|---|---|---|---|
| `net-event-bus` | 24 | 6,028 | 161 |
| `net-payments` | 17 | 3,201 | 79 |

Language-tagged blocks: **113 rust, 31 python, 30 ts, 9 go, 4 c** (187 total; the
remaining 273 fences are output, wire dumps, and diagrams).

---

## 1. The `examples/` never build — **Priority 1, S**

**Surface:** `.claude/skills/net-event-bus/examples/{hello.rs,hello.ts,hello.py,hello.go,hello.c}`
plus its `README.md`.

**Gap:** the README presents these as "the first thing a developer runs after
`npm install` / `pip install` / `cargo add` — before they write any application
code," and gives an exact run command per language. Nothing compiles or runs any
of them. They are the highest-exposure content in either skill — a broken
hello-world is the worst possible first contact — and the only content with a
documented, mechanical success criterion ("each prints exactly one line").

**Work:**
- Add an `examples` job to [`skills.yml`](../../../.github/workflows/skills.yml).
  The sketch is already in that file, commented, with a note on what each needs.
- Rust: the file is written to be dropped into a crate's `examples/` dir. Either
  copy it into a scratch crate that depends on `net-sdk` at the workspace version,
  or move it under `net/crates/net/sdk/examples/` and let `cargo build --examples`
  cover it — the latter is less machinery and makes it a first-class example.
- Go: `go build -o /dev/null` against the `go/` module.
- TS: `tsc --noEmit` against `@net-mesh/sdk` types; a full `npx tsx` run needs the
  napi artifact, so type-check first and consider running only on release builds.
- Python: `python -m py_compile` as a floor; a real run needs the built wheel.
- C: `gcc -fsyntax-only -I net/crates/net/include` as a floor; linking needs the
  cdylib.

**Decision to make:** type-check/compile-only for all five (cheap, no artifacts,
runs on every PR) versus actually executing them (proves the "one line" claim,
but needs built bindings and therefore belongs in a release workflow). Recommend
compile-only in `skills.yml` now, and a follow-up execution job wired into the
existing release pipelines where the artifacts already exist.

**Done when:** every file in `examples/` is compiled or type-checked by CI, and
the run commands in `examples/README.md` are the same ones CI uses.

**Why S:** five short files, no fixtures, and the job is already drafted.

---

## 2. Snippets are not compiled — **Priority 2, L**

**Surface:** 187 language-tagged fenced blocks across both skills.

**Gap:** the checker validates *names* — that a symbol, variant, or identifier
exists — never a signature, arity, or type. The three signature errors found by
hand in pass 2 (`RpcAppError`, `BlobRef::MAX_SIZE`,
`RedexFileConfig::with_blob_max_size`) were all name-shaped and would now be
caught; a wrong *argument order* still would not be.

**The reason this is L, not M:** most snippets are fragments. Measured over the
corpus:

| Lang | Blocks | With imports | Contains elision (`…`, `...`) |
|---|---|---|---|
| rust | 113 | 37 | 20 |
| python | 31 | 19 | 5 |
| ts | 30 | 14 | 7 |
| go | 9 | 4 | 7 |
| c | 4 | 3 | 1 |

Only about a third are self-contained. Wholesale extraction would produce a wall
of failures that says nothing about correctness, and rewriting 187 blocks to be
standalone would bloat the skills and hurt the reader.

**Work:** opt-in, incremental, in the rustdoc spirit.
- Add a marker the extractor honours — an info-string suffix (` ```rust,check`)
  or a preceding `<!-- check -->` comment. Unmarked blocks are skipped and
  counted, so the report reads "41/113 rust blocks checked" rather than implying
  full coverage (the `no silent caps` rule).
- Extractor writes each marked block into a generated crate/package per language
  under `target/skill-snippets/`, with a per-language preamble file supplying the
  common imports.
- Start by marking the blocks that already have imports and no elision — roughly
  30 rust, 15 python, 10 ts. Ratchet from there.
- Wire into `check-skills.sh` behind a flag (`--snippets`) so the fast path stays
  ~13s, and run the slow path as a separate CI job.

**Done when:** the marked subset compiles in CI, the skipped count is printed, and
the marker is documented in `.claude/skills/README.md` so new snippets opt in.

**Why L:** a per-language extractor plus preamble, five toolchains, and a
first-pass marking sweep. The ratchet is what makes it tractable.

---

## 3. `net-payments` has had a third the scrutiny — **Priority 1, M**

**Surface:** `.claude/skills/net-payments/` — 17 files, 3,201 lines.

**Gap:** passes 1–4 were weighted heavily toward `net-event-bus`. What was
actually verified in payments: three constructor signatures
(`PricingTerms::new`, `InProcessProvider::new`, `serve_payments`), the
Rust/Python/Node/Go parity claims, the `payment_gate` non-existence, the
failure-schematic scope, and the plan-vocabulary sweep. What was **not**: the
five envelope field tables in `object-model.md`, the `x402.md` byte-preservation
claims, the tier-transition rules in `verification.md`, the whole of
`signer.md` (242 lines of key-handling invariants), `spend-policy.md`'s decision
matrix, and the network ladder in `networks.md`.

Nothing suggests it is worse than the event-bus skill was — but the event-bus
skill turned out to have 16 defects, and this one has not been looked at the same
way. `signer.md` is the sharpest: it documents a security boundary ("keys never
cross"), and a wrong claim there is worse than a wrong claim about a metric.

**Work:** a pass at the depth applied to `net-event-bus`, in this order:
1. `signer.md` — every `SchemeSigner` / `ExternalSigner` / `ExternalSvmSigner` /
   `ExternalXrplSigner` method against `payments/src/`, and the no-raw-signing
   invariant against the actual trait definition.
2. `object-model.md` — the five envelope field tables against
   `payments/src/core/`, field by field, including optionality and types.
3. `verification.md` — tier transitions and reorg handling against
   `payments/src/checker/` and `core/verification.rs`.
4. `spend-policy.md`, `x402.md`, `networks.md`, `facilitator.md`.
5. Feed anything mechanical back into `check-skill-refs.py`.

**Done when:** each file above has been diffed against its source module and the
findings fixed; the check stays green.

**Why M:** ~3,200 lines against a crate that is already well-organised
(`engine/`, `flow/`, `facilitator/`, `core/`, `x402/`, `policy/`, `checker/`,
`billing/` map almost 1:1 onto the skill's files).

---

## 4. Non-Rust surfaces get name-checks only — **Priority 2, M**

**Surface:** `check-skill-refs.py` — `read_sources()` brace-matches `pub enum`
for `.rs` only; `.ts`, `.py`, and `.go` contribute to the identifier corpus but
get no structural checking.

**Gap:** the enum-variant check is what caught the worst defect in the skills
(four invented `RpcError` variants plus a one-letter misspelling that would not
compile). There is no equivalent for a TS union type, a Python enum or dataclass
field, or a Go const block — and the skills document all three. `bindings.md`,
`apis.md`, and the per-language sections in `nrpc.md` are the exposure.

**Work:**
- TS: parse `export type X = 'a' | 'b'` and `export enum` / `as const` objects
  from `sdk-ts/src/**` and `bindings/node/*.ts`; check documented members.
- Python: parse `class X(Enum)` / `StrEnum` members and dataclass fields from
  `sdk-py/src/net_sdk/**` and `bindings/python/python/**`.
- Go: parse `const (...)` blocks and exported struct fields from `go/`.
- Same output shape as the Rust check — "X is not a member of Y — actual: ..." —
  since that message is what made the Rust findings actionable.

**Done when:** a documented-but-nonexistent TS union member, Python enum member,
and Go const each fail the check under fault injection.

**Why M:** three lightweight parsers. Regex is adequate — the goal is a tripwire,
not a type checker — but each language needs its own shape.

---

## 5. Behavioural claims are unverifiable by grep — **Priority 3, L**

**Surface:** the prose. "Cluster-cap eviction withdraws chain announcements
inline." "After a conflicted heal the losing side's writes survive as a fork, not
a merge." "A missing path and an empty `$and` both match nothing." The failure
tables in `redex.md`, the partition phases in `runtime.md`, the ordering
guarantees throughout.

**Gap:** this is the most valuable content in the skills — it is the reason an
agent writes correct code instead of plausible code — and no mechanical check can
touch it. Pass 4 demonstrated the ceiling directly: a name check would now catch
the wrong metric name, but the *advice* attached to it was independently wrong,
and only reading `runtime.rs` revealed that.

**Work:** the only mechanism that scales is to make a claim point at the test that
proves it.
- Introduce a claim reference convention: an inline marker naming the test that
  pins the behaviour, e.g. `(pinned: tests/redex_replication.rs::under_capacity_retries)`.
- Extend the check to verify each referenced test **exists** and, where the
  harness allows, that it is pinned to a CI step — reusing the
  `integration-guard` logic already in [`ci.yml`](../../../.github/workflows/ci.yml).
- Apply it to the highest-consequence claims first: partition/fork semantics
  (`runtime.md`), replication failure modes (`redex.md`), filter-DSL empty-match
  semantics (`filter-dsl.md`), and the payment tier transitions
  (`verification.md`). A claim with no test to point at is itself a finding —
  either the behaviour is untested, or the claim is speculative.
- Explicitly **do not** attempt to check the prose itself. The deliverable is
  traceability, not NLP.

**Done when:** the top ~20 behavioural claims carry a test reference, the check
validates those references resolve, and any claim that could not be pinned is
recorded here as a testing gap.

**Why L:** the marking is a judgement call per claim, and some claims will turn
out to have no test behind them — which is the point, and which turns this into
crate work rather than docs work.

---

## Sequencing

```
1 (examples)  ──┐
3 (payments)  ──┼── independent, do first, small + high exposure
                │
4 (non-Rust)  ──┘   feeds the same script as 3's mechanical findings
                    │
2 (snippets)  ──────┴── largest; benefits from 4's parsers
                        │
5 (claims)    ──────────┴── last; partly crate work, not docs work
```

Gaps 1 and 3 are a day between them. Gap 4 is a natural follow-on because
payments' mechanical findings want somewhere to live. Gaps 2 and 5 are the
long tail and should be ratcheted, never big-banged.

## Non-goals

- **Rewriting snippets to be standalone.** It would bloat the skills and hurt the
  reader. The opt-in marker exists precisely to avoid this.
- **Checking prose with a model.** Gap 5 buys traceability to tests, not
  semantic verification.
- **Extending any of this to the docs site.** `web/` has its own link checker and
  `doc_link_guard` covers the crate's docs; this plan is scoped to
  `.claude/skills/`.
- **Blocking the publish mirror on anything new.** `publish` already needs
  `drift`; slow jobs (gap 2's snippet compile) should stay off that path.

## Appendix — what the current check already covers

`check-skills.sh`, ~13s, no toolchain. Nine classes, reported as eight
sections (enum variants and identifiers share one pass over the tree):

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

Every class has been fault-injected. The gaps above are what it still cannot see.
