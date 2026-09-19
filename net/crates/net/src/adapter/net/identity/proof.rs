//! Session-bound identity proof — establishing a peer's [`EntityId`]
//! **independently of capability publication**.
//!
//! # Why this exists
//!
//! Channel token admission binds a presented credential's leaf to the
//! peer that presented it, and it resolves "who is on the wire" from
//! the `node_id → EntityId` pin (`peer_entity_ids`). Before this
//! subprotocol there were exactly two ways that pin got installed:
//!
//! 1. a **signature-verified, hop-0 capability announcement** — i.e.
//!    the consumer had to advertise services on the discovery plane
//!    and wait for the producer to index the announcement, purely to
//!    use a credential already issued to it; and
//! 2. **verified subnet admission**, which is unavailable unless the
//!    deployment runs a subnet authority.
//!
//! Neither is what token admission actually needs. The prerequisite
//! is an authenticated peer→entity binding, not capability
//! publication, so a consumer with no services to advertise was
//! forced into an announce-and-poll warm-up that had nothing to do
//! with its request. This module provides the binding directly.
//!
//! # What it proves, and what it does not
//!
//! The exchange is a verifier-nonce challenge/response over an
//! existing encrypted session:
//!
//! ```text
//! prover  → verifier   ChallengeRequest { nonce }
//! prover ←  verifier   Challenge { nonce, verifier, challenge }
//! prover  → verifier   Proof { nonce, subject, challenge, signature }
//! prover ←  verifier   Verdict { nonce, accepted, reject }
//! ```
//!
//! The signature covers [`proof_transcript`]: a domain tag, the
//! **verifier's** entity, the **subject's** entity, the subject's
//! routing id, the session incarnation, and the one-use challenge.
//! That is proof-of-possession of the subject's ed25519 secret, bound
//! to one verifier, one session, and one nonce — so a captured proof
//! is useless to another verifier, on another session, or twice.
//!
//! Three non-properties worth stating, because each is a tempting
//! shortcut that proves nothing:
//!
//! - **A credential's subject is not a proof of identity.** The
//!   issuer's signature attests who *received* a grant, never that
//!   whoever presented the bytes owns that identity.
//! - **The session AEAD is not a proof of identity.** The Noise
//!   handshake authenticates an X25519 static key; the entity is an
//!   ed25519 key with no derivation between them.
//! - **A forwarded or unsigned capability announcement is not a proof
//!   of identity** — which is exactly why the announcement path pins
//!   only on the signed, hop-0 case.
//!
//! # Bounds
//!
//! [`IdentityChallengeStore`] mirrors
//! [`SubnetChallengeStore`](crate::adapter::net::subnet::admission::SubnetChallengeStore):
//! bounded per peer and node-wide, self-evicting, single-use on
//! consume whether verification then succeeds or fails. It is a
//! sibling rather than a shared instance on purpose — the two
//! protocols must not share a nonce namespace, and this one yields no
//! subnet binding.

use std::time::{Duration, Instant};

use bytes::{Buf, BufMut};
use dashmap::DashMap;

use super::EntityId;

/// Subprotocol ID for the identity-proof exchange. Sits beside
/// channel membership (`0x0A00`) because token admission is the first
/// consumer of the binding it installs.
pub const SUBPROTOCOL_IDENTITY_PROOF: u16 = 0x0A01;

/// How long an unissued-and-unconsumed challenge stays valid. Short:
/// the subject signs and returns it in one round trip.
pub const IDENTITY_CHALLENGE_TTL: Duration = Duration::from_secs(30);

/// Maximum outstanding challenges per peer. A peer that floods
/// attempts evicts only its own oldest entries.
pub const MAX_IDENTITY_CHALLENGES_PER_PEER: usize = 4;

/// Maximum peers holding outstanding challenges — node-wide backstop
/// against a fan-out flood.
pub const MAX_IDENTITY_CHALLENGE_PEERS: usize = 4096;

