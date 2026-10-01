# Python SDK wrapper parity — mesh channels, compute, groups, and the rest

The Rust SDK gaps found along the way have their own plan:
[`RUST_SDK_GAPS_PLAN.md`](RUST_SDK_GAPS_PLAN.md).

## Status

Planned, 2026-10-01. Targets the release after 0.38. Branch `LZL0/python-sdk`.
No slice has landed yet.

Amended 2026-10-01, same day:
- The two open checks from the first draft were verified (see S0 and S5).
- `NetMesh.num_shards` turned out to be a pure forward (Decision 3).
- A Rust SDK gap survey was added as Part B. Its one real gap (R1) feeds the
  Python receive-side design.
- Part B then moved to its own file,
  [`RUST_SDK_GAPS_PLAN.md`](RUST_SDK_GAPS_PLAN.md).

## The gap

The PyO3 wheel (`net`, built from `bindings/python/`) already implements mesh
channels, compute daemons, HA groups, blobs and identity, and has tests for
them. The ergonomic SDK (`net_sdk`, in `sdk-py/`) does not wrap or re-export
any of them. A Python user who starts from `net_sdk.MeshNode` (the documented
entry point) can't register a channel, spawn a daemon or form a group without
importing `net._net` directly or reaching into `MeshNode._native`.

[`SDK_PYTHON_PARITY_PLAN.md`](SDK_PYTHON_PARITY_PLAN.md) left this gap on
purpose: it declared Python "SDK wrapper classes" out of scope and kept the
Python surface a thin pyclass layer. Since then `net_sdk` has become a real
wrapper layer (`mesh.py`, `org/`, `deck.py`, `meshos.py`, `tool.py`,
`consent.py`, …), so the surfaces it skips now stand out. This plan is the
follow-up.

How this was checked: a survey of `sdk-py/src/net_sdk/*.py` against
`sdk-ts/src/*.ts` and `bindings/python/src/*.rs` + `python/net/_net.pyi`.
The commands were `grep -nE '^\s*def ' sdk-py/src/net_sdk/mesh.py`, the TS
`mesh.ts` method list, the `NetMesh` block of `_net.pyi` (lines 813–1259), and
the `#[pyclass]`/`fn` lists of `compute.rs` and `groups.rs`.

### G1 — Mesh channels (register / subscribe / publish)

| Layer | State |
|---|---|
| Wheel `NetMesh` | ✅ `register_channel` (visibility, reliable, `require_token`, `token_roots`, priority, `max_rate_pps`, `publish_caps`, `subscribe_caps`), `subscribe_channel(publisher, channel, token=)`, `unsubscribe_channel`, `publish(channel, payload, reliability=, on_failure=, max_inflight=) -> dict`, `poll_shard`. `AsyncNetMesh` has async `subscribe_channel` / `unsubscribe_channel` / `publish`. Tests: `bindings/python/tests/test_channels.py`, `test_channel_auth.py`. |
| Wheel exceptions | ✅ `ChannelError`, `ChannelAuthError(ChannelError)` re-exported from `net`. |
| `net_sdk.MeshNode` | ❌ None of the five methods. `ChannelError` / `ChannelAuthError` are not exported from `net_sdk` (TS exports both from `@net-mesh/sdk`). |
| Receive side | ❌ TS has `recv` / `recvShard` / `onStreamData`. The wheel has `poll_shard(shard_id, limit)` and `open_stream_inbox` (per-stream, with the authenticated sender). `NetMesh` has **no `num_shards` getter** (`_net.pyi` has one only on `Net`, line 239), so a wrapper can't tell how many shards to drain unless it remembers the constructor argument. Core already has `MeshNode::num_shards()`, and the Rust SDK forwards it (`sdk/src/mesh.rs:915`). Only the PyO3 forward is missing. |

