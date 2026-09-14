//! The control-plane boundary must stay a boundary.
//!
//! `control_plane.rs` states two rules. Neither is enforced by the
//! compiler, and both fail *silently* — the code keeps working
//! exactly as before and the serverless follow-on quietly stops
//! being an implementation:
//!
//! 1. **No anchor type crosses the trait.** No `PeerAddr`, no
//!    `PeerAddr::Rtc`, no anchor handle, no HTTP or WebSocket type.
//!    An address may appear only as an opaque string the
//!    implementation itself minted.
//! 2. **No Net data packet crosses it.** A control plane carries
//!    SDP, ICE candidates, signed announcements and signed
//!    signalling envelopes. One that forwarded packets would be a
//!    relay wearing a trait, and the anchorless mock would prove
//!    nothing.
//!
//! There is a third property this file exists to keep true, and it
//! is the one a refactor breaks first: **the bindgen surface must
//! not do the control plane's job itself.** Before Stage 5 slice 2
//! `wasm.rs` held the `fetch` calls, the trickle socket and the
//! listener's route strings inline, so the trait existed while
//! nothing used it. One inlined HTTP call is how a boundary stops
//! being one.
//!
//! # Why some of this is a source scan
//!
//! Two of the checks are free and already type-level, and this file
//! says so rather than re-asserting them:
//!
//! - **The trait module compiles natively.** `control_plane.rs` is
//!   not `#[cfg(target_arch = "wasm32")]`, so a trait method that
//!   took a `web_sys::Response`, a `WebSocket` or any other browser
//!   type would fail to compile this very test binary. The native
//!   build IS the assertion.
//! - **A control plane needs no anchor state.** [`NoAnchor`] below
//!   implements the whole trait over a zero-sized type. If a method
//!   ever required something only an anchor can supply, `NoAnchor`
//!   could not implement it and this file would not compile.
//!
//! What remains — that no *nameable* forbidden type appears in a
//! signature, and that the bindgen surface stops doing HTTP — cannot
//! be expressed in the type system, because the failure mode is
//! code that compiles perfectly.

use std::path::PathBuf;

use net_leaf::control_plane::{
    BootstrapAccepted, ControlEvent, ControlPlane, DialogId, IceCandidate, NodeId, Sdp,
    SignalEnvelope, SignedAnnouncement,
};
use net_leaf::error::LeafError;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn source(name: &str) -> String {
    let path = manifest_dir().join("src").join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{} is readable: {e}", path.display()))
}

