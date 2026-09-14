//! `net-mesh-leaf` — the Net **leaf profile**: a browser node.
//!
//! Stage 5 of
//! `docs/internal/plans/BROWSER_NATIVE_WEBRTC_TRANSPORT_PLAN.md`
//! (§7 leaf profile, §8 identity and leader election, §9 browser ↔
//! browser). The leaf owns no second implementation of the wire:
//! framing, Noise, the packet AEAD, reliability and the routing
//! envelope all come from `net-mesh-wire`, the same crate the native
//! node links.
//!
//! **What a leaf is** (§7): a node that never forwards. It never
//! originates pingwaves, never re-floods announcements, drops any
//! routing envelope not addressed to itself, tags its announcement
//! `leaf` + `transport:rtc`, and sets normal TTLs on what it
//! originates.
//!
//! **What it is not**: an anchor, a router, or a place where a second
//! copy of the protocol lives.
//!
//! ## Layout
//!
//! - [`error`] — the typed failures, including the corrected
//!   `UdpBlocked`/`IceTimeout` distinction.
//! - [`control_plane`] — the boundary between the leaf and whatever
//!   fronts the mesh for it, plus the session-independent signalling
//!   envelope the serverless follow-on needs.
//!
//! ## Targets
//!
//! wasm32 first, but the crate is not wasm-only: everything that does
//! not touch `web_sys` compiles and is tested natively, which is what
//! keeps the wire-level logic reviewable without a browser.

#![forbid(unsafe_code)]

/// The one v1 `ControlPlane`: a native anchor behind the Stage 4b
/// bootstrap listener. `wasm32`-only — it is HTTPS and a WebSocket.
#[cfg(target_arch = "wasm32")]
pub mod anchor_control_plane;
pub mod announce;
pub mod bootstrap;
pub mod channel;
pub mod clock;
pub mod control_plane;
pub mod counters;
pub mod dispatch;
pub mod enroll;
pub mod error;
pub mod frame;
pub mod identity;
/// §8 leader election and the D2 leader lifecycle: generations,
/// fencing, the follower registry and both ends of the follower
/// proxy. Deliberately `web_sys`-free — the browser half is
/// [`leader_session`] — so the lifecycle is reviewable and tested
/// without a browser.
/// The browser half of leader election: the Web Lock, the
/// `BroadcastChannel` carrier, and `MeshSession` as JavaScript sees
/// it. `wasm32`-only.
/// The genuinely anchorless in-memory `ControlPlane` (slice 6).
/// Test-only by feature, so a mock cannot become a production path
/// by accident.
#[cfg(feature = "mock-control-plane")]
pub mod mock_control_plane;
pub mod node;
pub mod rpc;
pub mod rpc_wire;
/// The browser transport. `wasm32`-only: it is the one module that
/// touches `web_sys`, and a native build has no `RTCPeerConnection`
/// to touch.
#[cfg(target_arch = "wasm32")]
pub mod rtc;
pub mod session;
pub mod signal;
/// §8 identity at rest: IndexedDB under a non-extractable WebCrypto
/// AES-GCM key, plus the fenced generation counter. `wasm32`-only;
/// the at-rest *format* lives in [`identity`], which is natively
/// tested.
pub mod stream;
/// The `cross_lang_wire` golden vectors, carried inside the
/// package so the wasm test target can replay them (and so an
/// unpacked `.crate` still builds its own tests).
#[cfg(feature = "test-vectors")]
pub mod test_vectors;
/// The `wasm-bindgen` surface — `LeafNode` and `LeafStream` as
/// JavaScript sees them.
#[cfg(target_arch = "wasm32")]
pub mod wasm;

pub use announce::{AnnouncementStore, VerifiedAnnouncement};
pub use channel::Channel;
pub use clock::Deadline;
pub use control_plane::{
    BootstrapAccepted, ControlEvent, ControlPlane, DialogId, IceCandidate, NodeId, Sdp,
    SignalEnvelope, SignalKind, SignedAnnouncement,
};
pub use counters::{DropReason, LeafCounters};
pub use dispatch::{Decoded, Subprotocol};
pub use enroll::{Invite, JoinOutcome};
pub use error::{LeafError, Result, RpcError, RtcError, UdpBlockedEvidence};
pub use frame::{Fragment, Reassembler};
pub use identity::{EntityKeypair, LeafIdentity};
#[cfg(feature = "mock-control-plane")]
pub use mock_control_plane::{Carried, CarriedKind, MockControlPlane, MockMesh};
pub use node::{LeafEvent, LeafNode, Outbound, StreamHandle};
pub use rpc::{CallResult, CallTable};
pub use session::{LeafSession, OpenedPacket, PendingHandshake, SessionTable};
pub use stream::{Reliability, RxStream};
