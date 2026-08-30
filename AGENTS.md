# AGENTS.md

Guidance for AI agents working in this repository. Only non-obvious, hard-won knowledge is recorded here — read the code for the rest.

## What this repo is

**Net** (`net-mesh`): a latency-first encrypted mesh network protocol ("Network Event Transport"). Every node is an equal peer on a flat topology; the mesh propagates state, not connections. Flagship use case: capability federation (discovery, typed RPC, durable logs, artifact transfer across a trusted mesh).

The repository is **polyglot with one Rust core**:

- `net/crates/net/` — the Rust workspace root (the actual implementation). **All cargo commands run from here.** The root crate publishes as `net-mesh` on crates.io but its lib name stays `net` so internal `use net::...` paths work.
- `net/crates/net/sdk/` — `net-mesh-sdk`, the Rust SDK (imports as `net_sdk`).
- `net/crates/net/sdk-ts/` — TypeScript SDK (pure-TS mirror, tested with vitest).
- `net/crates/net/sdk-py/` — Python SDK (`net_sdk` package, pytest).
- `net/crates/net/bindings/` — FFI shims: `go/*-ffi` (7 rlibs + `net-ffi` cdylib), `node/` (napi), `python/` (pyo3/maturin).
- `net/crates/net/cli/` — `net-cli` binary (no lib target). `net/crates/net/deck/` — `net-deck` TUI. `net/crates/net/adapters/mcp/` — MCP bridge. `net/crates/net/payments/` — x402 payments. `net/crates/net/aggregator-daemon/`.
- `go/` — Go bindings (cgo against a single Rust cdylib). Module `github.com/ai-2070/net/go`, Go 1.26.
- `web/` — Next.js docs/marketing site (docs content lives in `web/src/content/docs/`).
- `docs/` — design docs (`docs/internal/` plans/audits, `docs/misc/`). Crate-level design docs in `net/crates/net/docs/` (BEHAVIOR.md, SENSING.md, ORGANIZATIONS.md, TRANSPORT.md, etc.).

## Essential commands

All Rust commands run from `net/crates/net/` (the workspace root).

```bash
cargo test --lib --features "$UNIT_FEATURES"   # unit surface — see the feature-flag trap below
cargo test --doc --features "$UNIT_FEATURES"   # doctests are a SEPARATE target; --lib does not run them

go test ./...          # from go/ — cgo must actually be enabled (see Bindings)
npm run check          # from web/ — docs links + releases sync + types
```

### Pre-push checklist (Rust)

