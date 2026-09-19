//! S0b — RTC loop spike, browser leaf side.
//!
//! A thin `wasm-bindgen` wrapper around the S0a wire crate. This is the
//! first time the S0a wasm build is *executed* anywhere: every call
//! below runs the `chacha20poly1305` AEAD backend, `web_time`'s clock,
//! snow's default-resolver and `getrandom`'s `wasm_js` backend in a
//! real browser.

use wasm_bindgen::prelude::*;

use s0a_wire::crypto::{handshake_prologue, NoiseHandshake, StaticKeypair};
use s0a_wire::parsed_packet::ParsedPacket;
use s0a_wire::protocol::{EventFrame, NetHeader, PacketFlags, HEADER_SIZE, NONCE_SIZE};
use s0a_wire::session::NetSession;

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
    console_log("s0b-web: wasm module loaded");
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

/// The browser-side leaf endpoint: NKpsk0 initiator + one `NetSession`.
#[wasm_bindgen]
pub struct LeafEndpoint {
    handshake: Option<NoiseHandshake>,
    session: Option<NetSession>,
    stream_id: u64,
}

#[wasm_bindgen]
impl LeafEndpoint {
    /// Build the initiator state. `self_id_hex` / `peer_id_hex` are the
    /// 64-bit node ids that bind the Noise prologue.
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
        let prologue = handshake_prologue(self_id, peer_id);
        let hs = NoiseHandshake::initiator_with_prologue(&psk, &rs, &prologue)
            .map_err(|e| JsError::new(&e.to_string()))?;
        Ok(LeafEndpoint {
            handshake: Some(hs),
            session: None,
            stream_id: 0x00AA_00BB_00CC_00DD,
        })
    }

    /// Noise message 1. Exercises snow's default resolver and the
    /// `getrandom` `wasm_js` backend (ephemeral keypair generation).
    pub fn msg1(&mut self) -> Result<Vec<u8>, JsError> {
        let hs = self
            .handshake
            .as_mut()
            .ok_or_else(|| JsError::new("handshake already consumed"))?;
        hs.write_message(&[])
            .map_err(|e| JsError::new(&e.to_string()))
    }

    /// Consume Noise message 2 and install the `NetSession`.
    pub fn read_msg2(&mut self, msg: &[u8]) -> Result<(), JsError> {
        let mut hs = self
            .handshake
            .take()
            .ok_or_else(|| JsError::new("handshake already consumed"))?;
        hs.read_message(msg)
            .map_err(|e| JsError::new(&e.to_string()))?;
        if !hs.is_finished() {
            return Err(JsError::new("handshake not finished after msg2"));
        }
        let keys = hs
            .into_session_keys()
            .map_err(|e| JsError::new(&e.to_string()))?;
        // `NetSession::new` still takes a `SocketAddr` — the S0a §6.3
        // finding. A browser has no such thing; a placeholder is used.
        let addr = "127.0.0.1:1"
            .parse()
            .map_err(|_| JsError::new("addr parse"))?;
        self.session = Some(NetSession::new(keys, addr, 2, false));
        Ok(())
    }

    /// Build one Net packet on a reliable stream through the S0a
    /// `PacketBuilder`. Runs the wasm ChaCha20-Poly1305 seal path and
    /// `web_time` (the session/stream liveness stamps).
    pub fn build_packet(&mut self, payload: &[u8]) -> Result<Vec<u8>, JsError> {
        let session = self
            .session
            .as_ref()
            .ok_or_else(|| JsError::new("no session"))?;
        let seq = session.get_or_create_stream(self.stream_id).next_tx_seq();
        let events = [bytes::Bytes::copy_from_slice(payload)];
        let mut builder = session.thread_local_pool().get();
        Ok(builder
            .build(self.stream_id, seq, &events, PacketFlags::RELIABLE)
            .to_vec())
    }

    /// Decrypt one inbound Net packet and return its first event frame.
    /// Runs the wasm AEAD open path and the anti-replay window.
    pub fn open_packet(&mut self, raw: &[u8]) -> Result<Vec<u8>, JsError> {
        let session = self
            .session
            .as_ref()
            .ok_or_else(|| JsError::new("no session"))?;
        let addr = "127.0.0.1:1"
            .parse()
            .map_err(|_| JsError::new("addr parse"))?;
        let parsed = ParsedPacket::parse(bytes::Bytes::copy_from_slice(raw), addr)
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
            return Err(JsError::new("no event frames"));
        }
        Ok(frames.remove(0).to_vec())
    }

    /// S0c config **B** (the DTLS-exporter shortcut, approximated):
    /// the same real 68-byte `NetHeader`, payload copied straight in,
    /// **no AEAD**. Same allocation and same wire size minus the
    /// 16-byte tag, so the delta against `build_packet` is the
    /// ChaCha20-Poly1305 seal plus the event framing.
    pub fn build_plain(&mut self, payload: &[u8]) -> Result<Vec<u8>, JsError> {
        let session = self
            .session
            .as_ref()
            .ok_or_else(|| JsError::new("no session"))?;
        let seq = session.get_or_create_stream(self.stream_id).next_tx_seq();
        let header = NetHeader::new(
            session.session_id(),
            self.stream_id,
            seq,
            [0u8; NONCE_SIZE],
            payload.len() as u16,
            1,
            PacketFlags::RELIABLE,
        );
        let mut out = Vec::with_capacity(HEADER_SIZE + payload.len());
        out.extend_from_slice(&header.to_bytes());
        out.extend_from_slice(payload);
        Ok(out)
    }

    /// S0c config **B**, receive side: header parse and validate only,
    /// no AEAD. Returns the payload length.
    pub fn parse_plain(&self, raw: &[u8]) -> Result<u32, JsError> {
        let addr = "127.0.0.1:1"
            .parse()
            .map_err(|_| JsError::new("addr parse"))?;
        let parsed = ParsedPacket::parse(bytes::Bytes::copy_from_slice(raw), addr)
            .ok_or_else(|| JsError::new("packet did not parse"))?;
        Ok(parsed.payload.len() as u32)
    }

    /// S0c hook (not measured here): build `n` packets of `size` bytes
    /// and return the per-packet build+seal time in milliseconds. JS
    /// owns the send rate; this owns the Net-layer cost.
    pub fn bench_build(&mut self, n: u32, size: u32) -> Result<Vec<f64>, JsError> {
        let payload = vec![0x5Au8; size as usize];
        let mut out = Vec::with_capacity(n as usize);
        for _ in 0..n {
            let t0 = now_ms();
            let _pkt = self.build_packet(&payload)?;
            out.push(now_ms() - t0);
        }
        Ok(out)
    }
}

