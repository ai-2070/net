# Contributing to Net

Thanks for your interest in Net. This document covers the contribution
agreement and the basics of getting a change merged.

## Contributor License Agreement

Net requires every contributor to sign a CLA before their first contribution is
merged. It records a license to the Project so that Net can be distributed under
its dual [MIT](LICENSE-MIT) / [Apache-2.0](LICENSE-APACHE) license without
ambiguity about provenance.

**Your Contributions ship under the same dual license.** Unless you explicitly
state otherwise, any contribution intentionally submitted for inclusion in Net
is licensed as MIT OR Apache-2.0, at the recipient's option. The CLA records
that grant; it does not place different terms on your code.

**You keep full copyright ownership of your Contributions.** The CLA is a
license grant, not an assignment — you may reuse your own work anywhere else,
for any purpose.

**Released code stays under the license it shipped under, permanently.** Both
grants are irrevocable: every published version remains available under those
terms, and anyone may fork from the last released commit at any time. A change
of license affecting future development could not reach back and withdraw what
has already shipped.

| You are… | Sign |
| --- | --- |
| An individual contributing on your own behalf | [Individual CLA](CLA/individual-cla.md) |
| Contributing work your employer owns the copyright in | [Corporate CLA](CLA/corporate-cla.md) — and your employer must execute it |

### Signing the Individual CLA

Open your pull request as usual. A bot will comment if you have not signed yet.
Reply on the PR with exactly:

```
I have read the CLA Document and I hereby sign the CLA
```

Your signature is recorded once and covers all of your future contributions.

### Signing the Corporate CLA

The Corporate CLA is executed out of band. Email a completed and signed copy,
including Schedule A listing your authorized employees, to the address in the
document.

## Pull requests

- Branch from `master` and open the PR against `master`.
- Keep the change focused; unrelated cleanups are easier to review separately.
- CI runs the Rust, Node, Python, and Go suites plus clippy and rustfmt. Every
  job must be green before merge.

Useful local checks before pushing (run from `net/crates/net/`):

```bash
cargo fmt --all -- --check
cargo clippy --all-targets
cargo test --lib
```

Go bindings live in `go/` (`go test ./...`), the web docs in `web/`.

### Faster local builds (optional: sccache)

The pre-push checklist rebuilds the workspace several times over — three clippy
runs with different feature sets, a rustdoc pass, then the tests — and a
RED/GREEN mutation loop across detached worktrees pays for a cold target dir
every time. [sccache](https://github.com/mozilla/sccache) caches the compiler
objects themselves, so those repeats hit cache instead of rustc. It is entirely
optional and per-developer:

```bash
cargo install sccache

export RUSTC_WRAPPER=sccache
export CARGO_INCREMENTAL=0

sccache --show-stats   # hit/miss counters; `sccache --zero-stats` to reset
```

Two things worth knowing before you turn it on:

- **`CARGO_INCREMENTAL=0` is mandatory, not optional.** sccache does not cache
  incremental compiler invocations, and the dev profile enables them by
  default — leave incremental on and the workspace crates you actually care
  about silently bypass the cache. CI pairs the two for the same reason (see
  the sccache jobs in `.github/workflows/ci.yml`).
- **It is a real trade, not a free win.** Turning incremental off makes the
  narrow edit-one-file-rebuild-one-crate loop *slower*. sccache wins where this
  repo actually spends its time: switching between feature sets, switching
  branches, and fresh worktrees — all of which throw incremental state away
  anyway.

Deliberately NOT wired into `net/crates/net/.cargo/config.toml`: cargo invokes
the wrapper before it compiles anything, so a committed `rustc-wrapper` setting
is a hard build failure (`could not execute process 'sccache rustc -vV'`) for
anyone without the binary installed. Keep it in your shell.

### Focused runs, and why they should use nextest

Use `cargo nextest run` rather than `cargo test` whenever you are running one
named test or one module — a RED/GREEN mutation loop, a bisect, a "does this
witness still fail without the fix" check.

```bash
# One named test.
cargo nextest run --lib --features "$UNIT_FEATURES" --no-tests=fail --retries 0 \
  -E 'test(=adapter::net::mesh::org_routing_wiring_tests::the_exact_test)'

# A whole module.
cargo nextest run --lib --features "$UNIT_FEATURES" --no-tests=fail --retries 0 \
  -E 'test(/^adapter::net::mesh::org_routing_wiring_tests::/)'
```

`$UNIT_FEATURES` is the feature list the `unit-tests` job pins in
`.github/workflows/ci.yml`; a narrower set silently compiles feature-gated
modules to nothing.

Why these flags, specifically:

- **`--no-tests=fail`.** `cargo test -- <filter>` exits **0** when the filter
  matches nothing, so a typo or a renamed module is indistinguishable from a
  pass. This has already turned a real CI job into a green no-op once. An empty
  filterset must be an error.
- **`--retries 0`.** `.config/nextest.toml` grants two retries by default to
  absorb transport-saturation noise in the multi-node suites. That is exactly
  wrong for a mutation loop: you want the first attempt's verdict, not a
  best-of-three.
- **Process isolation and the `terminate-after` timeout** come along for free,
  so a mutation that hangs a test fails by name instead of stalling.

For a batch of mutations, reuse ONE detached worktree and ONE target directory
across the whole batch. Building a fresh target dir per mutation is where a
RED/GREEN loop actually goes slow — not in the test execution.

## Reporting security issues

Please do not open a public issue for a security vulnerability. Report it
privately to makerseven7@gmail.com.