`net_sdk.TypedChannel` exists, but it is bound to the local `NetNode` bus
(`node.channel(...)`), not to mesh channels. The two "channel" concepts share a
name and must not be confused in the new API.

### G2 — Compute / daemons

| Layer | State |
|---|---|
| Wheel | ✅ `compute.rs` (1,852 lines): `DaemonRuntime(mesh: NetMesh)` with `start` / `shutdown` / `is_ready` / `daemon_count` / `register_factory(kind, factory)` / `spawn` / `spawn_from_snapshot` / `stop` / `snapshot` / `deliver` / `start_migration(_with)` / `expect_migration` / `register_migration_target_identity` / `migration_phase`. Also `MigrationHandle` (`wait`, `wait_with_timeout`, `cancel`, `phases`), `CausalEvent`, `AsyncDaemonRuntime`, `AsyncMigrationHandle`. The `net.migration_error_kind(exc)` helper lives in `python/net/__init__.py:296`. Tests: `test_compute.py`. |
| `_net.pyi` | ⚠️ `DaemonRuntime`, `DaemonHandle` and `MigrationHandle` (lines 2541–2551) are class shells with a docstring and **no methods**, so type checkers and IDEs see no callable surface. The async twins *are* typed (line 3635). |
| `_net.pyi` | ⚠️ `MigrationError`'s docstring (line 2532) points to `net_sdk.compute.migration_error_kind`, which does not exist. The helper is `net.migration_error_kind`. |
| `net_sdk` | ❌ No `compute` module. TS `compute.ts` ships `MeshDaemon` (interface), `DaemonFactory`, `DaemonHostConfig`, `DaemonStats`, `MigrationPhase`, `MigrationOptions`, `MigrationErrorKind`, `DaemonRuntime`, `DaemonHandle`, `MigrationHandle`. |
| Wiring hazard | `DaemonRuntime.__init__` takes a native `NetMesh`. An `net_sdk.MeshNode` is not one; today the caller has to pass `node._native`. |

### G3 — Groups

| Layer | State |
|---|---|
| Wheel | ✅ `groups.rs`: `ReplicaGroup` (`spawn`, `route_event`, `scale_to`, `on_node_failure` / `on_node_recovery`, counts), `ForkGroup` (`fork`, `verify_lineage`, …), `StandbyGroup` (`spawn`, `promote`, `sync_standbys`, `member_role`, `synced_through`, …). `net.group_error_kind(exc)` at `python/net/__init__.py:342`. Tests: `test_groups.py`. |
| `_net.pyi` | ⚠️ All three group classes (lines 2556–2563) are method-less shells. |
| `net_sdk` | ❌ No `groups` module. TS `groups.ts` ships `GroupErrorKind`, `GroupStrategy`, `GroupHealth`, `GroupMemberInfo`, `ForkRecord`, `RequestContext`, `GroupHostConfig`, `{Replica,Fork,Standby}GroupConfig` and the three classes. |

### G4 — Other `MeshNode` methods the wheel has but the wrapper drops

| Area | Native (`NetMesh`) | TS `MeshNode` | `net_sdk.MeshNode` |
|---|---|---|---|
| Identity | `entity_id` | `entityId` | ❌ |
| nRPC | `MeshRpc(mesh)` in `net.mesh_rpc` | `rpc()` | ❌ |
| Capability aggregation | `capability_aggregate`, `capability_capacity_ranking` (registered in `src/lib.rs:3034`, `:3062`; **missing from `_net.pyi`**) | `capabilityAggregate`, `capabilityCapacityRanking` | ❌, yet `net_sdk/capability_aggregation.py`'s module docstring tells users to call `MeshNode.capability_capacity_ranking` |
| Blobs (dataforts) | module functions `serve_blob_transfer`, `fetch_blob`, `fetch_blob_discovered`, `store_dir`, `fetch_dir` + `MeshBlobAdapter` | methods on `MeshNode` | ❌ no methods, no `net_sdk.blob` module |
| Tools | free functions in `net.tool` | `listTools`, `watchTools` methods | free functions only (`net_sdk.tool`) |
| Connectivity / NAT | `discovered_nodes`, `traversal_stats`, `connect_direct(_auto)`, `rendezvous_string`, `join`, `renew` | partly | ❌ |
| A2A / publish | `publish_tools`, `serve_a2a`, `submit_task`, `task_status`, `cancel_task`, `describe_a2a` | (in SDK modules) | ❌ |
| Low-level | `push_to`, `poll`, `add_route`, `open_stream_inbox` | `recv`, `onStreamData` | ❌ |

