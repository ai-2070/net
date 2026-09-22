//! Layer 0: the credential, the bootstrap listener, and the STUN
//! probe that is the only thing allowed to turn `UdpBlocked` on.
//!
//! # What the credential is for
//!
//! The leaf runs its handshake against
//! [`Credential::anchor_noise_pubkey`] — **the key the credential
//! pins** — and never against the key `GET /rtc/anchor` returns. The
//! live key is fetched and *compared*; a mismatch is refused. A
//! browser that trusted the HTTP response instead would have no MITM
//! protection at all, which is exactly what the 4b MITM witness
//! exercises.
//!
//! # Only this file may classify `UdpBlocked`
//!
//! `stun_probe_failed` is the second half of the evidence
//! [`UdpBlockedEvidence`] requires. A browser cannot send a raw STUN
//! datagram, so the probe is the one form available to it: a
//! throwaway `RTCPeerConnection` whose *only* ICE server is the
//! anchor's published `rtc_addr`, gathering until a deadline. A
//! `srflx` candidate means a STUN binding response came back over
//! UDP. No `srflx` within the deadline, while the HTTPS bootstrap to
//! the same anchor succeeded, is the one observation that
//! distinguishes blocked UDP from an anchor that is simply not
//! there. Everything weaker stays
//! [`RtcError::IceTimeout`].

//!
//! Everything here except the probe itself is pure and tested
//! natively: the credential decoder, the anchor-info parser, the
//! pinned-key check and the classification. The probe needs a real
//! `RTCPeerConnection`, so it is `wasm32`-only and is exercised by
//! the browser matrix.

use base64::Engine as _;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::JsCast;
#[cfg(target_arch = "wasm32")]
use web_sys::{RtcConfiguration, RtcDataChannelInit, RtcPeerConnection, RtcPeerConnectionIceEvent};
use zeroize::Zeroize;

use crate::control_plane::NodeId;
use crate::enroll::Invite;
use crate::error::{LeafError, Result, RtcError, UdpBlockedEvidence};

/// The prefix on the credential string handed to JavaScript.
const CREDENTIAL_PREFIX: &str = "net-bootstrap:";

/// Magic at the head of the credential's byte form.
const CREDENTIAL_MAGIC: [u8; 4] = *b"NMBC";

/// The credential format version this build speaks.
pub const CREDENTIAL_VERSION: u8 = 2;

/// How long the STUN probe waits for a server-reflexive candidate.
///
/// Two seconds: S0b measured offer → DataChannel-open well inside
/// that, and a probe that waited longer would delay the typed
/// failure the whole correction exists to produce promptly.
pub const STUN_PROBE_MS: i32 = 2_000;

/// What the leaf needs out of a `BrowserBootstrapCredential`.
///
/// Only the fields a leaf uses are kept. The invite blob and the
/// issuer signature are carried opaquely: the **anchor** verifies
/// the issuer signature (the recipient holds no key that could
/// forge one, which is the point), and the leaf's job is to present
/// the string unmodified.
#[derive(Clone)]
pub struct Credential {
    /// The whole credential string, presented verbatim to
    /// `POST /rtc/offer`.
    pub encoded: String,
    /// The invite whose nonce the enrollment request echoes.
    ///
    /// The first pass treated this blob as opaque, which is what
    /// left every session provisional: the enrollment request has to
    /// echo `nonce` and bind `root` into its signature, so a leaf
    /// that cannot read the invite cannot be promoted.
    pub invite: Invite,
    /// The anchor's pinned Noise static public key.
    pub anchor_noise_pubkey: [u8; 32],
    /// The trust domain's NKpsk0 pre-shared key.
    pub psk: [u8; 32],
    /// The bootstrap listener's `https://` base URL.
    pub bootstrap_url: String,
    /// Unix seconds after which the standing PSK half is dead.
    pub psk_expires_at: u64,
}

/// Redacting `Debug` (Stage 5 secondary audit).
///
/// The derived one printed `encoded` — the whole bearer credential —
/// and `psk`, the trust domain's pre-shared key, into any error or
/// trace that formatted a `Credential`. Stage 4b gave `OfferRequest`
/// and `OfferResponse` the same treatment for the same reason: a
/// bearer secret has no business in a log line, and the browser's
/// console is a log line anyone with the page can read.
impl core::fmt::Debug for Credential {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Credential")
            .field("encoded", &"<redacted>")
            .field("psk", &"<redacted>")
            .field("invite", &self.invite)
            .field(
                "anchor_noise_pubkey",
                &crate::identity::hex_lower(&self.anchor_noise_pubkey),
            )
            .field("bootstrap_url", &self.bootstrap_url)
            .field("psk_expires_at", &self.psk_expires_at)
            .finish()
    }
}

/// Zeroize the three live secrets on drop (M50): the trust domain's
/// PSK, the whole bearer string, and the invite nonce the enrollment
/// request echoes as proof-of-invite.
///
/// Same honest caveat as `IdentitySecrets`' `Drop`: `zeroize` is a
/// volatile write and a compiler fence, so these bytes are gone from
/// *this* allocation. On wasm that is all there is — the browser
/// never returns linear memory to the OS, so a secret left in freed
/// wasm memory stays readable for the page's remaining life. This
/// says nothing about copies the JavaScript heap made before wasm
/// ever saw the bytes.
impl Drop for Credential {
    fn drop(&mut self) {
        self.psk.zeroize();
        self.encoded.zeroize();
        self.invite.nonce.zeroize();
    }
}

