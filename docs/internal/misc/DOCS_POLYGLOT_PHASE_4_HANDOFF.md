# Polyglot Lens — Phase 4 Handoff

**Written:** 2026-07-30. **Branch:** `polygot-docs`, HEAD `7b83c6c0e`.
**Plan:** `docs/internal/plans/DOCS_POLYGLOT_LENS_PLAN.md` (Phases 1A, 1B, 2 and 3
executed; see its *Execution log*).

This is the brief for whoever picks up Phase 4. It assumes you have not been in the
previous work, and it states the two things that block a clean start.

---

## What Phase 4 is

Take the eight-step critical path polyglot, measure what that actually cost, and
prove a stranger can integrate Net from public artifacts alone. The third part is
the one that matters commercially and the one the previous author cannot do.

```
install → define a capability → provide it → resolve it elsewhere
        → invoke → watch → move an artifact
        → prove denial, deadline and failure behaviour
```

---

## The blocking constraint, first

**The self-serve gate forbids "founder interpretation" and "manual correction by
the test author."** The docs were written by Claude; the system was written by Laz.
Neither can run the gate. Parts A and B below proceed without an operator; Part C
needs a named third party before it is scheduled.

---

## Substrate you are inheriting (all green at HEAD)

| Thing | Where | What it guarantees |
|---|---|---|
| Capability records | `docs/data/capabilities/*.yaml` | 145 cells, closed vocabulary, 119/119 anchors resolve, skill copies byte-match |
| Example index | `docs/data/examples.yaml` | every binding accounted for; evidence level declared and enforced |
| Tier manifest | `docs/data/tiers.yaml` | 149/149 pages, exhaustive and disjoint, allowlist ratchet at 28 |
| Composition | `web/src/lib/docs.ts` | `_shared.md` + `<lens>.md` → one route per lens, `/c` generated |
| Language invariant | `generateStaticParams` | the build fails if prev/next crosses a language boundary |

Two adaptive pages exist as worked examples: `start/install` (four fragments) and
`reference/redis-dedup` (four fragments, with its C content relocated to the annex).

```bash
# everything, before you start
.github/scripts/check-docs-tiers.py && .github/scripts/check-docs-tiers.py --self-test
.github/scripts/capability_records.py --check && .github/scripts/capability_records.py --self-test
python3 .github/scripts/skill_examples.py --validate && python3 .github/scripts/skill_examples.py --self-test
.github/scripts/check-skills.sh && .github/scripts/check-docs.sh
cd web && npm run check && npm run build
```

---

## Part A — the composition work, and a gap in D8

D8 says: compose the existing SDK spine in place, keep it `sdk_native`, **no URL
churn**, because `/docs/sdk/python/announce` already carries the language segment.

**That last clause does not hold against what was built.** The resolver recognises
`<page-dir>/<lens>` — a directory with `_shared.md` and fragments named `rust.md`,
`typescript.md`, `python.md`, `go.md`. The spine's URLs are the other order:
`<lens>/<page>`.

So composing the spine means one of:

| Option | Cost |
|---|---|
| **(a) Accept the URL shape** — `sdk/announce/{_shared,rust,…}.md` → `/docs/sdk/announce/python` | 28 URLs move. Needs redirects; `web/vercel.json` has `rewrites` only today, no `redirects` block |
| **(b) Add a second resolution shape** — lens-prefix routes mapping `/docs/sdk/<lens>/<page>` onto `sdk/<page>/<lens>.md` | No URLs move. A second routing rule in `resolveAdaptive`, and file layout stops matching URL layout |

Recommendation: **(a)**. One URL shape across the corpus is worth 28 redirects, and
the inconsistency in (b) will confuse every future contributor. But this is a real
decision that changes Phase 4's scope, and D8 was written assuming it was free.
**Settle it before writing any fragments.**

Scope either way: 7 pages × 4 lenses → **7 universal bodies + 28 fragments**,
replacing 28 authored pages. No C-annex work is needed here — `sdk/c` already *is*
the annex.

---

## Part B — the publishing measurement

Report **per page, not averaged**. Two data points exist and they diverge hard:

| Page | Universal | Fragments | Authored | 4 copies would be | Universal share |
|---|---|---|---|---|---|
| `start/install` | 51 | 223 | 274 | ~504 | **19%** |
| `reference/redis-dedup` | 55 | 56 | 111 | ~284 | **50%** |