/// Domain separator for the signed transcript. Distinct from every
/// other ed25519 signing domain in the mesh so a signature minted
/// here can never be replayed as a token, an announcement, or a
/// subnet presentation.
const PROOF_DOMAIN: &[u8] = b"net-identity-proof-v1";

/// Byte length of the signed transcript.
pub const TRANSCRIPT_LEN: usize = PROOF_DOMAIN.len() + 32 + 32 + 8 + 8 + 32;

const MSG_CHALLENGE_REQUEST: u8 = 0;
const MSG_CHALLENGE: u8 = 1;
const MSG_PROOF: u8 = 2;
const MSG_VERDICT: u8 = 3;

const REJECT_NONE: u8 = 0;
const REJECT_BUSY: u8 = 1;
const REJECT_NO_CHALLENGE: u8 = 2;
const REJECT_WRONG_SESSION: u8 = 3;
const REJECT_SUBJECT_MISMATCH: u8 = 4;
const REJECT_BAD_SIGNATURE: u8 = 5;
const REJECT_PIN_CONFLICT: u8 = 6;

/// Why a verifier refused a proof. Diagnostics: none of these are
/// retryable with the same inputs except [`Self::Busy`] and
/// [`Self::NoChallenge`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityProofReject {
    /// The verifier's challenge ceiling is full, or it holds no
    /// session with the requester. Retryable.
    Busy,
    /// No live challenge matched — expired, already consumed, or
    /// issued on a session incarnation that has since been replaced.
    /// Retryable from a fresh `ChallengeRequest`.
    NoChallenge,
    /// The session incarnation the challenge was issued on is gone.
    WrongSession,
    /// The claimed subject's `EntityId::node_id()` is not the routing
    /// id the session resolves to. A peer may only prove its own
    /// identity.
    SubjectMismatch,
    /// The transcript signature did not verify under the claimed
    /// subject's key — the presenter does not hold that secret.
    BadSignature,
    /// A different entity is already pinned to this routing id. An
    /// availability event, never credential aliasing: an established
    /// pin is refused, never overwritten.
    PinConflict,
}

impl IdentityProofReject {
    /// Wire encoding.
    const fn to_byte(self) -> u8 {
        match self {
            Self::Busy => REJECT_BUSY,
            Self::NoChallenge => REJECT_NO_CHALLENGE,
            Self::WrongSession => REJECT_WRONG_SESSION,
            Self::SubjectMismatch => REJECT_SUBJECT_MISMATCH,
            Self::BadSignature => REJECT_BAD_SIGNATURE,
            Self::PinConflict => REJECT_PIN_CONFLICT,
        }
    }
}

impl std::fmt::Display for IdentityProofReject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Busy => "verifier busy",
            Self::NoChallenge => "no live challenge",
            Self::WrongSession => "session incarnation replaced",
            Self::SubjectMismatch => "claimed subject is not this session's peer",
            Self::BadSignature => "transcript signature did not verify",
            Self::PinConflict => "a different entity is already pinned to this routing id",
        };
        f.write_str(s)
    }
}

/// Identity-proof wire message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdentityProofMsg {
    /// Prover → verifier: mint me a challenge on this session.
    ChallengeRequest {
        /// Request correlation nonce, echoed by every later leg.
        nonce: u64,
    },
    /// Verifier → prover: sign this.
    Challenge {
        /// Correlation nonce from the request.
        nonce: u64,
        /// The verifier's own entity, so the prover can bind the
        /// transcript to it. Claimed, not proven — the verifier
        /// re-derives the transcript from its *real* entity before
        /// checking the signature, so a verifier that lies about this
        /// gets a proof bound to an identity it cannot present.
        verifier: EntityId,
        /// One-use 32-byte verifier nonce.
        challenge: [u8; 32],
    },
    /// Prover → verifier: proof of possession.
    Proof {
        /// Correlation nonce.
        nonce: u64,
        /// The entity the prover claims — checked against the
        /// session's routing id and the signature.
        subject: EntityId,
        /// The challenge being answered.
        challenge: [u8; 32],
        /// ed25519 signature over [`proof_transcript`].
        signature: [u8; 64],
    },
    /// Verifier → prover: outcome.
    Verdict {
        /// Correlation nonce.
        nonce: u64,
        /// Whether the binding was installed (or already matched).
        accepted: bool,
        /// Why not, when `accepted` is false.
        reject: Option<IdentityProofReject>,
    },
}

