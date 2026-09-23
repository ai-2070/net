// SPDX-License-Identifier: MIT OR Apache-2.0
//! PSK-free Noise enrollment session: wire protocol, responder key and client.
//!
//! A clean device holds no mesh PSK, so it cannot complete the mesh's
//! `Noise_NKpsk0` handshake. Redemption instead uses a separate TCP listener
//! running `Noise_NK_25519_ChaChaPoly_BLAKE2s` with its own prologue: the device
//! authenticates the responder by the [`EnrollmentKey`] signed into the invite,
//! and stays anonymous at the Noise layer. The protocol name and prologue keep
//! this handshake disjoint from mesh sessions.
//!
//! Exchange, each frame a big-endian `u16` length followed by that many bytes:
//!
//! 1. device → service: Noise `-> e, es` (empty payload);
//! 2. service → device: Noise `<- e, ee` (empty payload);
//! 3. device → service: encrypted request — the signed invite, the canonical
//!    [`RedemptionIntent`], and the subject's ed25519 signature over
//!    `domain ‖ handshake hash ‖ invite digest ‖ subject ‖ intent digest`;
//! 4. service → device: encrypted response, then close.
//!
//! The Noise handshake hash is fresh on both sides and unique to this session,
//! so it is the connection-bound challenge: a captured signature cannot be
//! replayed on another session. One request per session.
//!
//! Before calling [`redeem`], the device must have durably stored its identity
//! and intent, so a retry after a lost response presents the same request and
//! recovers the same committed issuance instead of competing with itself.

use std::time::Duration;

use snow::params::NoiseParams;
use snow::{Builder, HandshakeState, TransportState};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;

use super::invite::{EnrollmentKey, InviteError, MembershipInvite, RedemptionIntent};
use super::store::ReceiptId;
use super::Reader;
use crate::identity::{EntityId, Identity};

/// Noise protocol used by the enrollment listener. Not the mesh's `NKpsk0`.
pub const NOISE_PROTOCOL: &str = "Noise_NK_25519_ChaChaPoly_BLAKE2s";
/// Upper bound on a redemption request's plaintext.
pub const MAX_REQUEST_BYTES: usize = 4096;
/// Upper bound on a delivered bundle (fits one Noise transport message).
pub const MAX_BUNDLE_BYTES: usize = 32 * 1024;
/// Default bound on one whole client session.
pub const DEFAULT_REDEEM_TIMEOUT: Duration = Duration::from_secs(10);

const PROLOGUE: &[u8] = b"net-mesh enrollment session v1";
const RESPONDER_KEY_CONTEXT: &str = "net-mesh enrollment responder x25519 v1";
const TRANSCRIPT_DOMAIN: &[u8] = b"net-mesh enrollment redeem v1";
const REQUEST_MAGIC: [u8; 4] = *b"NMRQ";
const RESPONSE_MAGIC: [u8; 4] = *b"NMRS";
const TAG_LEN: usize = 16;
/// Largest handshake frame either side accepts (NK messages are 48 bytes).
pub(crate) const MAX_HANDSHAKE_FRAME: usize = 128;
const RESPONSE_OVERHEAD: usize = 4 + 1 + 16 + 1 + 4;

const TAG_ISSUED: u8 = 0;
const TAG_PENDING: u8 = 1;
const TAG_REFUSED: u8 = 2;

/// The enrollment responder's X25519 static keypair.
///
/// Derived from the issuer's root seed with a dedicated KDF context, so it is
/// stable across restarts without extra storage and never equals the root key.
/// Rotating it requires a new derivation (future work) and invalidates unredeemed
/// links. `Debug` redacts the secret.
#[derive(Clone)]
pub struct ResponderKey {
    secret: [u8; 32],
    public: EnrollmentKey,
}

impl core::fmt::Debug for ResponderKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ResponderKey")
            .field("public", &self.public)
            .field("secret", &"<redacted>")
            .finish()
    }
}

