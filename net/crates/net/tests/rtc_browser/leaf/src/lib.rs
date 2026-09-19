//! Stage 4b Chromium harness — the **browser leaf**.
//!
//! A thin `wasm-bindgen` wrapper around `net-mesh-wire`, the
//! production wire layer (`net_wire`), compiled to
//! `wasm32-unknown-unknown`. Everything the browser does on the
//! DataChannel goes through this crate:
//!
//! - the NKpsk0 handshake as **initiator**, with the same prologue
//!   `MeshNode::accept_rtc` builds on the responder side
//!   (`handshake_prologue(routing_id(self), routing_id(peer))`);
//! - `PacketBuilder::build_handshake` for Noise message 1, which is
//!   what the anchor's dispatcher forwards to its handshake inbox;
//! - `PacketBuilder::build_subprotocol` for every frame the
//!   witnesses send, with the same `channel_hash` / `origin_hash`
//!   stamps `MeshNode::try_publish_to_peer` applies;
//! - `ParsedPacket` + the session's rx cipher for inbound packets.
//!
//! **What this is NOT.** It is not an nRPC client — that is Stage 5.
//! The *payload bytes* of the enrollment REQUEST, the membership
//! Subscribe, the capability announcement and the routing envelope
//! are built on the native side with the production encoders and
//! handed to the page over plain HTTP; the page owns the transport
//! (Noise, packet construction, AEAD, the DataChannel). See the
//! harness doc comment in `runner/src/main.rs`.

use wasm_bindgen::prelude::*;

use net_wire::crypto::{handshake_prologue, NoiseHandshake};
use net_wire::parsed_packet::ParsedPacket;
use net_wire::pool::PacketBuilder;
use net_wire::protocol::{EventFrame, PacketFlags};
use net_wire::session::NetSession;
use net_wire::PeerAddr;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console, js_name = error)]
    fn console_error(s: &str);
    #[wasm_bindgen(js_namespace = console, js_name = log)]
    fn console_log(s: &str);
}

/// Route Rust panics to `console.error` so a wasm runtime surprise is
/// legible in the browser log instead of an opaque `unreachable`.
#[wasm_bindgen(start)]
pub fn start() {
    std::panic::set_hook(Box::new(|info| {
        console_error(&format!("WASM PANIC: {info}"));
    }));
    console_log("rtc-browser-leaf: net-mesh-wire loaded");
}

/// The 32-bit routing projection of a node id. `mesh.rs`'s
/// `routing_id`, which both handshake halves feed into the prologue.
#[inline]
fn routing_id(node_id: u64) -> u64 {
    (node_id as u32) as u64
}

fn unhex(s: &str) -> Result<Vec<u8>, JsError> {
    if s.len() % 2 != 0 {
        return Err(JsError::new("odd hex length"));
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| JsError::new(&e.to_string())))
        .collect()
}

fn arr32(v: &[u8]) -> Result<[u8; 32], JsError> {
    v.try_into().map_err(|_| JsError::new("expected 32 bytes"))
}

/// A `PeerAddr` the wire layer only uses as a map key. A browser has
/// no socket address; the anchor keys this session on its RTC
/// endpoint, not on anything the browser says here.
fn placeholder_addr() -> Result<PeerAddr, JsError> {
    Ok(PeerAddr::Udp(
        "127.0.0.1:1".parse().map_err(|_| JsError::new("addr"))?,
    ))
}

/// The browser end of one mesh session: an NKpsk0 initiator that
/// becomes a `NetSession`.
#[wasm_bindgen]
pub struct LeafEndpoint {
    handshake: Option<NoiseHandshake>,
    session: Option<NetSession>,
}

#[wasm_bindgen]
impl LeafEndpoint {
    /// Build the initiator state against the **credential-pinned**
    /// responder static key.
    ///
    /// `responder_static_hex` comes from the bootstrap credential,
    /// never from `GET /rtc/anchor` — that is exactly what makes the
    /// MITM witness meaningful.
    #[wasm_bindgen(constructor)]
    pub fn new(
        psk_hex: &str,
        responder_static_hex: &str,
        self_id_hex: &str,
        peer_id_hex: &str,
    ) -> Result<LeafEndpoint, JsError> {
        let psk = arr32(&unhex(psk_hex)?)?;
        let rs = arr32(&unhex(responder_static_hex)?)?;
        let self_id =
            u64::from_str_radix(self_id_hex, 16).map_err(|e| JsError::new(&e.to_string()))?;
        let peer_id =
            u64::from_str_radix(peer_id_hex, 16).map_err(|e| JsError::new(&e.to_string()))?;
        let prologue = handshake_prologue(routing_id(self_id), routing_id(peer_id));
        let hs = NoiseHandshake::initiator_with_prologue(&psk, &rs, &prologue)
            .map_err(|e| JsError::new(&e.to_string()))?;
        Ok(LeafEndpoint {
            handshake: Some(hs),
            session: None,
        })
    }

    /// Noise message 1, already wrapped in the Net **handshake
    /// packet** the anchor's dispatcher recognises
    /// (`PacketBuilder::build_handshake`, the same call
    /// `handshake_initiator` makes natively).
    pub fn msg1_packet(&mut self) -> Result<Vec<u8>, JsError> {
        let hs = self
            .handshake
            .as_mut()
            .ok_or_else(|| JsError::new("handshake already consumed"))?;
        let msg1 = hs
            .write_message(&[])
            .map_err(|e| JsError::new(&e.to_string()))?;
        let mut builder = PacketBuilder::new(&[0u8; 32], 0);
        Ok(builder.build_handshake(&msg1).to_vec())
    }

