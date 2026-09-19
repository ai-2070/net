//! The enrollment exchange — the nRPC client's **first caller**.
//!
//! Brief §1 names this and I missed it in the first pass, so here is
//! the fact that makes it load-bearing rather than optional:
//!
//! A session that completes the Noise handshake is **provisional**.
//! §12's admission contract then permits a provisional peer exactly
//! four things — subscribe to its own `net.mesh.enroll.replies.<origin>`
//! channel, call `net.mesh.enroll` on the anchor itself with a body
//! under 16 KiB, cancel that call, and heartbeat — and refuses
//! everything else. So a leaf that connects and stops has a working
//! transport and a node that cannot call a service, cannot publish,
//! and whose announcement the anchor will not flood. From the
//! caller's side the refusal arrives as nothing at all, and the call
//! ends on its deadline: "rpc: the call's deadline elapsed", with
//! the service's handler never having run. That is precisely what
//! `net/crates/net/tests/rtc_browser`'s Stage 5 witnesses measured.
//!
//! Enrollment is the exchange that promotes the session. It is a
//! **data-path** nRPC call over the session that was just installed
//! — not a control-plane operation — so it belongs after the
//! handshake and it does not belong behind
//! [`ControlPlane`](crate::control_plane::ControlPlane). An
//! anchorless control plane does not carry it.
//!
//! # Another named second copy
//!
//! `InviteToken`, `JoinRequest` and `JoinOutcome` live in
//! `net-mesh-sdk`, which depends on the core and cannot be linked
//! from wasm. The three codecs here are a deliberate mirror of the
//! canonical ones in `sdk/src/enrollment.rs`, and the cost is
//! contained the same way as the other two:
//!
//! - `tests/cross_lang_wire/enroll_exchange.json` pins the bytes,
//!   and `tests/enroll_parity.rs` asserts this module reproduces
//!   them from a deterministic identity and invite;
//! - the **live** cross-check is stronger than either: the browser
//!   matrix runs this request against the anchor's real
//!   `net.mesh.enroll` provider, which verifies the device's
//!   self-signature over a challenge it reconstructs itself. A
//!   single wrong byte in [`join_challenge`] or in the request's
//!   framing and the provider rejects, the session stays
//!   provisional, and four witnesses go red. There is no version of
//!   this being subtly wrong and passing.
//!
//! The one-directional-pin gap is named in the report: the core
//! crate cannot dev-depend on the SDK (the SDK depends on the core),
//! so the SDK-side half of the pin needs a test in `sdk/tests/`,
//! which is outside this slice's ownership.

use crate::error::{LeafError, Result};
use crate::identity::LeafIdentity;

/// Magic + version at the head of an [`Invite`]'s wire form.
const INVITE_MAGIC: [u8; 4] = *b"NMI1";

/// Magic + version at the head of a join request.
const JOIN_MAGIC: [u8; 4] = *b"NMJ1";

/// Magic + version at the head of a join outcome.
const OUTCOME_MAGIC: [u8; 4] = *b"NMO1";

/// Domain-separation prefix for the device's join self-signature.
///
/// Byte-identical to the SDK's `JOIN_CHALLENGE_DOMAIN`. A different
/// string here would produce a signature the authority reconstructs
/// differently and rejects.
const JOIN_CHALLENGE_DOMAIN: &[u8] = b"net-mesh enrollment join-request v1";

/// The one nRPC service a provisional session may call (§12 / S0e §2 B).
pub const ENROLL_SERVICE: &str = "net.mesh.enroll";

/// Ceiling on the enrollment request body. Over it, admission
/// refuses — so the leaf refuses first, with a typed error naming
/// the bound instead of a call that dies on its deadline.
pub const MAX_ENROLL_BODY_BYTES: usize = 16 * 1024;

/// Stable rejection codes, mirroring the SDK's `reject` module.
pub mod reject {
    /// The join-request bytes were malformed.
    pub const MALFORMED: u16 = 1;
    /// No outstanding invite matched.
    pub const UNKNOWN_INVITE: u16 = 2;
    /// The invite's TTL had elapsed.
    pub const EXPIRED: u16 = 3;
    /// A binding check failed — wrong nonce, wrong mesh, or a bad
    /// self-signature.
    pub const BAD_REQUEST: u16 = 4;
    /// The invite was already redeemed; it is single-use.
    pub const REPLAY: u16 = 5;
    /// The operator side hit an internal error.
    pub const INTERNAL: u16 = 6;
    /// The operator explicitly denied the request.
    pub const DENIED: u16 = 7;
    /// A renewal presented an unrenewable grant.
    pub const UNRENEWABLE: u16 = 8;

