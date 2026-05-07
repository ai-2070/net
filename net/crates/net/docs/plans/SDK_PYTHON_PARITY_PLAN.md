# Python parity plan — Stage F of the SDK security surface

## Context

[`SDK_SECURITY_SURFACE_PLAN.md`](SDK_SECURITY_SURFACE_PLAN.md)
Stage F is "Python surface (identity + capabilities + subnets +
auth) — repeat A–E against the PyO3 layer." Unlike Stages C / D /
E, this is **not an iceberg** — the core work is already complete
from those stages. Stage F is purely bindings + tests + docs.

Current Python state (from a fresh survey of
`bindings/python/src/lib.rs` and `bindings/python/python/net/`):

| Surface | Status in Python today |
|---|---|
| `NetMesh` basics (connect/accept/start/open_stream/publish) | ✅ Present |
| Identity / `PermissionToken` / `TokenCache` | ❌ Zero bindings |
| Capabilities (announce / find_nodes) | ❌ Zero bindings |
| Subnets (`subnet`, `subnet_policy`) | ❌ Not on `NetMesh.__new__` |
| Channel auth (`publish_caps` / `subscribe_caps` on register, token on subscribe) | ❌ `require_token` flag exists; ACL + token path unwired |
| `CapabilityAnnouncement.entity_id` + signing | ✅ Core already emits signed announcements |
| `peer_entity_ids`, `TokenCache` on `MeshNode` | ✅ Core fields exist, just need to be plumbed from Python options |
| Test harness | ✅ pytest (sync — PyO3 wraps tokio internally); existing `test_channels.py` shows the idiom |

The core delivered by Stages A–E means every Python binding we add
here is a **thin PyO3 shim** over a type / method that already
works in Rust / NAPI / TS. No new plans, no new subprotocols.

This plan is shorter than the Stage C/D/E expansion plans because
there is no iceberg — just a lot of surface to mirror.

## Scope

**In scope (F-1 through F-5):**

- Greenfield PyO3 bindings for `Identity`, `PermissionToken`
  helpers (parse / verify / delegate / channel-hash), `TokenScope`
  enum via string lists (mirrors TS Stage B).
- Greenfield PyO3 bindings for `CapabilitySet` /
  `CapabilityFilter` as plain dict round-trip (match TS flatly —
  no pyclass).
- Greenfield PyO3 bindings for `SubnetId` / `SubnetPolicy` on
  `NetMesh.__new__` — optional kwargs.
- Extend `NetMesh.register_channel` with `publish_caps` /
  `subscribe_caps` (dicts) and `NetMesh.subscribe_channel` with
  optional `token: bytes`.
- Add `identity_seed: bytes | None` to `NetMesh.__new__` +
  `entity_id` getter — matches the NAPI `identitySeed` +
  `entityId` shape shipped in E-5.
- Type stubs in `_net.pyi` mirroring every new binding.
- Re-exports in `python/net/__init__.py`.
- pytest integration tests covering: identity round-trip,
  capabilities announce → find, subnet visibility, channel auth
  (cap-denied + token-denied + token round-trip).
- README Python section + cross-link from
  `SDK_SECURITY_SURFACE_PLAN.md`.

**Out of scope:**

- Python "SDK wrapper" classes that abstract over the raw PyO3
  types. The Python surface has deliberately been a thin pyclass
  layer to date; this plan preserves that.
- Python async/await ergonomics — PyO3 methods remain sync with
  internal `block_on`, same pattern as the existing channel/
  publish path.
- Revocation / HSM / dynamic policies (deferred same as Rust SDK).

## Design

### Error prefixing

Match the NAPI layer verbatim: `"identity: ..."` and
`"token: <kind>"` message prefixes for the Rust-side errors, plus
an existing `ChannelAuthError` / `ChannelError` / `BackpressureError`
Python exception hierarchy.

Either:

1. Define dedicated `IdentityError` / `TokenError` Python exception
   subclasses (matches the Stage B TS shape — `TokenError` has a
   `.kind` discriminator), **or**
