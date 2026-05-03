//! C FFI bindings for the encrypted-UDP mesh transport.
//!
//! Surface targeted at the Go SDK. Mirrors the Rust SDK's `Mesh`
//! type (not the full core `MeshNode`) — just the common path:
//! handshake, per-peer streams, channels, shard receive.
//!
//! Everything crosses the boundary as:
//!
//! - Opaque handles (`*mut T`) freed via dedicated `_free` functions.
//! - Scalar ids as `u64`.
//! - Everything else as JSON strings allocated with
//!   `CString::into_raw`, freed by the caller via `net_free_string`.
//!
//! Handshake + per-peer sends are async on the core side; the FFI
//! drives them via a shared `tokio::runtime::Runtime` (lazy OnceLock)
//! identical to the one used by `ffi/cortex.rs`.

use std::ffi::{c_char, c_int, CStr, CString};
use std::sync::Arc;

use bytes::Bytes;
use serde::{Deserialize, Serialize};
use tokio::runtime::Runtime;

use crate::adapter::net::identity::{
    EntityId, PermissionToken, TokenCache, TokenError as CoreTokenError, TokenScope,
};
use crate::adapter::net::{
    ChannelConfig as InnerChannelConfig, ChannelConfigRegistry, ChannelId,
    ChannelName as InnerChannelName, ChannelPublisher, EntityKeypair, MeshNode, MeshNodeConfig,
    OnFailure as InnerOnFailure, PublishConfig as InnerPublishConfig,
    PublishReport as InnerPublishReport, Reliability, Stream as CoreStream, StreamConfig,
    StreamError, Visibility as InnerVisibility, DEFAULT_STREAM_WINDOW_BYTES,
};
use crate::adapter::net::{SubnetId, SubnetPolicy, SubnetRule};
use crate::adapter::Adapter;
use crate::error::AdapterError;

use super::NetError;

// =========================================================================
// Mesh-specific error codes. Continues the -100..-99 range used by
// `ffi/cortex.rs`. The Go layer maps these to typed sentinels.
// =========================================================================

pub(crate) const NET_ERR_MESH_INIT: c_int = -110;
pub(crate) const NET_ERR_MESH_HANDSHAKE: c_int = -111;
pub(crate) const NET_ERR_MESH_BACKPRESSURE: c_int = -112;
pub(crate) const NET_ERR_MESH_NOT_CONNECTED: c_int = -113;
pub(crate) const NET_ERR_MESH_TRANSPORT: c_int = -114;
pub(crate) const NET_ERR_CHANNEL: c_int = -115;
pub(crate) const NET_ERR_CHANNEL_AUTH: c_int = -116;

// Identity + token error codes. Block -120..-129 mirrors the
// `"identity: ..."` / `"token: <kind>"` prefix convention used by
// PyO3 and NAPI; each `kind` gets its own integer so Go callers can
// `errors.Is(err, net.ErrTokenExpired)` without parsing strings.
pub(crate) const NET_ERR_IDENTITY: c_int = -120;
pub(crate) const NET_ERR_TOKEN_INVALID_FORMAT: c_int = -121;
pub(crate) const NET_ERR_TOKEN_INVALID_SIGNATURE: c_int = -122;
pub(crate) const NET_ERR_TOKEN_EXPIRED: c_int = -123;
pub(crate) const NET_ERR_TOKEN_NOT_YET_VALID: c_int = -124;
pub(crate) const NET_ERR_TOKEN_DELEGATION_EXHAUSTED: c_int = -125;
pub(crate) const NET_ERR_TOKEN_DELEGATION_NOT_ALLOWED: c_int = -126;
pub(crate) const NET_ERR_TOKEN_NOT_AUTHORIZED: c_int = -127;

// NAT-traversal error codes. Block -130..-139 — one integer per
// `TraversalError::kind()` so Go callers can
// `errors.Is(err, net.ErrTraversalPunchFailed)` without parsing
// strings, matching the token-error pattern above. Framing (plan
// §5): every `TraversalError` represents a missed *optimization*,
// not a connectivity failure — the routed-handshake path is
// always available. See `TraversalError` docs for per-variant
// semantics.
// Per-variant traversal error codes. Gated on the feature
// because they're only referenced by `traversal_err_to_code`,
// which only compiles with the feature on. `NET_ERR_TRAVERSAL_UNSUPPORTED`
// below is unconditional — the no-feature stubs need it.
#[cfg(feature = "nat-traversal")]
pub(crate) const NET_ERR_TRAVERSAL_REFLEX_TIMEOUT: c_int = -130;
#[cfg(feature = "nat-traversal")]
pub(crate) const NET_ERR_TRAVERSAL_PEER_NOT_REACHABLE: c_int = -131;
#[cfg(feature = "nat-traversal")]
pub(crate) const NET_ERR_TRAVERSAL_TRANSPORT: c_int = -132;
#[cfg(feature = "nat-traversal")]
pub(crate) const NET_ERR_TRAVERSAL_RENDEZVOUS_NO_RELAY: c_int = -133;
#[cfg(feature = "nat-traversal")]
pub(crate) const NET_ERR_TRAVERSAL_RENDEZVOUS_REJECTED: c_int = -134;
#[cfg(feature = "nat-traversal")]
pub(crate) const NET_ERR_TRAVERSAL_PUNCH_FAILED: c_int = -135;
#[cfg(feature = "nat-traversal")]
pub(crate) const NET_ERR_TRAVERSAL_PORT_MAP_UNAVAILABLE: c_int = -136;
// Unconditional — the `#[cfg(not(feature = "nat-traversal"))]`
// FFI stubs below return this so the Go / NAPI / PyO3 bindings
// surface `ErrTraversalUnsupported` when built against a cdylib
// without the feature, rather than failing at dlopen with a
// missing-symbol error.
pub(crate) const NET_ERR_TRAVERSAL_UNSUPPORTED: c_int = -137;

#[cfg(feature = "nat-traversal")]
fn traversal_err_to_code(e: &crate::adapter::net::traversal::TraversalError) -> c_int {
    use crate::adapter::net::traversal::TraversalError;
    match e {
        TraversalError::ReflexTimeout => NET_ERR_TRAVERSAL_REFLEX_TIMEOUT,
        TraversalError::PeerNotReachable => NET_ERR_TRAVERSAL_PEER_NOT_REACHABLE,
        TraversalError::Transport(_) => NET_ERR_TRAVERSAL_TRANSPORT,
        TraversalError::RendezvousNoRelay => NET_ERR_TRAVERSAL_RENDEZVOUS_NO_RELAY,
        TraversalError::RendezvousRejected(_) => NET_ERR_TRAVERSAL_RENDEZVOUS_REJECTED,
        TraversalError::PunchFailed => NET_ERR_TRAVERSAL_PUNCH_FAILED,
        TraversalError::PortMapUnavailable => NET_ERR_TRAVERSAL_PORT_MAP_UNAVAILABLE,
        TraversalError::Unsupported => NET_ERR_TRAVERSAL_UNSUPPORTED,
    }
}

/// Stable string form of a `NatClass`. Same vocabulary as the
/// NAPI / PyO3 bindings — callers branch on
/// `"open" | "cone" | "symmetric" | "unknown"`.
#[cfg(feature = "nat-traversal")]
fn nat_class_to_str(class: crate::adapter::net::traversal::classify::NatClass) -> &'static str {
    use crate::adapter::net::traversal::classify::NatClass;
    match class {
        NatClass::Open => "open",
        NatClass::Cone => "cone",
        NatClass::Symmetric => "symmetric",
        NatClass::Unknown => "unknown",
    }
}

fn token_err_to_code(e: &CoreTokenError) -> c_int {
    match e {
        CoreTokenError::InvalidFormat => NET_ERR_TOKEN_INVALID_FORMAT,
        CoreTokenError::InvalidSignature => NET_ERR_TOKEN_INVALID_SIGNATURE,
        CoreTokenError::Expired => NET_ERR_TOKEN_EXPIRED,
        CoreTokenError::NotYetValid => NET_ERR_TOKEN_NOT_YET_VALID,
        CoreTokenError::DelegationExhausted => NET_ERR_TOKEN_DELEGATION_EXHAUSTED,
        CoreTokenError::DelegationNotAllowed => NET_ERR_TOKEN_DELEGATION_NOT_ALLOWED,
        CoreTokenError::NotAuthorized => NET_ERR_TOKEN_NOT_AUTHORIZED,
        // Maps to `NET_ERR_IDENTITY` since a public-only keypair
        // is fundamentally an identity-availability issue, not a
        // token-content issue. The error message in `Display`
        // makes the cause clear to the caller.
        CoreTokenError::ReadOnly => NET_ERR_IDENTITY,
        // A zero-TTL request is a malformed token-issue
        // input. Routes to `NET_ERR_TOKEN_INVALID_FORMAT` (the
        // closest existing semantic — invalid input shape) so
        // the C/Go header surface stays unchanged. The Display
        // message ("token TTL must be > 0 seconds") tells the
        // caller exactly what was wrong.
        CoreTokenError::ZeroTtl => NET_ERR_TOKEN_INVALID_FORMAT,
    }
}

// =========================================================================
// Shared utilities
// =========================================================================

/// Shared tokio runtime. One per process, lazy-initialized.
///
/// On `tokio::Builder::build()` failure (worker-thread
/// `pthread_create` failure under `RLIMIT_NPROC` / container
/// limits / memory pressure) we `eprintln! + std::process::abort()`
/// rather than panic. `abort` is `extern "C"`-safe (terminates
/// rather than unwinds), so the failure cannot escape across the
/// surrounding `extern "C"` FFI frame into C / Go-cgo / NAPI /
/// PyO3 callers — that would be undefined behaviour. A daemon
/// that can't construct its async runtime is dead in the water,
/// so termination is the appropriate response.
fn runtime() -> &'static Arc<Runtime> {
    use std::sync::OnceLock;
    static RT: OnceLock<Arc<Runtime>> = OnceLock::new();
    RT.get_or_init(|| {
        match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => Arc::new(rt),
            Err(e) => {
                eprintln!(
                    "FATAL: mesh FFI tokio runtime build failure ({e:?}); aborting to avoid panic across the FFI boundary"
                );
                std::process::abort();
            }
        }
    })
}

/// `block_on(...)` wrapper that aborts on runtime-in-runtime
/// rather than panicking across the FFI boundary.
///
/// Calling `Runtime::block_on` from a thread that already holds a
/// tokio runtime context panics with "Cannot start a runtime from
/// within a runtime". The cortex / mesh FFI functions are
/// `extern "C"`, so the panic would unwind across cgo / N-API / cffi
/// — undefined behavior. The check costs one TLS lookup
/// (`Handle::try_current`) per FFI call, which is negligible against
/// the work the FFI is about to do (network I/O, JSON parsing,
/// channel operations). Common-case callers (C / Go / Python without
/// an embedding Rust runtime) hit the fast path; embedded-Rust
/// callers who violate the contract get a clean abort with a
/// diagnosable message instead of UB.
fn block_on<F: std::future::Future>(future: F) -> F::Output {
    if tokio::runtime::Handle::try_current().is_ok() {
        eprintln!(
            "FATAL: mesh FFI called from inside a tokio runtime context; \
             aborting to avoid runtime-in-runtime panic across the FFI boundary"
        );
        std::process::abort();
    }
    runtime().block_on(future)
}

/// The output borrow's lifetime is tied (via Rust's elision rules)
/// to the input reference's lifetime, so the caller cannot pick
/// `'static` and produce a dangling borrow. The borrow lives only
/// as long as the local stack frame holding the pointer — which is
/// the caller's responsibility to keep valid for the duration of
/// any resulting `&str` use, but no longer. Compare
/// `cortex.rs::c_str_to_owned` which sidesteps the issue entirely
/// by returning `Option<String>`.
///
/// Returns an OWNED `String` (not a borrowed `&str` tied to the C
/// buffer). The previous `Option<&str>` signature was a soundness
/// trap: lifetime elision on `&*const c_char` bound the returned
/// `&str` to the local pointer reference's stack slot rather than
/// to the underlying C buffer, so a future refactor that moved the
/// result into `tokio::spawn(async move { ... })` would compile
/// silently and hand a dangling pointer to the spawned task. The
/// owned-`String` shape removes the hazard at the cost of one
/// allocation per call, which is acceptable on FFI entry paths.
///
/// # Safety
/// Caller must ensure `p` is null or points to a NUL-terminated C
/// string valid at least until this function returns.
#[inline]
unsafe fn c_str_to_string(p: *const c_char) -> Option<String> {
    if p.is_null() {
        return None;
    }
    CStr::from_ptr(p).to_str().ok().map(str::to_owned)
}

/// Null-check `out_ptr` and `out_len` before writing through them.
/// The helper is callable from any FFI boundary; a future caller
/// forgetting to check produced UB (write through null). Returns
/// `NetError::NullPointer` so the FFI caller can distinguish "I
/// forgot to provide outputs" from "the operation failed."
fn write_json_out<T: Serialize>(
    value: &T,
    out_ptr: *mut *mut c_char,
    out_len: *mut usize,
) -> c_int {
    if out_ptr.is_null() || out_len.is_null() {
        return NetError::NullPointer.into();
    }
    let Ok(s) = serde_json::to_string(value) else {
        return NetError::Unknown.into();
    };
    let len = s.len();
    let Ok(cs) = CString::new(s) else {
        return NetError::Unknown.into();
    };
    unsafe {
        *out_ptr = cs.into_raw();
        *out_len = len;
    }
    0
}

fn write_string_out(s: String, out_ptr: *mut *mut c_char, out_len: *mut usize) -> c_int {
    if out_ptr.is_null() || out_len.is_null() {
        return NetError::NullPointer.into();
    }
    let len = s.len();
    let Ok(cs) = CString::new(s) else {
        return NetError::Unknown.into();
    };
    unsafe {
        *out_ptr = cs.into_raw();
        *out_len = len;
    }
    0
}

fn adapter_err_to_code(err: &AdapterError) -> c_int {
    match err {
        AdapterError::Connection(_) => NET_ERR_MESH_HANDSHAKE,
        _ => NET_ERR_MESH_TRANSPORT,
    }
}

fn stream_err_to_code(err: &StreamError) -> c_int {
    match err {
        StreamError::Backpressure => NET_ERR_MESH_BACKPRESSURE,
        StreamError::NotConnected => NET_ERR_MESH_NOT_CONNECTED,
        StreamError::Transport(_) => NET_ERR_MESH_TRANSPORT,
    }
}

// =========================================================================
// MeshNode
// =========================================================================

#[derive(Deserialize)]
struct SubnetPolicyJson {
    #[serde(default)]
    rules: Vec<SubnetRuleJson>,
}

#[derive(Deserialize)]
struct SubnetRuleJson {
    tag_prefix: String,
    level: u32,
    #[serde(default)]
    values: std::collections::HashMap<String, u32>,
}

fn u8_from_u32(value: u32) -> Option<u8> {
    if value > 255 {
        None
    } else {
        Some(value as u8)
    }
}

fn subnet_id_from_json(levels: Vec<u32>) -> Option<SubnetId> {
    if levels.is_empty() || levels.len() > 4 {
        return None;
    }
    let mut bytes = [0u8; 4];
    for (i, raw) in levels.iter().enumerate() {
        bytes[i] = u8_from_u32(*raw)?;
    }
    Some(SubnetId::new(&bytes[..levels.len()]))
}

fn subnet_policy_from_json(p: SubnetPolicyJson) -> Option<SubnetPolicy> {
    let mut policy = SubnetPolicy::new();
    for rule_json in p.rules {
        let level = u8_from_u32(rule_json.level)?;
        if level > 3 {
            return None;
        }
        let mut rule = SubnetRule::new(rule_json.tag_prefix, level);
        for (tag_value, raw_val) in rule_json.values {
            let v = u8_from_u32(raw_val)?;
            // `SubnetRule::map` panics when `v == 0` — zero is
            // reserved by the core as "unmatched / no restriction"
            // and must not appear as an explicit mapping. Reject
            // at the FFI boundary so Go callers surface a clean
            // `NET_ERR_MESH_INIT` instead of a cdylib abort.
            if v == 0 {
                return None;
            }
            rule = rule.map(tag_value, v);
        }
        policy = policy.add_rule(rule);
    }
    Some(policy)
}

