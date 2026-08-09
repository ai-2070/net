# SDK Packaging and Release Consistency Audit

**Date:** 2026-08-08  
**Repository:** `ai-2070/net`  
**Audited commit:** `d6d6225fc6e19da66477e900ef04ee59b71fb947`  
**Branch:** `sdk-bugs`  
**Scope:** Python wheel feature sets, Go module release selection and native build instructions, Python wrapper/core version constraints, and tracked generated artifacts. The already documented TypeScript `0.34.0` SDK versus `@net-mesh/core >=0.35.0` peer contradiction is acknowledged but not duplicated as a new finding.

## Executive summary

Release artifacts do not currently match the repository's lockstep and capability promises. Published Python wheel workflows omit advertised organization and Deck surfaces that CI enables explicitly, so CI can pass while release wheels lack headline APIs. The nested Go module has no `go/v0.34.0` tag and its README names a removed Cargo package and library, making the promised release impossible to select and the documented native build impossible to execute.

## 1. High — Published Python wheels omit advertised organization and Deck surfaces

The `net-python` default feature set omits `org` and `deck`:

- defaults: `net/crates/net/bindings/python/Cargo.toml:38-57`
- separate Deck feature: `net/crates/net/bindings/python/Cargo.toml:129-134`
- separate organization feature: `net/crates/net/bindings/python/Cargo.toml:198-200`

The release workflow adds only `redis,extension-module`; defaults remain active, but `org` and `deck` remain absent:

- Linux: `.github/workflows/release-python.yml:40-48`
- macOS: `.github/workflows/release-python.yml:83-91`
- Windows: `.github/workflows/release-python.yml:119-127`

Relevant Python exports are feature-gated:

- `net/crates/net/bindings/python/src/lib.rs:3702-3733,3970`

This contradicts:

- Python organization/subnet APIs marked supported: `docs/data/capabilities/event-bus.yaml:189-233`
- Python Deck marked supported: `docs/data/capabilities/event-bus.yaml:360-370`
- authority verbs claimed in five languages: `net/crates/net/docs/releases/RELEASE_v0.34_HOTEL_CALIFORNIA.md:49-67`
- SDK README saying PyPI wheels ship every feature enabled: `net/crates/net/sdk-py/README.md:45-46`

`cargo metadata` independently confirmed that the defaults exclude both features. CI masks the release discrepancy because its test wheel explicitly enables them:

- `.github/workflows/ci.yml:1505`

### Required closure

Define one canonical release feature set and use it for release builds, test wheels, capability generation, and import-surface tests. Build the actual wheel command from `release-python.yml`, install it into a clean environment, and assert every advertised symbol before publishing.

## 2. High — Go cannot be selected at the promised lockstep `v0.34.0`

The Go SDK is a nested module:

- module path: `go/go.mod:1-3`

Nested-module semantic versions require tags such as `go/v0.34.0`. No such tag exists. Repository tags include the other `0.34.0` package releases, but no Go release tag.

The public versioning contract says Go tracks the same version:

- `web/src/content/docs/reference/versioning.md:21-27`

The release procedure has no Go tag or Go artifact step:

- `net/crates/net/docs/releases/RELEASE_STEPS.md:15-42`

Independent resolution failed:

```text
go list -m github.com/ai-2070/net/go@v0.34.0
go: github.com/ai-2070/net/go@v0.34.0: invalid version: unknown revision go/v0.34.0
```

### Required closure

Add an atomic Go release step that creates and verifies `go/vX.Y.Z`, then run `go list -m` and a clean consumer build against the public module proxy before declaring lockstep release complete.

## 3. Medium — Current Go build instructions are unexecutable and name the wrong library

`go/README.md:52-69` tells users to:

- build Cargo package `net-go-ffi`;
- link `libnet_go`;
- expect `libnet_go.so`, `libnet_go.dylib`, or `net_go.dll`;
- use Go 1.21 or newer.

Current implementation uses:

- Cargo package `net-ffi`: `net/crates/net/bindings/go/net-ffi/Cargo.toml:1-3`
- library name `net`: `net/crates/net/bindings/go/net-ffi/Cargo.toml:12-42`
- Go linker flag `-lnet`: `go/net.go:21-23`
- Go toolchain requirement 1.26: `go/go.mod:3`

The documented build fails:

```text
cargo build --release -p net-go-ffi
error: package ID specification `net-go-ffi` did not match any packages
help: a package with a similar name exists: `net-ffi`
```

### Required closure

Update package name, output files, linker instructions, workspace directory, and Go version. Add a clean-room build job that follows the README commands verbatim on Linux, macOS, and Windows.

## 4. Medium — Python SDK permits unsupported cross-minor native-core skew

The ergonomic SDK declares:

- `net-mesh-sdk==0.34.0`
- dependency `net-mesh>=0.34.0` with no upper bound

at `net/crates/net/sdk-py/pyproject.toml:9-15`.

Public versioning guidance says packages should match, mixing versions is unsupported, and pre-1.0 minor versions may break APIs:

- `web/src/content/docs/reference/versioning.md:21-27,31-42`

Pip can therefore resolve or retain `net-mesh-sdk 0.34.0` with `net-mesh 0.35+`, even though that configuration is explicitly unsupported. Failures occur only when the wrapper reaches a changed or missing native method.

### Required closure

Use a compatible-release or explicit minor upper bound, ideally generated from the SDK version. Add resolver tests for matching, older, next-minor, and prerelease core versions.

## 5. Low — Tracked generated Python build mirror has drifted from canonical source

The tracked mirror:

- `net/crates/net/sdk-py/build/lib/net_sdk/mesh.py`

has a functional 55-line difference from:

- `net/crates/net/sdk-py/src/net_sdk/mesh.py`

The mirror lacks newer subnet constructor arguments and the `serve_subnet_exported` facade around the corresponding constructor section.

Setuptools currently packages from `src`:

- `net/crates/net/sdk-py/pyproject.toml:37-38`

Therefore the stale mirror does not enter the wheel today. It remains a misleading committed artifact with no synchronization check and could re-enter packaging through a future configuration change.

### Required closure

Delete the tracked build mirror or regenerate and enforce byte/semantic parity in CI. Build outputs should not be treated as source.

## Known companion release gate

The TypeScript package manifests at this commit declare both SDK and core as `0.34.0`, while the SDK requires core `>=0.35.0`. That gate is already documented in the channel audit and was intentionally not counted again here. It should still be closed in the same coordinated release operation.

The nRPC audit separately documents a stale public C header whose ABI and function signatures disagree with the implementation; it is not duplicated here.

## Verification

Executed against the audited commit:

- `cargo metadata --no-deps` confirmed Python defaults omit `org` and `deck`.
- `go list -m github.com/ai-2070/net/go@v0.34.0` failed with unknown revision `go/v0.34.0`.
- Local tags contained the other `0.34.0` releases but no `go/v0.34.0`.
- `cargo check -p net-ffi`: passed.
- Python static stub coverage in the audit lane: **2 passed**.
- TypeScript SDK and core `npm pack --dry-run` both produced package inventories with declared entry files.

### Limitations

- A complete isolated Python wheel build was not performed because the active environment lacked the Python `build` package. Feature resolution was independently derived from Cargo metadata and the exact release command.
- Go runtime tests were unavailable with `CGO_ENABLED=0`; packaging findings do not depend on their runtime success.
- Local TypeScript checking used a feature-incomplete ignored native declaration artifact; fresh release CI regenerates it, so those local symbol errors were not classified as additional packaging findings.

## Conclusion

Release validation must inspect the artifacts users actually install, not richer CI feature builds or in-tree linked dependencies. Python needs one canonical wheel feature set; Go needs an actual nested-module tag and executable native build path; and wrapper/core compatibility must be enforced by package metadata rather than prose.
