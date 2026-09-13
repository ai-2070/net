# Running Net's tests

How to run this repository's tests without wasting time, and why the commands
look the way they do. Exact timings, counts and the measurements behind every
claim here live in
[`docs/internal/misc/PERF_AUDIT_2026_09_13_TEST_EXECUTION.md`](docs/internal/misc/PERF_AUDIT_2026_09_13_TEST_EXECUTION.md).

All Rust commands run from `net/crates/net/` (the workspace root).

## The one thing that matters: stay on one feature graph

Cargo fingerprints per feature set. Every distinct `--features` list you use
is a separate from-scratch build of the root crate — which is a large crate —
and that dwarfs the cost of running the tests themselves. Switching *back* to
a set you built earlier is cheap, because the old fingerprint survives; the
price is paid once per new set, and a day of copying different `--features`
lines out of `ci.yml` pays it over and over.

So use the aliases in `net/crates/net/.cargo/config.toml`, which pin one graph
for the integration surface and one for the lib units:

```bash
cargo t  --test three_node_integration   # integration tests, one fingerprint
cargo t  -E 'binary(subnet_grant)'
cargo tl org_routing_wiring_tests        # in-source unit tests
```

`cargo t`'s feature set is the union of every graph the CI integration
families use, so one local fingerprint covers all of them. It is deliberately
**wider** than any single CI job:

- code that only compiles with a feature OFF is not exercised — that is what
  the `narrow-feature-check` CI job is for;
- `webrtc` is excluded, because it is the only feature that needs a C
  toolchain (see CONTRIBUTING.md).

`cargo tl` mirrors the `unit-tests` job's feature list. A narrower list
silently compiles feature-gated modules to nothing, so do not trim it to "just
what I'm working on".

## Use nextest, and make an empty filter an error

`cargo test -- <filter>` exits **0** when the filter matches nothing, so a
typo, a renamed module or a feature-gated-away test is indistinguishable from
a pass. That has already produced a green no-op CI job in this repository.
Both aliases pass `--no-tests=fail`; keep it if you write the command out by
hand:

```bash
cargo nextest run --lib --features "$UNIT_FEATURES" --no-tests=fail --retries 0 \
  -E 'test(=adapter::net::mesh::org_routing_wiring_tests::the_exact_test)'
```

`--retries 0` matters for mutation loops: `.config/nextest.toml` grants
retries by default to absorb transport-saturation noise in the multi-node
loopback-UDP suites, and best-of-three is the wrong answer when you are asking
"does this still fail without the fix". Security- and ordering-critical suites
are already pinned to zero retries there, because for them a flake *is* the
defect.

Process isolation and the `terminate-after` timeout come along for free, so a
change that hangs a test fails by name instead of stalling the run.

## What is slow, and what is not

- **Test execution is not the bottleneck.** nextest runs tests in parallel
  processes; even the sleep-heaviest suite finishes quickly in wall-clock
  terms.
- **Compilation is.** In order: building a feature set for the first time,
  then recompiling the crate after a lib edit, then relinking test binaries.
- **Relinking is why the dev profile ships `debug = "line-tables-only"`.**
  This workspace links a very large number of integration binaries against one
  crate, and full debug info dominated both link time and disk. Panics still
  resolve to file:line; if you need variable inspection, re-enable it for the
  one target you are stepping through (see CONTRIBUTING.md).
- Two things that sound promising and are not, both measured: optimising
  dependencies with `[profile.dev.package."*"]`, and swapping in a faster
  linker. Neither pays for itself. Don't re-litigate them without new numbers.

## Working in a loop (humans, and agents especially)

The expensive habit is not "running too many tests" — a warm run of a whole
integration family, or of the entire in-source unit suite, is quick. It is
re-entering the *compile* side over and over: a new feature graph, a fresh
target directory, or a sequence of differently-flagged invocations where one
would have done. Work the cheap end of the ladder and escalate only when the
step below has something to say:

1. **`cargo check --lib`** after a source edit. It answers "does this still
   compile" for the price of metadata, no linking, and it is what catches the
   majority of in-progress mistakes.
2. **The narrowest test selection that covers the change** —
   `cargo tf --test <binary>` or `cargo tf -E 'binary(a) + binary(b)'`, or
   `cargo tfl <module path>` for in-source units. `tf`/`tfl` stop at the first
   failure and print only failures, which is what you want while iterating.
3. **One full family or the full unit suite, once, at the end** — `cargo t` /
   `cargo tl`, which run everything and report everything.

Rules that keep the ladder cheap:

- **Never paste a `--features` list out of `ci.yml` into a local run.** That
  is how a session acquires a fifth feature graph and pays a full root-crate
  rebuild for it. The two aliases exist precisely so a session owns two
  fingerprints, both warm.
- **One invocation per checkpoint, not four.** Each `cargo nextest run` walks
  the build graph; four sequential narrow runs cost more than one run naming
  four binaries, and if they disagree on features they cost a rebuild each.
- **Do not re-run a suite to confirm it passed.** The run you already have is
  the evidence. Re-running to be sure is the single most common way an
  edit-verify loop doubles its own cost.
- **Keep the target directory.** `cargo clean` throws away every fingerprint
  in it; almost nothing is worth that. Keep disk headroom instead.

### Choosing what to run

Pick by surface, not by intuition: the `integration-*` jobs in `ci.yml` group
the integration binaries by the area they exercise, so the family that pins
your file's tests is the set worth running. `codegraph affected <file>` helps
only for genuine leaf modules — for anything central (the mesh, the
capability layer) it correctly answers "almost everything", which is true and
useless as a filter.

## Batches, worktrees and disk

For a batch of RED/GREEN mutations, reuse one detached worktree and one target
directory across the whole batch — building a fresh target dir per mutation is
where such a loop actually goes slow.

Watch disk. A target directory holding several feature graphs' worth of test
binaries gets very large, and running out of space surfaces as a confusing
`rustc-LLVM ERROR: IO failure on output stream` rather than a clear error.
`cargo clean` between long-lived feature-set experiments is cheaper than
debugging that.

## Results are machine-readable

Every nextest run writes a JUnit result set to
`target/nextest/default/junit.xml` (`[profile.default.junit]` in
`.config/nextest.toml`). CI's named-witness gates read that artifact via
`.github/scripts/check-witness-results.py` rather than re-running each pinned
test by name: one run, then a per-name verdict that a witness ran, passed, and
did not need a retry to do so.

If you add, rename or retire a pinned witness, update the roster in the CI
step that pins it in the same commit. The checker fails on a name that did not
run — that is the point of it.

## The other suites

```bash
cargo test --doc --features "$UNIT_FEATURES"   # doctests are a separate target
cd go            && go test ./...              # cgo must really be enabled
cd sdk-ts        && npm test
cd bindings/python && pytest
```

Feature lists, the pinned integration-test names and the per-job commands are
defined in `.github/workflows/ci.yml`; that file is the source of truth when
this one drifts.
