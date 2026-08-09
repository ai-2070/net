# Discovery and Capability SDK Consistency Audit

**Date:** 2026-08-08  
**Repository:** `ai-2070/net`  
**Audited commit:** `d6d6225fc6e19da66477e900ef04ee59b71fb947`  
**Branch:** `sdk-bugs`  
**Scope:** Capability conversion, announcement, expiry, discovery filters, placement weights, result reachability, and related documentation across Rust, Node/TypeScript, Python, Go, and C. Fixed historical best-node and scoped-filter findings are excluded.

## Executive summary

Recent best-node and scoped-discovery fixes are present, but capability conversion remains semantically inconsistent at several language boundaries. Unknown modality strings can become `Text`, disappear, or broaden a filter depending on the binding. Numeric fields are saturated or zeroed differently, Python's ergonomic SDK omits the entire capability/discovery lifecycle, and only Rust controls announcement TTL. Documentation also understates the implemented multi-hop behavior.

## 1. High — Unknown modalities have incompatible and unsafe meanings across bindings

Rust uses the closed `Modality` enum. String-based bindings disagree:

- Node maps every unknown value to `Modality::Text`: `net/crates/net/bindings/node/src/capabilities.rs:183-193`; the parser is used for announcements at `:346-348` and filters at `:461-463`.
- PyO3 also maps unknown values to `Text`: `net/crates/net/bindings/python/src/capabilities.rs:125-135`, used at `:269-271,400-402`.
- C warns and drops unknown values: `net/crates/net/src/ffi/mesh.rs:3291-3303,3425-3448`.
- Shipped Go routes through the C conversion.

The consequences depend on context:

- an announcement typo in Node/Python falsely advertises Text capability;
- an announcement typo in C/Go removes the modality;
- a filter typo in Node/Python selects Text nodes;
- a filter typo in C/Go removes the modality constraint and broadens matching to every otherwise eligible node;
- Rust cannot compile an unknown enum variant.

This is fail-open scheduling behavior in the C/Go filter path and false capability attribution in Node/Python.

### Required closure

Reject unknown modalities uniformly at every announcement and filter boundary. Do not reuse a lossy parser for filter construction. Add identical invalid-modality vectors across all bindings and prove no announcement or query is issued.

## 2. Medium — Python's ergonomic `MeshNode` omits capability lifecycle and discovery

`net_sdk.MeshNode` exposes none of:

- `announce_capabilities`;
- `find_nodes` / `find_nodes_scoped`;
- `find_best_node` / `find_best_node_scoped`.

Source:

- `net/crates/net/sdk-py/src/net_sdk/mesh.py:93-400`

Users must reach through private `node._native`. Public documentation acknowledges the workaround:

- `web/src/content/docs/sdk/announce/python.md:5-20`
- `web/src/content/docs/sdk/discover/python.md:5-17`

The coverage matrix marks Python as `core-only`, so this is not a hidden implementation bug, but it is a major ergonomic parity gap and forces application code onto an internal attribute.

### Required closure

Add typed forwarding methods and public result/config models to `net_sdk.MeshNode`, or expose and document the native handle as a supported stable boundary rather than `_native`.

## 3. Medium — C and Go truncate a public `u32` FP16 throughput field

The capability contract declares `fp16_tflops_x10` as `u32` in the higher-level bindings. C conversion saturates it to `u16::MAX`:

- `net/crates/net/src/ffi/mesh.rs:3204-3219`

Its focused regression pins:

```text
1,000,000,000 → 65,535
```

at `net/crates/net/src/ffi/mesh.rs:4578-4609`.

Node preserves full `u32`:

- `net/crates/net/bindings/node/src/capabilities.rs:255-263`

PyO3 also preserves full width:

- `net/crates/net/bindings/python/src/capabilities.rs:178-183`

Go advertises `uint32` but routes through the truncating C ABI:

- `go/capabilities.go:52`

A caller can submit a value allowed by its public type and have it silently changed only on C/Go.

### Required closure

Preserve `u32` end-to-end, or narrow every public type with explicit checked rejection. Saturation is unsuitable for a scheduling metric because it changes comparative ordering silently.

## 4. Medium — TypeScript silently converts invalid storage `BigInt` values to zero

TypeScript declares:

- `storageGb?: bigint`: `net/crates/net/sdk-ts/src/capabilities.ts:81-83`