Composition saves roughly `(lenses − 1) × universal`. Installing is almost entirely
a per-language act, so 19% is close to the floor — a spine sample should land
higher. Also record:

- authoring effort per fragment;
- **annex work required** (see traps);
- binding gaps the record did not predict;
- maintenance burden: what one API change costs across four fragments.

---

## Part C — the self-serve acceptance gate

**The question:** can an engineer or coding agent with no private context and no
access to Laz produce a working, conventional Net integration from public artifacts
alone?

```
inputs:      one supported language
             one existing public package or representative industrial SDK
             one typed read-only operation
             public docs, public skill, public packages, public source

forbidden:   private implementation notes
             founder interpretation
             unpublished examples
             manual correction by the test author
```

**Required output** — a real integration, not a demo:

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

**Metrics, reported whether or not they flatter the docs:**

| Metric | Why it is here |
|---|---|
| time to first successful call | the fifteen-minute claim |
| time to conventional minimal integration | the real adoption number |
| documentation dead ends | each one is a defect with an address |
| source inspections needed | a high count means the docs abstracted over a detail that mattered |
| invented / incorrect API attempts | the failure mode the skills exist to prevent |
| human interventions | **any at all means the gate did not pass** |

Targets are **hypotheses under test**: ~15 minutes to a demonstration, ~2–5 hours to
a minimal integration. They stay out of public copy until a run meets them.

---

## Traps already found — do not rediscover these

**1. A page with real C content costs more than its line count.** C gets no authored
fragment by design. `reference/redis-dedup` had 22 live lines of C — allocation,
`never returns NULL`, return codes, `free` — with nowhere to go; converting would
have deleted it. It went to `sdk/c/memory-and-threading`, because a handle you
allocate and free is an ownership question, not a capability question. **Check for C
content before estimating any conversion.**

**2. Inbound `#fragment` links break silently when a section moves into a
fragment.** Three pages linked to `/docs/start/install#feature-flags` and
`check-doc-links.mjs` passed all three — the router route registered as an empty
folder index, and the fragment check skips empty anchor sets. The checker now models
adaptive pages, but expect the *pattern*: grep for `#anchor` links into any page you
convert.

**3. The docs contain confidently wrong code.** In `start/install.md` alone: a
version pin for an unpublished release, `EventBus` as the Node and Python entry
point when the export is `Net`, and three C symbols (`net_bus_new`,
`net_bus_ingest`, `net_bus_shutdown`) that exist in no header. **Verify every
snippet against source or an executed example. Do not carry prose forward.**

**4. Fence count tells you how a page is written, not whose surface it describes.**
Two reference pages were misfiled by that proxy — `redis-dedup` looked Rust-native
and covers all five bindings.

---

## Converting a page — the mechanical checklist

```bash
# 1. a directory replaces the file
mkdir web/src/content/docs/<section>/<page>
#    _shared.md   frontmatter: title, description, optional boundary/boundaryLabel
#    rust.md typescript.md python.md go.md    omit a lens with nothing to teach

# 2. docs/data/tiers.yaml
#    state -> adaptive, remove from the allowlist, lower `max`, fix the counts comment

# 3. verify
.github/scripts/check-docs-tiers.py
cd web && npm run check:docs && npm run build
```

A lens with no fragment still gets a route and renders an honest absence state.
**Never fall back to another language's code** — that is the one thing the doctrine
forbids outright.

---

## Acceptance

- 7 universal bodies and up to 28 fragments build;
- every critical-path block enforced at its declared evidence level;
- every parity badge rendered from the record, none typed by hand;
- the five failure witnesses demonstrated in each lens that exposes them;
- pending allowlist unchanged at 28 — this phase touches the spine, not the pending
  tier;
- **both write-ups landed**: the publishing measurement and the self-serve run.

---

## Open decisions before starting

1. **The D8 URL question** in Part A. Blocks Part A.
2. **Who runs the gate.** Blocks Part C.
3. **The `boundary` frontmatter seam** (`web/src/lib/docs.ts`) is a placeholder for
   D5's `alternative.href`. Replacing it needs a generated, equality-checked JSON
   bridge so the site can read the capability records — the site has no YAML parser.
   Worth doing in Phase 4 if the spine's `/c` routes need specific annex targets.