`cargo check` plus a `--lib` clippy is **not** a green branch — several CI lints only fire on targets those commands never build (that exact gap has let three different CI failures through in a single day: a cfg'd test that never compiled, a lint that fires only on the lib-test target, and a rustdoc link error). Before calling a branch ready:

```bash
cargo fmt --all -- --check
cargo check --workspace --all-targets

# Production code, strict (matches CI):
cargo clippy --all-features --lib --bins -- -D warnings
cargo clippy --lib --bins -- -D warnings
cargo clippy --no-default-features --lib --bins -- -D warnings

# All targets — the -A flags are CI's own; the test surface is allowed to panic:
cargo clippy --all-features --all-targets -- \
  -D warnings \
  -A clippy::unwrap_used -A clippy::expect_used \
  -A clippy::undocumented_unsafe_blocks \
  -A clippy::multiple_unsafe_ops_per_block

RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features
```

That checklist compiles the workspace several times over with different feature sets, which is the workload [sccache](https://github.com/mozilla/sccache) is for — `export RUSTC_WRAPPER=sccache` **and** `export CARGO_INCREMENTAL=0` together (sccache does not cache incremental invocations, so setting only the first silently bypasses the cache). Opt-in per developer, from the shell only: a committed `rustc-wrapper` in `.cargo/config.toml` hard-fails the build for anyone without the binary. Full trade-offs in CONTRIBUTING.md.

CI also lints workspace members individually (`net-payments`, `sdk`, the ffi crates, `bindings/node`, `bindings/python`) with their own feature lists — run `cargo clippy -p <crate>` / `cargo doc -p <crate> --no-deps` for any member you touched, with the feature list from ci.yml. Doc comments on **public** items may not intra-doc-link private items (`rustdoc::private_intra_doc_links` is denied).

### The feature-flag trap (critical)

CI's unit-test job pins `UNIT_FEATURES` in `.github/workflows/ci.yml`:
`net redex redex-disk cortex netdb meshdb meshos dataforts nat-traversal port-mapping tool batched-ingress cli regex` (see the job for the current exact list).

- In-source `#[cfg(test)]` units behind features (`meshos`, `port-mapping`, `nat-traversal`, `meshdb`, `dataforts`, `cli`, …) **do not exist** unless those features are on. A bare `cargo test --lib` silently skips them.
- Deliberately excluded from that set: `redis`, `jetstream` (need running service containers) and `fixtures` (test/bench only; a `fixtures` gate would compile some witnesses to a silent 0-test no-op).
- When running a focused test, always pass the same broad feature set or you're testing nothing.

### nextest for focused runs

For one named test or one module, use nextest, not `cargo test`:

```bash
cargo nextest run --lib --features "$UNIT_FEATURES" --no-tests=fail --retries 0 \
  -E 'test(=adapter::net::mesh::org_routing_wiring_tests::the_exact_test)'
```

Why these exact flags (documented in CONTRIBUTING.md, learned from real CI incidents):

- `--no-tests=fail`: `cargo test -- <filter>` exits **0** when the filter matches nothing. A typo'd/renamed module is indistinguishable from a pass — this has already produced a green no-op CI job once.
- `--retries 0`: `.config/nextest.toml` grants 2 retries by default (absorbs transport-saturation flake noise in multi-node loopback-UDP suites). For mutation loops you want the first verdict, not best-of-three.
- `slow-timeout`/`terminate-after` in nextest.toml turn hung tests into named failures instead of eating the job's whole timeout.
- Security-critical suites (org/admission/nrpc-hijack binaries, see the overrides in `.config/nextest.toml`) get `retries = 0` even in CI: their regression signal is intermittent by construction, and a genuine flake is itself a defect.
- For a batch of RED/GREEN mutations, reuse one detached worktree and one target directory — compile is the slow part, not test execution.

### Other Rust test surfaces

- Integration tests: `net/crates/net/tests/*.rs` — CI pins each by name (`cargo test --test <file>`); a guard job fails if a `tests/*.rs` file isn't pinned, so new integration tests must be added to CI. `net-cli` and `net-aggregator-daemon` use auto-discovery (`cargo test -p <crate>`) instead.
- `net-mesh-mcp` tests need `--features fixture` (spawns a conformance server via `CARGO_BIN_EXE`).
- Loom model-checking: `tests/loom_models.rs`, run with `RUSTFLAGS="--cfg loom"`.
- Fuzzing: `net/crates/net/fuzz/` (nightly workflow).
- Golden-vector cross-language conformance: `tests/cross_lang_*/` (JSON fixtures shared by Rust, Go, TS, Python tests). If you change wire formats/tool schemas, these fixtures and their per-language tests must stay in lockstep — see `header_parity_test.go` and the `cross_lang_*` suites.

## Bindings: building and testing

- **Go** (`go/`): `go test ./...`. cgo must actually be on: with `CGO_ENABLED=0` (or no C toolchain — on Windows use an msys64/mingw gcc) the toolchain **silently skips** the cgo files, and both tests and typechecking are meaningless. The binding links the `net-ffi` cdylib — build it first (see the single-cdylib rule below).
- **Node** (`net/crates/net/bindings/node/`): `npm i`, then `npx napi build --platform` with the explicit `--no-default-features --features ...` list from ci.yml, then `npm run build:ts` and `npm test`.
- **Python** (`net/crates/net/bindings/python/`): venv + `maturin develop --no-default-features --features <list from ci.yml>`. Needs a real CPython on PATH (on Windows a missing one surfaces as `STATUS_DLL_NOT_FOUND`).

## Clippy discipline

Production code is held at `-D warnings` with `unwrap_used`/`expect_used` denied in some crates (`net-mesh-mcp` lints only `--lib --bins` for this reason). The workspace root keeps panic-hygiene lints (`unwrap_used`, `expect_used`, `undocumented_unsafe_blocks`, `multiple_unsafe_ops_per_block`) at **warn**, not deny, because Cargo applies `[lints]` after CLI flags — a package-level deny can't be relaxed for tests, and there are ~5000 test unwraps. CI splits enforcement: strict on lib/bins, permissive on all-targets. Never "fix" this by blanket-allowing lints, and never "fix" test-target unwrap/expect warnings — the test surface is allowed to panic by design.

## Toolchain

- Rust pinned to **1.98.0** via `net/crates/net/rust-toolchain.toml` (components: clippy, rustfmt, llvm-tools-preview for cargo-llvm-cov).
- Go 1.26. Web: Node 24, TypeScript 6, Next.js (turbopack dev).
- `.cargo/config.toml` and `clippy.toml`/`rustfmt.toml` live in `net/crates/net/`.

## Architecture notes and gotchas

### Single cdylib rule (cgo)

The Go bindings link **one** cdylib: `bindings/go/net-ffi` builds `libnet`, aggregating the seven C-ABI rlib shims (`rpc-ffi`, `org-ffi`, `meshos-ffi`, `meshdb-ffi`, `compute-ffi`, `deck-ffi`, `mcp-ffi`). The root crate is `lib + staticlib`, **not** `cdylib` — two cdylibs both exporting `net::ffi` caused a real Go CI hang (link order unifies duplicate functions but not `static`s; `parking_lot_core`'s parked-thread registry was one). Don't add another cdylib exporting `net::ffi`.

Header files: `net/crates/net/include/*.h` (C ABI) mirrored as `go/net.h`, `go/net_cortex.h`. The Go side has `abi_stability_*_test.go` guarding the ABI contract — changing any `extern "C"` signature or `NET_*` constant touches Rust, headers, and Go in the same commit, and the FFI crates' rustdoc documents against it (CI builds rustdoc for all workspace members with `RUSTDOCFLAGS: -D warnings`).

### Routing-plane witnesses

`net/crates/net/src/adapter/net/org_routing_wiring_tests.rs` (included from `mesh.rs`) is a named CI gate: CI asserts a **minimum count** (currently 86) of tests matching `org_routing_wiring_tests` and pins specific test names that carry security properties. These tests are `#[cfg(test)]`, not `feature = "fixtures"`. If you remove/rename witnesses intentionally, lower MIN and update REQUIRED names in `ci.yml` in the same commit — the gate exists so coverage loss is loud, not vacuous.

### Test-filter silent-skip hazard

`cargo test -- <filter>` exiting 0 on zero matches has bitten this repo twice (a green no-op CI job; a renamed module silently dropped from the Windows job). Always use `--no-tests=fail` with nextest or verify match counts.

### Platform-gated code

Only one CI job runs on Windows (`windows-security-tests`). All `#[cfg(windows)]` assertions (DACL validation on the org authority directory, `warn_secret_permissions` as the 0600 substitute, case-insensitive-NTFS guards in `cli/tests/org_grant.rs`) are dead code on ubuntu. Security-relevant modules live both under `adapter::net::behavior::org` (a prefix) and one level up at `adapter::net::org_admission_gate` — both surfaces must be named in filters; prefix filters alone have silently excluded the latter.

The reverse holds on a Windows workstation: no local command compiles `#[cfg(unix)]` code. Widen the cfg gate temporarily to compile-check it, or lean on CI.

### MCP adapter boundary

`net-mesh-mcp` rides only on the public `net-mesh-sdk` surface; `adapters/mcp/tests/dependency_boundary.rs` fails if it reaches past the SDK.

## CI layout

- `.github/workflows/ci.yml` — the main suite (Rust unit/integration/FFI/CLI/MCP/Windows-security, fmt, doc, Go, Python, skill examples). Path-filtered: only `net/**`, `go/**`, skill example paths, and the checker scripts trigger it.
- `coverage.yml` (information-only), `nightly-fuzz.yml`, `panic-probe-witness.yml`, `natsim.yml` (network simulator), `web.yml`, `skills.yml`, plus many release workflows (crates.io, npm, PyPI, binaries).
- CI comments are unusually detailed and encode incident history — read them before changing job wiring; they explain why each pin exists.
- `ci.yml` is the source of truth for every feature list and command in this file; the lists drift — when in doubt, read the workflow, not this file.

## Conventions

- Branch from `master`, PRs against `master`. Every CI job must be green. CLA required before first merge (bot comments on the PR).
- Dual license MIT OR Apache-2.0; new files should carry both license headers where existing files do.
- Published names vs. source names differ: crates.io `net-mesh*` / npm `@net-mesh/*` / PyPI `net-mesh-sdk`, but imports are `net_sdk` (Python), `@net-mesh/sdk` (TS), lib name `net` (Rust).
- Release notes live in `net/crates/net/docs/releases/` with Cyberpunk-themed codenames and are **mirrored** to `web/src/content/docs/releases/` — sync with `npm run sync:releases` from `web/` (`npm run check:releases` gates CI). `docs/releases/RELEASE_STEPS.md` describes the process.
- Security issues: report privately to makerseven7@gmail.com (see CONTRIBUTING.md), never via public issue.

## Where to look

- Design docs: `net/crates/net/docs/*.md` (behavior, capabilities, channels, transport, subnets, storage/cortex, organizations, identity).
- Internal plans/audits: `docs/internal/` (plans, reviews, performance, audits).
- Skill examples that CI actually executes: `.claude/skills/net-event-bus/examples/` (guarded by ci.yml paths — editing them re-runs the jobs that hold the maturin wheel and libnet cdylib).
