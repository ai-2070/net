//! The Go binding's single shared object.
//!
//! This crate defines almost nothing. Its job is to be the one place the seven
//! C-ABI surfaces and `net::ffi` are linked together, so a process loads ONE
//! copy of `net-mesh` instead of eight. See `Cargo.toml` for why that matters —
//! short version: per-`.so` `static`s do not unify the way exported functions
//! do, and `parking_lot_core`'s parked-thread registry is one of them.
//!
//! # Why `extern crate` and not just a dependency
//!
//! A dependency no code refers to may never be linked at all: rustc has no
//! reason to pull an rlib whose items are all unreachable from this crate's
//! root, and `#[no_mangle]` does not make an item reachable. Each line below is
//! load-bearing — dropping one silently removes that surface's symbols from
//! `libnet_go.so`, and the failure lands at `go build` link time in whichever
//! `go/*.go` file happened to call into it.
//!
//! The `#[allow(unused_extern_crates)]` is therefore deliberate: this is
//! exactly the case the lint cannot distinguish from a genuine leftover.
//!
//! The build-time guard in `ci.yml` counts the exported symbols per surface, so
//! a dropped line fails the build with a named diagnostic rather than a link
//! error three steps later.

#![allow(unused_extern_crates)]

// These are `[lib] name`s, not package names — `net-org-ffi` the package
// builds `net_org` the crate, and it is the latter that `extern crate` takes.
extern crate net_compute;
extern crate net_deck;
extern crate net_mcp_ffi;
extern crate net_meshdb;
extern crate net_meshos;
extern crate net_org;
extern crate net_rpc;

// `net::ffi`'s own `net_mesh_*` / `net_*` entry points ride in through the
// `net` dependency for the same reason.
extern crate net;

/// ABI generation of this bundle, for a consumer that wants to assert it loaded
/// the combined library rather than a stale per-surface one.
///
/// Deliberately the only symbol this crate defines itself: it gives the cdylib
/// a definition of its own, so "did the umbrella link at all" and "did a given
/// surface survive" are separate questions with separate answers.
///
/// `1` is the collapse-to-one-cdylib layout. Bump it only for a change that
/// alters which surfaces are present, never for ordinary version bumps.
#[unsafe(no_mangle)]
pub extern "C" fn net_go_ffi_abi_version() -> u32 {
    1
}