### G5 — Missing SDK modules

- **`net_sdk.identity`.** The wheel exports `Identity`, `TokenError`, `IdentityError`, `channel_hash`, `stream_id_from_label`, `verify_signature` and `delegate_token`. `net_sdk` only exposes the delegation helpers. Note: `_net.pyi` has no `Token` class (TS exports one). Tokens cross the Python boundary as `bytes`.
- **`net_sdk.subnets`.** TS `subnets.ts` has `subnetId`, `GLOBAL_SUBNET`, `SubnetRule`, `SubnetPolicy`. Python takes subnets as raw `list` / `dict` constructor kwargs.
- **Async SDK node.** `net_sdk` has no async `MeshNode`, although the wheel ships `AsyncNetMesh`, `AsyncDaemonRuntime`, `AsyncMeshBlobAdapter` and friends ([`PYTHON_ASYNC_SDK_SIDE_BY_SIDE.md`](PYTHON_ASYNC_SDK_SIDE_BY_SIDE.md) built the native half).

## The design

### Principle: forward, don't reimplement

Every new `net_sdk` member forwards to an existing native method. This plan adds
**one** native method (`NetMesh.num_shards`, slice S1) and otherwise touches the
binding only to fix `_net.pyi`. Validation, error mapping and wire encoding stay
in Rust. The wrapper adds three things: typed signatures, `TypedDict`s for the
dict-shaped inputs and outputs, and acceptance of `net_sdk` types where the
wheel wants native ones. This is the pattern `meshos.py` and `deck.py` already
follow.

### Decision 1 — methods on `MeshNode`, or free functions?

**Recommendation: methods on `MeshNode` for anything whose first argument is
the mesh** (channels, `entity_id`, `rpc`, aggregation, blobs, connectivity).
That matches TS, which is the reference SDK, and keeps discovery in one place.
Compute and groups get **their own modules** (`net_sdk.compute`,
`net_sdk.groups`), as in TS, because they are separate objects with their own
lifecycle that merely take a mesh.

Rejected: re-exporting the wheel's free functions (`fetch_blob(mesh, ...)`)
unchanged. It is less code, but it makes callers unwrap `MeshNode._native`
themselves, which is the hazard G2 already describes.

### Decision 2 — how `net_sdk` objects reach native constructors

`DaemonRuntime`, `MeshRpc`, `MeshBlobAdapter` and friends take a native
`NetMesh`. Add one private helper, `net_sdk.mesh._native_mesh(x)`. It returns
`x._native` for a `MeshNode` and `x` itself for a `NetMesh`, and raises
`TypeError` otherwise. Every wrapper constructor accepts either type through it.
Callers who already hold a raw `NetMesh` keep working.

### Decision 3 — the channel receive side

The publish path returns a `PublishReport` dict. For receiving, add:

- `MeshNode.poll_shard(shard_id, limit)`, forwarded as-is.
- `MeshNode.num_shards`. Needs a native getter. This is a one-line forward to
  core `MeshNode::num_shards()`, the same thing `net_sdk::Mesh::num_shards`
  does in Rust, so there's no new behaviour to review. (The first draft offered
  a cached-kwarg fallback. That fallback is dropped, because it would have to
  guess the default the native constructor applies.)
