//! The **browser bootstrap credential** (plan §5 Layer 0).
//!
//! A browser has no build-time provisioning step, so the secret a
//! Noise `NKpsk0` handshake needs has to come from somewhere named.
//! This module names it. The credential is a **new format**, not an
//! [`InviteToken`] with extra fields
//! bolted on:
//!
//! ```text
//! browser bootstrap credential =
//!     InviteToken          (root, rendezvous, nonce, expires_at)
//!   + anchor Noise static pubkey  (X25519 — the key Layer 0 pins)
//!   + mesh PSK                    (the NKpsk0 admission secret)
//!   + trust-domain id             (which PSK domain this is for)
//!   + anchor bootstrap URL
//!   + psk_expires_at              (the standing half's own deadline)
//! ```
//!
//! # The two lifetimes, stated in the format
//!
//! The two halves do **not** expire together, and the format says so
//! rather than leaving it to prose:
//!
//! - The invite **nonce is single-use** and short-lived; its deadline
//!   is the invite's own [`InviteToken::expires_at`], surfaced here as
//!   [`BrowserBootstrapCredential::nonce_expires_at`].
//! - The **PSK is standing** — a transport secret shared by every
//!   holder of a credential in the same trust domain. It has its own,
//!   normally much longer, [`BrowserBootstrapCredential::psk_expires_at`].
//!
//! [`BrowserBootstrapCredential::validate_at`] enforces both, and says
//! which one failed.
//!
//! # The trust domain, and why it is derived
//!
//! Plan §5: a public-browser deployment is a **deliberately separate
//! transport trust domain** with its own PSK, and an existing private
//! deployment's PSK must never be handed to public visitors. The
//! credential therefore carries a [`TrustDomainId`], which is *derived
//! from the PSK* rather than being an operator-chosen label. That makes
//! the check mechanical on both sides: an anchor compares the
//! credential's id against the id of the PSK it actually holds
//! ([`BrowserBootstrapCredential::check_trust_domain`]), so a credential
//! minted for one domain is refused by an anchor of another **before**
//! any handshake is attempted — and a credential whose stored id does
//! not match its own PSK is malformed, not merely wrong.
//!
//! The id is a one-way function of the PSK, so publishing it (it rides
//! in the credential and may appear in logs) reveals nothing about the
//! secret.
//!
//! # Secrecy
//!
//! The PSK is the one field in this crate that must never be printed.
//! [`BrowserBootstrapCredential`]'s [`Debug`] is hand-written and
//! redacts it; [`Psk`] redacts itself, so the secret does not leak
//! through a struct that merely contains one. The encoded string form
//! *does* carry the PSK — it is the credential — so it is handed to
//! JavaScript over an authenticated channel and never logged.

use std::fmt;
use std::time::Duration;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;

use crate::enrollment::{now_unix, EnrollmentError, InviteToken};
use crate::identity::EntityId;

/// 4-byte magic for the credential wire form (Net-Mesh Bootstrap).
/// The version rides in its own byte immediately after, so a v2
/// credential is *recognisably* a credential rather than garbage with
/// a bad magic.
const CREDENTIAL_MAGIC: [u8; 4] = *b"NMBC";

/// The only format version this build mints or accepts.
pub const CREDENTIAL_VERSION: u8 = 1;

/// Scheme-like prefix on the copy-paste string form, so it is
/// self-describing and cannot be confused with an invite string.
const CREDENTIAL_STRING_PREFIX: &str = "net-bootstrap:";

/// Domain separation for [`TrustDomainId`] derivation.
const TRUST_DOMAIN_KDF_CONTEXT: &str = "net-mesh browser bootstrap trust-domain v1";

/// Upper bound on the encoded credential, so a hostile blob cannot
/// make a parser walk megabytes before failing.
const MAX_CREDENTIAL_BYTES: usize = 8 * 1024;

/// Upper bound on the bootstrap URL — generous for a real URL, small
/// enough to be an obvious refusal for a blob.
const MAX_URL_BYTES: usize = 1024;

