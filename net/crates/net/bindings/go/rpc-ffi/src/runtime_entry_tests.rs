//! Registration must run inside an entered runtime (decision 1).
//!
//! The four `net_rpc_serve*` exports are synchronous and are called
//! from a cgo thread — a Go-scheduler thread, never a tokio worker.
//! `MeshNode::serve_rpc*` is itself synchronous, but it spawns an
//! inbound-event bridge with a bare `tokio::spawn`, which panics
//! "there is no reactor running" when no runtime is in context.
//!
//! That panic never reached anyone. `ffi_guard!` catches it and
//! returns the default, so the Go caller saw a NULL `ServeHandle`
//! with `out_err` unset — a serve that failed for no stated reason.
//! Only a live registration reaches the spawn, which is why the
//! null-safety unit tests beside these all passed throughout.
//!
//! Each test below drives an export from a freshly-spawned
//! `std::thread`, which reproduces the no-runtime condition exactly.
//! Deleting `runtime().enter()` from any of the four turns its test
//! NULL. These are the in-crate half of the witness; the
//! cross-process Go witnesses still have to run in CI against a built
//! `libnet`.

use super::*;
use net::adapter::net::identity::EntityKeypair;
use net::adapter::net::MeshNodeConfig;

// Never invoked — these tests register handlers and never dispatch to
// them. Each shape keeps its own dispatcher slot, and each `serve*`
// refuses outright when its own slot is empty, so all four exist.

unsafe extern "C" fn unused_unary(
    _handler_id: u64,
    _req_ptr: *const u8,
    _req_len: usize,
    _out_resp_ptr: *mut *mut u8,
    _out_resp_len: *mut usize,
    _out_err: *mut *mut c_char,
) -> c_int {
    NET_RPC_ERR_CALL_FAILED
}

unsafe extern "C" fn unused_client_streaming(
    _handler_id: u64,
    _request_stream: *mut RpcRequestStreamHandleC,
    _out_resp_ptr: *mut *mut u8,
    _out_resp_len: *mut usize,
    _out_err: *mut *mut c_char,
) -> c_int {
    NET_RPC_ERR_CALL_FAILED
}

unsafe extern "C" fn unused_streaming(
    _handler_id: u64,
    _req_ptr: *const u8,
    _req_len: usize,
    _response_sink: *mut RpcResponseSinkHandleC,
    _out_err: *mut *mut c_char,
) -> c_int {
    NET_RPC_ERR_CALL_FAILED
}

unsafe extern "C" fn unused_duplex(
    _handler_id: u64,
    _request_stream: *mut RpcRequestStreamHandleC,
    _response_sink: *mut RpcResponseSinkHandleC,
    _out_err: *mut *mut c_char,
) -> c_int {
    NET_RPC_ERR_CALL_FAILED
}

/// The runtime the *node* lives on — deliberately not the one the
/// serve exports enter.
///
/// In a real Go process the mesh is built through `net::ffi::mesh`,
/// which owns a separate static runtime; nRPC registration lands on
/// this crate's. Modelling that split keeps the test honest about the
/// arrangement it pins. Never dropped: tearing it down would
/// deregister the node's socket from its reactor mid-test.
fn node_runtime() -> &'static Runtime {
    static RT: OnceLock<Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("rpc-ffi-test-node")
            .build()
            .expect("build the test node runtime")
    })
}

/// A live `MeshRpcHandle` over a node bound to an ephemeral port,
/// with every shape's dispatcher installed.
fn rpc_handle() -> *mut MeshRpcHandle {
    net_rpc_set_handler_dispatcher(unused_unary);
    net_rpc_set_client_streaming_handler_dispatcher(unused_client_streaming);
    net_rpc_set_streaming_handler_dispatcher(unused_streaming);
    net_rpc_set_duplex_handler_dispatcher(unused_duplex);
    let node = node_runtime().block_on(async {
        MeshNode::new(
            EntityKeypair::generate(),
            MeshNodeConfig::new("127.0.0.1:0".parse().unwrap(), [0x5Au8; 32]),
        )
        .await
        .expect("bind a mesh node on an ephemeral port")
    });
    net_rpc_new(Box::into_raw(Box::new(Arc::new(node))))
}

