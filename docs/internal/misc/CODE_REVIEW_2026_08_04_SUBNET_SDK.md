# Subnet Authority SDK branch — review pass — 2026-08-04

Branch: `subnet-sdk` @ `f8be5ea56`, diffed against `master`
(merge-base `c9ced5336`). 17 commits, 77 files, ~9.4k insertions —
S0–S6 of `docs/internal/plans/SUBNET_AUTH_SDK_PLAN.md`: the subnet
AUTHORITY plane across the core crate, the Rust SDK facade, the offline
CLI issuance verbs, and the Node / Python / Go / C bindings.

## Status

**Open.** 5 items: **0 Critical / 0 High / 1 Medium / 3 Low / 1 Nit**.
Nothing here blocks the correctness of the authority plane itself; M1
and L3 are contract-vs-implementation mismatches that will surface for
integrators, L1/L2/N1 are localized slips.

Per the "no review-tracking IDs in code or commit messages" rule, the
labels below are for this doc only.

## Verification performed

| Check | Result |
|---|---|
| `cargo check --workspace --all-targets` (default features) | clean |
| `cargo check -p net-mesh-sdk --features "net,cortex,fixtures" --all-targets` | clean, no warnings |
| `cargo clippy -p net-mesh-sdk --features "net,cortex" --lib` | clean |
| `cargo test -p net-mesh-sdk --test subnet_facade --test subnet_kind_fixture` | 5 passed, 0 failed |

Not run on this host: the Go/cgo suite (needs a built `libnet_org`), the
Node/Vitest and Python/pytest binding suites (need a compiled native
module + CPython on PATH), and the `cfg(unix)` permission paths. See
`verify-cfg-unix-on-windows` — those stay with the Docker gate.

## What holds up

Recorded so a later pass does not re-derive it.

- **The authority pipeline is genuinely not forked.** `plan_exported`
  swaps only the discovery source; `authorize_discovered`
  (`sdk/src/org/call.rs:400`) runs grant matching, ambiguity detection,
  and proof-relevant classification verbatim for both the private and
  exported planes, and `select` is shared. There is no second place a
  same-org relation can be decided.
- **`public_owned_providers`
  (`src/adapter/net/behavior/fold/capability_bridge.rs:513`)** samples
  candidate and owner under one fold acquisition, drops unowned
  candidates outright, and excludes a publisher whose live entries
  project different orgs rather than tiebreaking. Both tear directions
  (floor retraction, announcement replacement) have tests.
- **Fail-closed decode ordering** in
  `admin::install_gateway_credentials_node` — the whole batch decodes
  before any install, so one malformed artifact mutates no node state.
- **`NetSubnetPath::to_core`** rejects a non-zero inactive tail instead
  of silently truncating, with a test for both refusal shapes.
- **The stable-kind fixture** is single-sourced through exhaustive
  matches with no wildcard, so a new core `SubnetAuthError` variant
  fails compilation before it can silently reclassify.
- **`alloc_probe`** is cleanly `cfg(any(test, feature = "fixtures"))`
  gated, const-initialized (no allocation inside the measured section),
  and the CI job the new `subnet_relay_alloc_e2e` pin lands in already
  runs `--features "net fixtures"`.
- **Reuse over copy in the bindings** — `auto_register_org_channels`,
  `org_bytes_handler`, `dispatch_to_js`, `run_py_org_handler`,
  `caller_dict`, and the Go handler registry are all promoted to shared
  visibility rather than duplicated for the subnet path.

## M. Medium

| ID | Area | Title | Location |
|----|------|-------|----------|
| M1 | bindings | Node and Python `subnet:` classifiers bare-prefix match, so the serve-registration wrapping case (`unknown_export_name`) never classifies — contradicting the shipped docs, which tell users these classifiers scan for the token. Go is the only binding that does. | `bindings/node/errors.ts:365`, `bindings/python/python/net/subnet.py:43` vs `go/subnet.go:163` |

### M1 detail

`web/src/content/docs/reference/error-codes.md:192` states:

> **Serve-registration failures wrap the envelope**
> (`… failed: subnet:unknown_export_name: …`) rather than leading with
> it. Use the binding classifiers — `classifySubnetError` (Node),
> `net.subnet.parse_subnet_kind` (Python), `ParseSubnetKind` /
> `errors.Is(err, ErrSubnet)` (Go), the `NET_ORG_ERR_SUBNET` return
> code (C) — **which scan for the token**; a bare-prefix parse misses
> these.

Only Go scans (`strings.Index(wire, "subnet:")`). Node and Python both
do the bare-prefix parse the paragraph warns against:

- `bindings/node/errors.ts:365` — `if (!msg.startsWith(ERR_SUBNET_PREFIX)) return e`
- `bindings/python/python/net/subnet.py:43` — `if not message.startswith(_ERR_SUBNET_PREFIX): return None`

Both binding serve paths wrap the message, so the prefix is never at
position 0 for the failure this matters most for:

- `bindings/node/src/subnet.rs:331` — `Error::from_reason(format!("subnet-exported serve registration failed: {e}"))`
- `bindings/python/src/subnet.rs:336` — `SubnetProvisionError::new_err(format!("subnet-exported serve registration failed: {e}"))`

Observable consequences:

1. **Node** throws a plain `Error` for `unknown_export_name`, never a
   `SubnetProvisionError`. `serveSubnetExportedTyped` routes through
   `classifyError` (`bindings/node/subnet.ts:181`), whose subnet branch
   (`errors.ts:390`) is the same `startsWith` gate. The most common
   provider-side failure is the one case the taxonomy does not cover.
2. **Python** raises the correct class (the native `PyErr` type is
   right), but `.kind` is only ever set by `classify_subnet_error`, so
   the natively-raised exception has no `kind` attribute and
   `parse_subnet_kind` returns `None` for it. That makes the docstring
   at `bindings/python/python/net/subnet.py:130` — "unknown name raises
   `SubnetProvisionError` (kind `unknown_export_name`)" — undeliverable
   as written.

The existing tests were written around the gap rather than against the
contract, so CI stays green: `test_subnet_binding.py:117` and
`node/test/subnet_binding.test.ts:136` both assert on a message
substring, and the Python test comment at lines 111–114 says explicitly
"classify on the substring, exactly as the Node suite does."

Two ways out, either acceptable:

- Make `classifySubnetError` / `parse_subnet_kind` scan for the token
  the way `ParseSubnetKind` does (smallest diff, matches the doc as
  written), then tighten both binding tests to assert on the classified
  type and `kind` rather than the raw string; or
- Stop wrapping in the two binding serve paths so the envelope leads,
  and correct the docs paragraph — but this loses the "registration
  failed" framing the other three bindings' messages carry.

Whichever is chosen, the Python docstring and the docs paragraph need to
end up agreeing with the code.

## L. Low

| ID | Area | Title | Location |
|----|------|-------|----------|
| L1 | core | `PublicOwnedProvider` was inserted between `UnaryAdmission`'s doc comment and its enum: the new public struct's rustdoc opens with three unrelated lines about unary serve registration, and `UnaryAdmission` is left undocumented. | `src/adapter/net/mesh_rpc.rs:6281-6299` |
| L2 | ffi | `net_subnet_declare_boundaries` returns before `Box::from_raw(mesh_arc)` when `authority` is NULL, stranding the consumed `Arc<MeshNode>` — a fresh instance of exactly the bug `provisioning_entry_does_not_leak_the_mesh_arc_on_bad_input` was written to guard. | `bindings/go/org-ffi/src/lib.rs:1285` |
| L3 | ffi | `NET_SUBNET_ACCESS_SAME_ORG` / `_GRANTED` exist in three places with no drift guard; the Go preamble comment claims the numeric mirror test covers them, and it does not. | `bindings/go/org-ffi/src/lib.rs:1209-1211`, `include/net_subnet.h:90-91`, `go/subnet.go` cgo preamble |