impl ResponderKey {
    /// Derive the responder key for `issuer`.
    pub fn derive(issuer: &Identity) -> Self {
        let mut hasher = blake3::Hasher::new_derive_key(RESPONDER_KEY_CONTEXT);
        hasher.update(&issuer.to_bytes());
        let secret = *hasher.finalize().as_bytes();
        let public = x25519_public(&secret);
        Self { secret, public }
    }

    /// Public key to sign into invitations.
    pub fn public(&self) -> EnrollmentKey {
        self.public
    }

    pub(crate) fn responder(&self) -> Result<HandshakeState, snow::Error> {
        Builder::new(params())
            .prologue(PROLOGUE)?
            .local_private_key(&self.secret)?
            .build_responder()
    }
}

fn params() -> NoiseParams {
    #[allow(
        clippy::expect_used,
        reason = "NOISE_PROTOCOL is a compile-time constant snow parses infallibly"
    )]
    NOISE_PROTOCOL.parse().expect("valid Noise protocol name")
}

fn x25519_public(secret: &[u8; 32]) -> EnrollmentKey {
    use snow::params::DHChoice;
    use snow::resolvers::{CryptoResolver, DefaultResolver};
    #[allow(
        clippy::expect_used,
        reason = "the default resolver always provides Curve25519"
    )]
    let mut dh = DefaultResolver
        .resolve_dh(&DHChoice::Curve25519)
        .expect("snow default resolver supports Curve25519");
    dh.set(secret);
    let mut public = [0u8; 32];
    public.copy_from_slice(dh.pubkey());
    EnrollmentKey(public)
}

fn initiator(key: &EnrollmentKey) -> Result<HandshakeState, snow::Error> {
    Builder::new(params())
        .prologue(PROLOGUE)?
        .remote_public_key(&key.0)?
        .build_initiator()
}

/// Why the service refused a request. Deliberately coarse: an invalid invite,
/// unknown invitation, mismatched intent and bad proof are all `Invalid`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Refusal {
    /// Invite, intent or proof did not verify, or the invitation is unknown here.
    #[error("invitation or proof is invalid")]
    Invalid,
    /// The first-issuance window has closed (or has not opened).
    #[error("invitation has expired")]
    Expired,
    /// Another device or intent already holds this invitation.
    #[error("invitation is bound to another claim")]
    Conflict,
    /// The invitation, or the device's standing, has been revoked.
    #[error("invitation or device has been revoked")]
    Revoked,
    /// The operator denied this claim.
    #[error("claim was denied")]
    Denied,
    /// The committed issuance can no longer be recovered.
    #[error("receipt recovery window is closed")]
    RecoveryClosed,
    /// The service cannot decide now (storage uncertainty, capacity, issuer error).
    #[error("enrollment service unavailable")]
    Unavailable,
}

impl Refusal {
    fn code(self) -> u8 {
        match self {
            Self::Invalid => 1,
            Self::Expired => 2,
            Self::Conflict => 3,
            Self::Revoked => 4,
            Self::Denied => 5,
            Self::RecoveryClosed => 6,
            Self::Unavailable => 7,
        }
    }

    fn from_code(code: u8) -> Option<Self> {
        Some(match code {
            1 => Self::Invalid,
            2 => Self::Expired,
            3 => Self::Conflict,
            4 => Self::Revoked,
            5 => Self::Denied,
            6 => Self::RecoveryClosed,
            7 => Self::Unavailable,
            _ => return None,
        })
    }
}

/// Successful service answer. `Debug` redacts the bundle.
#[derive(Clone, PartialEq, Eq)]
pub enum RedeemOutcome {
    /// The committed issuance for this device.
    Issued {
        /// Non-secret identifier of the committed issuance.
        receipt_id: ReceiptId,
        /// `true` when this is recovery of an earlier committed issuance.
        recovered: bool,
        /// Secret-bearing bundle bytes, exactly as committed.
        bundle: Vec<u8>,
    },
    /// Claim recorded; an operator decision is required. Retry later.
    PendingApproval,
}

