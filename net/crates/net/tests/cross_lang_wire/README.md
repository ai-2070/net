# `cross_lang_wire` fixtures

Golden vectors for the wire surface `net-mesh-wire` owns since Stage 2
of `docs/internal/plans/BROWSER_NATIVE_WEBRTC_TRANSPORT_PLAN.md`.

Every fixture is `{ description, …inputs, hex | bytes_utf8 }`: the
inputs name a value, the `hex` is what the encoder must emit for it,
byte for byte. The Rust consumer is `tests/cross_lang_wire.rs` (it
encodes AND decodes each one); the AEAD vector is additionally
replayed on the `chacha20poly1305` backend by the wasm test in
`wire/tests/wasm_wire.rs`, which is the point of that fixture — the
same (key, nonce, aad, plaintext) must seal identically under ring and
under RustCrypto, or the backend seam has silently changed the wire
format.

| Fixture | Pins |
|---|---|
| `net_header.json` | `NetHeader::to_bytes` / `from_bytes` (68-byte header) |
| `routing_header.json` | `RoutingHeader::to_bytes` / `from_bytes` (18-byte envelope) |
| `event_frame.json` | `EventFrame::write_events` / `read_events` framing |
| `nack_payload.json` | `NackPayload::to_bytes` / `from_bytes` |
| `stream_window.json` | `StreamWindow::encode` / `decode` (0x0B00) |
| `aead_vector.json` | ChaCha20-Poly1305 across both AEAD backends |
| `capability_announcement.json` | `CapabilityAnnouncement` JSON, **current** form |

Go / TypeScript / Python consumers are not part of Stage 2; the shape
matches the other `cross_lang_*` fixture sets so they can be added
without touching these files.

Regenerating by hand is deliberate: a fixture that a failing build can
rewrite is not a fixture. If a change to the wire format is intended,
edit the expected bytes in the same commit as the code and say why.