impl Credential {
    /// Decode the `net-bootstrap:` string.
    ///
    /// Structure and version only. The two lifetimes are checked by
    /// [`Self::validate_at`], because a caller inspecting an expired
    /// credential still wants to see its fields — the same split the
    /// SDK makes.
    pub fn decode(s: &str) -> Result<Self> {
        let body = s
            .trim()
            .strip_prefix(CREDENTIAL_PREFIX)
            .ok_or_else(|| malformed("missing the net-bootstrap: prefix"))?;
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(body)
            .map_err(|_| malformed("invalid base64"))?;

        let mut at = 0usize;
        let magic = take::<4>(&bytes, &mut at)?;
        if magic != CREDENTIAL_MAGIC {
            return Err(malformed("bad magic"));
        }
        let version = take::<1>(&bytes, &mut at)?[0];
        if version != CREDENTIAL_VERSION {
            return Err(malformed(&format!(
                "credential format version {version} is not supported \
                 (this build speaks {CREDENTIAL_VERSION})"
            )));
        }
        // The invite blob, length-prefixed.
        let invite_len = u32::from_le_bytes(take::<4>(&bytes, &mut at)?) as usize;
        let invite_end = at
            .checked_add(invite_len)
            .filter(|end| *end <= bytes.len())
            .ok_or_else(|| malformed("truncated invite"))?;
        let invite = Invite::decode(&bytes[at..invite_end])?;
        at = invite_end;

        let anchor_noise_pubkey = take::<32>(&bytes, &mut at)?;
        let psk = take::<32>(&bytes, &mut at)?;
        let _trust_domain = take::<16>(&bytes, &mut at)?;
        let psk_expires_at = u64::from_le_bytes(take::<8>(&bytes, &mut at)?);
        let url_len = u32::from_le_bytes(take::<4>(&bytes, &mut at)?) as usize;
        let url_end = at
            .checked_add(url_len)
            .filter(|end| *end <= bytes.len())
            .ok_or_else(|| malformed("truncated URL"))?;
        let bootstrap_url = core::str::from_utf8(&bytes[at..url_end])
            .map_err(|_| malformed("non-UTF-8 URL"))?
            .to_string();
        at = url_end;
        let _issuer = take::<32>(&bytes, &mut at)?;
        let _signature = take::<64>(&bytes, &mut at)?;
        if at != bytes.len() {
            return Err(malformed("trailing bytes"));
        }
        if !bootstrap_url.starts_with("https://") && !is_loopback_bootstrap_url(&bootstrap_url) {
            return Err(malformed(
                "the bootstrap URL must be https:// (or http:// on a loopback host — \
                 localhost, 127.0.0.1 or [::1] — for a harness)",
            ));
        }
        Ok(Self {
            encoded: s.trim().to_string(),
            invite,
            anchor_noise_pubkey,
            psk,
            bootstrap_url,
            psk_expires_at,
        })
    }

    /// Refuse a credential whose standing PSK half has expired.
    ///
    /// The invite's own single-use deadline is checked separately by
    /// [`Invite::validate_at`], at the point enrollment needs it —
    /// the two lifetimes are independent, and a credential whose
    /// invite has been redeemed still bootstraps a transport.
    pub fn validate_at(&self, now_unix_secs: u64) -> Result<()> {
        if self.psk_expires_at < now_unix_secs {
            return Err(LeafError::ControlPlane(format!(
                "the credential's PSK expired at {} (now {now_unix_secs})",
                self.psk_expires_at
            )));
        }
        Ok(())
    }
}

/// The one cleartext exception: `http://` on a **loopback host**, for
/// a harness. The URL is parsed and the HOST matched (M15).
///
/// The prefix shape this replaces — `url.starts_with("http://localhost")`
/// — named a URL prefix while claiming to name a host:
/// `http://localhost.attacker.example/rtc` and
/// `http://localhost@evil.example/` both passed it (in the second,
/// `localhost` is merely userinfo), and the leaf then POSTed the
/// whole bearer credential — PSK, invite nonce, issuer signature —
/// over cleartext HTTP to the attacker's host. A hostname checked as
/// a string prefix is the same defect class as a hostname prefix
/// check in TLS validation. `Credential::decode` verifies no issuer
/// signature (the anchor does that), so this check is the only thing
/// between a phished credential string and a cleartext bearer POST.
///
/// The parsing deliberately mirrors what a browser does with the
/// authority:
///
/// - the authority ends at `/`, `?`, `#` — **and `\`**, which the
///   WHATWG URL parser every browser runs folds into `/`. Without
///   that delimiter `http://evil\@localhost/` would read as host
///   `localhost` here while the browser connects to `evil`;
/// - userinfo runs to the LAST `@`, exactly as a browser splits it,
///   so `http://localhost@evil.example/` names host `evil.example`;
/// - the host must be `localhost`, `127.0.0.1` or `[::1]` — the
///   literal `[::1]` only, so a bracketed spelling of another
///   address, or an unbracketed IPv6, is refused rather than guessed
///   at.
///
/// **Port policy: any well-formed port, or none.** An absent port is
/// the scheme default (80); a named port must be decimal digits
/// within `u16`. The port cannot widen the exception — every port on
/// a loopback host is one trust domain, because the cleartext never
/// leaves the machine — so `http://localhost:8080/rtc` and
/// `http://localhost` are both admitted while
/// `http://localhost:8080@evil.example/` is not (its host is
/// `evil.example`).
fn is_loopback_bootstrap_url(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("http://") else {
        return false;
    };
    let authority = rest.split(['/', '\\', '?', '#']).next().unwrap_or("");
    let host = authority.rsplit('@').next().unwrap_or("");
    let (host, port) = if host.starts_with('[') {
        let Some(end) = host.find(']') else {
            return false;
        };
        let tail = &host[end + 1..];
        let port = if tail.is_empty() {
            None
        } else {
            let Some(port) = tail.strip_prefix(':') else {
                return false;
            };
            Some(port)
        };
        (&host[..=end], port)
    } else {
        match host.split_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (host, None),
        }
    };
    if let Some(port) = port {
        if port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit()) {
            return false;
        }
        if port.parse::<u16>().is_err() {
            return false;
        }
    }
    matches!(host, "[::1]") || host == "127.0.0.1" || host.eq_ignore_ascii_case("localhost")
}

/// What `GET /rtc/anchor` publishes.
#[derive(Debug, Clone)]
pub struct AnchorInfo {
    /// The anchor's node id.
    pub node_id: NodeId,
    /// Its live Noise static public key — for **comparison** with
    /// the credential's pinned key, never as a source of it.
    pub noise_pubkey: [u8; 32],
    /// Its public RTC socket, when the operator configured one.
    /// The diagnostic STUN probe's only legitimate target — and
    /// **not** an `iceServers` entry for a connection with this
    /// anchor, which is what [`Self::stun_addr`] is for.
    pub rtc_addr: Option<String>,
    /// The **separately announced** STUN endpoint, when the anchor
    /// announced one: a second, distinct UDP endpoint that answers
    /// STUN for connections pairing with this anchor.
    ///
    /// Absent on an anchor that predates the field or configures
    /// nothing, which parses as `None` and leaves the leaf
    /// configuring no ICE servers at all.
    pub stun_addr: Option<String>,
}

