//! Device enrollment — the invite → join → approve handshake that admits a
//! new device into an operator's mesh (Hermes V2 Phase 1).
//!
//! # The shape
//!
//! Enrollment turns "a machine I control" into "a device holding a revocable
//! delegation from my root", in one handshake:
//!
//! 1. **Invite** — the operator mints an [`InviteToken`] (`mesh.invite`): a
//!    *rendezvous address* the device dials, the mesh *root* it's joining, a
//!    single-use *nonce*, and a short *TTL*. The token is **not a key** — a
//!    leaked invite only lets someone *ask* to join, visibly and for minutes,
//!    never admits them. QR is just the token string encoded.
//! 2. **Join** — the device generates *its own* keypair locally (**keys never
//!    travel**), and sends a [`JoinRequest`]: its public [`EntityId`], a
//!    device-chosen name + tags, the invite nonce (proof-of-invite), and a
//!    signature over all of it proving it holds the key.
//! 3. **Approve** — the operator's [`EnrollmentAuthority`] checks the invite is
//!    live, unspent (single-use), and names this mesh; verifies the device's
//!    self-signature; then signs a `root → device` [`DelegationChain`]
//!    ([`DelegationChain::derive_device`]) back to the device. The signature
//!    *is* enrollment — channels/QR/LAN only signal the *request*.
//!
//! # Why this deprecates the shared-identity-file pattern
//!
//! Phase 3 derived the machine / gateway identities from the **root seed**
//! (`derive_child_seed`), so every box that ran an agent effectively needed the
//! root. Enrollment inverts that: the root stays on one machine, and each
//! device holds a delegation to a key *it* generated. Revoking a device is
//! then bumping the **device's** floor in the shared [`RevocationRegistry`] /
//! `RevocationStore` — killing that device's gateway subtree without touching a
//! sibling, exactly as revoking a machine does in Phase 3. A mesh where every
//! node *is* the root has no revocation story; this one does.
//!
//! # Mutual mesh verification
//!
//! The token carries the **full** root [`EntityId`], not just a truncated
//! fingerprint: the device needs the real root pubkey anyway to anchor-verify
//! the delegation it gets back, and carrying it lets the returned chain be
//! cryptographically bound to the mesh the human was invited to (an evil-twin
//! invite for a *different* root produces a delegation that fails to anchor).
//! [`fingerprint`] renders that root as a short human-comparable string shown
//! on both sides during join, so the operator and the joiner can eyeball that
//! they're talking about the same mesh.
//!
//! # Scope (Slice A)
//!
//! This module is the in-process crypto / data-structure core: minting,
//! request signing + verification, single-use enforcement, and the
//! `root → device` grant. The rendezvous *transport*, the machine-shared
//! device registry behind `mesh.devices()`, the base64 invite *string* + QR,
//! the `net mesh …` CLI, and the language bindings layer on top (Slice B).

use std::collections::HashMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use parking_lot::Mutex;

use crate::delegation::DelegationChain;
use crate::identity::{EntityId, Identity, TokenError};

// Re-export the anchor type so `net_sdk::enrollment` is a complete surface.
pub use crate::delegation::RevocationRegistry;

/// 4-byte magic + version for the [`InviteToken`] wire form (Net-Mesh Invite
/// v1). Distinct from [`JOIN_MAGIC`] so a join request fed to
/// [`InviteToken::from_bytes`] is rejected rather than mis-parsed.
const INVITE_MAGIC: [u8; 4] = *b"NMI1";

/// 4-byte magic + version for the [`JoinRequest`] wire form (Net-Mesh Join v1).
const JOIN_MAGIC: [u8; 4] = *b"NMJ1";

/// 4-byte magic + version for the [`JoinOutcome`] wire form (Net-Mesh Outcome
/// v1).
const OUTCOME_MAGIC: [u8; 4] = *b"NMO1";

/// Domain-separation prefix for the device's self-signature, so a join-request
/// signature can never be confused with any other signature the device key
/// produces (tokens, announcements, delegation-invoke envelopes).
const JOIN_CHALLENGE_DOMAIN: &[u8] = b"net-mesh enrollment join-request v1";

/// Domain-separation prefix for the displayed root [`fingerprint`].
const FINGERPRINT_DOMAIN: &[u8] = b"net-mesh root-fingerprint v1";

/// Scheme-like prefix on the copy-paste / QR invite string, so it's
/// self-describing and unambiguous to a scanner or a human.
const INVITE_STRING_PREFIX: &str = "net-invite:";

/// Upper bound on tags accepted from a serialized [`JoinRequest`] — a framing
/// sanity cap so a malformed/hostile blob can't declare a huge tag count.
const MAX_TAGS: usize = 64;

/// Cap on the outstanding-invite ledger an [`EnrollmentAuthority`] tracks for
/// single-use enforcement. Generous for an operator minting a handful of
/// invites at a time; a saturated ledger fails **closed**
/// ([`EnrollmentError::LedgerSaturated`]) rather than forgetting a spent nonce.
const MAX_OUTSTANDING_INVITES: usize = 4096;

