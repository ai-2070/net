# Go parity plan — Stage G of the SDK security surface

## Context

[`SDK_SECURITY_SURFACE_PLAN.md`](SDK_SECURITY_SURFACE_PLAN.md) Stage G
is "Go surface (identity + capabilities + subnets + auth) — repeat
A–E against the C-ABI / cgo layer." Unlike Stage F (Python), this
**is** an iceberg: the Go bindings go through a C ABI in
`src/ffi/mesh.rs`, and that FFI layer has **zero** symbols for
identity, capabilities, subnets, or channel-auth beyond the
`require_token` boolean. Rust SDK + NAPI + PyO3 all talk directly to
in-process Rust; Go talks through `extern "C"`, so every new surface
means extending:

1. `src/ffi/mesh.rs` — Rust C-ABI exports (`#[unsafe(no_mangle)] pub extern "C" fn …`)
2. `bindings/go/net/net.h` — C declarations the cgo layer includes
3. `bindings/go/net/mesh.go` — Go wrappers + marshalling
4. `bindings/go/net/*_test.go` — test coverage

Current Go state (from a fresh survey of
`bindings/go/net/{net.h,mesh.go,mesh_channels_test.go}` and
`src/ffi/mesh.rs`):

| Surface | Status in Go today |
|---|---|
| `MeshNode` basics (new/shutdown/connect/accept/start/open_stream/publish) | ✅ Present |
| `net_generate_keypair` (Noise keypair) | ✅ Present — but ed25519 `Identity` is absent |
| Identity / `PermissionToken` / `TokenCache` | ❌ Zero FFI symbols |
| Capabilities (announce / find_nodes) | ❌ Zero FFI symbols |
| Subnets (`subnet`, `subnet_policy`) | ❌ Zero FFI symbols |
| Channel auth (`publish_caps` / `subscribe_caps` on register, token on subscribe) | ❌ `RequireToken` flag exists; ACL + token path unwired |
| `CapabilityAnnouncement.entity_id` + signing | ✅ Core emits signed announcements; unused by Go |
| `peer_entity_ids`, `TokenCache` on `MeshNode` | ✅ Core fields exist — Go doesn't install a token cache at all |
| Test harness | ✅ Go standard `testing.T`; `mesh_channels_test.go` has the handshake-pair idiom |

## Scope

**In scope (G-1 through G-5):**

- Greenfield C-ABI exports for ed25519 identity + token helpers (parse
  / verify / delegate / channel-hash / issue / install / lookup).
  Tokens cross the boundary as raw `uint8_t*` buffers (159 bytes);
  entity ids as `uint8_t*` (32 bytes).
- Greenfield C-ABI exports for capabilities (announce / find_nodes)
  and subnets (config extension on `net_mesh_new`). Capability sets,
  filters, and subnet policies cross as JSON strings — matches the
  existing ChannelConfigInput / PublishConfigInput pattern in
  `src/ffi/mesh.rs`. Binary tokens + entity ids stay byte-array to
  avoid hex round-trips.
- Extend `net_mesh_new` JSON config with `identity_seed_hex`,
  `capability_gc_interval_ms`, `require_signed_capabilities`,
  `subnet`, `subnet_policy`.
- Extend channel config JSON with `publish_caps`, `subscribe_caps`.
  Extend `net_mesh_subscribe_channel` with an optional `token` byte
  buffer (add a new `net_mesh_subscribe_channel_with_token` to keep
  the existing two-arg ABI stable).
- Add `net_mesh_entity_id` getter (32 bytes into a caller-provided
  buffer).
- Go wrapper types: `Identity`, `IdentityError`, `TokenError`,
  `CapabilitySet`/`CapabilityFilter` (plain structs with JSON tags —
  no pyclass equivalent), `SubnetID`/`SubnetPolicy`.
- Go error values: `ErrIdentity`, `ErrTokenInvalidFormat`,
  `ErrTokenInvalidSignature`, `ErrTokenExpired`,
  `ErrTokenNotYetValid`, `ErrTokenDelegationExhausted`,
  `ErrTokenDelegationNotAllowed`, `ErrTokenNotAuthorized`.
- Go tests: `identity_test.go`, `capabilities_test.go`,
  `subnets_test.go`, `channel_auth_test.go`.
- README Go section + cross-link from
  `SDK_SECURITY_SURFACE_PLAN.md`.

**Out of scope:**

- Async Go API (existing bindings are synchronous via `block_on`;
  preserve that).
- A Go-level "SDK wrapper" on top of the raw `*C.net_…_t` handles.
  Users import `net.*` today.
- Revocation / HSM / dynamic policies (deferred same as Rust SDK).

