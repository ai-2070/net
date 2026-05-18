# Phase 3 — Wire-Boundary Decoder Audit

Date: 2026-05-18. Scope: five fuzz-harnessed wire decoders in
`net/crates/net/`. Each entry point was located via its fuzz target,
then audited for the nine bug classes in the audit brief.

Targets reviewed (entry point → file:line):

1. `CapabilityAnnouncement::from_bytes` — `src/adapter/net/behavior/capability.rs:2109`
2. `compute::orchestrator::wire::decode` — `src/adapter/net/compute/orchestrator.rs:285`
3. `natpmp::decode_response` — `src/adapter/net/traversal/portmap/natpmp.rs:269`
4. `RoutingHeader::from_bytes` — `src/adapter/net/route.rs:165`
5. `SnapshotReassembler::feed` — `src/adapter/net/compute/orchestrator.rs:821`

No fuzz corpus directories exist on disk yet — there are no known-bad
seeds to cross-reference.

---

## CapabilityAnnouncement::from_bytes

CapabilityAnnouncement: clean — no panics found across the JSON path
and the round-trip's `verify` / `is_expired` post-decode calls.

What was checked:

- `from_bytes` is a one-liner `serde_json::from_slice(data).ok()`.
  serde_json never panics on malformed UTF-8/JSON.
- `Signature64`'s custom `Deserialize` (`capability.rs:1978`) length-
  checks the hex (line 1986) and the raw bytes (line 1994) before
  `copy_from_slice`; mismatched length returns a serde error, not a
  panic.
- `verify()` (line 2094) calls `ed25519_dalek::Signature::from_bytes`
  on a fixed `[u8; 64]` — infallible in v2.x.
- `is_expired()` (line 2114) uses `saturating_sub` on `timestamp_ns`
  before dividing by 1e9 and casts `ttl_secs as u64` (widening). No
  overflow path.
- `to_bytes` uses `serde_json::to_vec(self).unwrap_or_default()` —
  panic-free even on serialization failure.
- `CapabilitySet` (line 838) uses `#[serde(default, serialize_with = …)]`
  on `tags` and `#[serde(default)]` on `metadata`; both decode through
  serde's HashSet/BTreeMap visitors which allocate proportional to
  the JSON input. A `serde_json::from_slice` reader is bounded by
  `data.len()` so no unbounded allocation beyond `data.len()`.
- The fuzz target's round-trip relies on `to_bytes` being canonical
  for the same `ann` — `metadata: BTreeMap` is canonical, `tags:
  HashSet` is sorted via `serialize_tags_sorted`. Symmetry holds.

---

## compute::orchestrator::wire::decode

### F-1 — `payload_len + 8` overflows on 32-bit targets in `BufferedEvents`
- **File:line:** `src/adapter/net/compute/orchestrator.rs:460–461`
- **Severity:** low (only 32-bit; the crate is built primarily for
  64-bit hosts).
- **Bug class:** overflow → bypassed length check → OOB-read panic.
- **What:** `let payload_len = cur.get_u32_le() as usize; if cur.remaining() < payload_len + 8`.
  On a 32-bit build (`usize == u32`), an attacker-supplied
  `payload_len = 0xFFFFFFF8..=0xFFFFFFFF` makes `payload_len + 8`
  wrap to `0..=7`, the check passes spuriously, and `vec![0u8;
  payload_len]` either OOMs immediately or `cur.copy_to_slice(&mut
  payload)` reads past `data` and panics with "advance out of
  bounds".
- **Adversarial input:** `MSG_BUFFERED_EVENTS(0x07) || daemon_origin
  u64 || count_u32 = 1 || CausalLink(32 valid bytes) || payload_len
  = 0xFFFFFFFF || …`. Total wire size ~50 bytes; `payload_len +
  8 = 7`, check passes, vec allocates 4 GiB.
- **Fix sketch:** `if (payload_len as u64) + 8 > cur.remaining() as
  u64 { return Err(...); }` or use `checked_add`.

### F-2 — `BufferedEvents` per-event payload has no hard cap
- **File:line:** `orchestrator.rs:460–467`
- **Severity:** low (defense-in-depth; transport framing caps the
  outer packet).