/// Errors from the enrollment handshake.
#[derive(Debug, thiserror::Error)]
pub enum EnrollmentError {
    /// A serialized [`InviteToken`] was truncated, mis-magicked, or had
    /// trailing bytes.
    #[error("malformed invite token: {0}")]
    MalformedInvite(&'static str),
    /// A serialized [`JoinRequest`] was truncated, mis-magicked, or had
    /// trailing bytes.
    #[error("malformed join request: {0}")]
    MalformedRequest(&'static str),
    /// The device's self-signature didn't verify — it doesn't hold the key it
    /// presented, so the request is rejected (never admitted on a bad proof).
    #[error("join request signature invalid: the device does not hold the presented key")]
    BadSignature,
    /// The invite's TTL has elapsed.
    #[error("invite has expired")]
    Expired,
    /// The request's nonce doesn't match the invite's — not a valid
    /// proof-of-invite.
    #[error("join request presents the wrong invite nonce")]
    NonceMismatch,
    /// The invite or the request names a different mesh root than this
    /// authority's — an evil-twin invite, or a request replayed to the wrong
    /// mesh.
    #[error("invite/request targets a different mesh root")]
    WrongMesh,
    /// The invite nonce was already spent — single-use, so a captured invite
    /// can't be redeemed twice.
    #[error("invite already used (single-use)")]
    Replay,
    /// The outstanding-invite ledger is full; retry once in-flight invites
    /// expire. Fail-closed so single-use is never silently dropped.
    #[error("invite ledger saturated; retry after outstanding invites expire")]
    LedgerSaturated,
    /// The `root → device` delegation could not be minted (e.g. an
    /// out-of-range grant TTL).
    #[error(transparent)]
    Token(#[from] TokenError),
}

/// A short, human-comparable fingerprint of an [`EntityId`], shown on both
/// sides of a join so a human can confirm the mesh identity matches
/// (evil-twin-invite defense). 16 hex chars (64 bits) in dash-separated
/// groups, e.g. `A1B2-C3D4-E5F6-0789`. Stable and collision-resistant enough
/// for eyeball comparison; the full [`EntityId`] rides in the token for the
/// cryptographic binding.
pub fn fingerprint(entity: &EntityId) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(FINGERPRINT_DOMAIN);
    hasher.update(entity.as_bytes());
    let digest = hasher.finalize();
    let b = &digest.as_bytes()[..8];
    let mut out = String::with_capacity(19);
    for (i, byte) in b.iter().enumerate() {
        if i > 0 && i % 2 == 0 {
            out.push('-');
        }
        out.push(nibble_hex(byte >> 4));
        out.push(nibble_hex(byte & 0x0f));
    }
    out
}

fn nibble_hex(n: u8) -> char {
    char::from_digit(n as u32, 16)
        .expect("nibble is 0..16")
        .to_ascii_uppercase()
}

/// Current unix-seconds, or 0 if the clock is before the epoch. Shared with the
/// operator facade so both read time the same way.
pub(crate) fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A pre-authorization to *ask* to join a mesh — **not a key**.
///
/// Carries the mesh [`InviteToken::root`] (anchor + [`fingerprint`] source), a
/// [`InviteToken::rendezvous`] address the device dials, a single-use
/// [`InviteToken::nonce`], and an [`InviteToken::expires_at`] deadline. A
/// leaked invite lets someone submit a [`JoinRequest`] for a few minutes,
/// visibly (the operator sees the request) and deniably (they still can't be
/// admitted without approval) — that's the whole blast radius.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InviteToken {
    /// The mesh root this invite admits into — the full ed25519 public key, so
    /// the joining device can anchor-verify the delegation it receives.
    pub root: EntityId,
    /// Where the device submits its [`JoinRequest`] (a socket address, relay
    /// locator, etc.). Opaque to the crypto core — the transport interprets it.
    pub rendezvous: String,
    /// Single-use pre-authorization nonce; echoed back in the [`JoinRequest`]
    /// as proof-of-invite and burned on first successful approval.
    pub nonce: [u8; 16],
    /// Unix-seconds expiry. Short by design (minutes) — the invite is a
    /// pre-auth to ask, not a standing credential.
    pub expires_at: u64,
}

impl InviteToken {
    /// Mint an invite for `root`, valid for `ttl` from now. The nonce is fresh
    /// CSPRNG bytes.
    ///
    /// A `getrandom` failure aborts the process (mirroring the core identity
    /// layer): a predictable invite nonce would undermine single-use, and a
    /// weak-nonce invite is worse than a crash.
    pub fn mint(root: &EntityId, rendezvous: impl Into<String>, ttl: Duration) -> Self {
        Self::mint_at(root, rendezvous, ttl, now_unix())
    }

    /// [`Self::mint`] with an explicit `now` (unix secs) — for deterministic
    /// tests and callers that already hold a clock reading.
    pub fn mint_at(
        root: &EntityId,
        rendezvous: impl Into<String>,
        ttl: Duration,
        now: u64,
    ) -> Self {
        let mut nonce = [0u8; 16];
        if let Err(e) = getrandom::fill(&mut nonce) {
            // Abort rather than unwind: a predictable single-use nonce is a
            // security failure, and these helpers are reachable from FFI.
            eprintln!(
                "FATAL: InviteToken::mint getrandom failure ({e:?}); aborting to avoid a predictable invite nonce"
            );
            std::process::abort();
        }
        Self {
            root: root.clone(),
            rendezvous: rendezvous.into(),
            nonce,
            expires_at: now.saturating_add(ttl.as_secs()),
        }
    }

    /// The displayed [`fingerprint`] of the mesh root — show this to the human
    /// joining so they can confirm they're joining the intended mesh.
    pub fn root_fingerprint(&self) -> String {
        fingerprint(&self.root)
    }

    /// Whether the invite has expired at `now` (unix secs).
    pub fn is_expired(&self, now: u64) -> bool {
        now >= self.expires_at
    }

