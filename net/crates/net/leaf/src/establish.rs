//! The establishment proof: **which individual node** the initiator
//! is.
//!
//! # What NKpsk0 proves, and what it does not
//!
//! `Noise_NKpsk0` authenticates two things: possession of the trust
//! domain's pre-shared key, and — for the initiator — that the
//! responder holds the static key the initiator pinned. It
//! authenticates the initiator as *nobody*. `N` is the pattern's
//! first letter and it means the initiator has no static key in the
//! handshake at all.
//!
//! [`crate::session::PendingHandshake`] binds the initiator's
//! **claimed** node id into the prologue, and that binding is real
//! but narrow: it stops a relay rewriting an honest handshake's id,
//! because the two ends would then disagree on the prologue and the
//! MAC on message 1 would fail. It says nothing about whether the
//! creator of the handshake owns the id it wrote there. Any holder of
//! the domain PSK can put any node id in a prologue. That is the
//! whole of the reviewer's reproduction: an independent initiator
//! built with the domain PSK and the responder's *public* static key,
//! claiming a discovered peer's id, was classified as that peer,
//! admitted, installed, and handed application bytes addressed to it.
//!
//! # What this module adds
//!
//! One signed statement, from the initiator's **entity** key — the
//! Ed25519 key the initiator's capability announcement is signed
//! with, whose public half the responder has already verified and
//! whose derivation to a node id the announcement verifier already
//! enforced (`announce::verify_announcement`'s R1 check). The
//! statement says:
//!
//! > under label [`PROOF_LABEL`] version [`PROOF_VERSION`], as the
//! > **initiator**, for the ordered pair (initiator id, responder
//! > id), over the Noise handshake whose final transcript hash is
//! > `h`.
//!
//! Every clause is load-bearing:
//!
//! - **The label and version** are domain separation. This signature
//!   can never be mistaken for an announcement signature, a `0x0D02`
//!   envelope signature, or a future version of itself.
//! - **Both ids, in role order, plus the signer's role byte.** A
//!   proof made as the initiator of (A, B) is not a proof made as
//!   the responder of (A, B), and it is not a proof for (B, A). The
//!   ordering alone would leave the two roles' statements identical
//!   for a symmetric pair, so the role is also stated explicitly.
//! - **The final handshake hash.** This is what makes the proof
//!   non-transferable to any *other* establishment. It is not the
//!   peer id, not the announcement, not the signalling dialog — all
//!   of which are stable across attempts and would let one captured
//!   proof authorise every future handshake. `h` exists exactly once
//!   per handshake and is known to both endpoints and to nobody else
//!   (an eavesdropper on the DataChannel sees the handshake messages,
//!   but the PSK is mixed into `h`, so the domain is the smallest set
//!   that can compute it — and inside the domain the signature is
//!   still the individual proof).
//!
//! # Where it rides
//!
//! Its own subprotocol, [`SUBPROTOCOL_ESTABLISHMENT_PROOF`], on the
//! freshly established session — *not* inside a Noise handshake
//! payload. Message 1 is written before `h` exists and message 2 is
//! the responder's, so no Noise payload can carry a signature over
//! the final transcript. Riding the session instead also means the
//! frame is sealed under the very keys `h` produced, so a proof
//! frame cannot be lifted off one establishment and replayed into
//! another even before its signature is checked.
//!
//! The consequence is the responder's **provisional** window, which
//! is [`crate::node::LeafNode`]'s: between message 1 and a verified
//! proof the responder holds keys and nothing else — no session in
//! the table, no `Connected` event, no `direct` attribution, and no
//! delivery of anything but the proof frame itself.

use crate::control_plane::NodeId;
use crate::error::{LeafError, Result};
use crate::identity::{node_id_for_entity, verify_entity_signature, EntityKeypair};

/// Subprotocol id the establishment proof rides.
///
/// Free in the mesh's map: `0x0D00`/`0x0D01` are the negotiation
/// plane's, `0x0D02` is RTC signalling. Deliberately **not** a
/// [`crate::dispatch::Subprotocol`] arm — see the note there.
pub const SUBPROTOCOL_ESTABLISHMENT_PROOF: u16 = 0x0D03;

/// The domain-separation label, carrying its own version digit.
///
/// 31 bytes, fixed. Every other field of the transcript is
/// fixed-width too, so the concatenation is unambiguous without
/// length prefixes: there is exactly one way to parse
/// [`PROOF_TRANSCRIPT_LEN`] bytes into these fields.
pub const PROOF_LABEL: &[u8; 31] = b"net-leaf-establishment-proof-v1";