/// Run `body` on a thread with no runtime in context.
///
/// The handle crosses as a `usize` because `*mut` is not `Send` —
/// which is exactly the C-ABI contract here: Go owns the pointer and
/// calls from whichever thread its scheduler picks.
fn on_a_bare_thread<F, R>(handle: *mut MeshRpcHandle, body: F) -> R
where
    F: FnOnce(*mut MeshRpcHandle) -> R + Send + 'static,
    R: Send + 'static,
{
    let addr = handle as usize;
    std::thread::spawn(move || {
        assert!(
            tokio::runtime::Handle::try_current().is_err(),
            "this thread must have no runtime in context, or the test \
             proves nothing about the cgo condition"
        );
        body(addr as *mut MeshRpcHandle)
    })
    .join()
    .expect("registration thread panicked")
}

/// Register `service` through `export`, freeing the handle it
/// produces and reporting `out_err` on failure.
///
/// A NULL return with *no* `out_err` written is the signature this
/// whole module exists for: every real refusal writes a message
/// first, so an empty one means `ffi_guard!` caught a panic and
/// returned the default. The panic is "there is no reactor running".
fn register(
    handle: *mut MeshRpcHandle,
    service: &str,
    export: impl FnOnce(
        *mut MeshRpcHandle,
        *const c_char,
        usize,
        u64,
        *mut *mut c_char,
    ) -> *mut ServeHandleC,
) -> Result<(), String> {
    let name = CString::new(service).unwrap();
    let mut err: *mut c_char = std::ptr::null_mut();
    let serve = export(
        handle,
        name.as_ptr(),
        service.len(),
        net_rpc_reserve_handler_id(),
        &mut err,
    );
    if !serve.is_null() {
        net_rpc_serve_handle_free(serve);
        return Ok(());
    }
    if err.is_null() {
        return Err("NULL with no out_err — ffi_guard swallowed a panic".into());
    }
    // Leaked rather than freed: `net_rpc_free_cstring` is the Go-side
    // owner's job and this is a one-shot failure path in a test.
    Err(unsafe { std::ffi::CStr::from_ptr(err) }
        .to_string_lossy()
        .into_owned())
}

/// Each export is passed by name so the closure below is the only
/// place its signature is spelled out; all four share it.
macro_rules! bare_thread_case {
    ($test:ident, $export:ident, $service:literal) => {
        #[test]
        fn $test() {
            let handle = rpc_handle();
            let outcome = on_a_bare_thread(handle, move |h| {
                register(h, $service, |h, ptr, len, id, err| {
                    $export(h, ptr, len, id, 0, err)
                })
            });
            if let Err(why) = outcome {
                panic!(
                    "{} failed from a thread with no runtime: {why}",
                    stringify!($export)
                );
            }
            net_rpc_free(handle);
        }
    };
}

bare_thread_case!(
    unary_registers_from_a_thread_with_no_runtime,
    net_rpc_serve,
    "ffi.runtime.unary"
);
bare_thread_case!(
    client_streaming_registers_from_a_thread_with_no_runtime,
    net_rpc_serve_client_stream,
    "ffi.runtime.clientstream"
);
bare_thread_case!(
    server_streaming_registers_from_a_thread_with_no_runtime,
    net_rpc_serve_streaming,
    "ffi.runtime.streaming"
);
bare_thread_case!(
    duplex_registers_from_a_thread_with_no_runtime,
    net_rpc_serve_duplex,
    "ffi.runtime.duplex"
);

/// All four shapes on one handle from one bare thread — what a Go
/// service that offers every call shape actually does at startup.
#[test]
fn all_four_shapes_register_from_one_bare_thread() {
    let handle = rpc_handle();
    let outcomes = on_a_bare_thread(handle, move |h| {
        [
            register(h, "ffi.runtime.all.unary", |h, p, l, id, e| {
                net_rpc_serve(h, p, l, id, 0, e)
            }),
            register(h, "ffi.runtime.all.cs", |h, p, l, id, e| {
                net_rpc_serve_client_stream(h, p, l, id, 0, e)
            }),
            register(h, "ffi.runtime.all.ss", |h, p, l, id, e| {
                net_rpc_serve_streaming(h, p, l, id, 0, e)
            }),
            register(h, "ffi.runtime.all.dx", |h, p, l, id, e| {
                net_rpc_serve_duplex(h, p, l, id, 0, e)
            }),
        ]
    });
    let failed: Vec<&String> = outcomes.iter().filter_map(|r| r.as_ref().err()).collect();
    assert!(
        failed.is_empty(),
        "shapes failed to register from one bare thread: {failed:?}"
    );
    net_rpc_free(handle);
}
