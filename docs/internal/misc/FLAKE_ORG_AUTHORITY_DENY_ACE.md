# Flake — `org_authority::tests::a_deny_ace_does_not_make_an_owner_only_dir_invalid`

**Status: OPEN, reproduced, NOT diagnosed.** No owner, no scheduled work. Split
out of the OLB-2B.3c-pre step-3 corrective record because it is unrelated to that
slice and was distorting its bookkeeping (Kyra, 2026-07-31).

## What is observed

A Windows-only unit test fails intermittently under full-suite parallel load. It
shells out to `icacls` against a PID-scoped temporary directory.

```
adapter::net::behavior::org_authority::tests::a_deny_ace_does_not_make_an_owner_only_dir_invalid
```

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

## Scope

Touches no OLB slice. It appears in OLB records only because that is when it was
observed.