    /// Canonical wire form (versioned, length-prefixed). The copy-paste / QR
    /// *string* wraps these bytes at the transport edge (Slice B).
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(4 + 32 + 16 + 8 + 4 + self.rendezvous.len());
        buf.extend_from_slice(&INVITE_MAGIC);
        buf.extend_from_slice(self.root.as_bytes());
        buf.extend_from_slice(&self.nonce);
        buf.extend_from_slice(&self.expires_at.to_le_bytes());
        push_lp(&mut buf, self.rendezvous.as_bytes());
        buf
    }

    /// Parse the canonical wire form. Rejects a bad magic/version, truncation,
    /// non-UTF-8 rendezvous, or trailing bytes.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, EnrollmentError> {
        let mut r = Reader::new(bytes);
        let magic = r
            .take_arr::<4>()
            .ok_or(EnrollmentError::MalformedInvite("truncated"))?;
        if magic != INVITE_MAGIC {
            return Err(EnrollmentError::MalformedInvite("bad magic or version"));
        }
        let root = EntityId::from_bytes(
            r.take_arr::<32>()
                .ok_or(EnrollmentError::MalformedInvite("truncated root"))?,
        );
        let nonce = r
            .take_arr::<16>()
            .ok_or(EnrollmentError::MalformedInvite("truncated nonce"))?;
        let expires_at = r
            .take_u64()
            .ok_or(EnrollmentError::MalformedInvite("truncated expiry"))?;
        let rendezvous = r
            .take_lp_string()
            .ok_or(EnrollmentError::MalformedInvite("bad rendezvous"))?;
        if !r.done() {
            return Err(EnrollmentError::MalformedInvite("trailing bytes"));
        }
        Ok(Self {
            root,
            rendezvous,
            nonce,
            expires_at,
        })
    }

    /// The copy-paste / QR invite string: a `net-invite:` prefix followed by
    /// URL-safe, unpadded base64 of the canonical [`Self::to_bytes`]. This is
    /// what `mesh.invite` hands the operator to share and `mesh.join` consumes;
    /// a QR code is just this string encoded.
    pub fn encode(&self) -> String {
        let mut s = String::from(INVITE_STRING_PREFIX);
        s.push_str(&URL_SAFE_NO_PAD.encode(self.to_bytes()));
        s
    }

    /// Parse an invite string produced by [`Self::encode`]. Tolerates
    /// surrounding whitespace (a trailing newline from copy-paste / a QR
    /// scanner); rejects a missing prefix, invalid base64, or malformed bytes.
    pub fn decode(s: &str) -> Result<Self, EnrollmentError> {
        let body =
            s.trim()
                .strip_prefix(INVITE_STRING_PREFIX)
                .ok_or(EnrollmentError::MalformedInvite(
                    "missing net-invite: prefix",
                ))?;
        let bytes = URL_SAFE_NO_PAD
            .decode(body)
            .map_err(|_| EnrollmentError::MalformedInvite("invalid base64"))?;
        Self::from_bytes(&bytes)
    }
}

/// A device's request to join, signed by the device's own key.
///
/// The device generated its keypair locally and only its public [`EntityId`]
/// travels. The [`JoinRequest::signature`] over the domain-separated challenge
/// (device id ∥ name ∥ tags ∥ invite nonce ∥ root) proves the device holds the
/// key and binds the request to *this* mesh — a request captured for one mesh
/// can't be presented to another.
#[derive(Clone, Debug)]
pub struct JoinRequest {
    /// The device's public key — the subject the root will delegate to.
    pub device: EntityId,
    /// Device-chosen name (in-flow: "name this device"), non-authoritative.
    pub name: String,
    /// Device-chosen routing/labeling tags, non-authoritative (tags route and
    /// scope; they never decide authority).
    pub tags: Vec<String>,
    /// The invite nonce, echoed as proof-of-invite.
    pub invite_nonce: [u8; 16],
    /// The mesh root the device intends to join — bound into the signature.
    pub root: EntityId,
    /// Ed25519 signature by `device` over the join challenge.
    pub signature: [u8; 64],
}

impl JoinRequest {
    /// Build and sign a request against `invite`. `device` must own its signing
    /// key (it just generated it).
    pub fn create(
        device: &Identity,
        name: impl Into<String>,
        tags: Vec<String>,
        invite: &InviteToken,
    ) -> Self {
        let name = name.into();
        let challenge = join_challenge(
            device.entity_id(),
            &name,
            &tags,
            &invite.nonce,
            &invite.root,
        );
        let signature = device.sign(&challenge);
        Self {
            device: device.entity_id().clone(),
            name,
            tags,
            invite_nonce: invite.nonce,
            root: invite.root.clone(),
            signature,
        }
    }

    /// Verify the device's self-signature — that it holds the key it presented.
    /// Reconstructs the exact challenge, so any tamper to name / tags / nonce /
    /// root invalidates it.
    pub fn verify_self_signature(&self) -> Result<(), EnrollmentError> {
        let challenge = join_challenge(
            &self.device,
            &self.name,
            &self.tags,
            &self.invite_nonce,
            &self.root,
        );
        self.device
            .verify_bytes(&challenge, &self.signature)
            .map_err(|_| EnrollmentError::BadSignature)
    }