- `MeshNode.recv(limit=..., timeout=None) -> list[StoredEvent]`, which drains
  every shard round-robin, matching TS `recv`.
- `MeshNode.open_stream_inbox(stream_id, capacity)`, forwarded. It's the only
  receive path that reports the **authenticated sender**, which TS
  `onStreamData` also exposes and `recv` can't. Rust gains the same in R1
  of [`RUST_SDK_GAPS_PLAN.md`](RUST_SDK_GAPS_PLAN.md), so the three SDKs line
  up.

Rejected: a background thread that fans shard polls into a queue. It hides the
thread from the caller and duplicates what the async node (S7) will do
properly.

Naming: these are **mesh** channels. The new methods go on `MeshNode` and are
named after the wheel (`register_channel`, `subscribe_channel`,
`unsubscribe_channel`, `publish`). Keep `net_sdk.TypedChannel` (the local-bus
channel) as it is, and say in its docstring that it is not a mesh channel.

### Decision 4 — the shape of `net_sdk.compute`

- `MeshDaemon`, a `typing.Protocol` with `process(event: CausalEvent) -> list[bytes]`
  plus optional `snapshot() -> bytes | None` and `restore(state: bytes) -> None`.
  It mirrors the TS interface and the contract the wheel already enforces in
  `PyDaemonBridge` (`compute.rs:976–1068`). Use a Protocol rather than an ABC,
  because the wheel duck-types and an ABC would reject objects that work today.
- `DaemonFactory = Callable[[], MeshDaemon]`.
- `DaemonHostConfig(TypedDict, total=False)` with `auto_snapshot_interval` and
  `max_log_entries`, the two keys `daemon_host_config_from_dict` reads.
- `MigrationPhase = Literal[...]`, taken from `migration_phase_str`
  (`compute.rs:164`). `MigrationErrorKind = Literal[...]`, taken from
  `format_migration_error`.
- `DaemonRuntime`, `DaemonHandle` and `MigrationHandle` re-exported from the
  wheel. `DaemonRuntime` gets a thin subclass or factory function so it accepts
  a `MeshNode` (Decision 2).
- Re-export `migration_error_kind`, `DaemonError`, `MigrationError` and
  `CausalEvent`.

`net_sdk.groups` follows the same shape: re-export the three classes,
`GroupError` and `group_error_kind`; add `GroupErrorKind` (a `Literal`), the
config `TypedDict`s, and `RequestContext(TypedDict)` for `route_event(ctx)`.

### Decision 5 — feature gating

The wheel compiles `compute`, `groups` and `dataforts` by default
(`bindings/python/Cargo.toml` `default = [...]`), but a minimal build can omit
them. Each new `net_sdk` module imports inside `try: … except ImportError`, the
pattern `__init__.py` already uses for `consent` / `delegation` /
`enrollment`. `net_sdk/__init__.py` extends `__all__` only for the modules that
imported.

## The slices

Each slice leaves the tree green, and each names the test that would fail if
the slice were wrong. Two test homes, with different jobs:

- **`sdk-py/tests/`** runs against the conftest auto-stub of `net`
  (`sdk-py/tests/conftest.py`), not a real wheel. It proves the
  **forwarding**: inject a mock `_native`, call the wrapper, assert the native
  method got the right arguments. It cannot prove behaviour.
- **`bindings/python/tests/`** runs against a built wheel, and CI's
  `python-tests` job installs the `sdk-py` wrapper alongside
  (`ci.yml` ~line 4351, `pip install --no-deps -e ../../sdk-py`). Live
  end-to-end witnesses for the wrapper go here, in files named
  `test_sdk_*.py`.

### S0 — Stub truth (`_net.pyi` only)

- Fill in methods for `DaemonRuntime`, `DaemonHandle`, `MigrationHandle`,
  `ReplicaGroup`, `ForkGroup` and `StandbyGroup` from the `#[pymethods]` blocks
  in `compute.rs` / `groups.rs`.
