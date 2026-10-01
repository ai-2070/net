# Python SDK wrapper parity — mesh channels, compute, groups, and the rest

The Rust SDK gaps found along the way have their own plan:
[`RUST_SDK_GAPS_PLAN.md`](RUST_SDK_GAPS_PLAN.md).

## Status

In progress, 2026-10-01. Targets the release after 0.38. Branch `LZL0/python-sdk`.
S0 and S1a done 2026-10-01 (see each slice). S1–S8 not started.

Amended 2026-10-01, same day:
- The two open checks from the first draft were verified (see S0 and S5).
- `NetMesh.num_shards` turned out to be a pure forward (Decision 3).
  *Superseded during S0:* `NetMesh.num_shards` already exists natively
  (`bindings/python/src/lib.rs` ~line 2165); only the stub was missing it. The
  plan now adds **no** native method. See the S0 defects list.
- A Rust SDK gap survey was added as Part B. Its one real gap (R1) feeds the
  Python receive-side design.
- Part B then moved to its own file,
  [`RUST_SDK_GAPS_PLAN.md`](RUST_SDK_GAPS_PLAN.md).
- After review: G6 (dropped constructor options) and slice S1a, which owns it,
  were added. The Node survey had handed G6 to this plan without a slice. S0
  also gained the `NetMesh.__init__` stub fix.

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
| Receive side | ❌ TS has `recv` / `recvShard` / `onStreamData`. The wheel has `poll_shard(shard_id, limit)` and `open_stream_inbox` (per-stream, with the authenticated sender). ~~`NetMesh` has no `num_shards` getter.~~ **Wrong, corrected in S0:** that came from reading the stub. The runtime `NetMesh` has `num_shards()` and `shard_for_stream()`; the stub omitted both. Only `net_sdk.MeshNode` lacks them. |

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

### G6 — `MeshNode.__init__` drops six native options

Found by the Node survey ([`NODE_SDK_GAPS_PLAN.md`](NODE_SDK_GAPS_PLAN.md)
N2, where Node has the same class of bug). Checked by diffing the parameters
of the native constructor (`#[pyo3(signature = …)]`,
`bindings/python/src/lib.rs:1456`) against `net_sdk.MeshNode.__init__`
(`sdk-py/src/net_sdk/mesh.py:98`).

The native constructor accepts six keyword arguments that the SDK neither
declares nor forwards:

| Option | Effect when dropped |
|---|---|
| `capability_gc_interval_ms` | Capability-index GC cadence can't be tuned |
| `require_signed_capabilities` | **Security-relevant:** an SDK user can't make the node reject unsigned capability announcements |
| `reflex_override` | Can't pin the public reflex address |
| `try_port_mapping` | Can't enable UPnP/NAT-PMP port mapping |
| `auto_direct_upgrade` | Can't enable direct-path upgrade after relay |
| `permissive_channels` | Can't opt into permissive channel admission |

This is the second time this wrapper has dropped constructor options: the
`mesh.py` comment "SSDK P4: forward the topology kwargs this wrapper used to
drop" records the first. Nothing stops a third, hence the drift guard in S1a.

**Related stub drift.** The `NetMesh.__init__` stub (`_net.pyi` ~line 828)
omits `subnet_authorities`, `subnet_attachment`, `subnet_control_channel` and
`subnet_exports`. The native signature (`lib.rs:1471–1474`) has all four, and
the SDK already passes them. Type checkers reject a correct call. S0 fixes it.

## The design

### Principle: forward, don't reimplement

Every new `net_sdk` member forwards to an existing native method. This plan adds
**no** native method (the first draft's `NetMesh.num_shards` already existed; see
S0). It touches the binding only to fix `_net.pyi`. Validation, error mapping and wire encoding stay
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
- `MeshNode.num_shards` / `shard_for_stream`, forwarded to the existing
  native methods. (Earlier drafts planned a new native getter, then a
  cached-kwarg fallback. Neither is needed: S0 found the native methods
  already exist, only unstubbed.)
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
- Add `subnet_authorities`, `subnet_attachment`, `subnet_control_channel` and
  `subnet_exports` to the `NetMesh.__init__` stub (G6, related stub drift).
  S1a's guard checks the stub against the native signature from then on.
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
- **Done 2026-10-01.**
  - **Wheel:** built with `maturin develop` and CI's exact `python-tests`
    feature list (`net,cortex,compute,groups,meshdb,meshos,deck,aggregator,
    tool,consent,mcp,delegation,publish,a2a,payments,payments-http,org,
    dataforts,extension-module`), into a scratch venv (CPython 3.10). The
    globally installed `net-mesh` is 0.36 and was not used.
  - **RED**, with the HEAD `_net.pyi` and the new tests: every daemon and
    group class failed `stub declares no methods for …` (`CausalEvent`,
    `DaemonRuntime`, `DaemonHandle`, `MigrationHandle`, `ReplicaGroup`,
    `ForkGroup`, `StandbyGroup`), and the reverse test failed for `NetMesh`
    and all those classes.
  - **GREEN:** `pytest tests/test_stub_drift.py
    tests/test_sdk_mesh_ctor_parity.py tests/test_pyi_stub_coverage.py` gives
    219 passed, 1 skipped. The skip is `RedisStreamDedup`, which needs the
    `redis` feature this wheel doesn't build.
  - **Defects found on the way, all fixed in this slice:**
    - **14 runtime `NetMesh` methods had no stub:** `num_shards`,
      `shard_for_stream`, the gang-claim scheduler (`publish_island_topology`,
      `match_islands`, `reserve_island`, `release_island`, `claim_island`),
      placement filters (`register_` / `unregister_` / `has_placement_filter`),
      `list_tools` / `watch_tools`, and `set_a2a_org_caller` /
      `a2a_org_caller`. Plus the two aggregation methods G4 already named.
      This **invalidated the plan's G1 claim** that `NetMesh` lacks
      `num_shards`; see the Status amendment.
    - **`CausalEvent` had no `__init__` in the stub**, though Python code
      constructs it (`CausalEvent(origin_hash, sequence, payload)`) to call
      `deliver`.
    - **The stub declared three `nat-traversal`-only methods**
      (`traversal_stats`, `connect_direct`, `connect_direct_auto`), absent
      from CI's wheel. They stay declared, and the test excuses them by an
      explicit `FEATURE_GATED` map of `(class, method) -> feature`, never just
      for being absent.
  - **Not done:** an exhaustive check of every class in the stub. The
    sampled set is the forwarded-to classes plus the original three.