    /// Canonical wire form (versioned, length-prefixed).
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(4 + 32 + 16 + 32 + 64 + 4 + self.name.len() + 4);
        buf.extend_from_slice(&JOIN_MAGIC);
        buf.extend_from_slice(self.device.as_bytes());
        buf.extend_from_slice(&self.invite_nonce);
        buf.extend_from_slice(self.root.as_bytes());
        buf.extend_from_slice(&self.signature);
        push_lp(&mut buf, self.name.as_bytes());
        buf.extend_from_slice(&(self.tags.len() as u32).to_le_bytes());
        for tag in &self.tags {
            push_lp(&mut buf, tag.as_bytes());
        }
        buf
    }

    /// Parse the canonical wire form. Rejects a bad magic/version, truncation,
    /// non-UTF-8 strings, an over-long tag count, or trailing bytes. Does **not**
    /// verify the signature — call [`Self::verify_self_signature`] for that.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, EnrollmentError> {
        let mut r = Reader::new(bytes);
        let magic = r
            .take_arr::<4>()
            .ok_or(EnrollmentError::MalformedRequest("truncated"))?;
        if magic != JOIN_MAGIC {
            return Err(EnrollmentError::MalformedRequest("bad magic or version"));
        }
        let device = EntityId::from_bytes(
            r.take_arr::<32>()
                .ok_or(EnrollmentError::MalformedRequest("truncated device"))?,
        );
        let invite_nonce = r
            .take_arr::<16>()
            .ok_or(EnrollmentError::MalformedRequest("truncated nonce"))?;
        let root = EntityId::from_bytes(
            r.take_arr::<32>()
                .ok_or(EnrollmentError::MalformedRequest("truncated root"))?,
        );
        let signature = r
            .take_arr::<64>()
            .ok_or(EnrollmentError::MalformedRequest("truncated signature"))?;
        let name = r
            .take_lp_string()
            .ok_or(EnrollmentError::MalformedRequest("bad name"))?;
        let tag_count = r
            .take_u32()
            .ok_or(EnrollmentError::MalformedRequest("truncated tag count"))?
            as usize;
        if tag_count > MAX_TAGS {
            return Err(EnrollmentError::MalformedRequest("too many tags"));
        }
        let mut tags = Vec::with_capacity(tag_count);
        for _ in 0..tag_count {
            tags.push(
                r.take_lp_string()
                    .ok_or(EnrollmentError::MalformedRequest("bad tag"))?,
            );
        }
        if !r.done() {
            return Err(EnrollmentError::MalformedRequest("trailing bytes"));
        }
        Ok(Self {
            device,
            name,
            tags,
            invite_nonce,
            root,
            signature,
        })
    }
}

/// The result of a successful [`EnrollmentAuthority::approve`]: the signed
/// `root → device` delegation to hand back to the device, plus the metadata to
/// record for `mesh.devices()`.
#[derive(Clone, Debug)]
pub struct Enrollment {
    /// The `root → device` chain the device holds and locally extends
    /// ([`DelegationChain::extend_delegate`]) to its gateway.
    pub chain: DelegationChain,
    /// The enrolled device's entity id.
    pub device: EntityId,
    /// The device's chosen name.
    pub name: String,
    /// The device's chosen tags.
    pub tags: Vec<String>,
}

/// [`JoinOutcome::Rejected`] codes — stable across versions so a device can
/// branch on *why* it was turned away (e.g. `EXPIRED` → fetch a fresh invite).
pub mod reject {
    /// The join-request bytes were malformed.
    pub const MALFORMED: u16 = 1;
    /// No outstanding invite matched (never minted here, already used, or
    /// expired and pruned).
    pub const UNKNOWN_INVITE: u16 = 2;
    /// The invite's TTL had elapsed.
    pub const EXPIRED: u16 = 3;
    /// A binding check failed (wrong nonce, wrong mesh, or bad self-signature).
    pub const BAD_REQUEST: u16 = 4;
    /// The invite was already redeemed (single-use).
    pub const REPLAY: u16 = 5;
    /// The operator side hit an internal error (store I/O, token minting).
    pub const INTERNAL: u16 = 6;
    /// The operator (a human, or a policy) explicitly denied the request —
    /// distinct from a failed check: the invite/signature were valid, the
    /// operator said no.
    pub const DENIED: u16 = 7;
}

/// The operator's response to a join request — the payload the enrollment RPC
/// returns to the device.
///
/// On [`Self::Admitted`] the device parses + verifies the carried chain
/// ([`Self::into_chain`]); on [`Self::Rejected`] it learns a stable
/// [`reject`] code and a human message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JoinOutcome {
    /// The device was admitted; carries the serialized `root → device`
    /// [`DelegationChain`].
    Admitted {
        /// The delegation chain bytes ([`DelegationChain::to_bytes`]).
        chain: Vec<u8>,
    },
    /// The request was rejected; carries a stable [`reject`] code and a message.
    Rejected {
        /// A stable [`reject`] code.
        code: u16,
        /// A human-readable reason.
        message: String,
    },
}

