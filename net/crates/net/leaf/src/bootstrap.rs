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
//! [`stun_probe_failed`] is the second half of the evidence
//! [`UdpBlockedEvidence`] requires. A browser cannot send a raw STUN
//! datagram, so the probe is the one form available to it: a
//! throwaway `RTCPeerConnection` whose *only* ICE server is the
//! anchor's published `rtc_addr`, gathering until a deadline. A
//! `srflx` candidate means a STUN binding response came back over
//! UDP. No `srflx` within the deadline, while the HTTPS bootstrap to
//! the same anchor succeeded, is the one observation that
//! distinguishes blocked UDP from an anchor that is simply not
//! there. Everything weaker stays
//! [`RtcError::IceTimeout`](crate::error::RtcError::IceTimeout).

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

use crate::control_plane::NodeId;
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
#[derive(Debug, Clone)]
pub struct Credential {
    /// The whole credential string, presented verbatim to
    /// `POST /rtc/offer`.
    pub encoded: String,
    /// The anchor's pinned Noise static public key.
    pub anchor_noise_pubkey: [u8; 32],
    /// The trust domain's NKpsk0 pre-shared key.
    pub psk: [u8; 32],
    /// The bootstrap listener's `https://` base URL.
    pub bootstrap_url: String,
    /// Unix seconds after which the standing PSK half is dead.
    pub psk_expires_at: u64,
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
        // The invite blob, length-prefixed. Opaque here.
        let invite_len = u32::from_le_bytes(take::<4>(&bytes, &mut at)?) as usize;
        at = at
            .checked_add(invite_len)
            .filter(|end| *end <= bytes.len())
            .ok_or_else(|| malformed("truncated invite"))?;

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
        if !bootstrap_url.starts_with("https://") && !bootstrap_url.starts_with("http://localhost")
        {
            return Err(malformed(
                "the bootstrap URL must be https:// (or http://localhost for a harness)",
            ));
        }
        Ok(Self {
            encoded: s.trim().to_string(),
            anchor_noise_pubkey,
            psk,
            bootstrap_url,
            psk_expires_at,
        })
    }

    /// Refuse a credential whose standing PSK half has expired.
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

/// What `GET /rtc/anchor` publishes.
#[derive(Debug, Clone)]
pub struct AnchorInfo {
    /// The anchor's node id.
    pub node_id: NodeId,
    /// Its live Noise static public key — for **comparison** with
    /// the credential's pinned key, never as a source of it.
    pub noise_pubkey: [u8; 32],
    /// Its public RTC/STUN socket, when the operator configured one.
    /// The STUN probe's only legitimate target.
    pub rtc_addr: Option<String>,
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
#[derive(Debug, Clone)]
pub struct OfferAccepted {
    /// The attempt token, presented as the trickle socket's
    /// WebSocket subprotocol.
    pub attempt_token: String,
    /// The dialog this attempt runs under.
    pub dialog: u64,
    /// The anchor's SDP answer.
    pub sdp: String,
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
    let (connection, saw_srflx) = probe;
    let deadline = gloo_timer_sleep(STUN_PROBE_MS);
    // A rejected timer promise cannot happen; either way the
    // deadline has elapsed by the time we are here.
    let _ = deadline.await;
    connection.close();
    !saw_srflx.get()
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

/// A throwaway peer connection whose only ICE server is `rtc_addr`,
/// with a flag that flips when a `srflx` candidate appears.
#[cfg(target_arch = "wasm32")]
fn build_probe(rtc_addr: &str) -> Result<(RtcPeerConnection, std::rc::Rc<core::cell::Cell<bool>>)> {
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
    // The callback must outlive this function; the connection is
    // closed by the caller, at which point both are dropped.
    closure.forget();

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
    Ok((connection, saw_srflx))
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
        let invite = b"invite-blob";
        buf.extend_from_slice(&(invite.len() as u32).to_le_bytes());
        buf.extend_from_slice(invite);
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
        let body = r#"{"node_id":"0xaabb","noise_pubkey":"a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1","rtc_addr":"198.51.100.7:4433","trust_domain":"x"}"#;
        let info = AnchorInfo::from_json(body).expect("parses");
        assert_eq!(info.node_id, 0xAABB);
        assert_eq!(info.noise_pubkey, [0xA1u8; 32]);
        assert_eq!(info.rtc_addr.as_deref(), Some("198.51.100.7:4433"));

        assert_eq!(parse_node_id("42"), Some(42));
        assert_eq!(parse_node_id("0X2A"), Some(42));
        assert_eq!(parse_node_id("nope"), None);

        let no_addr = r#"{"node_id":"1","noise_pubkey":"a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1"}"#;
        assert_eq!(
            AnchorInfo::from_json(no_addr).expect("parses").rtc_addr,
            None
        );
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
        };
        matching
            .check_pinned_key(&credential)
            .expect("the pinned key must be accepted");

        let impostor = AnchorInfo {
            node_id: 1,
            noise_pubkey: [0xBEu8; 32],
            rtc_addr: None,
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
}