/// Errors from minting, parsing or validating a
/// [`BrowserBootstrapCredential`].
#[derive(Debug, thiserror::Error)]
pub enum BootstrapCredentialError {
    /// Truncated, mis-magicked, over-long, or carrying trailing bytes.
    #[error("malformed bootstrap credential: {0}")]
    Malformed(&'static str),
    /// A format version this build does not implement. Carries the
    /// version seen, so an operator learns *which* one.
    #[error("bootstrap credential format version {0} is not supported (this build speaks {CREDENTIAL_VERSION})")]
    UnsupportedVersion(u8),
    /// The embedded invite did not parse.
    #[error("bootstrap credential carries a malformed invite: {0}")]
    Invite(#[from] EnrollmentError),
    /// The invite nonce's deadline has passed — the single-use half.
    #[error("the credential's invite nonce expired at {expires_at} (now {now})")]
    NonceExpired {
        /// Unix seconds the nonce stopped being presentable.
        expires_at: u64,
        /// The clock reading the check used.
        now: u64,
    },
    /// The PSK's own deadline has passed — the standing half.
    #[error("the credential's PSK expired at {expires_at} (now {now})")]
    PskExpired {
        /// Unix seconds the PSK stopped being usable.
        expires_at: u64,
        /// The clock reading the check used.
        now: u64,
    },
    /// The credential names a trust domain this anchor does not serve.
    #[error("the credential is for trust domain {presented}, this anchor serves {ours}")]
    WrongTrustDomain {
        /// The domain the credential names.
        presented: TrustDomainId,
        /// The domain the checking anchor's own PSK derives.
        ours: TrustDomainId,
    },
    /// The stored trust-domain id is not the one the carried PSK
    /// derives: the credential is internally inconsistent, which no
    /// honest minter produces.
    #[error("the credential's trust-domain id does not match its own PSK")]
    TrustDomainMismatch,
    /// The bootstrap URL is not an `https://` (or `http://localhost`)
    /// URL a browser could actually fetch.
    #[error("the credential's bootstrap URL is not usable by a browser: {0}")]
    BadUrl(&'static str),
}

/// The NKpsk0 pre-shared key, which never prints.
///
/// A newtype rather than a bare `[u8; 32]` so the redaction travels
/// with the value: anything that derives [`Debug`] and contains a
/// `Psk` stays safe.
#[derive(Clone, PartialEq, Eq)]
pub struct Psk([u8; 32]);

impl Psk {
    /// Wrap raw PSK bytes.
    pub fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The raw bytes, for handing to the Noise builder. Named
    /// `expose_*` so a call site that leaks one is visible in review.
    pub fn expose_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// The trust domain this PSK defines.
    pub fn trust_domain(&self) -> TrustDomainId {
        TrustDomainId::of_psk(self)
    }
}

impl fmt::Debug for Psk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Psk(<redacted>)")
    }
}

/// A stable, public identifier for a PSK trust domain: 16 bytes
/// derived from the PSK with a domain-separated KDF.
///
/// One-way, so it is safe to print, log and put on the wire; equality
/// is exactly "the same PSK".
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct TrustDomainId([u8; 16]);

impl TrustDomainId {
    /// Derive the id of the domain `psk` defines.
    pub fn of_psk(psk: &Psk) -> Self {
        let full = blake3::derive_key(TRUST_DOMAIN_KDF_CONTEXT, psk.expose_bytes());
        let mut id = [0u8; 16];
        id.copy_from_slice(&full[..16]);
        Self(id)
    }

    /// The raw id bytes.
    pub fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

impl fmt::Display for TrustDomainId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for b in self.0 {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for TrustDomainId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TrustDomainId({self})")
    }
}

/// The artefact an application hands a browser so it can reach an
/// anchor: an invite, the anchor key to pin, the transport PSK, the
/// domain that PSK belongs to, and where to POST the offer.
///
/// See the [module docs](self) for the two lifetimes and the trust
/// domain rules.
#[derive(Clone, PartialEq, Eq)]
pub struct BrowserBootstrapCredential {
    /// The invite whose nonce the browser echoes in its `JoinRequest`.
    pub invite: InviteToken,
    /// The anchor's Noise static X25519 public key — **the key Layer 0
    /// pins**. The browser runs its handshake against this key, never
    /// against one read out of an HTTP response.
    pub anchor_noise_pubkey: [u8; 32],
    /// The NKpsk0 pre-shared key of this trust domain.
    pub psk: Psk,
    /// Which trust domain [`Self::psk`] belongs to. Derived from the
    /// PSK, carried explicitly so an anchor can refuse a foreign
    /// credential without trial-decrypting anything.
    pub trust_domain: TrustDomainId,
    /// `https://…` base URL of the anchor's bootstrap listener.
    pub bootstrap_url: String,
    /// Unix seconds after which the **standing** PSK half is no longer
    /// usable. Distinct from the invite's nonce deadline.
    pub psk_expires_at: u64,
}

impl BrowserBootstrapCredential {
    /// Mint a credential. `psk_ttl` is the **standing** lifetime; the
    /// single-use half's deadline comes from `invite` and is not
    /// touched here.
    pub fn mint(
        invite: InviteToken,
        anchor_noise_pubkey: [u8; 32],
        psk: Psk,
        bootstrap_url: impl Into<String>,
        psk_ttl: Duration,
    ) -> Self {
        Self::mint_at(
            invite,
            anchor_noise_pubkey,
            psk,
            bootstrap_url,
            psk_ttl,
            now_unix(),
        )
    }