impl JoinOutcome {
    /// Canonical wire form (versioned, length-prefixed).
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(&OUTCOME_MAGIC);
        match self {
            JoinOutcome::Admitted { chain } => {
                buf.push(0);
                push_lp(&mut buf, chain);
            }
            JoinOutcome::Rejected { code, message } => {
                buf.push(1);
                buf.extend_from_slice(&code.to_le_bytes());
                push_lp(&mut buf, message.as_bytes());
            }
        }
        buf
    }

    /// Parse the canonical wire form. Rejects a bad magic/version, truncation,
    /// an unknown tag, or trailing bytes.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, JoinError> {
        let mut r = Reader::new(bytes);
        let magic = r.take_arr::<4>().ok_or(JoinError::Malformed("truncated"))?;
        if magic != OUTCOME_MAGIC {
            return Err(JoinError::Malformed("bad magic or version"));
        }
        let tag = r
            .take_arr::<1>()
            .ok_or(JoinError::Malformed("truncated tag"))?[0];
        let outcome = match tag {
            0 => JoinOutcome::Admitted {
                chain: r
                    .take_lp()
                    .ok_or(JoinError::Malformed("bad chain"))?
                    .to_vec(),
            },
            1 => {
                let code = r.take_u16().ok_or(JoinError::Malformed("truncated code"))?;
                let message = r
                    .take_lp_string()
                    .ok_or(JoinError::Malformed("bad message"))?;
                JoinOutcome::Rejected { code, message }
            }
            _ => return Err(JoinError::Malformed("unknown outcome tag")),
        };
        if !r.done() {
            return Err(JoinError::Malformed("trailing bytes"));
        }
        Ok(outcome)
    }

    /// Interpret the outcome the device received. On [`Self::Admitted`], parse
    /// the chain and **verify it anchors at the invited mesh root and binds to
    /// this device** — defending the joiner against a rogue operator returning
    /// a chain for a different mesh or a different key. On [`Self::Rejected`],
    /// surface the reason.
    pub fn into_chain(
        self,
        device: &EntityId,
        invite_root: &EntityId,
    ) -> Result<DelegationChain, JoinError> {
        match self {
            JoinOutcome::Admitted { chain } => {
                let chain =
                    DelegationChain::from_bytes(&chain).map_err(JoinError::MalformedGrant)?;
                let reg = RevocationRegistry::new();
                chain
                    .verify(device, invite_root, &reg, 0)
                    .map_err(|_| JoinError::UntrustedGrant)?;
                Ok(chain)
            }
            JoinOutcome::Rejected { code, message } => Err(JoinError::Rejected { code, message }),
        }
    }
}

/// Device-side errors interpreting a [`JoinOutcome`].
#[derive(Debug, thiserror::Error)]
pub enum JoinError {
    /// The operator rejected the request. Carries a stable [`reject`] code.
    #[error("enrollment rejected (code {code}): {message}")]
    Rejected {
        /// A stable [`reject`] code.
        code: u16,
        /// The operator's reason.
        message: String,
    },
    /// The admitted delegation bytes didn't parse.
    #[error("the admitted delegation is malformed: {0}")]
    MalformedGrant(TokenError),
    /// The admitted delegation doesn't anchor at the invited mesh root or bind
    /// to this device — a rogue or confused operator.
    #[error("the admitted delegation does not anchor at the invited mesh root / this device")]
    UntrustedGrant,
    /// The outcome bytes themselves were malformed.
    #[error("malformed join outcome: {0}")]
    Malformed(&'static str),
}

/// The operator side: holds the mesh **root** identity and the single-use
/// ledger, mints invites, and approves join requests into `root → device`
/// delegations.
pub struct EnrollmentAuthority {
    root: Identity,
    /// Spent invite nonce → its expiry (unix secs). Enforces single-use;
    /// entries prune once past expiry (a post-expiry replay is caught by the
    /// `Expired` check regardless).
    seen: Mutex<HashMap<[u8; 16], u64>>,
}

impl EnrollmentAuthority {
    /// Build an authority for the given mesh `root` identity (which owns the
    /// root signing key).
    pub fn new(root: Identity) -> Self {
        Self {
            root,
            seen: Mutex::new(HashMap::new()),
        }
    }

    /// The mesh root's entity id.
    pub fn root_id(&self) -> &EntityId {
        self.root.entity_id()
    }

    /// The displayed [`fingerprint`] of the mesh root.
    pub fn root_fingerprint(&self) -> String {
        fingerprint(self.root.entity_id())
    }

    /// Mint an invite for this mesh, valid for `ttl`. (`mesh.invite`.)
    pub fn mint_invite(&self, rendezvous: impl Into<String>, ttl: Duration) -> InviteToken {
        InviteToken::mint(self.root.entity_id(), rendezvous, ttl)
    }

    /// [`Self::mint_invite`] with an explicit `now` — for deterministic tests.
    pub fn mint_invite_at(
        &self,
        rendezvous: impl Into<String>,
        ttl: Duration,
        now: u64,
    ) -> InviteToken {
        InviteToken::mint_at(self.root.entity_id(), rendezvous, ttl, now)
    }

    /// Approve a join request against its invite (`now` in unix secs), minting
    /// the `root → device` delegation. Fail-closed at every step, in an order
    /// that never burns a legit invite on a garbage request:
    ///
    /// 1. invite **and** request name *this* mesh root,
    /// 2. invite is not expired,
    /// 3. request presents the invite's nonce (proof-of-invite),
    /// 4. the device holds its key (self-signature verifies),
    /// 5. the nonce is unspent — burned only now, so a bad request above can't
    ///    consume the invite,
    /// 6. sign `root → device` (delegable, so the device can extend locally).
    pub fn approve(
        &self,
        request: &JoinRequest,
        invite: &InviteToken,
        now: u64,
        grant_ttl: Duration,
        max_depth: u8,
    ) -> Result<Enrollment, EnrollmentError> {
        self.verify_request(request, invite, now)?;
        self.spend_nonce(invite.nonce, invite.expires_at, now)?;
        let chain =
            DelegationChain::derive_device(&self.root, &request.device, grant_ttl, max_depth)?;
        Ok(Enrollment {
            chain,
            device: request.device.clone(),
            name: request.name.clone(),
            tags: request.tags.clone(),
        })
    }

