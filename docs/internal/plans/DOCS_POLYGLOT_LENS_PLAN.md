# Documentation — The Polyglot Lens

**Status:** REVISED ×3 (2026-07-30). **Phases 1A, 2 and 3 are EXECUTED** — see
*Execution log* below for what landed and the two findings that change Phase 5's
cost model. Kyra reviewed the 2026-07-29 draft, then
reviewed the revision and **signed Phase 1A and Phase 2 for execution** subject to
seven corrections. A third pass cleared six stale contradictions and decided D8.
All applied below. **Phase 1A and Phase 2 are signed; Phase 3 is specified; Phase 4
is gated on real self-serve evidence.** Nothing executed yet.

This plan turns Kyra's 36,000-foot concept into a decision list, a phase order, and
the CI gates each phase has to pass. It supersedes nothing:
`DOCS_STRATEGY_PLAN.md` (positioning, worldview, agent briefs) and
`DOCS_SDK_SPINE_PLAN.md` (the five-language SDK spine) both landed and stay true.
This plan is the layer above them.

**Governing principle (Kyra's, quoted, unmodified):**

> Turn Net's docs from documentation of a Rust system with bindings into
> documentation of a multilingual protocol and runtime.

**The central architectural correction from review, also Kyra's:**

> Do not implement the polyglot lens as five authored versions of every page.
> Implement one canonical operation model composed with a selected binding
> expression.

Scope: `web/src/content/docs/` (149 pages) and the docs machinery in
`web/src/lib/docs.ts`, `web/src/docs.order.ts`, `web/src/components/Docs*.tsx`,
`web/src/store/useLanguageStore.ts`.

---

## Review disposition

### First review — architecture

| Item | Verdict | Where it landed |
|---|---|---|
| Doctrine | approve | unchanged |
| Measured baseline | approve | unchanged |
| Parity + snippets before the rewrite | approve | Phase 2 |
| Critical-path tracer as commitment boundary | approve | Phase 4 |
| Full 170 renditions | **do not commit** | scope arithmetic rewritten |
| D1 URL shape | **modify** | bare route is no longer Rust |
| D2 rendition storage | **modify** | composition; `shared_hash` deleted |
| D3 snippet source | **modify** | neutral canonical location + evidence levels |
| D4 ledger ownership | **modify** | domain records under docs, skills derive |
| D5 absence pages | **modify** | generated from the record |
| C as a fifth lens | **reject** | four lenses + a C boundary annex |
| Phase 1 diagrams | **decouple** | split 1A / 1B |
| Six-question spine everywhere | **modify** | semantic slots + archetypes |
| Phase 2 vs the existing corpus | **modify** | migration states, monotonic ratchet |
| Decision deadline label | **modify** | per-decision blocking phase |
| Predecessor-plan contradiction | **resolve** | D7 — narrower than assumed |

Four things the first round of changes newly obliged, which this plan owns:
**near-duplicate content across renditions** (D1), **link-checking prose that lives
in a data file** (D5), **committing and diffing generated skill copies** (D4), and
**a cookie plus middleware on a site that currently ships neither** (D1) — since
resolved by freezing the static router.

### Second review — required before execution

| Required change | Where it landed |
|---|---|
| Add the self-serve acceptance gate | Phase 4, plus doctrine 8 (the services gate) |
| Add the opensrc / source-inspection escalation path | D3 |
| Correct the exact tier and pending counts | taxonomy table — computed, 149/149 |
| Make the page taxonomy exhaustive | same, with reproducible assignment rules |
| Define generated `/c` boundary routes | D1 |
| Specify the pending-allowlist ratchet | Phase 2 |
| Clarify D7's canonical-URL language | D7 — "navigation owner", canonical unchanged |

### Decisions frozen by the second review

| Decision | Frozen as |
|---|---|
| bare route | static neutral router first; no middleware, no cookie |
| capability records | `docs/data/capabilities/` |
| example manifest | `docs/data/examples.yaml` |
| example source | stays in native SDK / build locations |
| record prose ownership | reviewed in the same change as the status it explains; no owner registry |

One departure from the review text, stated where it occurs: the **adaptive count is
29 rather than 36** — section READMEs and four measured zero-code guides are
universal. The rules are given; overrule them and the manifest moves the pages.

### Third review — stale contradictions cleared

| Fix | Where |
|---|---|
| Drop the `semantic_owner` field and review matrix | D4 — two sentences, no ceremony |
| `sdk/c` is five pages, not a "2-page spine" | information model |
| Examples are *indexed* neutrally, not relocated | Phase 2 |
| No cookie/middleware language left in Phase 3 | Phase 3, and the risk it replaced |
| Phase 4 arithmetic is 7 bodies / 28 fragments | scope arithmetic + Phase 4 |
| "four lenses project four syntaxes" | Phase 4 |

**D8 decided: compose the existing SDK spine in place**, and keep it classified
`sdk_native` — composition is a storage mechanism, `sdk_native` is an IA
classification, and they do not collapse into one concept.

---

## Execution log

**Phase 1A** — reading surface. Code blocks scroll instead of wrapping (at 375px a
53-char ASCII diagram line needed ~400px in a ~311px column, so every diagram
wrapped mid-box). Language selector is persistent chrome at every breakpoint. Found
en route: the sidebar footer hardcoded `v0.17` on all 149 pages while the newest
release note was v0.33 — now derived.

**Phase 2** — `docs/data/` established as public product metadata, holding three
records with generated, equality-checked derivatives: `capabilities/*.yaml` (145
cells, 119/119 anchors resolving, skill copies byte-matching),
`examples.yaml` (evidence levels declared and enforced), `tiers.yaml` (149/149
pages, exhaustive and disjoint, allowlist ratchet). `check-skill-coverage.py`
retired — two of its structural assertions became impossible to violate rather than
unchecked. Found en route: the payments matrix told Rust callers to install
`net-mesh-payments`, which does not exist on crates.io under that name or its own.

**Phase 3** — composition, per-language routing, the three live defects fixed, and
concepts measured clean (five Rust-idiom hits → zero). `assertNoCrossLanguageNeighbours()`
runs inside `generateStaticParams`, so every build proves prev/next never crosses a
language boundary; verified non-vacuous by regressing the `sdk/go` gate and watching
the build fail with 32 named cases.

Found en route, in the page the plan chose as its tracer: `start/install` shipped a
version pin for an unpublished release, `EventBus` as the Node and Python entry
point when the export is `Net`, and three C symbols (`net_bus_new`,
`net_bus_ingest`, `net_bus_shutdown`) that exist in no header. Wrong
install-and-first-call code in three of five languages, on the first page a
self-serve reader opens.

**Phase 4** — the spine composed in place: 7 universal bodies + 28 fragments at the
URLs they already had, 149 pages → 128, `sdk_native` 34 → 13, pending allowlist
untouched at 28. Parity badges render from a generated, equality-checked JSON
bridge over the capability record — the seam the Phase 3 log flagged for later is
gone. `check-spine-symbols.py` holds 194 symbols at evidence level `source-match`,
with a planted-defect test. Measurement in
`docs/internal/misc/DOCS_POLYGLOT_PHASE_4_MEASUREMENT.md`; the self-serve gate is
specified and **not run**, in `DOCS_POLYGLOT_PHASE_4_GATE.md`.

Found en route: nine defects, all on the critical path, all of one shape — see
*Three findings from the spine* below. Also that adaptive pages had no prev/next
at all: `addFolder` pushed a folder entry only for folders with a README, and an
adaptive page has `_shared.md`, so `start/install` had been absent from every
reading order since Phase 3.

### Three findings from the spine

**1. The universal share is lower than the plan predicted, and the predictor is
different from the one the plan named.** Phase 3 expected a broader sample to land
above 19%. Seven of seven spine pages landed at or below it; the mean is 16%. The
predictor is not how much of a page is prose — it is whether the page teaches a
**decision** (universal, so composition pays: `redis-dedup`, 50%) or a **surface**
(constructors, handle types, error types — irreducibly per-binding, so it does not:
every spine page, 14–19%). Phase 5's ordering should stop expecting savings from
SDK-surface pages and convert them for correctness instead.

**2. The composed spine is 71% larger than what it replaced, and none of it is
duplication.** 2187 authored lines against 1277. The growth is nine corrections,
two declared absences, 28 verification steps, the five failure witnesses and the
handshake — content the old pages did not have. "Saved 1053 lines" is true against
writing that content four times and misleading as a claim about the diff. The
honest framing: composition did not shrink the corpus, it made a 71% content
increase cost 71% instead of 300%.

**3. The strongest argument for composition is not the one this plan made.** The
nine defects were not four syntaxes drifting apart. They were **one object model —
Rust's — projected onto three bindings where it does not hold**: in Rust the tool
API hangs off the `Mesh` node, everywhere else off a typed RPC surface the
ergonomic wrapper does not carry. Four pages made the same error four times
because they were written from one Rust original by translators who changed the
syntax and kept the object graph. The plan's argument for composition was cost.
The real payoff is that a universal body is a *place to say the model once, at the
level where it is true*, which forces the fragment underneath to show its own
constructor rather than inherit an assumption. Four parallel manuals have no such
place, which is why they drifted in the same direction.

### Two findings that change the cost model

**1. The universal share varies enormously by page type.** Measured on the two
converted pages, non-blank lines:

| Page | Universal | Fragments | Authored | 4 standalone copies would be | Universal share |
|---|---|---|---|---|---|
| `start/install` | 51 | 223 | 274 | ~504 | **19%** |
| `reference/redis-dedup` | 55 | 56 | 111 | ~284 | **50%** |

Composition saves roughly `(lenses − 1) × universal`. So the saving is a function of
how much of a page is genuinely language-specific — and **installing is almost
entirely a per-language act**, which makes `start/install` close to the worst case.
The tracer therefore under-sells the model rather than flattering it; a broader
sample should land higher than 19%. Phase 4 should report this table per page rather
than a single average.

**2. A page carrying real C content costs more than its line count.** The tracer
converted cleanly only because its C material was already covered by `sdk/c`.
`redis-dedup`'s C section was 22 live lines — allocation, `never returns NULL`,
return codes, `free` — and since C gets no authored fragment by design, converting
would have deleted it. **Every conversion of a page with real C content needs an
annex home first.** Phase 5's demand ordering should weight that in: two pages of
equal length are not equal work if one of them has a C section.

Here the annex was already the right shape — a handle you allocate and free is an
ownership question, so it belongs beside the three memory rules in
`sdk/c/memory-and-threading` — which is the annex's organising principle working
rather than being worked around.

### One seam to remove later

The boundary route needs to point at a specific annex page, not the section index.
Until the capability record can be read by the site, the universal body declares its
target in frontmatter (`boundary` / `boundaryLabel`). D5 replaces this with
`alternative.href` from the record once a generated, equality-checked JSON bridge
exists; the code says so at the seam.

**Phase 4 built the bridge** (`web/src/lib/generated/capability-record.json`,
generated and equality-checked by `capability_records.py`), so the site can now
read the record and a page declares its operation with `capability:` instead of
typing a support claim. **`boundary` stays for now**, because the record's
`alternative` field is still unpopulated — the bridge removed the obstacle, not
the authoring. Populating `reason` and `alternative` per negative cell is what
retires `boundary`, and it is a Phase 6 job now that nothing blocks it.

---

## Baseline — what is actually true today, measured

Before deciding what the docs should become, here is what they are. Every number
below is counted from the tree, not estimated.

**Code fences by language, outside `sdk/`:**

| Section | pages | Rust | TS | Python | Go | C |
|---|---|---|---|---|---|---|
| `guides` | 22 | **99** | 5 | 4 | 2 | 0 |
| `tutorials` | 4 | **22** | 0 | 0 | 0 | 0 |
| `payments` | 10 | **17** | 0 | 0 | 0 | 0 |
| `reference` | 15 | **28** | 2 | 1 | 1 | 1 |
| `start` | 5 | 4 | 2 | 1 | 0 | 1 |
| `concepts` | 12 | 2 | 0 | 0 | 0 | 0 |
| `worldview` | 5 | 0 | 0 | 0 | 0 | 0 |
| **total** | | **172** | **9** | **6** | **3** | **2** |

Outside the SDK spine the docs are **89.6% Rust by code volume**. `sdk/` is the
only balanced section (rust 16, ts 15, go 13, python 10, c 4) — which is exactly
the shape "a Rust system with bindings" produces: one polyglot annex bolted to a
Rust body.

**The concepts are already nearly clean.** Grepping `concepts/` and `worldview/`
for Rust idiom (`Arc<…>`, `Type::method`, `impl`, `&str`, `.await`, `tokio::`)
returns **five hits across two files** (`concepts/channels.md`,
`concepts/agent-identity.md`). Kyra's step 3 — "separate concepts from Rust
expressions" — is therefore a **small job, mostly done**. The mass of the work is
in steps 4 and 5: guides, tutorials, payments.

**The docs already know they are Rust-first.** `guides/event-bus.md` opens with:

> **A note on languages.** The detailed sections below are in Rust, because the
> core crate is Rust and the tuning knobs … are exposed most directly there.

That paragraph is the whole problem stated as an apology. It is honest, and it
is what we are removing.

**Two Rust-native pages are filed as language-neutral reference.**
`reference/eventbus-api.md` (8 Rust fences, 0 others) and
`reference/adapter-trait.md` (8 Rust fences, 0 others) sit in `reference/`, a
section a TypeScript reader is told is theirs. They are Rust SDK reference. D7
fixes that without moving a file.

### Defects in the existing language mechanism

The switcher exists (`LANGUAGES = ["rust","ts","python","go","c"]`,
`DEFAULT_LANGUAGE = "rust"`, zustand + `localStorage` + `?lang=` override) and it
gates exactly one thing: **which `sdk/<lang>` folders appear in the sidebar**
(`DocsSidebar.tsx:65`, `entryVisibleIn`). Four consequences, all live today:

1. **`getPrevNext` is language-blind** (`lib/docs.ts:536` → `getLinearDocs()`
   flattens the whole tree). A Python reader who finishes `sdk/python/errors`
   and presses *Next* is delivered into `sdk/go/quickstart` — a language they did
   not choose, in a section their own sidebar hides.
2. **Gated pages render but vanish from the nav.** `page.tsx` has no language
   logic at all; gating is sidebar-only. Land on `/docs/sdk/go/invoke` with
   `rust` selected and the page is fine while the sidebar has no entry for where
   you are.
3. **Search is language-blind** (`DocsSearchModal.tsx` has no language import).
   Searching `invoke` returns five near-identical hits with no indication which
   one is yours.
4. **The page body never changes.** The selection is a sidebar filter, not a
   reader context. This is the gap between what we have and Kyra's sentence
   *"the docs remember that choice everywhere."*

Two more, from the visual brief:

5. **Diagrams are ASCII box-drawing inside `<pre>`**, and `DocsContent.tsx`
   renders `pre` with `whitespace-pre-wrap break-words` (lines 521, 527). On a
   narrow screen those diagrams wrap and shred. That is the mobile problem,
   concretely.
6. **The language selector is not persistent context.** It lives inside the
   sidebar `<aside>`, which is `hidden lg:block`; on mobile it is reachable only
   after opening the nav drawer (`DocsDrawer.tsx:120`). Kyra's "buried above the
   sidebar" is literally where it is.

7. **There is no version context to be persistent about.** No version selector,
   no versioned content tree, one live version. Kyra's "persistent language and
   version context" is half-buildable today. Flagged, not assumed — see D6.

8. **There is no middleware and no cookie anywhere.** `web/src/middleware.ts`
   does not exist, nothing under `src/` reads `cookies()` or `document.cookie`,
   and the docs route is `dynamic = "force-static"` with `dynamicParams = false`.
   This matters for D1: server-side language resolution is buildable, but it is
   net-new runtime surface on a site that currently ships none.

---

## Doctrine (non-negotiable)

1. **The reader's language is a lens over one model, not five products.** The
   universal body of an adaptive page exists **once** and is composed into every
   rendition. If a binding fragment restates it, that is a defect.
2. **No fake parity, no silent fallback.** A surface that does not exist in a
   binding is disclosed with its real status and the nearest honest path. Falling
   back to a Rust snippet on a page a Python reader selected is the specific
   failure this plan exists to prevent.
3. **One authored parity record per domain.** Portable copies are generated and
   equality-checked. Two hand-maintained matrices diverge.
4. **Every code block on the critical path declares an evidence level, and CI
   enforces the level it declares.** "Everything executes" is a promise the
   machinery cannot keep; see D3.
5. **Every new checker gets a planted-defect test.** Same discipline as
   `check-skills-depth.sh` and `check-skill-source-paths.py --self-test`: a check
   nobody has watched fail is not known to work.
6. **Concepts stay conceptual.** A concept page may name a wire field, a state, or
   a guarantee. It may not require the reader to know what `Arc` means.
7. **Migration is monotonic.** A page may move toward *adaptive* and never back,
   and no new page may be created in the legacy state. See Phase 2.
8. **The services gate.** Public documentation, packages, skills, examples and
   source-inspection guidance must be sufficient for a competent team to integrate
   and operate Net **without retained assistance**. No ordinary implementation
   knowledge is reserved for stewardship customers. This plan does not describe the
   retainer; it exists to stop the documentation architecture from accidentally
   manufacturing a services dependency. Phase 4's self-serve gate is how the
   doctrine gets tested rather than asserted.

### Required information, and where it lives

The draft mandated six literal sections in a fixed order on all 29 adaptive
pages. Review is right that this produces formulaic pages — installation,
troubleshooting and a tutorial do not share a shape. What must be constant is the
*information*, not the headings.

**Universal body (`_shared.md`) — authored once, composed into every rendition:**

- the objective: what are we accomplishing;
- the guarantees and boundaries that matter;
- the operation model: what happens, in what order.

**Binding fragment (`<lang>.md`) — authored per lens, only where it teaches:**

- the implementation in that language;
- runtime and lifecycle caveats peculiar to it;
- verification: how the reader knows it worked.

**Generated from the domain capability record — never hand-written:**

- support status and mode for this operation in this binding;
- the absence state, when there is nothing to teach (D5).

Page archetypes carry different narrative freedom: **task guide**, **tutorial**,
**operational guide**, **troubleshooting guide**, **security/authority guide**.
The eight critical-path pages use the strict six-slot template, because they are
the tracer and comparability is the point. Phase 5 pages must carry the required
information in an archetype-appropriate narrative — the checker asserts the slots
are present, not that headings match a string.

---

## The information model, applied to the real 149 pages

**Four adaptive lenses: Rust, TypeScript, Python, Go. C is a boundary annex.**

Review rejected C as a fifth lens, and the draft's own evidence supports it: the
C ABI has a **five-page boundary-native section and no general capability lens**,
because the ABI is bus-only, and `guides/` contains **zero** C fences today. Minting 34 C renditions that mostly
read "not exposed" does not make the docs more honest — it makes the information
architecture assert that C is a product shape it is not.

### The taxonomy, exhaustive and disjoint

Review is right that an approximate table cannot support a checker that requires
every page to declare a state. This one is computed, not estimated: **149 pages
in, 149 classified, zero unclassified, zero in two tiers.** Phase 2 lifts it
verbatim into the tier manifest and generates this table from it, so the prose
arithmetic cannot drift again.

| Tier | Pages | Renditions | Contents |
|---|---|---|---|
| **universal** | **79** | 1 each | `worldview` 5 · `concepts` 12 · `releases` 35 · `reference` 11 · `payments` 6 · `guides` 5 · `start` 3 · `tutorials` README · `agent-briefs` README |
| **adaptive** | **29** | universal body + ≤4 fragments | `guides` 17 · `payments` 4 · `tutorials` 3 · `agent-briefs` 3 · `start` 2 |
| **sdk_native** | **36** | already per-language | `sdk/{rust,typescript,python,go}` 32 + the 4 reclassified `reference` pages (D7) |
| **boundary_native** | **5** | its own shape | `sdk/c` 5, plus the annex it grows into |

Assignment rules, so the manifest is reproducible rather than a judgement call:

- **A section `README.md` is universal.** It is navigation, not a task. That is 8
  section indexes; the 5 SDK-language READMEs are inside a language spine and take
  that spine's tier.
- **A page with zero code fences and a CLI or doctrine character is universal.**
  Measured, not guessed: `guides/wrap-mcp-server`, `expose-net-as-mcp`,
  `nat-and-traversal` and `submitted-is-not-completed` each contain **0** code
  fences. Giving them four fragments would manufacture per-language content for
  pages that have none.
- **Reference pages that are protocol or operator surface are universal**
  (`wire-format`, `cli`, `deck`, `capability-schema`, `filter-dsl`, `glossary`,
  `mcp-bridge`, `error-codes`, `subprotocol-ids`, `versioning`, README). The four
  Rust-native ones are `sdk_native` per D7.

**Two corrections to accept, one count to reconcile.** Review is right about the
SDK spine — it is 8 pages each for Rust, TypeScript, Python and Go, and 5 for C,
so **32 sdk_native and 5 boundary_native**, not the 33 and 4 the last revision
printed. That was a miscount of the per-language READMEs.

The adaptive count reconciles as follows: review's 36 counts the `guides`,
`tutorials` and `agent-briefs` READMEs (−3 under the rules above) and the four
zero-fence guides (−4), giving **29**. If any of those four should be adaptive —
`wrap-mcp-server` is a plausible candidate, since `net-mesh wrap` is CLI-driven
but what you do with the wrapped capability is not — say so and the manifest moves
them; the count is derived, not asserted.

### The scope arithmetic, corrected

The draft's headline number was wrong in two ways at once, and both came from
treating a rendition as a document.

```
draft:      34 pages × 5 languages  =  170 complete documents
corrected:  29 universal bodies
            + up to 29 × 4 = 116 short binding fragments
            + 0 absence documents (generated from the record)
```

Under composition the universal text is authored once, so the multiplied unit is
only the part that is genuinely language-specific: install line, construction,
runtime caveat, verification. A fragment is a fraction of a page.

**What I will not do is guess the ratio.** The honest number is unknown until
something is built, so Phase 4 measures it: 7 universal bodies and up to **28
fragments** (D8), reporting authored lines universal vs per-fragment. That ratio is the
input to the Phase 5 decision, not a number picked now.

Two further reductions fall out of the model: a fragment exists only where the
binding has something to teach (Go has no A2A and no filter DSL, so those pages
have three fragments, not four), and no page needs a C rendition at all.

### The critical path already exists, as four parallel manuals

Computing the taxonomy surfaced something neither the draft nor the review states,
and it changes what Phase 4 touches.

Review's reframed tracer maps **one-to-one onto the existing SDK spine**:

| Tracer step | Existing page |
|---|---|
| install | `start/install` (adaptive) |
| create a node | `sdk/<lang>/quickstart` |
| define + provide a capability | `sdk/<lang>/announce` |
| resolve it elsewhere | `sdk/<lang>/discover` |
| invoke | `sdk/<lang>/invoke` |
| watch | `sdk/<lang>/watch` |
| move an artifact | `sdk/<lang>/artifacts` |
| prove denial, deadline, failure | `sdk/<lang>/errors` |

Those 7 pages exist **four times over** — 28 pages, 1,922 lines — and by
construction they are the same content in four syntaxes:
`DOCS_SDK_SPINE_PLAN.md` specifies "one conceptual spine, five language bindings.
Every SDK page teaches the same agentic step in the same order." Per-page
renditions run 34–123 lines.

**D8, decided: compose the existing spine in place.** Not eight new guide pages
describing the same journey. The spine is the single best target the corpus has for
the composition model — it removes duplication that already exists rather than
creating more, it is small enough to convert inside one phase, and it proves the
model on content four authors already agreed teaches the same thing.

```
28 pages  →  7 universal bodies + 28 fragments
```

**It costs no URLs.** `/docs/sdk/python/announce` already carries the language
segment — as a prefix rather than a suffix — so composition applies underneath the
existing routes with zero churn, satisfying `DOCS_STRATEGY_PLAN.md`'s "never by
moving files out from under inbound links" without a single redirect. The price is
two URL shapes for adaptive content: prefix for the spine, suffix (D1) for guides.
Cheaper than breaking 28 indexed URLs, and arguably honest —
`sdk/python/announce` means "the Python SDK's announce page" while
`guides/event-bus/python` means "this guide, read in Python."

**The spine stays classified `sdk_native`.** Review's refinement, and it is the
right distinction: **composition is a storage and authoring mechanism;
`sdk_native` is an information-architecture classification.** They do not collapse
into one concept. A composed spine page is still SDK reference — it has simply
stopped being written out four times:

```
/docs/sdk/python/announce
  = shared announce model  +  Python binding fragment
```

So the tier table does not move a single page, and the ratchet arithmetic is
untouched by Phase 4's main work:

```
adaptive_pending at end of Phase 2:  29
Phase 3 converts start/install:      28
Phase 4 composes the spine:          28 sdk_native pages, still sdk_native
```

**One consequence to name rather than absorb.** `start/quickstart` is
`adaptive_pending` and stays that way, so after Phase 4 a Rust-only
`start/quickstart` sits beside a composed `sdk/<lang>/quickstart`. That is the
highest-traffic page in the corpus reading Rust-only while its SDK twin speaks four
languages — not fake parity (its state is declared), but the most visible remaining
gap. It is therefore first in Phase 5's demand order, not an expansion of Phase 4.

---

## Decisions — with the phase each one blocks

Review is right that the draft mislabelled this section. Phase 2 builds the
rendition checker, extends the snippet model, promotes the ledger and defines
absence handling, so **D2–D5 must be frozen before Phase 2**, not Phase 3. Even
language-aware link validation needs the URL shape.

| Decision | Blocks | Status |
|---|---|---|
| D1 URL shape + the `/c` route | Phase 2 (link validation), Phase 3 (routing) | **frozen** |
| D2 rendition storage | Phase 2 | frozen |
| D3 example location, manifest, evidence levels, escalation path | Phase 2 | **frozen** |
| D4 parity record location + ownership | Phase 2 | **frozen** |
| D5 absence generation | Phase 2 | frozen |
| D6 version context | — | unchanged (out of scope) |
| D7 predecessor-plan resolution | Phase 3 | clarified |
| D8 what Phase 4 converts | Phase 4 | **frozen** — compose the spine in place, keep it `sdk_native` |

### D1 — URL shape for adaptive pages

**Every rendition gets an explicit language segment, Rust included:**

```
/docs/guides/event-bus/rust
/docs/guides/event-bus/typescript
/docs/guides/event-bus/python
/docs/guides/event-bus/go
```

**The bare route is a neutral static router. FROZEN.** The draft had it render
Rust and offer a switch link; review is right that this keeps Rust as the
privileged public meaning of the page — the exact framing the project exists to
remove. Review's answer to the follow-up question is also the right one, and it is
now the decision rather than a preference:

```
bare adaptive URL
  → universal objective and guarantees
  → four rendition choices
  → the C boundary choice
  → explicit support state for each
```

**No middleware, no cookie, for now.** The reasons are review's and they hold: the
site is fully static today, internal links can carry the selected language so the
bare route is mostly an external-entry concern, one initial selection is
acceptable, and a cookie beside `localStorage` creates a synchronisation and
privacy surface for the sake of one click. Server-side resolution stays available
if analytics later show the router costs adoption — but the static router has to
exist as the no-JS path regardless, so it is what gets built first. **Do not add
runtime infrastructure preemptively to save one click.**

Universal pages keep their single URL with no language segment. A prefix there
would mint four duplicate URLs for one page.

**The C route is defined, and it is not a rendition.** Selecting C on an adaptive
operation needs a shareable URL, so it gets one:

```
/docs/guides/event-bus/c
```

It renders the universal operation body + the generated C support status + a link
to the nearest C ABI operation, and **no fabricated ergonomic implementation**.
There is no `c.md` fragment and never will be. Three types the implementation must
keep distinct:

| Type | Values |
|---|---|
| reader context (the pills) | `rust · typescript · python · go · c` |
| fragment language (authorable) | `rust · typescript · python · go` |
| the `/c` route | a generated boundary projection |

**The obligation this creates.** Under composition, four renditions of a page
share their entire universal body — near-duplicate content by design, and a search
engine will read it that way. The mitigation is standard and must be built in
Phase 3 rather than retrofitted: each rendition self-canonical,
`alternates.languages` declaring its siblings, the bare route canonical to itself
as the router. `generateMetadata` already sets `alternates.canonical` per page, so
the hook exists.

### D2 — How a rendition is stored

**Composition, not duplication.**

```
guides/event-bus/
  _shared.md        # the universal body — authored once, rendered in every route
  rust.md           # fragment: implementation, caveats, verification
  typescript.md
  python.md
  go.md             # absent where there is nothing to teach — see D5
```

A route renders `_shared.md` + the selected fragment + generated support
information. The universal text is not copied, so there is nothing to keep in
sync.

**`shared_hash` is deleted.** The draft put a hash of `_shared.md` in every
rendition's frontmatter to prove the copies matched. Review is right that this is
bookkeeping masquerading as integrity: edit the shared body, recompute, update
four files. With composition there are no copies to compare.

What the checker asserts instead:

- the shared fragment exists, and the route composition includes it;
- each fragment declares its `lang` and its parity key;
- **no fragment contains a second copy of a universal heading** — the concrete
  test for "did someone start writing five manuals again";
- every `lang` is in the closed lens set (`rust|typescript|python|go`).

Composition also settles the draft's argument against the one-page/five-tabs model
without needing the argument: exactly one rendition's prose is ever in a document,
so `rehype-slug` cannot emit `verify-it-worked-3`, `extractToc` (`lib/docs.ts:554`,
which parses raw markdown) shows one entry per heading, and `check-doc-links.mjs`
can validate a fragment target.

The alternative — one file with `<Lang for="python">` blocks — additionally
requires the content tree to move from `.md` to `.mdx`, since `page.tsx:291`
passes `format={resolved.file.ext}` and every content file is `.md` today, so JSX
in content is not parsed at all.

### D3 — Where snippets come from, and what CI proves about each

**Neutralize the index and the contract, not the source location. FROZEN.**

```
docs/data/examples.yaml        # the manifest: paths, evidence levels, contracts
net/crates/net/sdk/examples/   # Rust source stays where cargo expects it
net/crates/net/sdk-ts/…        # TS beside its own tests
net/crates/net/sdk-py/…        # Python in the package
go/…                           # Go in the module
net/crates/net/include/…       # C compile/link fixtures
```

The previous revision moved the example *files* to a neutral directory for
symmetry. Review is right that this is the wrong half to neutralize: relocating
runnable source out of its native build location costs package-local
discoverability, native build integration, release coupling, IDE behaviour and
first-party provenance — all real, none bought back by conceptual tidiness. What
must be neutral is the **index** (which examples exist, at what evidence level,
under what contract) so that neither the docs nor the skills owns it.

`.github/skill-examples.json` therefore becomes a consumer of
`docs/data/examples.yaml`, not the record. The runner and per-language wiring are
unchanged and already work: `skill_examples.py` (validate / list / report /
run-spec) and `run-skill-examples.sh --lang <l>` execute five bindings across
`skills.yml` and `ci.yml`.

Docs pages transclude from the manifest-listed files. Inline fences stay legal only
at declared non-executable levels, and CI counts them so the exemption cannot
quietly become the norm.

**Evidence levels.** The draft's acceptance language — "every snippet is executed
by CI in its own language" — is a promise the machinery cannot keep, and review is
right that declaring the truth is stronger than pretending otherwise. Every code
block on the critical path declares one level, and CI enforces exactly that level:

| Level | Proves | Typical use |
|---|---|---|
| `run` | executes, exits 0, output matches its contract | quickstarts, the critical path |
| `compile` | compiles | Rust construction snippets |
| `link` | compiles and links against the real cdylib | C lifecycle examples |
| `typecheck` | type-checks against SDK source | TS API shapes |
| `source-match` | the quoted text matches the named source | wire layouts, config defaults |
| `expected-failure` | fails, in the named way | a denied grant, a rejected compile |
| `illustrative` | nothing — and says so | pseudo-code, wire dumps, shell transcripts |

Two rules on top, both from review: a block that *could* be `run` may not declare
something weaker without a reason, and **contracts assert observable state rather
than snapshotting nondeterministic output**. The existing runner already matches
stdout against a regex contract (`accepted: .*ingested=1`) rather than a literal
transcript, which is the right shape to generalize.

#### The escalation path — what an agent does when the example is not enough

Review is right that the revised plan handled canonical examples and then stopped,
leaving out the mechanism that started this work. Examples answer "how do I do
this"; they do not answer "what exactly does this return in my version". Without a
defined next step an agent invents an API, which is the failure the whole corpus
exists to prevent.

```
public docs
  → binding companion (which surface, which package)
  → checked example (at its declared evidence level)
  → exact public symbol
  → dependency source inspection
  → verification
```

The inspection step is already built and shipping: `source-access.md` in both
skills, and the docs page at `start/claude-skills`. It uses
[opensrc](https://github.com/vercel-labs/opensrc), pinned to the version the
consumer actually resolved:

```bash
opensrc path crates:net-mesh-sdk@<Cargo.lock version>
opensrc path @net-mesh/sdk@<package-lock version>
opensrc path pypi:net-mesh-sdk@<lock version>
```

**Provenance is not interchangeable, and this distinction has to survive into the
docs.** Verified while building `source-access.md`: opensrc resolves registry
metadata to a **repository checkout at the release tag** — all three spellings
above return the same directory. That is excellent for navigation and agent
legibility, and it is *not* the published artifact: a repo checkout can contain
files the package excludes and lacks generated files the package ships
(`bindings/node/index.d.ts` is the live example — napi-generated, git-ignored,
absent from any checkout).

| Purpose | Use |
|---|---|
| implementation navigation, agent legibility | `opensrc path …` |
| exact package contents, security adjudication | `cargo vendor --locked`, the registry source cache, `npm pack` |

So: opensrc to understand, the registry artifact to adjudicate. Phase 2 wires the
escalation path into the manifest's guidance and Phase 4's self-serve gate counts
how often it is needed — a high inspection count is a documentation defect signal,
not a success metric.

### D4 — Where the parity record lives

**One authored record per domain, in a neutral product-truth location. The skills
derive from it. Location FROZEN:**

```
docs/data/capabilities/event-bus.yaml
docs/data/capabilities/payments.yaml
docs/data/examples.yaml            # D3's manifest
docs/data/tiers.yaml               # Phase 2's tier manifest + pending allowlist
```

`docs/data/` is a **sibling of `docs/internal/`, not inside it** — the previous
revision raised this as an open question and review answered it. `docs/` today
contains only `internal/`, so `docs/data/` has to be established explicitly as
public product metadata rather than inheriting the internal-only reading of its
parent. That is a one-line README in `docs/data/` and a note in the repo's
contributing guide, both in Phase 2.

Generated or verified from that record:

- the docs rendering (support badges on every adaptive rendition, D5 absence);
- the skill-local `bindings/coverage.md` for each domain skill;
- later, an aggregate public compatibility page.

The draft proposed promoting `.claude/skills/*/bindings/coverage.md` to *be* the
canonical ledger. Review rejects that on publishing architecture, and the
hierarchy it protects is the right one: **docs are canonical product truth; the
website is category, outcome and navigation; skills are compact executable
guidance derived from verified truth.** A skill corpus that owns the parity record
becomes authoritative over the docs.

It also conflicts with a requirement the draft did not account for: domain skills
are **independently installable** — `npx skills add … --skill net-payments`
installs one directory — so each must still ship its own domain-local matrix.
Under the corrected model it does, and that copy is mechanical.

**The obligations this creates.**

- Generated copies must be **committed**, not built at install time, so a
  standalone skill install is complete. CI regenerates and diffs; a drifted copy
  fails.
- `check-skill-coverage.py`'s anchor-resolution logic does not die, it moves: it
  validates the canonical record (positive cells name a resolvable symbol anchor,
  negative cells do not, vocabulary is closed) and then asserts each generated
  copy equals its source.
- The record now carries reader-facing prose (D5's `reason`, `alternative.label`,
  `alternative.href`), so it needs an owner and its links need checking. Both are
  named in Phase 2.

**Ownership. FROZEN, and deliberately without ceremony.** No CODEOWNERS, no owner
registry, no review matrix. `domain: event-bus` already identifies what the record
describes.

> Reader-facing rationale is reviewed in the same change as the semantic status it
> describes. Generated copies have no independent authorship or review path.

The previous revision replaced a CODEOWNERS file with a `semantic_owner` field and a
review table, which is the same ceremony in a different format.

The vocabulary is already designed and in use: Status `supported · partial ·
experimental · not exposed · n/a`, plus an orthogonal Mode `poll · verify-only ·
core-only`. `core-only` matters enormously for docs — it is the difference between
"Python can do this" and "Python can do this **if you import from `net` rather
than `net_sdk`**", the most common way to be wrong about Net in Node and Python,
and today no docs page says it.

### D5 — What an absent rendition looks like

**Generated from the record, not authored.** The rule stands — an unsupported
binding gets an honest page state and never a Rust fallback — but the draft's
"absence is a written page, not a missing file" would create a second prose corpus
of hand-maintained stubs. The rationale belongs in the capability record:

```yaml
- operation: watch
  binding: c
  status: not_exposed
  reason: >
    The C ABI exposes polling over the event bus, not the high-level watch
    abstraction.
  alternative:
    label: Use the polling API
    href: /docs/sdk/c/quickstart
  tracking_issue: null
```

The route renders a standard absence state from that. A hand-authored absence
fragment is allowed only where the generated state is genuinely inadequate, and
the checker reports how many exist so the escape hatch stays visible.

`status` keeps the distinction the draft named, and it is load-bearing: **`n/a`
means "this operation makes no sense here"; `not_exposed` means "buildable, not
built".** The first tells a reader to stop asking; the second is a roadmap entry.

`alternative.href` is a docs link living in a data file, so
**`check-doc-links.mjs` must validate hrefs inside the capability records** — or
the honest-absence path becomes the one place broken links hide.

### D6 — Version context

**Out of scope, unchanged.** No versioned docs tree, no version selector, one live
version. Building versioning and the language lens at once doubles the routing
surface. Phase 1A's chrome leaves a slot; nothing more.

### D7 — The predecessor-plan contradiction (new)

Review flagged that `DOCS_STRATEGY_PLAN.md` freezes an IA constraint the draft
proposed to break, and that both cannot be normative at once. Reading the actual
text narrows the resolution considerably — `DOCS_STRATEGY_PLAN.md:84-88`:

> **Additive IA, never destructive.** … **Keep `concepts/` and `reference/`
> intact** — they are load-bearing (three reference pages were just ported into
> the `net-claude-skill`). No page deletions; renames only via `docs.order.ts`
> labels + slug redirects, **never by moving files out from under inbound links.**

The earlier plan already prescribes the mechanism: reclassify through
`docs.order.ts` and redirects, never by moving the file. So the two plans do not
actually conflict — the draft's word "move" did. **Resolution, no amendment
required:**

- **physical path and URL:** retained, so inbound links and the three ported skill
  pages keep working;
- **canonical URL:** **unchanged — it remains the existing `reference/…` URL.**
  Review is right that the previous revision's "canonical destination: the Rust SDK
  section" was ambiguous to the point of being wrong: emitting a canonical pointing
  at a different page that does not contain the same material is a real SEO defect,
  and classification alone must not touch canonical metadata. A canonical only
  moves if a second route is genuinely created, and none is here.
- **navigation owner:** the Rust SDK reference, via `docs.order.ts` (`languages`
  gating exists for exactly this). "Navigation owner" replaces "canonical
  destination" throughout, because that is all this change is.
- **plan ownership:** `DOCS_STRATEGY_PLAN.md` keeps doctrine 4; this plan owns the
  per-page classification that doctrine permits.

If a later phase does want a physical move, it needs an explicit amendment to that
plan plus a redirect — not a quiet reinterpretation here.

---

## Phases

Kyra's arc is: read better → language as context → de-Rustify concepts →
polyglot critical path → expand → parity honest. Two changes from the baseline
measurements, both approved in review:

- **Her step 3 is nearly done** (5 Rust-idiom hits in `concepts/` + `worldview/`)
  and shrinks to a cleanup inside Phase 3.
- **Her step 6 is already built** as vocabulary and anchors in the skills corpus,
  and moves *early* — Phase 4 cannot write an honest rendition without it. D4
  changes where it lives, not when it happens.

Phases 1A and 2 touch disjoint files (chrome vs CI scripts and data) and can run
in parallel. **Phase 1B must not block anything.**

### Phase 1A — The reading surface *(visible, independent, no content edits)*

- Reading column, type scale, and hierarchy between section / page / subsection /
  reference.
- Code blocks: language label always visible (so a screenshot is unambiguous),
  copy affordance, wrap behaviour that does not shred.
- **A mobile-safe `<pre>` treatment** — horizontal scroll or a scaled container
  instead of `break-words` — which stops the ASCII diagrams shredding (baseline
  defect 5) without waiting for a diagram system.
- Visual distinction between concept / guide / tutorial / reference / agent brief.
- Mobile navigation, and the language selector as persistent chrome at every
  breakpoint rather than inside the drawer (baseline defect 6).
- A slot for version context (D6). Empty.

**Acceptance:** `npm run build` and `npm run check` green; ASCII diagrams legible
at 375px; **zero changes under `src/content/docs/`**. This phase is chrome only —
the draft contradicted itself by promising no content edits while converting six
diagrams.

### Phase 1B — The diagram system *(EXECUTED)*

**The page list above was wrong and is corrected here.** It named six pages derived
from "files containing fenced blocks" — a proxy for something else. Measured
against box-drawing characters, four of the six have no diagram (`capabilities`,
`channels`, `subnets`, `agent-identity`); the real set is `concepts/architecture`,
`concepts/storage-stack`, `reference/replication-config` and the three tutorials.
Seven, with two in common.

Neither option the plan offered was taken, and the reason is a constraint the plan
did not weigh: **docs content is read raw by agents and three reference pages have
been ported verbatim into the skill corpus.** A `<Diagram>` MDX component would end
that portability, and seven hand-maintained SVGs would drift from the text they
illustrate. So the diagrams stay text and gain a fence info string —
```text title="…" — which renders as a captioned figure with no copy button and a
scroll affordance, and stays valid markdown in every other viewer.

Generalisable: **a presentational change that requires a syntax the rest of the
ecosystem cannot read is more expensive than it looks in a corpus that is
deliberately portable.**

### Phase 2 — The record, the harness, and a migration-aware checker

Requires D1–D5 frozen.

- **Capability records** (D4) authored per domain under `docs/data/capabilities/`;
  generation plus equality check for each skill's `bindings/coverage.md`; anchor
  and vocabulary validation moved onto the canonical record.
- **Canonical examples** indexed by `docs/data/examples.yaml` (D3), while the
  source stays in each binding's native SDK / build location. The manifest carries
  the evidence level per block, and each level is enforced by the existing
  per-language CI wiring.
- **Migration states.** Every page declares one:

  ```
  universal | adaptive_pending | adaptive | sdk_native | boundary_native
  ```

  `adaptive_pending` is an acknowledged legacy Rust-only page. This is the fix for
  the draft's impossible rule: a checker demanding a rendition per language would
  have failed all 29 adaptive pages the day it landed, before Phase 4 wrote the
  first one.

  **The ratchet is a committed allowlist, not a count.** Review is right that "may
  only decrease" is not implementable — a checker sees one checkout and has no
  memory of yesterday — and that a bare number is too weak anyway: it lets one page
  regress while another converts in the same change, netting zero.

  ```yaml
  # docs/data/tiers.yaml
  adaptive_pending:
    max: 29
    pages:
      - start/install
      - start/quickstart
      - guides/event-bus
      # … 26 more, the exact list
  ```

  CI proves all of:

  - every page in `adaptive_pending` state is on the allowlist;
  - no page not on the allowlist is in that state;
  - every listed page still exists (a deleted page is removed from the list, not
    left to rot);
  - the actual count is ≤ `max`;
  - a converted page is removed from the list in the same change;
  - every `adaptive` page satisfies the full rendition and absence checks.

  The allowlist shrinking is then visible in the diff and reviewable as content,
  which is stronger than any number a checker could remember.

- **`check-doc-links.mjs` extended:** language-aware rendition targets, and hrefs
  inside capability records (D5).

**Acceptance:** each new checker ships with a planted-defect test that fails
without the fix and passes with it, in the shape of `check-skills-depth.sh` and
`check-skill-source-paths.py --self-test`. No checker lands without one. The
pending count at the end of Phase 2 is **29** — a pass, not a failure.

### Phase 3 — Language becomes reader context, proved on one page

Requires D1, D2, D7.

- Routing for adaptive pages; an explicit language segment per rendition; the bare
  route as the neutral static router defined by D1.
- Per-rendition canonical plus `alternates.languages` (the D1 obligation).
- **Fix the three live defects:** language-aware `getPrevNext` (a Python reader
  never falls into Go), sidebar/page agreement (no stranded pages), language-aware
  search ranking.
- Language as persistent chrome, with the D6 slot beside it.
- "View in another language" / "Compare implementations" as a secondary
  affordance: the selected language is the rendition, comparison is one click,
  never five tabs fighting.
- Concepts cleanup: the five Rust-idiom hits. Nav reclassification of the four
  Rust-native reference pages per D7.
- **One real tracer page converted end to end** — `start/install`, which has the
  least prose and the most genuine per-binding difference. Review is right that
  routing, composition, search, navigation and the checker path should be
  exercised by one page before a 32-fragment fan-out.

**Acceptance:** prev/next never crosses a language boundary for a non-default
reader (a test, not an inspection); every gated page reachable from the nav of at
least one language; `start/install` renders in four languages from one universal
body; pending allowlist down to 28.

### Phase 4 — The critical path, the measurement, and the self-serve proof *(EXECUTED, except the gate)*

**D8's "it costs no URLs" needed one correction in execution, not in substance.**
The resolver built in Phase 3 understands only D1's suffix shape
(`<page>/<lens>`), while the spine's URLs are the prefix (`sdk/python/announce`).
D8 had already named the price — "two URL shapes for adaptive content" — so the
decision stood and the second shape got built: a section holding both lens folders
and adaptive page folders projects each page into each lens section. Zero URLs
moved. The handoff brief read D8 as having assumed this was free and recommended
moving 28 indexed URLs instead; the plan text had priced it, so the frozen
decision held.

Two consequences of the prefix shape, decided here: a projected page keeps its
bare route as D1's neutral router (the only language-neutral URL a universal body
can link a sibling at) and does **not** also get suffix routes; and it has no `/c`
route, because `sdk/c` is a real five-page annex that would collide by name. The C
pill points into the annex instead of at a generated absence beside it.


Eight steps, four lenses, one universal body each. Reframed per review so the
public developer model (**define → provide → resolve**) stays visible and the
failure step stops being vague:

```
install
  → define a capability
  → provide it
  → resolve it elsewhere
  → invoke
  → watch
  → move an artifact
  → prove denial, deadline and failure behaviour
```

That last step is the one the draft under-specified. "Handle failure" is not
sufficient for an authority boundary; the tracer must witness, at minimum:

- an **unauthorized invocation denied**;
- **grant expiry or revocation** taking effect;
- **deadline** behaviour;
- a **typed remote error** crossing the binding boundary;
- **ambiguous execution** guidance, where the binding exposes it.

Without those, four lenses project four syntaxes for the happy path rather than one
Net model. See D8 for which pages this phase touches.

Every fragment carries its runtime caveat, ends in a verification step, and takes
its code from the Phase 2 harness at a declared evidence level. Where a step does
not exist in a binding the route renders the generated absence state — and the
record already tells us some of these: A2A is `not exposed` in Go, several surfaces
are `core-only` in Node and Python.

#### The publishing measurement

- authored lines, universal vs per-fragment, **per page rather than averaged** —
  the two conversions so far came out at 19% and 50% universal, so a single mean
  would hide the thing that predicts cost;
- authoring effort per fragment;
- how much content turned out to be genuinely language-specific;
- **annex work required**: pages carrying real C content need a home in the C annex
  before they can be converted at all;
- binding gaps hit that the record did not predict;
- maintenance burden: what one API change costs across four fragments.

#### The self-serve acceptance gate

Review's most important addition, and the reason it matters: the measurements above
prove the *publishing system* works. They do not prove the commercial objective,
and Phase 4 could pass all of them while producing an elegant multilingual
architecture that still does not deliver a fifteen-minute first integration.

**The question the gate answers:** can an engineer or coding agent with no private
context and no access to Laz produce a working, conventional Net integration from
public artifacts alone?

```
inputs:      one supported language
             one existing public package or representative industrial SDK
             one typed read-only operation
             the public docs, the public skill, the public packages, the source

forbidden:   private implementation notes
             founder interpretation
             unpublished examples
             manual correction by the test author
```

Required output — a real integration, not a demo:

```
dependency manifest
customer-owned adapter source
capability declaration
provider
typed consumer
fixture-backed test
build and run instructions
a successful typed invocation
a denied-invocation witness
a deadline / failure witness
```

Recorded separately, and reported whether or not they flatter the docs:

| Metric | Why it is here |
|---|---|
| time to first successful call | the fifteen-minute claim |
| time to conventional minimal integration | the real adoption number |
| documentation dead ends | each one is a defect with an address |
| source inspections needed | high count = the docs abstracted over a detail that mattered (D3's escalation path) |
| invented or incorrect API attempts | the failure mode the skills exist to prevent |
| human interventions | any at all means the gate did not pass |

**Targets, to measure and not yet to publish:** ~15 minutes to a quick
demonstration, ~2–5 hours to a conventional minimal integration. These are
hypotheses this phase tests. They do not go into public copy until a run has met
them.

**Acceptance:** 7 universal bodies and up to 28 fragments build, replacing 28
authored spine pages that stay classified `sdk_native` (D8); every critical-path
block enforced at its declared level; every parity badge rendered from the record
and none typed by hand; the five failure witnesses demonstrated in each lens that
exposes them; the pending allowlist unchanged at 28, because this phase touches the
spine rather than the pending tier; the publishing measurement and the self-serve
run both written up.

### Phase 5 — Expand by demand, not by taxonomy

**Not approved by this plan.** It is scheduled after the Phase 4 measurement and
ordered by evidence rather than page count — review's criteria: traffic, support
burden, agent-failure frequency, commercial relevance, binding demand.

**First in that order, regardless of what the traffic says: `start/quickstart`.**
After Phase 4 it is the corpus's most-visited page still reading Rust-only while its
composed SDK twin speaks four languages (see D8). Then, for the self-serve wedge:
capability declaration, discovery, invocation, identity and grants, revocation,
watches, artifacts, intermittent failure, debugging and evidence. A rarely-read Rust tuning
guide does not earn four fragments because the taxonomy called it adaptive — it can
stay `adaptive_pending`, visibly, with its state declared.

**Acceptance:** unchanged gates. A section is "done" when no page in it carries an
undeclared absence — not when every page has four fragments.

### Phase 6 — Parity made visible

The capability records, true and enforced since Phase 2, become a reader-facing
aggregate: the full matrix, filterable, linked from every adaptive rendition's
caveat section, with the Status/Mode legend spelled out. Last, because by then it
is a rendering job over data CI has been enforcing for four phases.

---

## The C boundary annex

C stays prominently supported and rigorously documented; it stops being forced
through an ergonomic SDK information architecture it does not have. The annex
foregrounds what C actually requires:

handle validity · ownership · buffers and lengths · alignment · error codes ·
thread safety · shutdown ordering · polling · ABI compatibility ·
compile-and-link examples

When a reader has C selected and lands on an adaptive operation, they get the
`/docs/<section>/<page>/c` route defined in D1: the universal body, the generated
support status, and a link to the nearest C boundary operation. No `c.md` fragment,
no fabricated ergonomic implementation.

**The remaining UI detail for Phase 3:** the selector keeps five pills, but the
fifth behaves differently from the other four. That asymmetry must be visible in
the chrome rather than surprising — a reader who picks C should understand they have
selected a boundary surface, not a fifth SDK lens.

---

## Risks, and what I would cut first

- **Composition is now load-bearing.** If the renderer cannot cleanly compose
  `_shared.md` + fragment with a correct TOC and anchor set, the cost model
  reverts to authored duplication. This is the first thing Phase 3's tracer page
  must prove, and it is why the tracer exists.
- **Writing three languages we do not write daily.** Mitigated by Phase 2, not by
  care. If Phase 2 slips, Phase 4 must not start.
- **The record is only as honest as its negative cells.** Anchor validation proves
  positive cells; nothing can prove a `not_exposed` that is really `supported`.
  Human review of negative cells stays required — and under D5 those cells now
  render reader-facing prose, which raises the cost of getting one wrong.
- **Internal links must carry the selected language.** With the bare route as a
  static router, the reader's selection is preserved by every in-app link — sidebar,
  prev/next, in-body cross-reference. One link that drops the segment sends a Python
  reader back to the router, or worse to a Rust rendition, and it will read as the
  selection having been forgotten. This is a Phase 3 test, not a code-review habit.
- **Phase 1A chrome vs Phase 3 chrome.** Split so the routing decision does not
  invalidate typography work.

---

## Open items

**Nothing blocking. Every decision is frozen.**

**Round one:** trailing language segment **yes**; bare-route-as-Rust **no**; phase
order **yes**, with D2–D5 frozen before Phase 2 and a tracer page in Phase 3; the
170 renditions **not committed**; C **not a fifth lens**.

**Round two:** the bare route is a **static neutral router**, no middleware and no
cookie; capability records in `docs/data/capabilities/`; the example manifest is
`docs/data/examples.yaml`; example **source stays in native SDK and build
locations**; reader-facing rationale is reviewed in the same change as the semantic
status it describes, with no owner registry and no CODEOWNERS.

**Round three:** **D8 — compose the existing SDK spine in place**, `28 → 7 universal
bodies + 28 fragments`, and **keep those pages classified `sdk_native`**. No new
guide pages describing the same journey. Composition is a storage mechanism;
`sdk_native` is an IA classification; they stay separate.

Execution can start: **Phase 1A** (reading surface, chrome only) and **Phase 2**
(records, harness, migration-aware checker) are signed and touch disjoint files.

### Two things worth watching, not blocking

- **The self-serve gate needs an operator who is not me and not Laz.** "No founder
  interpretation" and "no manual correction by the test author" cannot be satisfied
  by the person who wrote the docs. Whoever runs it should be named when Phase 4 is
  scheduled.
- **The 15-minute and 2–5-hour targets are hypotheses.** They stay out of public
  copy until a run meets them. If the first run lands far off, that is a finding
  about the docs, not a reason to restate the target.