impl AnchorInfo {
    /// Parse the JSON body.
    pub fn from_json(body: &str) -> Result<Self> {
        let document: serde_json::Value =
            serde_json::from_str(body).map_err(|e| malformed(&format!("anchor info: {e}")))?;
        let node_id = document
            .get("node_id")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| malformed("anchor info carries no node_id"))?;
        let node_id = parse_node_id(node_id)
            .ok_or_else(|| malformed("anchor info's node_id is not a u64"))?;
        let noise_pubkey = document
            .get("noise_pubkey")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| malformed("anchor info carries no noise_pubkey"))?;
        let noise_pubkey: [u8; 32] = crate::identity::unhex(noise_pubkey)?
            .try_into()
            .map_err(|_| malformed("noise_pubkey is not 32 bytes"))?;
        Ok(Self {
            node_id,
            noise_pubkey,
            rtc_addr: document
                .get("rtc_addr")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            stun_addr: document
                .get("stun_addr")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
        })
    }

    /// The pin check: the live key must be the credential's key.
    ///
    /// Refusing here is what makes the MITM witness meaningful — an
    /// impostor serving its own key fails *before* a handshake is
    /// attempted, and the failure names the reason.
    pub fn check_pinned_key(&self, credential: &Credential) -> Result<()> {
        if self.noise_pubkey != credential.anchor_noise_pubkey {
            return Err(LeafError::ControlPlane(
                "the anchor's live Noise key is not the key this credential pins — \
                 refusing before any handshake"
                    .into(),
            ));
        }
        Ok(())
    }
}

/// Decimal or `0x`-prefixed hex, matching the listener's parser.
pub fn parse_node_id(raw: &str) -> Option<NodeId> {
    let raw = raw.trim();
    match raw.strip_prefix("0x").or_else(|| raw.strip_prefix("0X")) {
        Some(hex) => u64::from_str_radix(hex, 16).ok(),
        None => raw.parse().ok(),
    }
}

/// What `POST /rtc/offer` answers.
///
/// Redacting `Debug` (M48): `attempt_token` is a per-dialog bearer —
/// the trickle socket's WebSocket subprotocol — and anyone holding it
/// can inject `type:"candidate"` frames into this leaf's ICE. The
/// first `map_err`/debug-log message that formats this struct must
/// not hand it to a log line anyone with the page can read. Same
/// treatment, same reason, as `OfferRequest`/`OfferResponse` and
/// [`Credential`] — and that treatment covers `sdp` too: the anchor's
/// SDP answer carries `a=ice-ufrag`/`a=ice-pwd`, the dialog's STUN
/// short-term credentials, and a holder can forge binding requests
/// that pass this dialog's MESSAGE-INTEGRITY. So, exactly as both
/// named siblings do, the body is never printed — only its LENGTH,
/// which is what a diagnosis needs.
#[derive(Clone)]
pub struct OfferAccepted {
    /// The attempt token, presented as the trickle socket's
    /// WebSocket subprotocol.
    pub attempt_token: String,
    /// The dialog this attempt runs under. Never 0 — see
    /// [`Self::from_json`].
    pub dialog: u64,
    /// The anchor's SDP answer.
    pub sdp: String,
}

impl core::fmt::Debug for OfferAccepted {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("OfferAccepted")
            .field("attempt_token", &"<redacted>")
            .field("dialog", &self.dialog)
            .field("sdp_bytes", &self.sdp.len())
            .finish()
    }
}

impl OfferAccepted {
    /// Parse the JSON body.
    pub fn from_json(body: &str) -> Result<Self> {
        let document: serde_json::Value =
            serde_json::from_str(body).map_err(|e| malformed(&format!("offer response: {e}")))?;
        let field = |name: &str| -> Result<String> {
            document
                .get(name)
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| malformed(&format!("offer response carries no {name}")))
        };
        let dialog = document
            .get("dialog")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| malformed("offer response carries no dialog"))?;
        if dialog == 0 {
            // 0 is the driver's nothing-to-hand-back sentinel (`if
            // dialog != 0 { end_attempt(dialog) }`). A listener that
            // numbered its first dialog 0 would otherwise be accepted
            // here and never ended: the trickle socket stays open and
            // the anchor holds the attempt to its own deadline.
            return Err(malformed(
                "offer response carries dialog 0, which is reserved as the \
                 no-attempt sentinel",
            ));
        }
        Ok(Self {
            attempt_token: field("attempt_token")?,
            dialog,
            sdp: field("sdp")?,
        })
    }
}

/// Run the STUN probe against `rtc_addr`.
///
/// `true` means no server-reflexive candidate came back inside
/// [`STUN_PROBE_MS`] — i.e. the STUN binding went unanswered.
///
/// Paired with a *successful* HTTPS bootstrap to the same anchor by
/// [`classify_ice_failure`], and only then.
#[cfg(target_arch = "wasm32")]
pub async fn stun_probe_failed(rtc_addr: &str) -> bool {
    let Ok(probe) = build_probe(rtc_addr) else {
        // No probe means no evidence, which means no `UdpBlocked`.
        return false;
    };
    let deadline = gloo_timer_sleep(STUN_PROBE_MS);
    // A rejected timer promise cannot happen; either way the
    // deadline has elapsed by the time we are here.
    let _ = deadline.await;
    probe.connection.close();
    // Detach before dropping, so a queued ICE event cannot reach a
    // dropped `Closure`. With the connection closed and the handler
    // cleared, the probe's callback drops here with the handle that
    // owned it — nothing is leaked and nothing outlives the probe.
    probe.connection.set_onicecandidate(None);
    let failed = !probe.saw_srflx.get();
    drop(probe.on_icecandidate);
    failed
}

/// The corrected classification.
///
/// The default is [`RtcError::IceTimeout`], deliberately weaker than
/// "UDP is blocked": an anchor that is down, wrong or saturated
/// produces the identical symptom.
/// [`RtcError::UdpBlocked`] is returned **only** when both
/// observations hold, and it carries them.
pub fn classify_ice_failure(
    bootstrap_ok: bool,
    probe_failed: bool,
    probed: Option<&str>,
) -> RtcError {
    match probed {
        Some(addr) => match UdpBlockedEvidence::new(bootstrap_ok, probe_failed, addr) {
            Some(evidence) => RtcError::udp_blocked(evidence),
            None => RtcError::IceTimeout,
        },
        // Nothing to probe means nothing is established.
        None => RtcError::IceTimeout,
    }
}

/// The IANA STUN port, which a `stun:` URL with no port means.
pub const DEFAULT_STUN_PORT: u16 = 3478;

