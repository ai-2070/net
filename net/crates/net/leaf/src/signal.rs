//! The `0x0D02` signalling envelope on the wire, plus its replay
//! guard.
//!
//! [`SignalEnvelope`] declares
//! the signed body; this module is the byte form and the two checks
//! a receiver owes it:
//!
//! 1. **The signature**, against the Ed25519 key the receiver
//!    already holds for `from` — which it holds because §5 Layer 1
//!    (key discovery through a signed announcement) precedes
//!    signalling in both the native and the serverless world. An
//!    envelope from an entity this leaf has no announcement for is
//!    refused: there is nothing to verify against, and accepting it
//!    would make the carrier trusted.
//! 2. **The window and the seen-set.** `not_after` bounds replay to
//!    an interval, and [`SeenSignals`] refuses a second
//!    `(from, dialog, kind, payload digest)` inside it — only a
//!    byte-identical re-send is a replay. Without the seen-set an
//!    observer could re-offer a still-valid envelope and restart a
//!    dialog the sender had abandoned.
//!
//! Both checks together are what let a carrier — an anchor, a room
//! object, an in-memory mock — forward these blind: it learns
//! nothing and can tamper with nothing that verifies.

use std::collections::HashMap;

use crate::control_plane::{DialogId, NodeId, SignalEnvelope, SignalKind};
use crate::error::{LeafError, Result};
use crate::identity::{verify_entity_signature, LeafIdentity};

/// Subprotocol id for RTC signalling.
pub const SUBPROTOCOL_RTC_SIGNAL: u16 = 0x0D02;

/// The domain-separated prefix of the signed body.
const SIGNAL_MAGIC: &[u8] = b"net.signal.v1\0";

/// Offset of the fixed fields after the magic.
const FIXED_LEN: usize = 8 + 8 + 8 + 1 + 8 + 4;

/// Ed25519 signature length.
const SIG_LEN: usize = 64;

/// How far in the future a `not_after` may sit.
///
/// An envelope valid for a week would make the seen-set unbounded in
/// practice. Thirty seconds is generous for a signalling exchange
/// (S0b measured trickle at 22 ms) and bounds the seen-set to one
/// burst of dialogs.
pub const MAX_SIGNAL_LIFETIME_SECS: u64 = 30;

/// Encode an envelope: the signed body followed by its signature.
///
/// The body is exactly
/// [`SignalEnvelope::signing_bytes`],
/// so the verifier never re-derives a transcript — it takes the
/// prefix it was handed. A field the encoder and the signer disagreed
/// about is impossible by construction.
pub fn encode(envelope: &SignalEnvelope) -> Result<Vec<u8>> {
    if envelope.signature.len() != SIG_LEN {
        return Err(LeafError::ControlPlane(format!(
            "a signal envelope's signature must be {SIG_LEN} bytes, got {}",
            envelope.signature.len()
        )));
    }
    let mut out = envelope.signing_bytes();
    out.extend_from_slice(&envelope.signature);
    Ok(out)
}

/// Decode an envelope. Structure only — [`verify`] does the
/// cryptography.
pub fn decode(bytes: &[u8]) -> Result<SignalEnvelope> {
    let min = SIGNAL_MAGIC.len() + FIXED_LEN + SIG_LEN;
    if bytes.len() < min {
        return Err(LeafError::ControlPlane(format!(
            "signal envelope is {} bytes, under the {min}-byte minimum",
            bytes.len()
        )));
    }
    if &bytes[..SIGNAL_MAGIC.len()] != SIGNAL_MAGIC {
        return Err(LeafError::ControlPlane(
            "signal envelope does not carry the net.signal.v1 domain prefix".into(),
        ));
    }
    let mut at = SIGNAL_MAGIC.len();
    let from = read_u64(bytes, &mut at)?;
    let to = read_u64(bytes, &mut at)?;
    let dialog = read_u64(bytes, &mut at)?;
    let kind = kind_from_tag(bytes[at])?;
    at += 1;
    let not_after = read_u64(bytes, &mut at)?;
    let payload_len = read_u32(bytes, &mut at)? as usize;

    let body_end = at + payload_len;
    if bytes.len() != body_end + SIG_LEN {
        return Err(LeafError::ControlPlane(format!(
            "signal envelope declares a {payload_len}-byte payload but carries \
             {} trailing bytes instead of {SIG_LEN}",
            bytes.len().saturating_sub(at)
        )));
    }
    Ok(SignalEnvelope {
        from,
        to,
        dialog,
        kind,
        payload: bytes[at..body_end].to_vec(),
        not_after,
        signature: bytes[body_end..].to_vec(),
    })
}

