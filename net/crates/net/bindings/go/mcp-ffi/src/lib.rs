//! C FFI for the MCP bridge pure helpers + the graduated consent / pin
//! surface (`MCP_BRIDGE_SDK_PLAN.md` P3).
//!
//! This is the C face of exactly what the Python
//! (`bindings/python/src/{consent,mcp_helpers}.rs`) and Node
//! (`bindings/node/src/{consent,mcp_helpers}.rs`) bindings expose — the one
//! Rust implementation, three faces (doctrine #1: no logic in bindings).
//!
//! # Scope
//!
//! - **Pure helpers**: `net_mcp_classify` (credential-risk scoring a native
//!   node may DISPLAY before publishing) and `net_mcp_lower_tool` (an MCP
//!   `tools/list` entry lowered to the Net `ToolDescriptor` + bridge
//!   metadata). No mesh, no process, no secret crosses — the forwarding /
//!   keychain internals are never bound (bridge doctrine #3).
//! - **Consent gate**: `net_mcp_credential_requires_consent`, a
//!   `net_mcp_cap_id_canonicalize` identity helper, and an opaque
//!   `ConsentPolicy` handle.
//! - **Pin store**: path-scoped functions over `net_sdk::pins::PinStore`.
//!   Every mutation runs the core's full locked `mutate` transaction
//!   (block-on a shared current-thread runtime); the store file is never
//!   opened here directly, so the same file the `net mcp pin` CLI and a
//!   running `net mcp serve` shim use is honored bidirectionally.
//!
//! # Symbol naming
//!
//! `net_mcp_<noun>_<verb>` (the lib name is `net_mcp_ffi` to avoid an rlib
//! collision with the `net-mesh-mcp` adapter's `net_mcp` lib, but the C
//! symbols keep the `net_mcp_` prefix).
//!
//! # Error model
//!
//! Functions returning a `*mut c_char` yield NULL on error (and, for
//! [`net_mcp_pin_state`], an empty string `""` for "no record" — states are
//! never empty, so it is unambiguous). Functions returning `c_int` use
//! `-1` for error; `>= 0` is the result. The detail is fetched via
//! [`net_mcp_last_error_message`] / [`net_mcp_last_error_kind`] (the latest
//! pair on the calling thread; the pointer is valid until the next FFI call
//! on the same thread). Every entry point clears the last-error at the top,
//! so a NULL / `-1` with no last-error set means "not an error" (e.g. an
//! absent pin record).

use std::cell::RefCell;
use std::ffi::{c_char, c_int, CStr, CString};
use std::ptr;
use std::sync::OnceLock;

use serde::Serialize;

use net_mcp::wrap::{
    classify, lower_tool, CredentialOverride, CredentialStatus, LoweringContext, Substitutability,
    WrapEnv,
};
use net_sdk::consent::{
    CapabilityId, ConsentPolicy as CoreConsentPolicy, CredentialStatus as SdkCredentialStatus,
};
use net_sdk::pins::{PinState, PinStore};

// =====================================================================
// Thread-local last-error (message + kind), mirroring net_meshdb.
// =====================================================================

thread_local! {
    static LAST_ERROR_MESSAGE: RefCell<Option<CString>> = const { RefCell::new(None) };
    static LAST_ERROR_KIND: RefCell<Option<CString>> = const { RefCell::new(None) };
}

fn set_last_error(message: impl Into<String>, kind: &str) {
    let msg = CString::new(message.into()).ok();
    let kind = CString::new(kind).ok();
    LAST_ERROR_MESSAGE.with(|c| *c.borrow_mut() = msg);
    LAST_ERROR_KIND.with(|c| *c.borrow_mut() = kind);
}

fn clear_last_error() {
    LAST_ERROR_MESSAGE.with(|c| *c.borrow_mut() = None);
    LAST_ERROR_KIND.with(|c| *c.borrow_mut() = None);
}

fn set_last_error_from_panic(payload: &(dyn std::any::Any + Send)) {
    let detail = if let Some(s) = payload.downcast_ref::<&'static str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "panic across FFI boundary".to_string()
    };
    set_last_error(format!("runtime panic: {detail}"), "runtime_panic");
}

/// Return the most recent error message recorded on this thread, or NULL.
/// The pointer is valid until the next FFI call on the same thread touches
/// the thread-local; callers must NOT free it.
#[no_mangle]
pub extern "C" fn net_mcp_last_error_message() -> *const c_char {
    LAST_ERROR_MESSAGE.with(|c| match &*c.borrow() {
        Some(s) => s.as_ptr(),
        None => ptr::null(),
    })
}

/// Return the most recent error kind recorded on this thread, or NULL. Same
/// lifetime rules as [`net_mcp_last_error_message`].
#[no_mangle]
pub extern "C" fn net_mcp_last_error_kind() -> *const c_char {
    LAST_ERROR_KIND.with(|c| match &*c.borrow() {
        Some(s) => s.as_ptr(),
        None => ptr::null(),
    })
}

/// Clear the thread-local last-error state.
#[no_mangle]
pub extern "C" fn net_mcp_clear_last_error() {
    clear_last_error();
}

/// Wrap an FFI body in `catch_unwind` — unwinding across `extern "C"` is UB.
/// A trapped panic records the last-error pair with kind `"runtime_panic"`
/// and returns `$default`.
macro_rules! ffi_guard {
    ($default:expr, $body:block) => {{
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| $body)) {
            Ok(v) => v,
            Err(payload) => {
                set_last_error_from_panic(&*payload);
                $default
            }
        }
    }};
}