    /// A human-readable name for a code, for the typed error's
    /// message. An unknown code renders as its number rather than
    /// being swallowed.
    pub fn name(code: u16) -> &'static str {
        match code {
            MALFORMED => "malformed",
            UNKNOWN_INVITE => "unknown invite",
            EXPIRED => "expired",
            BAD_REQUEST => "bad request",
            REPLAY => "replay",
            INTERNAL => "internal",
            DENIED => "denied",
            UNRENEWABLE => "unrenewable",
            _ => "unknown",
        }
    }
}

/// The invite half of a bootstrap credential.
///
/// The leaf's first pass skipped this blob as opaque. It is not
/// opaque: the enrollment request has to echo `nonce` as
/// proof-of-invite and bind `root` into its signature, so a leaf
/// that cannot read it cannot enroll.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invite {
    /// The mesh root this invite admits to, bound into the join
    /// signature so a request captured for one mesh cannot be
    /// presented to another.
    pub root: [u8; 32],
    /// The single-use nonce, echoed as proof-of-invite.
    pub nonce: [u8; 16],
    /// Unix seconds after which the invite is dead. Distinct from
    /// the credential's standing PSK deadline.
    pub expires_at: u64,
    /// Where the device was told to present itself.
    pub rendezvous: String,
}

impl Invite {
    /// Parse the canonical wire form.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut at = 0usize;
        if take::<4>(bytes, &mut at)? != INVITE_MAGIC {
            return Err(malformed("invite: bad magic or version"));
        }
        let root = take::<32>(bytes, &mut at)?;
        let nonce = take::<16>(bytes, &mut at)?;
        let expires_at = u64::from_le_bytes(take::<8>(bytes, &mut at)?);
        let rendezvous_len = u32::from_le_bytes(take::<4>(bytes, &mut at)?) as usize;
        let end = at
            .checked_add(rendezvous_len)
            .filter(|end| *end <= bytes.len())
            .ok_or_else(|| malformed("invite: truncated rendezvous"))?;
        let rendezvous = core::str::from_utf8(&bytes[at..end])
            .map_err(|_| malformed("invite: non-UTF-8 rendezvous"))?
            .to_string();
        if end != bytes.len() {
            return Err(malformed("invite: trailing bytes"));
        }
        Ok(Self {
            root,
            nonce,
            expires_at,
            rendezvous,
        })
    }

    /// Refuse an invite whose single-use deadline has passed.
    ///
    /// Checked separately from parsing, so a caller inspecting a
    /// dead invite still sees its fields — the same split the SDK
    /// makes.
    pub fn validate_at(&self, now_unix_secs: u64) -> Result<()> {
        if now_unix_secs >= self.expires_at {
            return Err(LeafError::Identity(format!(
                "the credential's invite expired at {} (now {now_unix_secs}); \
                 the operator must mint a new one",
                self.expires_at
            )));
        }
        Ok(())
    }
}

/// The domain-separated, length-prefixed challenge the device signs
/// and the authority reconstructs.
///
/// Byte-identical to the SDK's `join_challenge`. The length prefixes
/// are what make the framing unambiguous — without them a name and a
/// tag could be confused across their boundary.
pub fn join_challenge(
    device: &[u8; 32],
    name: &str,
    tags: &[String],
    invite_nonce: &[u8; 16],
    root: &[u8; 32],
) -> Vec<u8> {
    let mut msg =
        Vec::with_capacity(JOIN_CHALLENGE_DOMAIN.len() + 4 + 32 + 4 + name.len() + 4 + 16 + 4 + 32);
    msg.extend_from_slice(JOIN_CHALLENGE_DOMAIN);
    push_lp(&mut msg, device);
    push_lp(&mut msg, name.as_bytes());
    msg.extend_from_slice(&(tags.len() as u32).to_le_bytes());
    for tag in tags {
        push_lp(&mut msg, tag.as_bytes());
    }
    msg.extend_from_slice(invite_nonce);
    push_lp(&mut msg, root);
    msg
}