/// A module's source with line comments removed.
///
/// The scans below look for *code*. Every one of these modules
/// explains in prose what it does not do — `control_plane.rs`'s
/// doc-comment is largely about `PeerAddr::Rtc` not crossing it —
/// so a scan over raw text would flag the documentation that exists
/// to prevent the bug.
fn code_only(body: &str) -> String {
    body.lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The `pub trait ControlPlane { … }` block, code only.
fn trait_body() -> String {
    let code = code_only(&source("control_plane.rs"));
    let start = code
        .find("pub trait ControlPlane {")
        .expect("control_plane.rs must declare `pub trait ControlPlane`");
    let tail = &code[start..];
    let end = tail
        .find("\n}")
        .expect("the trait block must close at column zero");
    tail[..end].to_string()
}

/// Rule 1, on the signatures: nothing an anchor owns is nameable in
/// the trait.
///
/// The list is not decoration. `PeerAddr`/`RtcPeerId` is the session
/// table's key and a browser has none; `Credential`, `AnchorInfo`
/// and `OfferAccepted` are the 4b listener's wire shapes; `Request`,
/// `Response`, `WebSocket` and `Url` are the transport that happens
/// to be HTTP today; `RtcPeerConnection` and `RtcDataChannel` are
/// the browser's. A Tier A control plane backed by a signed object
/// in blob storage has none of them, and each one that appeared here
/// would be a thing it had to fake.
#[test]
fn no_anchor_type_is_nameable_in_the_trait() {
    let body = trait_body();
    for forbidden in [
        "PeerAddr",
        "RtcPeerId",
        "Credential",
        "AnchorInfo",
        "OfferAccepted",
        "Request",
        "Response",
        "WebSocket",
        "Url",
        "RtcPeerConnection",
        "RtcDataChannel",
        "JsValue",
        "web_sys",
        "js_sys",
        "attempt_token",
        "bootstrap_url",
    ] {
        assert!(
            !body.contains(forbidden),
            "`{forbidden}` appears in the ControlPlane trait. The boundary's \
             first rule is that no anchor type crosses it — an address may \
             appear only as an opaque string the implementation minted.\n\
             trait body:\n{body}"
        );
    }
}

/// Rule 2, on the signatures: no Net packet type is nameable either.
///
/// `Bytes` is the wire crate's packet payload type and
/// `ParsedPacket`/`NetSession`/`PacketBuilder` are the data path. The
/// trait's byte-carrying types are `SignedAnnouncement` and
/// `SignalEnvelope::payload`, both `Vec<u8>`, both signed by the leaf
/// — which is exactly the difference between carrying an
/// authenticated blob and relaying a session's traffic.
#[test]
fn no_net_packet_type_is_nameable_in_the_trait() {
    let body = trait_body();
    for forbidden in [
        "ParsedPacket",
        "NetSession",
        "PacketBuilder",
        "Outbound",
        "packet",
        "datagram",
        "Bytes",
    ] {
        assert!(
            !body.contains(forbidden),
            "`{forbidden}` appears in the ControlPlane trait. A control plane \
             that carried Net packets would be a relay wearing a trait, and \
             the anchorless mock would prove nothing.\ntrait body:\n{body}"
        );
    }
}

/// The trait's module imports nothing from the anchor, the browser
/// or the wire.
///
/// A signature can stay clean while the module grows a
/// `use crate::bootstrap::Credential` for a helper — and the next
/// person puts it in a signature.
#[test]
fn the_trait_module_imports_nothing_from_the_anchor_or_the_wire() {
    let code = code_only(&source("control_plane.rs"));
    for forbidden in [
        "use net_wire",
        "use crate::bootstrap",
        "use crate::rtc",
        "use crate::session",
        "use crate::node",
        "web_sys",
        "js_sys",
        "wasm_bindgen",
    ] {
        assert!(
            !code.contains(forbidden),
            "control_plane.rs references `{forbidden}`. The trait is the one \
             module that must stay implementable by something with no anchor, \
             no browser and no session"
        );
    }
}

/// **The boundary is actually used.** `wasm.rs` performs no HTTP, no
/// WebSocket and knows none of the listener's routes.
///
/// This is the check that would have failed before slice 2 while
/// every other one passed: the trait existed and the connect
/// sequence ignored it.
#[test]
fn the_bindgen_surface_does_no_transport_of_its_own() {
    let code = code_only(&source("wasm.rs"));
    for forbidden in [
        "fetch_with_str",
        "fetch_with_request",
        "RequestInit",
        "WebSocket",
        "web_sys::Response",
        "/rtc/offer",
        "/rtc/anchor",
        "/rtc/trickle",
        "wss://",
        "attempt_token",
        "AnchorInfo",
        "OfferAccepted",
    ] {
        assert!(
            !code.contains(forbidden),
            "wasm.rs contains `{forbidden}`. The bindgen surface drives the \
             ControlPlane trait; the moment it speaks the anchor's transport \
             itself, the trait is decoration and the serverless follow-on is \
             a leaf refactor again"
        );
    }
    // And it does drive the trait.
    assert!(
        code.contains("AnchorControlPlane::attach"),
        "wasm.rs must build its control plane through `AnchorControlPlane::attach`"
    );
    for method in [
        "control.offer(",
        "control.trickle(",
        "control.signal(",
        "control.end_attempt(",
        "control.drain_events()",
    ] {
        assert!(
            code.contains(method),
            "wasm.rs never calls `{method}` — the boundary is not carrying \
             that job, so something else must be"
        );
    }
}

/// Neither implementation reaches the data path.
///
/// The two `impl ControlPlane` modules must not touch the node's
/// packet surface. A control plane that called `take_outbound`,
/// `on_datagram`, `stream_send` or the transport's `send` would be
/// forwarding Net packets no matter what its trait signatures say.
#[test]
fn no_control_plane_implementation_touches_the_data_path() {
    for module in ["anchor_control_plane.rs", "mock_control_plane.rs"] {
        let code = code_only(&source(module));
        // The tests at the foot of the mock construct a packet on
        // purpose, to prove the tripwire fires. The rule is about
        // the implementation.
        let implementation = code.split("mod tests").next().unwrap_or(&code).to_string();
        for forbidden in [
            "take_outbound",
            "on_datagram",
            "stream_send",
            "open_stream",
            "send_subprotocol",
            "announce_to_peer",
            "RtcLeafTransport",
            "LeafSession",
            "complete_handshake",
        ] {
            assert!(
                !implementation.contains(forbidden),
                "{module} references `{forbidden}`, which is the data path. \
                 A control plane carries signalling and nothing else"
            );
        }
    }
}

/// The anchor implementation does not hand its own types outward.
///
/// Its `pub` surface is what `wasm.rs` can reach, and it is allowed
/// two things beyond the trait: the anchor's node id and the
/// `rtc_addr` the STUN probe aims at (a string the implementation
/// minted, which rule 1 permits explicitly). A `pub fn` returning a
/// `WebSocket`, a `Response` or the credential would re-open the
/// boundary from the other side.
#[test]
fn the_anchor_implementations_public_surface_leaks_nothing() {
    let code = code_only(&source("anchor_control_plane.rs"));
    let signatures: Vec<&str> = code
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("pub fn") || line.starts_with("pub async fn"))
        .collect();
    assert!(
        signatures.len() >= 3,
        "expected the anchor implementation to have a public surface to check, \
         found {signatures:?}"
    );
    for signature in &signatures {
        for forbidden in [
            "WebSocket",
            "Response",
            "Request",
            "JsValue",
            "Credential",
            "PeerAddr",
            "AnchorInfo",
            "OfferAccepted",
        ] {
            // `attach` TAKES a credential — the page's own input,
            // travelling inward. It is the outward direction the
            // rule is about.
            if signature.starts_with("pub async fn attach") && forbidden == "Credential" {
                continue;
            }
            assert!(
                !signature.contains(forbidden),
                "`{signature}` mentions `{forbidden}`: the anchor's types must \
                 not reach the leaf, whatever the trait says"
            );
        }
    }
}