// =====================================================================
// Marshaling helpers
// =====================================================================

/// Borrow a required C string as `&str`. On null / bad-UTF-8 it records the
/// last-error and returns `None`.
///
/// # Safety
/// A non-null `ptr` must be a NUL-terminated C string valid for at least as
/// long as the returned borrow is used. `unsafe fn` because the returned
/// `&'a str` lifetime is caller-chosen and unconnected to the raw pointer — the
/// caller must not hold it past the pointer's validity.
unsafe fn cstr<'a>(ptr: *const c_char, arg: &str) -> Option<&'a str> {
    if ptr.is_null() {
        set_last_error(format!("{arg} must not be null"), "invalid_arg");
        return None;
    }
    match CStr::from_ptr(ptr).to_str() {
        Ok(s) => Some(s),
        Err(_) => {
            set_last_error(format!("{arg} is not valid UTF-8"), "invalid_arg");
            None
        }
    }
}

/// Borrow an optional C string: null → `Ok(None)` (the absent/default case, no
/// error). A non-null pointer must be valid UTF-8; bad UTF-8 records the
/// last-error and returns `Err(())` so the caller fails instead of silently
/// coercing a malformed argument to the default (matching `cstr`, which
/// already errors on bad UTF-8).
///
/// # Safety
/// As [`cstr`].
unsafe fn opt_cstr<'a>(ptr: *const c_char, arg: &str) -> Result<Option<&'a str>, ()> {
    if ptr.is_null() {
        return Ok(None);
    }
    match CStr::from_ptr(ptr).to_str() {
        Ok(s) => Ok(Some(s)),
        Err(_) => {
            set_last_error(format!("{arg} is not valid UTF-8"), "invalid_arg");
            Err(())
        }
    }
}

/// Turn an owned `String` into a heap C string for return; NULL if it
/// contained an interior NUL (records the last-error).
fn into_cstr(s: String) -> *mut c_char {
    match CString::new(s) {
        Ok(c) => c.into_raw(),
        Err(_) => {
            set_last_error("result contained an interior NUL byte", "encode_error");
            ptr::null_mut()
        }
    }
}

/// The shared runtime the pin-store `block_on` calls use. Multi-threaded so
/// concurrent FFI calls (the C ABI is callable from any thread) can each
/// `block_on` without contending on a single-threaded scheduler core; the
/// store's own cross-process advisory lock is what actually serializes
/// writers.
fn runtime() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("build pin-store runtime")
    })
}

/// Parse (and canonicalize) a `provider/capability` id, recording the
/// last-error with kind `"invalid_arg"` on failure.
fn parse_cap(display: &str) -> Option<CapabilityId> {
    match CapabilityId::parse(display) {
        Ok(id) => Some(id),
        Err(e) => {
            set_last_error(e.to_string(), "invalid_arg");
            None
        }
    }
}

/// The stable string form of a pin state (the SDK's — never re-tabulated).
fn pin_state_str(state: PinState) -> &'static str {
    state.as_str()
}

// =====================================================================
// Free functions
// =====================================================================

/// Free a string returned by any `net_mcp_*` function that yields a
/// `char*`. No-op on null.
///
/// # Safety
/// `s` must be a pointer returned by this library, or null.
#[no_mangle]
pub unsafe extern "C" fn net_mcp_free_string(s: *mut c_char) {
    clear_last_error();
    ffi_guard!((), {
        if !s.is_null() {
            drop(CString::from_raw(s));
        }
    })
}

// =====================================================================
// Pure helpers — classify / lower_tool
// =====================================================================

/// Classify a wrapped MCP server's credential exposure. Returns the status
/// label (`"credentialed"` / `"external_api"` / `"unknown"` / `"none"`) as
/// an owned C string, or NULL on error.
///
/// `args_json` is a JSON array of strings; `envs_json` is a JSON object of
/// `{ "KEY": "VALUE" }` env additions (only the KEYS drive detection — the
/// value is never inspected beyond presence and never appears in the
/// result). `credential_override` is `"detect"` (or null) / `"credentialed"`
/// / `"no-credentials"`; a downward override needs `force != 0`, mirroring
/// `net wrap --no-credentials --force`.
///
/// Free the return with [`net_mcp_free_string`].
///
/// # Safety
/// All non-null pointers must be valid NUL-terminated C strings.
#[no_mangle]
pub unsafe extern "C" fn net_mcp_classify(
    program: *const c_char,
    args_json: *const c_char,
    envs_json: *const c_char,
    credential_override: *const c_char,
    force: c_int,
) -> *mut c_char {
    clear_last_error();
    ffi_guard!(ptr::null_mut(), {
        let Some(program) = cstr(program, "program") else {
            return ptr::null_mut();
        };
        let args: Vec<String> = match opt_cstr(args_json, "args_json") {
            Err(()) => return ptr::null_mut(),
            Ok(Some(s)) if !s.trim().is_empty() => match serde_json::from_str(s) {
                Ok(v) => v,
                Err(e) => {
                    set_last_error(format!("args_json: {e}"), "invalid_arg");
                    return ptr::null_mut();
                }
            },
            Ok(_) => Vec::new(),
        };
        let envs: Vec<(String, String)> = match opt_cstr(envs_json, "envs_json") {
            Err(()) => return ptr::null_mut(),
            Ok(Some(s)) if !s.trim().is_empty() => {
                match serde_json::from_str::<std::collections::BTreeMap<String, String>>(s) {
                    Ok(m) => m.into_iter().collect(),
                    Err(e) => {
                        set_last_error(format!("envs_json: {e}"), "invalid_arg");
                        return ptr::null_mut();
                    }
                }
            }
            Ok(_) => Vec::new(),
        };
        let over = match opt_cstr(credential_override, "credential_override") {
            Err(()) => return ptr::null_mut(),
            Ok(None) => CredentialOverride::Detect,
            Ok(Some(label)) => match CredentialOverride::from_wire(label) {
                Some(o) => o,
                None => {
                    set_last_error(
                        format!(
                            "unknown credential_override {label:?} (expected {})",
                            CredentialOverride::EXPECTED
                        ),
                        "invalid_arg",
                    );
                    return ptr::null_mut();
                }
            },
        };
        match classify(
            &WrapEnv {
                program,
                args: &args,
                envs: &envs,
            },
            over,
            force != 0,
        ) {
            Ok(status) => into_cstr(status.as_str().to_string()),
            Err(e) => {
                set_last_error(e.to_string(), "classify_error");
                ptr::null_mut()
            }
        }
    })
}