impl core::fmt::Debug for RedeemOutcome {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Issued {
                receipt_id,
                recovered,
                bundle,
            } => f
                .debug_struct("Issued")
                .field("receipt_id", receipt_id)
                .field("recovered", recovered)
                .field("bundle_len", &bundle.len())
                .finish(),
            Self::PendingApproval => f.write_str("PendingApproval"),
        }
    }
}

/// Client-side redemption failure. No variant carries secret bytes.
#[derive(Debug, thiserror::Error)]
pub enum RedeemError {
    /// Intent does not belong to this device or invite; nothing was sent.
    #[error("redemption intent is not valid for this device and invite: {0}")]
    Intent(InviteError),
    /// TCP connection or I/O failure.
    #[error("enrollment connection failed: {0}")]
    Io(#[from] std::io::Error),
    /// The responder did not prove the invite's enrollment key.
    #[error("enrollment responder failed authentication")]
    Handshake,
    /// Malformed or oversized service message.
    #[error("enrollment protocol violation: {0}")]
    Protocol(&'static str),
    /// The session exceeded its deadline.
    #[error("enrollment session timed out")]
    Timeout,
    /// The service refused the request.
    #[error("enrollment refused: {0}")]
    Refused(Refusal),
}

/// Redeem `invite` for `device` over the PSK-free Noise enrollment session.
///
/// Verifies locally first that `intent` is this device's intent for this invite;
/// then connects to the invite's endpoint, requires the responder to prove the
/// signed enrollment key, and sends one signed request. Returns the committed
/// issuance (first or recovered) or a pending-approval answer. Does not install
/// or interpret the bundle.
pub async fn redeem(
    invite: &MembershipInvite,
    device: &Identity,
    intent: &RedemptionIntent,
    timeout: Duration,
) -> Result<RedeemOutcome, RedeemError> {
    if intent.subject() != device.entity_id() {
        return Err(RedeemError::Intent(InviteError::WrongSubject));
    }
    intent.check_against(invite).map_err(RedeemError::Intent)?;
    let session = async {
        let stream = TcpStream::connect(invite.endpoint().as_str()).await?;
        redeem_session(stream, invite, device, intent).await
    };
    tokio::time::timeout(timeout, session)
        .await
        .map_err(|_| RedeemError::Timeout)?
}

/// [`redeem`] over a byte stream the caller already opened towards the
/// service, such as a splice through a blind relay. The stream is not
/// trusted: the responder must still prove the invite's pinned enrollment key,
/// so a relay (or anyone who claimed the splice) cannot answer for it.
pub async fn redeem_over<S>(
    stream: S,
    invite: &MembershipInvite,
    device: &Identity,
    intent: &RedemptionIntent,
    timeout: Duration,
) -> Result<RedeemOutcome, RedeemError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    if intent.subject() != device.entity_id() {
        return Err(RedeemError::Intent(InviteError::WrongSubject));
    }
    intent.check_against(invite).map_err(RedeemError::Intent)?;
    tokio::time::timeout(timeout, redeem_session(stream, invite, device, intent))
        .await
        .map_err(|_| RedeemError::Timeout)?
}

async fn redeem_session<S>(
    mut stream: S,
    invite: &MembershipInvite,
    device: &Identity,
    intent: &RedemptionIntent,
) -> Result<RedeemOutcome, RedeemError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut hs = initiator(&invite.enrollment_key()).map_err(|_| RedeemError::Handshake)?;
    let mut buf = vec![0u8; MAX_HANDSHAKE_FRAME];
    let n = hs
        .write_message(&[], &mut buf)
        .map_err(|_| RedeemError::Handshake)?;
    write_frame(&mut stream, &buf[..n]).await?;
    let msg2 = read_frame(&mut stream, MAX_HANDSHAKE_FRAME)
        .await?
        .ok_or(RedeemError::Handshake)?;
    let mut payload = vec![0u8; MAX_HANDSHAKE_FRAME];
    // A responder without the pinned static key cannot produce a valid msg2.
    let n = hs
        .read_message(&msg2, &mut payload)
        .map_err(|_| RedeemError::Handshake)?;
    if n != 0 {
        return Err(RedeemError::Protocol("unexpected handshake payload"));
    }
    let hh = handshake_hash(&hs)?;
    let mut transport = hs
        .into_transport_mode()
        .map_err(|_| RedeemError::Handshake)?;