- Add `NetMesh.capability_aggregate` / `capability_capacity_ranking`.
- Fix the `MigrationError` docstring to point at `net.migration_error_kind`.
- **Verified 2026-10-01: today's tests can't catch the empty stubs.** Two tests
  guard the stub, and neither checks that a declared class's methods are all
  stubbed:
  - `tests/test_pyi_stub_coverage.py` works at the name level only. Every name
    `__init__.py` imports from `._net` must be declared in the stub, and
    `DaemonRuntime` / `ReplicaGroup` are family sentinels. A class shell
    satisfies both.
  - `tests/test_stub_drift.py::test_sampled_class_methods_present` checks only
    the **stub → runtime** direction (every stubbed method exists at runtime),
    and only for `SAMPLED_CLASSES = ["MeshOsDaemonSdk", "DeckClient",
    "MeshQueryRunner"]`. A method the runtime has but the stub omits is never
    looked for. That is exactly how `NetMesh.capability_aggregate` /
    `capability_capacity_ranking` went missing unnoticed.
- So S0 also changes the tests:
  - Add `DaemonRuntime`, `DaemonHandle`, `MigrationHandle`, `ReplicaGroup`,
    `ForkGroup`, `StandbyGroup` and `NetMesh` to `SAMPLED_CLASSES`. Its
    `assert declared, "stub declares no methods"` line then fails on today's
    shells, which is the RED witness.
  - Add `test_sampled_class_runtime_methods_are_stubbed`, the reverse
    direction. For each sampled class, every public runtime attribute (no
    leading `_`, callable or descriptor) must be declared in the stub, with a
    small explicit allow-list for PyO3-generated names.
- **Proves it:** both new assertions fail on the current tree (shells; missing
  `capability_aggregate`) and pass after the stub fill. Record the RED run in
  this plan when the slice lands. These tests skip when no wheel is built, so
  the RED/GREEN run needs `maturin develop` with the default features.

### S1 — Mesh channels on `MeshNode`

- `register_channel`, `subscribe_channel`, `unsubscribe_channel`, `publish`,
  `poll_shard`, `num_shards`, `recv`, `open_stream_inbox`.
- `ChannelConfig` / `PublishConfig` / `PublishReport` as `TypedDict`s,
  matching the TS type names.
- Type `visibility` as `Literal["subnet-local", "parent-visible", "exported",
  "global"]`. These are exactly the strings `parse_visibility` accepts
  (`bindings/python/src/lib.rs:1051`); anything else raises `ChannelError`.
  Do the same for `reliability` and `on_failure`, read from their parsers
  rather than from TS.
- Re-export `ChannelError` and `ChannelAuthError` from `net_sdk`.
- Native: add a `NetMesh.num_shards` getter (plus stub entry), forwarding to
  `MeshNode::num_shards()`.
- **Proves it:**
  - `sdk-py/tests/test_mesh_channels_wrapper.py` (forwarding, every kwarg
    reaches the native call).
  - `bindings/python/tests/test_sdk_mesh_channels.py`, two live `MeshNode`s:
    register → subscribe → publish → `recv` returns the payload; a
    `subscribe_caps` mismatch raises `net_sdk.ChannelAuthError`; `publish` with
    no subscribers returns `attempted == 0`.

### S2 — `net_sdk.compute`

- The module described in Decision 4, plus the `_native_mesh` helper.
- **Proves it:**
  - `sdk-py/tests/test_compute_wrapper.py`: imports, `__all__`, and a
    `MeshNode` is accepted.
  - `bindings/python/tests/test_sdk_compute.py`: an echo daemon written against
    the `MeshDaemon` Protocol spawns from a `net_sdk.MeshNode`, `deliver`
    returns its output, and snapshot → `spawn_from_snapshot` round-trips state.
    A daemon whose `process` raises comes back as `DaemonError`, not a crash.
    That last point is the `SDK_COMPUTE_SURFACE_PLAN.md` "daemon panics in
    non-Rust code" requirement, re-witnessed through the wrapper.

