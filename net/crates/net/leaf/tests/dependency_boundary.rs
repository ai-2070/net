//! The default build must not learn about the browser.
//!
//! `net-mesh-leaf` pulls `wasm-bindgen`, `web-sys`, `js-sys` and
//! `wasm-bindgen-futures`. A native `cargo build --workspace` on the
//! mesh must not resolve or compile any of them, which is why this
//! crate carries its own `[workspace]` table and is built for
//! `wasm32-unknown-unknown` by its own job.
//!
//! That separation is a property of two manifests, so nothing in the
//! compiler enforces it: adding `"leaf"` to the mesh workspace's
//! member list would silently drag the browser dependency graph into
//! every native build and every `cargo tree`. This is the tripwire,
//! in the same spirit as the core's `bootstrap_dep_boundary`.
//!
//! It also asserts the other half — that the leaf does **not**
//! depend on the core, on tokio, on `ring` or on `str0m`. The leaf's
//! whole premise is that it links the same *wire* crate a native
//! node links and nothing else; a dependency on the core would make
//! "the leaf owns no second implementation of the wire" true by
//! accident rather than by construction, and would not build for
//! wasm32 at all.

use std::path::{Path, PathBuf};

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

/// A module's source with line comments removed.
///
/// The scans below look for *code*. Every one of these modules
/// explains in prose why it does not do the thing being scanned for
/// — `clock`'s whole doc-comment is about
/// `std::time::Instant::now()` panicking — so a scan over raw text
/// would flag the documentation that exists to prevent the bug.
fn code_only(body: &str) -> String {
    body.lines()
        .filter(|line| {
            let trimmed = line.trim_start();
            !trimmed.starts_with("//")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// This crate declares its own workspace, so it is not a member of
/// the mesh one.
#[test]
fn the_leaf_is_its_own_workspace() {
    let manifest = read(&manifest_dir().join("Cargo.toml")).expect("own manifest");
    assert!(
        manifest.contains("\n[workspace]"),
        "net-mesh-leaf must declare its own `[workspace]` table, or `cargo \
         build` from the repository root would pull wasm-bindgen, web-sys \
         and js-sys into the native mesh build"
    );
}

/// The mesh workspace does not list the leaf as a member.
#[test]
fn the_mesh_workspace_does_not_include_the_leaf() {
    let path = manifest_dir().join("../Cargo.toml");
    let Some(manifest) = read(&path) else {
        // Not a repository checkout (an unpacked `.crate`); the
        // property is about the repository, and the sibling test
        // above still holds here.
        return;
    };
    let members = manifest
        .split("[workspace]")
        .nth(1)
        .and_then(|tail| tail.split(']').next())
        .expect("the mesh manifest must declare workspace members");
    assert!(
        !members.contains("leaf"),
        "the mesh workspace lists the leaf as a member — a native \
         `cargo build --workspace` would now compile wasm-bindgen, \
         web-sys and js-sys. The leaf is built for \
         wasm32-unknown-unknown by its own job.\nmembers:{members}"
    );
}

/// The leaf links the wire crate and not the core, and carries no
/// native runtime.
#[test]
fn the_leaf_depends_on_the_wire_crate_and_nothing_native() {
    let manifest = read(&manifest_dir().join("Cargo.toml")).expect("own manifest");
    assert!(
        manifest.contains("net-mesh-wire = { path = \"../wire\""),
        "the leaf must depend on net-mesh-wire by path — it owns no second \
         implementation of framing, Noise, AEAD or reliability"
    );

    // Each of these would either fail to build for wasm32 or defeat
    // the reason the crate exists.
    for forbidden in [
        "net-mesh =",    // the core: tokio, sockets, the route table
        "net-mesh-sdk",  // likewise
        "tokio =",       // no runtime in a browser tab
        "ring =",        // needs a wasm32-targeting clang (S0a)
        "str0m =",       // the native RTC driver; the leaf uses web_sys
        "parking_lot =", // the leaf is single-threaded (S0b: main thread)
    ] {
        assert!(
            !manifest.contains(forbidden),
            "net-mesh-leaf must not depend on `{forbidden}` — see this \
             file's module docs"
        );
    }
}

/// `web_sys` is confined to the modules that must reach the browser.
///
/// Everything else compiles and is tested natively, which is what
/// keeps the wire-level logic reviewable without a browser. A
/// `web_sys` import leaking into `session`, `dispatch`, `frame`,
/// `stream`, `rpc` or `announce` would move protocol behaviour out
/// of the native suite's reach one line at a time.
///
/// `anchor_control_plane` is on the list because the v1 control
/// plane *is* HTTPS plus a WebSocket. `mock_control_plane` is
/// deliberately NOT on it: the anchorless mock is in-memory and
/// needs no browser at all, and keeping it under the guard is what
/// stops it quietly growing one — a mock that reached the network
/// would prove nothing about being anchorless.
#[test]
fn web_sys_is_confined_to_the_transport_and_the_bindgen_surface() {
    let src = manifest_dir().join("src");
    // Every entry here is a module that CANNOT be written without
    // the browser, and the list is kept at exactly that. It has
    // been tightened twice: `mock_control_plane.rs` came off it
    // because the anchorless mock turned out to need no
    // `wasm_bindgen` at all — which is the stronger position, since
    // a mock that reached the browser could not prove anything
    // about being anchorless — and `leader.rs` came off it because
    // §8's browser session was split into `leader_session.rs`,
    // restoring this guard's protection over the lifecycle logic
    // (the interruption budget, pending-call disposition, stream
    // and subscription restoration, and stale-leader fencing) that
    // a per-file allow-list would otherwise have left to a
    // convention.
    let allowed = [
        // The RTC transport and the bindgen surface.
        "rtc.rs",
        "wasm.rs",
        // `wasm.rs`'s own witnesses. They are a `#[cfg(test)]
        // #[path]`-included child module of it, so this is the SAME
        // surface rather than a new one, and it compiles only for
        // wasm32 under test. It is a separate FILE because the
        // witnesses must build a control plane — which means naming
        // `AnchorInfo` — and `control_plane_boundary.rs` scans
        // `wasm.rs`'s own text for exactly that name. Both guards
        // are right; the split is what satisfies them together.
        "wasm_witnesses.rs",
        // Layer 0: the bootstrap listener is HTTPS plus a
        // WebSocket, and the STUN probe needs a real
        // `RTCPeerConnection`.
        "bootstrap.rs",
        "anchor_control_plane.rs",
        // §8's at-rest identity: IndexedDB plus a non-extractable
        // WebCrypto AES-GCM key. There is no native equivalent to
        // test against, which is why the *format* of what it
        // stores stays decided in `identity.rs`, on this side of
        // the line, where it is natively testable.
        "storage.rs",
        // §8's browser session: the Web Lock, the
        // `BroadcastChannel` transport adapter and the follower
        // proxy's bindgen types. The lifecycle logic above it is
        // in `leader.rs`, which this guard still protects.
        "leader_session.rs",
    ];
    let mut checked = 0;
    for entry in std::fs::read_dir(&src).expect("src is readable") {
        let path = entry.expect("dir entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .expect("utf-8 file name")
            .to_string();
        let body = code_only(&read(&path).expect("module is readable"));
        checked += 1;
        if allowed.contains(&name.as_str()) {
            continue;
        }
        for forbidden in ["web_sys", "wasm_bindgen", "js_sys"] {
            assert!(
                !body.contains(forbidden),
                "{name} references `{forbidden}`. Only {allowed:?} may touch \
                 the browser; everything else must stay natively testable"
            );
        }
    }
    assert!(checked > 10, "only {checked} modules were inspected");
}

/// No module reads the clock through `std::time`.
///
/// On `wasm32-unknown-unknown` `std::time::Instant::now()` compiles
/// and then panics at runtime, and so does `SystemTime::now()`
/// (S0a). `cargo check` cannot see it, and the wasm test can only
/// catch the sites it happens to execute — this catches every site.
#[test]
fn nothing_reads_the_clock_outside_the_seam() {
    let src = manifest_dir().join("src");
    for entry in std::fs::read_dir(&src).expect("src is readable") {
        let path = entry.expect("dir entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .expect("utf-8 file name")
            .to_string();
        let body = code_only(&read(&path).expect("module is readable"));
        for forbidden in ["Instant::now()", "SystemTime::now()"] {
            assert!(
                !body.contains(forbidden),
                "{name} calls `{forbidden}`, which panics at runtime on \
                 wasm32-unknown-unknown. Read the clock through \
                 `crate::clock` (native `std::time`, wasm `web_time`)"
            );
        }
    }
}