#[derive(Deserialize)]
struct MeshNewConfig {
    bind_addr: String,
    /// Hex-encoded 32-byte pre-shared key.
    psk_hex: String,
    heartbeat_ms: Option<u64>,
    session_timeout_ms: Option<u64>,
    num_shards: Option<u16>,
    /// Capability GC interval (ms). Drives eviction of stale
    /// capability index entries.
    capability_gc_interval_ms: Option<u64>,
    /// Reject unsigned capability announcements when `true`.
    /// Defaults to the core's default (`false` in v1).
    require_signed_capabilities: Option<bool>,
    /// 1–4 bytes, each 0–255. Leave unset for `SubnetId::GLOBAL`.
    subnet: Option<Vec<u32>>,
    /// Optional `{"rules": [{"tag_prefix", "level", "values"}]}` policy.
    subnet_policy: Option<SubnetPolicyJson>,
    /// Hex-encoded 32-byte ed25519 seed — when present, the mesh
    /// reproduces the same `entity_id` as
    /// `IdentityFromSeed(sameSeed)`. Leave unset to generate a fresh
    /// keypair.
    identity_seed_hex: Option<String>,
    /// Pin this mesh's publicly-advertised reflex address (an
    /// `"ip:port"` string). Classification is skipped; the node
    /// starts in `nat:open` with this address on its capability
    /// announcements. Silently ignored when the cdylib is built
    /// without `--features nat-traversal`.
    #[serde(default)]
    reflex_override: Option<String>,
    /// Opt into opportunistic UPnP / NAT-PMP / PCP port mapping
    /// at startup. Silently ignored when the cdylib is built
    /// without `--features port-mapping`.
    #[serde(default)]
    try_port_mapping: bool,
}

pub struct MeshNodeHandle {
    inner: Arc<MeshNode>,
    channel_configs: Arc<ChannelConfigRegistry>,
}

/// Create a new mesh node. `config_json` is:
///
/// ```json
/// {
///   "bind_addr": "127.0.0.1:9000",
///   "psk_hex":   "42424242...",   // 64 hex chars
///   "heartbeat_ms": 5000,
///   "session_timeout_ms": 30000,
///   "num_shards": 4
/// }
/// ```
///
/// Installs an empty `ChannelConfigRegistry` at creation time so
/// `net_mesh_register_channel` can insert without a mutable ref.
#[unsafe(no_mangle)]
pub extern "C" fn net_mesh_new(
    config_json: *const c_char,
    out_handle: *mut *mut MeshNodeHandle,
) -> c_int {
    if config_json.is_null() || out_handle.is_null() {
        return NetError::NullPointer.into();
    }
    let Some(s) = (unsafe { c_str_to_string(config_json) }) else {
        return NetError::InvalidUtf8.into();
    };
    let cfg: MeshNewConfig = match serde_json::from_str(&s) {
        Ok(v) => v,
        Err(_) => return NetError::InvalidJson.into(),
    };
    let bind_addr: std::net::SocketAddr = match cfg.bind_addr.parse() {
        Ok(a) => a,
        Err(_) => return NET_ERR_MESH_INIT,
    };
    let psk_bytes = match hex::decode(&cfg.psk_hex) {
        Ok(b) => b,
        Err(_) => return NET_ERR_MESH_INIT,
    };
    if psk_bytes.len() != 32 {
        return NET_ERR_MESH_INIT;
    }
    let mut psk = [0u8; 32];
    psk.copy_from_slice(&psk_bytes);

    let mut node_cfg = MeshNodeConfig::new(bind_addr, psk);
    // Reject `0` for `heartbeat_ms` and `session_timeout_ms`.
    // A zero heartbeat interval busy-loops the heartbeat task
    // (saturating a CPU); a zero session timeout makes every
    // session expire instantly. The Rust-side configs do their
    // own validation but the FFI JSON path bypasses that — pin
    // the guard here so a misconfig fails fast rather than
    // producing a hung daemon.
    if let Some(ms) = cfg.heartbeat_ms {
        if ms == 0 {
            return NetError::InvalidJson.into();
        }
        node_cfg = node_cfg.with_heartbeat_interval(std::time::Duration::from_millis(ms));
    }
    if let Some(ms) = cfg.session_timeout_ms {
        if ms == 0 {
            return NetError::InvalidJson.into();
        }
        node_cfg = node_cfg.with_session_timeout(std::time::Duration::from_millis(ms));
    }
    if let Some(n) = cfg.num_shards {
        node_cfg = node_cfg.with_num_shards(n);
    }
    if let Some(ms) = cfg.capability_gc_interval_ms {
        node_cfg = node_cfg.with_capability_gc_interval(std::time::Duration::from_millis(ms));
    }
    if let Some(b) = cfg.require_signed_capabilities {
        node_cfg = node_cfg.with_require_signed_capabilities(b);
    }
    if let Some(levels) = cfg.subnet {
        let Some(id) = subnet_id_from_json(levels) else {
            return NET_ERR_MESH_INIT;
        };
        node_cfg = node_cfg.with_subnet(id);
    }
    if let Some(policy_js) = cfg.subnet_policy {
        let Some(policy) = subnet_policy_from_json(policy_js) else {
            return NET_ERR_MESH_INIT;
        };
        node_cfg = node_cfg.with_subnet_policy(Arc::new(policy));
    }
    #[cfg(feature = "nat-traversal")]
    if let Some(external_str) = cfg.reflex_override.as_deref() {
        let Ok(external) = external_str.parse::<std::net::SocketAddr>() else {
            return NET_ERR_MESH_INIT;
        };
        node_cfg = node_cfg.with_reflex_override(external);
    }
    // Silently drop the field in builds without nat-traversal so
    // Go callers compiled against a full-feature cdylib can fall
    // back to a thin cdylib without a JSON-parse error.
    #[cfg(not(feature = "nat-traversal"))]
    let _ = cfg.reflex_override;
    #[cfg(feature = "port-mapping")]
    if cfg.try_port_mapping {
        node_cfg = node_cfg.with_try_port_mapping(true);
    }
    // Same drop-on-the-floor pattern as reflex_override above.
    #[cfg(not(feature = "port-mapping"))]
    let _ = cfg.try_port_mapping;

    let identity = match cfg.identity_seed_hex {
        Some(seed_hex) => {
            let bytes = match hex::decode(&seed_hex) {
                Ok(b) => b,
                Err(_) => return NET_ERR_MESH_INIT,
            };
            if bytes.len() != 32 {
                return NET_ERR_MESH_INIT;
            }
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&bytes);
            EntityKeypair::from_bytes(arr)
        }
        None => EntityKeypair::generate(),
    };
    let result = block_on(async move { MeshNode::new(identity, node_cfg).await });
    match result {
        Ok(mut node) => {
            let channel_configs = Arc::new(ChannelConfigRegistry::new());
            node.set_channel_configs(channel_configs.clone());
            // Install a fresh TokenCache — channel auth needs
            // somewhere to stash tokens presented on subscribe.
            // Matches the PyO3 / NAPI behaviour.
            node.set_token_cache(Arc::new(TokenCache::new()));
            let handle = Box::new(MeshNodeHandle {
                inner: Arc::new(node),
                channel_configs,
            });
            unsafe {
                *out_handle = Box::into_raw(handle);
            }
            0
        }
        Err(_) => NET_ERR_MESH_INIT,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn net_mesh_free(handle: *mut MeshNodeHandle) {
    if handle.is_null() {
        return;
    }
    unsafe {
        drop(Box::from_raw(handle));
    }
}

/// Clone the `Arc<MeshNode>` backing this handle and return a
/// `*mut Arc<MeshNode>`. Used by the compute-FFI crate so the
/// Go binding's `DaemonRuntime` can share the live mesh node
/// without opening a second socket.
///
/// Caller takes ownership of the returned pointer and MUST free it
/// with [`net_mesh_arc_free`]. Returns NULL if `handle` is NULL.
#[unsafe(no_mangle)]
pub extern "C" fn net_mesh_arc_clone(handle: *mut MeshNodeHandle) -> *mut Arc<MeshNode> {
    if handle.is_null() {
        return std::ptr::null_mut();
    }
    let h = unsafe { &*handle };
    Box::into_raw(Box::new(h.inner.clone()))
}

/// Clone the shared `Arc<ChannelConfigRegistry>` backing this
/// handle. Used by compute-FFI so migration-triggered channel
/// rebind replays hit the same registry the mesh publishes to.
///
/// Caller takes ownership and MUST free with
/// [`net_mesh_channel_configs_arc_free`].
#[unsafe(no_mangle)]
pub extern "C" fn net_mesh_channel_configs_arc_clone(
    handle: *mut MeshNodeHandle,
) -> *mut Arc<ChannelConfigRegistry> {
    if handle.is_null() {
        return std::ptr::null_mut();
    }
    let h = unsafe { &*handle };
    Box::into_raw(Box::new(h.channel_configs.clone()))
}

/// Free an `Arc<MeshNode>` handle produced by
/// [`net_mesh_arc_clone`]. Idempotent on NULL.
#[unsafe(no_mangle)]
pub extern "C" fn net_mesh_arc_free(p: *mut Arc<MeshNode>) {
    if p.is_null() {
        return;
    }
    unsafe {
        drop(Box::from_raw(p));
    }
}

/// Free an `Arc<ChannelConfigRegistry>` handle produced by
/// [`net_mesh_channel_configs_arc_clone`]. Idempotent on NULL.
#[unsafe(no_mangle)]
pub extern "C" fn net_mesh_channel_configs_arc_free(p: *mut Arc<ChannelConfigRegistry>) {
    if p.is_null() {
        return;
    }
    unsafe {
        drop(Box::from_raw(p));
    }
}

/// Write the hex-encoded 32-byte Noise static public key of this
/// node to `*out`. Caller frees via `net_free_string`.
#[unsafe(no_mangle)]
pub extern "C" fn net_mesh_public_key_hex(
    handle: *mut MeshNodeHandle,
    out_ptr: *mut *mut c_char,
    out_len: *mut usize,
) -> c_int {
    if handle.is_null() || out_ptr.is_null() || out_len.is_null() {
        return NetError::NullPointer.into();
    }
    let h = unsafe { &*handle };
    let s = hex::encode(h.inner.public_key());
    write_string_out(s, out_ptr, out_len)
}

#[unsafe(no_mangle)]
pub extern "C" fn net_mesh_node_id(handle: *mut MeshNodeHandle) -> u64 {
    if handle.is_null() {
        return 0;
    }
    let h = unsafe { &*handle };
    h.inner.node_id()
}

/// Writes the 32-byte ed25519 entity id of this mesh into `out[32]`.
/// Matches `Identity::from_seed(seed).entity_id` when the mesh was
/// constructed with `identity_seed_hex = hex::encode(seed)`.
#[unsafe(no_mangle)]
pub extern "C" fn net_mesh_entity_id(handle: *mut MeshNodeHandle, out: *mut u8) -> c_int {
    if handle.is_null() || out.is_null() {
        return NetError::NullPointer.into();
    }
    let h = unsafe { &*handle };
    let bytes = h.inner.entity_id().as_bytes();
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), out, 32);
    }
    0
}

/// Connect (initiator). Blocks until the handshake completes.
#[unsafe(no_mangle)]
pub extern "C" fn net_mesh_connect(
    handle: *mut MeshNodeHandle,
    peer_addr: *const c_char,
    peer_pubkey_hex: *const c_char,
    peer_node_id: u64,
) -> c_int {
    if handle.is_null() || peer_addr.is_null() || peer_pubkey_hex.is_null() {
        return NetError::NullPointer.into();
    }
    let h = unsafe { &*handle };
    let Some(addr_s) = (unsafe { c_str_to_string(peer_addr) }) else {
        return NetError::InvalidUtf8.into();
    };
    let addr: std::net::SocketAddr = match addr_s.parse() {
        Ok(a) => a,
        Err(_) => return NET_ERR_MESH_HANDSHAKE,
    };
    let Some(pk_s) = (unsafe { c_str_to_string(peer_pubkey_hex) }) else {
        return NetError::InvalidUtf8.into();
    };
    let pk_bytes = match hex::decode(pk_s) {
        Ok(b) => b,
        Err(_) => return NET_ERR_MESH_HANDSHAKE,
    };
    if pk_bytes.len() != 32 {
        return NET_ERR_MESH_HANDSHAKE;
    }
    let mut pk = [0u8; 32];
    pk.copy_from_slice(&pk_bytes);

    let node = h.inner.clone();
    match block_on(async move { node.connect(addr, &pk, peer_node_id).await }) {
        Ok(_) => 0,
        Err(e) => adapter_err_to_code(&e),
    }
}