### S3 — `net_sdk.groups`

- The module described in Decision 4.
- **Proves it:** `bindings/python/tests/test_sdk_groups.py`. A
  `ReplicaGroup` of 3 sees every event. `ForkGroup.verify_lineage()` is true.
  `StandbyGroup.promote()` changes `active_origin`. An unknown factory kind
  raises `GroupError` and `group_error_kind` classifies it.

### S4 — Remaining `MeshNode` methods

- `entity_id` and `rpc()` (returns `net.mesh_rpc.MeshRpc` bound to this node).
- `capability_aggregate` / `capability_capacity_ranking`, taking the
  `net_sdk.capability_aggregation` dataclasses and doing the JSON conversion
  that module already implements. This makes its docstring true.
- Blob methods (`serve_blob_transfer`, `fetch_blob`, `fetch_blob_discovered`,
  `store_dir`, `fetch_dir`) plus a `net_sdk.blob` module re-exporting
  `BlobRef`, `MeshBlobAdapter` and `BlobError`.
- `list_tools` / `watch_tools` as methods delegating to `net_sdk.tool`.
- Connectivity: `discovered_nodes`, `traversal_stats`, `connect_direct(_auto)`.
- **Proves it:**
  - `sdk-py/tests/test_mesh_node_surface.py`: a table-driven forwarding test,
    one row per method.
  - `bindings/python/tests/test_sdk_mesh_surface.py`: `rpc()` round-trips a
    unary call; `store_dir` → `fetch_dir` across two nodes; `capability_capacity_ranking`
    returns rows for an announced capability set.

### S5 — `net_sdk.identity` and `net_sdk.subnets`

- `identity`: re-export the wheel's identity surface, plus a `TokenScope`
  `Literal` that matches the scope strings the wheel accepts.
- `subnets`: `subnet_id(...)`, `GLOBAL_SUBNET` and `SubnetPolicy` /
  `SubnetRule` `TypedDict`s. These must match exactly what `NetMesh.__init__`
  accepts as `subnet` / `subnet_policy`; read the parser in `subnet.rs`, don't
  copy the TS shape blindly.
- **Verified 2026-10-01: the shapes to match.**
  - `subnet` is a list of 1–4 level bytes, each in range.
  - `subnet_policy` is `{"rules": [{"tag_prefix": str, "level": int,
    "values": {str: int (non-zero)}}]}`.
  - Both are pinned by the validation tests in `tests/test_subnets.py`.
  - Channel visibility strings are the four listed in S1.
- **Multi-node visibility is not witnessed from Python today.** The
  `test_subnets.py` docstring delegates it to the Rust suite
  (`tests/subnet_*.rs`). The first draft's "two nodes in different subnets
  don't see each other's subnet-local channel" witness would therefore be new
  ground for Python, not a wrapper check. It's dropped from this slice; the
  wrapper only has to produce shapes the native layer accepts.
- **Proves it:** `bindings/python/tests/test_sdk_identity_subnets.py`.
  - A `subnet` + `subnet_policy` built with the new helpers constructs a
    `MeshNode`.
  - The same invalid inputs `test_subnets.py` rejects are rejected through the
    helpers. The helpers must not validate more loosely or more strictly than
    the native layer.
  - `register_channel(visibility="subnet-local")` succeeds, and an unknown
    visibility string raises `net_sdk.ChannelError`.
  - `channel_hash` from `net_sdk.identity` matches the cross-lang fixture
    value.

### S6 — The A2A / publish / enrollment methods

`publish_tools`, `serve_a2a`, `submit_task`, `task_status`, `cancel_task`,
`describe_a2a`, `rendezvous_string`, `join`, `renew` on `MeshNode`. These have
working native tests under the Hermes track, so this slice is forwarding plus
type signatures only.