/// The `iceServers` URL a leaf defaults to for a connection with an
/// anchor that announced `announced` as its STUN endpoint.
///
/// `None` when nothing was announced — an anchor that predates the
/// field, or one that configures no second socket. The leaf then
/// configures **no** ICE servers at all, which is byte-for-byte the
/// pre-Stage-6 behaviour.
///
/// It never guesses: not a port adjacent to `rtc_addr`, and never
/// `rtc_addr` itself, which is the ICE peer rather than a STUN
/// server for the connection it is a party to.
pub fn default_stun_url(announced: Option<&str>) -> Option<String> {
    let announced = announced?.trim();
    if announced.is_empty() {
        return None;
    }
    if stun_endpoint(announced).is_some() {
        return Some(announced.to_string());
    }
    Some(format!("stun:{announced}"))
}

/// Refuse an `iceServers` configuration that aims a STUN URL at the
/// peer of the connection being established.
///
/// `urls` is every URL of every caller-supplied entry, in the order
/// the caller gave them; `peer_rtc_addr` is the RTC endpoint of the
/// peer **this** connection pairs with. The comparison is
/// deliberately connection-specific: an anchor may legitimately
/// serve STUN to a browser ↔ browser connection it is not a party
/// to, so a global blocklist would refuse a working configuration.
///
/// Returns [`LeafError::IceServerConflictsWithPeer`] on the first
/// conflicting entry, and it is the caller's job to return it
/// **before any ICE work** — the whole value of the check is that it
/// replaces an ICE deadline with a sentence.
///
/// **Detection is endpoint equality**, after default-port
/// normalisation (`stun:h` and `stun:h:3478` are the same endpoint,
/// `stun:[::1]:9` and `[::1]:9` are the same endpoint) and, for a
/// numeric address, after **parsing** it. A URL
/// naming a DNS alias of the peer is **not** detected: the leaf
/// resolves no names, and the announced STUN endpoint is what makes
/// detection unnecessary for the configuration Net supplies.
pub fn check_ice_servers_against_peer<'a>(
    urls: impl IntoIterator<Item = &'a str>,
    peer_rtc_addr: Option<&str>,
) -> Result<()> {
    // No published peer endpoint is nothing to compare against —
    // a browser peer has no RTC address at all.
    let Some(peer_rtc_addr) = peer_rtc_addr else {
        return Ok(());
    };
    let peer = EndpointKey::of(peer_rtc_addr);
    for url in urls {
        let Some(endpoint) = stun_endpoint(url) else {
            // A `turn:`/`turns:` relay is a different role and a
            // different contract; this check is about STUN.
            continue;
        };
        if EndpointKey::of(endpoint) == peer {
            return Err(LeafError::IceServerConflictsWithPeer {
                entry: url.to_string(),
                peer_rtc_addr: peer_rtc_addr.to_string(),
            });
        }
    }
    Ok(())
}

/// The `host:port` of a `stun:`/`stuns:` URL, or `None` when the URL
/// is not one.
fn stun_endpoint(url: &str) -> Option<&str> {
    let url = url.trim();
    let (scheme, rest) = url.split_once(':')?;
    if !scheme.eq_ignore_ascii_case("stun") && !scheme.eq_ignore_ascii_case("stuns") {
        return None;
    }
    // RFC 7064 gives a `stun:` URI no query component, but an engine
    // that tolerates one must not be able to smuggle the peer past
    // this comparison.
    Some(rest.split('?').next().unwrap_or(rest))
}

/// `host:port` with the IANA STUN port supplied when the endpoint
/// carries none, and an unbracketed IPv6 literal bracketed, so two
/// spellings of one endpoint compare equal.
fn normalized_endpoint(raw: &str) -> String {
    let raw = raw.trim();
    if raw.starts_with('[') {
        return match raw.split_once(']') {
            Some((host, tail)) if tail.len() > 1 && tail.starts_with(':') => {
                format!("{host}]{tail}")
            }
            Some((host, _)) => format!("{host}]:{DEFAULT_STUN_PORT}"),
            // Unterminated: not an endpoint, and comparing it
            // verbatim is the one disposition that cannot invent a
            // match.
            None => raw.to_string(),
        };
    }
    // A bare IPv6 literal has more than one colon and no brackets,
    // and carries no port — an unbracketed one cannot be told from
    // the address.
    if raw.matches(':').count() > 1 {
        return format!("[{raw}]:{DEFAULT_STUN_PORT}");
    }
    match raw.split_once(':') {
        Some((_, port)) if !port.is_empty() => raw.to_string(),
        _ => format!("{raw}:{DEFAULT_STUN_PORT}"),
    }
}

/// One endpoint, in the form two spellings of it compare equal in.
///
/// # Why a parse and not a string
///
/// [`normalized_endpoint`] makes `stun:h` and `stun:h:3478` and
/// `[::1]:9` and `::1` agree, and it has to: they are textual
/// differences. It cannot make `[2001:db8::1]:3478` agree with
/// `[2001:0db8:0000:0000:0000:0000:0000:0001]:3478`, because those
/// differ in the address itself and RFC 4291 gives one address many
/// legal spellings — compression, leading-zero omission, and
/// `::ffff:192.0.2.1` for an IPv4-mapped tuple. No amount of text
/// rewriting enumerates them, and every one that escapes is an
/// `iceServers` entry aimed at the peer of the connection: the
/// deadline this check exists to replace with a sentence.
///
/// So a numeric endpoint is compared as the `SocketAddr` it
/// parses to, and a name is compared as normalised text. A name and
/// an address never compare equal, which is right: the leaf
/// resolves nothing, so it does not know whether they name one
/// endpoint, and inventing a match is the one disposition that
/// could refuse a working configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
enum EndpointKey {
    /// A numeric `ip:port`, parsed — so every legal spelling of one
    /// address is one key.
    Addr(std::net::SocketAddr),
    /// A DNS name (or something unparseable), normalised textually.
    Name(String),
}

impl EndpointKey {
    /// The key `raw` compares under.
    fn of(raw: &str) -> Self {
        let normalized = normalized_endpoint(raw);
        match normalized.parse::<std::net::SocketAddr>() {
            Ok(addr) => Self::Addr(addr),
            Err(_) => Self::Name(normalized),
        }
    }
}

/// A throwaway peer connection whose only ICE server is `rtc_addr`,
/// with a flag that flips when a `srflx` candidate appears.
///
/// The ICE callback is owned here — not `forget`ten — so it drops
/// with the probe handle (L107). The caller must detach it from the
/// connection before the handle drops.
#[cfg(target_arch = "wasm32")]
struct StunProbe {
    connection: RtcPeerConnection,
    saw_srflx: std::rc::Rc<core::cell::Cell<bool>>,
    on_icecandidate: Closure<dyn FnMut(RtcPeerConnectionIceEvent)>,
}