## Design

### Error codes

Extend the existing -110..-116 range with a dedicated -120..-129
block for identity + tokens. Matches the pattern in
`ffi/mesh.rs` (mesh errors) / `ffi/cortex.rs` (cortex errors).

```rust
pub(crate) const NET_ERR_IDENTITY: c_int = -120;
pub(crate) const NET_ERR_TOKEN_INVALID_FORMAT: c_int = -121;
pub(crate) const NET_ERR_TOKEN_INVALID_SIGNATURE: c_int = -122;
pub(crate) const NET_ERR_TOKEN_EXPIRED: c_int = -123;
pub(crate) const NET_ERR_TOKEN_NOT_YET_VALID: c_int = -124;
pub(crate) const NET_ERR_TOKEN_DELEGATION_EXHAUSTED: c_int = -125;
pub(crate) const NET_ERR_TOKEN_DELEGATION_NOT_ALLOWED: c_int = -126;
pub(crate) const NET_ERR_TOKEN_NOT_AUTHORIZED: c_int = -127;
pub(crate) const NET_ERR_CAPABILITY: c_int = -128;
```

Go side: sentinel errors in `net.go` (or new `identity.go`) +
`meshErrorFromCode` extension.

### Identity across the C boundary

`Identity` is a heap handle behind a `*mut IdentityHandle` — matches
`MeshNodeHandle` layout. Cheap to clone (internal `Arc`s). Serialized
form is the 32-byte seed (same as TS / Python / PyO3 `to_bytes`).

```c
typedef struct net_identity_t net_identity_t;

int32_t net_identity_generate(net_identity_t** out_handle);
int32_t net_identity_from_seed(const uint8_t* seed, size_t seed_len,
                               net_identity_t** out_handle);
void    net_identity_free(net_identity_t* handle);

// Writes the 32-byte seed into out_seed[32].
int32_t net_identity_to_seed(net_identity_t* handle, uint8_t* out_seed);

// Writes the 32-byte entity id into out[32].
int32_t net_identity_entity_id(net_identity_t* handle, uint8_t* out);

uint64_t net_identity_node_id(net_identity_t* handle);
uint32_t net_identity_origin_hash(net_identity_t* handle);

// Signs `msg[len]`; writes 64-byte ed25519 signature into out[64].
int32_t net_identity_sign(net_identity_t* handle,
                          const uint8_t* msg, size_t len,
                          uint8_t* out_sig);
```

Tokens as raw byte buffers. Rust writes them through
`net_alloc_bytes` and frees via `net_free_bytes` (new pair — needed
because `net_free_string` assumes a NUL-terminated CString).

```c
// Rust-side alloc/free pair for opaque byte buffers.
void net_free_bytes(uint8_t* ptr, size_t len);

// Issue: writes a newly-allocated token blob; caller must net_free_bytes.
int32_t net_identity_issue_token(
    net_identity_t* signer,
    const uint8_t* subject,     // 32 bytes
    const char* scope_json,     // JSON list: ["publish","subscribe",...]
    const char* channel,        // channel name (not hash)
    uint32_t ttl_seconds,
    uint8_t delegation_depth,
    uint8_t** out_token,
    size_t* out_token_len
);

int32_t net_identity_install_token(
    net_identity_t* handle,
    const uint8_t* token, size_t len
);

// Writes an allocated blob; out_token==NULL && *out_len==0 on miss.
int32_t net_identity_lookup_token(
    net_identity_t* handle,
    const uint8_t* subject,     // 32 bytes
    const char* channel,
    uint8_t** out_token,
    size_t* out_token_len
);

uint32_t net_identity_token_cache_len(net_identity_t* handle);
```

Parse / verify / delegate as module-level functions. Parse returns a
JSON dict (same as PyO3 `parse_token`).

```c
// Writes a JSON string (see PyO3 parse_token fields).
int32_t net_parse_token(const uint8_t* token, size_t len,
                        char** out_json, size_t* out_len);

int32_t net_verify_token(const uint8_t* token, size_t len,
                         int32_t* out_ok);  // 1 = valid, 0 = tampered

int32_t net_token_is_expired(const uint8_t* token, size_t len,
                             int32_t* out_expired);

int32_t net_delegate_token(
    net_identity_t* signer,
    const uint8_t* parent, size_t parent_len,
    const uint8_t* new_subject,
    const char* restricted_scope_json,
    uint8_t** out_token, size_t* out_token_len
);

int32_t net_channel_hash(const char* channel, uint16_t* out_hash);
```

### Capabilities across the C boundary

JSON in, JSON / `u64` list out. Dict shape matches PyO3 / NAPI
byte-for-byte so the same test fixtures work across all three
bindings.