### S1a — Forward every constructor option, with a drift guard (G6)

- Add the six options from G6 to `MeshNode.__init__` as keyword-only
  arguments, with the native types, and forward them to `_NetMesh(...)`.
- **The drift guard:** new test `bindings/python/tests/test_sdk_mesh_ctor_parity.py`.
  It lives with the wheel tests because it needs the real native signature.
  - **The source of truth is the native constructor itself.** Read the
    parameter names from `inspect.signature(net._net.NetMesh)`. PyO3 publishes
    a `__text_signature__` for a `#[new]` with an explicit `signature`. If
    that's unavailable, parse the `#[pyo3(signature = (…))]` block above
    `NetMesh::new` in `bindings/python/src/lib.rs` instead. If **neither**
    source yields a list, the test fails; it must never skip, because a
    skipped drift guard is the silent no-op AGENTS.md warns about. Don't
    hand-maintain the list.
  - **Assertion 1:** native parameter names == `net_sdk.MeshNode.__init__`
    parameter names, minus an explicit allow-list where each entry has a reason.
    The allow-list starts empty.
  - **Assertion 2:** native parameter names == the `NetMesh.__init__`
    parameters in `_net.pyi`, parsed with `ast`. This catches the stub half of
    G6.
  - **Assertion 3 (forwarding, not just declaration):** construct a `MeshNode`
    with every optional kwarg set to a distinct sentinel, with `_NetMesh`
    monkeypatched to record its kwargs. Every sentinel must reach the native
    call unchanged. A declared-but-not-forwarded option fails here.
- **Behaviour is witnessed where it lives, not re-witnessed here.** What
  `require_signed_capabilities` *does* (rejecting unsigned announcements) is
  covered by the Rust integration suite (`tests/capability_broadcast.rs`,
  `tests/capability_multihop.rs`, `tests/subnet_auth_e2e.rs`). The Python
  binding only checks that the option constructs (`test_subnets.py:196`). A
  Python-level rejection test isn't buildable through the public API, because
  every Python node signs its own announcements. So this slice's claim is
  exactly "the option reaches the native constructor", which Assertion 3
  proves. A live smoke test constructs a `net_sdk.MeshNode` with all six
  options set to valid values and shuts it down cleanly, proving the
  forwarded values are accepted, not just passed along.
- **Proves it:**
  - Assertion 1 fails on today's tree, naming the six options.
  - Assertion 2 fails on today's stub, naming the four `subnet_*` kwargs.
  - Assertion 3 fails if a future option is declared but not forwarded.
  - Record both RED runs here when the slice lands.
- **Done 2026-10-01.**
  - `net_sdk.MeshNode.__init__` declares and forwards all six options, and
    documents each one. The stub's `NetMesh.__init__` gained the four
    `subnet_*` arguments (S0).
  - **Signature source:** `inspect.signature(net._net.NetMesh)` returned the
    full native list, so the `lib.rs` fallback parser wasn't needed in this
    run. It is kept, and is checked only by reading it.
  - **CI import path:** CI's main pytest run happens before the
    `pip install -e ../../sdk-py` step (`ci.yml` ~lines 4190 and 4351), so the
    test imports `net_sdk` from the in-repo `sdk-py/src` when it isn't
    installed. The existing pattern (`test_meshos.py`) `pytest.skip`s instead,
    which means those wrapper tests skip in CI's main run. This test
    deliberately doesn't.
  - **RED**, with the HEAD `sdk-py/.../mesh.py` and HEAD stub:
    - Assertion 1 failed, naming the six options: `net_sdk.MeshNode.__init__
      drops native options ['auto_direct_upgrade',
      'capability_gc_interval_ms', 'permissive_channels', 'reflex_override',
      'require_signed_capabilities', 'try_port_mapping']`.
    - Assertion 2 failed: `_net.pyi NetMesh.__init__ is missing
      ['subnet_attachment', 'subnet_authorities', 'subnet_control_channel',
      'subnet_exports']`.
    - The live real-values test failed too (unknown keyword).
    - Assertion 3 passed on HEAD, as it should: it checks that every
      *declared* option is forwarded, and the declared ones were.
  - **GREEN:** all 5 tests in `test_sdk_mesh_ctor_parity.py` pass.
  - **`sdk-py` suite:** `pytest tests` gives 345 passed. One file,
    `test_packaging_metadata.py`, was excluded locally because it needs
    Python 3.11's `tomllib`; CI's Python has it.

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
- ~~Native: add a `NetMesh.num_shards` getter.~~ Not needed; it exists
  (S0). `MeshNode.num_shards` / `shard_for_stream` forward to it.
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
- ~~**The `num_shards` getter is a native change.**~~ Retired: the native
  method already exists (S0).
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

- Any new **native** capability. If a slice
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