/// Sign an envelope body with this leaf's entity key.
pub fn sign(
    identity: &LeafIdentity,
    to: NodeId,
    dialog: DialogId,
    kind: SignalKind,
    payload: Vec<u8>,
    not_after: u64,
) -> SignalEnvelope {
    let mut envelope = SignalEnvelope {
        from: identity.node_id(),
        to,
        dialog,
        kind,
        payload,
        not_after,
        signature: Vec::new(),
    };
    envelope.signature = identity.entity().sign(&envelope.signing_bytes()).to_vec();
    envelope
}

/// Verify an envelope's signature against `entity_id`, and that it
/// is addressed to `self_node` and still inside its window.
///
/// `now_unix_secs` is passed rather than read so one sweep over many
/// envelopes takes one clock read, and so the window logic is
/// testable without sleeping.
pub fn verify(
    envelope: &SignalEnvelope,
    entity_id: &[u8; 32],
    self_node: NodeId,
    now_unix_secs: u64,
) -> Result<()> {
    if envelope.to != self_node {
        return Err(LeafError::ControlPlane(format!(
            "signal envelope is addressed to {:#x}, not to this leaf",
            envelope.to
        )));
    }
    if envelope.not_after < now_unix_secs {
        return Err(LeafError::ControlPlane(
            "signal envelope is past its not_after".into(),
        ));
    }
    if envelope.not_after > now_unix_secs + MAX_SIGNAL_LIFETIME_SECS {
        return Err(LeafError::ControlPlane(format!(
            "signal envelope claims a lifetime beyond the \
             {MAX_SIGNAL_LIFETIME_SECS}s ceiling"
        )));
    }
    let signature: [u8; SIG_LEN] = envelope
        .signature
        .as_slice()
        .try_into()
        .map_err(|_| LeafError::ControlPlane("signature is not 64 bytes".into()))?;
    verify_entity_signature(entity_id, &envelope.signing_bytes(), &signature)
        .map_err(|_| LeafError::ControlPlane("signal signature did not verify".into()))
}

/// A 32-byte digest of an envelope's payload, so two candidates in
/// one dialog are two facts rather than one repeated fact.
fn payload_digest(payload: &[u8]) -> [u8; 32] {
    use blake2::digest::{consts::U32, Digest};
    let mut hasher = blake2::Blake2s::<U32>::new();
    hasher.update(payload);
    let mut out = [0u8; 32];
    out.copy_from_slice(&hasher.finalize());
    out
}

/// The hard cap on remembered envelopes (R8).
///
/// The set used to be bounded only by what fits inside the lifetime
/// ceiling, which is a bound on nothing an attacker respects: a peer
/// that signs envelopes faster than they expire grows it without
/// limit. At the cap **new** admissions are refused; an unexpired
/// entry is never evicted, because an evicted entry is a replay
/// that will be accepted, and trading replay protection for
/// cardinality would hand any carrier able to deliver valid traffic
/// the ability to replay an envelope it never signed.
pub const MAX_REMEMBERED_SIGNALS: usize = 4096;

/// What the replay set did with an envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalAdmission {
    /// First sight inside its window. The caller may act on it.
    Fresh,
    /// Byte-identical to an envelope already admitted and not yet
    /// past its `not_after`.
    Replay,
    /// The set already holds [`MAX_REMEMBERED_SIGNALS`] *unexpired*
    /// admissions, so this envelope is refused. Nothing is wrong
    /// with it: this is the fail-closed half of a bounded window,
    /// and it is a distinct outcome precisely so a caller can count
    /// and report pressure instead of reporting a replay that did
    /// not happen.
    AtCapacity,
}