/// The lowered-tool JSON DTO. `descriptor` is a nested object (the Net
/// discovery descriptor); `bridge_metadata` the `tool::<id>::<field>`
/// announcement metadata (classification labels only, never a secret).
#[derive(Serialize)]
struct LoweredToolDto {
    tool_id: String,
    mcp_name: String,
    descriptor: serde_json::Value,
    bridge_metadata: std::collections::BTreeMap<String, String>,
}

/// Lower one MCP `tools/list` entry (its JSON object) to the Net discovery
/// shape. Returns the DTO
/// `{"tool_id","mcp_name","descriptor","bridge_metadata"}` as an owned JSON
/// C string, or NULL on error.
///
/// `credential_status` is the exact label the classifier produced (a
/// trusted local value — `"none"` is accepted verbatim, unlike a wire
/// value); `substitutability` is `"provider_local"` (or null) /
/// `"provider_equivalent"`.
///
/// Free the return with [`net_mcp_free_string`].
///
/// # Safety
/// All non-null pointers must be valid NUL-terminated C strings.
#[no_mangle]
pub unsafe extern "C" fn net_mcp_lower_tool(
    tool_json: *const c_char,
    server_version: *const c_char,
    credential_status: *const c_char,
    substitutability: *const c_char,
) -> *mut c_char {
    clear_last_error();
    ffi_guard!(ptr::null_mut(), {
        let (Some(tool_json), Some(server_version), Some(credential_status)) = (
            cstr(tool_json, "tool_json"),
            cstr(server_version, "server_version"),
            cstr(credential_status, "credential_status"),
        ) else {
            return ptr::null_mut();
        };
        let tool: net_mcp::spec::Tool = match serde_json::from_str(tool_json) {
            Ok(t) => t,
            Err(e) => {
                set_last_error(format!("invalid tools/list entry: {e}"), "invalid_arg");
                return ptr::null_mut();
            }
        };
        let Some(credential_status) = CredentialStatus::from_label(credential_status) else {
            set_last_error(
                format!("unknown credential_status {credential_status:?} (expected credentialed | external_api | unknown | none)"),
                "invalid_arg",
            );
            return ptr::null_mut();
        };
        let substitutability = match opt_cstr(substitutability, "substitutability") {
            Err(()) => return ptr::null_mut(),
            Ok(None) => Substitutability::ProviderLocal,
            Ok(Some(label)) => match Substitutability::from_label(label) {
                Some(s) => s,
                None => {
                    set_last_error(
                        format!(
                            "unknown substitutability {label:?} (expected {})",
                            Substitutability::EXPECTED
                        ),
                        "invalid_arg",
                    );
                    return ptr::null_mut();
                }
            },
        };
        let lowered = lower_tool(
            &tool,
            &LoweringContext {
                server_version: server_version.to_string(),
                credential_status,
                substitutability,
            },
        );
        let descriptor = match serde_json::to_value(&lowered.descriptor) {
            Ok(v) => v,
            Err(e) => {
                set_last_error(format!("encode descriptor: {e}"), "encode_error");
                return ptr::null_mut();
            }
        };
        let dto = LoweredToolDto {
            tool_id: lowered.descriptor.tool_id,
            mcp_name: lowered.mcp_name,
            descriptor,
            bridge_metadata: lowered.bridge_metadata.into_iter().collect(),
        };
        match serde_json::to_string(&dto) {
            Ok(s) => into_cstr(s),
            Err(e) => {
                set_last_error(format!("encode lowered tool: {e}"), "encode_error");
                ptr::null_mut()
            }
        }
    })
}

// =====================================================================
// Consent gate
// =====================================================================

/// Does a wire-declared credential status require local consent? Returns
/// `1` if it does, `0` if not. Implements the core trust boundary: a wire
/// `"none"` is NOT trusted (it gates like `"unknown"`), so even `"none"`
/// (and null / anything unrecognised) returns `1`.
///
/// # Safety
/// `status` must be null or a valid NUL-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn net_mcp_credential_requires_consent(status: *const c_char) -> c_int {
    clear_last_error();
    ffi_guard!(1, {
        // A non-UTF-8 status can't be trusted — gate it (return 1) rather than
        // coerce to "" and risk under-gating; opt_cstr records the detail.
        let status = match opt_cstr(status, "status") {
            Ok(s) => s.unwrap_or(""),
            Err(()) => return 1,
        };
        c_int::from(SdkCredentialStatus::from_wire(status).requires_consent())
    })
}