/// Build and sign the enrollment request body.
///
/// The signature proves the device holds the key it presents and
/// binds the request to this mesh. `name` and `tags` are
/// device-chosen and non-authoritative — they label and route, they
/// never decide authority.
pub fn build_join_request(
    identity: &LeafIdentity,
    name: &str,
    tags: &[String],
    invite: &Invite,
) -> Result<Vec<u8>> {
    let device = identity.entity().entity_id();
    let challenge = join_challenge(device, name, tags, &invite.nonce, &invite.root);
    let signature = identity.entity().sign(&challenge);

    let mut buf = Vec::with_capacity(4 + 32 + 16 + 32 + 64 + 4 + name.len() + 4);
    buf.extend_from_slice(&JOIN_MAGIC);
    buf.extend_from_slice(device);
    buf.extend_from_slice(&invite.nonce);
    buf.extend_from_slice(&invite.root);
    buf.extend_from_slice(&signature);
    push_lp(&mut buf, name.as_bytes());
    buf.extend_from_slice(&(tags.len() as u32).to_le_bytes());
    for tag in tags {
        push_lp(&mut buf, tag.as_bytes());
    }

    if buf.len() > MAX_ENROLL_BODY_BYTES {
        // Admission would refuse this, and the refusal would reach
        // the caller as a deadline. Refuse here, naming the bound.
        return Err(LeafError::Identity(format!(
            "the enrollment request is {} bytes, over §12's \
             {MAX_ENROLL_BODY_BYTES}-byte bound; shorten the device name or \
             the tag list",
            buf.len()
        )));
    }
    Ok(buf)
}

/// The operator's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JoinOutcome {
    /// Admitted. Carries the serialized `root → device` delegation
    /// chain, which is what promotes the session out of
    /// provisional.
    Admitted {
        /// The chain bytes, carried verbatim.
        chain: Vec<u8>,
    },
    /// Rejected, with a stable code and a human message.
    Rejected {
        /// One of [`reject`]'s codes.
        code: u16,
        /// The operator's reason.
        message: String,
    },
}

impl JoinOutcome {
    /// Parse the canonical wire form.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut at = 0usize;
        if take::<4>(bytes, &mut at)? != OUTCOME_MAGIC {
            return Err(malformed("join outcome: bad magic or version"));
        }
        let tag = take::<1>(bytes, &mut at)?[0];
        let outcome = match tag {
            0 => Self::Admitted {
                chain: take_lp(bytes, &mut at)?.to_vec(),
            },
            1 => {
                let code = u16::from_le_bytes(take::<2>(bytes, &mut at)?);
                let message = core::str::from_utf8(take_lp(bytes, &mut at)?)
                    .map_err(|_| malformed("join outcome: non-UTF-8 message"))?
                    .to_string();
                Self::Rejected { code, message }
            }
            other => return Err(malformed(&format!("join outcome: unknown tag {other}"))),
        };
        if at != bytes.len() {
            return Err(malformed("join outcome: trailing bytes"));
        }
        Ok(outcome)
    }

    /// The chain on success; a typed error naming the code and the
    /// operator's message on refusal.
    ///
    /// A rejection is a *result*, not a transport failure, and it is
    /// never retried on the leaf's own initiative: the invite is
    /// single-use, so a silent retry would burn it and turn a
    /// legible `DENIED` into an illegible `REPLAY`.
    pub fn into_chain(self) -> Result<Vec<u8>> {
        match self {
            Self::Admitted { chain } => Ok(chain),
            Self::Rejected { code, message } => Err(LeafError::Identity(format!(
                "the anchor rejected enrollment: {} ({code}): {message}",
                reject::name(code)
            ))),
        }
    }
}

fn push_lp(buf: &mut Vec<u8>, bytes: &[u8]) {
    buf.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    buf.extend_from_slice(bytes);
}

fn take<const N: usize>(bytes: &[u8], at: &mut usize) -> Result<[u8; N]> {
    let end = at.checked_add(N).ok_or_else(|| malformed("truncated"))?;
    let slice = bytes.get(*at..end).ok_or_else(|| malformed("truncated"))?;
    let mut out = [0u8; N];
    out.copy_from_slice(slice);
    *at = end;
    Ok(out)
}