    /// Validate a request against its invite **without** consuming the invite
    /// or signing anything: names this mesh (1), unexpired (2), correct nonce
    /// (3), holds its key (4). This is the check to run *before* asking a human
    /// to approve — it lets an operator surface show a legitimate request
    /// (device id / name / tags) and defer the single-use spend to the moment
    /// of actual admission ([`Self::approve`]), so a request a human ultimately
    /// denies never burns a still-good invite.
    pub fn verify_request(
        &self,
        request: &JoinRequest,
        invite: &InviteToken,
        now: u64,
    ) -> Result<(), EnrollmentError> {
        let root_id = self.root.entity_id();
        if &invite.root != root_id || &request.root != root_id {
            return Err(EnrollmentError::WrongMesh);
        }
        if invite.is_expired(now) {
            return Err(EnrollmentError::Expired);
        }
        if request.invite_nonce != invite.nonce {
            return Err(EnrollmentError::NonceMismatch);
        }
        request.verify_self_signature()
    }

    /// [`Self::approve`] reading the system clock for `now`.
    pub fn approve_now(
        &self,
        request: &JoinRequest,
        invite: &InviteToken,
        grant_ttl: Duration,
        max_depth: u8,
    ) -> Result<Enrollment, EnrollmentError> {
        self.approve(request, invite, now_unix(), grant_ttl, max_depth)
    }

