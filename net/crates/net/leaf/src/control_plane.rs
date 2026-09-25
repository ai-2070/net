//! The control-plane boundary (Stage 5 slice 2).
//!
//! Everything a leaf needs from an anchor that is **not** a Net
//! packet on the data path: the bootstrap offer/answer, candidate
//! trickle, announcement publish/subscribe, and the signalling
//! transport for a peer the leaf has no session with yet.
//!
//! **Why a trait on day one.** The serverless follow-on (plan,
//! §Follow-on) replaces each anchor *function* with a serverless
//! substitute; with this boundary that is a second implementation,
//! without it Tier A alone is a leaf refactor. Two rules keep the
//! boundary honest and are asserted by
//! `tests/control_plane_boundary.rs`:
//!
//! 1. **No anchor type crosses it.** No `PeerAddr`, no `PeerAddr::Rtc`,
//!    no anchor node handle, no HTTP type. Addresses appear only as
//!    opaque strings the implementation itself minted.
//! 2. **No Net data packet crosses it.** The control plane carries
//!    SDP, ICE candidates, signed announcements and signed signalling
//!    envelopes. A control plane that forwards Net packets is a relay
//!    wearing a trait, and the anchorless mock in slice 6 would prove
//!    nothing.
//!
//! The trait is `async fn`-in-trait and deliberately **not** `Send`:
//! the leaf runs on the browser main thread (S0b — `RTCPeerConnection`
//! is undefined in workers), and requiring `Send` would force
//! `wasm_bindgen_futures` values through an abstraction that cannot
//! hold them.

use core::future::Future;

use crate::error::LeafError;

/// A node id as the control plane sees it: an opaque 64-bit mesh id.
/// Not an address — the control plane never learns where a peer is.
pub type NodeId = u64;

/// One signalling exchange between two nodes, numbered by the side
/// that started it. The same value the native `0x0D02` frames carry.
///
/// **0 is reserved** as the driver's no-attempt sentinel (`if dialog
/// != 0 { end_attempt(dialog) }`), so the one parser that mints
/// dialogs from a listener response refuses it at parse rather than
/// handing the driver a dialog it will silently never end.
pub type DialogId = u64;

/// An SDP blob. Opaque to the control plane: it is carried, never
/// parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sdp(pub String);

/// One ICE candidate line plus its media id, exactly as the browser
/// emits it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IceCandidate {
    /// The `candidate:` line.
    pub candidate: String,
    /// The media id the line belongs to.
    pub mid: String,
}

/// What the leaf gets back when a control plane accepts its offer.
///
/// Candidates are **not** here. The one v1 implementation learns the
/// anchor's first candidate from the trickle socket's first frame
/// (`rtc_bootstrap.rs` sends it there, not in the offer response),
/// and the anchorless path carries every candidate as a signed
/// [`SignalEnvelope`] — so both worlds deliver candidates through
/// [`ControlEvent`] and nothing had a candidate to put here.
#[derive(Debug, Clone)]
pub struct BootstrapAccepted {
    /// The dialog this attempt owns.
    pub dialog: DialogId,
    /// The answer to install as the remote description.
    pub answer: Sdp,
    /// The peer's Noise static public key, **pinned by whatever
    /// authenticated this attempt** — for `AnchorControlPlane`, the
    /// bootstrap credential. The leaf handshakes against exactly this
    /// key; a key learned from the answer would authenticate nothing.
    pub peer_static: [u8; 32],
    /// The peer's mesh node id.
    pub peer_node: NodeId,
}

/// One event a control plane delivers to the leaf.
#[derive(Debug, Clone)]
pub enum ControlEvent {
    /// A remote candidate for a live attempt.
    Candidate {
        /// The attempt it belongs to.
        dialog: DialogId,
        /// The candidate itself.
        candidate: IceCandidate,
    },
    /// The far side ended the attempt, with the reason it gave.
    AttemptEnded {
        /// The attempt that ended.
        dialog: DialogId,
        /// Why, in the implementation's own words.
        reason: String,
    },
    /// A signed announcement the leaf should ingest. **Verified by
    /// the leaf, not by the transport** — that is what makes a dumb
    /// store sufficient in the serverless follow-on.
    Announcement(SignedAnnouncement),
    /// A signalling envelope addressed to this leaf from a peer it
    /// may have no session with. See [`SignalEnvelope`].
    Signal(SignalEnvelope),
}

/// A signed capability announcement, carried verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedAnnouncement(pub Vec<u8>);

/// An organization root's signed revocation-floor bundle, carried
/// verbatim. Opaque to the carrier exactly like
/// [`SignedAnnouncement`]: the leaf's org module verifies the
/// signature and merges the floors raise-only. See
/// [`ControlPlane::take_revocation_bundles`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevocationBundle(pub Vec<u8>);