### L1 detail

```
6281  /// The admission shape for a unary serve registration (E1.1), threaded from the
6282  /// public `serve_rpc` / protected `serve_rpc_protected` wrappers into the shared
6283  /// `serve_rpc_unary_impl`. Streaming / duplex have no protected form (E1.8).
6284  /// A publicly announced provider of a service together with its
...
6291  #[derive(Debug, Clone, PartialEq, Eq)]
6292  pub struct PublicOwnedProvider { ... }
6299  enum UnaryAdmission {
```

Move the struct (and its own doc block) above line 6281. Cosmetic, but
`PublicOwnedProvider` is public API and its rendered rustdoc currently
leads with someone else's paragraph.

### L2 detail

The file's stated discipline, repeated in four comments, is to take
ownership of the mesh arc *immediately* so that validation early-returns
drop the node rather than leaking it. `net_subnet_declare_boundaries`
folds the new `authority` null check into the same `if` as the
`mesh_arc` check, before the consume:

```rust
if mesh_arc.is_null() || authority.is_null() {
    return NET_ORG_ERR_NULL;          // <- mesh_arc never reclaimed
}
let node: Arc<MeshNode> = unsafe { *Box::from_raw(mesh_arc) };
```

`net_subnet_serve_exported:1401` adds the same shape via the new
`export_ref.is_null()` clause. (The `out_handle.is_null()` half there
mirrors pre-existing `net_org_serve`, so that one is inherited, not
introduced — worth fixing at the same time regardless.)

Severity is low because it is unreachable from ordinary Go: `go/subnet.go`
always passes `&authority[0]` from a length-validated slice, and
`&ref`/`&out` are always non-NULL. It only bites a raw-C consumer, and it
leaks rather than corrupting. Fix is mechanical — move the extra null
checks below the `Box::from_raw`, matching every other entry point in the
file.

### L3 detail

The two access-mode constants are declared three times: the Rust
`pub const`s, `include/net_subnet.h:90-91`, and again inside
`go/subnet.go`'s cgo preamble. `header_numeric_contract_matches_rust`
(`org-ffi/src/lib.rs:1674`) reads only `include/net_org.h` and only
matches `#define NET_ORG_`, so `net_subnet.h` has no drift guard at all —
and the `NET_ORG_ACCESS_*` entries in its `want` list are the *org*
constants, not these.

`go/subnet.go`'s preamble asserts otherwise:

> Mirrors include/net_subnet.h + the subnet codes in net_org.h. The Rust
> bindings/go/org-ffi/src/lib.rs is the source of truth, guarded by the
> header<->Rust numeric mirror test.

True for the `NET_ORG_*` codes it copies; false for the two
`NET_SUBNET_ACCESS_*` ones. Extending the existing test to also read
`net_subnet.h` and match `#define NET_SUBNET_` is a few lines and closes
the claim.

## N. Nits

| ID | Title | Location |
|----|-------|----------|
| N1 | `MeshNode.create` casts the whole native options literal `as any`, not just the four new subnet fields — a typo in `bindAddr`, `psk`, or any pre-existing property now compiles silently. Narrow the cast to the added properties. | `sdk-ts/src/mesh.ts:403` |

## Notes for a follow-up pass

- The known gaps the branch documents rather than hides — Go/C cannot
  declare subnet trust anchors (no post-construction installer; the DTO
  conversion module lives above base libnet), no X2 live cell, no cgo
  run — are recorded in `go/subnet.go`'s header comment and the plan.
  They are deferrals, not defects, and are out of scope for this pass.
- `run_issue_delegated` decodes the issuer grant and checks scope
  containment and rights attenuation with the core predicates, but does
  not verify the issuer grant's *signature* against an authority root.
  That is consistent with the offline-minting model (every verifier
  re-checks, and the CLI has no root to check against unless one is
  passed), but it means `issue-delegated` will happily frame a forged
  issuer grant into a credential set that no node will accept. Worth a
  sentence in the CLI docs rather than a code change.