/// The protocol version this build signs and accepts.
///
/// Stated inside the transcript as well as on the wire, so a
/// signature made under version 1 rules cannot be presented as a
/// version 2 statement even if a future frame layout would parse it.
pub const PROOF_VERSION: u8 = 1;

/// How long a responder holds an unproven establishment before
/// retiring it.
///
/// Five seconds, the same bound `wasm::RELAY_SESSION_DEADLINE_MS`
/// gives a whole relayed establishment — deliberately not longer,
/// because this window is one leg of that establishment: the
/// initiator signs the proof the instant it reads message 2 and
/// sends it on the connection it already has. A bound this generous
/// is already an order of magnitude past the round trip; it exists
/// to retire the attempt, not to wait out a slow peer.
pub const PROOF_DEADLINE_MS: u64 = 5_000;

/// Which end of the establishment signed a proof.
///
/// A byte in the transcript and on the wire, so the two roles'
/// statements are distinct documents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProofRole {
    /// The end that wrote Noise message 1 — the end NKpsk0 leaves
    /// anonymous, and therefore the end that owes a proof.
    Initiator,
    /// The end that answered with its static key. Already
    /// authenticated by the pattern itself; the role exists so that
    /// an initiator's proof can never be presented as this one.
    Responder,
}

impl ProofRole {
    /// The wire byte. Never zero, so a zeroed frame is not a role.
    #[inline]
    pub const fn to_wire(self) -> u8 {
        match self {
            Self::Initiator => 1,
            Self::Responder => 2,
        }
    }

    /// Classify a wire byte.
    #[inline]
    pub const fn from_wire(byte: u8) -> Option<Self> {
        match byte {
            1 => Some(Self::Initiator),
            2 => Some(Self::Responder),
            _ => None,
        }
    }
}

/// Bytes of the signed transcript: label ‖ version ‖ role ‖
/// initiator id ‖ responder id ‖ handshake hash.
pub const PROOF_TRANSCRIPT_LEN: usize = 31 + 1 + 1 + 8 + 8 + 32;

/// Bytes of the wire frame: version ‖ role ‖ initiator id ‖
/// responder id ‖ signature.
///
/// The handshake hash is **not** on the wire. It is not a value a
/// receiver may be told: each end recomputes its own, and a proof
/// signed over a different establishment simply fails to verify.
pub const PROOF_FRAME_LEN: usize = 1 + 1 + 8 + 8 + 64;

/// The exact bytes an establishment proof signs.
///
/// Fixed length, fixed field order, no allocation.
#[must_use]
pub fn proof_transcript(
    signer: ProofRole,
    initiator: NodeId,
    responder: NodeId,
    handshake_hash: &[u8; 32],
) -> [u8; PROOF_TRANSCRIPT_LEN] {
    let mut out = [0u8; PROOF_TRANSCRIPT_LEN];
    out[..31].copy_from_slice(PROOF_LABEL);
    out[31] = PROOF_VERSION;
    out[32] = signer.to_wire();
    out[33..41].copy_from_slice(&initiator.to_le_bytes());
    out[41..49].copy_from_slice(&responder.to_le_bytes());
    out[49..81].copy_from_slice(handshake_hash);
    out
}

/// One endpoint's signed claim to the identity it used in a
/// handshake.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EstablishmentProof {
    /// Which role the signer claims to have played.
    pub signer: ProofRole,
    /// The initiator's node id, as the signer states it.
    pub initiator: NodeId,
    /// The responder's node id, as the signer states it.
    pub responder: NodeId,
    /// Ed25519 signature over [`proof_transcript`].
    pub signature: [u8; 64],
}

impl EstablishmentProof {
    /// Sign the establishment `entity` just completed.
    pub fn sign(
        entity: &EntityKeypair,
        signer: ProofRole,
        initiator: NodeId,
        responder: NodeId,
        handshake_hash: &[u8; 32],
    ) -> Self {
        let transcript = proof_transcript(signer, initiator, responder, handshake_hash);
        Self {
            signer,
            initiator,
            responder,
            signature: entity.sign(&transcript),
        }
    }

    /// The wire frame.
    #[must_use]
    pub fn encode(&self) -> [u8; PROOF_FRAME_LEN] {
        let mut out = [0u8; PROOF_FRAME_LEN];
        out[0] = PROOF_VERSION;
        out[1] = self.signer.to_wire();
        out[2..10].copy_from_slice(&self.initiator.to_le_bytes());
        out[10..18].copy_from_slice(&self.responder.to_le_bytes());
        out[18..82].copy_from_slice(&self.signature);
        out
    }