2. Reuse the existing `ChannelAuthError` for the subscribe path and
   return `ValueError` for malformed-bytes / input-validation.

Pick (1) — keeps Python users' mental model aligned with the TS
SDK; `TokenError.kind` is useful for programmatic dispatch without
parsing `str(e)`.

### Token + EntityId on the wire

Match NAPI: tokens cross the boundary as `bytes` (the 159-byte
serialized `PermissionToken`). `EntityId` is a 32-byte `bytes`
value. `Identity.issue_token` returns `bytes`; `install_token` /
`lookup_token` consume / produce them. No Python-side `Token`
class in v1 — `bytes` is idiomatic and round-trips cleanly with
the core `PermissionToken::from_bytes`.

### `CapabilitySet` shape

Plain `dict`, not a pyclass, to keep the surface light. Matches
the NAPI POJO shape:

```python
caps: dict = {
    "hardware": {"cpu_cores": 16, "memory_mb": 65536, "gpu": {...}},
    "software": {"os": "linux", ...},
    "models": [{"model_id": "llama-3.1-70b", ...}],
    "tools": [...],
    "tags": ["gpu", "prod"],
    "limits": {...},
}
```

Conversion `dict → CapabilitySet` happens on the Rust side in a
helper `capability_set_from_py(obj: &PyDict) -> PyResult<CapabilitySet>`.
This mirrors `bindings/node/src/capabilities.rs::capability_set_from_js`.

### `SubnetId` / `SubnetPolicy`

`SubnetId` as a list of ints (max 4 entries, each 0–255) — matches
NAPI `{ levels: number[] }` with the `{levels: ...}` wrapping
dropped because Python doesn't care about struct-shape
discoverability the way JS does.

`SubnetPolicy` as a dict with `rules: [{tag_prefix, level, values: dict}]`.

### PyO3 identity module layout

New file: `bindings/python/src/identity.rs` — mirrors
`bindings/node/src/identity.rs` one-to-one:

```rust
#[pyclass]
pub struct Identity { keypair: Arc<EntityKeypair>, cache: Arc<TokenCache> }

#[pymethods]
impl Identity {
    #[staticmethod] fn generate() -> Self;
    #[staticmethod] fn from_seed(seed: &[u8]) -> PyResult<Self>;
    #[staticmethod] fn from_bytes(bytes: &[u8]) -> PyResult<Self>;
    fn to_bytes(&self) -> Vec<u8>;
    #[getter] fn entity_id(&self) -> Vec<u8>;
    #[getter] fn origin_hash(&self) -> u32;
    #[getter] fn node_id(&self) -> u64;
    fn sign(&self, message: &[u8]) -> Vec<u8>;
    fn issue_token(
        &self, subject: &[u8], scope: Vec<String>,
        channel: &str, ttl_seconds: u32, delegation_depth: u8,
    ) -> PyResult<Vec<u8>>;
    fn install_token(&self, token: &[u8]) -> PyResult<()>;
    fn lookup_token(&self, subject: &[u8], channel: &str)
        -> PyResult<Option<Vec<u8>>>;
}

#[pyfunction] fn parse_token(bytes: &[u8]) -> PyResult<PyObject>; // dict
#[pyfunction] fn verify_token(bytes: &[u8]) -> PyResult<bool>;
#[pyfunction] fn token_is_expired(bytes: &[u8]) -> PyResult<bool>;
#[pyfunction] fn delegate_token(signer, parent_bytes, new_subject, scope)
    -> PyResult<Vec<u8>>;
#[pyfunction] fn channel_hash(channel: &str) -> PyResult<u16>;
#[pyfunction] fn normalize_gpu_vendor(vendor: &str) -> String;
```

### `NetMesh.__new__` extensions