/// Generate a Noise static keypair in the browser. Pure
/// `getrandom`-`wasm_js` exercise; returns the public half.
#[wasm_bindgen]
pub fn keygen_probe() -> Vec<u8> {
    StaticKeypair::generate().public_key().to_vec()
}

/// Read the S0a `Clock` seam twice and report the monotonic delta in
/// milliseconds. On wasm this is `web_time::Instant` (`performance.now`);
/// `std::time::Instant::now()` would panic here.
#[wasm_bindgen]
pub fn clock_probe() -> f64 {
    use s0a_wire::clock::{Clock, SystemClock};
    let a = SystemClock::now();
    let mut spin = 0u64;
    while SystemClock::now().duration_since(a).as_micros() < 200 {
        spin = spin.wrapping_add(1);
    }
    let b = SystemClock::now();
    let _ = spin;
    b.duration_since(a).as_secs_f64() * 1000.0
}

/// Read the S0a wall clock (`current_timestamp`, `web_time::SystemTime`
/// on wasm). Returned as f64 milliseconds so JS can sanity-check it.
#[wasm_bindgen]
pub fn wallclock_probe() -> f64 {
    s0a_wire::time::current_timestamp() as f64 / 1.0e6
}

fn now_ms() -> f64 {
    use s0a_wire::clock::{Clock, SystemClock};
    SystemClock::now()
        .duration_since(EPOCH.with(|e| *e))
        .as_secs_f64()
        * 1000.0
}

thread_local! {
    static EPOCH: s0a_wire::clock::Instant = {
        use s0a_wire::clock::{Clock, SystemClock};
        SystemClock::now()
    };
}