/// **The session-independent signalling envelope.**
///
/// This is the design Kyra pinned and the plan requires Stage 5 to
/// specify (plan, Stage 5: "the trait alone does not reconcile the
/// two startup sequences").
///
/// The native §5 path establishes a routed A↔B Noise session first
/// and sends `0x0D02` frames *inside* it, so the frames inherit the
/// session's authentication and the anchors forward blind. A
/// serverless Tier A control plane has no relay and therefore cannot
/// establish that session, so the same frames would arrive
/// unauthenticated.
///
/// An envelope closes that gap by authenticating **itself**:
///
/// - `from` / `to` / `dialog` / `kind` / `payload` / `not_after` are
///   the signed body, in that order, length-prefixed;
/// - `signature` is the sender's Ed25519 entity signature over that
///   body, verified against the `noise_pubkey`-bearing signed
///   announcement the receiver already holds for `from` (§5 Layer 1
///   — key discovery precedes signalling in both worlds);
/// - `not_after` bounds replay to a window, and the receiver keeps a
///   `(from, dialog, kind, payload digest)` seen-set for that
///   window — only a byte-identical re-send is a replay;
/// - the carrier — an anchor, a room object, a mock — learns nothing
///   and can tamper with nothing that verifies.
///
/// **v1 implements only what it needs**: `AnchorControlPlane` carries
/// envelopes for the bootstrap dialog it already owns. The trait's
/// shape admits the specified path because `signal` takes a peer id
/// and an envelope, not a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignalEnvelope {
    /// Who signed it.
    pub from: NodeId,
    /// Who it is for.
    pub to: NodeId,
    /// The exchange it belongs to.
    pub dialog: DialogId,
    /// Offer, answer, candidate or reject — the `0x0D02` kinds.
    pub kind: SignalKind,
    /// The kind's payload: SDP text, a candidate line, a reason.
    pub payload: Vec<u8>,
    /// Unix seconds after which a receiver must refuse it.
    pub not_after: u64,
    /// Ed25519 over [`SignalEnvelope::signing_bytes`].
    pub signature: Vec<u8>,
}

/// The `0x0D02` message kinds, as the envelope carries them.
// `Ord` so a set of kinds has one printed order: the anchorless
// mock's accounting ledger is an assertion, and an assertion that
// reordered between runs would be useless.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SignalKind {
    /// An SDP offer.
    Offer,
    /// An SDP answer.
    Answer,
    /// One ICE candidate.
    Candidate,
    /// The sender is abandoning the dialog.
    Reject,
}

impl SignalKind {
    /// The single byte that names the kind inside the signed body.
    pub const fn tag(self) -> u8 {
        match self {
            Self::Offer => 1,
            Self::Answer => 2,
            Self::Candidate => 3,
            Self::Reject => 4,
        }
    }
}

impl SignalEnvelope {
    /// The exact bytes a signature covers.
    ///
    /// Length-prefixed and in a fixed order, so no two distinct
    /// envelopes share a signing input: a signature for one dialog,
    /// kind or recipient cannot be replayed onto another.
    pub fn signing_bytes(&self) -> Vec<u8> {
        // 14 = the `net.signal.v1\0` magic; then from, to, dialog,
        // kind, not_after, the payload length, and the payload.
        let mut out = Vec::with_capacity(14 + 8 + 8 + 8 + 1 + 8 + 4 + self.payload.len());
        out.extend_from_slice(b"net.signal.v1\0");
        out.extend_from_slice(&self.from.to_le_bytes());
        out.extend_from_slice(&self.to.to_le_bytes());
        out.extend_from_slice(&self.dialog.to_le_bytes());
        out.push(self.kind.tag());
        out.extend_from_slice(&self.not_after.to_le_bytes());
        out.extend_from_slice(&(self.payload.len() as u32).to_le_bytes());
        out.extend_from_slice(&self.payload);
        out
    }
}

/// The boundary itself.
///
/// One trait, four jobs: get an attempt accepted, trickle candidates
/// on it, publish and receive announcements, and carry signalling for
/// a peer with no session yet.
pub trait ControlPlane {
    /// Offer to **whatever this control plane fronts**, and get an
    /// accepted attempt back.
    ///
    /// One argument, not two. The earlier shape took
    /// `peer: Option<NodeId>` and promised that `Some(peer)` would
    /// travel as a [`SignalEnvelope`] — a promise no control plane
    /// can keep, because an envelope is signed by the *leaf's*
    /// entity key and a carrier holds no key to sign with. A carrier
    /// that minted an offer on a leaf's behalf would be precisely
    /// the trusted relay the envelope exists to remove.
    ///
    /// So: this is the front door and nothing else. For
    /// `AnchorControlPlane` the peer is the anchor, authenticated by
    /// the bootstrap credential, which is also what pins
    /// [`BootstrapAccepted::peer_static`]. A peer-to-peer attempt is
    /// [`ControlPlane::signal`] in both directions, and a control
    /// plane that fronts nobody — the serverless room object, the
    /// anchorless mock — refuses this call by contract rather than
    /// inventing an authority it does not have.
    fn offer(&self, offer: Sdp) -> impl Future<Output = Result<BootstrapAccepted, LeafError>>;