    let intent_bytes = intent.to_bytes();
    let message = transcript(&hh, &invite.digest(), device.entity_id(), &intent.digest());
    let request = encode_request(invite.to_bytes(), &intent_bytes, &device.sign(&message));
    let mut sealed = vec![0u8; request.len() + TAG_LEN];
    let n = transport
        .write_message(&request, &mut sealed)
        .map_err(|_| RedeemError::Protocol("request too large"))?;
    write_frame(&mut stream, &sealed[..n]).await?;

    let reply = read_frame(&mut stream, MAX_BUNDLE_BYTES + RESPONSE_OVERHEAD + TAG_LEN)
        .await?
        .ok_or(RedeemError::Protocol("service closed without a response"))?;
    let mut plain = vec![0u8; reply.len()];
    let n = transport
        .read_message(&reply, &mut plain)
        .map_err(|_| RedeemError::Protocol("response failed authentication"))?;
    decode_response(&plain[..n])?.map_err(RedeemError::Refused)
}

fn handshake_hash(hs: &HandshakeState) -> Result<[u8; 32], RedeemError> {
    <[u8; 32]>::try_from(hs.get_handshake_hash()).map_err(|_| RedeemError::Handshake)
}

pub(crate) fn session_hash(hs: &HandshakeState) -> Option<[u8; 32]> {
    <[u8; 32]>::try_from(hs.get_handshake_hash()).ok()
}

/// The message the device signs: binds this session, invite, subject and intent.
pub(crate) fn transcript(
    handshake_hash: &[u8; 32],
    invite_digest: &[u8; 32],
    subject: &EntityId,
    intent_digest: &[u8; 32],
) -> Vec<u8> {
    let mut m = Vec::with_capacity(TRANSCRIPT_DOMAIN.len() + 128);
    m.extend_from_slice(TRANSCRIPT_DOMAIN);
    m.extend_from_slice(handshake_hash);
    m.extend_from_slice(invite_digest);
    m.extend_from_slice(subject.as_bytes());
    m.extend_from_slice(intent_digest);
    m
}

/// Decoded (not yet verified) request.
pub(crate) struct Request {
    pub(crate) invite: Vec<u8>,
    pub(crate) intent: Vec<u8>,
    pub(crate) signature: [u8; 64],
}

pub(crate) fn encode_request(invite: &[u8], intent: &[u8], signature: &[u8; 64]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + 4 + invite.len() + intent.len() + 64);
    out.extend_from_slice(&REQUEST_MAGIC);
    // Both are bounded well below u16::MAX by their own codecs.
    out.extend_from_slice(&(invite.len() as u16).to_le_bytes());
    out.extend_from_slice(invite);
    out.extend_from_slice(&(intent.len() as u16).to_le_bytes());
    out.extend_from_slice(intent);
    out.extend_from_slice(signature);
    out
}

pub(crate) fn decode_request(bytes: &[u8]) -> Option<Request> {
    if bytes.len() > MAX_REQUEST_BYTES {
        return None;
    }
    let mut r = Reader::new(bytes);
    if r.take_arr::<4>()? != REQUEST_MAGIC {
        return None;
    }
    let n = r.take_u16()? as usize;
    let invite = r.take(n)?.to_vec();
    let n = r.take_u16()? as usize;
    let intent = r.take(n)?.to_vec();
    let signature = r.take_arr::<64>()?;
    r.done().then_some(Request {
        invite,
        intent,
        signature,
    })
}

