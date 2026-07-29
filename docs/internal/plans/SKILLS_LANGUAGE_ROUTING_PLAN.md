# Agent Skills — language routing

> Make the skills route by binding instead of presenting five API shapes at
> once. Shared doctrine + explicit routing + thin binding companions + checked
> examples — not five cloned skills, not one giant multilingual file.
> Companion to [`SKILLS_VERIFICATION_PLAN.md`](SKILLS_VERIFICATION_PLAN.md),
> which proves the skills match the tree; this one decides what an agent loads.

## Status

**Revision 3 — Phase 0 done, Phases 1–4 not started.** Revision 1's architecture
was approved; its
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

R3 corrects five smaller things, one of which was the same misclassification the
plan itself warns about — `C nRPC` was written `n/a` when the skill's own
`nrpc.md:387` says "**not exposed in `net.h`**". Making a rule and then breaking
it two sections later is how the rule stops being believed.

**One R3 item was not adopted**, with evidence: the review asked to weaken the
appendix to "membership, not shape." Shape checking *is* implemented and was at
the reviewed SHA — `check-skill-refs.py` compares a citation's `{ }` / `( )`
against the variant's real form, and it is fault-injected in both directions.
Weakening that line would have understated the tooling. The other two appendix
corrections were right and are applied.

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

## Phase 0 — make nested content verifiable — **Priority 0, S** — **DONE**

Prerequisite. Without it the later phases silently reduce coverage.

Landed in `752577efe` (0a), `37ec878f3` (0b) and the commit carrying 0c. Three
things were found while implementing it that the plan had not anticipated:

- `fail` in `check-skills.sh` and `check-docs.sh` was a 0/1 flag while every
  section reported success with `[ "$fail" -eq "$before" ]`. After the first
  failure, every later section printed a green tick it had not earned. Exit
  codes were always right, which is why it survived. Now a counter.
- The cross-reference extractor matched ``[a-z0-9-]*\.md`` and could not see a
  citation to an underscored filename at all.
- Recursion exposed `.claude/skills/README.md`, which the old glob never
  examined. It is the corpus index and routes *into* the skills, so its
  references resolve one level down and cannot name which skill — the rule
  relaxes there and only there.

**0a. Recursive discovery.** Every corpus-level check in `check-skills.sh`
enumerates markdown recursively, not two levels deep. Affected: cited repo
paths, CLI invocations, internal-plan vocabulary, and the cross-reference loop
(which reads `"$dir"*.md`).

**0b. A depth regression test, outside the published corpus.** A glob is one
edit away from regressing, so the depth guarantee needs a test — but the
`_probe.md` used above was *temporary evidence*, not a design. A permanently
broken file under `net-event-bus/bindings/` would be worse than no test:
`skills-publish` does `rsync -a --delete` over the whole skill directory, so it
would be **published to users** as part of the public skill, and the checker
would be permanently red or need an allowlist that weakens the real corpus.

Two acceptable shapes — the second is preferred, since it exercises the
production discovery path rather than a parallel one:

```
.github/fixtures/skill-checks/nested/bad.md   # checker run against a fixture corpus
```
```
harness: create a temporary nested file → run the checker → assert nonzero and
the expected diagnostic → remove it
```

*Implemented as a hybrid, because the harness shape has the same publication
hazard in weaker form — an interrupted run leaves the probe in the corpus and
the next push mirrors it.* `check-skills.sh` took an optional `SKILLS_DIR`, so
`check-skills-depth.sh` plants its probe in a throwaway copy under `$TMPDIR`.
That keeps the production discovery path under test — same script, same
`skill_md()`, same greps — while `.claude/skills/` is never written to at all.
Verified in both directions: four planted defects at depth 3 are all reported,
and reverting `skill_md()` to the old two-level glob turns all four red.

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

*Closed by a fourth option that costs less than any of those.* The check moved
into `check-skill-example-ts.sh`, called by both workflows, so there is one
definition rather than two copies to drift. `skills.yml` gained a `typescript`
job that builds napi and runs it, restricted by `if:` to exactly the events
where `publish` can run — a full napi build is ~10 minutes, and `skills.yml`
fires on every documented surface, so running it always would attach that cost
to most pushes to buy nothing. On a branch or PR the same script runs in
`ci.yml`, which is where the signal is wanted early; the one thing `ci.yml`
cannot do is block this workflow's mirror, and that is the only case the new job
covers.

Two conditions that must agree now sit 80 lines apart, so
`check-skill-workflow.py` asserts it. Divergence does not reopen the gate —
GitHub skips a job whose dependency was skipped, so `publish` goes with it — but
it silently stops the mirror on events where the gate does not run. The edit
that *would* reopen it is adding a status-check function to `publish`'s own
`if:`, since that replaces the implicit "every dependency succeeded" and turns
every `needs` entry into decoration; asserted separately. Both fault-injected.

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
anchors and selected critical absences.** Three obligations, because a `not
exposed` cell is not `n/a` but often has no enforceable proof either:

| Cell | Obligation |
|---|---|
| positive (`supported` / `partial` / `experimental`) | names an evidence anchor — source path, public symbol, compile-checked example, or conformance test — which CI resolves |
| negative (`not exposed` / `n/a`) | names its editorial rationale in prose; not machine-checked by default |
| **selected critical** negatives | additionally carry enforceable absence evidence, chosen case by case |