```c
int32_t net_mesh_announce_capabilities(
    net_meshnode_t* handle,
    const char* caps_json
);

int32_t net_mesh_find_nodes(
    net_meshnode_t* handle,
    const char* filter_json,
    char** out_json, size_t* out_len     // JSON: [nodeid, ...]
);

// Convenience: returns "nvidia" | "amd" | ... | "unknown".
int32_t net_normalize_gpu_vendor(
    const char* raw,
    char** out_str, size_t* out_len
);
```

### Subnets across the C boundary

`subnet` + `subnet_policy` attach to the `MeshNewConfig` JSON. No new
ABI symbols.

```rust
#[derive(Deserialize)]
struct MeshNewConfig {
    // … existing fields …
    identity_seed_hex: Option<String>,       // 64 hex chars
    capability_gc_interval_ms: Option<u64>,
    require_signed_capabilities: Option<bool>,
    subnet: Option<Vec<u8>>,                 // 1–4 entries
    subnet_policy: Option<SubnetPolicyJson>, // same shape as PyO3
}
```

### Channel auth

`publish_caps` / `subscribe_caps` attach to the
`ChannelConfigInput` JSON — no new ABI symbol, just extra fields.

`subscribe_channel_with_token` is a *new* symbol that takes a raw
token buffer; the existing `subscribe_channel` keeps its two-arg
signature so old callers don't break.

```c
int32_t net_mesh_subscribe_channel_with_token(
    net_meshnode_t* handle,
    uint64_t publisher_node_id,
    const char* channel,
    const uint8_t* token, size_t token_len
);
```

### `net_mesh_entity_id`

```c
int32_t net_mesh_entity_id(net_meshnode_t* handle, uint8_t* out_32);
```

Matches the TS / Python `entityId` / `entity_id` getter introduced in
Stage E-5. Bytes are written into a caller-provided 32-byte buffer
rather than returned through an alloc-then-free dance.

### Go layer layout

New files:

- `bindings/go/net/identity.go` — `Identity` struct + methods +
  free functions + token errors
- `bindings/go/net/capabilities.go` — `CapabilitySet` / `CapabilityFilter` types
  + `(m *MeshNode) AnnounceCapabilities` / `FindNodes`
- `bindings/go/net/subnets.go` — `SubnetID` / `SubnetPolicy` types

`mesh.go` gets:

- Extended `MeshConfig` struct (identity_seed_hex, capability_gc_interval_ms, require_signed_capabilities, subnet, subnet_policy)
- Extended `ChannelConfig` (publish_caps, subscribe_caps)
- New `(m *MeshNode) SubscribeChannelWithToken`
- New `(m *MeshNode) EntityID() [32]byte`

## Staged rollout

Five PRs, mirroring the original stage split and the F plan's
substage shape:

| Stage | What | Days |
|---|---|---|
| **G-1** | `src/ffi/mesh.rs` identity exports (generate / from_seed / entity_id / node_id / origin_hash / sign / to_seed / free). Token helpers (parse / verify / token_is_expired / delegate / channel_hash / issue / install / lookup / token_cache_len). New `net_free_bytes`. C header. Go `identity.go`. `identity_test.go` mirroring `test_identity.py`. | 2 |
| **G-2** | `src/ffi/mesh.rs` capability exports. `MeshConfig` extended with `capability_gc_interval_ms` + `require_signed_capabilities`. Go `capabilities.go`. `capabilities_test.go`. | 1 |
| **G-3** | `src/ffi/mesh.rs` subnet support in `MeshNewConfig`. Go `subnets.go`. `subnets_test.go`. | 1 |
| **G-4** | `src/ffi/mesh.rs` channel auth: `publish_caps` / `subscribe_caps` in `ChannelConfigInput`, new `net_mesh_subscribe_channel_with_token`, new `net_mesh_entity_id`. `identity_seed_hex` in `MeshNewConfig`. Go wrappers. `channel_auth_test.go`. | 2 |
| **G-5** | Go README section. Cross-link from `SDK_SECURITY_SURFACE_PLAN.md`. Example update (`bindings/go/example/main.go` if it's small enough to extend; otherwise leave alone). | 0.5 |

**Total ~6.5 days** — matching the plan's 2-week upper bound with
buffer for the iceberg (new alloc/free pair, extended JSON config
validation).

## Test plan

`bindings/go/net/identity_test.go` (new, G-1) — direct Go port of
`test_identity.py`. Same 22 assertions.

`bindings/go/net/capabilities_test.go` (new, G-2) — single-mesh
self-match:

1. `AnnounceCapabilities({Tags: []string{"gpu"}})` → `FindNodes({RequireTags: []string{"gpu"}})` includes own node id.
2. Non-matching filter returns empty slice.
3. `NormalizeGpuVendor("NVIDIA") == "nvidia"`.
4. Wrong-shape filter input returns `ErrCapability`.

`bindings/go/net/subnets_test.go` (new, G-3) — mesh construction with
`subnet=[]uint8{3, 7, 2}` + `subnet_policy` returns no error;
out-of-range rejected.

`bindings/go/net/channel_auth_test.go` (new, G-4) — single-mesh
(matches `test_channel_auth.py` discipline):

1. `RegisterChannel` accepts `PublishCaps` / `SubscribeCaps`.
2. Publish denied by own `PublishCaps` mismatch.
3. `SubscribeChannelWithToken(..., malformedBytes)` returns
   `ErrTokenInvalidFormat`.
4. Well-formed token reaches transport → `ErrChannel` (no peer) not
   `ErrToken*`.

Multi-mesh token round-trip coverage stays in the Rust integration
suite (`tests/channel_auth.rs`) and the TS SDK until a Go handshake
fixture with port pinning lands.

## Risks

- **JSON shape drift across SDKs**: five wire contracts to keep in
  sync. Mitigation — share the dict field names byte-for-byte (no
  aliases, no casing flips), rely on the fact that Rust SDK's
  `CapabilitySet` / `CapabilityFilter` / `SubnetPolicy` are the
  ground truth.
- **Go test process crashes on FFI misuse**: unlike PyO3's PyErr, a
  bad `unsafe { &*ptr }` takes the whole process down. Mitigation —
  always null-check in the Rust layer and return `NET_ERR_NULL_POINTER`;
  don't rely on Go-side `if handle == nil` guards alone.
- **cdylib build time**: adding ~25 new `extern "C"` symbols grows
  the cdylib slightly; no meaningful perf risk, but Go `go test`
  picks up the cdylib link time on every invocation. Accept as-is.
- **Memory ownership for byte buffers**: the new `net_alloc_bytes` /
  `net_free_bytes` pair is the first time we hand the Go caller a
  non-CString heap allocation. Must be used *only* by symbols
  advertised as returning "newly-allocated byte buffer caller must
  free via `net_free_bytes`." Every new symbol documented inline in
  the C header.
- **Finalizer interaction with token cache**: `Identity` clones the
  cache on copy (Arc). The Go finalizer frees the handle, which drops
  the last Arc and collects the cache — fine, as long as no other Go
  value holds a raw pointer into the cache. We keep tokens as owned
  `[]byte` on the Go side, never raw pointers.

## Files touched (estimate)

| File | Why |
|---|---|
| `src/ffi/mesh.rs` | ~25 new `#[unsafe(no_mangle)]` exports + JSON config extensions |
| `src/ffi/mod.rs` | New error codes; register `net_free_bytes` |
| `bindings/go/net/net.h` | Mirror every new FFI symbol |
| `bindings/go/net/net.go` | `net_free_bytes` binding, error code mapping |
| `bindings/go/net/mesh.go` | `MeshConfig` / `ChannelConfig` extensions, `EntityID`, `SubscribeChannelWithToken` |
| `bindings/go/net/identity.go` (new) | `Identity` wrapper |
| `bindings/go/net/capabilities.go` (new) | `CapabilitySet` / `CapabilityFilter` + mesh methods |
| `bindings/go/net/subnets.go` (new) | `SubnetID` / `SubnetPolicy` types |
| `bindings/go/net/{identity,capabilities,subnets,channel_auth}_test.go` (new) | G-1..G-4 tests |
| `bindings/go/README.md` | Security Surface section |
| `docs/SDK_SECURITY_SURFACE_PLAN.md` | Cross-link to this plan |

## Exit criteria

- `cargo build --features "net,cortex"` links clean with new FFI symbols.
- `cargo clippy --features "net,cortex"` clean on the `net` crate.
- `go test ./bindings/go/net/...` green on every new file + no
  regressions on existing suites.
- Every new C symbol documented inline (doc-comment + ownership note
  in `net.h`).
- Go README section explains the same Stage A–E story the Rust / TS /
  Python READMEs already show, with runnable Go examples.
- No change to NAPI / PyO3 / TS output (Stage G is additive on FFI).

## Explicit follow-ups (not in this plan)

- Go "SDK wrapper" types on top of the raw `*C.net_identity_t`
  handles.
- Async Go surface (`chan`-based publish/subscribe).
- `go vet` / `gofumpt` / CI integration for the new files.
- Example update showing token-gated subscribe end-to-end.