    /// [`Self::mint`] with an explicit `now` (unix secs) — for
    /// deterministic tests and callers holding a clock reading.
    pub fn mint_at(
        invite: InviteToken,
        anchor_noise_pubkey: [u8; 32],
        psk: Psk,
        bootstrap_url: impl Into<String>,
        psk_ttl: Duration,
        now: u64,
    ) -> Self {
        let trust_domain = psk.trust_domain();
        Self {
            invite,
            anchor_noise_pubkey,
            psk,
            trust_domain,
            bootstrap_url: bootstrap_url.into(),
            psk_expires_at: now.saturating_add(psk_ttl.as_secs()),
        }
    }

    /// The mesh root the invite admits into.
    pub fn root(&self) -> &EntityId {
        &self.invite.root
    }

    /// The single-use half's deadline (unix secs).
    pub fn nonce_expires_at(&self) -> u64 {
        self.invite.expires_at
    }

    /// The standing half's deadline (unix secs).
    pub fn psk_expires_at(&self) -> u64 {
        self.psk_expires_at
    }

    /// Both lifetimes and the internal consistency of the trust-domain
    /// id, at `now`. The error says **which** half failed, because
    /// "expired" alone sends an operator to the wrong knob: a stale
    /// nonce means mint another invite, a stale PSK means rotate the
    /// domain.
    pub fn validate_at(&self, now: u64) -> Result<(), BootstrapCredentialError> {
        if self.trust_domain != self.psk.trust_domain() {
            return Err(BootstrapCredentialError::TrustDomainMismatch);
        }
        if now >= self.invite.expires_at {
            return Err(BootstrapCredentialError::NonceExpired {
                expires_at: self.invite.expires_at,
                now,
            });
        }
        if now >= self.psk_expires_at {
            return Err(BootstrapCredentialError::PskExpired {
                expires_at: self.psk_expires_at,
                now,
            });
        }
        Ok(())
    }

    /// [`Self::validate_at`] against the system clock.
    pub fn validate(&self) -> Result<(), BootstrapCredentialError> {
        self.validate_at(now_unix())
    }

    /// Is this credential for the domain `ours` defines? An anchor
    /// calls this with its own PSK before doing anything else with a
    /// presented credential.
    pub fn check_trust_domain(&self, ours: &Psk) -> Result<(), BootstrapCredentialError> {
        let ours = ours.trust_domain();
        if self.trust_domain == ours {
            Ok(())
        } else {
            Err(BootstrapCredentialError::WrongTrustDomain {
                presented: self.trust_domain,
                ours,
            })
        }
    }

    /// Canonical wire form: magic, version byte, then tagged
    /// length-prefixed fields in declaration order.
    pub fn to_bytes(&self) -> Vec<u8> {
        let invite = self.invite.to_bytes();
        let mut buf = Vec::with_capacity(
            5 + 4 + invite.len() + 32 + 32 + 16 + 8 + 4 + self.bootstrap_url.len(),
        );
        buf.extend_from_slice(&CREDENTIAL_MAGIC);
        buf.push(CREDENTIAL_VERSION);
        push_lp(&mut buf, &invite);
        buf.extend_from_slice(&self.anchor_noise_pubkey);
        buf.extend_from_slice(self.psk.expose_bytes());
        buf.extend_from_slice(self.trust_domain.as_bytes());
        buf.extend_from_slice(&self.psk_expires_at.to_le_bytes());
        push_lp(&mut buf, self.bootstrap_url.as_bytes());
        buf
    }

