# Prebuilt anchor binaries (0.38)

## Status

**Shape B shipped for two targets in 0.38 (2026-09-29).** Written 2026-09-27
while preparing 0.37, when an anchor was `cargo install net-cli --features
rtc-bootstrap`.

- **What shipped:** `release-binaries-cli.yml` gained a `build-anchor` job. It
  builds `net-mesh-anchor-v<version>-x86_64-unknown-linux-gnu.tar.gz` on
  ubuntu-22.04 (glibc 2.35) and `…-x86_64-pc-windows-msvc.zip` on
  windows-latest, with `--features rtc-bootstrap`. Both are attached to the
  same `v<version>` release as the CLI archives, in the same run.
- **Scope:** decided by the maintainer as these two targets only. Both were
  proven to build and pass the smoke test in CI before the release
  (throwaway branch `anchor-build-0.38`).
- **Smoke test:** `--version`, and that `anchor serve --help` lists this
  release's flags.
- **Still open:** slice 2's loopback serve and browser-acceptance runs
  against the released binary; the other targets (musl via zig, ARM,
  macOS); the container image (C); and slice 4's docs sweep beyond the CLI
  reference, the release note and the `net-event-bus` skill.

## The gap

`@net-mesh/browser` ships on npm in 0.37, and every page needs a native
**anchor** to reach the mesh: `net-mesh anchor serve` (plus `--issuer-identity`
and `--game` to issue player credentials). That verb only exists in a CLI built
with the `rtc-bootstrap` feature, and **no prebuilt CLI we publish has it**.
Every channel builds the default feature set:

| Channel | Workflow | Build | Targets |
|---|---|---|---|
| GitHub release archives | `release-binaries-cli.yml` | `cargo build --release -p net-cli` (musl: `cargo zigbuild`) | x86_64/aarch64 linux gnu + musl, x86_64/aarch64 macOS, x86_64/aarch64 Windows MSVC |
| npm (`net-mesh` CLI) | `release-npm-cli.yml` | same, per-platform npm packages | same eight |
| PyPI (CLI wheel) | `release-pypi-cli.yml` | maturin `bindings = "bin"` | linux x86_64/aarch64 (gnu + musl), macOS, Windows x64/arm64 |
| crates.io | `release-net-cli.yml` | source publish | builds anywhere `cargo install` does |

So in 0.37 an operator who wants to host browser players needs a Rust
toolchain, `cmake` and a C compiler (below), and roughly a full release build of
the workspace. That is a steep first step for the audience `@net-mesh/browser`
is for: game developers who have never installed Rust.

## Why it is not simply "turn the feature on"

`rtc-bootstrap` = `net` + `webrtc` + the HTTPS listener stack. Verified with
`cargo tree -p net-cli --features rtc-bootstrap -i aws-lc-sys`:

- **A C build enters through WebRTC, not through HTTPS.** `str0m` → `dimpl` (the
  DTLS stack) → `aws-lc-rs` → **`aws-lc-sys`**, which needs `cmake` and a C
  toolchain at build time. The HTTPS side (`axum`, `rustls`, `tokio-rustls`,
  `instant-acme`, `hyper-rustls`) is pinned to the `ring` provider on purpose.
  `sdk/Cargo.toml` notes that the workspace avoids `aws-lc-rs` everywhere
  *except* `webrtc` itself.
- **Cross builds are the risk.** musl targets go through `cargo-zigbuild`.
  `aws-lc-sys` under zig as the C compiler is not something any job here has
  built. Windows on ARM (`windows-11-arm` runner) and both macOS targets have
  likewise never built `webrtc`.
- **What CI proves today.** `rtc-bootstrap` / `webrtc` builds run only on
  ubuntu-latest (x86_64 gnu): `rust-sdk-tests`, `cli-tests` (a full
  `cargo test -p net-cli --features rtc-bootstrap`), `webrtc-feature`,
  `browser-acceptance`, `acme-cold-start`. A developer workstation builds the
  browser harness (native `str0m`) on Windows x86_64, so that target is known to
  compile, but no CI job checks it.
- **Size and attack surface.** Every CLI user would carry a WebRTC stack, an HTTP
  server and an ACME client they do not use. The CLI crate deliberately keeps
  `rtc-bootstrap` "off by default: the standard CLI stays free of an HTTP
  server". That reasoning still holds for the CLI most people install.

## Decision to make first

Pick one of three shapes. The recommendation is **B**, with **C** as its
companion.

- **A. Turn `rtc-bootstrap` on in every shipped CLI.** One artifact, nothing new
  to name. It costs everyone the size and the extra surface, and every channel
  and target must be made to build `aws-lc-sys`. It contradicts the CLI's own
  "no HTTP server by default" stance.
