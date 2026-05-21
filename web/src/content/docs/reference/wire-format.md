# Wire Format

This page is the byte-level reference for Net's packet wire format. You won't need it for application code — the SDK does the framing — but you'll want it for debugging packet captures, writing a custom adapter, or building a relay or proxy that needs to read the routing fields without decrypting payloads.

The header is fixed at 64 bytes, aligned to one CPU cache line, and contains every field a forwarding node needs to make a routing decision without decrypting anything.

## Header layout

```
 0                   1                   2                   3
 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|         MAGIC (0x4E45)        |     VER       |     FLAGS     |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|   PRIORITY    |    HOP_TTL    |   HOP_COUNT   |  FRAG_FLAGS   |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|       SUBPROTOCOL_ID          |        CHANNEL_HASH           |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                         NONCE (12 bytes)                      |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                       SESSION_ID (8 bytes)                    |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                       STREAM_ID (8 bytes)                     |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                       SEQUENCE (8 bytes)                      |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|      SUBNET_ID (4 bytes)      |     ORIGIN_HASH (4 bytes)     |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|       FRAGMENT_ID             |        FRAGMENT_OFFSET        |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|       PAYLOAD_LEN             |        EVENT_COUNT            |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
```

All multi-byte integers are little-endian. The total header is 64 bytes; the payload follows, capped at 8,096 bytes; the Poly1305 authentication tag follows the payload, 16 bytes.

## Fields

| Field             | Bytes | Type    | Purpose                                                                       |
|-------------------|-------|---------|-------------------------------------------------------------------------------|
| `MAGIC`           | 2     | u16     | `0x4E45` (ASCII "NE"). Identifies Net packets.                                |
| `VERSION`         | 1     | u8      | Wire version. Current: `1`.                                                   |
| `FLAGS`           | 1     | u8      | Packet flags (see below).                                                     |
| `PRIORITY`        | 1     | u8      | Routing priority (`0`–`7`). Higher is more urgent.                            |
| `HOP_TTL`         | 1     | u8      | Time-to-live in hops. Forwarders decrement; zero = drop.                      |
| `HOP_COUNT`       | 1     | u8      | Hops traversed. Forwarders increment.                                         |
| `FRAG_FLAGS`      | 1     | u8      | Fragmentation flags (more-fragments bit, etc.).                               |
| `SUBPROTOCOL_ID`  | 2     | u16     | Identifies how the payload is interpreted. See [subprotocol-ids](./subprotocol-ids). |
| `CHANNEL_HASH`    | 2     | u16     | xxh3-truncated hash of the channel name. Used for wire-speed authz.           |
| `NONCE`           | 12    | bytes   | AEAD nonce (counter-based).                                                   |
| `SESSION_ID`      | 8     | u64     | Identifies the encrypted session.                                             |
| `STREAM_ID`       | 8     | u64     | Identifies the stream within the session.                                     |
| `SEQUENCE`        | 8     | u64     | Per-stream sequence number.                                                   |
| `SUBNET_ID`       | 4     | u32     | Packed 4-level subnet hierarchy. See [subnets](../concepts/subnets).         |
| `ORIGIN_HASH`     | 4     | u32     | BLAKE2s-MAC of sender's ed25519 pubkey.                                       |
| `FRAGMENT_ID`     | 2     | u16     | Identifies a fragment group for reassembly.                                   |
| `FRAGMENT_OFFSET` | 2     | u16     | Byte offset of this fragment in the original payload.                         |
| `PAYLOAD_LEN`     | 2     | u16     | Length of the encrypted payload (excluding tag).                              |
| `EVENT_COUNT`     | 2     | u16     | Number of events packed into the payload.                                     |

## Flags

The `FLAGS` byte is a bitfield:

| Bit | Name        | Meaning                                                              |
|-----|-------------|----------------------------------------------------------------------|
| 0   | `RELIABLE`  | Sender expects acknowledgement; receiver must send back `NACK` or implicit ack. |
| 1   | `NACK`      | This packet is a negative acknowledgement.                           |
| 2   | `PRIORITY`  | High-priority path; bypasses fair queueing.                          |
| 3   | `FIN`       | Closes the stream after this packet.                                 |
| 4   | `HANDSHAKE` | Carries Noise handshake material (not yet encrypted).                |
| 5   | `HEARTBEAT` | Liveness probe; no payload semantics.                                |
| 6–7 | reserved    | Future use.                                                          |

## Constants

| Constant                  | Value      |
|---------------------------|------------|
| Magic                     | `0x4E45`   |
| Version                   | `1`        |
| Header size               | 64 bytes   |
| Max packet                | 8,192 bytes |
| Max payload (excl. tag)   | 8,096 bytes |
| Nonce size                | 12 bytes   |
| AEAD tag size             | 16 bytes (Poly1305) |

## Encryption

The header is sent in the clear; the payload is encrypted with ChaCha20-Poly1305 AEAD. The 12-byte `NONCE` field is a per-session counter (not random), keyed independently for transmit and receive directions, ruling out nonce reuse without depending on randomness.

The handshake is Noise NKpsk0:

- **Initiator** is anonymous.
- **Responder's static public key** is known in advance (out-of-band exchange, certificate, or capability advertisement).
- **Pre-shared key** adds symmetric authentication on top of the asymmetric exchange.

When two peers can't talk directly, `MeshNode::connect_via(relay_addr)` carries the Noise messages inside subprotocol `0x0601` over an existing encrypted session through a relay. The relay sees authenticated Noise bytes but can't forge them or derive the post-handshake session keys.

## Session keys

After a successful handshake, each direction has its own key:

```rust
pub struct SessionKeys {
    pub tx_key: [u8; 32],
    pub rx_key: [u8; 32],
    pub session_id: u64,
}
```

`PacketCipher` wraps the AEAD primitive with the per-session monotonic counter for nonce generation.

## Fragmentation

The wire MTU is 8,192 bytes. Payloads larger than `8,192 − 64 − 16 = 8,112` bytes are fragmented into multiple packets sharing a `FRAGMENT_ID`, with each fragment's `FRAGMENT_OFFSET` indicating its position in the reassembled payload.

The receiving session reassembles fragments by `(SESSION_ID, FRAGMENT_ID)`. Out-of-order fragments are buffered until the group is complete; incomplete groups time out after a configurable interval.

## What a forwarder needs

A pure forwarding node (no subprotocol handlers, no application logic) needs to read exactly these fields:

- `MAGIC` and `VERSION` — to confirm it's a Net packet.
- `HOP_TTL` and `HOP_COUNT` — to decrement and drop if zero.
- `SUBPROTOCOL_ID` — to apply opaque-forwarding fallback for unknown protocols.
- `CHANNEL_HASH` and `ORIGIN_HASH` — to consult the AuthGuard for wire-speed authorization.
- `SUBNET_ID` — to apply gateway visibility rules at subnet boundaries.

None of these require decrypting the payload. The forwarder's decision is a header-only read plus a small number of in-memory lookups (channel registry, auth guard, subnet routing table) — typically under 10 nanoseconds per packet on modern hardware.

## Performance characteristics

- **Header read:** one cache line. Modern CPUs prefetch and decode in a few cycles.
- **AuthGuard probe:** 4 KB bloom filter fits in L1; two atomic reads. ~10 ns on x86-64.
- **AEAD verify** (when the packet is for the local node): ChaCha20-Poly1305 of a 1 KB payload, ~250 ns on a modern core.
- **Forwarding latency:** dominated by the network path, not by Net's per-packet work. Net contributes single-digit microseconds end-to-end on the same LAN.

The wire format is designed around these properties. If you're writing a packet sniffer, a relay, or a custom adapter, the rule is: do as little as the protocol allows. The header is enough for almost every routing decision.