- **Proves it:** forwarding rows added to `test_mesh_node_surface.py`, and one
  live `serve_a2a` → `submit_task` round-trip through `net_sdk.MeshNode`.

### S7 — `net_sdk.AsyncMeshNode`

- An async twin of `MeshNode` over `AsyncNetMesh`, with `async` channel methods
  and an `async for` receive iterator. `net_sdk.compute` / `groups` accept it
  via `_native_mesh`, with the async runtime and handle types.
- Its own slice because it depends on S1–S4 settling the sync API shape first.
- **Proves it:** `bindings/python/tests/test_sdk_async_mesh.py`. The S1 and S2
  live scenarios re-run under `asyncio`. Cancelling a pending
  `subscribe_channel` propagates, per TX-2 in
  `PYTHON_ASYNC_SDK_SIDE_BY_SIDE.md`.

### S8 — Docs

- `sdk-py/README.md`: sections for mesh channels, compute and groups.
- The `web/src/content/docs/` Python tabs for the same features, where TS tabs
  exist and Python ones don't.
- The `net-event-bus` skill's Python snippets, if any show `net._net` imports
  that now have an SDK path.
- **Proves it:** `npm run check` in `web/` stays green, and any README or skill
  example that CI executes still runs.

## Risks

- **The conftest stub can't catch signature drift.** The forwarding tests run
  against an auto-stub that accepts any call. *Fallback:* every slice also has a
  live `bindings/python/tests/test_sdk_*.py` witness, which is the real gate.
- **The `num_shards` getter is a native change.** It's a new public pyclass
  method, but a pure forward to an existing core accessor. *Fallback:* none
  needed. If review objects, `recv` can take an explicit `shards=` argument.
- **The stub tests only cover what they sample.** S0 widens
  `SAMPLED_CLASSES`, but classes added later are still unchecked unless someone
  adds them. *Mitigation:* the reverse-direction test's docstring says to add
  every class a `net_sdk` wrapper forwards to. Sampling every class in the stub
  is the stronger option; it is deferred because feature-gated classes skip
  differently per wheel.
- **Name collision between `net_sdk.TypedChannel` (local bus) and mesh
  channels.** *Mitigation:* the docstrings in S1. Renaming `TypedChannel` is out
  of scope; it would break users.
- **`publish` already means something on `NetNode`-adjacent types**
  (`TypedChannel.publish`). On `MeshNode` it takes `(channel, payload: bytes)`.
  Keep the wheel's signature; don't add a JSON-encoding overload that guesses
  intent.
- **GIL-bound daemons.** Python daemons serialize on the GIL. This is already
  documented in `SDK_COMPUTE_SURFACE_PLAN.md` § Risks. The `net_sdk.compute`
  module docstring repeats it, because it is now the front door.
- **Minimal wheels.** A wheel built without `compute` / `groups` / `dataforts`
  must still import `net_sdk`. *Proves it:* the existing `test_wrapper_modules.py`
  pattern, extended with an import that has a feature missing.

## Not in scope

- Any new **native** capability apart from `NetMesh.num_shards`. If a slice
  finds a wheel bug, it is recorded here and fixed in its own change.
- Renaming or deprecating `net_sdk.TypedChannel`.
- Python ports of TS `store-transport` / `store-persist`. Those serve the
  browser package's store and have no Python consumer.
- `net_sdk.redis_dedup`. `RedisStreamDedup` is already importable from `net`,
  and TS `redis-dedup.ts` exports nothing new.
- Release-wheel feature changes. Whether public wheels ship `compute` /
  `groups` is the separate decision noted in `SDK_GROUPS_SURFACE_PLAN.md`.
- Go and Node parity for anything above.
- Rust SDK work, including moving the bindings onto `net_sdk::Mesh`. See
  [`RUST_SDK_GAPS_PLAN.md`](RUST_SDK_GAPS_PLAN.md).