/// The mock holds no key material.
///
/// This is what makes the anchorless witness mean something. If the
/// mock could sign, it could mint an offer, and "the carrier is not
/// trusted" would be untested — the leaf would be relying on a
/// trustworthy mock instead of on the envelope's signature.
#[test]
fn the_anchorless_mock_holds_no_keys() {
    let path = manifest_dir().join("src").join("mock_control_plane.rs");
    let Ok(body) = std::fs::read_to_string(&path) else {
        panic!("{} is readable", path.display());
    };
    let code = code_only(&body);
    let implementation = code.split("mod tests").next().unwrap_or(&code).to_string();
    for forbidden in [
        "SigningKey",
        "StaticKeypair",
        "EntityKeypair",
        "LeafIdentity",
        "psk",
        "verify_entity_signature",
        "signal::verify",
        "signal::decode",
    ] {
        assert!(
            !implementation.contains(forbidden),
            "mock_control_plane.rs references `{forbidden}`. A carrier that \
             held a key, or that verified what it carries, would be trusted — \
             and the envelope exists so that it need not be"
        );
    }
}

// ───────────────────────── the type-level half ─────────────────────────

/// A control plane with **no anchor, no transport and no state**.
///
/// Zero-sized on purpose. It is the compile-time half of rule 1: if
/// a trait method ever needed something only an anchor could supply
/// — a socket, a credential, a session — this type could not
/// implement it and this test binary would not build.
///
/// Its bodies are refusals because there is nothing behind it; what
/// is being asserted is the *shape*, not the behaviour.
struct NoAnchor;

impl ControlPlane for NoAnchor {
    async fn offer(&self, _offer: Sdp) -> Result<BootstrapAccepted, LeafError> {
        Err(LeafError::ControlPlane("nothing is behind me".into()))
    }

    async fn trickle(&self, _dialog: DialogId, _candidate: IceCandidate) -> Result<(), LeafError> {
        Err(LeafError::ControlPlane("nothing is behind me".into()))
    }

    async fn end_attempt(&self, _dialog: DialogId) -> Result<(), LeafError> {
        Ok(())
    }

    async fn publish_announcement(
        &self,
        _announcement: SignedAnnouncement,
    ) -> Result<(), LeafError> {
        Err(LeafError::ControlPlane("nothing is behind me".into()))
    }

    async fn query_capability(
        &self,
        _capability: &str,
    ) -> Result<Vec<SignedAnnouncement>, LeafError> {
        Ok(Vec::new())
    }

    async fn signal(&self, _envelope: SignalEnvelope) -> Result<(), LeafError> {
        Err(LeafError::ControlPlane("nothing is behind me".into()))
    }

    fn drain_events(&self) -> Vec<ControlEvent> {
        Vec::new()
    }
}

/// The trait is implementable by a zero-sized type, and its
/// vocabulary is exactly the carrier vocabulary.
///
/// `size_of::<NoAnchor>() == 0` is the assertion that `NoAnchor` is
/// not quietly holding an anchor; the rest of this test is the
/// compiler having accepted the `impl` above.
#[test]
fn a_control_plane_needs_no_anchor_to_exist() {
    assert_eq!(core::mem::size_of::<NoAnchor>(), 0);

    // Every type a trait method mentions is constructible here, with
    // no anchor, no browser and no session — which is the property
    // the serverless follow-on depends on.
    let _: NodeId = 7;
    let _ = Sdp("v=0\r\n".into());
    let _ = IceCandidate {
        candidate: "candidate:1 1 udp 1 127.0.0.1 1 typ host".into(),
        mid: "0".into(),
    };
    let _ = SignedAnnouncement(vec![1, 2, 3]);
    let accepted = BootstrapAccepted {
        dialog: 1,
        answer: Sdp("v=0\r\n".into()),
        peer_static: [0u8; 32],
        peer_node: 9,
    };
    // The one key that crosses is the peer's Noise static, pinned by
    // whatever authenticated the attempt. It is 32 bytes, not a
    // handle to the thing that pinned it.
    assert_eq!(accepted.peer_static.len(), 32);
}