/// Error returned by the identity-proof codec.
#[derive(Debug, thiserror::Error)]
pub enum IdentityProofCodecError {
    /// Unknown or reserved message-type byte.
    #[error("unknown identity-proof message type: {0}")]
    UnknownType(u8),
    /// Unknown or reserved rejection byte.
    #[error("unknown identity-proof reject code: {0}")]
    UnknownReject(u8),
    /// Buffer ended mid-field.
    #[error("truncated identity-proof message: {0}")]
    Truncated(&'static str),
    /// Bytes remained after the last declared field.
    #[error("trailing bytes after identity-proof {0}")]
    Trailing(&'static str),
}

/// The bytes a prover signs.
///
/// Every component is load-bearing:
///
/// - `PROOF_DOMAIN` keeps the signature un-replayable as any other
///   mesh signature;
/// - `verifier` stops a proof presented to one node being relayed to
///   another;
/// - `subject` + `subject_node` bind the entity to the routing id the
///   session resolves to, so a proof cannot be re-keyed onto a
///   different peer slot;
/// - `session_id` (derived from the handshake hash, therefore
///   identical on both sides) bounds the proof to one incarnation, so
///   a reconnect cannot inherit it;
/// - `challenge` makes it fresh and single-use.
pub fn proof_transcript(
    verifier: &EntityId,
    subject: &EntityId,
    subject_node: u64,
    session_id: u64,
    challenge: &[u8; 32],
) -> [u8; TRANSCRIPT_LEN] {
    let mut out = [0u8; TRANSCRIPT_LEN];
    let mut at = 0;
    let mut put = |bytes: &[u8]| {
        out[at..at + bytes.len()].copy_from_slice(bytes);
        at += bytes.len();
    };
    put(PROOF_DOMAIN);
    put(verifier.as_bytes());
    put(subject.as_bytes());
    put(&subject_node.to_le_bytes());
    put(&session_id.to_le_bytes());
    put(challenge);
    out
}

/// Verify a presented proof against the verifier's own view of the
/// world. Pure: no state is touched, so the caller decides what a
/// success is worth.
///
/// `subject_node` is the routing id the **session** resolved to, never
/// a wire field, and `session_id` is the verifier's live incarnation.
pub fn verify_proof(
    verifier: &EntityId,
    subject: &EntityId,
    subject_node: u64,
    session_id: u64,
    challenge: &[u8; 32],
    signature: &[u8; 64],
) -> Result<(), IdentityProofReject> {
    // A peer may only prove its own identity: the claimed entity must
    // derive the routing id this session owns. Without this a peer
    // could prove a key it does hold and have it installed as some
    // other node's binding.
    if subject.node_id() != subject_node {
        return Err(IdentityProofReject::SubjectMismatch);
    }
    let transcript = proof_transcript(verifier, subject, subject_node, session_id, challenge);
    subject
        .verify_bytes(&transcript, signature)
        .map_err(|_| IdentityProofReject::BadSignature)
}

/// Encode an identity-proof message.
pub fn encode(msg: &IdentityProofMsg) -> Vec<u8> {
    let mut buf = Vec::with_capacity(144);
    match msg {
        IdentityProofMsg::ChallengeRequest { nonce } => {
            buf.put_u8(MSG_CHALLENGE_REQUEST);
            buf.put_u64_le(*nonce);
        }
        IdentityProofMsg::Challenge {
            nonce,
            verifier,
            challenge,
        } => {
            buf.put_u8(MSG_CHALLENGE);
            buf.put_u64_le(*nonce);
            buf.extend_from_slice(verifier.as_bytes());
            buf.extend_from_slice(challenge);
        }
        IdentityProofMsg::Proof {
            nonce,
            subject,
            challenge,
            signature,
        } => {
            buf.put_u8(MSG_PROOF);
            buf.put_u64_le(*nonce);
            buf.extend_from_slice(subject.as_bytes());
            buf.extend_from_slice(challenge);
            buf.extend_from_slice(signature);
        }
        IdentityProofMsg::Verdict {
            nonce,
            accepted,
            reject,
        } => {
            buf.put_u8(MSG_VERDICT);
            buf.put_u64_le(*nonce);
            buf.put_u8(u8::from(*accepted));
            buf.put_u8(reject.map_or(REJECT_NONE, IdentityProofReject::to_byte));
        }
    }
    buf
}

/// Decode an identity-proof message. Strict: every message rejects
/// trailing bytes, so a framer bug surfaces instead of being absorbed.
pub fn decode(data: &[u8]) -> Result<IdentityProofMsg, IdentityProofCodecError> {
    let mut cur = std::io::Cursor::new(data);
    if !cur.has_remaining() {
        return Err(IdentityProofCodecError::Truncated("empty"));
    }
    let tag = cur.get_u8();
    if cur.remaining() < 8 {
        return Err(IdentityProofCodecError::Truncated("nonce"));
    }
    let nonce = cur.get_u64_le();
    let msg = match tag {
        MSG_CHALLENGE_REQUEST => IdentityProofMsg::ChallengeRequest { nonce },
        MSG_CHALLENGE => {
            if cur.remaining() < 64 {
                return Err(IdentityProofCodecError::Truncated("challenge body"));
            }
            let mut verifier = [0u8; 32];
            cur.copy_to_slice(&mut verifier);
            let mut challenge = [0u8; 32];
            cur.copy_to_slice(&mut challenge);
            IdentityProofMsg::Challenge {
                nonce,
                verifier: EntityId::from_bytes(verifier),
                challenge,
            }
        }
        MSG_PROOF => {
            if cur.remaining() < 128 {
                return Err(IdentityProofCodecError::Truncated("proof body"));
            }
            let mut subject = [0u8; 32];
            cur.copy_to_slice(&mut subject);
            let mut challenge = [0u8; 32];
            cur.copy_to_slice(&mut challenge);
            let mut signature = [0u8; 64];
            cur.copy_to_slice(&mut signature);
            IdentityProofMsg::Proof {
                nonce,
                subject: EntityId::from_bytes(subject),
                challenge,
                signature,
            }
        }
        MSG_VERDICT => {
            if cur.remaining() < 2 {
                return Err(IdentityProofCodecError::Truncated("verdict body"));
            }
            // Strict boolean, matching the membership ACK codec: a
            // malformed sender must not be able to make an unknown
            // code imply acceptance.
            let accepted = match cur.get_u8() {
                0 => false,
                1 => true,
                other => return Err(IdentityProofCodecError::UnknownType(other)),
            };
            let reject = match cur.get_u8() {
                REJECT_NONE => None,
                REJECT_BUSY => Some(IdentityProofReject::Busy),
                REJECT_NO_CHALLENGE => Some(IdentityProofReject::NoChallenge),
                REJECT_WRONG_SESSION => Some(IdentityProofReject::WrongSession),
                REJECT_SUBJECT_MISMATCH => Some(IdentityProofReject::SubjectMismatch),
                REJECT_BAD_SIGNATURE => Some(IdentityProofReject::BadSignature),
                REJECT_PIN_CONFLICT => Some(IdentityProofReject::PinConflict),
                other => return Err(IdentityProofCodecError::UnknownReject(other)),
            };
            IdentityProofMsg::Verdict {
                nonce,
                accepted,
                reject,
            }
        }
        other => return Err(IdentityProofCodecError::UnknownType(other)),
    };
    if cur.has_remaining() {
        return Err(IdentityProofCodecError::Trailing(match tag {
            MSG_CHALLENGE_REQUEST => "challenge request",
            MSG_CHALLENGE => "challenge",
            MSG_PROOF => "proof",
            _ => "verdict",
        }));
    }
    Ok(msg)
}

/// One outstanding challenge, bound to the session incarnation it was
/// issued on.
#[derive(Debug, Clone)]
struct PendingChallenge {
    nonce: [u8; 32],
    session_id: u64,
    issued_at: Instant,
}

/// Verifier-side challenge state for the identity-proof exchange.
///
/// A challenge is single-use: [`Self::consume`] removes it whether or
/// not verification later succeeds, so a captured proof cannot be
/// replayed against the same nonce.
#[derive(Debug, Default)]
pub struct IdentityChallengeStore {
    /// `node_id → outstanding challenges`, newest last.
    by_peer: DashMap<u64, Vec<PendingChallenge>>,
}

impl IdentityChallengeStore {
    /// Empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Mint and retain a fresh challenge for `node_id` on
    /// `session_id`, returning the nonce to send. `None` when the
    /// node-wide ceiling is full of live challenges.
    ///
    /// Aborts rather than unwinding on RNG failure: a predictable
    /// challenge would make a captured proof replayable, which is the
    /// one property this exchange exists to provide.
    pub fn issue(&self, node_id: u64, session_id: u64, now: Instant) -> Option<[u8; 32]> {
        if !self.by_peer.contains_key(&node_id)
            && self.by_peer.len() >= MAX_IDENTITY_CHALLENGE_PEERS
        {
            // Pruning is otherwise per-peer on that peer's own next
            // touch, so the ceiling could be held entirely by peers
            // that asked once and never returned. Sweeping here keeps
            // the bound a cap on LIVE challenges, and costs O(peers)
            // only on the refusal path.
            self.by_peer.retain(|_, slot| {
                slot.retain(|c| now.duration_since(c.issued_at) < IDENTITY_CHALLENGE_TTL);
                !slot.is_empty()
            });
            if self.by_peer.len() >= MAX_IDENTITY_CHALLENGE_PEERS {
                return None;
            }
        }
        let mut nonce = [0u8; 32];
        if let Err(e) = getrandom::fill(&mut nonce) {
            eprintln!(
                "FATAL: identity challenge getrandom failure ({e:?}); aborting to avoid a predictable challenge"
            );
            std::process::abort();
        }
        let mut slot = self.by_peer.entry(node_id).or_default();
        slot.retain(|c| now.duration_since(c.issued_at) < IDENTITY_CHALLENGE_TTL);
        if slot.len() >= MAX_IDENTITY_CHALLENGES_PER_PEER {
            slot.remove(0);
        }
        slot.push(PendingChallenge {
            nonce,
            session_id,
            issued_at: now,
        });
        Some(nonce)
    }

    /// Consume the challenge matching `nonce` on `session_id`.
    ///
    /// Removal is unconditional on match — consumed on accept AND on
    /// reject — so a failed attempt cannot retry the same nonce. An
    /// expired entry is dropped and reported as absent.
    pub fn consume(&self, node_id: u64, session_id: u64, nonce: &[u8; 32], now: Instant) -> bool {
        let Some(mut slot) = self.by_peer.get_mut(&node_id) else {
            return false;
        };
        slot.retain(|c| now.duration_since(c.issued_at) < IDENTITY_CHALLENGE_TTL);
        let Some(idx) = slot.iter().position(|c| {
            c.session_id == session_id
                && bool::from(subtle::ConstantTimeEq::ct_eq(&c.nonce[..], &nonce[..]))
        }) else {
            return false;
        };
        slot.remove(idx);
        let empty = slot.is_empty();
        drop(slot);
        if empty {
            self.by_peer.remove_if(&node_id, |_, v| v.is_empty());
        }
        true
    }

    /// Drop every challenge for a peer — session replacement, peer
    /// failure, and eviction all invalidate outstanding attempts.
    pub fn forget_peer(&self, node_id: u64) {
        self.by_peer.remove(&node_id);
    }

    /// Outstanding challenge count (diagnostics/tests).
    pub fn len(&self) -> usize {
        self.by_peer.iter().map(|e| e.value().len()).sum()
    }

    /// `true` when no challenge is outstanding.
    pub fn is_empty(&self) -> bool {
        self.by_peer.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::identity::EntityKeypair;

    fn round_trip(msg: IdentityProofMsg) {
        let bytes = encode(&msg);
        let back = decode(&bytes).expect("decode");
        assert_eq!(back, msg);
    }

    #[test]
    fn every_message_round_trips() {
        round_trip(IdentityProofMsg::ChallengeRequest { nonce: 0xDEAD_BEEF });
        round_trip(IdentityProofMsg::Challenge {
            nonce: 7,
            verifier: EntityId::from_bytes([0x11; 32]),
            challenge: [0x22; 32],
        });
        round_trip(IdentityProofMsg::Proof {
            nonce: u64::MAX,
            subject: EntityId::from_bytes([0x33; 32]),
            challenge: [0x44; 32],
            signature: [0x55; 64],
        });
        round_trip(IdentityProofMsg::Verdict {
            nonce: 1,
            accepted: true,
            reject: None,
        });
        round_trip(IdentityProofMsg::Verdict {
            nonce: 2,
            accepted: false,
            reject: Some(IdentityProofReject::BadSignature),
        });
    }

    #[test]
    fn trailing_bytes_are_rejected() {
        let mut bytes = encode(&IdentityProofMsg::ChallengeRequest { nonce: 1 });
        bytes.push(0);
        assert!(matches!(
            decode(&bytes),
            Err(IdentityProofCodecError::Trailing(_))
        ));
    }

    #[test]
    fn truncated_proof_is_rejected() {
        let bytes = encode(&IdentityProofMsg::Proof {
            nonce: 1,
            subject: EntityId::from_bytes([0x33; 32]),
            challenge: [0x44; 32],
            signature: [0x55; 64],
        });
        assert!(matches!(
            decode(&bytes[..bytes.len() - 1]),
            Err(IdentityProofCodecError::Truncated(_))
        ));
    }

    #[test]
    fn verdict_rejects_non_boolean_accepted_byte() {
        let mut bytes = encode(&IdentityProofMsg::Verdict {
            nonce: 1,
            accepted: true,
            reject: None,
        });
        bytes[9] = 2;
        assert!(matches!(
            decode(&bytes),
            Err(IdentityProofCodecError::UnknownType(2))
        ));
    }

    /// The honest path: the subject's own key over the verifier's own
    /// view of the session verifies.
    #[test]
    fn genuine_proof_verifies() {
        let verifier = EntityKeypair::generate();
        let subject = EntityKeypair::generate();
        let challenge = [0x9Au8; 32];
        let transcript = proof_transcript(
            verifier.entity_id(),
            subject.entity_id(),
            subject.node_id(),
            0x1234,
            &challenge,
        );
        let sig = subject.sign(&transcript).to_bytes();
        verify_proof(
            verifier.entity_id(),
            subject.entity_id(),
            subject.node_id(),
            0x1234,
            &challenge,
            &sig,
        )
        .expect("genuine proof must verify");
    }

    /// Presenting somebody else's entity fails before any signature
    /// work: the claimed entity must derive the session's routing id.
    #[test]
    fn a_proof_for_another_entity_is_refused() {
        let verifier = EntityKeypair::generate();
        let subject = EntityKeypair::generate();
        let bystander = EntityKeypair::generate();
        let challenge = [0x01u8; 32];
        // The prover legitimately holds `subject`'s key and signs a
        // transcript naming `bystander` — but arrives on subject's
        // session.
        let transcript = proof_transcript(
            verifier.entity_id(),
            bystander.entity_id(),
            subject.node_id(),
            1,
            &challenge,
        );
        let sig = subject.sign(&transcript).to_bytes();
        assert_eq!(
            verify_proof(
                verifier.entity_id(),
                bystander.entity_id(),
                subject.node_id(),
                1,
                &challenge,
                &sig,
            )
            .unwrap_err(),
            IdentityProofReject::SubjectMismatch,
        );
    }

    /// Each binding component actually binds: perturbing any one of
    /// verifier / session / challenge invalidates the signature.
    #[test]
    fn transcript_binds_verifier_session_and_challenge() {
        let verifier = EntityKeypair::generate();
        let elsewhere = EntityKeypair::generate();
        let subject = EntityKeypair::generate();
        let challenge = [0x7Fu8; 32];
        let session = 0xABCDu64;
        let sig = subject
            .sign(&proof_transcript(
                verifier.entity_id(),
                subject.entity_id(),
                subject.node_id(),
                session,
                &challenge,
            ))
            .to_bytes();

        for (label, v, s, c) in [
            (
                "relayed to another verifier",
                elsewhere.entity_id(),
                session,
                challenge,
            ),
            (
                "replayed on a new session",
                verifier.entity_id(),
                session + 1,
                challenge,
            ),
            (
                "replayed with another nonce",
                verifier.entity_id(),
                session,
                [0x80u8; 32],
            ),
        ] {
            assert_eq!(
                verify_proof(v, subject.entity_id(), subject.node_id(), s, &c, &sig).unwrap_err(),
                IdentityProofReject::BadSignature,
                "{label} must not verify",
            );
        }
    }

    #[test]
    fn a_challenge_is_single_use_and_session_bound() {
        let store = IdentityChallengeStore::new();
        let t0 = Instant::now();
        let nonce = store.issue(9, 100, t0).expect("issue");
        assert!(
            !store.consume(9, 101, &nonce, t0),
            "a challenge must not be consumable on a different session incarnation"
        );
        assert!(store.consume(9, 100, &nonce, t0), "first use succeeds");
        assert!(
            !store.consume(9, 100, &nonce, t0),
            "a consumed challenge cannot be replayed"
        );
        assert!(store.is_empty(), "the peer slot is reclaimed when empty");
    }

    #[test]
    fn an_expired_challenge_is_absent() {
        let store = IdentityChallengeStore::new();
        let t0 = Instant::now();
        let nonce = store.issue(3, 1, t0).expect("issue");
        assert!(!store.consume(3, 1, &nonce, t0 + IDENTITY_CHALLENGE_TTL));
    }

    #[test]
    fn per_peer_challenges_are_bounded() {
        let store = IdentityChallengeStore::new();
        let t0 = Instant::now();
        let first = store.issue(5, 1, t0).expect("issue");
        for _ in 0..MAX_IDENTITY_CHALLENGES_PER_PEER {
            store.issue(5, 1, t0).expect("issue");
        }
        assert_eq!(store.len(), MAX_IDENTITY_CHALLENGES_PER_PEER);
        assert!(
            !store.consume(5, 1, &first, t0),
            "the oldest challenge is evicted once the per-peer bound is hit"
        );
    }

    #[test]
    fn forget_peer_drops_outstanding_challenges() {
        let store = IdentityChallengeStore::new();
        let t0 = Instant::now();
        let nonce = store.issue(11, 1, t0).expect("issue");
        store.forget_peer(11);
        assert!(!store.consume(11, 1, &nonce, t0));
    }
}