    /// Parse a wire frame.
    ///
    /// Exact length, known version, known role. A frame that is
    /// merely *long enough* is refused: this is a fixed-width
    /// document, and accepting trailing bytes would make two
    /// encodings of one statement.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != PROOF_FRAME_LEN {
            return Err(LeafError::Wire(format!(
                "an establishment proof is exactly {PROOF_FRAME_LEN} bytes, got {}",
                bytes.len()
            )));
        }
        if bytes[0] != PROOF_VERSION {
            return Err(LeafError::Wire(format!(
                "establishment proof version {} is not {PROOF_VERSION}",
                bytes[0]
            )));
        }
        let signer = ProofRole::from_wire(bytes[1])
            .ok_or_else(|| LeafError::Wire(format!("{} is not an establishment role", bytes[1])))?;
        #[expect(
            clippy::unwrap_used,
            reason = "length checked above; the slices are 8 and 64 bytes by construction"
        )]
        Ok(Self {
            signer,
            initiator: NodeId::from_le_bytes(bytes[2..10].try_into().unwrap()),
            responder: NodeId::from_le_bytes(bytes[10..18].try_into().unwrap()),
            signature: bytes[18..82].try_into().unwrap(),
        })
    }

    /// Verify this proof against what the **verifier** knows.
    ///
    /// `entity_id` is the signer's Ed25519 public key as the verifier
    /// resolved it — from a signed announcement it already verified,
    /// never from this frame. `initiator`, `responder` and
    /// `handshake_hash` are the verifier's own view of the
    /// establishment, never the frame's.
    ///
    /// **The frame's own ids are checked for equality and then not
    /// used.** They exist so that a mismatch is a named refusal
    /// rather than an opaque signature failure; the transcript is
    /// rebuilt from the verifier's values, so a frame that lied about
    /// either id could not verify even if this check were removed.
    ///
    /// The entity key is re-bound to the claimed node id here as
    /// well. The announcement verifier already enforces
    /// `node_id_for_entity(entity_id) == node_id`, and repeating it
    /// costs one BLAKE2s and makes this function safe for any caller
    /// rather than only for that one.
    pub fn verify(
        &self,
        entity_id: &[u8; 32],
        signer: ProofRole,
        initiator: NodeId,
        responder: NodeId,
        handshake_hash: &[u8; 32],
    ) -> Result<()> {
        if self.signer != signer {
            return Err(LeafError::Session(format!(
                "establishment proof signed as {:?}, expected {signer:?}",
                self.signer
            )));
        }
        if self.initiator != initiator || self.responder != responder {
            return Err(LeafError::Session(format!(
                "establishment proof names the pair ({:#018x}, {:#018x}), this establishment \
                 is ({initiator:#018x}, {responder:#018x})",
                self.initiator, self.responder
            )));
        }
        let claimed = match signer {
            ProofRole::Initiator => initiator,
            ProofRole::Responder => responder,
        };
        let derived = node_id_for_entity(entity_id);
        if derived != claimed {
            return Err(LeafError::Session(format!(
                "the signing entity derives to {derived:#018x}, not to the {signer:?} \
                 {claimed:#018x} it is presented for"
            )));
        }
        let transcript = proof_transcript(signer, initiator, responder, handshake_hash);
        verify_entity_signature(entity_id, &transcript, &self.signature).map_err(|_| {
            LeafError::Session(format!(
                "establishment proof for {claimed:#018x} did not verify against its \
                 announced entity key over this handshake's transcript"
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::LeafIdentity;

    fn hash(seed: u8) -> [u8; 32] {
        [seed; 32]
    }

    /// The honest round trip, through the wire form.
    #[test]
    fn a_signed_proof_verifies_for_the_establishment_it_names() {
        let id = LeafIdentity::generate().unwrap();
        let (initiator, responder) = (id.node_id(), 0x1234_5678_9abc_def0);
        let h = hash(7);
        let proof =
            EstablishmentProof::sign(id.entity(), ProofRole::Initiator, initiator, responder, &h);
        let decoded = EstablishmentProof::decode(&proof.encode()).unwrap();
        assert_eq!(decoded, proof, "the frame must round-trip exactly");
        decoded
            .verify(
                id.entity().entity_id(),
                ProofRole::Initiator,
                initiator,
                responder,
                &h,
            )
            .expect("the honest proof must verify");
    }

    /// The transcript binds the handshake, so the same signature is
    /// worthless for any other establishment.
    #[test]
    fn a_proof_does_not_verify_for_a_different_handshake() {
        let id = LeafIdentity::generate().unwrap();
        let (initiator, responder) = (id.node_id(), 9);
        let proof = EstablishmentProof::sign(
            id.entity(),
            ProofRole::Initiator,
            initiator,
            responder,
            &hash(1),
        );
        let err = proof
            .verify(
                id.entity().entity_id(),
                ProofRole::Initiator,
                initiator,
                responder,
                &hash(2),
            )
            .expect_err("a proof for another transcript must not verify");
        assert!(format!("{err}").contains("did not verify"), "{err}");
    }

    /// An initiator's statement is not a responder's, and the roles
    /// are refused before the signature is even consulted.
    #[test]
    fn an_initiator_proof_is_not_a_responder_proof() {
        let id = LeafIdentity::generate().unwrap();
        let (a, b) = (id.node_id(), 11);
        let h = hash(3);
        let proof = EstablishmentProof::sign(id.entity(), ProofRole::Initiator, a, b, &h);
        assert!(proof
            .verify(id.entity().entity_id(), ProofRole::Responder, a, b, &h)
            .is_err());

        // And the transcripts themselves differ, so even a signature
        // stripped of the role byte could not be reused.
        assert_ne!(
            proof_transcript(ProofRole::Initiator, a, b, &h),
            proof_transcript(ProofRole::Responder, a, b, &h)
        );
        assert_ne!(
            proof_transcript(ProofRole::Initiator, a, b, &h),
            proof_transcript(ProofRole::Initiator, b, a, &h)
        );
    }

    /// The signing key must derive to the id it signs for: a proof
    /// signed by an impostor's own entity key for a victim's id is
    /// refused even when the verifier is handed the impostor's key.
    #[test]
    fn a_proof_signed_for_someone_elses_id_is_refused() {
        let impostor = LeafIdentity::generate().unwrap();
        let victim = LeafIdentity::generate().unwrap();
        let h = hash(4);
        let proof = EstablishmentProof::sign(
            impostor.entity(),
            ProofRole::Initiator,
            victim.node_id(),
            5,
            &h,
        );
        let err = proof
            .verify(
                impostor.entity().entity_id(),
                ProofRole::Initiator,
                victim.node_id(),
                5,
                &h,
            )
            .expect_err("a key that does not derive to the claimed id proves nothing");
        assert!(format!("{err}").contains("derives to"), "{err}");
    }

    /// Fixed width, exactly. A longer frame is not a proof with
    /// trailing junk.
    #[test]
    fn the_frame_is_fixed_width_and_versioned() {
        let id = LeafIdentity::generate().unwrap();
        let encoded =
            EstablishmentProof::sign(id.entity(), ProofRole::Initiator, id.node_id(), 1, &hash(5))
                .encode();
        assert_eq!(encoded.len(), PROOF_FRAME_LEN);

        let mut longer = encoded.to_vec();
        longer.push(0);
        assert!(EstablishmentProof::decode(&longer).is_err());
        assert!(EstablishmentProof::decode(&encoded[..PROOF_FRAME_LEN - 1]).is_err());

        let mut wrong_version = encoded;
        wrong_version[0] = PROOF_VERSION + 1;
        assert!(EstablishmentProof::decode(&wrong_version).is_err());

        let mut wrong_role = encoded;
        wrong_role[1] = 0;
        assert!(EstablishmentProof::decode(&wrong_role).is_err());
    }

    /// The label is the domain separation, and it is the first thing
    /// in the transcript.
    #[test]
    fn the_transcript_is_domain_separated_and_fixed_layout() {
        let t = proof_transcript(
            ProofRole::Initiator,
            0x0102_0304_0506_0708,
            0x1122,
            &hash(6),
        );
        assert_eq!(t.len(), PROOF_TRANSCRIPT_LEN);
        assert_eq!(&t[..31], PROOF_LABEL.as_slice());
        assert_eq!(t[31], PROOF_VERSION);
        assert_eq!(t[32], ProofRole::Initiator.to_wire());
        assert_eq!(&t[33..41], &0x0102_0304_0506_0708u64.to_le_bytes());
        assert_eq!(&t[41..49], &0x1122u64.to_le_bytes());
        assert_eq!(&t[49..81], &hash(6));
    }
}