Add keyword-only arguments (PyO3's `#[pyo3(signature = (...))]`):

```rust
fn __new__(
    bind_addr: &str,
    psk_hex: &str,
    *,
    heartbeat_interval_ms: Option<u64> = None,
    session_timeout_ms: Option<u64> = None,
    num_shards: Option<u16> = None,
    capability_gc_interval_ms: Option<u64> = None,
    require_signed_capabilities: Option<bool> = None,
    subnet: Option<Vec<u8>> = None,
    subnet_policy: Option<&PyDict> = None,
    identity_seed: Option<&[u8]> = None,
) -> PyResult<Self>;
```

All optional, defaults match the existing Rust behavior. An
existing call `NetMesh(addr, psk)` continues to work.

### `register_channel` / `subscribe_channel` extensions

```rust
fn register_channel(
    &self,
    name: &str,
    *,
    visibility: Option<&str> = None,
    reliable: Option<bool> = None,
    require_token: Option<bool> = None,
    priority: Option<u8> = None,
    max_rate_pps: Option<u32> = None,
    publish_caps: Option<&PyDict> = None,      // NEW
    subscribe_caps: Option<&PyDict> = None,    // NEW
) -> PyResult<()>;

fn subscribe_channel(
    &self,
    publisher_node_id: u64,
    channel: &str,
    token: Option<&[u8]> = None,               // NEW
) -> PyResult<()>;
```

Keyword-only after `channel` so positional callers don't break.

### `announce_capabilities` / `find_nodes`

```rust
fn announce_capabilities(&self, caps: &PyDict) -> PyResult<()>;
fn find_nodes(&self, filter: &PyDict) -> PyResult<Vec<u64>>;
```

Returns `List[int]` (node ids as plain Python ints — u64 fits
comfortably in Python's unbounded int).

## Staged rollout

Five PRs, mirroring the original stage split:

| Stage | What | Days |
|---|---|---|
| **F-1** | Identity + tokens pyclass + helpers + `identity_seed` + `entity_id()` on `NetMesh`. Type stubs + `IdentityError` / `TokenError` exception classes. pytest round-trip. | 1.5 |
| **F-2** | Capabilities dict conversion + `announce_capabilities` / `find_nodes` on `NetMesh`. `require_signed_capabilities` + `capability_gc_interval_ms`. pytest self-match. | 1 |
| **F-3** | Subnets (`subnet`, `subnet_policy` on `__new__`) + a minimal pytest enforcement test using two meshes in one process. | 0.5 |
| **F-4** | `register_channel` extended with `publish_caps` / `subscribe_caps`; `subscribe_channel` with `token`. pytest: cap-denied, token-denied, token round-trip. | 1 |
| **F-5** | README Python section, `__init__.py` re-exports for new types, cross-link from `SDK_SECURITY_SURFACE_PLAN.md`. | 0.5 |

**Total ~4.5 days** — matching the plan's 1-week estimate with a
small buffer.

## Test plan

`bindings/python/tests/test_identity.py` (new, F-1):

1. `generate()` produces a valid 32-byte entity_id.
2. Seed round-trip: `from_seed(to_bytes(id))` reproduces
   `entity_id`.
3. `issue_token` → `parse_token` → field match (subject / scope /
   channel_hash). `verify_token(bytes)` → `True`; tampered bytes →
   `False`.
4. `install_token` followed by `lookup_token` by subject returns
   the same bytes.
5. Delegation chain: A issues with depth=2 → B re-issues with
   depth=1 → C re-issues with depth=0 → D's re-issue fails with
   `TokenError(kind="delegation_exhausted")`.

`bindings/python/tests/test_capabilities.py` (new, F-2):

1. Single-node self-match: `announce_capabilities({tags: ["gpu"]})`
   → `find_nodes({require_tags: ["gpu"]})` contains own node id.
2. Non-matching filter returns empty list.
3. GpuVendor normalization via `normalize_gpu_vendor("NVIDIA") ==
   "nvidia"` (optional convenience if exposed).

`bindings/python/tests/test_subnets.py` (new, F-3):

1. `NetMesh(..., subnet=[3, 7, 2])` binds to that subnet.
2. Two-node: A/B same subnet, channel `visibility="subnet-local"`
   → subscribe succeeds. (Full three-node test parallels the
   Rust/TS tests but requires the subnet_policy + handshake
   plumbing; start with two-node, expand if time.)

`bindings/python/tests/test_channel_auth.py` (new, F-4):

1. Subscribe denied by `subscribe_caps` filter.
2. Subscribe denied by missing token when `require_token=True`.
3. Subscribe accepted with valid token presented via
   `subscribe_channel(..., token=bytes)`.
4. Publish denied by own `publish_caps` mismatch.

`bindings/python/tests/test_integration.py` (update):
Keep existing test; no regression changes required.

## Risks

- **Two-mesh-per-process pytest**: the TS / Rust integration
  pattern spins two MeshNodes in one process and handshakes. The
  existing `test_channels.py` runs one-mesh smoke tests —
  multi-mesh may hit PyO3 + tokio reentrancy issues (nested
  `block_on`). Mitigation: use a single shared Tokio runtime
  installed in module init, so every `block_on` goes through the
  same handle. If flakey, fall back to subprocess-based tests.
- **PyO3 + DashMap lifetimes**: `peer_entity_ids.get(&id).map(|e|
  e.value().clone())` has been stable throughout the Rust work,
  but the PyO3 GIL + Tokio interplay occasionally surprises.
  Prefer `.clone()` aggressively in the Python binding layer even
  at a small perf cost.
- **Type-stub drift**: Python `_net.pyi` is maintained by hand. A
  binding change without a stub update silently breaks `mypy`
  users. Mitigation: add a check in CI that grepping `#[pyfn]` /
  `#[pymethod]` counts stay in sync with `.pyi` declarations
  (cheap regex audit).
- **Scope creep around an async Python layer** (async/await for
  subscribe/publish). Deferred; PyO3 `block_on` is acceptable for
  v1 and matches what the existing Python surface does.

## Files touched (estimate)

| File | Why |
|---|---|
| `bindings/python/src/identity.rs` (new) | `Identity` pyclass + helper `#[pyfunction]`s |
| `bindings/python/src/capabilities.rs` (new) | dict ↔ core conversion |
| `bindings/python/src/subnets.rs` (new) | `SubnetId` / `SubnetPolicy` conversion |
| `bindings/python/src/lib.rs` | wire new modules; add fields to `NetMesh.__new__`; extend `register_channel` / `subscribe_channel`; add `announce_capabilities` / `find_nodes`; add `entity_id` getter |
| `bindings/python/python/net/_net.pyi` | add stubs for `Identity`, new kwargs, exceptions |
| `bindings/python/python/net/__init__.py` | re-export `Identity`, `TokenError`, `IdentityError` |
| `bindings/python/tests/test_identity.py` (new) | F-1 tests |
| `bindings/python/tests/test_capabilities.py` (new) | F-2 tests |
| `bindings/python/tests/test_subnets.py` (new) | F-3 tests |
| `bindings/python/tests/test_channel_auth.py` (new) | F-4 tests |
| `bindings/python/README.md` or project README | Python security section |
| `docs/SDK_SECURITY_SURFACE_PLAN.md` | cross-link to this plan |

## Exit criteria

- Every binding shipped has a corresponding `_net.pyi` stub.
- `maturin develop --features "net,cortex"` builds clean.
- `pytest bindings/python/tests/` green on `test_identity.py`,
  `test_capabilities.py`, `test_subnets.py`, `test_channel_auth.py`
  + existing suites.
- `cargo clippy --features "net,cortex"` clean on the Python
  binding crate.
- Python README section explains the same Stage A–E story the
  Rust / TS READMEs already show, with runnable-from-pytest
  examples.
- No regression in any existing test suite (Rust integration,
  TS vitest).

## Explicit follow-ups (not in this plan)

- Async Python surface (`async def subscribe_channel` returning
  awaitable).
- Python SDK wrapper classes (today users import from
  `net.*` raw); decide later when user feedback demands it.
- Stub sync-check CI lint.
- mypy strict-mode compliance in generated stubs.
- Stage G (Go) mirror — lands after F stabilizes.