/// Accept an incoming connection (responder). Writes the peer's wire
/// address to `*out_addr` (caller frees via `net_free_string`).
#[unsafe(no_mangle)]
pub extern "C" fn net_mesh_accept(
    handle: *mut MeshNodeHandle,
    peer_node_id: u64,
    out_addr: *mut *mut c_char,
    out_len: *mut usize,
) -> c_int {
    if handle.is_null() || out_addr.is_null() || out_len.is_null() {
        return NetError::NullPointer.into();
    }
    let h = unsafe { &*handle };
    let node = h.inner.clone();
    match block_on(async move { node.accept(peer_node_id).await }) {
        Ok((addr, _)) => write_string_out(addr.to_string(), out_addr, out_len),
        Err(e) => adapter_err_to_code(&e),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn net_mesh_start(handle: *mut MeshNodeHandle) -> c_int {
    if handle.is_null() {
        return NetError::NullPointer.into();
    }
    let h = unsafe { &*handle };
    let node = h.inner.clone();
    // `start` spawns internal tasks via tokio::spawn; run under the
    // shared runtime.
    block_on(async move { node.start() });
    0
}

/// Shut down the node. Must be called before `net_mesh_free` to
/// release network resources. Idempotent.
///
/// Runs unconditionally — `MeshNode::shutdown` takes `&self` and
/// the underlying primitives (shutdown flag, notify, deactivate)
/// are safe to call while other handles still hold the `Arc`. A
/// prior version silently returned 0 whenever `Arc::strong_count`
/// exceeded 1, which meant a caller that held a stream handle
/// would see "shutdown successful" without any tasks actually
/// stopping — the node kept running until every stream was
/// dropped. Callers now always get the real shutdown outcome.
#[unsafe(no_mangle)]
pub extern "C" fn net_mesh_shutdown(handle: *mut MeshNodeHandle) -> c_int {
    if handle.is_null() {
        return NetError::NullPointer.into();
    }
    let h = unsafe { &*handle };
    match block_on(async { h.inner.shutdown().await }) {
        Ok(()) => 0,
        Err(e) => adapter_err_to_code(&e),
    }
}

// =========================================================================
// NAT traversal
// =========================================================================
//
// Framing (plan §5, load-bearing): every user-visible docstring
// positions NAT traversal as **optimization, not correctness**.
// Nodes behind NAT can always reach each other through the
// routed-handshake path. A `nat_type` of `"symmetric"` or any
// `NET_ERR_TRAVERSAL_*` code is not a connectivity failure —
// traffic keeps riding the relay. Each function returns early
// with `NetError::Unsupported` (= -1 NetError variant) when the
// crate is built without `nat-traversal`, so cgo call sites that
// unconditionally reference these symbols still link.

/// Write this mesh's NAT classification into `out_str` as one of
/// `"open" | "cone" | "symmetric" | "unknown"`. Stable vocabulary
/// — matches the NAPI / PyO3 binding strings. Caller frees via
/// `net_free_string`.
///
/// Returns `0` on success or a NetError code on failure. Only
/// present when the crate is built with `--features nat-traversal`.
#[cfg(feature = "nat-traversal")]
#[unsafe(no_mangle)]
pub extern "C" fn net_mesh_nat_type(
    handle: *mut MeshNodeHandle,
    out_str: *mut *mut c_char,
    out_len: *mut usize,
) -> c_int {
    if handle.is_null() || out_str.is_null() || out_len.is_null() {
        return NetError::NullPointer.into();
    }
    let h = unsafe { &*handle };
    write_string_out(
        nat_class_to_str(h.inner.nat_class()).to_string(),
        out_str,
        out_len,
    )
}

/// Write this mesh's last-observed reflex `ip:port` into
/// `out_str`. When no reflex has been observed yet (pre-
/// classification, or only one peer connected), writes an empty
/// string and still returns `0`.
#[cfg(feature = "nat-traversal")]
#[unsafe(no_mangle)]
pub extern "C" fn net_mesh_reflex_addr(
    handle: *mut MeshNodeHandle,
    out_str: *mut *mut c_char,
    out_len: *mut usize,
) -> c_int {
    if handle.is_null() || out_str.is_null() || out_len.is_null() {
        return NetError::NullPointer.into();
    }
    let h = unsafe { &*handle };
    let s = h
        .inner
        .reflex_addr()
        .map(|a| a.to_string())
        .unwrap_or_default();
    write_string_out(s, out_str, out_len)
}

/// Write `peer_node_id`'s advertised NAT classification (read
/// from its `nat:*` capability tag) into `out_str`. Returns
/// `"unknown"` when we have no announcement from that peer.
#[cfg(feature = "nat-traversal")]
#[unsafe(no_mangle)]
pub extern "C" fn net_mesh_peer_nat_type(
    handle: *mut MeshNodeHandle,
    peer_node_id: u64,
    out_str: *mut *mut c_char,
    out_len: *mut usize,
) -> c_int {
    if handle.is_null() || out_str.is_null() || out_len.is_null() {
        return NetError::NullPointer.into();
    }
    let h = unsafe { &*handle };
    write_string_out(
        nat_class_to_str(h.inner.peer_nat_class(peer_node_id)).to_string(),
        out_str,
        out_len,
    )
}

/// Send one reflex probe to `peer_node_id` and write the public
/// `ip:port` the peer observed into `out_str`. Blocks on the
/// shared runtime until the probe completes or times out.
///
/// Returns `0` on success or a `NET_ERR_TRAVERSAL_*` code on
/// failure. `NET_ERR_TRAVERSAL_REFLEX_TIMEOUT` means the probe
/// didn't complete in time; `NET_ERR_TRAVERSAL_PEER_NOT_REACHABLE`
/// means we have no session with `peer_node_id`.
#[cfg(feature = "nat-traversal")]
#[unsafe(no_mangle)]
pub extern "C" fn net_mesh_probe_reflex(
    handle: *mut MeshNodeHandle,
    peer_node_id: u64,
    out_str: *mut *mut c_char,
    out_len: *mut usize,
) -> c_int {
    if handle.is_null() || out_str.is_null() || out_len.is_null() {
        return NetError::NullPointer.into();
    }
    let h = unsafe { &*handle };
    let node = h.inner.clone();
    match block_on(async move { node.probe_reflex(peer_node_id).await }) {
        Ok(addr) => write_string_out(addr.to_string(), out_str, out_len),
        Err(e) => traversal_err_to_code(&e),
    }
}

/// Explicitly re-run the NAT classification sweep. No-op when
/// fewer than 2 peers are connected. Never returns an error;
/// callers that want the result should read `nat_type` +
/// `reflex_addr` afterward.
#[cfg(feature = "nat-traversal")]
#[unsafe(no_mangle)]
pub extern "C" fn net_mesh_reclassify_nat(handle: *mut MeshNodeHandle) -> c_int {
    if handle.is_null() {
        return NetError::NullPointer.into();
    }
    let h = unsafe { &*handle };
    let node = h.inner.clone();
    block_on(async move { node.reclassify_nat().await });
    0
}

/// Fill `out_punches_attempted`, `out_punches_succeeded`,
/// `out_relay_fallbacks` with the current cumulative counters.
/// Each pointer may be null to skip that field. Monotonic —
/// counters never decrease or reset.
#[cfg(feature = "nat-traversal")]
#[unsafe(no_mangle)]
pub extern "C" fn net_mesh_traversal_stats(
    handle: *mut MeshNodeHandle,
    out_punches_attempted: *mut u64,
    out_punches_succeeded: *mut u64,
    out_relay_fallbacks: *mut u64,
) -> c_int {
    if handle.is_null() {
        return NetError::NullPointer.into();
    }
    let h = unsafe { &*handle };
    let snap = h.inner.traversal_stats();
    unsafe {
        if !out_punches_attempted.is_null() {
            *out_punches_attempted = snap.punches_attempted;
        }
        if !out_punches_succeeded.is_null() {
            *out_punches_succeeded = snap.punches_succeeded;
        }
        if !out_relay_fallbacks.is_null() {
            *out_relay_fallbacks = snap.relay_fallbacks;
        }
    }
    0
}

/// Establish a session to `peer_node_id` via rendezvous through
/// `coordinator`, picking between direct-handshake and a
/// coordinated punch per the pair-type matrix. Always resolves
/// (on punch-failed, falls back to routed). Inspect the stats
/// counters afterward to distinguish outcomes.
///
/// `peer_pubkey_hex` is the peer's 32-byte Noise static public
/// key as a 64-char hex string.
///
/// Returns `0` on success or a `NET_ERR_TRAVERSAL_*` /
/// `NET_ERR_MESH_HANDSHAKE` code on failure.
#[cfg(feature = "nat-traversal")]
#[unsafe(no_mangle)]
pub extern "C" fn net_mesh_connect_direct(
    handle: *mut MeshNodeHandle,
    peer_node_id: u64,
    peer_pubkey_hex: *const c_char,
    coordinator: u64,
) -> c_int {
    if handle.is_null() || peer_pubkey_hex.is_null() {
        return NetError::NullPointer.into();
    }
    let h = unsafe { &*handle };
    let Some(pk_s) = (unsafe { c_str_to_string(peer_pubkey_hex) }) else {
        return NetError::InvalidUtf8.into();
    };
    let pk_bytes = match hex::decode(pk_s) {
        Ok(b) => b,
        Err(_) => return NET_ERR_MESH_HANDSHAKE,
    };
    if pk_bytes.len() != 32 {
        return NET_ERR_MESH_HANDSHAKE;
    }
    let mut pk = [0u8; 32];
    pk.copy_from_slice(&pk_bytes);

    let node = h.inner.clone();
    match block_on(async move { node.connect_direct(peer_node_id, &pk, coordinator).await }) {
        Ok(_) => 0,
        Err(e) => traversal_err_to_code(&e),
    }
}

/// Install a runtime reflex override. `external` is a
/// UTF-8 / null-terminated `"ip:port"` string. Forces `nat_type`
/// to `"open"` and `reflex_addr` to `external` immediately;
/// short-circuits any further classifier sweeps.
///
/// Returns `0` on success or `NET_ERR_MESH_INIT` on a malformed
/// address.
#[cfg(feature = "nat-traversal")]
#[unsafe(no_mangle)]
pub extern "C" fn net_mesh_set_reflex_override(
    handle: *mut MeshNodeHandle,
    external: *const c_char,
) -> c_int {
    if handle.is_null() || external.is_null() {
        return NetError::NullPointer.into();
    }
    let h = unsafe { &*handle };
    let Some(s) = (unsafe { c_str_to_string(external) }) else {
        return NetError::InvalidUtf8.into();
    };
    let Ok(addr) = s.parse::<std::net::SocketAddr>() else {
        return NET_ERR_MESH_INIT;
    };
    h.inner.set_reflex_override(addr);
    0
}

/// Drop a previously-installed reflex override. The classifier
/// resumes on its normal cadence; `reflex_addr` clears to empty
/// immediately so a between-sweep read doesn't return a stale
/// override.
///
/// No-op when no override is active. Always returns `0` on a
/// live handle.
#[cfg(feature = "nat-traversal")]
#[unsafe(no_mangle)]
pub extern "C" fn net_mesh_clear_reflex_override(handle: *mut MeshNodeHandle) -> c_int {
    if handle.is_null() {
        return NetError::NullPointer.into();
    }
    let h = unsafe { &*handle };
    h.inner.clear_reflex_override();
    0
}

// =========================================================================
// NAT-traversal fallback stubs — built when the core is
// compiled *without* `--features nat-traversal`.
//
// Bug L (cubic, P1): the Go / NAPI / PyO3 bindings unconditionally
// link against these symbols, so a cdylib without the feature
// used to fail at dlopen / load time with missing-symbol
// errors. The doc comment on each binding promised
// `ErrTraversalUnsupported` as the runtime surface for a no-
// feature build, but there were no stubs to back that promise.
//
// These stubs make the promise real: the symbol resolves, the
// call returns `NET_ERR_TRAVERSAL_UNSUPPORTED`, and the Go
// error-mapping layer translates that to
// `ErrTraversalUnsupported`. No heap allocation — the `_out_*`
// pointers are left untouched (the Go side treats them as
// invalid on a nonzero return).
//
// Every signature mirrors the `#[cfg(feature = "nat-traversal")]`
// definition above. Ordering matches the feature-on block so
// diff review can line up the pair at a glance.

#[cfg(not(feature = "nat-traversal"))]
#[unsafe(no_mangle)]
pub extern "C" fn net_mesh_nat_type(
    _handle: *mut MeshNodeHandle,
    _out_str: *mut *mut c_char,
    _out_len: *mut usize,
) -> c_int {
    NET_ERR_TRAVERSAL_UNSUPPORTED
}

#[cfg(not(feature = "nat-traversal"))]
#[unsafe(no_mangle)]
pub extern "C" fn net_mesh_reflex_addr(
    _handle: *mut MeshNodeHandle,
    _out_str: *mut *mut c_char,
    _out_len: *mut usize,
) -> c_int {
    NET_ERR_TRAVERSAL_UNSUPPORTED
}

#[cfg(not(feature = "nat-traversal"))]
#[unsafe(no_mangle)]
pub extern "C" fn net_mesh_peer_nat_type(
    _handle: *mut MeshNodeHandle,
    _peer_node_id: u64,
    _out_str: *mut *mut c_char,
    _out_len: *mut usize,
) -> c_int {
    NET_ERR_TRAVERSAL_UNSUPPORTED
}

#[cfg(not(feature = "nat-traversal"))]
#[unsafe(no_mangle)]
pub extern "C" fn net_mesh_probe_reflex(
    _handle: *mut MeshNodeHandle,
    _peer_node_id: u64,
    _out_str: *mut *mut c_char,
    _out_len: *mut usize,
) -> c_int {
    NET_ERR_TRAVERSAL_UNSUPPORTED
}

#[cfg(not(feature = "nat-traversal"))]
#[unsafe(no_mangle)]
pub extern "C" fn net_mesh_reclassify_nat(_handle: *mut MeshNodeHandle) -> c_int {
    NET_ERR_TRAVERSAL_UNSUPPORTED
}

#[cfg(not(feature = "nat-traversal"))]
#[unsafe(no_mangle)]
pub extern "C" fn net_mesh_traversal_stats(
    _handle: *mut MeshNodeHandle,
    _out_punches_attempted: *mut u64,
    _out_punches_succeeded: *mut u64,
    _out_relay_fallbacks: *mut u64,
) -> c_int {
    NET_ERR_TRAVERSAL_UNSUPPORTED
}

#[cfg(not(feature = "nat-traversal"))]
#[unsafe(no_mangle)]
pub extern "C" fn net_mesh_connect_direct(
    _handle: *mut MeshNodeHandle,
    _peer_node_id: u64,
    _peer_pubkey_hex: *const c_char,
    _coordinator: u64,
) -> c_int {
    NET_ERR_TRAVERSAL_UNSUPPORTED
}

#[cfg(not(feature = "nat-traversal"))]
#[unsafe(no_mangle)]
pub extern "C" fn net_mesh_set_reflex_override(
    _handle: *mut MeshNodeHandle,
    _external: *const c_char,
) -> c_int {
    NET_ERR_TRAVERSAL_UNSUPPORTED
}

#[cfg(not(feature = "nat-traversal"))]
#[unsafe(no_mangle)]
pub extern "C" fn net_mesh_clear_reflex_override(_handle: *mut MeshNodeHandle) -> c_int {
    NET_ERR_TRAVERSAL_UNSUPPORTED
}

// =========================================================================
// Streams
// =========================================================================

#[derive(Deserialize, Default)]
struct StreamOpenConfig {
    /// `"reliable" | "fire_and_forget"`. Default `"fire_and_forget"`.
    reliability: Option<String>,
    /// Initial send-credit window in bytes. 0 disables backpressure.
    /// Default: `DEFAULT_STREAM_WINDOW_BYTES` (64 KB).
    window_bytes: Option<u32>,
    fairness_weight: Option<u8>,
}

pub struct MeshStreamHandle {
    stream: CoreStream,
    // Keep the node alive as long as the stream is alive so sends
    // don't race a concurrent shutdown.
    _node: Arc<MeshNode>,
}

#[unsafe(no_mangle)]
pub extern "C" fn net_mesh_open_stream(
    handle: *mut MeshNodeHandle,
    peer_node_id: u64,
    stream_id: u64,
    config_json: *const c_char,
    out_stream: *mut *mut MeshStreamHandle,
) -> c_int {
    if handle.is_null() || out_stream.is_null() {
        return NetError::NullPointer.into();
    }
    let h = unsafe { &*handle };
    let cfg_json: StreamOpenConfig = if config_json.is_null() {
        StreamOpenConfig::default()
    } else {
        let Some(s) = (unsafe { c_str_to_string(config_json) }) else {
            return NetError::InvalidUtf8.into();
        };
        match serde_json::from_str(&s) {
            Ok(v) => v,
            Err(_) => return NetError::InvalidJson.into(),
        }
    };
    let reliability = match cfg_json.reliability.as_deref() {
        None | Some("fire_and_forget") => Reliability::FireAndForget,
        Some("reliable") => Reliability::Reliable,
        Some(_) => return NET_ERR_MESH_TRANSPORT,
    };
    let window = cfg_json.window_bytes.unwrap_or(DEFAULT_STREAM_WINDOW_BYTES);
    let weight = cfg_json.fairness_weight.unwrap_or(1);
    let cfg = StreamConfig::new()
        .with_reliability(reliability)
        .with_window_bytes(window)
        .with_fairness_weight(weight);
    match h.inner.open_stream(peer_node_id, stream_id, cfg) {
        Ok(stream) => {
            let sh = Box::new(MeshStreamHandle {
                stream,
                _node: h.inner.clone(),
            });
            unsafe {
                *out_stream = Box::into_raw(sh);
            }
            0
        }
        Err(e) => adapter_err_to_code(&e),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn net_mesh_stream_free(handle: *mut MeshStreamHandle) {
    if handle.is_null() {
        return;
    }
    unsafe {
        drop(Box::from_raw(handle));
    }
}

/// Collect an array of borrowed `(ptr, len)` pairs into a
/// `Vec<Bytes>`. Caller must keep the pointer / length arrays alive
/// for the duration of the C call.
///
/// Returns `None` if any per-entry pointer is null *with* a non-zero
/// length — the C contract has no "skip this entry" channel, so the
/// only correct response is to refuse the whole batch. A null pointer
/// with `len == 0` is treated as an empty payload (it never gets
/// dereferenced).
unsafe fn collect_payloads(
    payloads: *const *const u8,
    lens: *const usize,
    count: usize,
) -> Option<Vec<Bytes>> {
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let ptr = *payloads.add(i);
        let len = *lens.add(i);
        if ptr.is_null() {
            if len == 0 {
                out.push(Bytes::new());
                continue;
            }
            return None;
        }
        // `slice::from_raw_parts` requires `len <= isize::MAX`.
        // A caller passing a sign-extended `-1` would otherwise
        // immediately UB before any other validation runs.
        if len > isize::MAX as usize {
            return None;
        }
        let slice = std::slice::from_raw_parts(ptr, len);
        out.push(Bytes::copy_from_slice(slice));
    }
    Some(out)
}

/// Ensure the supplied stream handle was created by the supplied
/// node handle. Without this check, `net_mesh_send` would happily
/// route bytes through whichever `MeshNode` was passed, even if the
/// stream belonged to a different one — silent cross-session
/// traffic. `Arc::ptr_eq` is O(1) and definitive: stream handles
/// cache the originating
/// node Arc in `_node` for exactly this purpose.
#[inline]
fn handles_match(sh: &MeshStreamHandle, nh: &MeshNodeHandle) -> bool {
    Arc::ptr_eq(&sh._node, &nh.inner)
}

#[unsafe(no_mangle)]
pub extern "C" fn net_mesh_send(
    handle: *mut MeshStreamHandle,
    payloads: *const *const u8,
    lens: *const usize,
    count: usize,
    node_handle: *mut MeshNodeHandle,
) -> c_int {
    if handle.is_null() || node_handle.is_null() {
        return NetError::NullPointer.into();
    }
    if count > 0 && (payloads.is_null() || lens.is_null()) {
        return NetError::NullPointer.into();
    }
    let sh = unsafe { &*handle };
    let nh = unsafe { &*node_handle };
    if !handles_match(sh, nh) {
        return NetError::MismatchedHandles.into();
    }
    let payloads = match unsafe { collect_payloads(payloads, lens, count) } {
        Some(v) => v,
        None => return NetError::NullPointer.into(),
    };
    let node = nh.inner.clone();
    let stream = sh.stream.clone();
    match block_on(async move { node.send_on_stream(&stream, &payloads).await }) {
        Ok(()) => 0,
        Err(e) => stream_err_to_code(&e),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn net_mesh_send_with_retry(
    handle: *mut MeshStreamHandle,
    payloads: *const *const u8,
    lens: *const usize,
    count: usize,
    max_retries: u32,
    node_handle: *mut MeshNodeHandle,
) -> c_int {
    if handle.is_null() || node_handle.is_null() {
        return NetError::NullPointer.into();
    }
    if count > 0 && (payloads.is_null() || lens.is_null()) {
        return NetError::NullPointer.into();
    }
    let sh = unsafe { &*handle };
    let nh = unsafe { &*node_handle };
    if !handles_match(sh, nh) {
        return NetError::MismatchedHandles.into();
    }
    let payloads = match unsafe { collect_payloads(payloads, lens, count) } {
        Some(v) => v,
        None => return NetError::NullPointer.into(),
    };
    let node = nh.inner.clone();
    let stream = sh.stream.clone();
    match block_on(async move {
        node.send_with_retry(&stream, &payloads, max_retries as usize)
            .await
    }) {
        Ok(()) => 0,
        Err(e) => stream_err_to_code(&e),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn net_mesh_send_blocking(
    handle: *mut MeshStreamHandle,
    payloads: *const *const u8,
    lens: *const usize,
    count: usize,
    node_handle: *mut MeshNodeHandle,
) -> c_int {
    if handle.is_null() || node_handle.is_null() {
        return NetError::NullPointer.into();
    }
    if count > 0 && (payloads.is_null() || lens.is_null()) {
        return NetError::NullPointer.into();
    }
    let sh = unsafe { &*handle };
    let nh = unsafe { &*node_handle };
    if !handles_match(sh, nh) {
        return NetError::MismatchedHandles.into();
    }
    let payloads = match unsafe { collect_payloads(payloads, lens, count) } {
        Some(v) => v,
        None => return NetError::NullPointer.into(),
    };
    let node = nh.inner.clone();
    let stream = sh.stream.clone();
    match block_on(async move { node.send_blocking(&stream, &payloads).await }) {
        Ok(()) => 0,
        Err(e) => stream_err_to_code(&e),
    }
}

#[derive(Serialize)]
struct StreamStatsJson {
    tx_seq: u64,
    rx_seq: u64,
    inbound_pending: u64,
    last_activity_ns: u64,
    active: bool,
    backpressure_events: u64,
    tx_credit_remaining: u32,
    tx_window: u32,
    credit_grants_received: u64,
    credit_grants_sent: u64,
}

#[unsafe(no_mangle)]
pub extern "C" fn net_mesh_stream_stats(
    node_handle: *mut MeshNodeHandle,
    peer_node_id: u64,
    stream_id: u64,
    out_json: *mut *mut c_char,
    out_len: *mut usize,
) -> c_int {
    if node_handle.is_null() || out_json.is_null() || out_len.is_null() {
        return NetError::NullPointer.into();
    }
    let h = unsafe { &*node_handle };
    match h.inner.stream_stats(peer_node_id, stream_id) {
        Some(s) => {
            let js = StreamStatsJson {
                tx_seq: s.tx_seq,
                rx_seq: s.rx_seq,
                inbound_pending: s.inbound_pending,
                last_activity_ns: s.last_activity_ns,
                active: s.active,
                backpressure_events: s.backpressure_events,
                tx_credit_remaining: s.tx_credit_remaining,
                tx_window: s.tx_window,
                credit_grants_received: s.credit_grants_received,
                credit_grants_sent: s.credit_grants_sent,
            };
            write_json_out(&js, out_json, out_len)
        }
        None => {
            // Encode `null` so Go can distinguish "no such stream"
            // from an error.
            write_string_out("null".to_string(), out_json, out_len)
        }
    }
}

// =========================================================================
// Shard receive
// =========================================================================

#[derive(Serialize)]
struct RecvEventJson {
    id: String,
    /// Base64 payload (binary-safe across the JSON boundary).
    payload_b64: String,
    insertion_ts: u64,
    shard_id: u16,
}

#[unsafe(no_mangle)]
pub extern "C" fn net_mesh_recv_shard(
    handle: *mut MeshNodeHandle,
    shard_id: u16,
    limit: u32,
    out_json: *mut *mut c_char,
    out_len: *mut usize,
) -> c_int {
    if handle.is_null() || out_json.is_null() || out_len.is_null() {
        return NetError::NullPointer.into();
    }
    let h = unsafe { &*handle };
    let node = h.inner.clone();
    let result = block_on(async move { node.poll_shard(shard_id, None, limit as usize).await });
    let result = match result {
        Ok(r) => r,
        Err(e) => return adapter_err_to_code(&e),
    };
    let events: Vec<RecvEventJson> = result
        .events
        .into_iter()
        .map(|e| RecvEventJson {
            id: e.id,
            payload_b64: encode_b64(&e.raw),
            insertion_ts: e.insertion_ts,
            shard_id: e.shard_id,
        })
        .collect();
    write_json_out(&events, out_json, out_len)
}

fn encode_b64(bytes: &[u8]) -> String {
    // Small stdlib-free base64. Net already pulls in `base64` via
    // other deps, but a local encoder keeps this module independent.
    const ALPH: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(bytes.len().div_ceil(3) * 4);
    let mut i = 0;
    while i + 3 <= bytes.len() {
        let chunk = &bytes[i..i + 3];
        s.push(ALPH[(chunk[0] >> 2) as usize] as char);
        s.push(ALPH[(((chunk[0] & 0b11) << 4) | (chunk[1] >> 4)) as usize] as char);
        s.push(ALPH[(((chunk[1] & 0b1111) << 2) | (chunk[2] >> 6)) as usize] as char);
        s.push(ALPH[(chunk[2] & 0b111111) as usize] as char);
        i += 3;
    }
    let rem = bytes.len() - i;
    if rem == 1 {
        let b = bytes[i];
        s.push(ALPH[(b >> 2) as usize] as char);
        s.push(ALPH[((b & 0b11) << 4) as usize] as char);
        s.push('=');
        s.push('=');
    } else if rem == 2 {
        let b0 = bytes[i];
        let b1 = bytes[i + 1];
        s.push(ALPH[(b0 >> 2) as usize] as char);
        s.push(ALPH[(((b0 & 0b11) << 4) | (b1 >> 4)) as usize] as char);
        s.push(ALPH[((b1 & 0b1111) << 2) as usize] as char);
        s.push('=');
    }
    s
}

// =========================================================================
// Channels (distributed pub/sub)
// =========================================================================

#[derive(Deserialize)]
struct ChannelConfigInput {
    name: String,
    visibility: Option<String>,
    reliable: Option<bool>,
    require_token: Option<bool>,
    priority: Option<u8>,
    max_rate_pps: Option<u32>,
    /// Capability filter restricting who may publish on this
    /// channel. Same POJO shape as `CapabilityFilter` (see
    /// `net_mesh_find_nodes`).
    publish_caps: Option<CapabilityFilterJson>,
    /// Capability filter restricting who may subscribe. Subscribers
    /// whose announced caps miss this filter are rejected with
    /// `NET_ERR_CHANNEL_AUTH`.
    subscribe_caps: Option<CapabilityFilterJson>,
}

fn parse_visibility(s: &str) -> Option<InnerVisibility> {
    match s {
        "subnet-local" => Some(InnerVisibility::SubnetLocal),
        "parent-visible" => Some(InnerVisibility::ParentVisible),
        "exported" => Some(InnerVisibility::Exported),
        "global" => Some(InnerVisibility::Global),
        _ => None,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn net_mesh_register_channel(
    handle: *mut MeshNodeHandle,
    config_json: *const c_char,
) -> c_int {
    if handle.is_null() || config_json.is_null() {
        return NetError::NullPointer.into();
    }
    let h = unsafe { &*handle };
    let Some(s) = (unsafe { c_str_to_string(config_json) }) else {
        return NetError::InvalidUtf8.into();
    };
    let input: ChannelConfigInput = match serde_json::from_str(&s) {
        Ok(v) => v,
        Err(_) => return NetError::InvalidJson.into(),
    };
    let name = match InnerChannelName::new(&input.name) {
        Ok(n) => n,
        Err(_) => return NET_ERR_CHANNEL,
    };
    let mut cfg = InnerChannelConfig::new(ChannelId::new(name));
    if let Some(v) = input.visibility {
        let Some(vis) = parse_visibility(&v) else {
            return NET_ERR_CHANNEL;
        };
        cfg = cfg.with_visibility(vis);
    }
    if let Some(r) = input.reliable {
        cfg = cfg.with_reliable(r);
    }
    if let Some(t) = input.require_token {
        cfg = cfg.with_require_token(t);
    }
    if let Some(p) = input.priority {
        cfg = cfg.with_priority(p);
    }
    if let Some(pps) = input.max_rate_pps {
        cfg = cfg.with_rate_limit(pps);
    }
    if let Some(filter_json) = input.publish_caps {
        cfg = cfg.with_publish_caps(capability_filter_from_json(filter_json));
    }
    if let Some(filter_json) = input.subscribe_caps {
        cfg = cfg.with_subscribe_caps(capability_filter_from_json(filter_json));
    }
    h.channel_configs.insert(cfg);
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn net_mesh_subscribe_channel(
    handle: *mut MeshNodeHandle,
    publisher_node_id: u64,
    channel: *const c_char,
) -> c_int {
    subscribe_or_unsubscribe(handle, publisher_node_id, channel, true)
}

#[unsafe(no_mangle)]
pub extern "C" fn net_mesh_unsubscribe_channel(
    handle: *mut MeshNodeHandle,
    publisher_node_id: u64,
    channel: *const c_char,
) -> c_int {
    subscribe_or_unsubscribe(handle, publisher_node_id, channel, false)
}

/// Subscribe with a serialized `PermissionToken` attached. Parses
/// the token client-side (rejecting malformed bytes with
/// `NET_ERR_TOKEN_INVALID_FORMAT`) before dispatching the request
/// to the publisher. Signature verification happens on the
/// publisher side; a tampered token will surface as
/// `NET_ERR_CHANNEL_AUTH` rather than a token error in this call.
#[unsafe(no_mangle)]
pub extern "C" fn net_mesh_subscribe_channel_with_token(
    handle: *mut MeshNodeHandle,
    publisher_node_id: u64,
    channel: *const c_char,
    token: *const u8,
    token_len: usize,
) -> c_int {
    if handle.is_null() || channel.is_null() || token.is_null() {
        return NetError::NullPointer.into();
    }
    let h = unsafe { &*handle };
    let Some(s) = (unsafe { c_str_to_string(channel) }) else {
        return NetError::InvalidUtf8.into();
    };
    let name = match InnerChannelName::new(&s) {
        Ok(n) => n,
        Err(_) => return NET_ERR_CHANNEL,
    };
    let slice = unsafe { std::slice::from_raw_parts(token, token_len) };
    let parsed = match PermissionToken::from_bytes(slice) {
        Ok(t) => t,
        Err(e) => return token_err_to_code(&e),
    };
    let node = h.inner.clone();
    match block_on(async move {
        node.subscribe_channel_with_token(publisher_node_id, name, parsed)
            .await
    }) {
        Ok(()) => 0,
        Err(e) => adapter_err_to_channel_code(&e),
    }
}

fn subscribe_or_unsubscribe(
    handle: *mut MeshNodeHandle,
    publisher_node_id: u64,
    channel: *const c_char,
    subscribe: bool,
) -> c_int {
    if handle.is_null() || channel.is_null() {
        return NetError::NullPointer.into();
    }
    let h = unsafe { &*handle };
    let Some(s) = (unsafe { c_str_to_string(channel) }) else {
        return NetError::InvalidUtf8.into();
    };
    let name = match InnerChannelName::new(&s) {
        Ok(n) => n,
        Err(_) => return NET_ERR_CHANNEL,
    };
    let node = h.inner.clone();
    let outcome = if subscribe {
        block_on(async move { node.subscribe_channel(publisher_node_id, name).await })
    } else {
        block_on(async move { node.unsubscribe_channel(publisher_node_id, name).await })
    };
    match outcome {
        Ok(()) => 0,
        Err(e) => adapter_err_to_channel_code(&e),
    }
}

fn adapter_err_to_channel_code(err: &AdapterError) -> c_int {
    if let AdapterError::Connection(msg) = err {
        let prefix = "membership request rejected: ";
        if let Some(tail) = msg.strip_prefix(prefix) {
            if tail.trim() == "Some(Unauthorized)" {
                return NET_ERR_CHANNEL_AUTH;
            }
        }
    }
    NET_ERR_CHANNEL
}

#[derive(Deserialize, Default)]
struct PublishConfigInput {
    reliability: Option<String>,
    on_failure: Option<String>,
    max_inflight: Option<u32>,
}

#[derive(Serialize)]
struct PublishReportJson {
    attempted: u32,
    delivered: u32,
    errors: Vec<PublishFailureJson>,
}

#[derive(Serialize)]
struct PublishFailureJson {
    node_id: u64,
    message: String,
}

fn to_publish_report_json(r: InnerPublishReport) -> PublishReportJson {
    PublishReportJson {
        attempted: r.attempted as u32,
        delivered: r.delivered as u32,
        errors: r
            .errors
            .into_iter()
            .map(|(id, e)| PublishFailureJson {
                node_id: id,
                message: format!("{}", e),
            })
            .collect(),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn net_mesh_publish(
    handle: *mut MeshNodeHandle,
    channel: *const c_char,
    payload: *const u8,
    len: usize,
    config_json: *const c_char,
    out_json: *mut *mut c_char,
    out_len: *mut usize,
) -> c_int {
    if handle.is_null() || channel.is_null() || out_json.is_null() || out_len.is_null() {
        return NetError::NullPointer.into();
    }
    let h = unsafe { &*handle };
    let Some(ch) = (unsafe { c_str_to_string(channel) }) else {
        return NetError::InvalidUtf8.into();
    };
    let name = match InnerChannelName::new(&ch) {
        Ok(n) => n,
        Err(_) => return NET_ERR_CHANNEL,
    };
    let cfg_in: PublishConfigInput = if config_json.is_null() {
        PublishConfigInput::default()
    } else {
        let Some(s) = (unsafe { c_str_to_string(config_json) }) else {
            return NetError::InvalidUtf8.into();
        };
        match serde_json::from_str(&s) {
            Ok(v) => v,
            Err(_) => return NetError::InvalidJson.into(),
        }
    };
    let reliability = match cfg_in.reliability.as_deref() {
        None | Some("fire_and_forget") => Reliability::FireAndForget,
        Some("reliable") => Reliability::Reliable,
        Some(_) => return NET_ERR_CHANNEL,
    };
    let on_failure = match cfg_in.on_failure.as_deref() {
        None | Some("best_effort") => InnerOnFailure::BestEffort,
        Some("fail_fast") => InnerOnFailure::FailFast,
        Some("collect") => InnerOnFailure::Collect,
        Some(_) => return NET_ERR_CHANNEL,
    };
    let max_inflight = cfg_in.max_inflight.unwrap_or(32) as usize;
    let publish_cfg = InnerPublishConfig {
        reliability,
        on_failure,
        max_inflight,
    };
    let publisher = ChannelPublisher::new(name, publish_cfg);

    // Payload may be NULL only when len == 0.
    let bytes = if len == 0 {
        Bytes::new()
    } else if payload.is_null() {
        return NetError::NullPointer.into();
    } else if len > isize::MAX as usize {
        // `slice::from_raw_parts` requires `len <= isize::MAX`.
        return NetError::InvalidJson.into();
    } else {
        Bytes::copy_from_slice(unsafe { std::slice::from_raw_parts(payload, len) })
    };

    let node = h.inner.clone();
    match block_on(async move { node.publish(&publisher, bytes).await }) {
        Ok(report) => {
            let js = to_publish_report_json(report);
            write_json_out(&js, out_json, out_len)
        }
        Err(e) => adapter_err_to_channel_code(&e),
    }
}

// =========================================================================
// Identity + permission tokens
// =========================================================================

/// Opaque handle holding an ed25519 keypair plus a local
/// `TokenCache`. Matches the PyO3 / NAPI `Identity` pyclass layout —
/// cheap to clone (both fields are `Arc`s inside the core), and the
/// cache is owned by the handle rather than shared across peers.
pub struct IdentityHandle {
    keypair: Arc<EntityKeypair>,
    cache: Arc<TokenCache>,
}

/// Allocate and copy `src` into a freshly allocated buffer owned by
/// `std::alloc::alloc` with a layout of `Layout::array::<u8>(len)`.
/// The matching `net_free_bytes` must deallocate with the same layout
/// — both sides pin the capacity to `len`, so there is no reliance on
/// `Vec::shrink_to_fit` producing `capacity == len` (which is not
/// guaranteed by the allocator API).
fn alloc_bytes(src: &[u8], out_ptr: *mut *mut u8, out_len: *mut usize) -> c_int {
    let len = src.len();
    if len == 0 {
        unsafe {
            *out_ptr = std::ptr::null_mut();
            *out_len = 0;
        }
        return 0;
    }
    // `Layout::array::<u8>(len)` rejects `len > isize::MAX` (the
    // documented bound — NOT `usize::MAX`). The current call
    // sites stay well under that limit because `to_bytes()`
    // produces token-sized payloads, so the failure mode is
    // unreachable today; defending against it here also keeps the
    // helper safe to reuse from non-token code paths in the
    // future. A panic here would unwind across the surrounding
    // `extern "C"` boundary.
    let layout = match std::alloc::Layout::array::<u8>(len) {
        Ok(l) => l,
        // Reuse the closest sentinel we have — `NET_ERR_IDENTITY`
        // covers the only call sites today (token/identity helpers
        // that delegate to `alloc_bytes`). The negative integer is
        // an FFI-safe error code; the alternative `panic!` would
        // unwind across `extern "C"`.
        Err(_) => return NET_ERR_IDENTITY,
    };
    let ptr = unsafe { std::alloc::alloc(layout) };
    if ptr.is_null() {
        std::alloc::handle_alloc_error(layout);
    }
    unsafe {
        std::ptr::copy_nonoverlapping(src.as_ptr(), ptr, len);
        *out_ptr = ptr;
        *out_len = len;
    }
    0
}

/// Free a byte buffer allocated by the Rust side (tokens, entity ids
/// returned by reference, etc.). The `len` argument MUST match the
/// length returned by the allocating call — the buffer was allocated
/// with `Layout::array::<u8>(len)` and is freed with the same layout.
///
/// We silently no-op on `len > isize::MAX`: the allocation that
/// produced `ptr` could not have come from this process under that
/// layout (the allocator would have rejected the matching
/// `alloc`), so any such call is already memory-corruption
/// territory and the safest response is to abandon the free rather
/// than unwind. `net_free_bytes` is `extern "C"` with no
/// `catch_unwind` shim, so a panic would unwind across the FFI
/// boundary into a C / Go-cgo / NAPI / PyO3 caller — undefined
/// behaviour.
#[unsafe(no_mangle)]
pub extern "C" fn net_free_bytes(ptr: *mut u8, len: usize) {
    if ptr.is_null() || len == 0 {
        return;
    }
    // Reject `len > isize::MAX` before calling `Layout::array`. The
    // allocating call paired with this free uses the same layout and
    // would itself have failed for any such `len`, so a buffer
    // matching this `len` cannot have come from us; treat as a no-op
    // rather than panic across the FFI boundary.
    let layout = match std::alloc::Layout::array::<u8>(len) {
        Ok(l) => l,
        Err(_) => return,
    };
    unsafe {
        std::alloc::dealloc(ptr, layout);
    }
}

fn entity_id_from_bytes(bytes: *const u8, len: usize) -> Option<EntityId> {
    if bytes.is_null() || len != 32 {
        return None;
    }
    let slice = unsafe { std::slice::from_raw_parts(bytes, 32) };
    let mut arr = [0u8; 32];
    arr.copy_from_slice(slice);
    Some(EntityId::from_bytes(arr))
}

fn parse_scope_list(raw: &str) -> Option<TokenScope> {
    // JSON array of string scope names — same shape as PyO3's
    // `Vec<String>` parsing. Keeps the ABI aligned to the Python /
    // NAPI surfaces for round-trip fixtures.
    let values: Vec<String> = serde_json::from_str(raw).ok()?;
    let mut acc = TokenScope::NONE;
    for s in &values {
        acc = acc.union(match s.as_str() {
            "publish" => TokenScope::PUBLISH,
            "subscribe" => TokenScope::SUBSCRIBE,
            "admin" => TokenScope::ADMIN,
            "delegate" => TokenScope::DELEGATE,
            _ => return None,
        });
    }
    Some(acc)
}

fn scope_to_strings(scope: TokenScope) -> Vec<&'static str> {
    let mut out = Vec::new();
    if scope.contains(TokenScope::PUBLISH) {
        out.push("publish");
    }
    if scope.contains(TokenScope::SUBSCRIBE) {
        out.push("subscribe");
    }
    if scope.contains(TokenScope::ADMIN) {
        out.push("admin");
    }
    if scope.contains(TokenScope::DELEGATE) {
        out.push("delegate");
    }
    out
}

fn channel_name_to_hash(channel: &str) -> Option<u16> {
    InnerChannelName::new(channel).ok().map(|n| n.hash())
}

/// Generate a fresh ed25519 identity. Writes an owned handle to
/// `*out_handle`. Free via `net_identity_free`.
#[unsafe(no_mangle)]
pub extern "C" fn net_identity_generate(out_handle: *mut *mut IdentityHandle) -> c_int {
    if out_handle.is_null() {
        return NetError::NullPointer.into();
    }
    let handle = Box::new(IdentityHandle {
        keypair: Arc::new(EntityKeypair::generate()),
        cache: Arc::new(TokenCache::new()),
    });
    unsafe {
        *out_handle = Box::into_raw(handle);
    }
    0
}

/// Construct an identity from a caller-owned 32-byte ed25519 seed.
/// Installs a fresh, empty `TokenCache` — reinstall tokens via
/// `net_identity_install_token` after rehydrating from disk.
#[unsafe(no_mangle)]
pub extern "C" fn net_identity_from_seed(
    seed: *const u8,
    seed_len: usize,
    out_handle: *mut *mut IdentityHandle,
) -> c_int {
    if seed.is_null() || out_handle.is_null() {
        return NetError::NullPointer.into();
    }
    if seed_len != 32 {
        return NET_ERR_IDENTITY;
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(unsafe { std::slice::from_raw_parts(seed, 32) });
    let handle = Box::new(IdentityHandle {
        keypair: Arc::new(EntityKeypair::from_bytes(arr)),
        cache: Arc::new(TokenCache::new()),
    });
    unsafe {
        *out_handle = Box::into_raw(handle);
    }
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn net_identity_free(handle: *mut IdentityHandle) {
    if handle.is_null() {
        return;
    }
    unsafe {
        drop(Box::from_raw(handle));
    }
}

/// Write the 32-byte ed25519 seed into `out[32]`. Caller must pass
/// a buffer of at least 32 bytes.
#[unsafe(no_mangle)]
pub extern "C" fn net_identity_to_seed(handle: *mut IdentityHandle, out: *mut u8) -> c_int {
    if handle.is_null() || out.is_null() {
        return NetError::NullPointer.into();
    }
    let h = unsafe { &*handle };
    let seed = h.keypair.secret_bytes();
    unsafe {
        std::ptr::copy_nonoverlapping(seed.as_ptr(), out, 32);
    }
    0
}

/// Write the 32-byte entity id into `out[32]`.
#[unsafe(no_mangle)]
pub extern "C" fn net_identity_entity_id(handle: *mut IdentityHandle, out: *mut u8) -> c_int {
    if handle.is_null() || out.is_null() {
        return NetError::NullPointer.into();
    }
    let h = unsafe { &*handle };
    let id = h.keypair.entity_id().as_bytes();
    unsafe {
        std::ptr::copy_nonoverlapping(id.as_ptr(), out, 32);
    }
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn net_identity_node_id(handle: *mut IdentityHandle) -> u64 {
    if handle.is_null() {
        return 0;
    }
    let h = unsafe { &*handle };
    h.keypair.node_id()
}

#[unsafe(no_mangle)]
pub extern "C" fn net_identity_origin_hash(handle: *mut IdentityHandle) -> u32 {
    if handle.is_null() {
        return 0;
    }
    let h = unsafe { &*handle };
    h.keypair.origin_hash()
}

/// Sign `msg[len]` with the identity's ed25519 secret key. Writes a
/// 64-byte signature into `out_sig[64]`.
#[unsafe(no_mangle)]
pub extern "C" fn net_identity_sign(
    handle: *mut IdentityHandle,
    msg: *const u8,
    len: usize,
    out_sig: *mut u8,
) -> c_int {
    if handle.is_null() || out_sig.is_null() {
        return NetError::NullPointer.into();
    }
    if len > 0 && msg.is_null() {
        return NetError::NullPointer.into();
    }
    let h = unsafe { &*handle };
    let slice = if len == 0 {
        &[][..]
    } else {
        unsafe { std::slice::from_raw_parts(msg, len) }
    };
    let sig = h.keypair.sign(slice).to_bytes();
    unsafe {
        std::ptr::copy_nonoverlapping(sig.as_ptr(), out_sig, 64);
    }
    0
}

/// Issue a token to `subject`. Writes a newly-allocated blob to
/// `*out_token`; caller frees via `net_free_bytes(ptr, *out_len)`.
#[unsafe(no_mangle)]
pub extern "C" fn net_identity_issue_token(
    signer: *mut IdentityHandle,
    subject: *const u8,
    subject_len: usize,
    scope_json: *const c_char,
    channel: *const c_char,
    ttl_seconds: u32,
    delegation_depth: u8,
    out_token: *mut *mut u8,
    out_token_len: *mut usize,
) -> c_int {
    if signer.is_null() || out_token.is_null() || out_token_len.is_null() {
        return NetError::NullPointer.into();
    }
    let Some(subject_id) = entity_id_from_bytes(subject, subject_len) else {
        return NET_ERR_IDENTITY;
    };
    let Some(scope_s) = (unsafe { c_str_to_string(scope_json) }) else {
        return NetError::InvalidUtf8.into();
    };
    let Some(scope) = parse_scope_list(&scope_s) else {
        return NET_ERR_IDENTITY;
    };
    let Some(channel_s) = (unsafe { c_str_to_string(channel) }) else {
        return NetError::InvalidUtf8.into();
    };
    let Some(channel_hash) = channel_name_to_hash(&channel_s) else {
        return NET_ERR_IDENTITY;
    };
    let h = unsafe { &*signer };
    // Route through `try_issue` so a public-only signer keypair
    // (post-migration zeroize, etc.) surfaces as
    // `TokenError::ReadOnly` → `NET_ERR_IDENTITY` instead of
    // panic-unwinding across this `extern "C"` frame into the
    // caller's binding.
    let token = match PermissionToken::try_issue(
        &h.keypair,
        subject_id,
        scope,
        channel_hash,
        u64::from(ttl_seconds),
        delegation_depth,
    ) {
        Ok(t) => t,
        Err(e) => return token_err_to_code(&e),
    };
    alloc_bytes(&token.to_bytes(), out_token, out_token_len)
}

/// Install a token received from another issuer. Signature +
/// structural checks run on insert; malformed or tampered tokens
/// return the relevant `NET_ERR_TOKEN_*` code.
#[unsafe(no_mangle)]
pub extern "C" fn net_identity_install_token(
    handle: *mut IdentityHandle,
    token: *const u8,
    len: usize,
) -> c_int {
    if handle.is_null() || token.is_null() {
        return NetError::NullPointer.into();
    }
    let slice = unsafe { std::slice::from_raw_parts(token, len) };
    let parsed = match PermissionToken::from_bytes(slice) {
        Ok(t) => t,
        Err(e) => return token_err_to_code(&e),
    };
    let h = unsafe { &*handle };
    match h.cache.insert(parsed) {
        Ok(()) => 0,
        Err(e) => token_err_to_code(&e),
    }
}

/// Look up a cached token by `(subject, channel)`. Writes a newly-
/// allocated blob to `*out_token` on hit; writes `NULL` / `0` on
/// miss. Caller must always free on hit via `net_free_bytes`.
#[unsafe(no_mangle)]
pub extern "C" fn net_identity_lookup_token(
    handle: *mut IdentityHandle,
    subject: *const u8,
    subject_len: usize,
    channel: *const c_char,
    out_token: *mut *mut u8,
    out_token_len: *mut usize,
) -> c_int {
    if handle.is_null() || out_token.is_null() || out_token_len.is_null() {
        return NetError::NullPointer.into();
    }
    let Some(subject_id) = entity_id_from_bytes(subject, subject_len) else {
        return NET_ERR_IDENTITY;
    };
    let Some(channel_s) = (unsafe { c_str_to_string(channel) }) else {
        return NetError::InvalidUtf8.into();
    };
    let Some(channel_hash) = channel_name_to_hash(&channel_s) else {
        return NET_ERR_IDENTITY;
    };
    let h = unsafe { &*handle };
    match h.cache.get(&subject_id, channel_hash) {
        Some(token) => alloc_bytes(&token.to_bytes(), out_token, out_token_len),
        None => {
            unsafe {
                *out_token = std::ptr::null_mut();
                *out_token_len = 0;
            }
            0
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn net_identity_token_cache_len(handle: *mut IdentityHandle) -> u32 {
    if handle.is_null() {
        return 0;
    }
    let h = unsafe { &*handle };
    h.cache.len() as u32
}

// -------------------------------------------------------------------------
// Module-level token helpers
// -------------------------------------------------------------------------

#[derive(Serialize)]
struct ParsedTokenJson {
    issuer_hex: String,
    subject_hex: String,
    scope: Vec<&'static str>,
    channel_hash: u16,
    not_before: u64,
    not_after: u64,
    delegation_depth: u8,
    nonce: u64,
    signature_hex: String,
}

/// Parse a serialized `PermissionToken` into a JSON dict. Fields are
/// hex-encoded on the wire (`issuer_hex`, `subject_hex`,
/// `signature_hex`) so the JSON round-trips cleanly. Binary variants
/// live on the `Identity` handle.
#[unsafe(no_mangle)]
pub extern "C" fn net_parse_token(
    token: *const u8,
    len: usize,
    out_json: *mut *mut c_char,
    out_len: *mut usize,
) -> c_int {
    if token.is_null() || out_json.is_null() || out_len.is_null() {
        return NetError::NullPointer.into();
    }
    let slice = unsafe { std::slice::from_raw_parts(token, len) };
    let parsed = match PermissionToken::from_bytes(slice) {
        Ok(t) => t,
        Err(e) => return token_err_to_code(&e),
    };
    let out = ParsedTokenJson {
        issuer_hex: hex::encode(parsed.issuer.as_bytes()),
        subject_hex: hex::encode(parsed.subject.as_bytes()),
        scope: scope_to_strings(parsed.scope),
        channel_hash: parsed.channel_hash,
        not_before: parsed.not_before,
        not_after: parsed.not_after,
        delegation_depth: parsed.delegation_depth,
        nonce: parsed.nonce,
        signature_hex: hex::encode(parsed.signature),
    };
    write_json_out(&out, out_json, out_len)
}

/// Verify a serialized token's ed25519 signature. Writes `1` for
/// valid / `0` for tampered-or-wrong-subject. Time-bound validity is
/// a separate check — see `net_token_is_expired`.
#[unsafe(no_mangle)]
pub extern "C" fn net_verify_token(token: *const u8, len: usize, out_ok: *mut c_int) -> c_int {
    if token.is_null() || out_ok.is_null() {
        return NetError::NullPointer.into();
    }
    let slice = unsafe { std::slice::from_raw_parts(token, len) };
    let parsed = match PermissionToken::from_bytes(slice) {
        Ok(t) => t,
        Err(e) => return token_err_to_code(&e),
    };
    unsafe {
        *out_ok = if parsed.verify().is_ok() { 1 } else { 0 };
    }
    0
}

/// Writes `1` to `*out_expired` if the token's `not_after` has
/// passed; `0` otherwise. Pure time check — a tampered-but-expired
/// token still reports `1`. Use `net_verify_token` for signature
/// integrity.
#[unsafe(no_mangle)]
pub extern "C" fn net_token_is_expired(
    token: *const u8,
    len: usize,
    out_expired: *mut c_int,
) -> c_int {
    if token.is_null() || out_expired.is_null() {
        return NetError::NullPointer.into();
    }
    let slice = unsafe { std::slice::from_raw_parts(token, len) };
    let parsed = match PermissionToken::from_bytes(slice) {
        Ok(t) => t,
        Err(e) => return token_err_to_code(&e),
    };
    unsafe {
        *out_expired = if parsed.is_expired() { 1 } else { 0 };
    }
    0
}

/// Delegate a token to a new subject. Returns the child token blob;
/// caller frees via `net_free_bytes`.
#[unsafe(no_mangle)]
pub extern "C" fn net_delegate_token(
    signer: *mut IdentityHandle,
    parent: *const u8,
    parent_len: usize,
    new_subject: *const u8,
    new_subject_len: usize,
    restricted_scope_json: *const c_char,
    out_token: *mut *mut u8,
    out_token_len: *mut usize,
) -> c_int {
    if signer.is_null()
        || parent.is_null()
        || new_subject.is_null()
        || restricted_scope_json.is_null()
        || out_token.is_null()
        || out_token_len.is_null()
    {
        return NetError::NullPointer.into();
    }
    let parent_slice = unsafe { std::slice::from_raw_parts(parent, parent_len) };
    let parent_tok = match PermissionToken::from_bytes(parent_slice) {
        Ok(t) => t,
        Err(e) => return token_err_to_code(&e),
    };
    let Some(subject_id) = entity_id_from_bytes(new_subject, new_subject_len) else {
        return NET_ERR_IDENTITY;
    };
    let Some(scope_s) = (unsafe { c_str_to_string(restricted_scope_json) }) else {
        return NetError::InvalidUtf8.into();
    };
    let Some(scope) = parse_scope_list(&scope_s) else {
        return NET_ERR_IDENTITY;
    };
    let h = unsafe { &*signer };
    match parent_tok.delegate(&h.keypair, subject_id, scope) {
        Ok(child) => alloc_bytes(&child.to_bytes(), out_token, out_token_len),
        Err(e) => token_err_to_code(&e),
    }
}

/// Hash a channel name to its 16-bit wire-format value. Returns
/// `NET_ERR_IDENTITY` for invalid names.
#[unsafe(no_mangle)]
pub extern "C" fn net_channel_hash(channel: *const c_char, out_hash: *mut u16) -> c_int {
    if channel.is_null() || out_hash.is_null() {
        return NetError::NullPointer.into();
    }
    let Some(s) = (unsafe { c_str_to_string(channel) }) else {
        return NetError::InvalidUtf8.into();
    };
    let Some(hash) = channel_name_to_hash(&s) else {
        return NET_ERR_IDENTITY;
    };
    unsafe {
        *out_hash = hash;
    }
    0
}

// =========================================================================
// Capabilities (announce / find_nodes)
// =========================================================================

// Local alias to keep the capability helpers out of the mesh module's
// import list when the Go surface doesn't need them.
use crate::adapter::net::behavior::capability::{
    AcceleratorInfo, AcceleratorType, CapabilityFilter, CapabilitySet, GpuInfo, GpuVendor,
    HardwareCapabilities, Modality, ModelCapability, ResourceLimits, SoftwareCapabilities,
    ToolCapability,
};

// ----- enum helpers (byte-for-byte mirrors of PyO3/NAPI) ---------------------

fn parse_gpu_vendor_cap(s: &str) -> GpuVendor {
    match s.to_ascii_lowercase().as_str() {
        "nvidia" => GpuVendor::Nvidia,
        "amd" => GpuVendor::Amd,
        "intel" => GpuVendor::Intel,
        "apple" => GpuVendor::Apple,
        "qualcomm" => GpuVendor::Qualcomm,
        _ => GpuVendor::Unknown,
    }
}

fn gpu_vendor_to_string_cap(v: GpuVendor) -> &'static str {
    match v {
        GpuVendor::Nvidia => "nvidia",
        GpuVendor::Amd => "amd",
        GpuVendor::Intel => "intel",
        GpuVendor::Apple => "apple",
        GpuVendor::Qualcomm => "qualcomm",
        GpuVendor::Unknown => "unknown",
    }
}

fn parse_modality_cap(s: &str) -> Option<Modality> {
    match s.to_ascii_lowercase().as_str() {
        "text" => Some(Modality::Text),
        "image" => Some(Modality::Image),
        "audio" => Some(Modality::Audio),
        "video" => Some(Modality::Video),
        "code" => Some(Modality::Code),
        "embedding" => Some(Modality::Embedding),
        "tool-use" | "tool_use" | "tooluse" => Some(Modality::ToolUse),
        // Pre-fix unknown strings (typos) silently fell back to
        // `Modality::Text`. For announce-capabilities that meant
        // a node advertised "Text" support it didn't actually
        // have; for find-nodes filters that meant a typo'd
        // constraint (`require_modalities: ["audoi"]`) was
        // re-interpreted as "require Text" and returned the
        // wrong nodes. Now `None`; callers must handle the
        // unknown case explicitly.
        _ => None,
    }
}

fn parse_accelerator_type_cap(s: &str) -> AcceleratorType {
    match s.to_ascii_lowercase().as_str() {
        "tpu" => AcceleratorType::Tpu,
        "npu" => AcceleratorType::Npu,
        "fpga" => AcceleratorType::Fpga,
        "asic" => AcceleratorType::Asic,
        "dsp" => AcceleratorType::Dsp,
        _ => AcceleratorType::Unknown,
    }
}

// ----- JSON shapes -----------------------------------------------------------

#[derive(Deserialize, Default)]
struct CapabilitySetJson {
    #[serde(default)]
    hardware: Option<HardwareJson>,
    #[serde(default)]
    software: Option<SoftwareJson>,
    #[serde(default)]
    models: Vec<ModelJson>,
    #[serde(default)]
    tools: Vec<ToolJson>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    limits: Option<LimitsJson>,
}

#[derive(Deserialize, Default)]
struct HardwareJson {
    cpu_cores: Option<u32>,
    cpu_threads: Option<u32>,
    memory_mb: Option<u32>,
    gpu: Option<GpuJson>,
    #[serde(default)]
    additional_gpus: Vec<GpuJson>,
    storage_mb: Option<u64>,
    network_mbps: Option<u32>,
    #[serde(default)]
    accelerators: Vec<AcceleratorJson>,
}

#[derive(Deserialize)]
struct GpuJson {
    vendor: Option<String>,
    #[serde(default)]
    model: String,
    #[serde(default)]
    vram_mb: u32,
    compute_units: Option<u32>,
    tensor_cores: Option<u32>,
    fp16_tflops_x10: Option<u32>,
}

#[derive(Deserialize)]
struct AcceleratorJson {
    #[serde(default)]
    kind: String,
    #[serde(default)]
    model: String,
    memory_mb: Option<u32>,
    tops_x10: Option<u32>,
}

#[derive(Deserialize, Default)]
struct SoftwareJson {
    os: Option<String>,
    os_version: Option<String>,
    #[serde(default)]
    runtimes: Vec<Vec<String>>,
    #[serde(default)]
    frameworks: Vec<Vec<String>>,
    cuda_version: Option<String>,
    #[serde(default)]
    drivers: Vec<Vec<String>>,
}

#[derive(Deserialize)]
struct ModelJson {
    #[serde(default)]
    model_id: String,
    #[serde(default)]
    family: String,
    parameters_b_x10: Option<u32>,
    context_length: Option<u32>,
    quantization: Option<String>,
    #[serde(default)]
    modalities: Vec<String>,
    tokens_per_sec: Option<u32>,
    loaded: Option<bool>,
}

#[derive(Deserialize)]
struct ToolJson {
    #[serde(default)]
    tool_id: String,
    #[serde(default)]
    name: String,
    version: Option<String>,
    input_schema: Option<String>,
    output_schema: Option<String>,
    #[serde(default)]
    requires: Vec<String>,
    estimated_time_ms: Option<u32>,
    stateless: Option<bool>,
}

#[derive(Deserialize, Default)]
struct LimitsJson {
    max_concurrent_requests: Option<u32>,
    max_tokens_per_request: Option<u32>,
    rate_limit_rpm: Option<u32>,
    max_batch_size: Option<u32>,
    max_input_bytes: Option<u32>,
    max_output_bytes: Option<u32>,
}

#[derive(Deserialize, Default)]
struct CapabilityFilterJson {
    #[serde(default)]
    require_tags: Vec<String>,
    #[serde(default)]
    require_models: Vec<String>,
    #[serde(default)]
    require_tools: Vec<String>,
    min_memory_mb: Option<u32>,
    require_gpu: Option<bool>,
    gpu_vendor: Option<String>,
    min_vram_mb: Option<u32>,
    min_context_length: Option<u32>,
    #[serde(default)]
    require_modalities: Vec<String>,
}

// ----- Conversions -----------------------------------------------------------

fn pair_vec(xs: Vec<Vec<String>>) -> Vec<(String, String)> {
    xs.into_iter()
        .filter_map(|mut p| {
            if p.len() >= 2 {
                Some((std::mem::take(&mut p[0]), std::mem::take(&mut p[1])))
            } else {
                None
            }
        })
        .collect()
}

/// Clamp an untrusted JSON `u32` into a core `u16` field,
/// saturating at `u16::MAX`. Bare `as u16` silently wraps on
/// overflow — a Go caller reporting 65536 cores could land 0 on
/// the wire. Applied uniformly so every capability JSON
/// conversion is consistent with the NAPI + PyO3 paths.
#[inline]
fn saturating_u16_cap(v: u32) -> u16 {
    v.min(u16::MAX as u32) as u16
}

fn gpu_info_from_json(g: GpuJson) -> GpuInfo {
    let vendor = g
        .vendor
        .as_deref()
        .map(parse_gpu_vendor_cap)
        .unwrap_or(GpuVendor::Unknown);
    let mut info = GpuInfo::new(vendor, g.model, g.vram_mb);
    if let Some(cu) = g.compute_units {
        info = info.with_compute_units(saturating_u16_cap(cu));
    }
    if let Some(tc) = g.tensor_cores {
        info = info.with_tensor_cores(saturating_u16_cap(tc));
    }
    if let Some(tf) = g.fp16_tflops_x10 {
        // Saturate at `u16::MAX` before the f32 conversion. Pre-fix
        // `tf as f32` lost precision for u32 values ≥ 2²⁴ (f32 has
        // a 24-bit mantissa), so the round-trip
        // `u32 → f32/10.0 → with_fp16_tflops → *10.0 as u32`
        // could land a different `fp16_tflops_x10` than the
        // operator declared. The neighboring `tops_x10` field
        // already routes through `saturating_u16_cap` for the same
        // reason; the matching cap here keeps the round-trip exact
        // (u16::MAX = 65 535 is far below the f32 precision
        // boundary of 2²⁴ = 16 777 216) and aligns the two fields'
        // surfaces. The dynamic range loss (2³² → 2¹⁶) is
        // acceptable: 6 553.5 TFLOPS is far above any current or
        // near-future GPU's fp16 throughput.
        let tf_capped = saturating_u16_cap(tf);
        info = info.with_fp16_tflops(tf_capped as f32 / 10.0);
    }
    info
}

fn accelerator_from_json(a: AcceleratorJson) -> AcceleratorInfo {
    AcceleratorInfo {
        accel_type: parse_accelerator_type_cap(&a.kind),
        model: a.model,
        memory_mb: a.memory_mb.unwrap_or(0),
        tops_x10: a.tops_x10.map(saturating_u16_cap).unwrap_or(0),
    }
}

fn hardware_from_json(h: HardwareJson) -> HardwareCapabilities {
    let mut hw = HardwareCapabilities::new();
    match (h.cpu_cores, h.cpu_threads) {
        (Some(c), Some(t)) => hw = hw.with_cpu(saturating_u16_cap(c), saturating_u16_cap(t)),
        (Some(c), None) => {
            let c16 = saturating_u16_cap(c);
            hw = hw.with_cpu(c16, c16);
        }
        _ => {}
    }
    if let Some(mb) = h.memory_mb {
        hw = hw.with_memory(mb);
    }
    if let Some(g) = h.gpu {
        hw = hw.with_gpu(gpu_info_from_json(g));
    }
    for g in h.additional_gpus {
        hw = hw.add_gpu(gpu_info_from_json(g));
    }
    if let Some(mb) = h.storage_mb {
        hw = hw.with_storage(mb);
    }
    if let Some(mbps) = h.network_mbps {
        hw = hw.with_network(mbps);
    }
    for a in h.accelerators {
        hw = hw.add_accelerator(accelerator_from_json(a));
    }
    hw
}

fn software_from_json(s: SoftwareJson) -> SoftwareCapabilities {
    let mut sw = SoftwareCapabilities::new()
        .with_os(s.os.unwrap_or_default(), s.os_version.unwrap_or_default());
    for (k, v) in pair_vec(s.runtimes) {
        sw = sw.add_runtime(k, v);
    }
    for (k, v) in pair_vec(s.frameworks) {
        sw = sw.add_framework(k, v);
    }
    if let Some(c) = s.cuda_version {
        sw = sw.with_cuda(c);
    }
    sw.drivers = pair_vec(s.drivers);
    sw
}

fn model_from_json(m: ModelJson) -> ModelCapability {
    let mut mc = ModelCapability::new(m.model_id, m.family);
    if let Some(p) = m.parameters_b_x10 {
        mc.parameters_b_x10 = p;
    }
    if let Some(c) = m.context_length {
        mc = mc.with_context_length(c);
    }
    if let Some(q) = m.quantization {
        mc = mc.with_quantization(q);
    }
    for modality in m.modalities {
        match parse_modality_cap(&modality) {
            Some(parsed) => mc = mc.add_modality(parsed),
            None => {
                tracing::warn!(
                    modality = %modality,
                    "announce_capabilities: unknown modality string (typo?), \
                     skipping rather than the pre-fix silent fallback to Text — \
                     advertising a Text capability the node doesn't actually \
                     have produced wrong scheduling decisions on the receiver",
                );
            }
        }
    }
    if let Some(t) = m.tokens_per_sec {
        mc = mc.with_tokens_per_sec(t);
    }
    if let Some(l) = m.loaded {
        mc = mc.with_loaded(l);
    }
    mc
}

fn tool_from_json(t: ToolJson) -> ToolCapability {
    let mut tc = ToolCapability::new(t.tool_id, t.name);
    if let Some(v) = t.version {
        tc = tc.with_version(v);
    }
    if let Some(s) = t.input_schema {
        tc = tc.with_input_schema(s);
    }
    if let Some(s) = t.output_schema {
        tc = tc.with_output_schema(s);
    }
    for r in t.requires {
        tc = tc.requires(r);
    }
    if let Some(ms) = t.estimated_time_ms {
        tc = tc.with_estimated_time(ms);
    }
    if let Some(st) = t.stateless {
        tc = tc.with_stateless(st);
    }
    tc
}

fn limits_from_json(l: LimitsJson) -> ResourceLimits {
    let mut rl = ResourceLimits::new();
    if let Some(n) = l.max_concurrent_requests {
        rl = rl.with_max_concurrent(n);
    }
    if let Some(n) = l.max_tokens_per_request {
        rl = rl.with_max_tokens(n);
    }
    if let Some(n) = l.rate_limit_rpm {
        rl = rl.with_rate_limit(n);
    }
    if let Some(n) = l.max_batch_size {
        rl = rl.with_max_batch(n);
    }
    if let Some(n) = l.max_input_bytes {
        rl.max_input_bytes = n;
    }
    if let Some(n) = l.max_output_bytes {
        rl.max_output_bytes = n;
    }
    rl
}

fn capability_set_from_json(caps: CapabilitySetJson) -> CapabilitySet {
    let mut cs = CapabilitySet::new();
    if let Some(h) = caps.hardware {
        cs = cs.with_hardware(hardware_from_json(h));
    }
    if let Some(s) = caps.software {
        cs = cs.with_software(software_from_json(s));
    }
    for m in caps.models {
        cs = cs.add_model(model_from_json(m));
    }
    for t in caps.tools {
        cs = cs.add_tool(tool_from_json(t));
    }
    for tag in caps.tags {
        cs = cs.add_tag(tag);
    }
    if let Some(l) = caps.limits {
        cs = cs.with_limits(limits_from_json(l));
    }
    cs
}

fn capability_filter_from_json(f: CapabilityFilterJson) -> CapabilityFilter {
    let mut cf = CapabilityFilter::new();
    for t in f.require_tags {
        cf = cf.require_tag(t);
    }
    for m in f.require_models {
        cf = cf.require_model(m);
    }
    for t in f.require_tools {
        cf = cf.require_tool(t);
    }
    if let Some(mb) = f.min_memory_mb {
        cf = cf.with_min_memory(mb);
    }
    if f.require_gpu.unwrap_or(false) {
        cf = cf.require_gpu();
    }
    if let Some(v) = f.gpu_vendor {
        cf = cf.with_gpu_vendor(parse_gpu_vendor_cap(&v));
    }
    if let Some(mb) = f.min_vram_mb {
        cf = cf.with_min_vram(mb);
    }
    if let Some(n) = f.min_context_length {
        cf = cf.with_min_context(n);
    }
    for m in f.require_modalities {
        match parse_modality_cap(&m) {
            Some(parsed) => cf = cf.require_modality(parsed),
            None => {
                // For a filter, the lossy direction matters even
                // more than for announce: pre-fix the typo'd
                // string was re-interpreted as `require Text`,
                // returning Text-capable nodes that did NOT
                // satisfy the operator's intended constraint.
                // Skipping the unknown is also imperfect (the
                // resulting filter is too permissive — it
                // returns more nodes than intended), but the
                // failure mode is "scheduler matched too
                // broadly" rather than "scheduler matched the
                // wrong type." The loud warn surfaces the typo
                // so operators can fix it.
                tracing::warn!(
                    modality = %m,
                    "find_nodes: unknown modality string in require_modalities \
                     filter (typo?), dropping the constraint; the resulting \
                     filter is too permissive — pre-fix it was silently \
                     re-interpreted as `require Text`, which returned the \
                     wrong nodes",
                );
            }
        }
    }
    cf
}

// ----- Exports ---------------------------------------------------------------

pub(crate) const NET_ERR_CAPABILITY: c_int = -128;

/// Announce this node's capabilities to every directly-connected
/// peer. Also self-indexes, so `find_nodes` on the same node matches
/// on the announcement. Multi-hop propagation is deferred.
///
/// `caps_json` is the same POJO shape as PyO3 / NAPI:
/// `{hardware, software, models, tools, tags, limits}`.
#[unsafe(no_mangle)]
pub extern "C" fn net_mesh_announce_capabilities(
    handle: *mut MeshNodeHandle,
    caps_json: *const c_char,
) -> c_int {
    if handle.is_null() || caps_json.is_null() {
        return NetError::NullPointer.into();
    }
    let h = unsafe { &*handle };
    let Some(s) = (unsafe { c_str_to_string(caps_json) }) else {
        return NetError::InvalidUtf8.into();
    };
    let parsed: CapabilitySetJson = match serde_json::from_str(&s) {
        Ok(v) => v,
        Err(_) => return NetError::InvalidJson.into(),
    };
    let caps = capability_set_from_json(parsed);
    let node = h.inner.clone();
    match block_on(async move { node.announce_capabilities(caps).await }) {
        Ok(()) => 0,
        Err(_) => NET_ERR_CAPABILITY,
    }
}

/// Query the local capability index. Writes a JSON array of node
/// ids (u64) to `*out_json`; caller frees via `net_free_string`.
#[unsafe(no_mangle)]
pub extern "C" fn net_mesh_find_nodes(
    handle: *mut MeshNodeHandle,
    filter_json: *const c_char,
    out_json: *mut *mut c_char,
    out_len: *mut usize,
) -> c_int {
    if handle.is_null() || filter_json.is_null() || out_json.is_null() || out_len.is_null() {
        return NetError::NullPointer.into();
    }
    let h = unsafe { &*handle };
    let Some(s) = (unsafe { c_str_to_string(filter_json) }) else {
        return NetError::InvalidUtf8.into();
    };
    let parsed: CapabilityFilterJson = match serde_json::from_str(&s) {
        Ok(v) => v,
        Err(_) => return NetError::InvalidJson.into(),
    };
    let filter = capability_filter_from_json(parsed);
    let ids = h.inner.find_nodes_by_filter(&filter);
    write_json_out(&ids, out_json, out_len)
}

/// JSON shape of a [`ScopeFilter`] for the C ABI. Mirrors the
/// NAPI / PyO3 tagged-union form:
///
/// ```text
/// {"kind": "any"}
/// {"kind": "global_only"}
/// {"kind": "same_subnet"}
/// {"kind": "tenant", "tenant": "<id>"}
/// {"kind": "tenants", "tenants": ["<id>", ...]}
/// {"kind": "region", "region": "<name>"}
/// {"kind": "regions", "regions": ["<name>", ...]}
/// ```
///
/// Unrecognized `kind` values fall through to `Any` defensively;
/// empty strings or empty lists also collapse to `Any` (matches
/// the PyO3 / NAPI converters).
#[derive(serde::Deserialize)]
struct ScopeFilterJson {
    kind: String,
    #[serde(default)]
    tenant: Option<String>,
    #[serde(default)]
    tenants: Option<Vec<String>>,
    #[serde(default)]
    region: Option<String>,
    #[serde(default)]
    regions: Option<Vec<String>>,
}

/// Owned scope filter holding the strings the borrowed
/// [`net::adapter::net::behavior::capability::ScopeFilter`] points
/// into. Constructed inside [`net_mesh_find_nodes_scoped`] and
/// dropped at the end of the call so the borrow stays valid for
/// the query.
enum ScopeFilterOwned {
    Any,
    GlobalOnly,
    SameSubnet,
    Tenant(String),
    Tenants(Vec<String>),
    Region(String),
    Regions(Vec<String>),
}

fn scope_filter_from_json(f: ScopeFilterJson) -> ScopeFilterOwned {
    match f.kind.as_str() {
        "any" => ScopeFilterOwned::Any,
        "global_only" | "globalOnly" => ScopeFilterOwned::GlobalOnly,
        "same_subnet" | "sameSubnet" => ScopeFilterOwned::SameSubnet,
        "tenant" => match f.tenant {
            Some(t) if !t.is_empty() => ScopeFilterOwned::Tenant(t),
            _ => ScopeFilterOwned::Any,
        },
        "tenants" => match f.tenants {
            // Drop empty tenant ids — `scope_from_tags` rejects
            // empty announcements, so a query containing `[""]`
            // would never match a real tenant and would only pin
            // to Global candidates. Fall back to Any when cleaned
            // list is empty.
            Some(ts) => {
                let cleaned: Vec<String> = ts.into_iter().filter(|t| !t.is_empty()).collect();
                if cleaned.is_empty() {
                    ScopeFilterOwned::Any
                } else {
                    ScopeFilterOwned::Tenants(cleaned)
                }
            }
            None => ScopeFilterOwned::Any,
        },
        "region" => match f.region {
            Some(r) if !r.is_empty() => ScopeFilterOwned::Region(r),
            _ => ScopeFilterOwned::Any,
        },
        "regions" => match f.regions {
            // Same reasoning as `tenants` above.
            Some(rs) => {
                let cleaned: Vec<String> = rs.into_iter().filter(|r| !r.is_empty()).collect();
                if cleaned.is_empty() {
                    ScopeFilterOwned::Any
                } else {
                    ScopeFilterOwned::Regions(cleaned)
                }
            }
            None => ScopeFilterOwned::Any,
        },
        _ => ScopeFilterOwned::Any,
    }
}

/// Run `f` with a borrowed scope filter projected from `owned`.
/// Multi-element variants need an intermediate `Vec<&str>` that
/// outlives the borrow — that intermediate lives on this call's
/// stack, matching the NAPI / PyO3 helpers.
fn with_scope_filter<R>(
    owned: &ScopeFilterOwned,
    f: impl FnOnce(&crate::adapter::net::behavior::capability::ScopeFilter<'_>) -> R,
) -> R {
    use crate::adapter::net::behavior::capability::ScopeFilter as F;
    match owned {
        ScopeFilterOwned::Any => f(&F::Any),
        ScopeFilterOwned::GlobalOnly => f(&F::GlobalOnly),
        ScopeFilterOwned::SameSubnet => f(&F::SameSubnet),
        ScopeFilterOwned::Tenant(t) => f(&F::Tenant(t.as_str())),
        ScopeFilterOwned::Tenants(ts) => {
            let refs: Vec<&str> = ts.iter().map(|s| s.as_str()).collect();
            f(&F::Tenants(refs.as_slice()))
        }
        ScopeFilterOwned::Region(r) => f(&F::Region(r.as_str())),
        ScopeFilterOwned::Regions(rs) => {
            let refs: Vec<&str> = rs.iter().map(|s| s.as_str()).collect();
            f(&F::Regions(refs.as_slice()))
        }
    }
}

/// Scoped variant of [`net_mesh_find_nodes`]. Filters candidates
/// through a scope filter derived from each node's `scope:*`
/// reserved tags. Untagged nodes resolve to `Global` and stay
/// visible under most filters; nodes tagged `scope:subnet-local`
/// only show up under `{"kind":"same_subnet"}`.
///
/// `scope_json` is a tagged-union JSON form (see the private
/// `ScopeFilterJson` struct above):
///
/// ```text
/// {"kind": "any"}
/// {"kind": "global_only"}
/// {"kind": "same_subnet"}
/// {"kind": "tenant", "tenant": "<id>"}
/// {"kind": "tenants", "tenants": ["<id>", ...]}
/// {"kind": "region", "region": "<name>"}
/// {"kind": "regions", "regions": ["<name>", ...]}
/// ```
///
/// `filter_json` is the same shape as [`net_mesh_find_nodes`].
/// Result: JSON array of u64 node ids written to `*out_json`;
/// caller frees via `net_free_string`.
#[unsafe(no_mangle)]
pub extern "C" fn net_mesh_find_nodes_scoped(
    handle: *mut MeshNodeHandle,
    filter_json: *const c_char,
    scope_json: *const c_char,
    out_json: *mut *mut c_char,
    out_len: *mut usize,
) -> c_int {
    if handle.is_null()
        || filter_json.is_null()
        || scope_json.is_null()
        || out_json.is_null()
        || out_len.is_null()
    {
        return NetError::NullPointer.into();
    }
    let h = unsafe { &*handle };
    let Some(filter_s) = (unsafe { c_str_to_string(filter_json) }) else {
        return NetError::InvalidUtf8.into();
    };
    let Some(scope_s) = (unsafe { c_str_to_string(scope_json) }) else {
        return NetError::InvalidUtf8.into();
    };
    let parsed_filter: CapabilityFilterJson = match serde_json::from_str(&filter_s) {
        Ok(v) => v,
        Err(_) => return NetError::InvalidJson.into(),
    };
    let parsed_scope: ScopeFilterJson = match serde_json::from_str(&scope_s) {
        Ok(v) => v,
        Err(_) => return NetError::InvalidJson.into(),
    };
    let filter = capability_filter_from_json(parsed_filter);
    let owned = scope_filter_from_json(parsed_scope);
    let ids = with_scope_filter(&owned, |sf| {
        h.inner.find_nodes_by_filter_scoped(&filter, sf)
    });
    write_json_out(&ids, out_json, out_len)
}

/// JSON shape of [`CapabilityRequirement`] for the C ABI. Mirrors
/// the field set of the core type with snake_case keys; weights are
/// f32 in [0.0, 1.0] (the core clamps).
///
/// ```text
/// {
///   "filter": { … CapabilityFilter shape … },
///   "prefer_more_memory":     0.5,
///   "prefer_more_vram":       1.0,
///   "prefer_faster_inference": 0.0,
///   "prefer_loaded_models":   0.0
/// }
/// ```
#[derive(serde::Deserialize)]
struct CapabilityRequirementJson {
    #[serde(default)]
    filter: CapabilityFilterJson,
    #[serde(default)]
    prefer_more_memory: f32,
    #[serde(default)]
    prefer_more_vram: f32,
    #[serde(default)]
    prefer_faster_inference: f32,
    #[serde(default)]
    prefer_loaded_models: f32,
}

fn capability_requirement_from_json(
    j: CapabilityRequirementJson,
) -> crate::adapter::net::behavior::capability::CapabilityRequirement {
    crate::adapter::net::behavior::capability::CapabilityRequirement::from_filter(
        capability_filter_from_json(j.filter),
    )
    .prefer_memory(j.prefer_more_memory)
    .prefer_vram(j.prefer_more_vram)
    .prefer_speed(j.prefer_faster_inference)
    .prefer_loaded(j.prefer_loaded_models)
}

/// Pick the best-scoring node for a placement requirement. Writes
/// the winning node id to `*out_node_id` and `1` to `*out_has_match`
/// when a node matches; writes `0` to `*out_has_match` and leaves
/// `*out_node_id` untouched when no node matches. Returns `0` for
/// success in either case; non-zero only on input / parse error.
///
/// `requirement_json` is the JSON form documented on the private
/// `CapabilityRequirementJson` struct above — a `filter` object
/// plus four optional `prefer_*` weights in `[0.0, 1.0]`.
#[unsafe(no_mangle)]
pub extern "C" fn net_mesh_find_best_node(
    handle: *mut MeshNodeHandle,
    requirement_json: *const c_char,
    out_node_id: *mut u64,
    out_has_match: *mut c_int,
) -> c_int {
    if handle.is_null()
        || requirement_json.is_null()
        || out_node_id.is_null()
        || out_has_match.is_null()
    {
        return NetError::NullPointer.into();
    }
    let h = unsafe { &*handle };
    let Some(s) = (unsafe { c_str_to_string(requirement_json) }) else {
        return NetError::InvalidUtf8.into();
    };
    let parsed: CapabilityRequirementJson = match serde_json::from_str(&s) {
        Ok(v) => v,
        Err(_) => return NetError::InvalidJson.into(),
    };
    let req = capability_requirement_from_json(parsed);
    match h.inner.find_best_node(&req) {
        Some(node_id) => unsafe {
            *out_node_id = node_id;
            *out_has_match = 1;
        },
        None => unsafe {
            *out_has_match = 0;
        },
    }
    0
}

/// Scoped variant of [`net_mesh_find_best_node`]. Filters
/// candidates through `scope_json` (same shape as
/// [`net_mesh_find_nodes_scoped`]) before scoring; picks the
/// highest-scoring node within the scope-filtered set.
///
/// Same out-param contract as [`net_mesh_find_best_node`]:
/// `*out_has_match = 1` + `*out_node_id = winner` on hit;
/// `*out_has_match = 0` on no match.
#[unsafe(no_mangle)]
pub extern "C" fn net_mesh_find_best_node_scoped(
    handle: *mut MeshNodeHandle,
    requirement_json: *const c_char,
    scope_json: *const c_char,
    out_node_id: *mut u64,
    out_has_match: *mut c_int,
) -> c_int {
    if handle.is_null()
        || requirement_json.is_null()
        || scope_json.is_null()
        || out_node_id.is_null()
        || out_has_match.is_null()
    {
        return NetError::NullPointer.into();
    }
    let h = unsafe { &*handle };
    let Some(req_s) = (unsafe { c_str_to_string(requirement_json) }) else {
        return NetError::InvalidUtf8.into();
    };
    let Some(scope_s) = (unsafe { c_str_to_string(scope_json) }) else {
        return NetError::InvalidUtf8.into();
    };
    let parsed_req: CapabilityRequirementJson = match serde_json::from_str(&req_s) {
        Ok(v) => v,
        Err(_) => return NetError::InvalidJson.into(),
    };
    let parsed_scope: ScopeFilterJson = match serde_json::from_str(&scope_s) {
        Ok(v) => v,
        Err(_) => return NetError::InvalidJson.into(),
    };
    let req = capability_requirement_from_json(parsed_req);
    let owned = scope_filter_from_json(parsed_scope);
    let result = with_scope_filter(&owned, |sf| h.inner.find_best_node_scoped(&req, sf));
    match result {
        Some(node_id) => unsafe {
            *out_node_id = node_id;
            *out_has_match = 1;
        },
        None => unsafe {
            *out_has_match = 0;
        },
    }
    0
}

/// Normalize a GPU vendor string to its canonical lowercase form.
#[unsafe(no_mangle)]
pub extern "C" fn net_normalize_gpu_vendor(
    raw: *const c_char,
    out_json: *mut *mut c_char,
    out_len: *mut usize,
) -> c_int {
    if raw.is_null() || out_json.is_null() || out_len.is_null() {
        return NetError::NullPointer.into();
    }
    let Some(s) = (unsafe { c_str_to_string(raw) }) else {
        return NetError::InvalidUtf8.into();
    };
    let canonical = gpu_vendor_to_string_cap(parse_gpu_vendor_cap(&s));
    write_string_out(canonical.to_string(), out_json, out_len)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression for a cubic-flagged P2: Go-supplied JSON values
    /// wider than u16::MAX silently wrapped via `as u16` in
    /// `gpu_info_from_json` / `accelerator_from_json` /
    /// `hardware_from_json`, turning 65536 cores into 0. Every
    /// conversion site now routes through `saturating_u16_cap`.
    ///
    /// The NAPI binding has parallel end-to-end tests on
    /// `hardware_from_js`; the Go side verifies saturation in
    /// its own integration suite by round-tripping an overflow
    /// announcement through `announce_capabilities` (separate
    /// file).
    #[test]
    fn saturating_u16_cap_clamps_at_u16_max() {
        assert_eq!(saturating_u16_cap(0), 0);
        assert_eq!(saturating_u16_cap(42), 42);
        assert_eq!(saturating_u16_cap(u16::MAX as u32), u16::MAX);
        assert_eq!(saturating_u16_cap(u16::MAX as u32 + 1), u16::MAX);
        assert_eq!(saturating_u16_cap(u32::MAX), u16::MAX);
    }

    /// Regression: `parse_modality_cap` must surface unknown
    /// modality strings as `None`, not silently fall back to
    /// `Modality::Text`. Pre-fix a typo in announce-capabilities
    /// like `"audoi"` advertised a Text capability the node
    /// didn't have; in find-nodes filters, the same typo was
    /// reinterpreted as `require Text` and returned the wrong
    /// nodes. The strict shape lets callers handle the unknown
    /// case explicitly (callers in this file warn-and-skip).
    #[test]
    fn parse_modality_cap_returns_none_on_unknown_strings() {
        // Known values still parse.
        for (s, expected) in [
            ("text", Modality::Text),
            ("Text", Modality::Text),
            ("TEXT", Modality::Text),
            ("image", Modality::Image),
            ("audio", Modality::Audio),
            ("video", Modality::Video),
            ("code", Modality::Code),
            ("embedding", Modality::Embedding),
            ("tool-use", Modality::ToolUse),
            ("tool_use", Modality::ToolUse),
            ("tooluse", Modality::ToolUse),
        ] {
            assert_eq!(
                parse_modality_cap(s),
                Some(expected),
                "known modality `{s}` must parse",
            );
        }

        // Typos and unknowns return None, NOT Modality::Text.
        for s in ["audoi", "imageX", "vidoe", "embeding", "garbage", ""] {
            assert_eq!(
                parse_modality_cap(s),
                None,
                "unknown modality `{s}` must return None — pre-fix this \
                 fell back to Modality::Text, advertising a capability \
                 the node didn't actually have",
            );
        }
    }

    /// Regression: `gpu_info_from_json` must saturate large
    /// `fp16_tflops_x10` values at `u16::MAX` before the f32
    /// conversion. Pre-fix `tf as f32` lost precision for u32
    /// values above 2²⁴ (f32 has a 24-bit mantissa) — the
    /// round-trip `u32 → f32/10.0 → with_fp16_tflops → *10.0
    /// as u32` could land a different `fp16_tflops_x10` than
    /// the operator declared. The matching saturation aligns
    /// with the neighboring `tops_x10` field's surface and
    /// keeps the round-trip exact.
    #[test]
    fn gpu_info_from_json_saturates_fp16_tflops_to_u16_max() {
        // A hostile or just unrealistically large value well
        // above the f32 precision boundary (2^24 = 16_777_216).
        let g = GpuJson {
            vendor: None,
            model: "test".to_string(),
            vram_mb: 0,
            compute_units: None,
            tensor_cores: None,
            fp16_tflops_x10: Some(1_000_000_000u32),
        };
        let info = gpu_info_from_json(g);
        // The cap is u16::MAX = 65535; the f32 round-trip back to
        // x10 storage must reproduce 65_535, NOT some lossily
        // rounded approximation of 1_000_000_000.
        assert_eq!(
            info.fp16_tflops_x10,
            u16::MAX as u32,
            "fp16_tflops_x10 must saturate at u16::MAX (65535) instead of \
             losing precision through the f32 round-trip; got {}",
            info.fp16_tflops_x10,
        );

        // Sanity: a small in-range value round-trips exactly.
        let g_small = GpuJson {
            vendor: None,
            model: "test".to_string(),
            vram_mb: 0,
            compute_units: None,
            tensor_cores: None,
            fp16_tflops_x10: Some(425), // 42.5 TFLOPS
        };
        let info_small = gpu_info_from_json(g_small);
        assert_eq!(
            info_small.fp16_tflops_x10, 425,
            "small fp16_tflops_x10 must round-trip exactly"
        );
    }

    /// Regression: `alloc_bytes` used to call `Vec::shrink_to_fit`
    /// and then hand the raw `(ptr, len)` to C, expecting
    /// `net_free_bytes` to reconstruct with
    /// `Vec::from_raw_parts(ptr, len, len)`. `shrink_to_fit` is not
    /// guaranteed to make `capacity == len`, so the reconstruction
    /// could UB on drop (allocator size mismatch). The fix uses
    /// `Layout::array::<u8>(len)` on both sides so the capacity is
    /// always exactly `len`.
    ///
    /// This test exercises the alloc/free round-trip across a range
    /// of sizes; under miri (or with the system allocator) any size
    /// mismatch would surface here.
    #[test]
    fn alloc_bytes_round_trip_across_sizes() {
        for size in [0usize, 1, 15, 16, 17, 32, 64, 1024, 8192] {
            let src: Vec<u8> = (0..size).map(|i| (i as u8).wrapping_mul(37)).collect();
            let mut ptr: *mut u8 = std::ptr::null_mut();
            let mut len: usize = 0;
            let rc = alloc_bytes(&src, &mut ptr as *mut _, &mut len as *mut _);
            assert_eq!(rc, 0);
            assert_eq!(len, size);
            if size == 0 {
                assert!(ptr.is_null());
            } else {
                assert!(!ptr.is_null());
                let observed = unsafe { std::slice::from_raw_parts(ptr, len) };
                assert_eq!(observed, &src[..]);
            }
            // Freeing with a null or zero-len must be a no-op; freeing
            // a real buffer must not abort or corrupt the allocator.
            net_free_bytes(ptr, len);
        }
    }

    #[test]
    fn net_free_bytes_null_and_zero_len_are_noops() {
        // Both explicitly documented as safe no-ops.
        net_free_bytes(std::ptr::null_mut(), 0);
        net_free_bytes(std::ptr::null_mut(), 42);
        // A non-null pointer with len == 0 is also a no-op — we must
        // not try to free it, since we never allocated.
        let mut sentinel: u8 = 0;
        net_free_bytes(&mut sentinel as *mut u8, 0);
    }

    /// `net_free_bytes` must NOT panic when called with a
    /// `len` larger than `isize::MAX`. Pre-fix
    /// `Layout::array::<u8>(len).expect(...)` panicked on such
    /// values (a documented `Layout::array` failure mode); the
    /// panic would unwind across the `extern "C"` boundary into
    /// any non-Rust caller (C / Go-cgo / NAPI / PyO3) — undefined
    /// behaviour. Now the function silently no-ops on
    /// `Layout::array` failure: an allocation of that size could
    /// not have come from this process under matching layout
    /// rules, so it's already memory-corruption territory and
    /// abandoning the free is the safest response.
    #[test]
    fn net_free_bytes_does_not_panic_on_oversized_len() {
        // We can't actually allocate a buffer of `isize::MAX + 1`
        // bytes to free; the fix's load-bearing check is that the
        // function reaches the `Err(_) => return` branch instead
        // of panicking. Pass a non-null pointer with an oversized
        // len; with the old `expect("byte layout")` this panics.
        // We use a stack sentinel as the pointer — the function
        // must short-circuit without touching it.
        let mut sentinel: u8 = 0;
        let ptr = &mut sentinel as *mut u8;
        // `usize::MAX` is well past `isize::MAX`, so
        // `Layout::array::<u8>(usize::MAX)` is `Err(LayoutError)`.
        net_free_bytes(ptr, usize::MAX);
        // If we got here without panicking, the fix is in place.
        // Sentinel must still be untouched (we never tried to free).
        assert_eq!(sentinel, 0, "sentinel must not have been written through");
    }

    /// Regression for a cubic-flagged P1: `net_mesh_shutdown`
    /// previously returned success (0) without actually shutting
    /// the node down whenever `Arc::strong_count(&inner) > 1`
    /// (e.g. the FFI caller was holding a stream handle). The real
    /// shutdown was silently skipped, so background tasks kept
    /// draining UDP and consuming CPU. This test holds an extra
    /// `Arc` clone, calls `net_mesh_shutdown`, and asserts the
    /// shutdown flag flipped.
    #[test]
    fn net_mesh_shutdown_runs_even_with_outstanding_arc_refs() {
        let cfg = serde_json::json!({
            "bind_addr": "127.0.0.1:0",
            "psk_hex": "0".repeat(64),
        });
        let cfg_c = CString::new(cfg.to_string()).unwrap();
        let mut out: *mut MeshNodeHandle = std::ptr::null_mut();
        let rc = net_mesh_new(cfg_c.as_ptr(), &mut out);
        assert_eq!(rc, 0, "net_mesh_new failed: {rc}");
        assert!(!out.is_null());

        // Clone the inner Arc so strong_count > 1 — this is what a
        // live stream handle would look like from the guard's POV.
        let inner_clone = {
            let h = unsafe { &*out };
            h.inner.clone()
        };
        assert!(Arc::strong_count(&inner_clone) >= 2);
        assert!(!inner_clone.is_shutdown());

        let rc = net_mesh_shutdown(out);
        assert_eq!(rc, 0, "net_mesh_shutdown returned {rc}");
        assert!(
            inner_clone.is_shutdown(),
            "shutdown flag must be set even when extra Arc refs are outstanding"
        );

        drop(inner_clone);
        let _ = unsafe { Box::from_raw(out) };
    }

    /// Regression: BUG_REPORT.md #19 — `net_mesh_send` family
    /// accepted any `(MeshStreamHandle, MeshNodeHandle)` pair and
    /// sent through the supplied node, regardless of whether the
    /// stream was opened on it. The fix uses `Arc::ptr_eq` to
    /// require the stream's cached `_node` to match the supplied
    /// node handle's inner `Arc`.
    ///
    /// Build two distinct nodes via the FFI constructor (so all
    /// the internal fields are populated correctly), open a stream
    /// on the first, then verify `handles_match` accepts the
    /// matched pair and rejects the cross-pair.
    #[test]
    fn handles_match_rejects_stream_node_mismatch() {
        fn make_node_handle() -> *mut MeshNodeHandle {
            let cfg = serde_json::json!({
                "bind_addr": "127.0.0.1:0",
                "psk_hex": "0".repeat(64),
            });
            let cfg_c = CString::new(cfg.to_string()).unwrap();
            let mut out: *mut MeshNodeHandle = std::ptr::null_mut();
            let rc = net_mesh_new(cfg_c.as_ptr(), &mut out);
            assert_eq!(rc, 0);
            assert!(!out.is_null());
            out
        }

        let nh_a = make_node_handle();
        let nh_b = make_node_handle();

        // Build a stream handle whose `_node` Arc is node_a's
        // inner. We can't go through `open_stream` here because
        // that requires an established session with the peer
        // (which the unit test can't synthesize), but `handles_match`
        // only inspects the cached `_node` Arc — the stream fields
        // are irrelevant to the check. Direct field init is fine
        // since we're in the same module.
        let sh_a = {
            let h = unsafe { &*nh_a };
            MeshStreamHandle {
                stream: CoreStream {
                    peer_node_id: 0xDEAD,
                    stream_id: 1,
                    epoch: 0,
                    config: StreamConfig::new(),
                },
                _node: h.inner.clone(),
            }
        };

        // Matched pair: stream's _node == nh_a.inner — accepted.
        assert!(
            handles_match(&sh_a, unsafe { &*nh_a }),
            "stream from node_a + node_a handle must match"
        );
        // Mismatched pair: stream's _node != nh_b.inner — rejected.
        assert!(
            !handles_match(&sh_a, unsafe { &*nh_b }),
            "stream from node_a + node_b handle must be rejected (#19)"
        );

        // Cleanup: drop the boxes.
        let _ = unsafe { Box::from_raw(nh_a) };
        let _ = unsafe { Box::from_raw(nh_b) };
    }

    #[test]
    fn hardware_from_json_saturates_overflow_cpu_fields() {
        // 70_000 > u16::MAX (65_535). Pre-fix: 70_000 as u16 = 4464.
        // Post-fix: saturates to 65_535.
        let h = HardwareJson {
            cpu_cores: Some(70_000),
            cpu_threads: Some(200_000),
            memory_mb: None,
            gpu: None,
            additional_gpus: Vec::new(),
            storage_mb: None,
            network_mbps: None,
            accelerators: Vec::new(),
        };
        let hw = hardware_from_json(h);
        assert_eq!(hw.cpu_cores, u16::MAX);
        assert_eq!(hw.cpu_threads, u16::MAX);
    }
}

#[cfg(all(test, not(feature = "nat-traversal")))]
mod nat_traversal_stub_tests {
    //! Regression coverage for cubic-flagged P1 Bug L: the Go /
    //! NAPI / PyO3 bindings unconditionally link against the
    //! `net_mesh_nat_type` / `net_mesh_connect_direct` / ...
    //! symbols. Without these stubs, a cdylib built without
    //! `--features nat-traversal` failed at dlopen with a missing-
    //! symbol error, contradicting the binding docs' promise of
    //! `ErrTraversalUnsupported` at runtime.
    //!
    //! Each test here asserts the stub resolves *and* returns
    //! [`super::NET_ERR_TRAVERSAL_UNSUPPORTED`] (-137) — the exact
    //! value the Go / NAPI / PyO3 translation layers map to their
    //! respective `Unsupported` sentinels.
    //!
    //! Only compiled in the no-feature build; the feature-on path
    //! has different semantics (real NAT-traversal work) tested
    //! elsewhere.
    use super::*;
    use std::ptr;

    #[test]
    fn nat_type_stub_returns_unsupported() {
        let mut out_str: *mut c_char = ptr::null_mut();
        let mut out_len: usize = 0;
        let code = net_mesh_nat_type(ptr::null_mut(), &mut out_str, &mut out_len);
        assert_eq!(code, NET_ERR_TRAVERSAL_UNSUPPORTED);
    }

    #[test]
    fn reflex_addr_stub_returns_unsupported() {
        let mut out_str: *mut c_char = ptr::null_mut();
        let mut out_len: usize = 0;
        let code = net_mesh_reflex_addr(ptr::null_mut(), &mut out_str, &mut out_len);
        assert_eq!(code, NET_ERR_TRAVERSAL_UNSUPPORTED);
    }

    #[test]
    fn peer_nat_type_stub_returns_unsupported() {
        let mut out_str: *mut c_char = ptr::null_mut();
        let mut out_len: usize = 0;
        let code = net_mesh_peer_nat_type(ptr::null_mut(), 0, &mut out_str, &mut out_len);
        assert_eq!(code, NET_ERR_TRAVERSAL_UNSUPPORTED);
    }

    #[test]
    fn probe_reflex_stub_returns_unsupported() {
        let mut out_str: *mut c_char = ptr::null_mut();
        let mut out_len: usize = 0;
        let code = net_mesh_probe_reflex(ptr::null_mut(), 0, &mut out_str, &mut out_len);
        assert_eq!(code, NET_ERR_TRAVERSAL_UNSUPPORTED);
    }

    #[test]
    fn reclassify_nat_stub_returns_unsupported() {
        let code = net_mesh_reclassify_nat(ptr::null_mut());
        assert_eq!(code, NET_ERR_TRAVERSAL_UNSUPPORTED);
    }

    #[test]
    fn traversal_stats_stub_returns_unsupported() {
        let mut a: u64 = 0;
        let mut b: u64 = 0;
        let mut c: u64 = 0;
        let code = net_mesh_traversal_stats(ptr::null_mut(), &mut a, &mut b, &mut c);
        assert_eq!(code, NET_ERR_TRAVERSAL_UNSUPPORTED);
    }

    #[test]
    fn connect_direct_stub_returns_unsupported() {
        let code = net_mesh_connect_direct(ptr::null_mut(), 0, ptr::null(), 0);
        assert_eq!(code, NET_ERR_TRAVERSAL_UNSUPPORTED);
    }

    #[test]
    fn set_reflex_override_stub_returns_unsupported() {
        let code = net_mesh_set_reflex_override(ptr::null_mut(), ptr::null());
        assert_eq!(code, NET_ERR_TRAVERSAL_UNSUPPORTED);
    }

    #[test]
    fn clear_reflex_override_stub_returns_unsupported() {
        let code = net_mesh_clear_reflex_override(ptr::null_mut());
        assert_eq!(code, NET_ERR_TRAVERSAL_UNSUPPORTED);
    }

    /// Pins the constant itself. If anyone ever renumbers
    /// `NET_ERR_TRAVERSAL_UNSUPPORTED`, every Go / NAPI / PyO3
    /// binding's error translation silently breaks — the stubs
    /// return the new value but the mapping layers are hardcoded
    /// to -137.
    #[test]
    fn unsupported_code_is_stable() {
        assert_eq!(NET_ERR_TRAVERSAL_UNSUPPORTED, -137);
    }

    /// Repro for the failing Go `TestHardwareAndGpuFilter_Matches`:
    /// parse the exact JSON the Go binding marshals, convert via
    /// the FFI helpers, then verify the GpuVendor lands as Nvidia.
    #[test]
    fn capability_set_from_go_marshal_preserves_gpu_vendor() {
        let json = r#"{"hardware":{"cpu_cores":16,"memory_mb":65536,"gpu":{"vendor":"nvidia","model":"h100","vram_mb":81920}},"tags":["gpu"]}"#;
        let parsed: CapabilitySetJson = serde_json::from_str(json).expect("JSON should parse");
        let caps = capability_set_from_json(parsed);
        assert_eq!(
            caps.hardware.gpu_vendor(),
            Some(super::GpuVendor::Nvidia),
            "vendor lost in conversion"
        );
        assert_eq!(caps.hardware.memory_mb, 65536);
        assert_eq!(caps.hardware.total_vram_mb(), 81920);
        assert!(caps.has_tag("gpu"));
    }

    /// Regression: BUG_REPORT.md #15 — `collect_payloads` previously
    /// dereferenced every per-entry pointer without a null check, so a C
    /// caller passing an array containing a null entry produced UB on
    /// `from_raw_parts(null, len)`. The fix returns `None` for any null
    /// pointer with non-zero length so the caller can return
    /// `NetError::NullPointer`. A null pointer with length 0 is treated
    /// as an empty payload (allowed because the pointer is never
    /// dereferenced).
    #[test]
    fn collect_payloads_rejects_null_entry_with_nonzero_length() {
        let buf_a = b"hello".as_slice();
        let buf_b = b"world".as_slice();
        let ptrs: [*const u8; 3] = [buf_a.as_ptr(), std::ptr::null(), buf_b.as_ptr()];
        let lens: [usize; 3] = [buf_a.len(), 4, buf_b.len()];

        let result = unsafe { collect_payloads(ptrs.as_ptr(), lens.as_ptr(), 3) };
        assert!(
            result.is_none(),
            "null entry with non-zero length must reject the whole batch"
        );
    }

    #[test]
    fn collect_payloads_allows_null_entry_with_zero_length() {
        let buf_a = b"hello".as_slice();
        let ptrs: [*const u8; 2] = [buf_a.as_ptr(), std::ptr::null()];
        let lens: [usize; 2] = [buf_a.len(), 0];

        let result = unsafe { collect_payloads(ptrs.as_ptr(), lens.as_ptr(), 2) }
            .expect("zero-length null is treated as empty payload");
        assert_eq!(result.len(), 2);
        assert_eq!(&result[0][..], b"hello");
        assert!(result[1].is_empty());
    }

    #[test]
    fn collect_payloads_happy_path() {
        let buf_a = b"abc".as_slice();
        let buf_b = b"defg".as_slice();
        let ptrs: [*const u8; 2] = [buf_a.as_ptr(), buf_b.as_ptr()];
        let lens: [usize; 2] = [buf_a.len(), buf_b.len()];

        let result = unsafe { collect_payloads(ptrs.as_ptr(), lens.as_ptr(), 2) }
            .expect("non-null entries should succeed");
        assert_eq!(result.len(), 2);
        assert_eq!(&result[0][..], b"abc");
        assert_eq!(&result[1][..], b"defg");
    }
}
