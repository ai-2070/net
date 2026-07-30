# Polyglot Lens — Phase 4, the self-serve acceptance gate

**Written:** 2026-07-30. **Branch:** `polygot-docs`. **Status: NOT RUN — blocked
on an operator.** **Plan:** `docs/internal/plans/DOCS_POLYGLOT_LENS_PLAN.md`,
Phase 4.

This is Part C of the Phase 4 handoff, as a protocol somebody can execute rather
than as a description of one. Parts A and B are landed; this one is not, and the
reason is structural rather than scheduling.

---

## Why this is not run, and cannot be run by the people who could

The gate asks: **can an engineer or coding agent with no private context and no
access to Laz produce a working, conventional Net integration from public
artifacts alone?**

Its forbidden list is what makes it meaningful:

```
forbidden:   private implementation notes
             founder interpretation
             unpublished examples
             manual correction by the test author
```

- **Laz cannot run it.** He wrote the system. Every "obviously it's the native
  handle" is founder interpretation, and the gate exists to measure what a
  stranger does *without* that.
- **The author of the docs cannot run it.** Phase 4's fragments were written from
  the binding sources — `mesh.ts`, `tool.py`, `tool.go`, `mesh_rpc.rs`. Whoever
  did that is now carrying the private context the gate is designed to exclude,
  and "manual correction by the test author" is the exact prohibition. A run by
  the docs author measures the author, not the docs.

A rehearsal by either would produce a number, and the number would be wrong in the
flattering direction. **A gate that cannot fail is not a gate.** So this document
specifies the run and stops.

**What is needed to unblock it: one named third party**, briefed with nothing but
the "Inputs" section below.

---

## Operator profile

The result is only interpretable if the operator is representative. Required:

- Competent in the chosen language; **no prior exposure to Net** — not the docs,
  not the skills, not the repo, not a conversation about it.
- No channel to Laz or to the docs author for the duration. Questions get written
  down as dead ends, not asked.
- Willing to report a bad number. The gate's value is entirely in the runs that
  fail.

A coding agent is an acceptable operator and arguably the better one, since the
"invented or incorrect API attempts" metric is precisely the failure mode the
skills exist to prevent. If an agent is used, record the model and the harness —
the number is not portable across either.

**Not acceptable:** anyone who has read this document, the plan, or the Phase 4
measurement. They name the traps.

---

## Inputs — the complete brief

Everything below is what the operator gets. Nothing else.

```
inputs:      one supported language          (their choice: rust | typescript | python | go)
             one existing public package, or a representative industrial SDK
             one typed read-only operation
             the public docs      https://<docs host>/docs
             the public skill     npx skills add … --skill net-event-bus
             the public packages  crates.io / npm / PyPI / go get
             the public source    the repository
```

**The task, stated to the operator verbatim:**

> Wrap one read-only operation from an existing package you already know as a Net
> capability. Another process must be able to discover it, invoke it with a typed
> request, and get a typed response. Prove that an unauthorized caller is refused
> and that a deadline behaves. Use only the public docs, the public skill, the
> published packages, and the source.

The operation must be **read-only** and **typed**. Read-only so a retry during
debugging cannot corrupt anything; typed so the request/response path is actually
exercised rather than a string echo.

---

## Required output — a real integration, not a demo

All ten. A run missing any of them did not pass, however good the timings look.

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

"Customer-owned adapter source" is the load-bearing one. Calling a Net example is
not the test; wrapping *their own* package in a shape they would ship is.

---

## Metrics — reported whether or not they flatter the docs

| Metric | How to record it | Why it is here |
|---|---|---|
| Time to first successful call | Wall clock, from `npm install` / `cargo add` to a response printed | The fifteen-minute claim |
| Time to conventional minimal integration | Wall clock to all ten outputs | The real adoption number |
| Documentation dead ends | One line each: page URL, what it said, what was true | Each one is a defect with an address |
| Source inspections needed | One line each: file opened, what the docs did not say | A high count means the docs abstracted over a detail that mattered |
| Invented / incorrect API attempts | One line each: what was tried, where the belief came from | The failure mode the skills exist to prevent |
| Human interventions | Count, with what was said | **Any at all means the gate did not pass** |

**Targets, under test and not yet published:** ~15 minutes to a demonstration,
~2–5 hours to a conventional minimal integration. These are hypotheses. They stay
out of public copy until a run meets them.

### Recording rules

- **Timestamp as you go.** Reconstructed timings are optimistic; nobody remembers
  the twenty minutes spent on the wrong package name.
- **A dead end counts even if you recovered.** The recovery is the cost.
- **Attribute every invented API.** "I assumed it by symmetry with the Rust page"
  is the most valuable sentence the operator can write — it is the exact defect
  Phase 4 found nine of.
- **An intervention is any information from a human**, including "try the other
  package name". Log it and keep going; the run is already a fail, and the rest of
  the data is still worth having.

---

## What a run is likely to hit, and why that is not a spoiler for the operator

For whoever *reads* the results — not for the operator, who must not see this.

Phase 4 corrected nine defects of one shape: the tool API's object model. It is
reasonable to expect the residue of that shape to be what a run trips on, because
the docs now state it and have never been read cold by anyone who did not already
know it. Three specific things to watch for in the report:

1. **Does the operator find the RPC handle?** TS and Python both require reaching
   `_native` on the ergonomic node. The fragments say so explicitly. If a run
   still fails here, saying it is not enough and the binding needs the accessor.
2. **Does the operator announce the tool?** Serving and announcing are separate in
   three of four bindings. The universal body leads with this. If it is still
   missed, the sequencing needs to be structural, not editorial.
3. **Does the denied-invocation witness work at all?** It is the least exercised
   path in the corpus and the one with the most riding on it commercially.

If a run produces zero dead ends, be suspicious before being pleased: check the
operator was really cold.

---

## Scheduling

| Blocker | Resolves when |
|---|---|
| No named operator | Someone meeting the profile above is identified and briefed with the Inputs section only |
| Docs not published | The run needs the *public* artifacts; `polygot-docs` must be merged and deployed first |

The second is the reason not to rush the first. Running the gate against a branch
preview measures a thing no customer can reach.

**Phase 4 acceptance is therefore complete except for this item**, and this item is
explicitly open rather than quietly assumed. The plan's own framing is the right
one to hold: the measurements in Part B prove the publishing system works; they do
not prove the commercial objective, and Phase 4 could pass all of them while
producing an elegant multilingual architecture that still does not deliver a
fifteen-minute first integration.

That question is still open. This document is what closing it requires.