#[cfg(target_arch = "wasm32")]
fn build_probe(rtc_addr: &str) -> Result<StunProbe> {
    let servers = js_sys::Array::new();
    let server = js_sys::Object::new();
    let urls = js_sys::Array::new();
    urls.push(&JsValue::from_str(&format!("stun:{rtc_addr}")));
    js_sys::Reflect::set(&server, &JsValue::from_str("urls"), &urls)
        .map_err(|_| malformed("could not build the probe's iceServers"))?;
    servers.push(&server);

    let config = RtcConfiguration::new();
    config.set_ice_servers(&servers);
    let connection = RtcPeerConnection::new_with_configuration(&config)
        .map_err(|_| malformed("no RTCPeerConnection for the probe"))?;

    let saw_srflx = std::rc::Rc::new(core::cell::Cell::new(false));
    let flag = std::rc::Rc::clone(&saw_srflx);
    let closure = Closure::wrap(Box::new(move |event: RtcPeerConnectionIceEvent| {
        if let Some(candidate) = event.candidate() {
            if candidate.candidate().contains("typ srflx") {
                flag.set(true);
            }
        }
    }) as Box<dyn FnMut(RtcPeerConnectionIceEvent)>);
    connection.set_onicecandidate(Some(closure.as_ref().unchecked_ref()));

    // A channel is what makes the browser gather at all.
    let init = RtcDataChannelInit::new();
    init.set_ordered(true);
    let _ = connection.create_data_channel_with_data_channel_dict("probe", &init);
    let offer = connection.create_offer();
    let connection_for_offer = connection.clone();
    wasm_bindgen_futures::spawn_local(async move {
        if let Ok(offer) = wasm_bindgen_futures::JsFuture::from(offer).await {
            if let Some(sdp) = js_sys::Reflect::get(&offer, &JsValue::from_str("sdp"))
                .ok()
                .and_then(|v| v.as_string())
            {
                let description =
                    web_sys::RtcSessionDescriptionInit::new(web_sys::RtcSdpType::Offer);
                description.set_sdp(&sdp);
                let _ = wasm_bindgen_futures::JsFuture::from(
                    connection_for_offer.set_local_description(&description),
                )
                .await;
            }
        }
    });
    Ok(StunProbe {
        connection,
        saw_srflx,
        on_icecandidate: closure,
    })
}

/// `setTimeout` as a future. No `tokio`, no `gloo` dependency — the
/// leaf's only timer.
#[cfg(target_arch = "wasm32")]
pub fn gloo_timer_sleep(millis: i32) -> wasm_bindgen_futures::JsFuture {
    let promise = js_sys::Promise::new(&mut |resolve, _reject| {
        if let Some(window) = web_sys::window() {
            let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, millis);
        }
    });
    wasm_bindgen_futures::JsFuture::from(promise)
}

fn take<const N: usize>(bytes: &[u8], at: &mut usize) -> Result<[u8; N]> {
    let end = at.checked_add(N).ok_or_else(|| malformed("truncated"))?;
    let slice = bytes.get(*at..end).ok_or_else(|| malformed("truncated"))?;
    let mut out = [0u8; N];
    out.copy_from_slice(slice);
    *at = end;
    Ok(out)
}

