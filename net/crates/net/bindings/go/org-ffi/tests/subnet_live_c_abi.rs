//! S4 — the C live cell for the subnet-exported surface.
//!
//! The C twin of the Node, Python, and Go cells: a provider inside a protected
//! subnet serves a NAMED export over real transport, a same-org caller invokes
//! it with organization authority only, and a foreign-org caller is refused —
//! all from artifacts minted by `gen_subnet_scenario` and loaded from disk,
//! driven **entirely through the C ABI**.
//!
//! # Why this is Rust and not a `.c` file
//!
//! It calls the same `extern "C"` symbols a C program links against, in the
//! same order, with the same ownership rules — `net_mesh_new` from a JSON
//! config, `net_mesh_arc_clone` per consuming call, the `net_subnet_*` admin
//! and serve entry points, `net_org_credentials_new` / `net_org_bind` /
//! `net_org_call_exported`, and a process-wide `NetOrgHandlerFn` trampoline.
//! Nothing here reaches around the ABI into a Rust convenience API.
//!
//! What a standalone executable would add is *link* evidence, not *coverage*:
//! it needs every sibling cdylib present and a single toolchain, which is the
//! MSVC/mingw duplicate-`net::ffi`-symbol constraint documented in
//! `org-ffi/Cargo.toml`. That packaging concern stays CI-owed. The behaviour a
//! C caller depends on is proven here, where it links and runs today.
//!
//! Ten points, all proven here:
//!
//! ```text
//!  1 provider construction: roots, attachment, named exports (via net_mesh_new JSON)
//!  2 local refusal of an unknown export, before announcement
//!  3 serve through the frozen named-export API (net_subnet_serve_exported)
//!  4 caller construction from real generated org credentials
//!  5 live public discovery
//!  6 a successful net_org_call_exported
//!  7 verified caller + organization attribution at the handler
//!  8 fail-closed for a foreign-org caller
//!  9 that denial is not retried
//! 10 clean close, with no callback racing teardown
//! ```

use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int};
use std::sync::OnceLock;

use parking_lot::Mutex;

use net::ffi::mesh::{
    net_mesh_accept, net_mesh_announce_capabilities, net_mesh_arc_clone, net_mesh_connect,
    net_mesh_free, net_mesh_new, net_mesh_node_id, net_mesh_public_key_hex, net_mesh_start,
    MeshNodeHandle,
};
use net_org::{
    net_org_bind, net_org_call_exported, net_org_client_free, net_org_credentials_new,
    net_org_free_cstring, net_org_reserve_handler_id, net_org_response_free,
    net_org_serve_handle_close, net_org_serve_handle_free, net_org_set_callback_free,
    net_org_set_handler_dispatcher,
    net_subnet_declare_boundaries, net_subnet_install_gateway_credentials,
    net_subnet_serve_exported, NetOrgCaller, NetOrgClient, NetOrgCredentials, NetOrgServeHandle,
    NetSubnetPath, NET_ORG_OK,
};

// ---------------------------------------------------------------------------
// The handler trampoline — exactly the shape a C program registers.
// ---------------------------------------------------------------------------

/// What the handler observed, per invocation. A C harness would keep the same
/// state in a static struct behind its own lock.
#[derive(Default)]
struct HandlerFacts {
    calls: usize,
    attribution_ok: bool,
}

static FACTS: OnceLock<Mutex<HashMap<u64, HandlerFacts>>> = OnceLock::new();
/// The identities the MANIFEST declares, so attribution is checked against the
/// generator's word rather than re-derived here.
static EXPECTED: OnceLock<(String, String, String)> = OnceLock::new();