- **Bug class:** unbounded-alloc.
- **What:** `MAX_BUFFERED_EVENTS` caps `count` at 1M and
  `MIN_EVENT_WIRE_SIZE` bounds it against remaining bytes, but a
  single event's `payload_len` is checked only against
  `cur.remaining()`. A peer can ship one event whose payload
  consumes the entire frame — fine over `send_subprotocol` since
  framing caps packet size, but a future caller that hands a
  pre-buffered multi-megabyte slice in (e.g. from a stream
  reassembler) would allocate proportionally with no per-event
  ceiling.
- **Adversarial input:** Single event with `payload_len = N` where N
  is large; vec allocates N bytes.
- **Fix sketch:** Add `const MAX_EVENT_PAYLOAD: usize = …` (mirror
  `MAX_SNAPSHOT_CHUNK_SIZE`) and reject `payload_len >
  MAX_EVENT_PAYLOAD` before `vec![0u8; payload_len]`.

Other branches checked:

- `MSG_SNAPSHOT_READY` (line 304) explicitly bounds `total_chunks`,
  `chunk_index`, and `len`; no overflow path.
- `decode_failure_reason` (line 511) covers codes 0–6 and rejects
  unknowns; `read_u16_string` (line 544) length-checks `cur.remaining()
  < len` (u16, no overflow) before reading bytes and validates UTF-8
  via `String::from_utf8`. Clean.
- Round-trip: every accepted variant re-encodes to the same byte
  pattern (no canonicalization drift — confirmed by walking
  `encode`/`decode` per-variant).

---

## natpmp::decode_response

