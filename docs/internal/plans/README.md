# Plans: the house style

A plan is written **before** the work and **amended as it lands**. It records
what we meant to build, why, and in what order, so a reviewer (or a later
contributor) can check the change against its intent. Every pull request that
adds a feature, a subsystem, a protocol or wire change, a new public API, or
anything else substantial commits its plan here, in the same pull request. Bug
fixes, refactors, dependency updates and documentation changes do not need one.

## Naming

One file per feature track, `UPPER_SNAKE_CASE`, ending in what the document is:

| Suffix | For |
|---|---|
| `_PLAN.md` | An implementation plan. The default. |
| `_DESIGN.md` | A design decision worked out before a plan, when the design is the hard part. |
| `_GATE.md` | The exit criteria a phase must meet before it counts as done. |
| `_PLAN_V2.md`, `_PLAN_V3.md` | A successor plan when the first is finished or abandoned. Leave the old one in place. |

Code reviews go in [`../reviews/`](../reviews/), audits in
[`../audits/`](../audits/) or [`../misc/`](../misc/), benchmarks in
[`../performance/`](../performance/), and throwaway experiments in
[`../spikes/`](../spikes/). This folder is for plans.

## What a plan contains

Use these sections, in this order. Keep each one as short as the work allows.

1. **Status.** One line: planned, in progress, or done; the release it targets;
   the date. Update it as the work moves. It is the first thing a reader checks.
2. **The gap.** What is missing or wrong today, and why it matters. Verify it
   against the code and name where you looked (file paths, the command you ran).
   A gap that was assumed rather than checked is the most common way a plan
   builds the wrong thing.
3. **The design.** The approach, and the alternatives you considered with why
   you rejected them. If there is a decision someone else must make, state it
   as a decision with a recommendation, not as an open question buried in prose.
4. **The slices.** The order the work lands in. Each slice says what it
   delivers and **what proves it**: the test, witness or measurement that would
   fail if the slice were wrong. Prefer slices that each leave the tree working.
5. **Risks.** What could go wrong, and the fallback for each.
6. **Not in scope.** What this plan deliberately does not do, so nobody reads
   its silence as a promise.

## Keeping it true

- **Amend, don't rewrite.** When a slice lands, mark it done in place, with the
  date and what was measured or witnessed. When the design changes, say what
  changed and why, next to the original. The reasoning is often more useful
  later than the outcome.
- **Name the evidence.** "Tested" is not evidence; name the test. A number is
  only evidence with the command that produced it.
- **Record defects found on the way**, and whether they were fixed or deferred.
- **Mark the end.** When the track is finished or abandoned, set the status and
  say so. Do not delete the plan.

## Examples of the expected depth

- [`FETCH_DIR_ATOMIC_PLAN.md`](FETCH_DIR_ATOMIC_PLAN.md): a small, focused plan
  with a gap verified against the code.
- [`ANCHOR_PREBUILT_BINARIES_PLAN.md`](ANCHOR_PREBUILT_BINARIES_PLAN.md): a
  decision with options and a recommendation, slices with exit criteria, risks
  and scope.
- [`BROWSER_LOBBIES_AND_LARGE_WORLDS_PLAN.md`](BROWSER_LOBBIES_AND_LARGE_WORLDS_PLAN.md):
  a large plan amended as it landed, with slices marked done and the defects
  found along the way.

## Writing a plan with an AI agent

Ask the agent to read this file first, then write the plan as a new file here
**before** implementing: *"Read `docs/internal/plans/README.md`, write the
implementation plan as a new file there, then implement it."* Review the plan
before the code. Check that the gap was verified against the code rather than
inferred, and that each slice names what proves it.