/// The exact-replay set for the signalling window.
///
/// **Keyed by `(from, dialog, kind, payload digest)`** (R8). The
/// old `(from, dialog, kind)` key made the SECOND legitimate ICE
/// candidate of a dialog a replay of the first — every dialog was
/// limited to one candidate per kind, which is not a property
/// anybody wanted and which silently degraded connectivity to
/// whatever the first candidate could reach. Only a byte-identical
/// re-send is a replay now.
#[derive(Debug, Default)]
pub struct SeenSignals {
    /// Key → the `not_after` it was admitted under, so a prune needs
    /// no second timestamp.
    seen: HashMap<(NodeId, DialogId, u8, [u8; 32]), u64>,
}

impl SeenSignals {
    /// An empty seen-set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Admit an envelope once. `false` means it was not admitted —
    /// a replay, or refused under capacity pressure.
    ///
    /// [`Self::admit_checked`] distinguishes the two; this is the
    /// predicate for callers that only need to know whether they may
    /// act on the envelope.
    pub fn admit(&mut self, envelope: &SignalEnvelope, now_unix_secs: u64) -> bool {
        self.admit_checked(envelope, now_unix_secs) == SignalAdmission::Fresh
    }

    /// Admit an envelope once, naming the outcome.
    ///
    /// Prunes expired entries on the way through, so the set's size
    /// is bounded by the dialogs actually live inside
    /// [`MAX_SIGNAL_LIFETIME_SECS`] rather than by uptime — and, at
    /// the cap, by refusing new work rather than by forgetting an
    /// envelope that is still inside its own validity window.
    pub fn admit_checked(
        &mut self,
        envelope: &SignalEnvelope,
        now_unix_secs: u64,
    ) -> SignalAdmission {
        self.seen.retain(|_, not_after| *not_after >= now_unix_secs);
        let key = (
            envelope.from,
            envelope.dialog,
            envelope.kind.tag(),
            payload_digest(&envelope.payload),
        );
        if self.seen.contains_key(&key) {
            return SignalAdmission::Replay;
        }
        // Every entry left after the prune is unexpired, so every
        // one of them is protection this leaf is still relying on.
        // There is nothing here it is safe to drop, and refusing is
        // the only disposition that does not weaken the guarantee.
        if self.seen.len() >= MAX_REMEMBERED_SIGNALS {
            return SignalAdmission::AtCapacity;
        }
        self.seen.insert(key, envelope.not_after);
        SignalAdmission::Fresh
    }

    /// How many envelopes are remembered.
    pub fn len(&self) -> usize {
        self.seen.len()
    }

    /// Whether nothing is remembered.
    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }
}

fn kind_from_tag(tag: u8) -> Result<SignalKind> {
    match tag {
        1 => Ok(SignalKind::Offer),
        2 => Ok(SignalKind::Answer),
        3 => Ok(SignalKind::Candidate),
        4 => Ok(SignalKind::Reject),
        other => Err(LeafError::ControlPlane(format!(
            "unknown signal kind {other}"
        ))),
    }
}

fn read_u64(bytes: &[u8], at: &mut usize) -> Result<u64> {
    let slice = bytes
        .get(*at..*at + 8)
        .ok_or_else(|| LeafError::ControlPlane("signal envelope truncated".into()))?;
    *at += 8;
    Ok(u64::from_le_bytes(slice.try_into().unwrap_or([0; 8])))
}

