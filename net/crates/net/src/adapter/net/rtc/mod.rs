//! Native WebRTC DataChannel transport (Stage 3, `webrtc` feature).
//!
//! ```text
//!   mesh tasks                         driver task
//!   (PeerSink::try_send,               (owns the RTC UdpSocket and
//!    scheduler drain)                   every str0m::Rtc — nothing
//!        |                              else ever touches one)
//!        |-- submit(packet, RtcPeerId) ------>|  per-peer bounded queue
//!        |   synchronous, total:              |  (reserved slots+bytes)
//!        |   reserved slots+bytes and the     |
//!        |   published advisory decide here   |
//!        |                                    |
//!   receive loop  <--- bounded ingress -------|  IngressReceiver::Rtc
//!   (dispatch_packet, the single owner)       |
//! ```
//!
//! The shape is S0b's, which ran ~1 000 sessions against a real
//! headless Chromium: every mutation of an `Rtc` is followed by a
//! complete `poll_output` drain before the next one, one
//! `Channel::write` per drain, and the driver never blocks — a blocked
//! driver stalls every peer's `poll_output`, not just one.
//!
//! Admission is the only place a packet is refused. Past it the driver
//! owns the packet and retains it across `Ok(false)` until the channel
//! takes it or closes; a close discards the remainder **and counts it**
//! ([`RtcStats::discarded_at_close`]).

mod config;
mod driver;
#[cfg(any(test, feature = "fixtures"))]
mod loopback;
mod stats;
mod stun;
mod transport;

pub use config::{
    RtcConfig, DEFAULT_BUFFERED_AMOUNT_ADVISORY, DEFAULT_ICE_DEADLINE,
    DEFAULT_INGRESS_QUEUE_PACKETS, DEFAULT_MAX_PEERS, DEFAULT_SEND_QUEUE_BYTES,
    DEFAULT_SEND_QUEUE_PACKETS,
};
#[cfg(any(test, feature = "fixtures"))]
pub use loopback::connect_rtc_loopback;
pub use driver::{RtcDriver, RtcDriverHandle, RtcSignal};
pub use stats::RtcStats;
pub use stun::{binding_response, is_binding_request, parse_xor_mapped_address, STUN_MAGIC_COOKIE};
pub use transport::{RtcSubmitError, RtcTransport};

pub use net_wire::peer_addr::RtcPeerId;
