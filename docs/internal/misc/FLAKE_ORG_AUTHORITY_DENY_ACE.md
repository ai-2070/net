# Windows test flakes and unexplained failures (OPEN, undiagnosed)

**Status: OPEN, reproduced, NOT diagnosed.** No owner, no scheduled work. Split
out of the OLB-2B.3c-pre step-3 corrective record because none of it is related
to that slice and it was distorting the slice's bookkeeping (Kyra, 2026-07-31).

**Two** unit tests have now been observed failing intermittently under full-suite
parallel load on Windows. Both are filesystem-adjacent; neither touches any OLB
code path.

```
adapter::net::behavior::org_authority::tests::a_deny_ace_does_not_make_an_owner_only_dir_invalid
adapter::net::redex::disk::tests::append_failure_after_dat_write_rolls_back_dat
```

The second was found on 2026-07-31 while hunting the first — 1 failure in 6
full-load runs. That it surfaced immediately once someone looked suggests the
population of load-sensitive filesystem tests may be larger than these two, which
is the more useful thing to know than either individual failure.

## Evidence, stated as two separate populations

The two must not be pooled — they are different experiments, and combining them
understates the failure rate:

| Population | Runs | Failures |
|---|---|---|
| Full-suite, parallel load | 15 | **2** |
| The `org_authority` suite alone, isolated | 10 | 0 |

Dates: one failure 2026-07-29, one 2026-07-30. All observations on the same
Windows machine.

An earlier note in the step-3 handoff said "did not reproduce" (accurate about
the first 15 runs available at that time) and then "2 in ~25 full-load runs"
(wrong — that pooled the isolated runs into the full-load denominator). Both are
superseded by the table above.

## What is NOT known

- **No failure message has been captured.** Both failures occurred in runs whose
  output was not retained. Without one there is no diagnosis, only a shape.
- Whether the failure is in the ACL logic, the `icacls` subprocess, or the
  temporary directory is **unknown**.

## Hypothesis, explicitly not a diagnosis

Contention on the external `icacls` process or on the temp directory under
parallel load, rather than anything in the authority logic. The test's own
subject — that a deny ACE does not invalidate an owner-only directory — has no
obvious concurrency surface. **This is a guess and should not be treated as
narrowing the search.**

## Suggested first step

Capture a failure. Run the full suite in a loop with per-run output retained
until it reproduces, then read the actual assertion and any `icacls` stderr. Until
then there is nothing to fix.

## A third observation: one unidentified wiring-gate failure

**Status: OBSERVED ONCE, NOT REPRODUCED, NOT DIAGNOSED.** Recorded here because
it previously existed only in a commit message, which is not a place anyone
looks for open questions (Kyra, review of `010c718ea`).

During OLB-2B.3c-pre step 3, a single routing-plane wiring witness failed inside
a shell command that ran `cargo fmt` alongside the test run. **The failing test
name was not captured**, so there is nothing to attribute it to.

| | |
|---|---|
| Occurrences | 1 |
| Reproduction attempts since | 40 wiring-only runs, 6 full-load runs |
| Reproduced | no |
| Failing test name | **not captured** |

The leading hypothesis is a rebuild race — `cargo fmt` rewriting sources while
the test binary was being built — which would make it an artifact of that one
command rather than a property of any witness. **That is a guess.** Without the
test name it cannot be confirmed or ruled out, and "did not reproduce in 46
runs" is weaker evidence than it looks for a race that needs a concurrent
formatter to trigger.

If it recurs, capture the test name before anything else.

## Scope

None of this touches any OLB slice. It appears in OLB records only because that
is when it was observed.