fn read_u32(bytes: &[u8], at: &mut usize) -> Result<u32> {
    let slice = bytes
        .get(*at..*at + 4)
        .ok_or_else(|| LeafError::ControlPlane("signal envelope truncated".into()))?;
    *at += 4;
    Ok(u32::from_le_bytes(slice.try_into().unwrap_or([0; 4])))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::EntityKeypair;

    const PEER: NodeId = 0xDEAD_BEEF_0000_0001;

    fn identity() -> LeafIdentity {
        LeafIdentity::from_secrets(EntityKeypair::from_secret([0x31; 32]), [0x32; 32])
    }

    fn envelope(id: &LeafIdentity, now: u64) -> SignalEnvelope {
        sign(
            id,
            PEER,
            0x1234,
            SignalKind::Offer,
            b"v=0\r\no=- 1 1 IN IP4 0.0.0.0\r\n".to_vec(),
            now + 10,
        )
    }

    #[test]
    fn an_envelope_round_trips_through_the_wire_form_and_verifies() {
        let id = identity();
        let env = envelope(&id, 1_000);
        let bytes = encode(&env).expect("encode");
        let decoded = decode(&bytes).expect("decode");
        assert_eq!(decoded, env, "the wire form must be lossless");
        verify(&decoded, id.entity().entity_id(), PEER, 1_000).expect("verifies");
    }

    /// The carrier must not be able to change anything that
    /// verifies — the property the whole design rests on.
    #[test]
    fn a_carrier_cannot_alter_any_signed_field() {
        let id = identity();
        let env = envelope(&id, 1_000);
        let bytes = encode(&env).expect("encode");

        // Every byte of the signed body is covered. Walk a sample
        // across each field rather than all 100+ bytes.
        let body_len = bytes.len() - SIG_LEN;
        for at in [14, 18, 22, 30, 38, 39, 47, body_len - 1] {
            let mut tampered = bytes.clone();
            tampered[at] ^= 0x01;
            // A flip in a length or kind field can make the
            // envelope structurally invalid, which is also a
            // refusal; the point is that nothing verifies.
            if let Ok(decoded) = decode(&tampered) {
                assert!(
                    verify(&decoded, id.entity().entity_id(), decoded.to, 1_000).is_err(),
                    "a flipped bit at offset {at} still verified"
                );
            }
        }
    }

    #[test]
    fn an_envelope_for_another_node_is_refused() {
        let id = identity();
        let env = envelope(&id, 1_000);
        let err = verify(&env, id.entity().entity_id(), PEER + 1, 1_000).expect_err("must refuse");
        assert!(format!("{err}").contains("addressed to"), "{err}");
    }

    #[test]
    fn the_window_is_enforced_at_both_ends() {
        let id = identity();
        let env = envelope(&id, 1_000); // not_after = 1_010

        verify(&env, id.entity().entity_id(), PEER, 1_010).expect("valid at the boundary");
        assert!(
            verify(&env, id.entity().entity_id(), PEER, 1_011).is_err(),
            "one second past not_after must be refused"
        );

        // A sender claiming an absurd lifetime is refused too,
        // because the seen-set's bound depends on the ceiling.
        let long = sign(&id, PEER, 1, SignalKind::Offer, Vec::new(), 1_000_000);
        assert!(
            verify(&long, id.entity().entity_id(), PEER, 1_000).is_err(),
            "a lifetime beyond the ceiling must be refused"
        );
    }

    #[test]
    fn a_signature_from_another_entity_does_not_verify() {
        let id = identity();
        let other = LeafIdentity::from_secrets(EntityKeypair::from_secret([0x41; 32]), [0x42; 32]);
        let env = envelope(&id, 1_000);
        assert!(
            verify(&env, other.entity().entity_id(), PEER, 1_000).is_err(),
            "an envelope must only verify against the signer's own key"
        );
    }

    #[test]
    fn the_seen_set_admits_once_and_prunes_by_the_window() {
        let id = identity();
        let env = envelope(&id, 1_000);
        let mut seen = SeenSignals::new();

        assert!(seen.admit(&env, 1_000), "first admission");
        assert!(
            !seen.admit(&env, 1_000),
            "the same (from, dialog, kind, payload digest) must be refused inside the window"
        );

        // A different kind on the same dialog is a different message.
        let answer = sign(&id, PEER, 0x1234, SignalKind::Answer, Vec::new(), 1_010);
        assert!(seen.admit(&answer, 1_000));
        assert_eq!(seen.len(), 2);

        // Past the window, the entries are pruned and the envelope
        // is admissible again — its own `not_after` check is what
        // stops it being useful.
        assert!(seen.admit(&env, 1_011));
        assert_eq!(seen.len(), 1, "expired entries must not accumulate");
    }

    #[test]
    fn a_malformed_envelope_is_refused_rather_than_guessed() {
        assert!(decode(&[]).is_err(), "empty");
        assert!(decode(&[0u8; 200]).is_err(), "no domain prefix");

        let id = identity();
        let good = encode(&envelope(&id, 1_000)).expect("encode");
        assert!(
            decode(&good[..good.len() - 1]).is_err(),
            "a short signature must be refused"
        );

        let mut trailing = good.clone();
        trailing.push(0);
        assert!(
            decode(&trailing).is_err(),
            "trailing garbage must be refused, not ignored"
        );

        let mut bad_kind = good.clone();
        bad_kind[14 + 24] = 9;
        assert!(
            decode(&bad_kind).is_err(),
            "an unknown kind must be refused"
        );
    }
}