    /// Parse the canonical wire form with strict bounds. Rejects a bad
    /// magic, an unsupported version, truncation, an over-long blob or
    /// URL, a trust-domain id inconsistent with the carried PSK, a URL
    /// a browser could not fetch, and trailing bytes.
    ///
    /// Parsing does **not** check the two lifetimes — that is
    /// [`Self::validate_at`], because a caller inspecting an expired
    /// credential (the `inspect` CLI) still wants to see its fields.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, BootstrapCredentialError> {
        if bytes.len() > MAX_CREDENTIAL_BYTES {
            return Err(BootstrapCredentialError::Malformed("over the size bound"));
        }
        let mut r = Reader::new(bytes);
        let magic = r
            .take_arr::<4>()
            .ok_or(BootstrapCredentialError::Malformed("truncated"))?;
        if magic != CREDENTIAL_MAGIC {
            return Err(BootstrapCredentialError::Malformed("bad magic"));
        }
        let version = r
            .take_arr::<1>()
            .ok_or(BootstrapCredentialError::Malformed("truncated version"))?[0];
        if version != CREDENTIAL_VERSION {
            return Err(BootstrapCredentialError::UnsupportedVersion(version));
        }
        let invite = InviteToken::from_bytes(
            r.take_lp()
                .ok_or(BootstrapCredentialError::Malformed("truncated invite"))?,
        )?;
        let anchor_noise_pubkey = r
            .take_arr::<32>()
            .ok_or(BootstrapCredentialError::Malformed(
                "truncated anchor Noise key",
            ))?;
        let psk = Psk::new(
            r.take_arr::<32>()
                .ok_or(BootstrapCredentialError::Malformed("truncated PSK"))?,
        );
        let trust_domain = TrustDomainId(r.take_arr::<16>().ok_or(
            BootstrapCredentialError::Malformed("truncated trust-domain id"),
        )?);
        let psk_expires_at = r
            .take_u64()
            .ok_or(BootstrapCredentialError::Malformed("truncated PSK expiry"))?;
        let url_bytes = r
            .take_lp()
            .ok_or(BootstrapCredentialError::Malformed("truncated URL"))?;
        if url_bytes.len() > MAX_URL_BYTES {
            return Err(BootstrapCredentialError::Malformed("URL over the bound"));
        }
        let bootstrap_url = std::str::from_utf8(url_bytes)
            .map_err(|_| BootstrapCredentialError::Malformed("non-UTF-8 URL"))?
            .to_string();
        if !r.done() {
            return Err(BootstrapCredentialError::Malformed("trailing bytes"));
        }
        if trust_domain != psk.trust_domain() {
            return Err(BootstrapCredentialError::TrustDomainMismatch);
        }
        check_browser_url(&bootstrap_url)?;
        Ok(Self {
            invite,
            anchor_noise_pubkey,
            psk,
            trust_domain,
            bootstrap_url,
            psk_expires_at,
        })
    }

    /// The string form handed to JavaScript: a `net-bootstrap:` prefix
    /// followed by URL-safe unpadded base64 of [`Self::to_bytes`].
    ///
    /// **This string contains the PSK.** It is the credential; treat it
    /// like one.
    pub fn encode(&self) -> String {
        let mut s = String::from(CREDENTIAL_STRING_PREFIX);
        s.push_str(&URL_SAFE_NO_PAD.encode(self.to_bytes()));
        s
    }

    /// Parse a string produced by [`Self::encode`]. Tolerates
    /// surrounding whitespace; rejects a missing prefix, invalid
    /// base64, or malformed bytes.
    pub fn decode(s: &str) -> Result<Self, BootstrapCredentialError> {
        let body = s.trim().strip_prefix(CREDENTIAL_STRING_PREFIX).ok_or(
            BootstrapCredentialError::Malformed("missing net-bootstrap: prefix"),
        )?;
        if body.len() > MAX_CREDENTIAL_BYTES * 2 {
            return Err(BootstrapCredentialError::Malformed("over the size bound"));
        }
        let bytes = URL_SAFE_NO_PAD
            .decode(body)
            .map_err(|_| BootstrapCredentialError::Malformed("invalid base64"))?;
        Self::from_bytes(&bytes)
    }
}