NAPI maps negative or greater-than-`u64` values to zero without an error:

- conversion helper: `net/crates/net/bindings/node/src/capabilities.rs:207-217`
- use in hardware conversion: `net/crates/net/bindings/node/src/capabilities.rs:305-307`

Other boundaries reject or cannot represent those values:

- PyO3 integer extraction rejects outside `u64`;
- Go's type is `uint64`;
- C JSON deserialization rejects negative and overflowing input.

Zero is a valid storage value, so callers cannot distinguish invalid conversion from a genuine no-storage announcement.

### Required closure

Return a typed boundary error for signed or non-lossless `BigInt` values. Add `-1n`, `u64::MAX`, and `u64::MAX + 1` tests.

## 5. Medium — Capability expiry control is Rust-only and immediate withdrawal exists nowhere

Rust exposes caller-selected announcement TTL and signing:

- `announce_capabilities_with(caps, ttl, sign)`: `net/crates/net/sdk/src/mesh.rs:1054-1067`

Node/TypeScript, PyO3/Python, Go, and C expose only default-TTL announcement. No public language surface exposes `withdraw_capabilities`.

Replacement announcements can remove individual properties from the advertised set, but cannot immediately remove the node's index entry. Full removal depends on expiry or peer failure. Only Rust can request accelerated expiry.

### Required closure

Decide whether TTL and withdrawal are part of the public lifecycle contract. If so, expose both consistently and test replacement, explicit withdrawal, expiry timing, peer restart, and callback/index cleanup.

## 6. Low — Rust accepts non-finite placement weights while other boundaries reject them

Rust's public weight builders apply `weight.clamp(0.0, 1.0)` without checking finiteness:

- `net/crates/net/src/adapter/net/behavior/capability.rs:2984-3005`

`NaN` survives that clamp. Scoring guards such as `if weight > 0.0` then treat the axis as omitted:

- `net/crates/net/src/adapter/net/behavior/capability.rs:3021-3055`

A focused reproduction returned `is_nan=true` and `gt_zero=false`.

Node and PyO3 reject non-finite weights. Go/C JSON cannot represent them. Rust is therefore the permissive outlier.

### Required closure

Make Rust builders fallible or normalize through the same explicit finite-value validator used at foreign-function boundaries. Add NaN and positive/negative infinity tests beside finite clamp tests.

## 7. Low — Multi-hop capability documentation is stale across SDKs

Core forwards capability announcements up to:

- `MAX_CAPABILITY_HOPS = 16`: `net/crates/net/src/adapter/net/behavior/capability.rs:2478`

A three-node witness exists:

- `net/crates/net/tests/capability_multihop.rs:90`

Rust SDK documentation still describes one-hop/deferred behavior:

- `net/crates/net/sdk/src/capabilities.rs:28-60`
- `net/crates/net/sdk/src/mesh.rs:1038-1042`

TypeScript repeats it:

- `net/crates/net/sdk-ts/src/capabilities.ts:28-29`
- `net/crates/net/sdk-ts/src/mesh.ts:689-694`

Go and C comments also retain the stale one-hop model.

### Required closure

Update the cross-SDK mental model to state the hop cap, forwarding prerequisites, expiry behavior, signature policy, and the distinction between discovery propagation and transport connectivity.

## Verification

Executed or independently checked against the audited commit:

- focused core placement-scoring test passed;
- focused C modality and numeric-boundary regressions passed;
- all four Node weight-conversion tests passed, including non-finite rejection;
- recent best-node/scoped fixes were confirmed present across all five language surfaces, including zero-ID/no-match shapes and scope-before-score behavior.

### Limitations

- A focused npm run encountered an ignored stale local native artifact lacking `findBestNode`; this was not attributed to tracked source behavior.
- Python runtime tests were blocked by unavailable local packages/DLLs.
- Go runtime tests were blocked by `CGO_ENABLED=0`; source and Rust-side C conversion tests were used.
- Generic capability-change callbacks are absent across the basic SDKs; specialized tool-watch APIs were treated as a separate surface.
- Generated coverage checks symbol anchors and cannot detect lossy conversion differences.

## Conclusion

Capability and discovery parity requires shared boundary semantics, not only shared field names. Unknown filter values must fail closed, numeric values must be preserved or rejected rather than saturated/zeroed, and lifecycle operations such as TTL and withdrawal must be represented explicitly in the capability matrix.