Absence is never inferred globally.

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
8. a link to its canonical checked example **where one already exists** — today
   that is the five `hello.*` files; Phase 4 adds the rest and links them. The
   routing refactor does not wait on the example expansion.
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

**The initial operation set is a public critical path per skill** — R1 gave one
list, and it was an event-bus capability/invocation journey that does not
describe payments at all:

```
net-event-bus     construct/start · publish+subscribe OR announce/discover/invoke ·
                  observe · handle one failure · shutdown

net-payments      caller:   inspect pricing · invoke · hit approval requirement ·
                            approve · pay · classify outcome · close
                  provider: construct · author pricing terms · publish one paid
                            capability · gate an invocation · read billing · close
```

**Not all of that ships at once.** The first checked route is deliberately one
per skill: the event-bus journey, and **one** payments demand path. The provider
path follows only once the demand path is green in every binding that claims
support. Naming the initial route is what keeps "bounded" real.

Subsystem examples get added beyond that only where a binding's shape is
surprising or commercially important — not by default.

**One canonical location per example, and companions link only.** The runnable
file under `examples/` is canonical; the companion points at it and does **not**
quote it. A hand-copied excerpt diverges as readily as a duplicated file — the
thing this rule exists to prevent — and "just a short excerpt" is exactly how
that starts. If reading experience later proves excerpts are needed, add them
mechanically (included from the file, or CI-verified as a literal contiguous
region), never by hand.

*Status: the checker generalisation is done (manifest, not directory
conventions) and the **second** route shipped in all five bindings — but it is
not the one named above.*

`announce/discover/invoke` was attempted first and deliberately deferred. The
canonical Rust version already exists at
`net/crates/net/sdk/examples/nrpc_echo.rs`, and this plan forbids duplicating
it; meanwhile Go and C have **no** existing announce+invoke reference anywhere
in the tree to derive a correct example from. Writing one from the headers
would have been exactly the invention this programme exists to prevent — the
fabricated Rust predicate builder came from precisely that move.

What shipped instead is the rest of the journey, which `hello.*` did not cover:
**ingest under backpressure · read stats · handle one failure · shutdown**
(`observe.*`). It was chosen because every API it needs is verifiable per
binding, and because it checks the claim the skills repeat most and verified
least — "drops are silent, watch `events_dropped`". That paid immediately:
`observability.md` asserted "the Go binding mirrors `net_stats_ex` directly —
same field names", which is false in two ways at once.

`announce/discover/invoke` stays the next route. It needs a Go and a C
reference first, or an accepted `absent` entry with that as the reason.

**The checkers need extending, and that is the real cost.**
`check-skill-snippets.py` supports Rust only *by design* — a non-Rust marker is
a hard error — and `check-skill-examples.sh` is hardcoded around the five
`hello.*` files. Either generalise to a manifest, or extend per known directory
structure. Decide before starting.

Report coverage with explicit `n/a`, so a missing example is never mistaken for
an unsupported binding:

```
announce:  Rust ✓  TS ✓  Python ✓  Go ✓            C ✓
nRPC:      Rust ✓  TS ✓  Python ✓  Go ✓            C not-exposed
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

Stated at the precision the implementation actually supports, since the
appendix's own point is that these catch *wrong*, not *incomplete*:

- cited repo paths exist **and are tracked in git** (so a local build artifact
  cannot mask a bad citation);
- **a hard-coded set of 18 high-risk callable symbols** resolves — not every
  documented symbol;
- backticked, qualified `Enum::Variant` citations are checked for **membership
  in known Rust enums, and for shape** — a tuple variant written `Foo { x }`
  fails, and so does the reverse;
- snake_case metric/config identifiers of ≥12 chars appear somewhere in source;
- **one registered cross-binding vocabulary — the nRPC wire kinds** — agrees
  across its checked sources; this is not a general comparison of all
  vocabularies;
- Rust snippets carrying `<!-- skill-check: compile -->` compile (2 of 114);
- the docs site carries the same path / variant / identifier / CLI checks,
  excluding release notes.

Two honest qualifications, both corrected from R1:

- **All five hello-worlds now gate publication, and none of them *runs*.**
  C is syntax-checked, Go gets `go vet`, Rust builds, Python gets mypy — all in
  `skills.yml`'s `examples` job. TypeScript is type-checked in a separate
  `typescript` job, because it needs a ~10-minute `napi build` to produce the
  gitignored `index.d.ts`; that job is restricted to publication-eligible events
  and `publish` needs both. On a branch or PR the same script runs in `ci.yml`
  instead. No example's behaviour is executed anywhere, so the README's "prints
  exactly one line" is still unverified; that is the Level 2 release-execution
  proof, not this.
- **The checks catch *wrong*, not *incomplete*.** Only backticked, qualified
  `Enum::Variant` citations are seen, and a page listing three of four variants
  passes. That defect existed in `guides/gang-scheduler.md` and was found by
  reading.

The gap this plan closes is not correctness. It is that an agent loads five API
shapes to write one.