    /// Burn `nonce` for single-use. Prunes expired entries first, rejects a
    /// replay, and fails closed if the ledger is saturated.
    fn spend_nonce(&self, nonce: [u8; 16], expiry: u64, now: u64) -> Result<(), EnrollmentError> {
        let mut seen = self.seen.lock();
        seen.retain(|_, exp| *exp > now);
        if seen.contains_key(&nonce) {
            return Err(EnrollmentError::Replay);
        }
        if seen.len() >= MAX_OUTSTANDING_INVITES {
            return Err(EnrollmentError::LedgerSaturated);
        }
        seen.insert(nonce, expiry);
        Ok(())
    }
}

/// Build the canonical, domain-separated, length-prefixed challenge the device
/// signs and the authority reconstructs. Length prefixes make the framing
/// unambiguous — no field-boundary confusion between name and tags.
fn join_challenge(
    device: &EntityId,
    name: &str,
    tags: &[String],
    invite_nonce: &[u8; 16],
    root: &EntityId,
) -> Vec<u8> {
    let mut msg =
        Vec::with_capacity(JOIN_CHALLENGE_DOMAIN.len() + 32 + 4 + name.len() + 4 + 16 + 4 + 32);
    msg.extend_from_slice(JOIN_CHALLENGE_DOMAIN);
    push_lp(&mut msg, device.as_bytes());
    push_lp(&mut msg, name.as_bytes());
    msg.extend_from_slice(&(tags.len() as u32).to_le_bytes());
    for tag in tags {
        push_lp(&mut msg, tag.as_bytes());
    }
    msg.extend_from_slice(invite_nonce);
    push_lp(&mut msg, root.as_bytes());
    msg
}

/// Append a `u32`-length-prefixed byte field.
fn push_lp(buf: &mut Vec<u8>, bytes: &[u8]) {
    buf.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    buf.extend_from_slice(bytes);
}

/// Bounds-checked forward cursor for the hand-rolled wire formats.
struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        if end > self.buf.len() {
            return None;
        }
        let s = &self.buf[self.pos..end];
        self.pos = end;
        Some(s)
    }

    fn take_arr<const N: usize>(&mut self) -> Option<[u8; N]> {
        let s = self.take(N)?;
        let mut a = [0u8; N];
        a.copy_from_slice(s);
        Some(a)
    }

    fn take_u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(self.take_arr::<2>()?))
    }

    fn take_u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take_arr::<4>()?))
    }

    fn take_u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take_arr::<8>()?))
    }

    fn take_lp(&mut self) -> Option<&'a [u8]> {
        let n = self.take_u32()? as usize;
        self.take(n)
    }

    fn take_lp_string(&mut self) -> Option<String> {
        std::str::from_utf8(self.take_lp()?)
            .ok()
            .map(|s| s.to_string())
    }

    fn done(&self) -> bool {
        self.pos == self.buf.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::delegation::DEFAULT_DELEGATION_DEPTH;

    const HOUR: Duration = Duration::from_secs(3600);
    const T0: u64 = 1_700_000_000;

    fn authority() -> EnrollmentAuthority {
        EnrollmentAuthority::new(Identity::generate())
    }

    #[test]
    fn mint_produces_a_live_then_expired_invite() {
        let auth = authority();
        let invite = auth.mint_invite_at("relay://rv", HOUR, T0);
        assert_eq!(&invite.root, auth.root_id());
        assert!(!invite.is_expired(T0));
        assert!(!invite.is_expired(T0 + 3599));
        assert!(invite.is_expired(T0 + 3600));
        assert!(invite.is_expired(T0 + 10_000));
    }

    #[test]
    fn join_request_self_verifies_and_binds_its_fields() {
        let auth = authority();
        let invite = auth.mint_invite_at("relay://rv", HOUR, T0);
        let device = Identity::generate();
        let req = JoinRequest::create(&device, "laptop", vec!["region:office".into()], &invite);
        assert_eq!(&req.device, device.entity_id());
        req.verify_self_signature().expect("fresh request verifies");

        // Tampering any signed field breaks the signature.
        let mut tampered = req.clone();
        tampered.name = "not-laptop".into();
        assert!(matches!(
            tampered.verify_self_signature(),
            Err(EnrollmentError::BadSignature)
        ));
        let mut tampered = req.clone();
        tampered.tags = vec!["region:evil".into()];
        assert!(tampered.verify_self_signature().is_err());
    }

    #[test]
    fn approve_happy_path_mints_a_verifiable_device_chain() {
        let auth = authority();
        let invite = auth.mint_invite_at("relay://rv", HOUR, T0);
        let device = Identity::generate();
        let req = JoinRequest::create(&device, "vps", vec![], &invite);

        let enrollment = auth
            .approve(&req, &invite, T0, HOUR, DEFAULT_DELEGATION_DEPTH)
            .expect("valid join must approve");
        assert_eq!(&enrollment.device, device.entity_id());
        assert_eq!(enrollment.name, "vps");

        // The device holds a root → device chain that anchors at the mesh root.
        let reg = RevocationRegistry::new();
        assert_eq!(enrollment.chain.len(), 1);
        assert_eq!(&enrollment.chain.root(), auth.root_id());
        enrollment
            .chain
            .verify(device.entity_id(), auth.root_id(), &reg, 0)
            .expect("enrolled device chain must verify");
    }

    #[test]
    fn approve_rejects_an_expired_invite() {
        let auth = authority();
        let invite = auth.mint_invite_at("relay://rv", HOUR, T0);
        let device = Identity::generate();
        let req = JoinRequest::create(&device, "late", vec![], &invite);
        // Approve after the TTL elapsed.
        assert!(matches!(
            auth.approve(&req, &invite, T0 + 3600, HOUR, DEFAULT_DELEGATION_DEPTH),
            Err(EnrollmentError::Expired)
        ));
    }

    #[test]
    fn approve_rejects_a_wrong_nonce() {
        let auth = authority();
        let invite = auth.mint_invite_at("relay://rv", HOUR, T0);
        let device = Identity::generate();
        // A request built against a *different* invite (different nonce), but
        // for the same mesh root.
        let other = auth.mint_invite_at("relay://rv", HOUR, T0);
        let req = JoinRequest::create(&device, "confused", vec![], &other);
        assert!(matches!(
            auth.approve(&req, &invite, T0, HOUR, DEFAULT_DELEGATION_DEPTH),
            Err(EnrollmentError::NonceMismatch)
        ));
    }

    #[test]
    fn approve_is_single_use() {
        let auth = authority();
        let invite = auth.mint_invite_at("relay://rv", HOUR, T0);
        let device = Identity::generate();
        let req = JoinRequest::create(&device, "d", vec![], &invite);

        auth.approve(&req, &invite, T0, HOUR, DEFAULT_DELEGATION_DEPTH)
            .expect("first approval succeeds");
        // A second redemption of the same invite — even by the same device —
        // is a replay.
        assert!(matches!(
            auth.approve(&req, &invite, T0, HOUR, DEFAULT_DELEGATION_DEPTH),
            Err(EnrollmentError::Replay)
        ));
    }

    #[test]
    fn approve_rejects_a_tampered_request() {
        let auth = authority();
        let invite = auth.mint_invite_at("relay://rv", HOUR, T0);
        let device = Identity::generate();
        let mut req = JoinRequest::create(&device, "d", vec![], &invite);
        req.signature[0] ^= 0xff; // corrupt the proof-of-key
        assert!(matches!(
            auth.approve(&req, &invite, T0, HOUR, DEFAULT_DELEGATION_DEPTH),
            Err(EnrollmentError::BadSignature)
        ));
        // A rejected request must NOT have burned the invite — a fresh valid
        // request still approves.
        let good = JoinRequest::create(&device, "d", vec![], &invite);
        auth.approve(&good, &invite, T0, HOUR, DEFAULT_DELEGATION_DEPTH)
            .expect("a valid request after a rejected one still approves");
    }

    #[test]
    fn approve_rejects_a_foreign_mesh_invite() {
        let auth = authority();
        // An invite minted by a *different* root (evil-twin / wrong mesh).
        let other_root = EnrollmentAuthority::new(Identity::generate());
        let foreign = other_root.mint_invite_at("relay://rv", HOUR, T0);
        let device = Identity::generate();
        let req = JoinRequest::create(&device, "d", vec![], &foreign);
        assert!(matches!(
            auth.approve(&req, &foreign, T0, HOUR, DEFAULT_DELEGATION_DEPTH),
            Err(EnrollmentError::WrongMesh)
        ));
    }

    #[test]
    fn enrolled_device_extends_and_is_revocable() {
        // End-to-end through the enrollment entry point: approve → device
        // extends to its gateway → the gateway chain verifies → revoking the
        // device kills it.
        let auth = authority();
        let invite = auth.mint_invite_at("relay://rv", HOUR, T0);
        let device = Identity::generate();
        let req = JoinRequest::create(&device, "d", vec![], &invite);
        let enrollment = auth
            .approve(&req, &invite, T0, HOUR, DEFAULT_DELEGATION_DEPTH)
            .unwrap();

        let gateway = Identity::generate();
        let gw_chain = enrollment
            .chain
            .extend_delegate(&device, gateway.entity_id())
            .unwrap();
        let reg = RevocationRegistry::new();
        gw_chain
            .verify(gateway.entity_id(), auth.root_id(), &reg, 0)
            .expect("device → gateway chain must verify");

        // Revoke the device: its gateway chain fails on next check.
        reg.revoke_below(device.entity_id(), 1);
        assert!(gw_chain
            .verify(gateway.entity_id(), auth.root_id(), &reg, 0)
            .is_err());
    }

    #[test]
    fn invite_token_round_trips_through_bytes() {
        let auth = authority();
        let invite = auth.mint_invite_at("relay://some/rendezvous", HOUR, T0);
        let bytes = invite.to_bytes();
        let parsed = InviteToken::from_bytes(&bytes).unwrap();
        assert_eq!(parsed, invite);
        // Trailing garbage and truncation are rejected.
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(InviteToken::from_bytes(&trailing).is_err());
        assert!(InviteToken::from_bytes(&bytes[..bytes.len() - 1]).is_err());
        // A join blob must not parse as an invite (magic separation).
        let device = Identity::generate();
        let req = JoinRequest::create(&device, "d", vec![], &invite);
        assert!(InviteToken::from_bytes(&req.to_bytes()).is_err());
    }

    #[test]
    fn join_request_round_trips_through_bytes() {
        let auth = authority();
        let invite = auth.mint_invite_at("relay://rv", HOUR, T0);
        let device = Identity::generate();
        let req = JoinRequest::create(
            &device,
            "workstation",
            vec!["region:office".into(), "gpu:true".into()],
            &invite,
        );
        let bytes = req.to_bytes();
        let parsed = JoinRequest::from_bytes(&bytes).unwrap();
        assert_eq!(parsed.device, req.device);
        assert_eq!(parsed.name, req.name);
        assert_eq!(parsed.tags, req.tags);
        assert_eq!(parsed.invite_nonce, req.invite_nonce);
        assert_eq!(parsed.root, req.root);
        assert_eq!(parsed.signature, req.signature);
        // The round-tripped request still self-verifies.
        parsed.verify_self_signature().unwrap();
        assert!(JoinRequest::from_bytes(&bytes[..bytes.len() - 1]).is_err());
    }

    #[test]
    fn invite_string_round_trips_and_tolerates_whitespace() {
        let auth = authority();
        let invite = auth.mint_invite_at("relay://some/rendezvous", HOUR, T0);
        let s = invite.encode();
        assert!(s.starts_with("net-invite:"));
        assert_eq!(InviteToken::decode(&s).unwrap(), invite);
        // A trailing newline / leading spaces from copy-paste or a QR scan.
        assert_eq!(InviteToken::decode(&format!("  {s}\n")).unwrap(), invite);
        // Missing prefix, invalid base64, and malformed bytes are all rejected.
        assert!(InviteToken::decode("deadbeef").is_err());
        assert!(InviteToken::decode("net-invite:!!!not-base64!!!").is_err());
        assert!(InviteToken::decode("net-invite:AAAA").is_err());
    }

    #[test]
    fn join_outcome_round_trips_both_variants() {
        let admitted = JoinOutcome::Admitted {
            chain: vec![1, 2, 3, 4],
        };
        assert_eq!(
            JoinOutcome::from_bytes(&admitted.to_bytes()).unwrap(),
            admitted
        );
        let rejected = JoinOutcome::Rejected {
            code: reject::EXPIRED,
            message: "invite has expired".into(),
        };
        assert_eq!(
            JoinOutcome::from_bytes(&rejected.to_bytes()).unwrap(),
            rejected
        );
        // Malformed / truncated / cross-typed bytes are rejected.
        assert!(JoinOutcome::from_bytes(b"nope").is_err());
        let bytes = admitted.to_bytes();
        assert!(JoinOutcome::from_bytes(&bytes[..bytes.len() - 1]).is_err());
        let auth = authority();
        let invite = auth.mint_invite_at("relay://rv", HOUR, T0);
        assert!(JoinOutcome::from_bytes(&invite.to_bytes()).is_err());
    }

    #[test]
    fn into_chain_accepts_a_valid_grant_and_rejects_impostors() {
        // A real admitted grant from an approve.
        let auth = authority();
        let invite = auth.mint_invite_at("relay://rv", HOUR, T0);
        let device = Identity::generate();
        let req = JoinRequest::create(&device, "d", vec![], &invite);
        let enrollment = auth
            .approve(&req, &invite, T0, HOUR, DEFAULT_DELEGATION_DEPTH)
            .unwrap();
        let admitted = JoinOutcome::Admitted {
            chain: enrollment.chain.to_bytes(),
        };
        // The device accepts it: anchors at the invited root, binds to itself.
        let chain = admitted
            .clone()
            .into_chain(device.entity_id(), &invite.root)
            .expect("valid grant accepted");
        assert_eq!(&chain.leaf(), device.entity_id());

        // A grant that anchors at a *different* root (rogue operator) is refused.
        assert!(matches!(
            admitted
                .clone()
                .into_chain(device.entity_id(), Identity::generate().entity_id()),
            Err(JoinError::UntrustedGrant)
        ));
        // A grant bound to a *different* device is refused.
        assert!(matches!(
            admitted.into_chain(Identity::generate().entity_id(), &invite.root),
            Err(JoinError::UntrustedGrant)
        ));
        // A rejection surfaces its code.
        let rejected = JoinOutcome::Rejected {
            code: reject::REPLAY,
            message: "used".into(),
        };
        assert!(matches!(
            rejected.into_chain(device.entity_id(), &invite.root),
            Err(JoinError::Rejected { code, .. }) if code == reject::REPLAY
        ));
    }

    #[test]
    fn fingerprint_is_stable_grouped_and_distinct() {
        let a = Identity::generate();
        let b = Identity::generate();
        let fa = fingerprint(a.entity_id());
        assert_eq!(fa, fingerprint(a.entity_id()), "stable across calls");
        assert_ne!(fa, fingerprint(b.entity_id()), "distinct ids differ");
        assert_eq!(fa.len(), 19); // 16 hex + 3 dashes
        assert_eq!(fa.matches('-').count(), 3);
        assert!(fa
            .chars()
            .all(|c| c == '-' || c.is_ascii_hexdigit() && !c.is_ascii_lowercase()));
    }
}