decode_response: clean — no issues found in the 4 dispatched code
paths (`OP_EXTERNAL_ADDRESS`, `OP_MAP_UDP`, fast-rejected short
packets, fast-rejected unknown ops). Min-length gate is
`EXTERNAL_RESPONSE_LEN = 12` before any indexed read; the largest
fixed index used is `data[15]` and that's gated by `MAP_RESPONSE_LEN
= 16`. `raw_op - RESPONSE_OP_OFFSET` is preceded by `raw_op <
RESPONSE_OP_OFFSET` so the `u8` subtraction never wraps. No length-
prefixed fields, no recursion, no dynamic allocation.

---

## RoutingHeader::from_bytes

### F-3 — Encoding ambiguity: high-nibble flag bits silently dropped
- **File:line:** `src/adapter/net/route.rs:51–53` (`RouteFlags::from_u8`)
- **Severity:** low (no immediate exploit; flagging because the
  audit brief calls out encoding ambiguity for security-relevant
  identifiers).
- **Bug class:** encoding ambiguity.
- **What:** `from_u8` does `Self(v & 0x0F)`. Sixteen distinct wire
  bytes (`0x00`..`0x0F` plus `0x10`..`0xFF` masked) map to the same
  in-memory `RouteFlags`. The to/from round-trip held by the fuzz
  target succeeds only because `to_bytes` writes back the masked
  value. Bytes 5 (`_reserved`) is round-tripped raw — so the wire
  encoding has 4 garbage bits per packet plus a 1-byte raw reserved
  field. None of `CONTROL`/`REQUIRES_ACK`/`PRIORITY`/`END_OF_STREAM`
  decisions read those bits today, so there's no privilege gain via
  flag injection, but two parties using a future flag in the high
  nibble would silently disagree across an upgrade boundary.
- **Adversarial input:** Two routed packets identical except byte 4
  is `0x01` vs `0xF1` — both decode to `RouteFlags(CONTROL)`.
- **Fix sketch:** Either widen `RouteFlags` to accept all 8 bits
  (`Self(v)`) and reserve the high nibble explicitly, or reject
  `v & 0xF0 != 0` so wire ambiguity becomes a parse failure.

Everything else in `from_bytes` (route.rs:165) is bounds-checked:
fixed 18-byte read, magic check, then `try_into().ok()?` on
sub-slices of guaranteed length. No panics.

---

## SnapshotReassembler::feed

### F-4 — `total_chunks=1` fast-path bypasses `TotalChunksMismatch` guard
- **File:line:** `src/adapter/net/compute/orchestrator.rs:877–881`
- **Severity:** medium (cross-message state corruption on the
  migration target).
- **Bug class:** state-corruption.
- **What:** The single-chunk fast path matches `total_chunks == 1`
  *before* consulting any existing `pending[(origin, seq_through)]`
  state. Sequence:
  1. Peer feeds chunk 0/3 (`total_chunks=3`, `chunk_index=0`,
     payload `A`). State stored.
  2. Same peer feeds chunk 0/1 (`total_chunks=1`, `chunk_index=0`,
     payload `B`). Line 879 removes pending; line 880 returns
     `Ok(Some(B))` as a completed snapshot.

  The multi-chunk path's `TotalChunksMismatch` guard at line 892–897
  rejects shrinking `total_chunks` in the slow path, but the
  fast-path return at line 880 dodges it. The orchestrator then
  treats `B` as the authoritative restored snapshot for that
  `(origin, seq_through)`, even though the original message stream
  declared three chunks. A malicious source daemon can therefore
  switch in a one-chunk payload of its choice mid-reassembly,
  effectively a snapshot substitution for its own migration. (The
  per-daemon origin check rules out a third party — but a
  compromised source already has authority to ship a snapshot, so
  the actual surface is "source can swap shapes after the receiver
  has committed buffer to a larger one" → wasted memory and
  trivially-replaceable state.)
- **Adversarial input:** Two `MSG_SNAPSHOT_READY` frames with the
  same `(daemon_origin, seq_through)`, first with `total_chunks=3
  chunk_index=0`, second with `total_chunks=1 chunk_index=0`.
- **Fix sketch:** Before the fast return at line 879, check whether
  `self.pending` already has the key with a different
  `total_chunks` and reject with
  `ReassemblyError::TotalChunksMismatch`. Or simpler: drop the
  fast-path entirely and let the slow path handle `total_chunks ==
  1` (it already does correctly).

### F-5 — Zero-byte chunks bypass `MAX_PENDING_REASSEMBLY_BYTES` cap, work-amplifying the BTreeMap
- **File:line:** `orchestrator.rs:907–918`
- **Severity:** low (already-bounded by `MAX_TOTAL_CHUNKS = 700_000`
  and the sweep-stale path; flagging because the byte-cap is
  documented as the primary defense and this is its blind spot).
- **Bug class:** unbounded-alloc (relative).
- **What:** The cap is on `bytes_buffered`. A peer that declares
  `total_chunks = 700_000` and ships 700K chunks of `len = 0` keeps
  `bytes_buffered = 0` forever while accumulating 700K BTreeMap
  entries (~24 MiB) per `(daemon_origin, seq_through)`. Bounded by
  the work the attacker does per feed call, but per-feed cost is
  ~constant while per-entry memory grows.
- **Adversarial input:** Loop sending `MSG_SNAPSHOT_READY` with
  `chunk_index = i`, `total_chunks = 700_000`, payload empty, for
  `i = 0..699_999`.
- **Fix sketch:** Also cap `state.chunks.len()` against a small
  multiple of `bytes_buffered / (some min chunk size)`, or
  reject `snapshot_bytes.is_empty() && total_chunks > 1`. The
  legitimate chunker (`chunk_snapshot`) never emits empty chunks.

Other points checked and clean:

- The `state.chunks.len() == state.total_chunks as usize` complete-
  reassembly check (line 931) is sound: with `chunk_index <
  total_chunks` enforced and BTreeMap dedupping by key, reaching
  `total_chunks` entries proves coverage.
- `self.pending.remove(&key).unwrap()` at line 932 is safe — the
  entry was inserted via `entry().or_insert_with` earlier in the
  call and the borrow ends before the remove.
- `Vec::with_capacity(state.chunks.values().map(|c| c.len()).sum())`
  at line 933 is bounded by `MAX_PENDING_REASSEMBLY_BYTES` (64 MiB).
- `sweep_stale` uses `checked_duration_since` — no panic on time
  going backward.
- `cancel` is a single `retain`; no panic surface.

---

## Summary

| Decoder                                | Findings                                |
| -------------------------------------- | --------------------------------------- |
| CapabilityAnnouncement::from_bytes     | clean                                   |
| wire::decode (migration)               | F-1 (32-bit only), F-2 (low DiD)        |
| natpmp::decode_response                | clean                                   |
| RoutingHeader::from_bytes              | F-3 (encoding ambiguity, low)           |
| SnapshotReassembler::feed              | F-4 (medium), F-5 (low)                 |

Highest-priority: **F-4** — fast-path state-substitution in the
snapshot reassembler is the only finding that breaks a security
property (consistency of the chunk-set declared for an in-flight
reassembly). Everything else is defense-in-depth or
target-architecture-dependent.