fn facts() -> &'static Mutex<HashMap<u64, HandlerFacts>> {
    FACTS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn hex_of(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// The process-wide dispatcher. First-call-wins, exactly as a C program's is.
///
/// # Safety
///
/// Called by the Rust serve path with a valid `caller`, a `(req_ptr, req_len)`
/// buffer, and non-null out-params — the contract `NetOrgHandlerFn` documents.
unsafe extern "C" fn dispatcher(
    handler_id: u64,
    caller: *const NetOrgCaller,
    _req_ptr: *const u8,
    _req_len: usize,
    out_resp_ptr: *mut *mut u8,
    out_resp_len: *mut usize,
    _out_err: *mut *mut c_char,
) -> c_int {
    // SAFETY: the serve path always passes a live `NetOrgCaller`.
    let c = unsafe { &*caller };
    let (want_caller, want_acting, want_provider) = EXPECTED.get().expect("expected identities");
    let ok = hex_of(&c.caller) == *want_caller
        && hex_of(&c.acting_org) == *want_acting
        && hex_of(&c.provider_org) == *want_provider
        && c.acting_org == c.provider_org;

    let mut guard = facts().lock();
    let entry = guard.entry(handler_id).or_default();
    entry.calls += 1;
    entry.attribution_ok = ok;
    drop(guard);

    // A Go/C handler returns a malloc'd buffer the Rust side copies, then
    // releases through the deallocator registered below.
    let body = br#"{"n":2,"servedBy":"c-abi-s4"}"#;
    // SAFETY: `libc::malloc` of a nonzero size; the serve path frees it.
    let buf = unsafe { libc::malloc(body.len()) } as *mut u8;
    assert!(!buf.is_null(), "malloc for the handler response");
    // SAFETY: `buf` has `body.len()` writable bytes.
    unsafe { std::ptr::copy_nonoverlapping(body.as_ptr(), buf, body.len()) };
    // SAFETY: both out-params are non-null per the dispatcher contract.
    unsafe {
        *out_resp_ptr = buf;
        *out_resp_len = body.len();
    }
    NET_ORG_OK
}

// ---------------------------------------------------------------------------
// Thin helpers over the C ABI — no Rust convenience APIs.
// ---------------------------------------------------------------------------

fn cstr(s: &str) -> CString {
    CString::new(s).expect("no interior NUL")
}

/// Reserve an OS-assigned UDP port and hand back its address.
///
/// `net_mesh_new` on `127.0.0.1:0` binds an ephemeral port that C has no way to
/// read back, and `net_mesh_accept` REPORTS the peer's address rather than its
/// own. So the acceptor binds where we say — the same trick the Go harness uses.
fn reserved_addr() -> String {
    let s = std::net::UdpSocket::bind("127.0.0.1:0").expect("reserve udp port");
    let addr = s.local_addr().expect("local addr").to_string();
    drop(s);
    addr
}

/// A raw handle shipped to the accept thread.
///
/// # Safety
///
/// `MeshNodeHandle` is used across threads by design — the C ABI is explicitly
/// callable from any thread, and the accept/connect pair MUST run concurrently
/// (accept blocks until a connector dials). This wrapper only carries the
/// pointer; the ABI does its own synchronization.
struct SendHandle(*mut MeshNodeHandle);
// SAFETY: see the type doc — the C ABI is thread-safe by contract.
unsafe impl Send for SendHandle {}

/// `net_mesh_new` from a JSON config — the ONE constructor C has.
fn mesh_new(config_json: &str) -> *mut MeshNodeHandle {
    let json = cstr(config_json);
    let mut handle: *mut MeshNodeHandle = std::ptr::null_mut();
    // SAFETY: a valid NUL-terminated config and a live out-param.
    let rc = unsafe { net_mesh_new(json.as_ptr(), &mut handle) };
    assert_eq!(rc, 0, "net_mesh_new failed for {config_json}");
    assert!(!handle.is_null());
    handle
}

fn node_id(h: *mut MeshNodeHandle) -> u64 {
    // SAFETY: `h` came from `net_mesh_new`.
    unsafe { net_mesh_node_id(h) }
}

fn public_key(h: *mut MeshNodeHandle) -> String {
    let mut out: *mut c_char = std::ptr::null_mut();
    let mut len: usize = 0;
    // SAFETY: live handle, live out-params.
    let rc = unsafe { net_mesh_public_key_hex(h, &mut out, &mut len) };
    let _ = len;
    assert_eq!(rc, 0, "net_mesh_public_key");
    // SAFETY: the ABI wrote a NUL-terminated string.
    let s = unsafe { CStr::from_ptr(out) }
        .to_string_lossy()
        .into_owned();
    // SAFETY: returned by this ABI, freed by its matching free.
    unsafe { net::ffi::net_free_string(out) };
    s
}

/// Read an `out_err` wire and free it, exactly as C must.
fn take_err(err: *mut c_char) -> String {
    if err.is_null() {
        return String::new();
    }
    // SAFETY: a NUL-terminated string this ABI allocated.
    let s = unsafe { CStr::from_ptr(err) }
        .to_string_lossy()
        .into_owned();
    net_org_free_cstring(err);
    s
}

/// The scan a C consumer performs over an `out_err` wire.
fn scan_kind(message: &str) -> Option<&str> {
    const MARKER: &str = "subnet:";
    let rest = &message[message.find(MARKER)? + MARKER.len()..];
    let end = rest
        .find(|c: char| c == ':' || c.is_whitespace())
        .unwrap_or(rest.len());
    let kind = rest[..end].trim();
    (!kind.is_empty()).then_some(kind)
}

#[test]
fn live_subnet_exported_call_through_the_c_abi() {
    use net_sdk::subnet::fixtures;

    let dir = std::env::temp_dir().join(format!("s4-cabi-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scenario dir");
    let m = fixtures::write_subnet_scenario(&dir).expect("generate subnet scenario");
    let path = |rel: &str| dir.join(rel);
    let read = |rel: &str| std::fs::read(path(rel)).expect("read generated artifact");

    EXPECTED
        .set((
            m.caller.entity_id_hex.clone(),
            m.caller.org_id_hex.clone(),
            m.provider.org_id_hex.clone(),
        ))
        .ok();

    // ---- (1) provider construction, through net_mesh_new's JSON ----
    //
    // Trust anchors, security attachment, and the named export are all
    // CONFIGURATION here. C application code builds no authority object.
    let auth = &m.subnet_authorities[0];
    let provider_addr = reserved_addr();
    let provider_json = format!(
        r#"{{"bind_addr":"{bind}","psk_hex":"{psk}","identity_seed_hex":"{seed}",
             "heartbeat_ms":200,"permissive_channels":true,
             "subnet_authorities":[{{"authority_hex":"{ah}","root_hexes":["{rh}"],
                                    "maximum_grant_lifetime_secs":{life}}}],
             "subnet_attachment":{attach:?},
             "subnet_exports":[{{"name":"{ename}","access":"{access}",
                 "binding":{{"subnet":{{"authority_hex":"{bh}","path":{{"levels":{bpath:?}}}}},
                             "topology_epoch":{epoch}}}}}]}}"#,
        bind = provider_addr,
        psk = m.psk_hex,
        seed = m.provider.seed_hex,
        ah = auth.authority_hex,
        rh = auth.root_hexes[0],
        life = auth.maximum_grant_lifetime_secs,
        attach = m.provider.attachment,
        ename = m.export_name,
        access = m.export_access,
        bh = m.export_binding.authority_hex,
        bpath = m.export_binding.path,
        epoch = m.export_binding.topology_epoch,
    );
    let provider = mesh_new(&provider_json);

    // Callers carry NO subnet configuration: organization authority only.
    let caller_json = |seed: &str| {
        format!(
            r#"{{"bind_addr":"127.0.0.1:0","psk_hex":"{psk}","identity_seed_hex":"{seed}",
                 "heartbeat_ms":200,"permissive_channels":true}}"#,
            psk = m.psk_hex,
        )
    };
    let caller = mesh_new(&caller_json(&m.caller.seed_hex));
    let foreign = mesh_new(&caller_json(&m.foreign_caller.seed_hex));

    // Org authorities, through the C provisioning entry point.
    for (h, rel) in [
        (provider, &m.provider.authority_dir),
        (caller, &m.caller.authority_dir),
        (foreign, &m.foreign_caller.authority_dir),
    ] {
        let dir_s = path(rel);
        let dir_s = dir_s.to_string_lossy().into_owned();
        // SAFETY: a live handle; `net_mesh_arc_clone` hands over a fresh
        // consumed clone, which the callee owns on every path.
        let arc = unsafe { net_mesh_arc_clone(h) };
        let mut err: *mut c_char = std::ptr::null_mut();
        let rc = net_org::net_org_install_authority(
            arc,
            dir_s.as_ptr() as *const c_char,
            dir_s.len(),
            &mut err,
        );
        assert_eq!(rc, NET_ORG_OK, "install authority: {}", take_err(err));
    }

    // Gateway provisioning — wholesale, from the generated artifacts.
    let creds = read(&m.provider.gateway_credentials_path);
    let ptrs = [creds.as_ptr()];
    let lens = [creds.len()];
    // SAFETY: a fresh consumed clone plus two live parallel arrays of length 1.
    let rc = unsafe {
        let arc = net_mesh_arc_clone(provider);
        let mut err: *mut c_char = std::ptr::null_mut();
        let rc =
            net_subnet_install_gateway_credentials(arc, ptrs.as_ptr(), lens.as_ptr(), 1, &mut err);
        assert_eq!(rc, NET_ORG_OK, "install gateway creds: {}", take_err(err));
        rc
    };
    assert_eq!(rc, NET_ORG_OK);

    let boundary_paths: Vec<NetSubnetPath> = m
        .provider
        .boundary_paths
        .iter()
        .map(|p| {
            let mut levels = [0u8; 4];
            levels[..p.len()].copy_from_slice(p);
            NetSubnetPath {
                depth: p.len() as u8,
                levels,
            }
        })
        .collect();
    let authority_bytes: Vec<u8> = (0..32)
        .map(|i| {
            u8::from_str_radix(&m.export_binding.authority_hex[i * 2..i * 2 + 2], 16)
                .expect("authority hex")
        })
        .collect();
    // SAFETY: a fresh consumed clone, a 32-byte authority, and a live array.
    unsafe {
        let arc = net_mesh_arc_clone(provider);
        let mut err: *mut c_char = std::ptr::null_mut();
        let rc = net_subnet_declare_boundaries(
            arc,
            authority_bytes.as_ptr(),
            m.export_binding.topology_epoch,
            boundary_paths.as_ptr(),
            boundary_paths.len(),
            &mut err,
        );
        assert_eq!(rc, NET_ORG_OK, "declare boundaries: {}", take_err(err));
    }

    // Every accept() must land before any start(), and accept BLOCKS until a
    // connector dials — so each pair runs concurrently, exactly as the Node,
    // Python, and Go harnesses do.
    let provider_pub = cstr(&public_key(provider));
    for c in [caller, foreign] {
        let peer_id = node_id(c);
        let acceptor = SendHandle(provider);
        let accept = std::thread::spawn(move || {
            let acceptor = acceptor;
            let mut out: *mut c_char = std::ptr::null_mut();
            let mut len: usize = 0;
            // SAFETY: live handle and out-params.
            let rc = unsafe { net_mesh_accept(acceptor.0, peer_id, &mut out, &mut len) };
            if !out.is_null() {
                // SAFETY: allocated by this ABI, freed through its own free.
                unsafe { net::ffi::net_free_string(out) };
            }
            rc
        });
        let addr_c = cstr(&provider_addr);
        // SAFETY: live handle and NUL-terminated strings.
        let rc = unsafe {
            net_mesh_connect(c, addr_c.as_ptr(), provider_pub.as_ptr(), node_id(provider))
        };
        assert_eq!(rc, 0, "net_mesh_connect");
        assert_eq!(accept.join().expect("accept thread"), 0, "net_mesh_accept");
    }
    for h in [provider, caller, foreign] {
        // SAFETY: live handle.
        assert_eq!(unsafe { net_mesh_start(h) }, 0, "net_mesh_start");
    }

    // Register the deallocator before the dispatcher — the library
    // refuses the dispatcher without one on Windows, because it cannot
    // know which allocator produced a callback's buffer.
    //
    // This test stands in for the Go module, and it allocates with
    // `libc::malloc` from *this* test binary. Its matching release is
    // this binary's `libc::free`, which is exactly the point: the
    // allocator supplies the deallocator, so nobody has to assume the
    // two modules share a heap.
    unsafe extern "C" fn dispatcher_free(p: *mut std::ffi::c_void) {
        // SAFETY: only ever called with a pointer this file's
        // `libc::malloc` produced, or NULL.
        unsafe { libc::free(p) };
    }
    assert_eq!(
        net_org_set_callback_free(Some(dispatcher_free)),
        0,
        "the deallocator must be accepted"
    );
    assert_eq!(
        net_org_set_handler_dispatcher(dispatcher),
        0,
        "the dispatcher must be accepted once a deallocator is registered"
    );

    // ---- (2) an unknown export is refused LOCALLY ----
    let service = cstr(&m.exported_service);
    let unknown = cstr(&m.unknown_export_name);
    let handler_id = net_org_reserve_handler_id();
    // SAFETY: a fresh consumed clone plus live string and out-params.
    let (rc, wire) = unsafe {
        let arc = net_mesh_arc_clone(provider);
        let mut handle: *mut NetOrgServeHandle = std::ptr::null_mut();
        let mut err: *mut c_char = std::ptr::null_mut();
        let rc = net_subnet_serve_exported(
            arc,
            service.as_ptr(),
            m.exported_service.len(),
            unknown.as_ptr(),
            m.unknown_export_name.len(),
            handler_id,
            &mut handle,
            &mut err,
        );
        (rc, take_err(err))
    };
    assert_ne!(
        rc, NET_ORG_OK,
        "an unconfigured export name must be refused"
    );
    assert_eq!(
        scan_kind(&wire),
        Some("unknown_export_name"),
        "the stable kind must ride out_err, got {wire:?}",
    );

    // ---- (3) serve through the frozen named-export API ----
    let export_name = cstr(&m.export_name);
    let serve_id = net_org_reserve_handler_id();
    // SAFETY: as above.
    let mut serve_handle: *mut NetOrgServeHandle = std::ptr::null_mut();
    let rc = unsafe {
        let arc = net_mesh_arc_clone(provider);
        let mut err: *mut c_char = std::ptr::null_mut();
        let rc = net_subnet_serve_exported(
            arc,
            service.as_ptr(),
            m.exported_service.len(),
            export_name.as_ptr(),
            m.export_name.len(),
            serve_id,
            &mut serve_handle,
            &mut err,
        );
        assert_eq!(
            rc,
            NET_ORG_OK,
            "serve the configured export: {}",
            take_err(err)
        );
        rc
    };
    assert_eq!(rc, NET_ORG_OK);

    // ---- (4) caller credentials, through net_org_credentials_new ----
    let bind_client = |mesh: *mut MeshNodeHandle, role: &fixtures::ScenarioSubnetCaller| {
        let membership = std::fs::read(path(&role.membership_path)).expect("membership");
        let dispatcher_bytes = std::fs::read(path(&role.dispatcher_path)).expect("dispatcher");
        let mut creds: *mut NetOrgCredentials = std::ptr::null_mut();
        let mut err: *mut c_char = std::ptr::null_mut();
        let rc = net_org_credentials_new(
            membership.as_ptr(),
            membership.len(),
            dispatcher_bytes.as_ptr(),
            dispatcher_bytes.len(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            std::ptr::null(),
            0,
            &mut creds,
            &mut err,
        );
        assert_eq!(rc, NET_ORG_OK, "credentials_new: {}", take_err(err));

        let mut client: *mut NetOrgClient = std::ptr::null_mut();
        let mut err: *mut c_char = std::ptr::null_mut();
        // SAFETY: a fresh consumed clone; `credentials` is consumed too.
        let rc =
            unsafe { net_org_bind(net_mesh_arc_clone(mesh), &mut creds, &mut client, &mut err) };
        assert_eq!(rc, NET_ORG_OK, "net_org_bind: {}", take_err(err));
        client
    };
    let client = bind_client(caller, &m.caller);
    let foreign_client = bind_client(foreign, &m.foreign_caller);

    // ---- (5) live public discovery, and (6) the call ----
    let empty_caps = cstr("{}");
    let request = br#"{"n":1}"#;
    let mut admitted: Option<Vec<u8>> = None;
    let mut last_wire = String::new();
    for _ in 0..90 {
        for h in [provider, caller] {
            // SAFETY: live handle, NUL-terminated JSON.
            unsafe { net_mesh_announce_capabilities(h, empty_caps.as_ptr()) };
        }
        let mut out_ptr: *mut u8 = std::ptr::null_mut();
        let mut out_len: usize = 0;
        let mut err: *mut c_char = std::ptr::null_mut();
        let rc = net_org_call_exported(
            client,
            service.as_ptr(),
            m.exported_service.len(),
            request.as_ptr(),
            request.len(),
            0,
            0,
            &mut out_ptr,
            &mut out_len,
            &mut err,
        );
        if rc == NET_ORG_OK {
            // SAFETY: the ABI wrote `(out_ptr, out_len)` on success.
            let body = unsafe { std::slice::from_raw_parts(out_ptr, out_len) }.to_vec();
            net_org_response_free(out_ptr, out_len);
            admitted = Some(body);
            break;
        }
        last_wire = take_err(err);
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    let body = admitted
        .unwrap_or_else(|| panic!("the exported call was never admitted; last error: {last_wire}"));
    assert_eq!(
        String::from_utf8_lossy(&body),
        r#"{"n":2,"servedBy":"c-abi-s4"}"#,
    );

    let calls_after_admit = {
        let guard = facts().lock();
        let f = guard.get(&serve_id).expect("handler ran");
        // ---- (7) ----
        assert!(
            f.attribution_ok,
            "the handler saw the verified caller and organization attribution",
        );
        assert_eq!(f.calls, 1, "handler ran exactly once");
        f.calls
    };

    // ---- (8) fail-closed: a FOREIGN-org caller with valid credentials ----
    let must_not_be_served = |c: *mut NetOrgClient, payload: &[u8]| {
        let mut out_ptr: *mut u8 = std::ptr::null_mut();
        let mut out_len: usize = 0;
        let mut err: *mut c_char = std::ptr::null_mut();
        // A 5 s deadline: a call after teardown gets no reply at all, and an
        // unbounded wait would turn a correct refusal into a hung test.
        let rc = net_org_call_exported(
            c,
            service.as_ptr(),
            m.exported_service.len(),
            payload.as_ptr(),
            payload.len(),
            5_000,
            0,
            &mut out_ptr,
            &mut out_len,
            &mut err,
        );
        if rc == NET_ORG_OK {
            net_org_response_free(out_ptr, out_len);
            panic!("the call must not be served");
        }
        let _ = take_err(err);
    };

    must_not_be_served(foreign_client, br#"{"n":50}"#);
    assert_eq!(
        facts().lock().get(&serve_id).unwrap().calls,
        calls_after_admit,
        "the handler must never run for a refused caller",
    );

    // ---- (9) the denial is not retried ----
    must_not_be_served(foreign_client, br#"{"n":51}"#);
    assert_eq!(
        facts().lock().get(&serve_id).unwrap().calls,
        calls_after_admit,
        "no retry may smuggle a refused caller into the handler",
    );

    // ---- (10) clean close, no callback racing teardown ----
    net_org_serve_handle_close(serve_handle);
    net_org_serve_handle_close(serve_handle); // idempotent
    must_not_be_served(client, br#"{"n":99}"#);
    assert_eq!(
        facts().lock().get(&serve_id).unwrap().calls,
        1,
        "no handler invocation may land after close",
    );

    // Teardown, through the ABI's own frees.
    net_org_serve_handle_free(&mut serve_handle);
    let mut client = client;
    let mut foreign_client = foreign_client;
    net_org_client_free(&mut client);
    net_org_client_free(&mut foreign_client);
    for h in [provider, caller, foreign] {
        // SAFETY: each came from `net_mesh_new` and is freed once.
        unsafe { net_mesh_free(h) };
    }
    let _ = std::fs::remove_dir_all(&dir);
}