/// Canonicalize a `provider/capability` id (trims whitespace, rewrites a
/// `0x`-hex node id to decimal), so `0x2a/echo` and `42/echo` return the
/// same string. Returns the canonical display as an owned C string, or NULL
/// on a parse error (missing / empty half). Free with
/// [`net_mcp_free_string`].
///
/// # Safety
/// `display` must be a valid NUL-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn net_mcp_cap_id_canonicalize(display: *const c_char) -> *mut c_char {
    clear_last_error();
    ffi_guard!(ptr::null_mut(), {
        let Some(display) = cstr(display, "display") else {
            return ptr::null_mut();
        };
        match parse_cap(display) {
            Some(id) => into_cstr(id.display()),
            None => ptr::null_mut(),
        }
    })
}

/// Opaque handle to a [`CoreConsentPolicy`]. Free with
/// [`net_mcp_consent_policy_free`].
///
/// The C ABI is callable from any thread and the core policy takes `&mut` for
/// its mutators, so the inner policy is behind a `Mutex`: concurrent
/// `allow`/`pin`/`unpin`/`is_pinned`/`decide`/`pinned` calls on one handle are
/// serialized here rather than racing. (Handle *lifetime* — free vs. a call in
/// flight — is still the caller's responsibility; the Go binding guards it with
/// its own mutex.)
pub struct ConsentPolicyHandle {
    inner: std::sync::Mutex<CoreConsentPolicy>,
}