/// Hand-written so the PSK cannot escape through a debug print, a
/// panic message, or a `tracing` field. Every other field is shown —
/// redacting the whole struct would make it useless for diagnosis.
impl fmt::Debug for BrowserBootstrapCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BrowserBootstrapCredential")
            .field("invite", &self.invite)
            .field("anchor_noise_pubkey", &hex16(&self.anchor_noise_pubkey))
            .field("psk", &"<redacted>")
            .field("trust_domain", &self.trust_domain)
            .field("bootstrap_url", &self.bootstrap_url)
            .field("psk_expires_at", &self.psk_expires_at)
            .finish()
    }
}

fn hex16(bytes: &[u8; 32]) -> String {
    bytes[..8].iter().map(|b| format!("{b:02x}")).collect()
}

/// A browser can only fetch `https://`, with `http://localhost` (and
/// the loopback literals) as the development exception browsers
/// themselves make for secure contexts.
fn check_browser_url(url: &str) -> Result<(), BootstrapCredentialError> {
    let rest = if let Some(rest) = url.strip_prefix("https://") {
        rest
    } else if let Some(rest) = url.strip_prefix("http://") {
        let host = rest.split(['/', ':']).next().unwrap_or("");
        if host != "localhost" && host != "127.0.0.1" && host != "[::1]" {
            return Err(BootstrapCredentialError::BadUrl(
                "plain http is only a secure context on localhost",
            ));
        }
        rest
    } else {
        return Err(BootstrapCredentialError::BadUrl("not an http(s) URL"));
    };
    if rest.is_empty() || rest.starts_with('/') {
        return Err(BootstrapCredentialError::BadUrl("no host"));
    }
    Ok(())
}

/// Append a `u32`-length-prefixed byte field (the crate's wire idiom).
fn push_lp(buf: &mut Vec<u8>, bytes: &[u8]) {
    buf.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    buf.extend_from_slice(bytes);
}