- **B. A separate anchor build of the same CLI**, e.g. archives named
  `net-mesh-anchor-<version>-<target>`, built with
  `--features rtc-bootstrap`, from `release-binaries-cli.yml` beside the
  existing archives. The binary is the same `net-mesh` with the anchor verbs;
  only the build differs. Ship it where anchors actually run, not to every
  channel. **Recommended.**
- **C. A container image** (`ghcr.io/ai-2070/net-mesh-anchor:<version>`,
  linux/amd64 + linux/arm64) wrapping the B build. Anchors are servers (public
  HTTPS, a UDP port, a TLS certificate), and a container is how most hosts will
  run one.

npm and PyPI stay default-feature only. Their users are developers wiring the
CLI into toolchains, not operators standing up a public server. If demand shows
up, an `npx`-able anchor can come later as its own package.

## Plan (for B + C)

### Slice 1 — prove the targets build (no publishing)

1. Add a CI job `anchor-build-matrix` (release-only or nightly; it pays for C
   builds). It runs `cargo build --release -p net-cli --features rtc-bootstrap`
   on the targets we mean to ship, in order of value:
   - `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`: the server
     targets, and the most likely to work.
   - `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl` through
     `cargo-zigbuild`: the static binaries a container wants.
   - `x86_64-pc-windows-msvc`, `aarch64-apple-darwin`, `x86_64-apple-darwin`:
     for developers running a local anchor.
   - `aarch64-pc-windows-msvc`: last, and only if it is cheap.
2. Record each target's result, the binary size against the default build, and
   the build time. Any target that needs more than `cmake` plus the runner's
   default toolchain gets a written reason, or is dropped from the ship list.
3. **Exit:** a table of target → builds / binary size / wall time, and a
   decided ship list. Expect musl-via-zig to be the one that needs work (for
   example, setting `AWS_LC_SYS_CMAKE_BUILDER` or `CC`/`CXX` for zig, or building
   musl inside an Alpine container instead).

### Slice 2 — prove the binaries work

1. A smoke test per shipped archive, run on the matching runner. Extract it,
   then run `net-mesh anchor serve --help`, and start an anchor with a
   self-signed certificate on loopback. `GET /rtc/anchor` must answer, and
   `POST /credential {game}` must issue for a configured `--game`.
2. On the linux x86_64 archive, run the existing `browser-acceptance` scenario
   (`examples/anchor-acceptance/run.mjs --net-mesh <extracted binary>`). The
   ten named checks, pinned in `ci.yml` today, must pass against the **released
   binary**, not a debug build of the workspace.
3. **Exit:** smoke green on every shipped target, and acceptance green on linux
   x86_64.

### Slice 3 — publish

1. `release-binaries-cli.yml` builds the anchor variant beside the existing
   archives, with the same checksums and signing and the same release. Name the
   archives so the two cannot be confused. The binary inside stays `net-mesh`,
   so docs and scripts do not fork.
2. A container workflow builds `net-mesh-anchor` from the musl (or glibc) linux
   archives on a minimal base: non-root, `ENTRYPOINT ["net-mesh", "anchor",
   "serve"]`. Publish to GHCR with the release tag.
3. `net-mesh --version` (or `anchor serve --version`) states whether the anchor
   feature is built in. A default build asked for `anchor serve` already fails
   typed; its error should name the anchor archive and image as the fix.

### Slice 4 — docs

- Replace `cargo install net-cli --features rtc-bootstrap` as the primary path
  in:
  - the release notes of 0.38;
  - `web/src/content/docs/reference/cli.md`;
  - `concepts/webrtc-transport.md`;
  - `sdk/browser/quickstart.md`, `README.md` and `session.md`;
  - `start/install/*`;
  - `net/crates/net/browser-ts/README.md`;
  - the `net-browser` and `net-event-bus/browser.md` skills.
- Keep `cargo install … --features rtc-bootstrap` as the from-source
  alternative.
- A short "run an anchor" guide: container first, archive second. It covers TLS
  (ACME versus bring-your-own certificate), the UDP/STUN port that must be
  reachable, `--allow-origin`, and `--issuer-identity` with `--game`.

## Risks

- **`aws-lc-sys` on musl via zig.** If it cannot be made to build, fall back to
  glibc-only linux archives plus an Alpine-built static binary for the
  container. Do not ship a musl anchor that was never smoke-tested.
- **Two archives per target confuse people.** Mitigated by the naming, a
  version string that states the feature, and a typed error in the default
  build that points at the anchor build.
- **Release time grows.** The C builds add minutes per target. Build the anchor
  variant in parallel jobs, and only for the ship list from slice 1.
- **Version skew.** The package and its anchor must be the same release (the
  lossy channel, `--game`). Build anchor archives from the same tag, in the
  same workflow run as the default CLI, never from a separate trigger.

## Not in scope

- A shared, hosted multi-game anchor service. That is the browser plan's
  release step 4, a separate project.
- TURN. The browser plan defers it to P5.
- Changing what the default CLI contains.