fn malformed(what: &str) -> LeafError {
    LeafError::ControlPlane(format!("bootstrap credential: {what}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a credential's byte form the way the SDK's `to_bytes`
    /// does, so the decoder is tested against the real layout
    /// rather than against itself.
    fn credential_string(url: &str, psk_expires_at: u64) -> String {
        let mut buf = Vec::new();
        buf.extend_from_slice(&CREDENTIAL_MAGIC);
        buf.push(CREDENTIAL_VERSION);
        let mut invite = Vec::new();
        invite.extend_from_slice(b"NMI1");
        invite.extend_from_slice(&[0x77u8; 32]); // root
        invite.extend_from_slice(&[0x5Au8; 16]); // nonce
        invite.extend_from_slice(&2_000_000_000u64.to_le_bytes());
        invite.extend_from_slice(&(3u32).to_le_bytes());
        invite.extend_from_slice(b"rdv");
        buf.extend_from_slice(&(invite.len() as u32).to_le_bytes());
        buf.extend_from_slice(&invite);
        buf.extend_from_slice(&[0xA1u8; 32]); // anchor noise pubkey
        buf.extend_from_slice(&[0x4Bu8; 32]); // psk
        buf.extend_from_slice(&[0x77u8; 16]); // trust domain
        buf.extend_from_slice(&psk_expires_at.to_le_bytes());
        buf.extend_from_slice(&(url.len() as u32).to_le_bytes());
        buf.extend_from_slice(url.as_bytes());
        buf.extend_from_slice(&[0x11u8; 32]); // issuer
        buf.extend_from_slice(&[0x22u8; 64]); // signature
        format!(
            "{CREDENTIAL_PREFIX}{}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&buf)
        )
    }

    #[test]
    fn a_credential_decodes_to_the_pinned_key_the_psk_and_the_url() {
        let encoded = credential_string("https://anchor.example/rtc", 2_000_000_000);
        let credential = Credential::decode(&encoded).expect("decodes");
        assert_eq!(credential.anchor_noise_pubkey, [0xA1u8; 32]);
        assert_eq!(credential.psk, [0x4Bu8; 32]);
        assert_eq!(credential.bootstrap_url, "https://anchor.example/rtc");
        // The invite is parsed, not skipped: without its nonce and
        // root the leaf cannot build an enrollment request, and the
        // session stays provisional forever.
        assert_eq!(credential.invite.nonce, [0x5Au8; 16]);
        assert_eq!(credential.invite.root, [0x77u8; 32]);
        assert_eq!(credential.invite.rendezvous, "rdv");
        assert_eq!(
            credential.encoded, encoded,
            "the string must be presented to the listener verbatim"
        );
    }

    #[test]
    fn a_malformed_or_foreign_credential_is_refused() {
        assert!(Credential::decode("").is_err(), "empty");
        assert!(
            Credential::decode("net-bootstrap:####").is_err(),
            "invalid base64"
        );
        let good = credential_string("https://anchor.example/rtc", 2_000_000_000);
        assert!(
            Credential::decode(good.trim_start_matches("net-bootstrap:")).is_err(),
            "a missing prefix must be refused"
        );
        assert!(
            Credential::decode(&format!("{good}AA")).is_err(),
            "trailing bytes must be refused"
        );
        assert!(
            Credential::decode(&credential_string(
                "http://anchor.example/rtc",
                2_000_000_000
            ))
            .is_err(),
            "a plain-http bootstrap URL a browser could not use must be refused"
        );
        // A harness on localhost is the one exception, and it is
        // explicit rather than a hole in the check.
        assert!(Credential::decode(&credential_string(
            "http://localhost:8080/rtc",
            2_000_000_000
        ))
        .is_ok());
    }

    /// M15: the plain-http exception matches the URL's HOST, not the
    /// URL's first bytes. A prefix match here routes the whole bearer
    /// over cleartext to an attacker host.
    #[test]
    fn the_plain_http_exception_matches_the_host_not_the_url_prefix() {
        for admitted in [
            "http://localhost",
            "http://localhost:8080/rtc",
            "http://LOCALHOST:8080/rtc",
            "http://127.0.0.1:8080/rtc",
            "http://[::1]:8080/rtc",
            "https://anchor.example/rtc",
        ] {
            assert!(
                Credential::decode(&credential_string(admitted, 2_000_000_000)).is_ok(),
                "{admitted} must be admitted"
            );
        }
        for refused in [
            // The two shapes the prefix match admitted: a host that
            // merely begins with the name, and `localhost` as
            // userinfo of an attacker's host.
            "http://localhost.attacker.example/rtc",
            "http://localhost@evil.example/",
            "http://localhost:8080@evil.example/",
            // The WHATWG parser every browser runs folds `\` into
            // `/`: a browser reads this as host `evil`, path
            // `/@localhost` — a prefix check that split on `@` alone
            // would read host `localhost` and hand it the bearer.
            "http://evil\\@localhost/",
            // Percent-encoding and case do not conjure a host match.
            "http://localhost%2eevil.example/",
            "http://LOCALHOST.evil.example/",
            "http://127.0.0.1.evil.example/",
            "http://[::1].evil.example/",
            // Port policy: named ports are decimal digits within
            // u16, and a malformed port is refused, not ignored.
            "http://localhost:99999/",
            "http://localhost:/",
            "http://localhost:8080.evil.example/",
            // Only the literal `[::1]`, not another spelling of some
            // v6 address inside brackets.
            "http://[0:0:0:0:0:0:0:1]:8080/",
            // Not a URL shape this check trusts at all.
            "http:/localhost/",
            "HTTP://LOCALHOST:8080/",
        ] {
            assert!(
                Credential::decode(&credential_string(refused, 2_000_000_000)).is_err(),
                "{refused} must be refused"
            );
        }
    }

    #[test]
    fn an_expired_psk_is_refused_at_validation_not_at_parse() {
        let credential =
            Credential::decode(&credential_string("https://a.example/rtc", 1_000)).expect("parses");
        credential
            .validate_at(1_000)
            .expect("valid at the boundary");
        let err = credential.validate_at(1_001).expect_err("expired");
        assert!(format!("{err}").contains("expired"), "{err}");
    }

    #[test]
    fn anchor_info_parses_both_node_id_spellings() {
        let body = r#"{"node_id":"0xaabb","noise_pubkey":"a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1","rtc_addr":"198.51.100.7:4433","stun_addr":"198.51.100.7:3479","trust_domain":"x"}"#;
        let info = AnchorInfo::from_json(body).expect("parses");
        assert_eq!(info.node_id, 0xAABB);
        assert_eq!(info.noise_pubkey, [0xA1u8; 32]);
        assert_eq!(info.rtc_addr.as_deref(), Some("198.51.100.7:4433"));
        // Two distinct announced endpoints, read as two fields: the
        // STUN endpoint is never derived from `rtc_addr`.
        assert_eq!(info.stun_addr.as_deref(), Some("198.51.100.7:3479"));

        assert_eq!(parse_node_id("42"), Some(42));
        assert_eq!(parse_node_id("0X2A"), Some(42));
        assert_eq!(parse_node_id("nope"), None);

        let no_addr = r#"{"node_id":"1","noise_pubkey":"a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1"}"#;
        let bare = AnchorInfo::from_json(no_addr).expect("parses");
        assert_eq!(bare.rtc_addr, None);
        // An anchor that predates the field, which must parse — not
        // fail, and not default to `rtc_addr`.
        assert_eq!(bare.stun_addr, None);

        // Announced RTC endpoint, no announced STUN endpoint: the
        // one field is absent on its own.
        let rtc_only = r#"{"node_id":"1","noise_pubkey":"a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1","rtc_addr":"198.51.100.7:4433"}"#;
        let rtc_only = AnchorInfo::from_json(rtc_only).expect("parses");
        assert_eq!(rtc_only.rtc_addr.as_deref(), Some("198.51.100.7:4433"));
        assert_eq!(rtc_only.stun_addr, None);
    }

    /// The MITM refusal: the live key must be the pinned key.
    #[test]
    fn an_anchor_serving_a_different_key_is_refused_before_any_handshake() {
        let credential =
            Credential::decode(&credential_string("https://a.example/rtc", 2_000_000_000))
                .expect("parses");
        let matching = AnchorInfo {
            node_id: 1,
            noise_pubkey: [0xA1u8; 32],
            rtc_addr: None,
            stun_addr: None,
        };
        matching
            .check_pinned_key(&credential)
            .expect("the pinned key must be accepted");

        let impostor = AnchorInfo {
            node_id: 1,
            noise_pubkey: [0xBEu8; 32],
            rtc_addr: None,
            stun_addr: None,
        };
        let err = impostor
            .check_pinned_key(&credential)
            .expect_err("an impostor must be refused");
        assert!(
            format!("{err}").contains("pins"),
            "the refusal must name the reason: {err}"
        );
    }

    #[test]
    fn an_offer_response_parses_its_three_load_bearing_fields() {
        let body = r#"{"attempt_token":"deadbeef","dialog":7,"sdp":"v=0\r\n","other":1}"#;
        let accepted = OfferAccepted::from_json(body).expect("parses");
        assert_eq!(accepted.attempt_token, "deadbeef");
        assert_eq!(accepted.dialog, 7);
        assert_eq!(accepted.sdp, "v=0\r\n");

        for missing in [
            r#"{"dialog":7,"sdp":"x"}"#,
            r#"{"attempt_token":"t","sdp":"x"}"#,
            r#"{"attempt_token":"t","dialog":7}"#,
        ] {
            assert!(
                OfferAccepted::from_json(missing).is_err(),
                "a missing field must be refused, not defaulted: {missing}"
            );
        }
    }

    /// M49: dialog 0 is the driver's no-attempt sentinel and must not
    /// be parseable — a listener numbering its first dialog 0 would
    /// otherwise get `end_attempt` silently skipped on every cancel,
    /// abandon and close.
    #[test]
    fn an_offer_response_refuses_the_reserved_dialog_zero() {
        let body = r#"{"attempt_token":"t","dialog":0,"sdp":"x"}"#;
        assert!(
            OfferAccepted::from_json(body).is_err(),
            "dialog 0 must be refused at parse, not carried into `if dialog != 0` sentinels"
        );
    }

    /// M16/M48: a bearer secret has no business in a log line. The
    /// redacting `Debug`s must keep every secret out — in **both**
    /// spellings a `Debug` impl would print it (a derived `[u8; N]`
    /// renders decimal, a redaction-by-hex regression renders hex) —
    /// while still showing the fields nobody is hiding.
    #[test]
    fn a_formatted_credential_or_offer_prints_no_bearer_secret() {
        let encoded = credential_string("https://anchor.example/rtc", 2_000_000_000);
        let credential = Credential::decode(&encoded).expect("decodes");
        let text = format!("{credential:?}");
        // Positive control: it is a formatter, not a deletion.
        for visible in ["bootstrap_url", "anchor.example", "psk_expires_at", "root"] {
            assert!(text.contains(visible), "{visible} missing from {text}");
        }
        for secret in [
            // The derived-Debug spelling, which is exactly what the
            // old impls printed for `invite` and `psk`.
            format!("{:?}", credential.psk),
            format!("{:?}", credential.invite.nonce),
            // …and the hex spelling.
            crate::identity::hex_lower(&credential.psk),
            crate::identity::hex_lower(&credential.invite.nonce),
            // The whole bearer string.
            encoded.clone(),
        ] {
            assert!(
                !text.contains(secret.as_str()),
                "the redacting Credential Debug leaked a bearer secret: {text}"
            );
        }

        // The attempt token is a per-dialog bearer: holding it lets
        // anyone inject `type:"candidate"` frames into this leaf's
        // ICE.
        //
        // And the SDP body is one too: the answer carries the
        // dialog's STUN short-term credentials in `a=ice-ufrag`/
        // `a=ice-pwd`, and a holder can forge binding requests that
        // pass this dialog's MESSAGE-INTEGRITY — the same ICE-hijack
        // surface `attempt_token` names. Neither the body nor its
        // credentials may appear in the output; only its length,
        // exactly as the `sdp_bytes` of `OfferRequest`/`OfferResponse`.
        let accepted = OfferAccepted::from_json(
            r#"{"attempt_token":"deadbeef","dialog":7,"sdp":"v=0\r\na=ice-ufrag:UFRAG42\r\na=ice-pwd:PWDSUPERSECRET\r\n"}"#,
        )
        .expect("parses");
        let text = format!("{accepted:?}");
        assert!(
            !text.contains("deadbeef"),
            "the redacting OfferAccepted Debug leaked the attempt token: {text}"
        );
        for leak in [
            // The body itself…
            "v=0",
            accepted.sdp.as_str(),
            // …and both spellings of its ICE credentials: the
            // attribute lines and the bare secrets a Debug impl that
            // redacted by attribute name would still print values of.
            "ice-ufrag",
            "ice-pwd",
            "UFRAG42",
            "PWDSUPERSECRET",
        ] {
            assert!(
                !text.contains(leak),
                "the redacting OfferAccepted Debug leaked the SDP body or its ICE \
                 credentials: {text}"
            );
        }
        assert!(
            text.contains("dialog"),
            "non-secret fields stay visible: {text}"
        );
        assert!(
            text.contains("sdp_bytes"),
            "and the diagnosis half stays too: the SDP's length is printed like both \
             named siblings print it: {text}"
        );
    }

    /// The correction, as a truth table. This is the property the
    /// whole `UdpBlocked` design exists for: nothing short of both
    /// observations turns the type.
    #[test]
    fn only_both_observations_together_classify_udp_as_blocked() {
        let addr = Some("198.51.100.7:4433");
        assert!(matches!(
            classify_ice_failure(true, true, addr),
            RtcError::UdpBlocked(_)
        ));
        for (bootstrap_ok, probe_failed, probed) in [
            (true, false, addr),
            (false, true, addr),
            (false, false, addr),
            // No published rtc_addr: nothing to probe, nothing
            // established, however the other bits fall.
            (true, true, None),
        ] {
            assert_eq!(
                classify_ice_failure(bootstrap_ok, probe_failed, probed),
                RtcError::IceTimeout,
                "bootstrap_ok={bootstrap_ok} probe_failed={probe_failed} \
                 probed={probed:?} must stay IceTimeout"
            );
        }

        // And the evidence names its own subject.
        let RtcError::UdpBlocked(evidence) = classify_ice_failure(true, true, addr) else {
            panic!("expected UdpBlocked");
        };
        assert_eq!(evidence.probed, "198.51.100.7:4433");
        assert!(evidence.bootstrap_ok && evidence.stun_probe_failed);
    }

    /// The Stage 6 default: the announced endpoint, and nothing
    /// when nothing was announced.
    #[test]
    fn the_default_ice_server_is_the_announced_stun_endpoint_or_nothing() {
        assert_eq!(
            default_stun_url(Some("198.51.100.7:3479")).as_deref(),
            Some("stun:198.51.100.7:3479")
        );
        // IPv6 arrives already bracketed, the way `SocketAddr`
        // renders it, and must stay that way inside the URL.
        assert_eq!(
            default_stun_url(Some("[2001:db8::1]:3479")).as_deref(),
            Some("stun:[2001:db8::1]:3479")
        );
        // An anchor that announced a URL rather than an endpoint is
        // taken at its word, not double-prefixed.
        assert_eq!(
            default_stun_url(Some("stun:anchor.example:3479")).as_deref(),
            Some("stun:anchor.example:3479")
        );

        // Nothing announced, nothing configured: the pre-Stage-6
        // behaviour, which is an empty `iceServers`. The leaf never
        // substitutes `rtc_addr` here — that substitution is the
        // defect Stage 6 exists to remove.
        assert_eq!(default_stun_url(None), None);
        assert_eq!(default_stun_url(Some("")), None);
        assert_eq!(default_stun_url(Some("   ")), None);
    }

    /// The safeguard: the peer's own RTC endpoint configured as this
    /// connection's STUN server is refused, by name.
    #[test]
    fn a_stun_entry_naming_this_connections_peer_is_refused_with_both_endpoints_named() {
        let peer = Some("198.51.100.7:4433");
        let err = check_ice_servers_against_peer(["stun:198.51.100.7:4433"], peer)
            .expect_err("the peer's own RTC endpoint must be refused");
        assert_eq!(
            err,
            LeafError::IceServerConflictsWithPeer {
                entry: "stun:198.51.100.7:4433".into(),
                peer_rtc_addr: "198.51.100.7:4433".into(),
            }
        );
        let text = format!("{err}");
        // Descriptive, per the acceptance boundary: the conflicting
        // entry, the peer it collides with, and the way out. Pinned
        // whole, not by substring, because `@net-mesh/browser`
        // reconstructs this variant by parsing exactly this sentence
        // — `IceServerConflictError` carries a byte-identical copy,
        // and a reworded prefix here would silently demote the error
        // to `unknown` in the browser.
        assert_eq!(
            text,
            "ice configuration: the iceServers entry stun:198.51.100.7:4433 names this \
             connection's peer RTC endpoint 198.51.100.7:4433; a peer cannot be its own STUN \
             server. Omit iceServers to use the STUN endpoint the anchor announces (the \
             stun_addr field of GET /rtc/anchor), or name a STUN server that is not this peer"
        );

        // The conflicting entry is found wherever it sits, and the
        // error names *it*, not the first entry.
        let err = check_ice_servers_against_peer(
            ["stun:stun.example:3478", "stun:198.51.100.7:4433"],
            peer,
        )
        .expect_err("a later entry conflicts just as much");
        assert!(format!("{err}").contains("stun:198.51.100.7:4433"), "{err}");

        // Default-port spellings of one endpoint compare equal in
        // both directions.
        assert!(check_ice_servers_against_peer(
            ["stun:anchor.example"],
            Some("anchor.example:3478")
        )
        .is_err());
        assert!(check_ice_servers_against_peer(
            ["stun:anchor.example:3478"],
            Some("anchor.example")
        )
        .is_err());
        // An unbracketed IPv6 peer and a bracketed URL are one
        // endpoint.
        assert!(
            check_ice_servers_against_peer(["stun:[2001:db8::1]:3478"], Some("2001:db8::1"))
                .is_err()
        );
        // A tolerated query component cannot smuggle the peer past
        // the comparison.
        assert!(
            check_ice_servers_against_peer(["stun:198.51.100.7:4433?transport=udp"], peer).is_err()
        );
    }

    /// What must NOT be refused — the check is connection-specific
    /// and STUN-specific, and equality is all it claims.
    #[test]
    fn a_separate_endpoint_a_relay_or_an_unknown_peer_is_not_a_conflict() {
        let peer = Some("198.51.100.7:4433");
        // The whole point of Stage 6: the separately announced
        // endpoint on the same host, a different port.
        check_ice_servers_against_peer(["stun:198.51.100.7:3479"], peer)
            .expect("the announced STUN endpoint is the working configuration");
        // A third-party STUN server.
        check_ice_servers_against_peer(["stun:stun.example:3478"], peer).expect("unrelated");
        // A TURN relay is a different role; this check is about
        // STUN, and refusing a relay would refuse a configuration
        // that works.
        check_ice_servers_against_peer(["turn:198.51.100.7:4433"], peer).expect("a relay");
        check_ice_servers_against_peer(["turns:198.51.100.7:4433"], peer).expect("a relay");
        // A browser peer publishes no RTC endpoint, so there is
        // nothing this connection could collide with — including the
        // anchor's own endpoint, which an anchor may legitimately
        // serve to a browser ↔ browser connection it is not a party
        // to.
        check_ice_servers_against_peer(["stun:198.51.100.7:4433"], None)
            .expect("no peer endpoint is no conflict");
        // No entries at all.
        check_ice_servers_against_peer(core::iter::empty(), peer).expect("nothing configured");

        // The documented boundary, asserted so it stays documented:
        // a DNS alias of the peer is NOT detected. The leaf resolves
        // no names, and this test records that as a known limit
        // rather than a promise.
        check_ice_servers_against_peer(["stun:alias.example:4433"], peer)
            .expect("an unresolved alias is outside what equality can see");
    }

    /// **S6-07.5.** Two legal spellings of ONE IPv6 endpoint are one
    /// endpoint.
    ///
    /// RFC 4291 lets a single address be written compressed, fully
    /// expanded, with or without leading zeros, and — for an
    /// IPv4-mapped tuple — in dotted form. A textual comparison
    /// sees those as different servers, so an `iceServers` entry
    /// aimed squarely at the peer of this connection passed the
    /// check and bought an ICE deadline with no diagnostic. The key
    /// is the parsed address.
    ///
    /// Controls, all three, because the fix must not widen the
    /// check: the equal-string case still refuses, a genuinely
    /// distinct address is still accepted, and a NAME is never
    /// equal to an address (the leaf resolves nothing, so claiming
    /// otherwise would refuse a working configuration).
    #[test]
    fn equivalent_ipv6_spellings_of_the_peers_endpoint_are_one_endpoint() {
        // Compressed URL versus fully expanded peer, and back.
        let expanded = Some("[2001:0db8:0000:0000:0000:0000:0000:0001]:4433");
        assert!(
            check_ice_servers_against_peer(["stun:[2001:db8::1]:4433"], expanded).is_err(),
            "the compressed spelling of the peer's own endpoint must be refused"
        );
        let compressed = Some("[2001:db8::1]:4433");
        assert!(
            check_ice_servers_against_peer(
                ["stun:[2001:0db8:0000:0000:0000:0000:0000:0001]:4433"],
                compressed
            )
            .is_err(),
            "and so must the expanded spelling"
        );
        // Leading zeros inside one group, and uppercase hex.
        assert!(
            check_ice_servers_against_peer(["stun:[2001:0DB8::0001]:4433"], compressed).is_err()
        );

        // Control 1 — the equal-string case still refuses.
        assert!(
            check_ice_servers_against_peer(["stun:[2001:db8::1]:4433"], compressed).is_err(),
            "the case that already worked must keep working"
        );

        // Control 2 — a genuinely distinct address is accepted. The
        // separately announced STUN endpoint of the same anchor is
        // exactly this shape: same host, different port.
        check_ice_servers_against_peer(["stun:[2001:db8::1]:3479"], compressed)
            .expect("a different port is a different endpoint");
        check_ice_servers_against_peer(["stun:[2001:db8::2]:4433"], compressed)
            .expect("a different address is a different endpoint");

        // Control 3 — a name is not an address. Nothing here
        // resolves, so the two cannot be compared and must not be
        // reported equal.
        check_ice_servers_against_peer(["stun:anchor.example:4433"], compressed)
            .expect("an unresolved name is outside what equality can see");
    }
}