/// Bounds-checked forward cursor, mirroring `enrollment`'s.
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

    fn done(&self) -> bool {
        self.pos == self.buf.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Identity;

    const T0: u64 = 1_700_000_000;

    fn credential_at(now: u64) -> BrowserBootstrapCredential {
        let root = Identity::generate().entity_id().clone();
        let invite = InviteToken::mint_at(
            &root,
            "https://anchor.example/rtc",
            Duration::from_secs(600),
            now,
        );
        BrowserBootstrapCredential::mint_at(
            invite,
            [7u8; 32],
            Psk::new([9u8; 32]),
            "https://anchor.example/rtc",
            Duration::from_secs(30 * 86_400),
            now,
        )
    }

    #[test]
    fn a_credential_round_trips_through_bytes_and_the_string_form() {
        let credential = credential_at(T0);
        let via_bytes = BrowserBootstrapCredential::from_bytes(&credential.to_bytes())
            .expect("the canonical form parses");
        assert_eq!(via_bytes, credential);
        let via_string =
            BrowserBootstrapCredential::decode(&credential.encode()).expect("the string parses");
        assert_eq!(via_string, credential);
        assert!(credential.encode().starts_with("net-bootstrap:"));
    }

    #[test]
    fn every_tampered_byte_is_refused_or_changes_the_credential() {
        // A format with no signature of its own must still be
        // tamper-*evident* to its consumer: any single-byte edit must
        // either fail to parse or produce a credential that differs
        // from the minted one (so the anchor key, PSK, domain, URL or
        // deadline a consumer acts on is never silently substituted).
        let credential = credential_at(T0);
        let bytes = credential.to_bytes();
        for i in 0..bytes.len() {
            let mut tampered = bytes.clone();
            tampered[i] ^= 0x40;
            match BrowserBootstrapCredential::from_bytes(&tampered) {
                Err(_) => {}
                Ok(parsed) => assert_ne!(
                    parsed, credential,
                    "byte {i} was flipped and the credential parsed identically"
                ),
            }
        }
    }

    #[test]
    fn a_truncated_credential_is_refused_at_every_length() {
        let credential = credential_at(T0);
        let bytes = credential.to_bytes();
        for cut in 0..bytes.len() {
            assert!(
                BrowserBootstrapCredential::from_bytes(&bytes[..cut]).is_err(),
                "a {cut}-byte prefix must not parse"
            );
        }
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(BrowserBootstrapCredential::from_bytes(&trailing).is_err());
    }

    #[test]
    fn the_two_lifetimes_expire_independently_and_say_which() {
        let credential = credential_at(T0);
        credential.validate_at(T0).expect("fresh");

        // The single-use half goes first: the nonce is minutes, the
        // PSK is days.
        let after_nonce = T0 + 601;
        assert!(matches!(
            credential.validate_at(after_nonce),
            Err(BootstrapCredentialError::NonceExpired { .. })
        ));

        // A credential whose nonce is still live but whose standing
        // half has been rotated out reports the PSK, not the nonce.
        let long_invite = InviteToken::mint_at(
            credential.root(),
            "https://anchor.example/rtc",
            Duration::from_secs(90 * 86_400),
            T0,
        );
        let short_psk = BrowserBootstrapCredential::mint_at(
            long_invite,
            [7u8; 32],
            Psk::new([9u8; 32]),
            "https://anchor.example/rtc",
            Duration::from_secs(60),
            T0,
        );
        assert!(matches!(
            short_psk.validate_at(T0 + 61),
            Err(BootstrapCredentialError::PskExpired { .. })
        ));
    }

    #[test]
    fn a_credential_from_another_trust_domain_is_refused() {
        let credential = credential_at(T0);
        let ours = Psk::new([1u8; 32]);
        let err = credential
            .check_trust_domain(&ours)
            .expect_err("a foreign domain must be refused");
        assert!(matches!(
            err,
            BootstrapCredentialError::WrongTrustDomain { .. }
        ));
        // …and the anchor that actually holds the PSK accepts it.
        credential
            .check_trust_domain(&Psk::new([9u8; 32]))
            .expect("our own domain");
    }

    #[test]
    fn a_credential_whose_domain_id_is_not_its_psks_is_malformed() {
        let mut credential = credential_at(T0);
        credential.trust_domain = Psk::new([1u8; 32]).trust_domain();
        assert!(matches!(
            BrowserBootstrapCredential::from_bytes(&credential.to_bytes()),
            Err(BootstrapCredentialError::TrustDomainMismatch)
        ));
        assert!(matches!(
            credential.validate_at(T0),
            Err(BootstrapCredentialError::TrustDomainMismatch)
        ));
    }

    #[test]
    fn the_psk_never_appears_in_debug_output() {
        let credential = credential_at(T0);
        let rendered = format!("{credential:?}");
        let psk_hex: String = credential
            .psk
            .expose_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert!(!rendered.contains(&psk_hex), "the PSK leaked into Debug");
        // Not even one byte of it, in any of the shapes a formatter
        // might produce.
        assert!(!rendered.contains("[9, 9"), "the PSK leaked as a slice");
        assert!(!rendered.contains("999999"), "the PSK leaked as hex");
        assert!(rendered.contains("<redacted>"));
        assert_eq!(format!("{:?}", credential.psk), "Psk(<redacted>)");
        // The rest is still diagnosable.
        assert!(rendered.contains("https://anchor.example/rtc"));
        assert!(rendered.contains(&credential.trust_domain.to_string()));
    }

    #[test]
    fn a_url_a_browser_cannot_fetch_is_refused() {
        let credential = credential_at(T0);
        for bad in [
            "http://anchor.example/rtc",
            "ws://anchor.example/rtc",
            "anchor.example/rtc",
            "https://",
        ] {
            let mut c = credential.clone();
            c.bootstrap_url = bad.to_string();
            assert!(
                matches!(
                    BrowserBootstrapCredential::from_bytes(&c.to_bytes()),
                    Err(BootstrapCredentialError::BadUrl(_))
                ),
                "{bad} must be refused"
            );
        }
        // The localhost development exception browsers themselves make.
        let mut local = credential.clone();
        local.bootstrap_url = "http://localhost:8443/rtc".to_string();
        BrowserBootstrapCredential::from_bytes(&local.to_bytes()).expect("localhost is allowed");
    }

    #[test]
    fn a_future_version_is_refused_by_number_not_by_magic() {
        let credential = credential_at(T0);
        let mut bytes = credential.to_bytes();
        bytes[4] = 2;
        assert!(matches!(
            BrowserBootstrapCredential::from_bytes(&bytes),
            Err(BootstrapCredentialError::UnsupportedVersion(2))
        ));
    }

    #[test]
    fn an_invite_string_is_not_a_credential_string() {
        let root = Identity::generate().entity_id().clone();
        let invite = InviteToken::mint_at(&root, "rz", Duration::from_secs(60), T0);
        assert!(BrowserBootstrapCredential::decode(&invite.encode()).is_err());
        assert!(InviteToken::decode(&credential_at(T0).encode()).is_err());
    }
}