    /// Consume the anchor's Noise message 2 — delivered as a Net
    /// handshake packet — and install the `NetSession`.
    pub fn read_msg2_packet(&mut self, raw: &[u8]) -> Result<(), JsError> {
        let parsed = ParsedPacket::parse(bytes::Bytes::copy_from_slice(raw), placeholder_addr()?)
            .ok_or_else(|| JsError::new("msg2 packet did not parse"))?;
        if !parsed.header.flags.is_handshake() {
            return Err(JsError::new("expected a handshake packet"));
        }
        let mut hs = self
            .handshake
            .take()
            .ok_or_else(|| JsError::new("handshake already consumed"))?;
        hs.read_message(&parsed.payload)
            .map_err(|e| JsError::new(&e.to_string()))?;
        if !hs.is_finished() {
            return Err(JsError::new("handshake not finished after msg2"));
        }
        let keys = hs
            .into_session_keys()
            .map_err(|e| JsError::new(&e.to_string()))?;
        self.session = Some(NetSession::new(keys, placeholder_addr()?, 4, false));
        Ok(())
    }

    /// The session id both halves derived from the handshake. The
    /// anchor keys its peer record on the same value.
    pub fn session_id(&self) -> Result<String, JsError> {
        let s = self
            .session
            .as_ref()
            .ok_or_else(|| JsError::new("no session"))?;
        Ok(format!("{:016x}", s.session_id()))
    }

    /// Build one outbound packet, exactly as
    /// `MeshNode::try_publish_to_peer` /
    /// `send_subprotocol_to_node` do: open/advance the stream,
    /// stamp `channel_hash` (u16) and `origin_hash` (u64), then
    /// `build_subprotocol`.
    ///
    /// `subprotocol_id == 0` is the event plane (nRPC and every
    /// application frame); anything else is a control subprotocol.
    #[expect(clippy::too_many_arguments, reason = "one call per wire field")]
    pub fn build_frame(
        &mut self,
        stream_id_hex: &str,
        subprotocol_id: u16,
        channel_hash_u16: u16,
        origin_hash_hex: &str,
        reliable: bool,
        payload: &[u8],
    ) -> Result<Vec<u8>, JsError> {
        let session = self
            .session
            .as_ref()
            .ok_or_else(|| JsError::new("no session"))?;
        let stream_id =
            u64::from_str_radix(stream_id_hex, 16).map_err(|e| JsError::new(&e.to_string()))?;
        let origin_hash =
            u64::from_str_radix(origin_hash_hex, 16).map_err(|e| JsError::new(&e.to_string()))?;
        let seq = session.get_or_create_stream(stream_id).next_tx_seq();
        let flags = if reliable {
            PacketFlags::RELIABLE
        } else {
            PacketFlags::NONE
        };
        let events = [bytes::Bytes::copy_from_slice(payload)];
        let mut builder = session.thread_local_pool().get();
        builder.set_channel_hash(channel_hash_u16);
        builder.set_origin_hash(origin_hash);
        Ok(builder
            .build_subprotocol(stream_id, seq, &events, flags, subprotocol_id)
            .to_vec())
    }

    /// Decrypt one inbound packet and return its first event frame.
    /// Runs the wasm ChaCha20-Poly1305 open path and the anti-replay
    /// window, both from the production wire crate.
    pub fn open_packet(&mut self, raw: &[u8]) -> Result<Vec<u8>, JsError> {
        let session = self
            .session
            .as_ref()
            .ok_or_else(|| JsError::new("no session"))?;
        let parsed = ParsedPacket::parse(bytes::Bytes::copy_from_slice(raw), placeholder_addr()?)
            .ok_or_else(|| JsError::new("packet did not parse"))?;
        let aad = parsed.header.aad();
        let counter = u64::from_le_bytes(
            parsed.header.nonce[4..12]
                .try_into()
                .map_err(|_| JsError::new("short nonce"))?,
        );
        let rx = session.rx_cipher();
        let plain = rx
            .decrypt_to_bytes(counter, &aad, parsed.payload.clone())
            .map_err(|e| JsError::new(&e.to_string()))?;
        if !rx.try_admit_rx_counter(counter) {
            return Err(JsError::new("replay window rejected the counter"));
        }
        let mut frames = EventFrame::read_events(plain, parsed.header.event_count);
        if frames.is_empty() {
            return Ok(Vec::new());
        }
        Ok(frames.remove(0).to_vec())
    }

    /// Header-only view of an inbound packet, before any decryption:
    /// `is_handshake`, `subprotocol_id`, `channel_hash`. Lets the
    /// page route a packet without pretending to own the dispatch
    /// loop.
    pub fn peek(&self, raw: &[u8]) -> Result<String, JsError> {
        let parsed = ParsedPacket::parse(bytes::Bytes::copy_from_slice(raw), placeholder_addr()?)
            .ok_or_else(|| JsError::new("packet did not parse"))?;
        Ok(format!(
            "{{\"handshake\":{},\"subprotocol\":{},\"channel\":{},\"events\":{}}}",
            parsed.header.flags.is_handshake(),
            parsed.header.subprotocol_id,
            parsed.header.channel_hash,
            parsed.header.event_count
        ))
    }
}