    /// Send one local candidate for a live attempt.
    fn trickle(
        &self,
        dialog: DialogId,
        candidate: IceCandidate,
    ) -> impl Future<Output = Result<(), LeafError>>;

    /// Abandon an attempt. Idempotent: the second call for a dialog
    /// that is already gone is not an error.
    fn end_attempt(&self, dialog: DialogId) -> impl Future<Output = Result<(), LeafError>>;

    /// Publish this leaf's signed announcement.
    fn publish_announcement(
        &self,
        announcement: SignedAnnouncement,
    ) -> impl Future<Output = Result<(), LeafError>>;

    /// Ask for the announcements the control plane holds for a
    /// capability, so a leaf can discover a peer before it has any
    /// session. The reply is a list of signed announcements the leaf
    /// verifies itself.
    fn query_capability(
        &self,
        capability: &str,
    ) -> impl Future<Output = Result<Vec<SignedAnnouncement>, LeafError>>;

    /// Carry one self-authenticating envelope to `envelope.to`.
    ///
    /// The session-independent path: no A↔B session is required, and
    /// the carrier is not trusted with the contents.
    fn signal(&self, envelope: SignalEnvelope) -> impl Future<Output = Result<(), LeafError>>;

    /// Take whatever the control plane has received since the last
    /// call. Polled by the leaf's own loop, which keeps the trait
    /// free of a callback type and free of `Send`.
    fn drain_events(&self) -> Vec<ControlEvent>;

    /// Take every org-revocation bundle the carrier delivered since
    /// the last call. This is the revocation FEED's leaf endpoint.
    ///
    /// A bundle is an organization root's signed revocation-floor
    /// bundle on the wire — opaque here exactly like
    /// [`SignedAnnouncement`]: the carrier delivers the bytes, the
    /// leaf's own org module verifies the signature and merges the
    /// floors raise-only. A control plane that parsed, merged or
    /// reordered floors would be an authority this boundary exists
    /// to deny, so the signature check stays on the far side of it —
    /// "verified by the leaf, not by the transport", as with
    /// [`ControlEvent::Announcement`].
    ///
    /// The default is "no feed": a control plane with no revocation
    /// carrier hands back nothing and every certificate verifies
    /// against implicit floor 0 — core's un-adopted-node behaviour,
    /// exactly. Implementations override only to deliver what their
    /// carrier actually sent, in arrival order.
    fn take_revocation_bundles(&mut self) -> Vec<RevocationBundle> {
        vec![]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The signing input must separate every field, or a signature
    /// minted for one dialog is a signature for another.
    #[test]
    fn the_signed_body_distinguishes_every_field() {
        let base = SignalEnvelope {
            from: 1,
            to: 2,
            dialog: 3,
            kind: SignalKind::Offer,
            payload: b"v=0".to_vec(),
            not_after: 100,
            signature: Vec::new(),
        };
        let mut others = Vec::new();
        others.push(SignalEnvelope {
            from: 9,
            ..base.clone()
        });
        others.push(SignalEnvelope {
            to: 9,
            ..base.clone()
        });
        others.push(SignalEnvelope {
            dialog: 9,
            ..base.clone()
        });
        others.push(SignalEnvelope {
            kind: SignalKind::Answer,
            ..base.clone()
        });
        others.push(SignalEnvelope {
            not_after: 101,
            ..base.clone()
        });
        others.push(SignalEnvelope {
            payload: b"v=1".to_vec(),
            ..base.clone()
        });
        for other in others {
            assert_ne!(
                base.signing_bytes(),
                other.signing_bytes(),
                "two envelopes that differ must not share a signing input",
            );
        }
    }

    /// A length prefix, not a delimiter: a payload that contains the
    /// next field's bytes cannot forge that field.
    #[test]
    fn the_payload_cannot_run_into_the_next_field() {
        let a = SignalEnvelope {
            from: 1,
            to: 2,
            dialog: 3,
            kind: SignalKind::Candidate,
            payload: b"ab".to_vec(),
            not_after: 7,
            signature: Vec::new(),
        };
        let b = SignalEnvelope {
            payload: b"a".to_vec(),
            ..a.clone()
        };
        assert_ne!(a.signing_bytes(), b.signing_bytes());
        // …and the length is part of the input, so truncating the
        // payload changes the bytes even when the remainder matches.
        assert_ne!(
            a.signing_bytes().len(),
            b.signing_bytes().len(),
            "the length prefix must move with the payload",
        );
    }
}
