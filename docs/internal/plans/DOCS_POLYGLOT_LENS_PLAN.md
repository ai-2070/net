# Documentation — The Polyglot Lens

**Status:** DRAFT (2026-07-29). Nothing executed. This plan turns Kyra's
36,000-foot concept into a decision list, a phase order, and the CI gates each
phase has to pass. It supersedes nothing: `DOCS_STRATEGY_PLAN.md` (positioning,
worldview, agent briefs) and `DOCS_SDK_SPINE_PLAN.md` (the five-language SDK
spine) both landed and stay true. This plan is the layer above them.

**Governing principle (Kyra's, quoted, unmodified):**

> Turn Net's docs from documentation of a Rust system with bindings into
> documentation of a multilingual protocol and runtime.

Scope: `web/src/content/docs/` (149 pages) and the docs machinery in
`web/src/lib/docs.ts`, `web/src/docs.order.ts`, `web/src/components/Docs*.tsx`,
`web/src/store/useLanguageStore.ts`.

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
in steps 4 and 5: guides, tutorials, payments. Reordering the phases around that
fact is the single biggest change this plan makes to her arc.

**The docs already know they are Rust-first.** `guides/event-bus.md` opens with:

> **A note on languages.** The detailed sections below are in Rust, because the
> core crate is Rust and the tuning knobs … are exposed most directly there.

That paragraph is the whole problem stated as an apology. It is honest, and it
is what we are removing.

**Two Rust-native pages are filed as language-neutral reference.**
`reference/eventbus-api.md` (8 Rust fences, 0 others) and
`reference/adapter-trait.md` (8 Rust fences, 0 others) sit in `reference/`, a
section a TypeScript reader is told is theirs. They are Rust SDK reference.

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

---

## Doctrine (non-negotiable)

1. **The reader's language is a lens over one model, not five products.** Every
   adaptive page answers the same five questions in the same order (below). If a
   rendition wanders off that spine, it is a different page and CI should say so.
2. **No fake parity, no silent fallback.** A surface that does not exist in a
   binding is disclosed with its real status and the nearest honest path. Falling
   back to a Rust snippet on a page a Python reader selected is the specific
   failure this plan exists to prevent.
3. **One parity record.** The status of `Go × A2A` is written down in exactly one
   place and rendered everywhere. Two hand-maintained matrices diverge; we
   already maintain one in `.claude/skills/*/bindings/coverage.md`.
4. **No snippet ships unexecuted on the critical path.** See the arithmetic
   below — this is the precondition for going polyglot at all, not a nicety.
5. **Every new checker gets a planted-defect test.** Same discipline as
   `check-skills-depth.sh`: a check nobody has watched fail is not known to work.
6. **Concepts stay conceptual.** A concept page may name a wire field, a state, or
   a guarantee. It may not require the reader to know what `Arc` means.

### The adaptive-page spine (Kyra's, made checkable)

Every language-adaptive page, in every rendition, in this order:

```
What are we accomplishing?      (universal — identical across renditions)
What guarantees matter?         (universal — identical across renditions)
What is the sequence?           (universal — identical across renditions)
How does this look in <lang>?   (per-language)
What is peculiar about <lang>?  (per-language — the runtime caveat)
How do I verify it worked?      (per-language — a command and its expected output)
```

The first three being *identical text* across renditions is what makes this one
Net instead of five manuals, and it is mechanically checkable (hash the shared
block). The last three being *different* is the whole point.

---

## The information model, applied to the real 149 pages

| Tier | Sections | Pages | Renditions |
|---|---|---|---|
| **Universal** | `worldview` (5), `concepts` (12), `releases` (35), protocol reference (`wire-format`, `subprotocol-ids`, `versioning`, `capability-schema`, `error-codes`, `filter-dsl`, `glossary`, `mcp-bridge`), `start/what-is-net` | ~62 | 1 each |
| **Language-adaptive** | `start/install`, `start/quickstart`, `guides` (22), `tutorials` (4), `agent-briefs` (4), payments *doing* pages (`spend-policy-and-approvals`, `non-custodial-signing`, `billing`, `failure-schematic`) | ~34 | 5 each |
| **Language-native** | `sdk/{rust,typescript,python,go,c}` (37) | 37 | already per-language |
| **Reclassify** | `reference/eventbus-api`, `reference/adapter-trait`, `reference/replication-config`, `reference/redis-dedup` — Rust-native content in a neutral section | 4 | move under `sdk/rust/` or make adaptive |
| **Universal, payments** | `what-net-payments-is`, `x402-and-net`, `the-lifecycle`, `verification-tiers`, `networks` | 5 | 1 each |

**The honest cost: ~34 adaptive pages × 5 = ~170 renditions**, against ~40
language-specific pages today. That is the number to react to before agreeing to
anything. Phase 4 deliberately proves the model on 8 pages (40 renditions) before
we commit to the remaining 130.

**Why "no snippet ships unexecuted" is a precondition, not a nicety:** 170
renditions at the current density of `guides/` (≈4.5 fences per page) is **~750
code blocks**, four fifths of them in languages nobody on the team writes daily.
We have already watched what unverified examples do — `hello.rs` and `hello.ts`
type-checked clean for months while hanging forever, and `observe.rs` used a
buffer capacity below the enforced 1024 minimum. Those were **five** files under
active review. Seven hundred will be worse. The snippet harness (Phase 2) has to
exist before the writing starts, or the polyglot docs will be confidently wrong
in four languages instead of vaguely thin in four languages.

---

## Decisions required before Phase 3

These are the forks where a wrong turn is expensive to reverse. Recommendation
first, reasoning after.

### D1 — URL shape for adaptive pages

**Recommend: a trailing language segment, `/docs/guides/event-bus/python`**, with
the bare `/docs/guides/event-bus` remaining a real SSG page that renders the
default rendition.

The alternative — one URL, all five renditions in the HTML, four hidden by CSS —
is tempting because it changes no routing. It fails on a specific, checkable
problem: **renditions differ in prose, not just code.** Five copies of
`## Verify it worked` in one document means `rehype-slug` emits
`verify-it-worked`, `-1`, `-2`, `-3`, `-4`; `extractToc` (`lib/docs.ts:554`,
which parses raw markdown) shows all five in the TOC rail; and every cross-page
`#fragment` link becomes a coin flip that `check-doc-links.mjs` cannot validate.
Add ~5× page weight and copy-paste from hidden blocks. It is the wrong trade.

**Do not auto-redirect the bare URL** to the reader's stored language. A redirect
on first paint is a flash for every search-engine landing and every shared link,
and it makes the URL somebody pasted not be the page they saw. Instead: the bare
page renders the default and carries a non-blocking line —
*"You're reading the Rust rendition. Your selection is Python — switch."* Honest,
no flash, one click. Internal navigation (sidebar, prev/next, in-body links)
carries the reader's language, so a reader who chose Python essentially never
lands on a bare URL after the first one.

Universal pages keep their single URL. No language prefix on them — a prefix
would mint five duplicate URLs for one page and force a canonical-tag strategy to
undo the damage.

### D2 — How a rendition is stored

**Recommend: one file per (page, language), plus a shared block that CI proves
identical.**

```
guides/event-bus/
  _shared.md        # the three universal sections + frontmatter contract
  rust.md
  typescript.md
  python.md
  go.md
  c.md              # or: absent.md, with a declared reason (see D5)
```

Frontmatter contract on every rendition: `spine: event-bus`, `lang: python`,
`shared_hash: <sha of _shared.md>`, `parity: <ledger key>`. The hash is what
stops five renditions drifting into five manuals — Kyra's stated fear — and it
costs one line in a checker.

The alternative (one file with `<Lang for="python">` blocks) needs the content
tree to move from `.md` to `.mdx` — today `page.tsx:291` passes
`format={resolved.file.ext}` and every content file is `.md`, so JSX in content
is not parsed at all. It also puts five languages in one editor buffer, which is
how a Go paragraph acquires a Python caveat.

### D3 — Where snippets come from

**Recommend: transclusion from files that CI compiles and runs.** Inline fences
stay legal only when marked `illustrative` (wire dumps, pseudo-code, shell
transcripts) and CI counts them so the exemption cannot quietly become the norm.

We have the machinery: `.github/skill-examples.json` +
`.github/scripts/skill_examples.py` (validate/list/report/run-spec) +
`run-skill-examples.sh --lang <l>`, already executing all five bindings across
`skills.yml` and `ci.yml`. Extending that manifest to cover docs snippets is
strictly cheaper than building a second harness, and it means a snippet cannot be
right in the skills corpus and wrong in the docs.

### D4 — Where the parity ledger lives

**Recommend: promote the skill coverage matrices to one machine-readable ledger,
rendered into both surfaces.**

`.claude/skills/net-event-bus/bindings/coverage.md` (18 operations × 5 bindings)
and `.claude/skills/net-payments/bindings/coverage.md` (11 × 5) already carry the
exact vocabulary Kyra asks for — Status `supported · partial · experimental · not
exposed · n/a`, plus an orthogonal Mode `poll · verify-only · core-only` — with
`check-skill-coverage.py` verifying every positive cell names a resolvable symbol
anchor. Kyra's step 6 is **already built**; it is just not visible to readers.

Adding a second matrix under `web/` would give us two records of `Go × A2A` and,
within a quarter, two different answers. One ledger, two renderings.

Note the `core-only` mode matters enormously for docs: it is the difference
between "Python can do this" and "Python can do this **if you import from `net`
rather than `net_sdk`**." That distinction is the most common way to be wrong
about Net in Node and Python, and today no docs page says it.

### D5 — What an absent rendition looks like

**Recommend: an absence is a written page, not a missing file.** It states the
ledger status, why (`not exposed` vs `n/a` — buildable gap vs permanent
non-concept), what to do instead (another binding, the CLI, the core package),
and it links the tracking issue if one exists. `skill_examples.py` already
enforces this shape for examples (`absent` requires a reason); the docs checker
inherits it.

What we do **not** do: render the Rust rendition with a "C not available" banner.
That is the silent fallback the doctrine forbids.

### D6 — Version context

**Recommend: out of scope for this plan.** There is no versioned docs tree, no
version selector, and one live version. Building versioning *and* the language
lens at once doubles the routing surface. The chrome designed in Phase 1 should
leave a slot for it; nothing more.

---

## Phases

Kyra's arc is: read better → language as context → de-Rustify concepts →
polyglot critical path → expand → parity honest. Two changes, both from the
baseline measurements:

- **Her step 3 is nearly done** (5 Rust-idiom hits in `concepts/` + `worldview/`)
  and shrinks to a cleanup inside Phase 3.
- **Her step 6 is already built** in the skills corpus and moves *early*, because
  Phase 4 cannot write an honest rendition without it.

Phases 1 and 2 touch disjoint files (chrome vs CI scripts) and can run in
parallel.

### Phase 1 — Make the docs a better place to read *(visible, independent)*

Deliberately split from the orientation chrome, which depends on D1.

- Reading column, type scale, and hierarchy between section / page / subsection /
  reference.
- Code blocks: language label always visible (so a screenshot is unambiguous —
  Kyra's point), copy affordance, wrap behaviour that does not shred.
- **Diagrams off ASCII.** Defect 5 above. Pick one representation (inline SVG
  authored alongside the page, or a diagram component) and convert
  `concepts/architecture`, `capabilities`, `channels`, `subnets`,
  `storage-stack`, `agent-identity` first — those are the six pages with box
  drawings today.
- Visual distinction between concept / guide / tutorial / reference / agent brief.
- Mobile navigation.

**Acceptance:** `cd web && npm run build` green; `npm run check` green; the six
converted diagram pages legible at 375px; no content edits in this phase.

### Phase 2 — The ledger and the snippet harness *(invisible, unblocks Phase 4)*

- Promote the two coverage matrices to one ledger (D4). Both the skills corpus
  and the docs read from it. `check-skill-coverage.py` keeps working against the
  new source or is rewritten to.
- Extend the example manifest (D3) to docs snippets: every adaptive-page snippet
  is a tracked file, compiled in all five bindings, executed where a job holds
  the artifacts (Rust + TS in `skills.yml`; Python, Go, C in `ci.yml` — the
  existing split, for the existing reasons).
- New `web/scripts/check-docs-tiers.mjs`: every page declares its tier; adaptive
  pages have a rendition per language or a declared absence (D5); shared blocks
  hash-match across renditions (D2).
- Make `check-doc-links.mjs` language-aware: a link into an adaptive page must
  resolve to a rendition that exists.

**Acceptance:** each new checker has a planted-defect regression test that fails
without the fix and passes with it, in the shape of `check-skills-depth.sh`. No
checker lands without one.

### Phase 3 — Language becomes reader context

Depends on D1, D2.

- Routing for adaptive pages; bare URL renders default + the honest
  "your selection is X" line.
- **Fix the three live defects:** language-aware `getPrevNext` (a Python reader
  never falls into Go), language-aware sidebar/page agreement (no stranded
  pages), language-aware search ranking.
- Language + (slot for) version as persistent chrome, visible at every
  breakpoint — not inside the drawer.
- "View in another language" / "Compare implementations" as a secondary
  affordance on adaptive pages, per Kyra: the selected language is the default
  rendition, comparison is one click away and never five tabs fighting.
- Concepts cleanup: the five Rust-idiom hits, and the reclassification of the
  four Rust-native `reference/` pages (baseline table).

**Acceptance:** prev/next never crosses a language boundary for a non-default
reader (test, not inspection); every gated page is reachable from the nav of at
least one language; `npm run check` green.

### Phase 4 — The critical path, genuinely polyglot *(the proof)*

Eight pages, five renditions, forty documents:

```
install → create a node → announce → discover → invoke → watch
        → move an artifact → handle failure
```

Every rendition follows the six-question spine, carries its runtime caveat, ends
in a verification command with real expected output, and takes its code from the
Phase 2 harness. Where a step does not exist in a binding, it is an absence page
(D5) with its ledger status — and given the ledger, we already know some of these
will be: A2A is `not exposed` in Go and C; several surfaces are `core-only` in
Node and Python.

**Acceptance:** all forty renditions build; every snippet in them is executed by
CI in its own language; the parity badge on each rendition is rendered from the
ledger, not typed by hand.

### Phase 5 — Expand by use case, demand-ordered

Remaining ~26 adaptive pages: core guides, integrations, task lifecycle, storage,
payments, tutorials, deployment, troubleshooting. Ordered by traffic and by
support load, not alphabetically. Each page lands with all five renditions or a
declared absence — never a partial page.

**Acceptance:** unchanged gates. A section is not "done" until zero pages in it
carry an undeclared absence.

### Phase 6 — Parity made visible

The ledger, already true since Phase 2, becomes a reader-facing page: the full
matrix, filterable, linked from every adaptive page's caveat section, with the
Status/Mode legend spelled out. This is last because by then it is a rendering
job over data CI has been enforcing for four phases.

---

## Risks, and what I would cut first

- **The ~170-rendition number is the risk.** If Phase 4's forty renditions take
  materially longer than budgeted, cut Phase 5's scope to the sections with
  measured traffic and leave the rest single-language *with a visible, honest
  marker* — a page that says "Rust rendition only" is not a failure of this plan;
  a page that silently shows Rust to a Python reader is.
- **Writing four languages we don't write daily.** Mitigated by Phase 2, not by
  care. If Phase 2 slips, Phase 4 must not start.
- **The ledger is only as honest as its anchors.** `check-skill-coverage.py`
  deliberately does not infer absence — it validates positive cells and rejects
  symbol anchors on negative ones. A cell wrongly marked `not exposed` will not
  be caught by CI. Human review of negative cells stays required.
- **Phase 1 chrome vs Phase 3 chrome.** Split so the routing decision does not
  invalidate typography work. If D1 is settled early, they merge.

## Open questions for Kyra

1. **D1** — trailing language segment, and *no* auto-redirect on bare URLs? The
   alternative is a redirect that guarantees the reader's language everywhere at
   the cost of a first-paint flash on every shared link.
2. **Phase order** — I moved parity (her step 6) to Phase 2 because Phase 4
   cannot be honest without it, and shrank her step 3 because `concepts/` is
   already 98% clean. Agreed?
3. **The 170-rendition budget** — is the answer "yes, all of it, over N months",
   or "prove it on the critical path and then decide"? Phase 4 is written to
   support either, but the writing capacity question should be answered before
   Phase 5 is scheduled, not during.
4. **C.** `sdk/c` is an honest 2-page spine because the ABI is bus-only, and
   `guides/` has **zero** C fences today. Is C a first-class lens with many
   honest absences, or a documented boundary surface with its own shape? Kyra's
   text ("C should foreground ownership, handles, buffers, and teardown rather
   than pretending it has the same ergonomics") reads as the second, which would
   drop the adaptive tier from 5 renditions to 4 + a C boundary annex — a ~34-page
   reduction in scope.
