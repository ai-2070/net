//! S0a — wire-boundary spike.
//!
//! Throwaway scratch crate. It holds a verbatim copy of the seven wire
//! modules named in §7 of
//! `docs/internal/plans/BROWSER_NATIVE_WEBRTC_TRANSPORT_PLAN.md`, plus
//! the routing-envelope codec and the transitive pieces those modules
//! turned out to need, with tokio and the `net` crate cut out and
//! every `Instant` read behind [`clock::Clock`].
//!
//! Purpose: prove the boundary compiles for `wasm32-unknown-unknown`
//! and that a routed handshake/envelope round-trip works through the
//! extracted code. Evidence for Stage 2, not production code — see
//! `docs/internal/spikes/S0A_WIRE_BOUNDARY.md`.

#![allow(dead_code)]
#![allow(clippy::all)]

pub mod aead;
pub mod clock;
pub mod time;

// §7's seven modules, copied verbatim modulo import paths and the
// `Clock` seam.
pub mod batch;
pub mod crypto;
pub mod pool;
pub mod protocol;
pub mod reliability;
pub mod session;
pub mod stream;

// The routing-envelope codec (route.rs:1-319), no route table.
pub mod route_codec;

// Dragged in transitively — see the report.
pub mod event;
pub mod parsed_packet;
pub mod route_hop;

/// Exported wasm probe: forces the Noise handshake **and** the packet
/// cipher to be reachable from a `cdylib` entry point so neither is
/// dead-stripped from the `.wasm` size measurement.
///
/// Runs the same shape as the native round-trip test: NKpsk0
/// handshake between two in-memory endpoints, a `PacketBuilder`
/// packet wrapped in a `RoutingHeader`, unwrapped and decrypted.
/// Returns the decrypted payload length, or `0` on any failure.
///
/// `no_mangle` + `extern "C"` keeps the symbol in the module's export
/// table; `wasm-bindgen` is deliberately not a dependency of this
/// spike (`net-leaf`, not `net-wire`, owns the JS boundary).
///
/// # Safety
///
/// Takes no pointers and touches no foreign memory; the `unsafe` is
/// only the `no_mangle` attribute's requirement.
#[no_mangle]
pub extern "C" fn wasm_wire_probe(psk_byte: u8) -> u32 {
    match roundtrip::routed_roundtrip(psk_byte) {
        Ok(payload) => payload.len() as u32,
        Err(_) => 0,
    }
}

pub mod roundtrip;