fn take_lp<'a>(bytes: &'a [u8], at: &mut usize) -> Result<&'a [u8]> {
    let len = u32::from_le_bytes(take::<4>(bytes, at)?) as usize;
    let end = at
        .checked_add(len)
        .filter(|end| *end <= bytes.len())
        .ok_or_else(|| malformed("truncated length-prefixed field"))?;
    let slice = &bytes[*at..end];
    *at = end;
    Ok(slice)
}

fn malformed(what: &str) -> LeafError {
    LeafError::Identity(what.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::{verify_entity_signature, EntityKeypair};

    fn identity() -> LeafIdentity {
        LeafIdentity::from_secrets(EntityKeypair::from_secret([0x21; 32]), [0x22; 32])
    }

    fn invite() -> Invite {
        Invite {
            root: [0x77; 32],
            nonce: [0x5A; 16],
            expires_at: 2_000_000_000,
            rendezvous: "https://anchor.example/rtc".into(),
        }
    }

    fn invite_bytes(rendezvous: &str, expires_at: u64) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(&INVITE_MAGIC);
        buf.extend_from_slice(&[0x77u8; 32]);
        buf.extend_from_slice(&[0x5Au8; 16]);
        buf.extend_from_slice(&expires_at.to_le_bytes());
        push_lp(&mut buf, rendezvous.as_bytes());
        buf
    }

    #[test]
    fn an_invite_decodes_to_the_root_the_nonce_and_the_deadline() {
        let decoded = Invite::decode(&invite_bytes("https://anchor.example/rtc", 2_000_000_000))
            .expect("decodes");
        assert_eq!(decoded, invite());
    }

    #[test]
    fn a_malformed_invite_is_refused_rather_than_guessed() {
        assert!(Invite::decode(&[]).is_err(), "empty");
        let good = invite_bytes("x", 1);
        assert!(
            Invite::decode(&good[..good.len() - 1]).is_err(),
            "a truncated rendezvous must be refused"
        );
        let mut trailing = good.clone();
        trailing.push(0);
        assert!(
            Invite::decode(&trailing).is_err(),
            "trailing bytes must be refused, not ignored"
        );
        let mut bad_magic = good.clone();
        bad_magic[3] = b'9';
        assert!(Invite::decode(&bad_magic).is_err(), "bad version");
    }

    #[test]
    fn an_expired_invite_is_refused_at_validation_not_at_parse() {
        let invite = Invite::decode(&invite_bytes("x", 1_000)).expect("parses");
        invite.validate_at(999).expect("valid just before");
        let err = invite
            .validate_at(1_000)
            .expect_err("the deadline is exclusive — `now >= expires_at` is dead");
        assert!(format!("{err}").contains("expired"), "{err}");
    }

    /// The request's self-signature must verify against the exact
    /// challenge the authority reconstructs. If the domain string,
    /// the field order or a length prefix were wrong, this passes
    /// locally and the anchor rejects — so the test rebuilds the
    /// challenge from the *decoded* request rather than reusing the
    /// builder's own value.
    #[test]
    fn the_join_request_self_signature_verifies_against_a_reconstructed_challenge() {
        let identity = identity();
        let invite = invite();
        let tags = vec!["browser".to_string(), "leaf".to_string()];
        let body = build_join_request(&identity, "chrome-tab", &tags, &invite).expect("build");

        // Walk the wire form field by field, as the authority does.
        let mut at = 0usize;
        assert_eq!(take::<4>(&body, &mut at).expect("magic"), JOIN_MAGIC);
        let device = take::<32>(&body, &mut at).expect("device");
        assert_eq!(&device, identity.entity().entity_id());
        assert_eq!(take::<16>(&body, &mut at).expect("nonce"), invite.nonce);
        assert_eq!(take::<32>(&body, &mut at).expect("root"), invite.root);
        let signature = take::<64>(&body, &mut at).expect("signature");
        let name = core::str::from_utf8(take_lp(&body, &mut at).expect("name")).expect("utf8");
        assert_eq!(name, "chrome-tab");
        let tag_count = u32::from_le_bytes(take::<4>(&body, &mut at).expect("count")) as usize;
        assert_eq!(tag_count, 2);
        let mut decoded_tags = Vec::new();
        for _ in 0..tag_count {
            decoded_tags.push(
                core::str::from_utf8(take_lp(&body, &mut at).expect("tag"))
                    .expect("utf8")
                    .to_string(),
            );
        }
        assert_eq!(decoded_tags, tags);
        assert_eq!(at, body.len(), "no trailing bytes");

        let challenge = join_challenge(&device, name, &decoded_tags, &invite.nonce, &invite.root);
        verify_entity_signature(&device, &challenge, &signature)
            .expect("the device's self-signature must verify");

        // And the binding: the same device, name and tags against a
        // DIFFERENT mesh root must not verify. This is what stops a
        // captured request being replayed at another mesh.
        let other_root = [0x11u8; 32];
        let other = join_challenge(&device, name, &decoded_tags, &invite.nonce, &other_root);
        assert!(
            verify_entity_signature(&device, &other, &signature).is_err(),
            "the signature must bind the mesh root"
        );
        let other_nonce = [0u8; 16];
        let replayed = join_challenge(&device, name, &decoded_tags, &other_nonce, &invite.root);
        assert!(
            verify_entity_signature(&device, &replayed, &signature).is_err(),
            "the signature must bind the invite nonce"
        );
    }

    #[test]
    fn an_over_cap_request_is_refused_before_admission_can_swallow_it() {
        let identity = identity();
        let invite = invite();
        let huge = "t".repeat(MAX_ENROLL_BODY_BYTES);
        let err =
            build_join_request(&identity, &huge, &[], &invite).expect_err("over the §12 bound");
        let text = format!("{err}");
        assert!(text.contains("16384"), "the bound must be named: {text}");
        assert!(
            text.contains("shorten"),
            "the error must say what to do: {text}"
        );
    }

    #[test]
    fn an_admitted_outcome_yields_the_chain() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&OUTCOME_MAGIC);
        bytes.push(0);
        push_lp(&mut bytes, b"chain-bytes");
        let outcome = JoinOutcome::decode(&bytes).expect("decodes");
        assert_eq!(
            outcome,
            JoinOutcome::Admitted {
                chain: b"chain-bytes".to_vec()
            }
        );
        assert_eq!(
            outcome.into_chain().expect("admitted"),
            b"chain-bytes".to_vec()
        );
    }

    /// A rejection is a typed result that names the code, not a
    /// timeout and not a retry.
    #[test]
    fn a_rejection_names_its_code_and_is_never_retried() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&OUTCOME_MAGIC);
        bytes.push(1);
        bytes.extend_from_slice(&reject::REPLAY.to_le_bytes());
        push_lp(&mut bytes, b"that invite was already redeemed");

        let outcome = JoinOutcome::decode(&bytes).expect("decodes");
        assert_eq!(
            outcome,
            JoinOutcome::Rejected {
                code: reject::REPLAY,
                message: "that invite was already redeemed".into()
            }
        );
        let err = outcome.into_chain().expect_err("a rejection is an error");
        let text = format!("{err}");
        assert!(text.contains("replay"), "{text}");
        assert!(text.contains('5'), "the stable code must survive: {text}");
        assert!(text.contains("already redeemed"), "{text}");
    }

    #[test]
    fn a_malformed_outcome_is_refused() {
        assert!(JoinOutcome::decode(&[]).is_err());
        assert!(JoinOutcome::decode(b"NMO1").is_err(), "no tag");
        let mut unknown_tag = Vec::from(OUTCOME_MAGIC);
        unknown_tag.push(9);
        assert!(
            JoinOutcome::decode(&unknown_tag).is_err(),
            "an unknown tag must be refused, not treated as a rejection"
        );
        let mut trailing = Vec::from(OUTCOME_MAGIC);
        trailing.push(0);
        push_lp(&mut trailing, b"chain");
        trailing.push(0xFF);
        assert!(JoinOutcome::decode(&trailing).is_err(), "trailing bytes");
    }

    /// Every reject code has a name, so a refusal can never render
    /// as a bare number the operator has to look up.
    #[test]
    fn every_reject_code_has_a_name() {
        for code in [
            reject::MALFORMED,
            reject::UNKNOWN_INVITE,
            reject::EXPIRED,
            reject::BAD_REQUEST,
            reject::REPLAY,
            reject::INTERNAL,
            reject::DENIED,
            reject::UNRENEWABLE,
        ] {
            assert_ne!(reject::name(code), "unknown", "code {code} has no name");
        }
        assert_eq!(reject::name(9999), "unknown");
    }
}