pub(crate) fn encode_response(result: &Result<RedeemOutcome, Refusal>) -> Vec<u8> {
    let mut out = Vec::from(RESPONSE_MAGIC);
    match result {
        Ok(RedeemOutcome::Issued {
            receipt_id,
            recovered,
            bundle,
        }) => {
            out.push(TAG_ISSUED);
            out.extend_from_slice(receipt_id.as_bytes());
            out.push(u8::from(*recovered));
            // Bounded by MAX_BUNDLE_BYTES at the service.
            out.extend_from_slice(&(bundle.len() as u32).to_le_bytes());
            out.extend_from_slice(bundle);
        }
        Ok(RedeemOutcome::PendingApproval) => out.push(TAG_PENDING),
        Err(refusal) => {
            out.push(TAG_REFUSED);
            out.push(refusal.code());
        }
    }
    out
}

fn decode_response(bytes: &[u8]) -> Result<Result<RedeemOutcome, Refusal>, RedeemError> {
    let bad = RedeemError::Protocol("malformed response");
    let mut r = Reader::new(bytes);
    if r.take_arr::<4>() != Some(RESPONSE_MAGIC) {
        return Err(bad);
    }
    let result = match r
        .take_arr::<1>()
        .ok_or(RedeemError::Protocol("truncated"))?[0]
    {
        TAG_ISSUED => {
            let receipt_id = ReceiptId::from_bytes(r.take_arr::<16>().ok_or(bad)?);
            let recovered = match r.take_arr::<1>().map(|b| b[0]) {
                Some(0) => false,
                Some(1) => true,
                _ => return Err(RedeemError::Protocol("malformed response")),
            };
            let len = r.take_u32().ok_or(RedeemError::Protocol("truncated"))? as usize;
            if len == 0 || len > MAX_BUNDLE_BYTES {
                return Err(RedeemError::Protocol("invalid bundle length"));
            }
            let bundle = r
                .take(len)
                .ok_or(RedeemError::Protocol("truncated"))?
                .to_vec();
            Ok(RedeemOutcome::Issued {
                receipt_id,
                recovered,
                bundle,
            })
        }
        TAG_PENDING => Ok(RedeemOutcome::PendingApproval),
        TAG_REFUSED => Err(r
            .take_arr::<1>()
            .and_then(|c| Refusal::from_code(c[0]))
            .ok_or(RedeemError::Protocol("unknown refusal"))?),
        _ => return Err(RedeemError::Protocol("unknown response")),
    };
    if !r.done() {
        return Err(RedeemError::Protocol("trailing bytes"));
    }
    Ok(result)
}

/// Seal and send one transport message.
pub(crate) async fn send_sealed<S: AsyncWrite + Unpin>(
    stream: &mut S,
    transport: &mut TransportState,
    plain: &[u8],
) -> std::io::Result<()> {
    let mut sealed = vec![0u8; plain.len() + TAG_LEN];
    let n = transport
        .write_message(plain, &mut sealed)
        .map_err(|_| std::io::Error::other("noise seal failed"))?;
    write_frame(stream, &sealed[..n]).await
}

pub(crate) async fn write_frame<S: AsyncWrite + Unpin>(
    stream: &mut S,
    bytes: &[u8],
) -> std::io::Result<()> {
    let len = u16::try_from(bytes.len())
        .map_err(|_| std::io::Error::other("frame exceeds u16 length"))?;
    stream.write_all(&len.to_be_bytes()).await?;
    stream.write_all(bytes).await?;
    stream.flush().await
}

/// Read one frame of at most `max` bytes. `Ok(None)` on clean EOF before a
/// frame; an over-limit length is an error before any body allocation.
pub(crate) async fn read_frame<S: AsyncRead + Unpin>(
    stream: &mut S,
    max: usize,
) -> std::io::Result<Option<Vec<u8>>> {
    let mut len = [0u8; 2];
    match stream.read_exact(&mut len).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u16::from_be_bytes(len) as usize;
    if len == 0 || len > max {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "enrollment frame length out of bounds",
        ));
    }
    let mut body = vec![0u8; len];
    stream.read_exact(&mut body).await?;
    Ok(Some(body))
}
