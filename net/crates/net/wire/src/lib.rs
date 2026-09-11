//! The Net mesh **wire layer**: everything that decides what a packet
//! looks like on the wire, with nothing that decides how it is sent.
//!
//! Extracted from `net-mesh`'s `adapter::net` in Stage 2 of
//! `docs/internal/plans/BROWSER_NATIVE_WEBRTC_TRANSPORT_PLAN.md` (§7),
//! along the boundary the S0a spike proved
//! (`docs/internal/spikes/S0A_WIRE_BOUNDARY.md`). The core depends on
//! this crate and re-exports every module under its previous path, so
//! no `use net::adapter::net::…` site changed.
//!
//! What is here:
//!
//! - [`protocol`] — the `NetHeader` byte layout and the frame codecs.
//! - [`crypto`] — the Noise NKpsk0 handshake and the packet cipher.
//! - [`aead`] — the packet-AEAD backend seam: `ring` natively, the
//!   pure-Rust `chacha20poly1305` on `wasm32` (`ring` needs a
//!   wasm32-targeting clang to build at all).
//! - [`pool`], [`batch`] — packet construction and batching.
//! - [`stream`], [`reliability`], [`session`] — per-stream state,
//!   retransmission and session state.
//! - [`route_codec`] — the routing **envelope** codec (the route table
//!   stays in the core).
//! - [`route_hop`] — the authenticated route-hop envelope.
//! - [`parsed_packet`], [`peer_addr`] — the receive-side packet view
//!   and the endpoint type the session and the packet are keyed on.
//! - [`clock`], [`time`] — the monotonic/wall-clock seam. On
//!   `wasm32-unknown-unknown` `std::time::Instant::now()` compiles and
//!   then **panics**, so every time read in this crate goes through
//!   [`clock::Clock`].
//! - [`event`] — `StoredEvent`, the element of the session's inbound
//!   queue.
//!
//! What is deliberately **not** here: sockets, tokio, the route table,
//! the mesh node, the submission surface (`PeerSink`). This crate has
//! no native runtime dependency, and `cargo check --target
//! wasm32-unknown-unknown -p net-mesh-wire` is a CI job.

#![deny(missing_docs)]

pub mod aead;
pub mod batch;
pub mod clock;
pub mod crypto;
pub mod event;
pub mod parsed_packet;
pub mod peer_addr;
pub mod pool;
pub mod protocol;
pub mod reliability;
pub mod route_codec;
pub mod route_hop;
pub mod session;
pub mod stream;
pub mod time;

pub use peer_addr::PeerAddr;