impl ConsentPolicyHandle {
    /// Lock the inner policy, tolerating a poisoned mutex: the consent ops
    /// leave no inconsistent state on a panic and every entry point is
    /// `catch_unwind`-guarded, so a poisoned lock must not permanently wedge
    /// the handle.
    fn lock(&self) -> std::sync::MutexGuard<'_, CoreConsentPolicy> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Allocate an empty consent policy. With no entries, EVERY discovered
/// capability requires approval. Free with [`net_mcp_consent_policy_free`].
#[no_mangle]
pub extern "C" fn net_mcp_consent_policy_new() -> *mut ConsentPolicyHandle {
    clear_last_error();
    ffi_guard!(ptr::null_mut(), {
        Box::into_raw(Box::new(ConsentPolicyHandle {
            inner: std::sync::Mutex::new(CoreConsentPolicy::new()),
        }))
    })
}

/// Free a consent-policy handle. No-op on null.
///
/// # Safety
/// `policy` must be a pointer returned by [`net_mcp_consent_policy_new`], or
/// null, and must not be used afterwards.
#[no_mangle]
pub unsafe extern "C" fn net_mcp_consent_policy_free(policy: *mut ConsentPolicyHandle) {
    clear_last_error();
    ffi_guard!((), {
        if !policy.is_null() {
            drop(Box::from_raw(policy));
        }
    })
}

/// Shared body for the `allow` / `pin` / `unpin` mutators: borrow the
/// handle, parse the cap id, apply `f`. Returns `0` on success, `-1` on a
/// null handle or a bad cap id.
///
/// # Safety
/// `policy` must be a valid handle pointer.
unsafe fn policy_mutate(
    policy: *mut ConsentPolicyHandle,
    cap_id: *const c_char,
    f: impl FnOnce(&mut CoreConsentPolicy, CapabilityId),
) -> c_int {
    clear_last_error();
    ffi_guard!(-1, {
        if policy.is_null() {
            set_last_error("policy must not be null", "invalid_arg");
            return -1;
        }
        let Some(display) = cstr(cap_id, "cap_id") else {
            return -1;
        };
        let Some(id) = parse_cap(display) else {
            return -1;
        };
        f(&mut (*policy).lock(), id);
        0
    })
}

/// Allowlist a capability (a standing pre-approval). `0` on success, `-1` on
/// error.
///
/// # Safety
/// `policy` must be a valid handle; `cap_id` a valid C string.
#[no_mangle]
pub unsafe extern "C" fn net_mcp_consent_policy_allow(
    policy: *mut ConsentPolicyHandle,
    cap_id: *const c_char,
) -> c_int {
    policy_mutate(policy, cap_id, |p, id| p.allow(id))
}

/// Record an approved pin. `0` on success, `-1` on error.
///
/// # Safety
/// As [`net_mcp_consent_policy_allow`].
#[no_mangle]
pub unsafe extern "C" fn net_mcp_consent_policy_pin(
    policy: *mut ConsentPolicyHandle,
    cap_id: *const c_char,
) -> c_int {
    policy_mutate(policy, cap_id, |p, id| p.pin(id))
}

/// Remove a pin. `0` on success, `-1` on error.
///
/// # Safety
/// As [`net_mcp_consent_policy_allow`].
#[no_mangle]
pub unsafe extern "C" fn net_mcp_consent_policy_unpin(
    policy: *mut ConsentPolicyHandle,
    cap_id: *const c_char,
) -> c_int {
    policy_mutate(policy, cap_id, |p, id| p.unpin(&id))
}

/// Is the capability pinned? `1` yes, `0` no, `-1` on error.
///
/// # Safety
/// As [`net_mcp_consent_policy_allow`].
#[no_mangle]
pub unsafe extern "C" fn net_mcp_consent_policy_is_pinned(
    policy: *mut ConsentPolicyHandle,
    cap_id: *const c_char,
) -> c_int {
    clear_last_error();
    ffi_guard!(-1, {
        if policy.is_null() {
            set_last_error("policy must not be null", "invalid_arg");
            return -1;
        }
        let Some(display) = cstr(cap_id, "cap_id") else {
            return -1;
        };
        let Some(id) = parse_cap(display) else {
            return -1;
        };
        c_int::from((*policy).lock().is_pinned(&id))
    })
}

/// Decide whether the capability, with the given wire credential status,
/// may be invoked. Returns `"allowed"` or `"requires_approval"` as an owned
/// C string (the SDK enum's stable string — never re-derive it), or NULL on
/// error. Free with [`net_mcp_free_string`].
///
/// # Safety
/// As [`net_mcp_consent_policy_allow`], plus `credential_status` a valid C
/// string.
#[no_mangle]
pub unsafe extern "C" fn net_mcp_consent_policy_decide(
    policy: *mut ConsentPolicyHandle,
    cap_id: *const c_char,
    credential_status: *const c_char,
) -> *mut c_char {
    clear_last_error();
    ffi_guard!(ptr::null_mut(), {
        if policy.is_null() {
            set_last_error("policy must not be null", "invalid_arg");
            return ptr::null_mut();
        }
        let (Some(display), Some(status)) = (
            cstr(cap_id, "cap_id"),
            cstr(credential_status, "credential_status"),
        ) else {
            return ptr::null_mut();
        };
        let Some(id) = parse_cap(display) else {
            return ptr::null_mut();
        };
        // The SDK enum's stable string form — never re-derived here.
        let decision = (*policy).lock().decide(&id, status).as_str().to_string();
        into_cstr(decision)
    })
}

/// The pinned capabilities' display ids as an owned JSON array string
/// (sorted), or NULL on error. Free with [`net_mcp_free_string`].
///
/// # Safety
/// `policy` must be a valid handle.
#[no_mangle]
pub unsafe extern "C" fn net_mcp_consent_policy_pinned(
    policy: *mut ConsentPolicyHandle,
) -> *mut c_char {
    clear_last_error();
    ffi_guard!(ptr::null_mut(), {
        if policy.is_null() {
            set_last_error("policy must not be null", "invalid_arg");
            return ptr::null_mut();
        }
        let mut ids: Vec<String> = (*policy).lock().pinned().map(|id| id.display()).collect();
        ids.sort();
        match serde_json::to_string(&ids) {
            Ok(s) => into_cstr(s),
            Err(e) => {
                set_last_error(format!("encode pinned: {e}"), "encode_error");
                ptr::null_mut()
            }
        }
    })
}

// =====================================================================
// Pin store — path-scoped, cross-process-locked
// =====================================================================

/// Record a pin **request** (the model-callable verb) at `store_path` for
/// `cap_id`: writes a `"pending"` record if none exists; an existing record
/// is left untouched. Returns the resulting state (`"pending"` /
/// `"approved"`) as an owned C string, or NULL on error. Free with
/// [`net_mcp_free_string`].
///
/// # Safety
/// `store_path` and `cap_id` must be valid NUL-terminated C strings.
#[no_mangle]
pub unsafe extern "C" fn net_mcp_pin_request(
    store_path: *const c_char,
    cap_id: *const c_char,
) -> *mut c_char {
    clear_last_error();
    ffi_guard!(ptr::null_mut(), {
        let (Some(path), Some(display)) = (cstr(store_path, "store_path"), cstr(cap_id, "cap_id"))
        else {
            return ptr::null_mut();
        };
        let Some(id) = parse_cap(display) else {
            return ptr::null_mut();
        };
        let path = path.to_string();
        match runtime().block_on(PinStore::mutate(path, move |s| {
            pin_state_str(s.request(&id)).to_string()
        })) {
            Ok(state) => into_cstr(state),
            Err(e) => {
                set_last_error(e.to_string(), "pins_error");
                ptr::null_mut()
            }
        }
    })
}

/// Run a locked pin mutation returning a bool; `1`/`0` result, `-1` error.
///
/// # Safety
/// `store_path` / `cap_id` must be valid C strings.
unsafe fn pin_bool_mutate(
    store_path: *const c_char,
    cap_id: *const c_char,
    f: impl FnOnce(&mut PinStore, &CapabilityId) -> bool + Send + 'static,
) -> c_int {
    clear_last_error();
    ffi_guard!(-1, {
        let (Some(path), Some(display)) = (cstr(store_path, "store_path"), cstr(cap_id, "cap_id"))
        else {
            return -1;
        };
        let Some(id) = parse_cap(display) else {
            return -1;
        };
        let path = path.to_string();
        match runtime().block_on(PinStore::mutate(path, move |s| f(s, &id))) {
            Ok(b) => c_int::from(b),
            Err(e) => {
                set_last_error(e.to_string(), "pins_error");
                -1
            }
        }
    })
}

/// **Approve** a pin (operator verb). `1` if this changed the stored state,
/// `0` if not, `-1` on error.
///
/// # Safety
/// As [`net_mcp_pin_request`].
#[no_mangle]
pub unsafe extern "C" fn net_mcp_pin_approve(
    store_path: *const c_char,
    cap_id: *const c_char,
) -> c_int {
    pin_bool_mutate(store_path, cap_id, |s, id| s.approve(id))
}

/// **Reject / remove** a pin (operator verb). `1` if a record was removed,
/// `0` if not, `-1` on error.
///
/// # Safety
/// As [`net_mcp_pin_request`].
#[no_mangle]
pub unsafe extern "C" fn net_mcp_pin_reject(
    store_path: *const c_char,
    cap_id: *const c_char,
) -> c_int {
    pin_bool_mutate(store_path, cap_id, |s, id| s.remove(id))
}

/// Is the capability approved (fresh snapshot)? `1` yes, `0` no, `-1`
/// error.
///
/// # Safety
/// As [`net_mcp_pin_request`].
#[no_mangle]
pub unsafe extern "C" fn net_mcp_pin_is_approved(
    store_path: *const c_char,
    cap_id: *const c_char,
) -> c_int {
    clear_last_error();
    ffi_guard!(-1, {
        let (Some(path), Some(display)) = (cstr(store_path, "store_path"), cstr(cap_id, "cap_id"))
        else {
            return -1;
        };
        let Some(id) = parse_cap(display) else {
            return -1;
        };
        match runtime().block_on(PinStore::load(path.to_string())) {
            Ok(store) => c_int::from(store.is_approved(&id)),
            Err(e) => {
                set_last_error(e.to_string(), "pins_error");
                -1
            }
        }
    })
}

/// The capability's state (fresh snapshot): `"pending"` / `"approved"` as
/// an owned C string, an **empty string** `""` when there is no record, or
/// NULL on error (check the last-error). Free a non-null return with
/// [`net_mcp_free_string`].
///
/// # Safety
/// As [`net_mcp_pin_request`].
#[no_mangle]
pub unsafe extern "C" fn net_mcp_pin_state(
    store_path: *const c_char,
    cap_id: *const c_char,
) -> *mut c_char {
    clear_last_error();
    ffi_guard!(ptr::null_mut(), {
        let (Some(path), Some(display)) = (cstr(store_path, "store_path"), cstr(cap_id, "cap_id"))
        else {
            return ptr::null_mut();
        };
        let Some(id) = parse_cap(display) else {
            return ptr::null_mut();
        };
        match runtime().block_on(PinStore::load(path.to_string())) {
            Ok(store) => match store.state(&id) {
                Some(state) => into_cstr(pin_state_str(state).to_string()),
                None => into_cstr(String::new()),
            },
            Err(e) => {
                set_last_error(e.to_string(), "pins_error");
                ptr::null_mut()
            }
        }
    })
}

/// One pin record in the JSON list.
#[derive(Serialize)]
struct PinRecordDto {
    cap_id: String,
    state: String,
}

/// All records at `store_path` as an owned JSON-array string of
/// `{"cap_id","state"}` (sorted by cap_id), or NULL on error. Free with
/// [`net_mcp_free_string`].
///
/// # Safety
/// `store_path` must be a valid NUL-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn net_mcp_pin_list(store_path: *const c_char) -> *mut c_char {
    clear_last_error();
    ffi_guard!(ptr::null_mut(), {
        let Some(path) = cstr(store_path, "store_path") else {
            return ptr::null_mut();
        };
        match runtime().block_on(PinStore::load(path.to_string())) {
            Ok(store) => {
                let mut rows: Vec<PinRecordDto> = store
                    .list()
                    .into_iter()
                    .map(|(id, state)| PinRecordDto {
                        cap_id: id.display(),
                        state: pin_state_str(state).to_string(),
                    })
                    .collect();
                rows.sort_by(|a, b| a.cap_id.cmp(&b.cap_id));
                match serde_json::to_string(&rows) {
                    Ok(s) => into_cstr(s),
                    Err(e) => {
                        set_last_error(format!("encode pin list: {e}"), "encode_error");
                        ptr::null_mut()
                    }
                }
            }
            Err(e) => {
                set_last_error(e.to_string(), "pins_error");
                ptr::null_mut()
            }
        }
    })
}

