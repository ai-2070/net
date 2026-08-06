# Changelog

Notable changes to `@net-mesh/sdk`.

This file records what a TypeScript or JavaScript consumer has to do
differently. The full per-release story for the whole system lives in the
crate's release notes (`net/crates/net/docs/releases/`); this is the subset
that reaches this package's public surface.

`@net-mesh/sdk` and `@net-mesh/core` are released together and must be
upgraded together — the SDK is a thin typed layer over the native binding, so
a version skew shows up as a missing method at the call site rather than at
install time.

## Unreleased — targets 0.35.0

### Breaking

- **`CapabilityFilter.minVramMb` and `.minMemoryMb` are now `minVramGb` and
  `minMemoryGb`, in gigabytes.**

  Rename **and** rescale: `{ minVramMb: 16_384 }` becomes `{ minVramGb: 16 }`,
  not `{ minVramGb: 16_384 }`.

  The old names never reached the substrate. The native binding generates
  `minVramGb` / `minMemoryGb`, napi passes through the fields it knows and
  ignores the rest, and neither side treats an unknown key as an error — so
  both filters were dropped in transit and every threshold matched
  everything. A query written as "at least 16 GB of VRAM" returned nodes with
  none.

  That makes the upgrade a behaviour change in one direction only: queries
  using these axes were already returning **more** nodes than asked for, and
  will now return the correct, smaller set. Nothing that used to match will
  stop matching for any reason other than the filter finally applying.

  TypeScript callers get a compile error and can rename against it. **Plain
  JavaScript callers get neither an error nor a warning** — the old key is
  silently ignored exactly as it was before, so the filter stays inert
  instead of becoming correct. Grep for `minVramMb` and `minMemoryMb` before
  upgrading.

- **The `@net-mesh/core` peer range is now `>=0.35.0`.**

  `findBestNode` and `findBestNodeScoped` dispatch straight into the native
  binding, and 0.34.0 has neither symbol. Against that core they resolve
  cleanly at install and fail at the call site with
  `this.native.findBestNode is not a function`.

### Added

- **`MeshNode.findBestNode(requirement)` and `.findBestNodeScoped(requirement,
  scope)`** — single-winner capability discovery, previously Rust, Go and C
  only.

  Where `findNodes` returns every match and leaves the choice to you, these
  apply the requirement's weights and return one node:

  ```typescript
  const target = node.findBestNode({
    filter: { requireTags: ['gpu'] },
    preferMoreVram: 1,
  });
  if (target !== null) { /* … */ }
  ```

  `preferMoreMemory`, `preferMoreVram`, `preferFasterInference` and
  `preferLoadedModels` are each optional; an omitted weight means that axis is
  not consulted. Weights must be finite — `NaN` and `Infinity` throw, because
  neither has a meaningful clamp and a `NaN` weight would quietly select as
  though the axis were unweighted. Finite values outside `[0, 1]` are clamped
  by the substrate, so every binding shares one clamp.

  Ties, including the all-weights-omitted case, resolve to the lowest matching
  node id.

  **`null` means no match, and `0n` is a real node id.** Test `=== null`,
  never falsiness.

  Both are local and synchronous: they read this node's capability fold and
  send nothing, so they only see peers whose announcements have already
  arrived.