// =====================================================================
// Tests — drive the extern "C" symbols directly through the rlib, so the
// ABI logic (marshaling, error signalling, the pin-store lock) is verified
// without needing cgo. The Go wrapper (`go/mcp.go`) is thin marshaling that
// CI link-tests on Linux.
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// A NUL-terminated C string for a test argument.
    fn c(s: &str) -> CString {
        CString::new(s).unwrap()
    }

    /// Take an owned `char*` return into a `String`, freeing it. Panics on
    /// NULL (the caller asserts success first via a helper below).
    unsafe fn take(p: *mut c_char) -> String {
        assert!(!p.is_null(), "expected a non-null string return");
        let s = CStr::from_ptr(p).to_str().unwrap().to_string();
        net_mcp_free_string(p);
        s
    }

    #[test]
    fn classify_parity_vectors() {
        unsafe {
            let vectors = [
                (
                    "npx",
                    r#"["-y","some-server"]"#,
                    r#"{"GITHUB_TOKEN":"ghp_x"}"#,
                    "credentialed",
                ),
                (
                    "npx",
                    r#"["-y","@modelcontextprotocol/server-github"]"#,
                    "{}",
                    "external_api",
                ),
                (
                    "uvx",
                    r#"["mcp-server-time"]"#,
                    r#"{"TZ":"UTC"}"#,
                    "unknown",
                ),
            ];
            for (program, args, envs, want) in vectors {
                let got = take(net_mcp_classify(
                    c(program).as_ptr(),
                    c(args).as_ptr(),
                    c(envs).as_ptr(),
                    ptr::null(),
                    0,
                ));
                assert_eq!(got, want, "classify {program}");
            }
        }
    }

    #[test]
    fn classify_overrides_and_force() {
        unsafe {
            // Downward override needs force.
            let denied = net_mcp_classify(
                c("uvx").as_ptr(),
                c("[]").as_ptr(),
                c("{}").as_ptr(),
                c("no-credentials").as_ptr(),
                0,
            );
            assert!(denied.is_null());
            let kind = CStr::from_ptr(net_mcp_last_error_kind()).to_str().unwrap();
            assert_eq!(kind, "classify_error");

            let forced = take(net_mcp_classify(
                c("uvx").as_ptr(),
                c("[]").as_ptr(),
                c("{}").as_ptr(),
                c("no-credentials").as_ptr(),
                1,
            ));
            assert_eq!(forced, "none");

            // Unknown override is an invalid arg.
            assert!(net_mcp_classify(
                c("uvx").as_ptr(),
                c("[]").as_ptr(),
                c("{}").as_ptr(),
                c("bogus").as_ptr(),
                0,
            )
            .is_null());
        }
    }

    #[test]
    fn lower_tool_and_secret_negative() {
        unsafe {
            let secret = "ghp_must_never_cross";
            let status = take(net_mcp_classify(
                c("npx").as_ptr(),
                c(r#"["srv"]"#).as_ptr(),
                c(&format!(r#"{{"API_KEY":"{secret}"}}"#)).as_ptr(),
                ptr::null(),
                0,
            ));
            assert_eq!(status, "credentialed");

            let tool = r#"{"name":"echo","description":"echo it","inputSchema":{"type":"object","properties":{"message":{"type":"string"}}}}"#;
            let json = take(net_mcp_lower_tool(
                c(tool).as_ptr(),
                c("2.0.0").as_ptr(),
                c(&status).as_ptr(),
                c("provider_local").as_ptr(),
            ));
            let v: serde_json::Value = serde_json::from_str(&json).unwrap();
            assert_eq!(v["tool_id"], "echo");
            assert_eq!(v["mcp_name"], "echo");
            assert_eq!(
                v["bridge_metadata"]["tool::echo::compat_tier"],
                "mcp_bridge"
            );
            assert_eq!(
                v["bridge_metadata"]["tool::echo::credential_status"],
                "credentialed"
            );
            assert_eq!(v["descriptor"]["tool_id"], "echo");
            // The env value never crosses back through either helper.
            assert!(!json.contains(secret), "env value leaked into lowered DTO");
        }
    }

    #[test]
    fn lower_tool_sanitizes_and_rejects_garbage_status() {
        unsafe {
            let json = take(net_mcp_lower_tool(
                c(r#"{"name":"getCaps","inputSchema":{"type":"object"}}"#).as_ptr(),
                c("1.0.0").as_ptr(),
                c("none").as_ptr(),
                ptr::null(),
            ));
            let v: serde_json::Value = serde_json::from_str(&json).unwrap();
            assert_eq!(v["mcp_name"], "getCaps");
            let tool_id = v["tool_id"].as_str().unwrap();
            assert_ne!(tool_id, "getCaps");
            assert!(tool_id.starts_with("getcaps"));

            // A garbage credential_status is an error, never silently gated.
            assert!(net_mcp_lower_tool(
                c(r#"{"name":"echo","inputSchema":{"type":"object"}}"#).as_ptr(),
                c("1.0.0").as_ptr(),
                c("trust-me").as_ptr(),
                ptr::null(),
            )
            .is_null());
        }
    }

    #[test]
    fn credential_consent_and_cap_id() {
        unsafe {
            for status in [
                "credentialed",
                "external_api",
                "unknown",
                "none",
                "",
                "bogus",
            ] {
                assert_eq!(
                    net_mcp_credential_requires_consent(c(status).as_ptr()),
                    1,
                    "{status} must be gated"
                );
            }
            // Null status gates too.
            assert_eq!(net_mcp_credential_requires_consent(ptr::null()), 1);

            for spelling in ["0x2a/echo", "0X2A/echo", " 42/echo", "42 /echo"] {
                let got = take(net_mcp_cap_id_canonicalize(c(spelling).as_ptr()));
                assert_eq!(got, "42/echo", "{spelling}");
            }
            assert!(net_mcp_cap_id_canonicalize(c("bareword").as_ptr()).is_null());
        }
    }

    #[test]
    fn consent_policy_handle() {
        unsafe {
            let p = net_mcp_consent_policy_new();
            assert!(!p.is_null());

            let decide = |cap: &str, status: &str| {
                take(net_mcp_consent_policy_decide(
                    p,
                    c(cap).as_ptr(),
                    c(status).as_ptr(),
                ))
            };
            assert_eq!(decide("b/echo", "none"), "requires_approval");
            assert_eq!(net_mcp_consent_policy_allow(p, c("b/echo").as_ptr()), 0);
            assert_eq!(decide("b/echo", "credentialed"), "allowed");

            // Pin under the hex spelling admits the decimal spelling.
            assert_eq!(net_mcp_consent_policy_pin(p, c("0x2a/echo").as_ptr()), 0);
            assert_eq!(
                net_mcp_consent_policy_is_pinned(p, c("42/echo").as_ptr()),
                1
            );
            let pinned = take(net_mcp_consent_policy_pinned(p));
            assert_eq!(pinned, r#"["42/echo"]"#);
            assert_eq!(net_mcp_consent_policy_unpin(p, c("42/echo").as_ptr()), 0);
            assert_eq!(
                net_mcp_consent_policy_is_pinned(p, c("42/echo").as_ptr()),
                0
            );

            net_mcp_consent_policy_free(p);
        }
    }

    #[test]
    fn consent_policy_handle_is_thread_safe() {
        // The handle's inner policy is Mutex-guarded, so concurrent mutators
        // and readers on ONE handle through the C ABI are serialized rather
        // than racing on `&mut` (a data race here would be UB — this is the
        // guard that makes the C ABI safe for multi-threaded callers). Handle
        // lifetime stays the caller's job: we free only after every thread
        // joins.
        unsafe {
            let p = net_mcp_consent_policy_new();
            assert!(!p.is_null());
            // Raw pointers aren't `Send`; carry the address across threads.
            let addr = p as usize;
            const N: usize = 32;
            let handles: Vec<_> = (0..N)
                .map(|i| {
                    std::thread::spawn(move || {
                        let p = addr as *mut ConsentPolicyHandle;
                        let cap = c(&format!("b/cap{i}"));
                        // SAFETY: `p` is live (freed only after join); the inner
                        // Mutex serializes these concurrent calls.
                        unsafe {
                            net_mcp_consent_policy_allow(p, cap.as_ptr());
                            net_mcp_consent_policy_pin(p, cap.as_ptr());
                            net_mcp_consent_policy_is_pinned(p, cap.as_ptr());
                            let _ = take(net_mcp_consent_policy_decide(
                                p,
                                cap.as_ptr(),
                                c("credentialed").as_ptr(),
                            ));
                            let _ = take(net_mcp_consent_policy_pinned(p));
                        }
                    })
                })
                .collect();
            for h in handles {
                h.join().unwrap();
            }
            // Every cap was allowlisted and pinned, so each decides "allowed".
            for i in 0..N {
                let d = take(net_mcp_consent_policy_decide(
                    p,
                    c(&format!("b/cap{i}")).as_ptr(),
                    c("credentialed").as_ptr(),
                ));
                assert_eq!(d, "allowed");
            }
            net_mcp_consent_policy_free(p);
        }
    }

    #[test]
    fn pin_store_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pins.json");
        let path_c = c(path.to_str().unwrap());
        unsafe {
            let pp = path_c.as_ptr();
            // request -> pending, grants nothing.
            assert_eq!(
                take(net_mcp_pin_request(pp, c("b/echo").as_ptr())),
                "pending"
            );
            assert_eq!(net_mcp_pin_is_approved(pp, c("b/echo").as_ptr()), 0);
            // approve -> changed; a re-request never disturbs it.
            assert_eq!(net_mcp_pin_approve(pp, c("b/echo").as_ptr()), 1);
            assert_eq!(
                take(net_mcp_pin_request(pp, c("b/echo").as_ptr())),
                "approved"
            );
            assert_eq!(net_mcp_pin_is_approved(pp, c("b/echo").as_ptr()), 1);
            // state + list.
            assert_eq!(
                take(net_mcp_pin_state(pp, c("b/echo").as_ptr())),
                "approved"
            );
            let list = take(net_mcp_pin_list(pp));
            assert_eq!(list, r#"[{"cap_id":"b/echo","state":"approved"}]"#);
            // reject -> removed; absent state is "".
            assert_eq!(net_mcp_pin_reject(pp, c("b/echo").as_ptr()), 1);
            assert_eq!(net_mcp_pin_reject(pp, c("b/echo").as_ptr()), 0);
            assert_eq!(take(net_mcp_pin_state(pp, c("b/echo").as_ptr())), "");
        }
    }

    #[test]
    fn pin_store_corrupt_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pins.json");
        std::fs::write(&path, b"{ not valid json").unwrap();
        let path_c = c(path.to_str().unwrap());
        unsafe {
            assert!(net_mcp_pin_list(path_c.as_ptr()).is_null());
            let kind = CStr::from_ptr(net_mcp_last_error_kind()).to_str().unwrap();
            assert_eq!(kind, "pins_error");
        }
    }

    #[test]
    fn pin_store_concurrent_mutations_lose_nothing() {
        // The whole point of the cross-process lock: N threads each approve
        // a distinct capability through the C ABI and nothing is lost to a
        // stale-snapshot race.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pins.json").to_str().unwrap().to_string();
        const N: usize = 40;

        let handles: Vec<_> = (0..N)
            .map(|i| {
                let path = path.clone();
                std::thread::spawn(move || {
                    let path_c = c(&path);
                    let cap = c(&format!("node/tool{i}"));
                    // SAFETY: distinct cap ids; the store's file lock
                    // serializes the writers.
                    unsafe { net_mcp_pin_approve(path_c.as_ptr(), cap.as_ptr()) }
                })
            })
            .collect();
        for h in handles {
            assert_eq!(h.join().unwrap(), 1, "each approve must change the store");
        }

        let path_c = c(&path);
        let list = unsafe { take(net_mcp_pin_list(path_c.as_ptr())) };
        let rows: Vec<serde_json::Value> = serde_json::from_str(&list).unwrap();
        assert_eq!(rows.len(), N, "concurrent approves lost updates");
    }
}
