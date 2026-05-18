//! Migration orchestrator — coordinates all 6 phases of daemon migration.
//!
//! The orchestrator runs on the controller node (which may be the source,
//! target, or a third-party coordinator). It sequences phase transitions,
//! routes migration messages, and handles failures/timeouts.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Buf;
use dashmap::DashMap;
use parking_lot::Mutex;

use super::migration::{MigrationError, MigrationFailureReason, MigrationPhase, MigrationState};
use super::migration_source::MigrationSourceHandler;
use super::registry::DaemonRegistry;
use crate::adapter::net::continuity::superposition::SuperpositionState;
use crate::adapter::net::state::causal::{CausalEvent, CausalLink};
use crate::adapter::net::state::snapshot::StateSnapshot;

// ── Migration message protocol ──────────────────────────────────────────────

/// Wire message types for the migration subprotocol (0x0500).
#[derive(Debug, Clone)]
pub enum MigrationMessage {
    /// Phase 0→1: Request snapshot on source.
    TakeSnapshot {
        /// Origin hash of daemon to migrate.
        daemon_origin: u64,
        /// Destination node ID.
        target_node: u64,
    },

    /// Phase 1→2: Snapshot taken, payload included.
    ///
    /// Large snapshots are chunked across multiple `SnapshotReady` messages.
    /// The receiver must reassemble all chunks (0..total_chunks) before
    /// deserializing the snapshot. Single-chunk snapshots have
    /// `chunk_index = 0, total_chunks = 1`.
    SnapshotReady {
        /// Origin hash of daemon being migrated.
        daemon_origin: u64,
        /// Serialized `StateSnapshot` bytes (or chunk thereof).
        snapshot_bytes: Vec<u8>,
        /// Sequence number the snapshot covers through.
        seq_through: u64,
        /// Zero-based index of this chunk.
        chunk_index: u32,
        /// Total number of chunks for this snapshot.
        total_chunks: u32,
    },

    /// Phase 2→3: Target restored daemon from snapshot.
    RestoreComplete {
        /// Origin hash of daemon being migrated.
        daemon_origin: u64,
        /// Sequence number restored through.
        restored_seq: u64,
    },

    /// Phase 3→4: Target finished replaying buffered events.
    ReplayComplete {
        /// Origin hash of daemon being migrated.
        daemon_origin: u64,
        /// Sequence number replayed through.
        replayed_seq: u64,
    },

    /// Phase 4: Source stops accepting writes, routing switches.
    CutoverNotify {
        /// Origin hash of daemon being migrated.
        daemon_origin: u64,
        /// Target node that is now authoritative.
        target_node: u64,
    },

    /// Phase 5: Source cleaned up.
    CleanupComplete {
        /// Origin hash of daemon whose migration is complete.
        daemon_origin: u64,
    },

    /// Any phase: Abort migration.
    MigrationFailed {
        /// Origin hash of daemon whose migration failed.
        daemon_origin: u64,
        /// Structured reason code — source dispatches on this to
        /// decide whether the migration is retriable. See
        /// [`MigrationFailureReason`].
        reason: MigrationFailureReason,
    },

    /// Buffered events from source for replay on target.
    BufferedEvents {
        /// Origin hash of daemon being migrated.
        daemon_origin: u64,
        /// Events to replay, in causal order.
        events: Vec<CausalEvent>,
    },

    /// Phase 5→6: Source has cleaned up; target should now go live.
    ///
    /// Emitted by the orchestrator once it observes `CleanupComplete`. The
    /// target calls `MigrationTargetHandler::activate` in response and
    /// replies with `ActivateAck`.
    ActivateTarget {
        /// Origin hash of daemon whose target should activate.
        daemon_origin: u64,
    },

    /// Phase 6: Target has activated and is now authoritative.
    ActivateAck {
        /// Origin hash of daemon whose migration is complete.
        daemon_origin: u64,
        /// Sequence number the target is authoritative through.
        replayed_seq: u64,
    },
}

// ── Wire format ─────────────────────────────────────────────────────────────

/// Wire format encode/decode for migration messages.
pub mod wire {
    use super::*;
    use bytes::{Buf, BufMut};

    /// Wire type: request snapshot on source.
    pub const MSG_TAKE_SNAPSHOT: u8 = 0;
    /// Wire type: snapshot taken, payload included.
    pub const MSG_SNAPSHOT_READY: u8 = 1;
    /// Wire type: target restored daemon from snapshot.
    pub const MSG_RESTORE_COMPLETE: u8 = 2;
    /// Wire type: target finished replaying buffered events.
    pub const MSG_REPLAY_COMPLETE: u8 = 3;
    /// Wire type: source stops writes, routing switches.
    pub const MSG_CUTOVER_NOTIFY: u8 = 4;
    /// Wire type: source cleaned up.
    pub const MSG_CLEANUP_COMPLETE: u8 = 5;
    /// Wire type: migration failed/aborted.
    pub const MSG_FAILED: u8 = 6;
    /// Wire type: buffered events for replay.
    pub const MSG_BUFFERED_EVENTS: u8 = 7;
    /// Wire type: orchestrator tells target to activate.
    pub const MSG_ACTIVATE_TARGET: u8 = 8;
    /// Wire type: target acknowledges activation.
    pub const MSG_ACTIVATE_ACK: u8 = 9;

    /// Encode a migration message to bytes.
    ///
    /// Returns `MigrationError::StateFailed` when a length-prefixed field
    /// would not fit in its on-wire width. Length prefixes are `u32` for
    /// payloads and counts and `u16` for the failure reason string; silently
    /// truncating to fit would corrupt the stream and confuse the decoder.
    pub fn encode(msg: &MigrationMessage) -> Result<Vec<u8>, MigrationError> {
        // Helper: convert a usize length to u32 with an error on overflow.
        fn len_u32(field: &str, n: usize) -> Result<u32, MigrationError> {
            u32::try_from(n).map_err(|_| {
                MigrationError::StateFailed(format!("{} length {} exceeds u32::MAX", field, n))
            })
        }

        let mut buf = Vec::with_capacity(128);

        match msg {
            MigrationMessage::TakeSnapshot {
                daemon_origin,
                target_node,
            } => {
                buf.put_u8(MSG_TAKE_SNAPSHOT);
                buf.put_u64_le(*daemon_origin);
                buf.put_u64_le(*target_node);
            }
            MigrationMessage::SnapshotReady {
                daemon_origin,
                snapshot_bytes,
                seq_through,
                chunk_index,
                total_chunks,
            } => {
                let payload_len = len_u32("snapshot_bytes", snapshot_bytes.len())?;
                buf.put_u8(MSG_SNAPSHOT_READY);
                buf.put_u64_le(*daemon_origin);
                buf.put_u64_le(*seq_through);
                buf.put_u32_le(*chunk_index);
                buf.put_u32_le(*total_chunks);
                buf.put_u32_le(payload_len);
                buf.extend_from_slice(snapshot_bytes);
            }
            MigrationMessage::RestoreComplete {
                daemon_origin,
                restored_seq,
            } => {
                buf.put_u8(MSG_RESTORE_COMPLETE);
                buf.put_u64_le(*daemon_origin);
                buf.put_u64_le(*restored_seq);
            }
            MigrationMessage::ReplayComplete {
                daemon_origin,
                replayed_seq,
            } => {
                buf.put_u8(MSG_REPLAY_COMPLETE);
                buf.put_u64_le(*daemon_origin);
                buf.put_u64_le(*replayed_seq);
            }
            MigrationMessage::CutoverNotify {
                daemon_origin,
                target_node,
            } => {
                buf.put_u8(MSG_CUTOVER_NOTIFY);
                buf.put_u64_le(*daemon_origin);
                buf.put_u64_le(*target_node);
            }
            MigrationMessage::CleanupComplete { daemon_origin } => {
                buf.put_u8(MSG_CLEANUP_COMPLETE);
                buf.put_u64_le(*daemon_origin);
            }
            MigrationMessage::MigrationFailed {
                daemon_origin,
                reason,
            } => {
                buf.put_u8(MSG_FAILED);
                buf.put_u64_le(*daemon_origin);
                // Wire layout:
                //   code:  u16 le
                //   then variant-specific payload (0 bytes for
                //   zero-payload variants; `u16 le + bytes` for
                //   string-bearing variants; `u8` for NotReadyTimeout).
                buf.put_u16_le(reason.code());
                match reason {
                    MigrationFailureReason::NotReady
                    | MigrationFailureReason::FactoryNotFound
                    | MigrationFailureReason::ComputeNotSupported
                    | MigrationFailureReason::AlreadyMigrating => {}
                    MigrationFailureReason::StateFailed(msg)
                    | MigrationFailureReason::IdentityTransportFailed(msg) => {
                        let len = u16::try_from(msg.len()).map_err(|_| {
                            MigrationError::StateFailed(format!(
                                "failure reason message length {} exceeds u16::MAX",
                                msg.len()
                            ))
                        })?;
                        buf.put_u16_le(len);
                        buf.extend_from_slice(msg.as_bytes());
                    }
                    MigrationFailureReason::NotReadyTimeout { attempts } => {
                        buf.put_u8(*attempts);
                    }
                }
            }
            MigrationMessage::BufferedEvents {
                daemon_origin,
                events,
            } => {
                let event_count = len_u32("events", events.len())?;
                buf.put_u8(MSG_BUFFERED_EVENTS);
                buf.put_u64_le(*daemon_origin);
                buf.put_u32_le(event_count);
                for event in events {
                    let payload_len = len_u32("event payload", event.payload.len())?;
                    let link_bytes = event.link.to_bytes();
                    buf.extend_from_slice(&link_bytes);
                    buf.put_u32_le(payload_len);
                    buf.extend_from_slice(&event.payload);
                    buf.put_u64_le(event.received_at);
                }
            }
            MigrationMessage::ActivateTarget { daemon_origin } => {
                buf.put_u8(MSG_ACTIVATE_TARGET);
                buf.put_u64_le(*daemon_origin);
            }
            MigrationMessage::ActivateAck {
                daemon_origin,
                replayed_seq,
            } => {
                buf.put_u8(MSG_ACTIVATE_ACK);
                buf.put_u64_le(*daemon_origin);
                buf.put_u64_le(*replayed_seq);
            }
        }

        Ok(buf)
    }

    /// Decode a migration message from bytes.
    pub fn decode(data: &[u8]) -> Result<MigrationMessage, MigrationError> {
        if data.is_empty() {
            return Err(MigrationError::StateFailed("empty message".into()));
        }

        let mut cur = std::io::Cursor::new(data);

        let msg_type = cur.get_u8();

        match msg_type {
            MSG_TAKE_SNAPSHOT => {
                if cur.remaining() < 16 {
                    return Err(MigrationError::StateFailed("truncated TakeSnapshot".into()));
                }
                Ok(MigrationMessage::TakeSnapshot {
                    daemon_origin: cur.get_u64_le(),
                    target_node: cur.get_u64_le(),
                })
            }
            MSG_SNAPSHOT_READY => {
                // daemon_origin(8) + seq_through(8) + chunk_index(4) + total_chunks(4) + len(4) = 28
                if cur.remaining() < 28 {
                    return Err(MigrationError::StateFailed(
                        "truncated SnapshotReady".into(),
                    ));
                }
                let daemon_origin = cur.get_u64_le();
                let seq_through = cur.get_u64_le();
                let chunk_index = cur.get_u32_le();
                let total_chunks = cur.get_u32_le();
                let len = cur.get_u32_le() as usize;
                // Reject structurally invalid chunks at the wire boundary so
                // malformed messages never even reach the reassembler. The
                // reassembler enforces the same invariants defensively.
                if total_chunks == 0 {
                    return Err(MigrationError::StateFailed(
                        "SnapshotReady: total_chunks must be >= 1".into(),
                    ));
                }
                if total_chunks > MAX_TOTAL_CHUNKS {
                    return Err(MigrationError::StateFailed(format!(
                        "SnapshotReady: total_chunks {} exceeds MAX_TOTAL_CHUNKS ({})",
                        total_chunks, MAX_TOTAL_CHUNKS
                    )));
                }
                if chunk_index >= total_chunks {
                    return Err(MigrationError::StateFailed(format!(
                        "SnapshotReady: chunk_index {} out of range for total_chunks {}",
                        chunk_index, total_chunks
                    )));
                }
                if len > MAX_SNAPSHOT_CHUNK_SIZE {
                    return Err(MigrationError::StateFailed(format!(
                        "SnapshotReady: chunk len {} exceeds MAX_SNAPSHOT_CHUNK_SIZE ({})",
                        len, MAX_SNAPSHOT_CHUNK_SIZE
                    )));
                }
                if cur.remaining() < len {
                    return Err(MigrationError::StateFailed(
                        "truncated snapshot payload".into(),
                    ));
                }
                let mut snapshot_bytes = vec![0u8; len];
                cur.copy_to_slice(&mut snapshot_bytes);
                Ok(MigrationMessage::SnapshotReady {
                    daemon_origin,
                    snapshot_bytes,
                    seq_through,
                    chunk_index,
                    total_chunks,
                })
            }
            MSG_RESTORE_COMPLETE => {
                if cur.remaining() < 16 {
                    return Err(MigrationError::StateFailed(
                        "truncated RestoreComplete".into(),
                    ));
                }
                Ok(MigrationMessage::RestoreComplete {
                    daemon_origin: cur.get_u64_le(),
                    restored_seq: cur.get_u64_le(),
                })
            }
            MSG_REPLAY_COMPLETE => {
                if cur.remaining() < 16 {
                    return Err(MigrationError::StateFailed(
                        "truncated ReplayComplete".into(),
                    ));
                }
                Ok(MigrationMessage::ReplayComplete {
                    daemon_origin: cur.get_u64_le(),
                    replayed_seq: cur.get_u64_le(),
                })
            }
            MSG_CUTOVER_NOTIFY => {
                if cur.remaining() < 16 {
                    return Err(MigrationError::StateFailed(
                        "truncated CutoverNotify".into(),
                    ));
                }
                Ok(MigrationMessage::CutoverNotify {
                    daemon_origin: cur.get_u64_le(),
                    target_node: cur.get_u64_le(),
                })
            }
            MSG_CLEANUP_COMPLETE => {
                if cur.remaining() < 8 {
                    return Err(MigrationError::StateFailed(
                        "truncated CleanupComplete".into(),
                    ));
                }
                Ok(MigrationMessage::CleanupComplete {
                    daemon_origin: cur.get_u64_le(),
                })
            }
            MSG_FAILED => {
                if cur.remaining() < 8 + 2 {
                    return Err(MigrationError::StateFailed(
                        "truncated MigrationFailed header".into(),
                    ));
                }
                let daemon_origin = cur.get_u64_le();
                let code = cur.get_u16_le();
                let reason = decode_failure_reason(&mut cur, code)?;
                Ok(MigrationMessage::MigrationFailed {
                    daemon_origin,
                    reason,
                })
            }
            MSG_BUFFERED_EVENTS => {
                if cur.remaining() < 12 {
                    return Err(MigrationError::StateFailed(
                        "truncated BufferedEvents".into(),
                    ));
                }
                let daemon_origin = cur.get_u64_le();
                let count = cur.get_u32_le() as usize;
                // Bound `count` against the remaining wire bytes before
                // allocating. Each event on the wire is at least
                // CAUSAL_LINK_SIZE(24) + u32 payload_len(4) + u64
                // received_at(8) = 36 bytes (empty payload). Without this
                // check, a malformed packet could claim `count = u32::MAX`
                // and force the decoder to allocate ~4G Vec slots before
                // the per-event bound checks fire — a cheap DoS against
                // the migration subprotocol.
                use crate::adapter::net::state::causal::CAUSAL_LINK_SIZE;
                const MIN_EVENT_WIRE_SIZE: usize = CAUSAL_LINK_SIZE + 4 + 8;
                // Hard cap as defense-in-depth. Well above any realistic
                // buffered-event batch (the orchestrator sends one
                // per-daemon batch at restore-complete; millions of
                // events per daemon per migration is already pathological).
                const MAX_BUFFERED_EVENTS: usize = 1_000_000;
                let max_possible = cur.remaining() / MIN_EVENT_WIRE_SIZE;
                if count > max_possible || count > MAX_BUFFERED_EVENTS {
                    return Err(MigrationError::StateFailed(format!(
                        "BufferedEvents: count {} exceeds bound (remaining={}, \
                         min_event_size={}, max_possible={}, hard_cap={})",
                        count,
                        cur.remaining(),
                        MIN_EVENT_WIRE_SIZE,
                        max_possible,
                        MAX_BUFFERED_EVENTS,
                    )));
                }
                let mut events = Vec::with_capacity(count);
                for _ in 0..count {
                    if cur.remaining() < CAUSAL_LINK_SIZE + 4 {
                        return Err(MigrationError::StateFailed(
                            "truncated buffered event".into(),
                        ));
                    }
                    let mut link_bytes = [0u8; CAUSAL_LINK_SIZE];
                    cur.copy_to_slice(&mut link_bytes);
                    let link = CausalLink::from_bytes(&link_bytes)
                        .ok_or_else(|| MigrationError::StateFailed("invalid causal link".into()))?;
                    let payload_len = cur.get_u32_le() as usize;
                    // Per-event payload cap. Defence-in-depth against
                    // a peer that ships a buffered-events message
                    // declaring a max-u32 (4 GiB) payload — without
                    // the cap, `Vec::with_capacity(payload_len)` 30
                    // lines below would attempt the allocation. Cap
                    // at MAX_SNAPSHOT_CHUNK_SIZE, the same byte limit
                    // every other per-event wire surface uses; a real
                    // BufferedEvents stream never carries payloads
                    // larger than the snapshot chunk size.
                    if payload_len > MAX_SNAPSHOT_CHUNK_SIZE {
                        return Err(MigrationError::StateFailed(format!(
                            "buffered event payload {} exceeds per-event cap {}",
                            payload_len, MAX_SNAPSHOT_CHUNK_SIZE
                        )));
                    }
                    // Saturate-add so `payload_len + 8` can't wrap on
                    // 32-bit targets and cause the `<` check below to
                    // pass against an attacker-shaped length. The
                    // crate's primary deployment is 64-bit, but the
                    // type is `usize` and a 32-bit cdylib build would
                    // expose the wrap.
                    let need = payload_len.saturating_add(8);
                    if cur.remaining() < need {
                        return Err(MigrationError::StateFailed(
                            "truncated event payload".into(),
                        ));
                    }
                    let mut payload = vec![0u8; payload_len];
                    cur.copy_to_slice(&mut payload);
                    let received_at = cur.get_u64_le();
                    events.push(CausalEvent {
                        link,
                        payload: bytes::Bytes::from(payload),
                        received_at,
                    });
                }
                Ok(MigrationMessage::BufferedEvents {
                    daemon_origin,
                    events,
                })
            }
            MSG_ACTIVATE_TARGET => {
                if cur.remaining() < 8 {
                    return Err(MigrationError::StateFailed(
                        "truncated ActivateTarget".into(),
                    ));
                }
                Ok(MigrationMessage::ActivateTarget {
                    daemon_origin: cur.get_u64_le(),
                })
            }
            MSG_ACTIVATE_ACK => {
                if cur.remaining() < 16 {
                    return Err(MigrationError::StateFailed("truncated ActivateAck".into()));
                }
                Ok(MigrationMessage::ActivateAck {
                    daemon_origin: cur.get_u64_le(),
                    replayed_seq: cur.get_u64_le(),
                })
            }
            _ => Err(MigrationError::StateFailed(format!(
                "unknown message type: {}",
                msg_type
            ))),
        }
    }
}

/// Decode a `MigrationFailureReason` from the `MSG_FAILED` variant
/// payload. The 16-bit tag already consumed by the caller selects
/// the variant; unknown tags are rejected so forward-compat is
/// explicit rather than silent-ignore.
fn decode_failure_reason(
    cur: &mut std::io::Cursor<&[u8]>,
    code: u16,
) -> Result<MigrationFailureReason, MigrationError> {
    match code {
        0 => Ok(MigrationFailureReason::NotReady),
        1 => Ok(MigrationFailureReason::FactoryNotFound),
        2 => Ok(MigrationFailureReason::ComputeNotSupported),
        3 => {
            let msg = read_u16_string(cur, "StateFailed message")?;
            Ok(MigrationFailureReason::StateFailed(msg))
        }
        4 => Ok(MigrationFailureReason::AlreadyMigrating),
        5 => {
            let msg = read_u16_string(cur, "IdentityTransportFailed message")?;
            Ok(MigrationFailureReason::IdentityTransportFailed(msg))
        }
        6 => {
            if cur.remaining() < 1 {
                return Err(MigrationError::StateFailed(
                    "truncated NotReadyTimeout attempts field".into(),
                ));
            }
            Ok(MigrationFailureReason::NotReadyTimeout {
                attempts: cur.get_u8(),
            })
        }
        other => Err(MigrationError::StateFailed(format!(
            "unknown MigrationFailureReason code {other}",
        ))),
    }
}

fn read_u16_string(cur: &mut std::io::Cursor<&[u8]>, ctx: &str) -> Result<String, MigrationError> {
    if cur.remaining() < 2 {
        return Err(MigrationError::StateFailed(format!(
            "truncated {ctx} length prefix",
        )));
    }
    let len = cur.get_u16_le() as usize;
    if cur.remaining() < len {
        return Err(MigrationError::StateFailed(format!("truncated {ctx} body")));
    }
    let mut bytes = vec![0u8; len];
    cur.copy_to_slice(&mut bytes);
    String::from_utf8(bytes)
        .map_err(|e| MigrationError::StateFailed(format!("{ctx} is not valid UTF-8: {e}")))
}

// ── Snapshot chunking ────────────────────────────────────────────────────────

/// Maximum snapshot chunk size. Sized to fit within `MAX_PAYLOAD_SIZE` after
/// accounting for the SnapshotReady wire header overhead
/// (msg_type + daemon_origin + seq_through + chunk_index + total_chunks + len = 29 bytes)
/// and leaving headroom for the outer transport framing.
pub const MAX_SNAPSHOT_CHUNK_SIZE: usize = 7000;

/// Maximum transferable snapshot size: `u32::MAX` chunks * 7,000 bytes per chunk.
///
/// This is ~28 TB — effectively unlimited for daemon state. The `StateSnapshot`
/// wire format itself caps at ~4 GB (`state_len: u32`), so in practice the
/// snapshot serialization limit is reached first.
pub const MAX_SNAPSHOT_SIZE: usize = u32::MAX as usize * MAX_SNAPSHOT_CHUNK_SIZE;

/// Maximum `total_chunks` the reassembler will accept per reassembly.
///
/// `StateSnapshot` wire format caps payload at ~4 GB (`state_len: u32`), so
/// `ceil(u32::MAX / MAX_SNAPSHOT_CHUNK_SIZE)` ≈ 613,566 chunks is the largest
/// legitimate value. We cap above that with headroom; anything higher is an
/// attacker declaring a fake `total_chunks` to either flood us with
/// BTreeMap insertions or stall the reassembler forever waiting for chunks
/// that will never arrive.
pub const MAX_TOTAL_CHUNKS: u32 = 700_000;

/// Hard upper bound on bytes buffered for a SINGLE in-flight reassembly.
///
/// `MAX_TOTAL_CHUNKS × MAX_SNAPSHOT_CHUNK_SIZE` ≈ 4.3 GiB; combined with the
/// fact that `seq_through == latest` doesn't trigger eviction, an attacker
/// can park up to that much memory per `(daemon_origin, seq_through)` and
/// refresh forever without ever completing the snapshot. This cap is a
/// hard ceiling on the per-entry buffer regardless of the declared
/// `total_chunks`. Real daemon snapshots run in the megabytes, not
/// gigabytes — 64 MiB leaves plenty of headroom while bounding the
/// flood-amplification a malicious peer can produce.
pub const MAX_PENDING_REASSEMBLY_BYTES: usize = 64 * 1024 * 1024;

/// Maximum age of a pending reassembly entry before it is swept.
///
/// Even with the per-entry byte cap, a peer can park up to
/// `MAX_PENDING_REASSEMBLY_BYTES` indefinitely under a single
/// `(daemon_origin, seq_through)` key: the cap refuses *additional*
/// chunks once buffered bytes reach the ceiling, but it doesn't
/// evict what's already there, and the `seq_through > latest`
/// eviction never fires while the peer re-uses the same
/// `seq_through`. Across many distinct `daemon_origin` values that
/// produces unbounded growth. The age sweep closes that hole by
/// removing entries whose last-progress timestamp is older than
/// this duration. Real snapshots complete in seconds; 5 minutes
/// leaves headroom for a slow legitimate peer while bounding the
/// persistence of attacker-shaped state.
pub const MAX_PENDING_REASSEMBLY_AGE: Duration = Duration::from_secs(300);

/// Split a snapshot into chunked `SnapshotReady` messages.
///
/// Small snapshots (<= `MAX_SNAPSHOT_CHUNK_SIZE`) produce a single message
/// with `chunk_index = 0, total_chunks = 1`. Larger snapshots are split
/// into multiple messages that the receiver must reassemble.
///
/// Returns `MigrationError::SnapshotTooLarge` if the snapshot exceeds
/// `MAX_SNAPSHOT_SIZE` (~28 TB).
pub fn chunk_snapshot(
    daemon_origin: u64,
    snapshot_bytes: Vec<u8>,
    seq_through: u64,
) -> Result<Vec<MigrationMessage>, MigrationError> {
    if snapshot_bytes.len() <= MAX_SNAPSHOT_CHUNK_SIZE {
        return Ok(vec![MigrationMessage::SnapshotReady {
            daemon_origin,
            snapshot_bytes,
            seq_through,
            chunk_index: 0,
            total_chunks: 1,
        }]);
    }

    let total_chunks = snapshot_bytes.len().div_ceil(MAX_SNAPSHOT_CHUNK_SIZE);
    let total_chunks =
        u32::try_from(total_chunks).map_err(|_| MigrationError::SnapshotTooLarge {
            size: snapshot_bytes.len(),
            max: MAX_SNAPSHOT_SIZE,
        })?;

    Ok(snapshot_bytes
        .chunks(MAX_SNAPSHOT_CHUNK_SIZE)
        .enumerate()
        .map(|(i, chunk)| MigrationMessage::SnapshotReady {
            daemon_origin,
            snapshot_bytes: chunk.to_vec(),
            seq_through,
            chunk_index: i as u32,
            total_chunks,
        })
        .collect())
}

/// Why a chunk was rejected by [`SnapshotReassembler::feed`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReassemblyError {
    /// `total_chunks == 0` — a well-formed message always declares at least one.
    ZeroTotalChunks,
    /// `chunk_index >= total_chunks` — attacker trying to smuggle out-of-range
    /// indices past the "all chunks received" count check.
    ChunkIndexOutOfRange {
        /// The chunk index declared by the peer.
        chunk_index: u32,
        /// The `total_chunks` declared by the peer.
        total_chunks: u32,
    },
    /// `total_chunks > MAX_TOTAL_CHUNKS` — peer declared more chunks than any
    /// legitimate snapshot could produce.
    TotalChunksTooLarge {
        /// The `total_chunks` declared by the peer.
        total_chunks: u32,
    },
    /// An individual chunk exceeds `MAX_SNAPSHOT_CHUNK_SIZE`.
    ChunkTooLarge {
        /// The chunk length observed.
        len: usize,
    },
    /// A later chunk declared a different `total_chunks` than the first chunk
    /// for the same `(daemon_origin, seq_through)`. Peer is either buggy or
    /// trying to resize an in-flight reassembly to force extra allocations.
    TotalChunksMismatch {
        /// The value declared by the current chunk.
        got: u32,
        /// The value locked in by the first chunk.
        expected: u32,
    },
    /// Peer sent a chunk for an older `seq_through` after we already
    /// accepted a newer one for the same daemon.
    StaleSeqThrough {
        /// The `seq_through` on the incoming chunk.
        got: u64,
        /// The newest `seq_through` we've accepted for this daemon.
        latest: u64,
    },
    /// Buffered bytes for this `(daemon_origin, seq_through)` would
    /// exceed `MAX_PENDING_REASSEMBLY_BYTES`. Refusing the chunk
    /// bounds the memory amplification a peer can drive by sending
    /// only some of the chunks for an outsized declared snapshot.
    TooManyPendingBytes {
        /// Bytes already buffered for this entry.
        buffered: usize,
        /// Length of the chunk being rejected.
        incoming: usize,
        /// Per-entry cap.
        cap: usize,
    },
}

impl std::fmt::Display for ReassemblyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ZeroTotalChunks => write!(f, "total_chunks == 0"),
            Self::ChunkIndexOutOfRange {
                chunk_index,
                total_chunks,
            } => write!(
                f,
                "chunk_index {} out of range for total_chunks {}",
                chunk_index, total_chunks
            ),
            Self::TotalChunksTooLarge { total_chunks } => write!(
                f,
                "total_chunks {} exceeds MAX_TOTAL_CHUNKS ({})",
                total_chunks, MAX_TOTAL_CHUNKS
            ),
            Self::ChunkTooLarge { len } => write!(
                f,
                "chunk length {} exceeds MAX_SNAPSHOT_CHUNK_SIZE ({})",
                len, MAX_SNAPSHOT_CHUNK_SIZE
            ),
            Self::TotalChunksMismatch { got, expected } => write!(
                f,
                "total_chunks {} does not match first chunk's declared {}",
                got, expected
            ),
            Self::StaleSeqThrough { got, latest } => write!(
                f,
                "seq_through {} is older than latest accepted {} for this daemon",
                got, latest
            ),
            Self::TooManyPendingBytes {
                buffered,
                incoming,
                cap,
            } => write!(
                f,
                "buffered {} + incoming {} would exceed per-entry cap {}",
                buffered, incoming, cap
            ),
        }
    }
}

impl std::error::Error for ReassemblyError {}

/// Reassembles chunked `SnapshotReady` messages into a complete snapshot.
///
/// Keyed by `(daemon_origin, seq_through)` so chunks from different snapshot
/// generations cannot be mixed. At most one in-flight reassembly is kept
/// per daemon — a chunk for a newer `seq_through` evicts any older pending
/// state for that daemon, and chunks for older `seq_through` are rejected.
pub struct SnapshotReassembler {
    /// Pending reassemblies: (daemon_origin, seq_through) → chunks.
    pending: std::collections::HashMap<(u64, u64), ReassemblyState>,
    /// Latest `seq_through` accepted per daemon, for stale-chunk rejection
    /// even after a reassembly completes and is evicted from `pending`.
    latest_seq: std::collections::HashMap<u64, u64>,
    /// Max age applied by the opportunistic sweep at the head of
    /// every `feed`. Defaults to `MAX_PENDING_REASSEMBLY_AGE`. The
    /// public `sweep_stale` accepts its own `max_age` and ignores
    /// this; this field exists so tests can drive the implicit
    /// sweep without waiting wall-clock minutes.
    max_pending_age: Duration,
}

struct ReassemblyState {
    total_chunks: u32,
    chunks: std::collections::BTreeMap<u32, Vec<u8>>,
    /// Sum of `chunks` values' lengths. Maintained explicitly (rather
    /// than recomputed via `chunks.values().map(Vec::len).sum()` per
    /// feed) so the `MAX_PENDING_REASSEMBLY_BYTES` gate is O(1) per
    /// chunk instead of O(chunks).
    bytes_buffered: usize,
    /// Time of the most recent chunk arrival for this entry. Resets
    /// on every accepted chunk so a slow-but-progressing peer never
    /// trips the age sweep; a stalled entry that hasn't received a
    /// chunk in `MAX_PENDING_REASSEMBLY_AGE` is dropped by either
    /// `sweep_stale` or the opportunistic sweep at the head of
    /// `feed`.
    last_progress_at: Instant,
}

impl SnapshotReassembler {
    /// Create a new reassembler with the default
    /// `MAX_PENDING_REASSEMBLY_AGE` opportunistic-sweep age.
    pub fn new() -> Self {
        Self::with_max_pending_age(MAX_PENDING_REASSEMBLY_AGE)
    }

    /// Create a reassembler with a custom opportunistic-sweep age.
    /// Production callers should use `new()`; this exists primarily
    /// for tests that need to exercise the in-`feed` sweep without
    /// waiting wall-clock minutes.
    pub fn with_max_pending_age(max_pending_age: Duration) -> Self {
        Self {
            pending: std::collections::HashMap::new(),
            latest_seq: std::collections::HashMap::new(),
            max_pending_age,
        }
    }

    /// Feed a snapshot chunk.
    ///
    /// Returns `Ok(Some(bytes))` when all chunks for the current
    /// `(daemon_origin, seq_through)` have been received, `Ok(None)` while
    /// still waiting, and `Err(ReassemblyError)` if the chunk is malformed or
    /// part of an attacker-shaped sequence. Rejected chunks never mutate
    /// in-flight state.
    pub fn feed(
        &mut self,
        daemon_origin: u64,
        snapshot_bytes: Vec<u8>,
        seq_through: u64,
        chunk_index: u32,
        total_chunks: u32,
    ) -> Result<Option<Vec<u8>>, ReassemblyError> {
        // Opportunistic age sweep. Even without an external scheduler
        // driving `sweep_stale`, the pending map self-heals as new
        // traffic arrives, so a hostile peer who parks an entry at
        // the byte cap and goes silent can't keep it alive
        // indefinitely. Cheap: `pending` is bounded to one entry
        // per daemon and the retain is O(n) over a small map.
        self.sweep_stale(self.max_pending_age);

        // ---- Per-chunk validation (no mutation until we've passed these) ----
        if total_chunks == 0 {
            return Err(ReassemblyError::ZeroTotalChunks);
        }
        if total_chunks > MAX_TOTAL_CHUNKS {
            return Err(ReassemblyError::TotalChunksTooLarge { total_chunks });
        }
        if chunk_index >= total_chunks {
            return Err(ReassemblyError::ChunkIndexOutOfRange {
                chunk_index,
                total_chunks,
            });
        }
        // Zero-byte chunks are nonsensical: every legitimate
        // SnapshotReady carries at least one byte of state (an empty
        // snapshot would be a 1-byte length-prefixed empty payload,
        // not a 0-byte chunk). Pre-fix a peer could ship
        // MAX_TOTAL_CHUNKS = 700_000 zero-byte chunks per reassembly
        // without ever consuming the documented byte-budget guard,
        // bookkeeping `BTreeMap` entries until `MAX_TOTAL_CHUNKS`
        // alone bounded the abuse. Refuse them at the boundary.
        if snapshot_bytes.is_empty() {
            return Err(ReassemblyError::ChunkTooLarge { len: 0 });
        }
        if snapshot_bytes.len() > MAX_SNAPSHOT_CHUNK_SIZE {
            return Err(ReassemblyError::ChunkTooLarge {
                len: snapshot_bytes.len(),
            });
        }
        if let Some(&latest) = self.latest_seq.get(&daemon_origin) {
            if seq_through < latest {
                return Err(ReassemblyError::StaleSeqThrough {
                    got: seq_through,
                    latest,
                });
            }
        }

        // A newer seq_through for the same daemon evicts older in-flight state.
        // This is what the public docstring always claimed; without it, the
        // `pending` map grew unbounded across seq_through values.
        if self
            .latest_seq
            .get(&daemon_origin)
            .is_none_or(|&latest| seq_through > latest)
        {
            self.pending
                .retain(|&(origin, seq), _| origin != daemon_origin || seq == seq_through);
            self.latest_seq.insert(daemon_origin, seq_through);
        }

        // Single-chunk fast path: no state to keep. Honour the
        // total_chunks-mismatch guard before bypassing — a peer that
        // shipped chunk 0/3 for `(daemon_origin, seq_through)` and
        // followed up with chunk 0/1 for the same key would otherwise
        // have the second message accepted as a complete snapshot,
        // dodging the mismatch error the multi-chunk path below
        // returns. The dedup-by-seq_through eviction above only fires
        // when a *newer* seq_through arrives; same-key collisions
        // still need to be caught here.
        if total_chunks == 1 {
            if let Some(state) = self.pending.get(&(daemon_origin, seq_through)) {
                if state.total_chunks != 1 {
                    return Err(ReassemblyError::TotalChunksMismatch {
                        got: 1,
                        expected: state.total_chunks,
                    });
                }
            }
            self.pending.remove(&(daemon_origin, seq_through));
            return Ok(Some(snapshot_bytes));
        }

        let key = (daemon_origin, seq_through);
        let state = self.pending.entry(key).or_insert_with(|| ReassemblyState {
            total_chunks,
            chunks: std::collections::BTreeMap::new(),
            bytes_buffered: 0,
            last_progress_at: Instant::now(),
        });

        // The first chunk fixes total_chunks; later chunks must agree.
        if state.total_chunks != total_chunks {
            return Err(ReassemblyError::TotalChunksMismatch {
                got: total_chunks,
                expected: state.total_chunks,
            });
        }

        // Per-entry bytes cap. Refuse a chunk that would push the
        // accumulated buffer past `MAX_PENDING_REASSEMBLY_BYTES`.
        // Re-sending the same chunk index doesn't double-count: we
        // subtract the displaced chunk's length below before
        // re-checking. A peer that declares an oversized snapshot
        // and ships only some of the chunks can no longer park
        // ~4 GiB indefinitely — the cap forces the entry to be
        // refused once buffered bytes exceed the ceiling.
        let displaced_len = state.chunks.get(&chunk_index).map(Vec::len).unwrap_or(0);
        let projected = state
            .bytes_buffered
            .saturating_sub(displaced_len)
            .saturating_add(snapshot_bytes.len());
        if projected > MAX_PENDING_REASSEMBLY_BYTES {
            return Err(ReassemblyError::TooManyPendingBytes {
                buffered: state.bytes_buffered,
                incoming: snapshot_bytes.len(),
                cap: MAX_PENDING_REASSEMBLY_BYTES,
            });
        }

        let new_len = snapshot_bytes.len();
        state.chunks.insert(chunk_index, snapshot_bytes);
        state.bytes_buffered = state
            .bytes_buffered
            .saturating_sub(displaced_len)
            .saturating_add(new_len);
        state.last_progress_at = Instant::now();

        // With `chunk_index < total_chunks` enforced above, the BTreeMap's
        // keys are all in 0..total_chunks. Reaching total_chunks entries
        // therefore means we have every distinct index exactly once.
        if state.chunks.len() == state.total_chunks as usize {
            let state = self.pending.remove(&key).unwrap();
            let mut full = Vec::with_capacity(state.chunks.values().map(|c| c.len()).sum());
            for (_idx, chunk) in state.chunks {
                full.extend_from_slice(&chunk);
            }
            Ok(Some(full))
        } else {
            Ok(None)
        }
    }

    /// Cancel reassembly for a daemon (e.g., on migration abort).
    ///
    /// Removes all pending reassemblies for this daemon regardless of
    /// `seq_through`. Does **not** reset `latest_seq`, so a subsequent
    /// replay of old chunks is still rejected.
    pub fn cancel(&mut self, daemon_origin: u64) {
        self.pending
            .retain(|&(origin, _), _| origin != daemon_origin);
    }

    /// Drop pending reassemblies whose last-progress timestamp is
    /// older than `max_age`. Returns the number of entries evicted.
    ///
    /// Called opportunistically at the head of every `feed`, but
    /// also exposed publicly so a topology-aware caller (e.g. the
    /// migration dispatcher's housekeeping tick) can drive it on a
    /// timer when no inbound traffic is arriving — the
    /// `seq_through == latest` path in `feed` cannot self-trigger
    /// the sweep without a fresh chunk to cause it.
    ///
    /// Does **not** reset `latest_seq`: a peer that comes back later
    /// with the same `seq_through` is still rejected via the usual
    /// `StaleSeqThrough` gate, so dropping the in-flight buffer
    /// can't be turned into a snapshot-replacement amplifier.
    pub fn sweep_stale(&mut self, max_age: Duration) -> usize {
        let before = self.pending.len();
        let now = Instant::now();
        self.pending.retain(|_, state| {
            now.checked_duration_since(state.last_progress_at)
                .is_none_or(|age| age < max_age)
        });
        before - self.pending.len()
    }

    /// Number of pending reassemblies.
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }
}

impl Default for SnapshotReassembler {
    fn default() -> Self {
        Self::new()
    }
}

// ── Orchestrator ─────────────────────────────────────────────────────────────

/// Tracks an in-flight migration with its superposition state.
struct MigrationRecord {
    state: MigrationState,
    superposition: SuperpositionState,
    started_at: Instant,
}

/// One row of [`MigrationOrchestrator::list_migrations`]. Used
/// by operator-facing surfaces (Deck MIGRATIONS tab via the
/// `MigrationSnapshot` wire form, ICE blast-radius simulator).
/// Pre-`MigrationListItem` this was a `(u64, MigrationPhase, u64)`
/// tuple; the operator-facing columns outgrew the tuple's
/// readability budget, and a named struct documents which
/// field is which without per-caller comments.
#[derive(Clone, Debug)]
pub struct MigrationListItem {
    /// Origin hash of the daemon being migrated.
    pub daemon_origin: u64,
    /// Source node ID.
    pub source_node: u64,
    /// Target node ID.
    pub target_node: u64,
    /// Current phase.
    pub phase: MigrationPhase,
    /// Milliseconds since the migration started.
    pub elapsed_ms: u64,
    /// Milliseconds since the current phase was entered. Distinct
    /// from `elapsed_ms` — a migration ten minutes old that
    /// transitioned to Replay one minute ago reports `60_000` here.
    pub age_in_phase_ms: u64,
    /// Snapshot payload size in bytes; `None` while the source
    /// hasn't produced a snapshot yet.
    pub snapshot_bytes: Option<u64>,
    /// Retry attempts accumulated by orchestrator-driven retries.
    pub retries: u32,
    /// Events buffered awaiting replay.
    pub buffered_events: u32,
}

/// Outcome of [`MigrationOrchestrator::buffer_event`].
///
/// A `bool` return conflated two distinct caller responses
/// ("no migration → route to source" vs. "post-cutover →
/// route to target"); see the method's doc-comment for the
/// concrete failure mode the bool produced. Branch on this
/// enum instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BufferOutcome {
    /// Event was added to the daemon's migration buffer.
    /// The migration is between Snapshot and Replay phases.
    Buffered,
    /// A migration record exists for this daemon but it has
    /// already entered Cutover or Complete — the source has
    /// stopped accepting writes and the target is now (or
    /// will shortly become) the authoritative copy. Caller
    /// should route the event to the target node, not the
    /// source.
    PostCutover,
    /// No migration record exists for this daemon. Caller
    /// should route the event normally (to the source, which
    /// is still authoritative).
    NoMigration,
}

/// Coordinates all 6 phases of daemon migration.
///
/// The orchestrator manages the lifecycle of migrations: initiating snapshots,
/// forwarding snapshot data to targets, coordinating replay, executing cutover,
/// and cleaning up the source.
pub struct MigrationOrchestrator {
    /// In-flight migrations: daemon_origin → record.
    migrations: DashMap<u64, Mutex<MigrationRecord>>,
    /// Local daemon registry (for taking snapshots on local daemons).
    daemon_registry: Arc<DaemonRegistry>,
    /// Local node ID.
    local_node_id: u64,
    /// Source-side migration handler.
    ///
    /// Production wiring (`MigrationDispatcher::new`) sets this so
    /// the local-source path of `start_migration` registers the
    /// migration in the source-side handler — without that
    /// registration, `is_migrating(origin)` returns false and
    /// `DaemonRegistry::deliver` keeps mutating the source daemon's
    /// state past the snapshot's `seq_through`, so events arriving
    /// between snapshot capture and cutover would be silently lost.
    /// Tests that don't exercise the local-source path can leave
    /// this as `None`; the fallback is direct-snapshot behavior
    /// with a `tracing::warn!` flagging the gap.
    source_handler: Option<Arc<MigrationSourceHandler>>,
}

impl MigrationOrchestrator {
    /// Create a new orchestrator.
    ///
    /// The source-side handler defaults to `None`. Production
    /// callers should chain [`Self::with_source_handler`] to wire
    /// the orchestrator to the dispatcher's source handler — see
    /// the field doc for why that matters.
    pub fn new(daemon_registry: Arc<DaemonRegistry>, local_node_id: u64) -> Self {
        Self {
            migrations: DashMap::new(),
            daemon_registry,
            local_node_id,
            source_handler: None,
        }
    }

    /// Builder: install the source-side migration handler. Required
    /// for correct local-source migration behavior.
    /// `MigrationDispatcher::new` already calls this; tests that
    /// exercise the local-source code path must call it explicitly.
    pub fn with_source_handler(mut self, source_handler: Arc<MigrationSourceHandler>) -> Self {
        self.source_handler = Some(source_handler);
        self
    }

    /// Shared handle to the daemon registry this orchestrator was
    /// built against. Exposed so the migration subprotocol
    /// dispatcher can reach the registry without an extra `Arc`
    /// plumbed alongside.
    pub fn daemon_registry(&self) -> &Arc<DaemonRegistry> {
        &self.daemon_registry
    }

    /// Initiate a migration (phase 0: Snapshot).
    ///
    /// If the source is the local node, takes the snapshot immediately and
    /// returns `SnapshotReady`. Otherwise, returns `TakeSnapshot` for the
    /// caller to send to the source node.
    pub fn start_migration(
        &self,
        daemon_origin: u64,
        source_node: u64,
        target_node: u64,
    ) -> Result<Vec<MigrationMessage>, MigrationError> {
        // Atomic check-and-insert via entry() to prevent TOCTOU races
        let entry = match self.migrations.entry(daemon_origin) {
            dashmap::mapref::entry::Entry::Occupied(_) => {
                return Err(MigrationError::AlreadyMigrating(daemon_origin));
            }
            dashmap::mapref::entry::Entry::Vacant(entry) => entry,
        };

        let mut state = MigrationState::new(daemon_origin, source_node, target_node);

        // If we are the source, take snapshot locally.
        if source_node == self.local_node_id {
            // The snapshot MUST be routed through the source-side
            // handler so it gets registered for the migration;
            // otherwise `source_handler.is_migrating(origin)`
            // returns false and `DaemonRegistry::deliver` keeps
            // routing post-snapshot events into the live source
            // daemon's state, losing them at cutover. The remote-
            // source path goes through
            // `source_handler.start_snapshot` via the dispatcher
            // (`migration_handler.rs:310-312`); we mirror that
            // here.
            //
            // `orchestrator_node` is `local_node_id` here — for a
            // self-initiated local migration the orchestrator IS this
            // node, so the source handler's reply-routing field
            // points back to us.
            let snapshot = match &self.source_handler {
                Some(handler) => {
                    handler.start_snapshot(daemon_origin, target_node, self.local_node_id)?
                }
                None => {
                    // Surface the unwired-source-handler path
                    // loudly. Production wiring
                    // (`MigrationDispatcher::new`) always sets the
                    // handler, so this branch is only reached by
                    // tests / direct orchestrator construction.
                    // Gating this with `cfg(not(test))` and Err in
                    // production would break integration tests
                    // that link against the library compiled
                    // WITHOUT `cfg(test)`. Warn-loud is the
                    // actionable signal: operators running under
                    // tracing see the missing-handler condition
                    // without the cfg mismatch silently breaking
                    // integration tests that intentionally
                    // exercise the orchestrator without a
                    // dispatcher.
                    tracing::warn!(
                        daemon_origin = format_args!("{:#x}", daemon_origin),
                        "MigrationOrchestrator::start_migration on local source without \
                         a source_handler installed — events arriving between snapshot \
                         capture and cutover may be silently lost. \
                         Production callers wire the handler via \
                         `MigrationDispatcher::new`. Direct orchestrator construction \
                         should call `MigrationOrchestrator::with_source_handler`."
                    );
                    self.daemon_registry
                        .snapshot(daemon_origin)
                        .map_err(|e| MigrationError::StateFailed(e.to_string()))?
                        .ok_or_else(|| {
                            MigrationError::StateFailed(
                                "daemon is stateless or snapshot failed".into(),
                            )
                        })?
                }
            };

            // Surface oversized-snapshot errors as a
            // MigrationError instead of a panic that would crash the
            // dispatch task without releasing locks.
            let snapshot_bytes = snapshot
                .try_to_bytes()
                .map_err(|e| MigrationError::StateFailed(e.to_string()))?;
            let seq_through = snapshot.through_seq;

            state.set_snapshot(snapshot)?;

            let source_head = state
                .snapshot()
                .map(|s| s.chain_link)
                .unwrap_or_else(|| CausalLink::genesis(daemon_origin, 0));

            let superposition = SuperpositionState::new(daemon_origin, source_head);

            entry.insert(Mutex::new(MigrationRecord {
                state,
                superposition,
                started_at: Instant::now(),
            }));

            // Pre-fix this returned a single `SnapshotReady`
            // with `chunk_index: 0, total_chunks: 1` regardless
            // of `snapshot_bytes.len()`. Any snapshot larger
            // than `MAX_SNAPSHOT_CHUNK_SIZE` (7 KB) was
            // rejected at the wire encoder
            // (`migration_handler.rs:336`) and the receiver
            // dropped it as `ChunkTooLarge`. Locally-initiated
            // migration of any stateful daemon with a
            // non-trivial state vector (cached models, large
            // bindings, behaviour history) thus could not be
            // sent at all. Route through `chunk_snapshot` so a
            // multi-chunk snapshot returns multiple messages
            // for the caller to dispatch in order.
            chunk_snapshot(daemon_origin, snapshot_bytes, seq_through)
        } else {
            let source_head = CausalLink::genesis(daemon_origin, 0);
            let superposition = SuperpositionState::new(daemon_origin, source_head);

            entry.insert(Mutex::new(MigrationRecord {
                state,
                superposition,
                started_at: Instant::now(),
            }));

            Ok(vec![MigrationMessage::TakeSnapshot {
                daemon_origin,
                target_node,
            }])
        }
    }

    /// Initiate a migration with automatic target selection.
    ///
    /// Uses the scheduler to find the best migration-capable target node
    /// based on the daemon's capability requirements. The scheduler queries
    /// the `CapabilityIndex` for nodes advertising `subprotocol:0x0500`.
    ///
    /// Returns the target node ID and the first migration message.
    pub fn start_migration_auto(
        &self,
        daemon_origin: u64,
        source_node: u64,
        scheduler: &super::Scheduler,
        daemon_filter: &crate::adapter::net::behavior::capability::CapabilityFilter,
    ) -> Result<(u64, Vec<MigrationMessage>), MigrationError> {
        // Map a scheduler "no candidate" / "index unavailable"
        // outcome to the typed `NoTargetAvailable` variant. Pre-
        // fix this used `TargetUnavailable(0)`, surfacing
        // "target node 0x0 unavailable" to operators — confusing
        // because no specific node id was ever attempted; the
        // auto-placement found nobody to attempt against.
        // Phase G slice 8 — auto-target migration runs through v2
        // placement by default. Ranking flows via
        // `select_migration_target` + LOCKED §7 tie-breaker;
        // observable eligibility matches v1 because v2 wraps
        // `LegacyPlacement::permissive`. The legacy
        // `Scheduler::place_migration` is still available for
        // callers who explicitly want the v1 contract.
        let placement = scheduler
            .place_migration_v2(daemon_filter, source_node)
            .map_err(|_| MigrationError::NoTargetAvailable)?;

        let target_node = placement.node_id;
        let msgs = self.start_migration(daemon_origin, source_node, target_node)?;
        Ok((target_node, msgs))
    }

    /// Handle snapshot taken on source (phase 1→2).
    ///
    /// Validates and stores the snapshot, advances to Transfer phase.
    /// Returns the message to forward to the target node. For chunked
    /// snapshots, only the first chunk (index 0) triggers validation
    /// and phase advancement — subsequent chunks are forwarded as-is.
    pub fn on_snapshot_ready(
        &self,
        daemon_origin: u64,
        snapshot_bytes: Vec<u8>,
        seq_through: u64,
        chunk_index: u32,
        total_chunks: u32,
    ) -> Result<MigrationMessage, MigrationError> {
        let entry = self
            .migrations
            .get(&daemon_origin)
            .ok_or(MigrationError::DaemonNotFound(daemon_origin))?;

        let mut record = entry.lock();

        // Only validate and advance phase on the first chunk
        if chunk_index == 0 && total_chunks == 1 {
            // Single-chunk: validate immediately and set snapshot
            let snapshot = StateSnapshot::from_bytes(&snapshot_bytes).ok_or_else(|| {
                MigrationError::StateFailed("failed to parse snapshot bytes".into())
            })?;

            if record.state.phase() == MigrationPhase::Snapshot {
                record.state.set_snapshot(snapshot)?;
            }
        } else if chunk_index == 0 {
            // Multi-chunk: can't validate until target reassembles all chunks.
            // Advance phase past Snapshot so buffering and subsequent phases work.
            // The target will validate the full snapshot after reassembly.
            if record.state.phase() == MigrationPhase::Snapshot {
                record.state.force_phase(MigrationPhase::Transfer);
            }
        }

        // Update superposition on first chunk
        if chunk_index == 0 {
            record.superposition.advance(MigrationPhase::Transfer);
        }

        // Forward to target
        Ok(MigrationMessage::SnapshotReady {
            daemon_origin,
            snapshot_bytes,
            seq_through,
            chunk_index,
            total_chunks,
        })
    }

    /// Handle restore complete on target (phase 2→3).
    ///
    /// Advances to Replay phase. Returns buffered events message if there
    /// are any, or None if no events were buffered.
    pub fn on_restore_complete(
        &self,
        daemon_origin: u64,
        _restored_seq: u64,
    ) -> Result<Option<MigrationMessage>, MigrationError> {
        let entry = self
            .migrations
            .get(&daemon_origin)
            .ok_or(MigrationError::DaemonNotFound(daemon_origin))?;

        let mut record = entry.lock();

        // Advance: Transfer → Restore → Replay
        if record.state.phase() == MigrationPhase::Transfer {
            record.state.transfer_complete()?;
        }
        if record.state.phase() == MigrationPhase::Restore {
            record.state.restore_complete()?;
        }

        record.superposition.advance(MigrationPhase::Replay);

        // Drain buffered events for replay
        let events = record.state.take_buffered_events();
        if events.is_empty() {
            Ok(None)
        } else {
            Ok(Some(MigrationMessage::BufferedEvents {
                daemon_origin,
                events,
            }))
        }
    }

    /// Handle replay complete on target (phase 3→4).
    ///
    /// Returns `CutoverNotify` to send to the source node.
    pub fn on_replay_complete(
        &self,
        daemon_origin: u64,
        replayed_seq: u64,
    ) -> Result<MigrationMessage, MigrationError> {
        let entry = self
            .migrations
            .get(&daemon_origin)
            .ok_or(MigrationError::DaemonNotFound(daemon_origin))?;

        let mut record = entry.lock();

        // Query the freshly-replayed daemon for its *real* head
        // link so the superposition's `target_head` carries the
        // actual cryptographic anchor (parent_hash) — not a
        // synthetic `parent_hash: 0` that no downstream verifier
        // could ever reconcile against the chain. The replay just
        // landed on this node's daemon
        // registry, so the local host's chain head is by
        // construction the head we just produced.
        //
        // If the daemon registry doesn't have the host (a Stale
        // race after register/replace, or a snapshot-only path
        // that didn't actually populate state), fall back to a
        // synthetic link with `parent_hash: 0` and a
        // `tracing::warn!`. The continuity proof from a synthetic
        // link is unverifiable downstream — same failure mode as
        // pre-fix — but at least the operator sees the gap.
        let target_head = match self
            .daemon_registry
            .with_host(daemon_origin, |host| host.head_link())
        {
            Ok(link) => {
                // Sanity: the host's head sequence should equal
                // `replayed_seq`. If it doesn't, the replay
                // pipeline diverged from what `on_replay_complete`
                // was told. Use the host's head (it's the source
                // of truth) but log so operators can spot the
                // pipeline disagreement.
                if link.sequence != replayed_seq {
                    tracing::warn!(
                        daemon_origin = format_args!("{:#x}", daemon_origin),
                        host_seq = link.sequence,
                        replayed_seq,
                        "on_replay_complete: replayed_seq disagrees with host's chain \
                         head sequence; using host's head as the authoritative anchor"
                    );
                }
                link
            }
            Err(e) => {
                tracing::warn!(
                    daemon_origin = format_args!("{:#x}", daemon_origin),
                    error = ?e,
                    replayed_seq,
                    "on_replay_complete: daemon not registered locally; \
                     falling back to synthetic target_head with parent_hash=0 — \
                     downstream continuity-proof verification will fail"
                );
                CausalLink {
                    origin_hash: daemon_origin,
                    horizon_encoded: 0,
                    sequence: replayed_seq,
                    parent_hash: 0,
                }
            }
        };
        record.superposition.target_replayed(target_head);

        // Advance to Cutover
        if record.state.phase() == MigrationPhase::Replay {
            record.state.replay_complete()?;
        }

        record.superposition.advance(MigrationPhase::Cutover);
        record.superposition.collapse();

        let target_node = record.state.target_node();

        Ok(MigrationMessage::CutoverNotify {
            daemon_origin,
            target_node,
        })
    }

    /// Handle cutover acknowledged by source.
    ///
    /// Source has stopped accepting writes. Advances to Complete.
    pub fn on_cutover_acknowledged(&self, daemon_origin: u64) -> Result<(), MigrationError> {
        let entry = self
            .migrations
            .get(&daemon_origin)
            .ok_or(MigrationError::DaemonNotFound(daemon_origin))?;

        let mut record = entry.lock();

        if record.state.phase() == MigrationPhase::Cutover {
            record.state.cutover_complete()?;
        }

        record.superposition.advance(MigrationPhase::Complete);
        record.superposition.resolve();

        Ok(())
    }

    /// Handle cleanup complete from source (phase 5→6).
    ///
    /// The source has stopped accepting writes and freed its local daemon
    /// state. Advances Cutover→Complete on the orchestrator — the source's
    /// local `on_cutover_acknowledged` call is a no-op when the orchestrator
    /// lives on a different node (it operates on the source's local
    /// orchestrator, which has no record), so `CleanupComplete` is the
    /// authoritative signal on the orchestrator side. The record is kept in
    /// place until the target acknowledges activation via `on_activate_ack`,
    /// so the subprotocol handler still has somewhere to look up
    /// `target_node` when it needs to route `ActivateTarget`.
    pub fn on_cleanup_complete(
        &self,
        daemon_origin: u64,
    ) -> Result<MigrationMessage, MigrationError> {
        let entry = self
            .migrations
            .get(&daemon_origin)
            .ok_or(MigrationError::DaemonNotFound(daemon_origin))?;
        let mut record = entry.lock();
        if record.state.phase() == MigrationPhase::Cutover {
            record.state.cutover_complete()?;
        }
        // Also resolve `SuperpositionState` here, mirroring
        // `on_cutover_acknowledged`. On a remote orchestrator
        // `on_cutover_acknowledged` is a no-op (operates on the
        // source's local orchestrator, which has no record for
        // this daemon), so this path is the ONLY authoritative
        // one. Without the resolve, `SuperpositionState` would be
        // stuck mid-collapse and operator dashboards / readiness
        // probes / SDK handles keyed on superposition state
        // wouldn't observe resolution until `on_activate_ack`
        // removed the record entirely. The advance/resolve is
        // idempotent — safe to run on the local-orchestrator path
        // too if both signals arrive on the same node.
        record.superposition.advance(MigrationPhase::Complete);
        record.superposition.resolve();
        Ok(MigrationMessage::ActivateTarget { daemon_origin })
    }

    /// Handle activation acknowledgement from target (phase 6 terminus).
    ///
    /// The target has drained remaining events and is now the authoritative
    /// copy. This is the true end of the migration lifecycle; the record is
    /// removed here, not in `on_cleanup_complete`.
    pub fn on_activate_ack(
        &self,
        daemon_origin: u64,
        _replayed_seq: u64,
    ) -> Result<(), MigrationError> {
        self.migrations
            .remove(&daemon_origin)
            .ok_or(MigrationError::DaemonNotFound(daemon_origin))?;
        Ok(())
    }

    /// Buffer an event for a daemon that is currently migrating.
    ///
    /// Pre-fix this returned a `bool`: `true` if buffered,
    /// `false` for both "no migration" AND "migration past
    /// cutover." A caller checking `if !buffer_event(...)
    /// { route_to_source(...) }` would, post-cutover, route the
    /// event to a source that has stopped accepting writes for
    /// this daemon — silently lost. The two `false` cases need
    /// different remediation: no-migration means "route to the
    /// source as normal," post-cutover means "route to the
    /// target (the new authoritative copy)."
    ///
    /// Return a typed [`BufferOutcome`] so the caller can branch
    /// on the actual state instead of inferring the wrong
    /// behavior from an ambiguous bool.
    pub fn buffer_event(&self, daemon_origin: u64, event: CausalEvent) -> BufferOutcome {
        if let Some(entry) = self.migrations.get(&daemon_origin) {
            let mut record = entry.lock();
            let phase = record.state.phase();
            // Buffer during Snapshot through Replay phases
            if phase != MigrationPhase::Cutover && phase != MigrationPhase::Complete {
                record.state.buffer_event(event);
                return BufferOutcome::Buffered;
            }
            return BufferOutcome::PostCutover;
        }
        BufferOutcome::NoMigration
    }

    /// Abort a migration at any phase.
    ///
    /// Returns the abort message to broadcast to involved nodes.
    /// `reason` is wrapped in [`MigrationFailureReason::StateFailed`]
    /// since a generic abort doesn't fit any of the more specific
    /// variants.
    pub fn abort_migration(
        &self,
        daemon_origin: u64,
        reason: String,
    ) -> Result<MigrationMessage, MigrationError> {
        self.abort_migration_with_reason(daemon_origin, MigrationFailureReason::StateFailed(reason))
    }

    /// Abort a migration with a caller-supplied structured reason.
    ///
    /// Removes the orchestrator's record AND, if wired, also clears
    /// the matching entry on the local `MigrationSourceHandler`.
    /// Pre-fix only the orchestrator entry was removed; the source
    /// handler's `migrations` map retained the entry, so
    /// `is_migrating()` stayed true forever and `buffer_event` kept
    /// stuffing the now-undrained buffered_events vector. Subsequent
    /// retry attempts then tripped `AlreadyMigrating` against a
    /// migration the orchestrator believed was already aborted.
    pub fn abort_migration_with_reason(
        &self,
        daemon_origin: u64,
        reason: MigrationFailureReason,
    ) -> Result<MigrationMessage, MigrationError> {
        self.migrations
            .remove(&daemon_origin)
            .ok_or(MigrationError::DaemonNotFound(daemon_origin))?;

        // Clear the source-side mirror entry. `abort` is a no-op if
        // the daemon was never tracked there (e.g. remote-source
        // migration where this node is only the orchestrator), so
        // calling it unconditionally is safe.
        if let Some(source) = &self.source_handler {
            let _ = source.abort(daemon_origin);
        }

        Ok(MigrationMessage::MigrationFailed {
            daemon_origin,
            reason,
        })
    }

    /// Check if a daemon is currently being migrated.
    pub fn is_migrating(&self, daemon_origin: u64) -> bool {
        self.migrations.contains_key(&daemon_origin)
    }

    /// Get migration status for a daemon.
    pub fn status(&self, daemon_origin: u64) -> Option<MigrationPhase> {
        self.migrations
            .get(&daemon_origin)
            .map(|entry| entry.lock().state.phase())
    }

    /// Get the source node for an in-flight migration.
    pub fn source_node(&self, daemon_origin: u64) -> Option<u64> {
        self.migrations
            .get(&daemon_origin)
            .map(|entry| entry.lock().state.source_node())
    }

    /// Get the target node for an in-flight migration.
    pub fn target_node(&self, daemon_origin: u64) -> Option<u64> {
        self.migrations
            .get(&daemon_origin)
            .map(|entry| entry.lock().state.target_node())
    }

    /// Get superposition phase for a daemon.
    pub fn superposition_phase(
        &self,
        daemon_origin: u64,
    ) -> Option<crate::adapter::net::continuity::superposition::SuperpositionPhase> {
        self.migrations
            .get(&daemon_origin)
            .map(|entry| entry.lock().superposition.phase())
    }

    /// List all in-flight migrations: (daemon_origin, phase, elapsed_ms).
    pub fn list_migrations(&self) -> Vec<MigrationListItem> {
        self.migrations
            .iter()
            .map(|entry| {
                let record = entry.lock();
                let elapsed = record.started_at.elapsed().as_millis() as u64;
                MigrationListItem {
                    daemon_origin: *entry.key(),
                    source_node: record.state.source_node(),
                    target_node: record.state.target_node(),
                    phase: record.state.phase(),
                    elapsed_ms: elapsed,
                    age_in_phase_ms: record.state.age_in_phase_ms(),
                    snapshot_bytes: record.state.snapshot_size_bytes(),
                    retries: record.state.retry_count(),
                    // Saturate at `u32::MAX` instead of a raw cast.
                    // The whole point of surfacing this is to flag
                    // stuck-in-Replay migrations buffering without
                    // bound; a wrap to a small number would read as
                    // forward progress when reality is the opposite.
                    buffered_events: u32::try_from(record.state.buffered_event_count())
                        .unwrap_or(u32::MAX),
                }
            })
            .collect()
    }

    /// Number of active migrations.
    pub fn active_count(&self) -> usize {
        self.migrations.len()
    }
}

impl std::fmt::Debug for MigrationOrchestrator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MigrationOrchestrator")
            .field("active_migrations", &self.migrations.len())
            .field("local_node_id", &format!("{:#x}", self.local_node_id))
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::behavior::capability::CapabilityFilter;
    use crate::adapter::net::compute::{
        DaemonError, DaemonHost, DaemonHostConfig, DaemonRegistry, MeshDaemon,
    };
    use crate::adapter::net::identity::EntityKeypair;
    use bytes::{BufMut, Bytes};

    struct CounterDaemon {
        count: u64,
    }

    impl CounterDaemon {
        fn new() -> Self {
            Self { count: 0 }
        }
    }

    impl MeshDaemon for CounterDaemon {
        fn name(&self) -> &str {
            "counter"
        }
        fn requirements(&self) -> CapabilityFilter {
            CapabilityFilter::default()
        }
        fn process(&mut self, _event: &CausalEvent) -> Result<Vec<Bytes>, DaemonError> {
            self.count += 1;
            Ok(vec![Bytes::from(self.count.to_le_bytes().to_vec())])
        }
        fn snapshot(&self) -> Option<Bytes> {
            Some(Bytes::from(self.count.to_le_bytes().to_vec()))
        }
        fn restore(&mut self, state: Bytes) -> Result<(), DaemonError> {
            if state.len() != 8 {
                return Err(DaemonError::RestoreFailed("bad state size".into()));
            }
            self.count = u64::from_le_bytes(state[..8].try_into().unwrap());
            Ok(())
        }
    }

    fn setup_registry() -> (Arc<DaemonRegistry>, u64) {
        let reg = Arc::new(DaemonRegistry::new());
        let kp = EntityKeypair::generate();
        let origin = kp.origin_hash();
        let host = DaemonHost::new(
            Box::new(CounterDaemon::new()),
            kp,
            DaemonHostConfig::default(),
        );
        reg.register(host).unwrap();
        (reg, origin)
    }

    /// CR-32: pin that the unwired-source-handler path emits a
    /// loud `tracing::warn!` referencing `source_handler`. Pre-fix
    /// this path silently fell back to `daemon_registry.snapshot`
    /// — events arriving between snapshot capture and cutover got
    /// lost.
    ///
    /// History: an earlier attempt added a `cfg(not(test))` gate
    /// that returned `Err` in production. That broke integration
    /// tests because they link against the library compiled
    /// WITHOUT `cfg(test)`. Reverted to warn-loud + still-fall-
    /// back. Production callers wire `source_handler` via
    /// `MigrationDispatcher::new`; the warn fires only on direct
    /// orchestrator construction (tests / SDK consumers who skip
    /// the dispatcher).
    ///
    /// Tripwire pins the warn-message shape so a future maintainer
    /// who removes the `tracing::warn!` (or drops the
    /// `source_handler` reference) trips the test.
    #[test]
    fn cr32_unwired_source_handler_must_emit_loud_warn() {
        let src = include_str!("orchestrator.rs");

        // Anchor on a string we BUILD at runtime so this test's
        // own source doesn't contain the verbatim anchor —
        // otherwise `src.find(anchor)` would match the test's own
        // literal and the test would silently pass even after the
        // production warn block was removed.
        //
        // The runtime-assembled anchor only matches the
        // production comment line, which writes the marker as
        // a plain English phrase rather than concatenating
        // fragments.
        let anchor = format!("{}{}{}", "Surface ", "the unwired-", "source-handler");
        let anchor_idx = src.find(&anchor).expect(
            "regression: the production unwired-source-handler marker in \
             start_migration's None arm is gone — either the fix was reverted or \
             the comment was rewritten. If the fix is intentionally being changed, \
             update this test.",
        );

        // Sanity: the anchor must occur exactly ONCE so the
        // production block is the only match. The earlier shape
        // would falsely pass if the anchor existed anywhere else
        // in the file, including in this test's own source.
        let occurrences = src.matches(&anchor).count();
        assert_eq!(
            occurrences, 1,
            "anchor must occur exactly once in orchestrator.rs (production \
             site). Got {occurrences} occurrences — the test source likely contains \
             a verbatim copy of the anchor, defeating the tripwire."
        );

        let block: String = src[anchor_idx..]
            .lines()
            .take(20)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            block.contains("tracing::warn!"),
            "regression: unwired-source-handler path must emit \
             tracing::warn!. Block:\n{}",
            block
        );
        assert!(
            block.contains("source_handler"),
            "regression: warn message must reference source_handler"
        );
        assert!(
            block.contains("MigrationDispatcher::new") || block.contains("with_source_handler"),
            "regression: warn message must point operators at how to wire \
             the handler (`MigrationDispatcher::new` or \
             `MigrationOrchestrator::with_source_handler`) so the log line is \
             actionable. Block:\n{}",
            block
        );
    }

    #[test]
    fn test_start_migration_local_source() {
        let (reg, origin) = setup_registry();
        let orch = MigrationOrchestrator::new(reg, 0x1111);

        let msgs = orch.start_migration(origin, 0x1111, 0x2222).unwrap();
        assert!(!msgs.is_empty(), "must emit at least one chunk");
        match &msgs[0] {
            MigrationMessage::SnapshotReady { daemon_origin, .. } => {
                assert_eq!(*daemon_origin, origin);
            }
            other => panic!("expected SnapshotReady, got {:?}", other),
        }

        assert!(orch.is_migrating(origin));
        assert_eq!(orch.status(origin), Some(MigrationPhase::Transfer));
    }

    /// Regression: pre-fix the local-source path called
    /// `daemon_registry.snapshot()` directly and never invoked
    /// `MigrationSourceHandler::start_snapshot`. The source-side
    /// handler had no record of the migration; `is_migrating(origin)`
    /// returned false; callers that consulted the source handler to
    /// gate their buffering / write-rejection paths skipped them.
    /// At cutover, `on_cutover` returned `DaemonNotFound` and the
    /// dispatcher's tolerance fallback swallowed any buffered
    /// events that *might* have been collected. The fix wires the
    /// orchestrator to the source handler via `with_source_handler`
    /// and routes the local-source path through
    /// `source_handler.start_snapshot`. Pin both halves of the
    /// post-fix invariant directly.
    #[test]
    fn local_source_migration_registers_in_source_handler() {
        let (reg, origin) = setup_registry();
        let source_handler = Arc::new(MigrationSourceHandler::new(reg.clone()));
        let orch =
            MigrationOrchestrator::new(reg, 0x1111).with_source_handler(source_handler.clone());

        // Pre-condition: no migration registered anywhere.
        assert!(!source_handler.is_migrating(origin));
        assert!(!orch.is_migrating(origin));

        let _ = orch.start_migration(origin, 0x1111, 0x2222).unwrap();

        // Post-condition: BOTH the orchestrator and the source
        // handler have records of the migration. Pre-fix only the
        // orchestrator had a record; the source handler's
        // `is_migrating(origin)` returned false (the
        // failure mode).
        assert!(
            source_handler.is_migrating(origin),
            "regression: source_handler must have a record \
             of the local-source migration after `start_migration` \
             returns",
        );
        assert!(orch.is_migrating(origin));
    }

    /// Regression: pin that the dispatcher's cutover path now finds
    /// a real `source_handler` record for local-source migrations
    /// and correctly drains buffered events. Pre-fix `on_cutover`
    /// returned `DaemonNotFound` for any local-source migration
    /// (the orchestrator never called `start_snapshot`), and the
    /// dispatcher's tolerance fallback (`migration_handler.rs:537`)
    /// swallowed the error and treated the cutover as having no
    /// buffered events to forward. Post-fix `on_cutover` finds the
    /// record and returns the buffered events for forwarding.
    /// A future refactor that drops the `start_snapshot` wire-up
    /// in the orchestrator's local branch would silently regress
    /// this end-to-end drain — this test pins it directly.
    #[test]
    fn local_source_cutover_drains_buffered_events_through_source_handler() {
        use crate::adapter::net::state::causal::CausalEvent;
        use bytes::Bytes;

        let (reg, origin) = setup_registry();
        let source_handler = Arc::new(MigrationSourceHandler::new(reg.clone()));
        let orch =
            MigrationOrchestrator::new(reg, 0x1111).with_source_handler(source_handler.clone());

        let _ = orch.start_migration(origin, 0x1111, 0x2222).unwrap();

        // Buffer two events through the source handler — the
        // dispatcher will drain these on cutover.
        for seq in 1..=2u64 {
            let event = CausalEvent {
                link: CausalLink {
                    origin_hash: origin,
                    horizon_encoded: 0,
                    sequence: seq,
                    parent_hash: 0,
                },
                payload: Bytes::from_static(b"buffered"),
                received_at: 0,
            };
            assert!(source_handler.buffer_event(origin, event).unwrap());
        }

        let drained = source_handler
            .on_cutover(origin)
            .expect("post-fix on_cutover must find the local-source migration record");
        assert_eq!(
            drained.len(),
            2,
            "cutover must drain the buffered events for forwarding to target — \
             pre-fix this returned `DaemonNotFound` for local-source migrations \
             and the buffered events were silently lost",
        );
    }

    /// Regression: with the source handler registered,
    /// `source_handler.buffer_event` is now invokable for a
    /// local-source migration. Pre-fix it returned `Ok(false)`
    /// ("no migration active") because `start_snapshot` had never
    /// run. Pin the fix-enabled-functionality directly: a caller
    /// that funnels post-snapshot events through
    /// `source_handler.buffer_event` gets them buffered (and
    /// drainable at cutover via `on_cutover`).
    #[test]
    fn local_source_migration_enables_source_handler_buffering() {
        use crate::adapter::net::state::causal::CausalEvent;
        use bytes::Bytes;

        let (reg, origin) = setup_registry();
        let source_handler = Arc::new(MigrationSourceHandler::new(reg.clone()));
        let orch =
            MigrationOrchestrator::new(reg, 0x1111).with_source_handler(source_handler.clone());

        let _ = orch.start_migration(origin, 0x1111, 0x2222).unwrap();

        // Now post-snapshot events can be buffered via the source
        // handler. Pre-fix this returned `Ok(false)` because
        // `is_migrating(origin)` was false.
        let event = CausalEvent {
            link: CausalLink {
                origin_hash: origin,
                horizon_encoded: 0,
                sequence: 1,
                parent_hash: 0,
            },
            payload: Bytes::from_static(b"post-snapshot event"),
            received_at: 0,
        };
        let buffered = source_handler.buffer_event(origin, event).unwrap();
        assert!(
            buffered,
            "fix must enable source-handler buffering for \
             local-source migrations — pre-fix `buffer_event` returned \
             `Ok(false)` because the migration was never registered",
        );

        let drained = source_handler.take_buffered_events(origin).unwrap();
        assert_eq!(
            drained.len(),
            1,
            "buffered event must be drainable through the source handler",
        );
    }

    #[test]
    fn test_start_migration_remote_source() {
        let (reg, origin) = setup_registry();
        let orch = MigrationOrchestrator::new(reg, 0x3333);

        let msgs = orch.start_migration(origin, 0x1111, 0x2222).unwrap();
        assert_eq!(
            msgs.len(),
            1,
            "remote-source path emits exactly one TakeSnapshot"
        );
        match &msgs[0] {
            MigrationMessage::TakeSnapshot {
                daemon_origin,
                target_node,
            } => {
                assert_eq!(*daemon_origin, origin);
                assert_eq!(*target_node, 0x2222);
            }
            other => panic!("expected TakeSnapshot, got {:?}", other),
        }

        assert_eq!(orch.status(origin), Some(MigrationPhase::Snapshot));
    }

    #[test]
    fn test_duplicate_migration_rejected() {
        let (reg, origin) = setup_registry();
        let orch = MigrationOrchestrator::new(reg, 0x1111);

        orch.start_migration(origin, 0x1111, 0x2222).unwrap();
        let err = orch.start_migration(origin, 0x1111, 0x3333).unwrap_err();
        assert_eq!(err, MigrationError::AlreadyMigrating(origin));
    }

    /// Regression: `start_migration_auto` returns
    /// `MigrationError::NoTargetAvailable` (not
    /// `TargetUnavailable(0)`) when the scheduler finds no
    /// candidate satisfying the daemon's capability filter.
    /// Pre-fix the auto path constructed
    /// `TargetUnavailable(0)`, surfacing "target node 0x0
    /// unavailable" to operators — confusing because no
    /// specific node id was ever attempted; the auto-placement
    /// found nobody to attempt against.
    #[test]
    fn start_migration_auto_returns_no_target_available_when_scheduler_finds_nothing() {
        use crate::adapter::net::behavior::capability::{CapabilityIndex, CapabilitySet};

        let (reg, origin) = setup_registry();
        let orch = MigrationOrchestrator::new(reg, 0x1111);

        // Empty index — no candidate nodes anywhere.
        let index = Arc::new(CapabilityIndex::new());
        let scheduler = super::super::Scheduler::new(index, 0x1111, CapabilitySet::default());

        // A filter that nothing in the empty index can satisfy.
        let filter = CapabilityFilter::default();

        let err = orch
            .start_migration_auto(origin, 0x1111, &scheduler, &filter)
            .unwrap_err();
        assert_eq!(
            err,
            MigrationError::NoTargetAvailable,
            "auto-placement with no candidates must surface as \
             NoTargetAvailable, not TargetUnavailable(0). The 0 \
             was a fake node id that pre-fix appeared in operator \
             error logs as `target node 0x0 unavailable`."
        );
    }

    #[test]
    fn test_abort_migration() {
        let (reg, origin) = setup_registry();
        let orch = MigrationOrchestrator::new(reg, 0x1111);

        orch.start_migration(origin, 0x1111, 0x2222).unwrap();
        assert!(orch.is_migrating(origin));

        let msg = orch.abort_migration(origin, "test abort".into()).unwrap();
        match msg {
            MigrationMessage::MigrationFailed { reason, .. } => {
                // `abort_migration` wraps its string in `StateFailed`.
                match reason {
                    MigrationFailureReason::StateFailed(msg) => {
                        assert_eq!(msg, "test abort")
                    }
                    other => panic!("expected StateFailed, got {other:?}"),
                }
            }
            _ => panic!("expected MigrationFailed"),
        }

        assert!(!orch.is_migrating(origin));
    }

    /// Regression: pre-fix `abort_migration_with_reason` only
    /// removed the orchestrator's record. The matching
    /// `MigrationSourceHandler` entry stayed put, so
    /// `is_migrating()` on the source remained `true`,
    /// `buffer_event` kept appending to a never-drained vector,
    /// and a retry trip-tested `AlreadyMigrating` against a
    /// migration the orchestrator believed was aborted. The fix
    /// calls `source.abort` from the orchestrator's abort path.
    #[test]
    fn abort_migration_propagates_to_source_handler() {
        use crate::adapter::net::compute::migration_source::MigrationSourceHandler;
        let (reg, origin) = setup_registry();
        let source = Arc::new(MigrationSourceHandler::new(reg.clone()));
        let orch = MigrationOrchestrator::new(reg, 0x1111).with_source_handler(source.clone());

        // Stand up the migration end-to-end via the orchestrator's
        // `start_migration`, which records on BOTH sides (the
        // orchestrator's record AND the source handler's mirror).
        // Pre-fix the orchestrator's `abort_migration_with_reason`
        // only cleared its own record, leaving the source mirror
        // intact.
        orch.start_migration(origin, 0x1111, 0x2222).unwrap();
        assert!(
            orch.is_migrating(origin),
            "orchestrator records the migration"
        );
        assert!(
            source.is_migrating(origin),
            "source handler also records the migration via the orchestrator",
        );

        // Abort. Both sides must clear.
        orch.abort_migration(origin, "test abort".into()).unwrap();
        assert!(
            !orch.is_migrating(origin),
            "orchestrator must clear its record"
        );
        assert!(
            !source.is_migrating(origin),
            "source handler must also clear its mirror entry on abort",
        );

        // The decisive sealed property: a fresh
        // `start_migration` for the same daemon now succeeds.
        // Pre-fix this would `AlreadyMigrating` because the source
        // handler still tracked the daemon.
        orch.start_migration(origin, 0x1111, 0x3333).unwrap();
    }

    #[test]
    fn test_event_buffering() {
        let (reg, origin) = setup_registry();
        let orch = MigrationOrchestrator::new(reg, 0x3333);

        orch.start_migration(origin, 0x1111, 0x2222).unwrap();

        let event = CausalEvent {
            link: CausalLink::genesis(origin, 0),
            payload: Bytes::from_static(b"test"),
            received_at: 0,
        };

        assert_eq!(orch.buffer_event(origin, event), BufferOutcome::Buffered);
        assert_eq!(
            orch.buffer_event(
                0xDEAD,
                CausalEvent {
                    link: CausalLink::genesis(0xDEAD, 0),
                    payload: Bytes::from_static(b"nope"),
                    received_at: 0,
                }
            ),
            BufferOutcome::NoMigration,
        );
    }

    /// Regression: `buffer_event` must distinguish "no
    /// migration" from "migration past cutover" via the
    /// `BufferOutcome` enum. Pre-fix both cases collapsed to
    /// `false`, so a caller running
    /// `if !orch.buffer_event(...) { route_to_source(...) }`
    /// would, post-cutover, route the event to the source —
    /// which has stopped accepting writes for this daemon, so
    /// the event was silently lost.
    #[test]
    fn buffer_event_distinguishes_post_cutover_from_no_migration() {
        let (reg, origin) = setup_registry();
        let orch = MigrationOrchestrator::new(reg, 0x3333);

        let event = || CausalEvent {
            link: CausalLink::genesis(origin, 0),
            payload: Bytes::from_static(b"test"),
            received_at: 0,
        };

        // Case A: no migration record at all.
        assert_eq!(
            orch.buffer_event(origin, event()),
            BufferOutcome::NoMigration,
            "buffer_event with no migration must surface as NoMigration"
        );

        // Case B: migration in Snapshot phase — event buffered.
        orch.start_migration(origin, 0x1111, 0x2222).unwrap();
        assert_eq!(
            orch.buffer_event(origin, event()),
            BufferOutcome::Buffered,
            "buffer_event during Snapshot must surface as Buffered"
        );

        // Force the migration into Cutover via the test-only
        // phase setter. We can't drive a real cutover here
        // without going through the full handler protocol, but
        // BufferOutcome only inspects `state.phase()`.
        {
            let entry = orch.migrations.get(&origin).unwrap();
            let mut record = entry.lock();
            record.state.force_phase(MigrationPhase::Cutover);
        }

        // Case C: migration past cutover — must NOT collapse
        // to NoMigration. Caller needs to route to target.
        assert_eq!(
            orch.buffer_event(origin, event()),
            BufferOutcome::PostCutover,
            "buffer_event in Cutover phase must surface as PostCutover, \
             not NoMigration. Pre-fix the bool conflated these and \
             callers routed post-cutover events to the source, where \
             they were silently lost."
        );
    }

    #[test]
    fn test_wire_roundtrip_take_snapshot() {
        let msg = MigrationMessage::TakeSnapshot {
            daemon_origin: 0xAAAA,
            target_node: 0x2222,
        };
        let encoded = wire::encode(&msg).unwrap();
        let decoded = wire::decode(&encoded).unwrap();
        match decoded {
            MigrationMessage::TakeSnapshot {
                daemon_origin,
                target_node,
            } => {
                assert_eq!(daemon_origin, 0xAAAA);
                assert_eq!(target_node, 0x2222);
            }
            _ => panic!("expected TakeSnapshot"),
        }
    }

    #[test]
    fn test_wire_roundtrip_snapshot_ready() {
        let msg = MigrationMessage::SnapshotReady {
            daemon_origin: 0xBBBB,
            snapshot_bytes: vec![1, 2, 3, 4, 5],
            seq_through: 42,
            chunk_index: 0,
            total_chunks: 1,
        };
        let encoded = wire::encode(&msg).unwrap();
        let decoded = wire::decode(&encoded).unwrap();
        match decoded {
            MigrationMessage::SnapshotReady {
                daemon_origin,
                snapshot_bytes,
                seq_through,
                chunk_index,
                total_chunks,
            } => {
                assert_eq!(daemon_origin, 0xBBBB);
                assert_eq!(snapshot_bytes, vec![1, 2, 3, 4, 5]);
                assert_eq!(seq_through, 42);
                assert_eq!(chunk_index, 0);
                assert_eq!(total_chunks, 1);
            }
            _ => panic!("expected SnapshotReady"),
        }
    }

    #[test]
    fn test_chunk_snapshot_small() {
        let chunks = chunk_snapshot(0xAAAA, vec![1, 2, 3], 10).unwrap();
        assert_eq!(chunks.len(), 1);
        match &chunks[0] {
            MigrationMessage::SnapshotReady {
                chunk_index,
                total_chunks,
                snapshot_bytes,
                ..
            } => {
                assert_eq!(*chunk_index, 0);
                assert_eq!(*total_chunks, 1);
                assert_eq!(snapshot_bytes, &[1, 2, 3]);
            }
            _ => panic!("expected SnapshotReady"),
        }
    }

    #[test]
    fn test_chunk_snapshot_large() {
        // Create a snapshot larger than MAX_SNAPSHOT_CHUNK_SIZE
        let big = vec![0xABu8; MAX_SNAPSHOT_CHUNK_SIZE * 3 + 100];
        let total_len = big.len();
        let chunks = chunk_snapshot(0xBBBB, big, 42).unwrap();

        assert_eq!(chunks.len(), 4); // 3 full + 1 partial

        // Verify chunk metadata
        for (i, chunk) in chunks.iter().enumerate() {
            match chunk {
                MigrationMessage::SnapshotReady {
                    chunk_index,
                    total_chunks,
                    daemon_origin,
                    seq_through,
                    ..
                } => {
                    assert_eq!(*chunk_index, i as u32);
                    assert_eq!(*total_chunks, 4);
                    assert_eq!(*daemon_origin, 0xBBBB);
                    assert_eq!(*seq_through, 42);
                }
                _ => panic!("expected SnapshotReady"),
            }
        }

        // Verify reassembly
        let mut reassembler = SnapshotReassembler::new();
        for chunk in chunks {
            if let MigrationMessage::SnapshotReady {
                daemon_origin,
                snapshot_bytes,
                seq_through,
                chunk_index,
                total_chunks,
            } = chunk
            {
                let result = reassembler
                    .feed(
                        daemon_origin,
                        snapshot_bytes,
                        seq_through,
                        chunk_index,
                        total_chunks,
                    )
                    .expect("legitimate chunks must not be rejected");
                if chunk_index < total_chunks - 1 {
                    assert!(result.is_none());
                } else {
                    let full = result.expect("last chunk should complete reassembly");
                    assert_eq!(full.len(), total_len);
                    assert!(full.iter().all(|&b| b == 0xAB));
                }
            }
        }
    }

    #[test]
    fn test_reassembler_cancel() {
        let mut reassembler = SnapshotReassembler::new();
        reassembler.feed(0xAAAA, vec![1, 2], 10, 0, 3).unwrap();
        assert_eq!(reassembler.pending_count(), 1);
        reassembler.cancel(0xAAAA);
        assert_eq!(reassembler.pending_count(), 0);
    }

    // ---- Regression tests: SnapshotReassembler DoS / forgery holes ----

    #[test]
    fn test_regression_reassembler_rejects_chunk_index_out_of_range() {
        // Regression: feed() never checked that `chunk_index < total_chunks`,
        // so an attacker could declare total_chunks=3 and feed indices
        // {0, 5, 7}. The BTreeMap happily stored them, `chunks.len() == 3 ==
        // total_chunks` fired "complete", and the reassembler concatenated
        // three non-contiguous chunks as if they were chunks 0,1,2 —
        // silently forging a snapshot from attacker-chosen partial content.
        //
        // Fix: feed() rejects any chunk with `chunk_index >= total_chunks`
        // before touching state.
        let mut reassembler = SnapshotReassembler::new();

        let r0 = reassembler.feed(0xAAAA, vec![1; 10], 1, 0, 3);
        assert!(r0.is_ok(), "in-range chunk must be accepted: {:?}", r0);

        let forged = reassembler.feed(0xAAAA, vec![2; 10], 1, 5, 3);
        assert!(
            matches!(
                forged,
                Err(ReassemblyError::ChunkIndexOutOfRange {
                    chunk_index: 5,
                    total_chunks: 3,
                })
            ),
            "chunk_index=5 with total_chunks=3 must be rejected, got {:?}",
            forged
        );

        // The reassembly must not have "completed" from the forged chunk —
        // still waiting for real chunks 1 and 2.
        assert_eq!(
            reassembler.pending_count(),
            1,
            "state must stay in-flight after rejected chunk"
        );
    }

    #[test]
    fn test_regression_reassembler_rejects_zero_total_chunks() {
        // Regression: total_chunks == 0 created a ReassemblyState that
        // could never complete (len check 0 == 0 never true after the
        // first insert), leaking memory. Fix: reject at the entry point.
        let mut reassembler = SnapshotReassembler::new();
        let result = reassembler.feed(0xAAAA, vec![1; 10], 1, 0, 0);
        assert!(matches!(result, Err(ReassemblyError::ZeroTotalChunks)));
        assert_eq!(reassembler.pending_count(), 0);
    }

    #[test]
    fn test_regression_reassembler_caps_total_chunks() {
        // Regression: an attacker could declare total_chunks = u32::MAX
        // and flood the BTreeMap with up to ~4B insertions before any
        // completion check would fire. Fix: cap total_chunks at
        // MAX_TOTAL_CHUNKS (well above any legitimate snapshot).
        let mut reassembler = SnapshotReassembler::new();
        let result = reassembler.feed(0xAAAA, vec![1; 10], 1, 0, u32::MAX);
        assert!(matches!(
            result,
            Err(ReassemblyError::TotalChunksTooLarge {
                total_chunks: u32::MAX
            })
        ));
        assert_eq!(reassembler.pending_count(), 0);
    }

    #[test]
    fn test_regression_reassembler_rejects_oversized_chunk() {
        // Defense in depth: even if the transport framing lets a larger
        // payload through, the reassembler refuses a single chunk bigger
        // than MAX_SNAPSHOT_CHUNK_SIZE.
        let mut reassembler = SnapshotReassembler::new();
        let oversized = vec![0u8; MAX_SNAPSHOT_CHUNK_SIZE + 1];
        let result = reassembler.feed(0xAAAA, oversized, 1, 0, 3);
        assert!(
            matches!(result, Err(ReassemblyError::ChunkTooLarge { .. })),
            "got {:?}",
            result
        );
    }

    #[test]
    fn test_regression_reassembler_rejects_total_chunks_mismatch() {
        // Regression: an attacker who opened a reassembly with
        // total_chunks=3 could send a later chunk declaring total_chunks=100
        // and the code would just keep inserting. Fix: the first chunk's
        // total_chunks is locked in; later chunks must agree.
        let mut reassembler = SnapshotReassembler::new();
        reassembler.feed(0xAAAA, vec![1; 10], 1, 0, 3).unwrap();
        let result = reassembler.feed(0xAAAA, vec![2; 10], 1, 1, 100);
        assert!(
            matches!(
                result,
                Err(ReassemblyError::TotalChunksMismatch {
                    got: 100,
                    expected: 3,
                })
            ),
            "got {:?}",
            result
        );
        assert_eq!(reassembler.pending_count(), 1);
    }

    /// Zero-byte chunks are rejected at the boundary. Pre-fix
    /// `MAX_TOTAL_CHUNKS = 700_000` zero-byte chunks could be
    /// admitted per reassembly without the byte-budget cap firing —
    /// nonsensical for legitimate snapshots and a cheap way to
    /// inflate BTreeMap bookkeeping.
    #[test]
    fn reassembler_refuses_zero_byte_chunk() {
        let mut reassembler = SnapshotReassembler::new();
        let result = reassembler.feed(0xAAAA, vec![], 1, 0, 3);
        assert!(
            matches!(result, Err(ReassemblyError::ChunkTooLarge { len: 0 })),
            "got {:?}",
            result
        );
        assert_eq!(reassembler.pending_count(), 0);
    }

    /// The total_chunks==1 fast path must not bypass the
    /// total_chunks-mismatch guard. A peer that opened reassembly
    /// with chunk 0/3 for `(daemon, seq)` could otherwise follow up
    /// with chunk 0/1 for the same key and have the second payload
    /// accepted as a complete snapshot — substituting its content
    /// for the in-flight multi-chunk one. The fast path now consults
    /// `pending` first and surfaces TotalChunksMismatch when the
    /// declared total disagrees with the in-flight state.
    #[test]
    fn fast_path_rejects_single_chunk_after_multi_chunk_state() {
        let mut reassembler = SnapshotReassembler::new();
        // Open with chunk 0/3.
        reassembler.feed(0xAAAA, vec![1; 10], 7, 0, 3).unwrap();
        // Attacker follows up declaring total_chunks=1 for same key.
        let result = reassembler.feed(0xAAAA, vec![2; 10], 7, 0, 1);
        assert!(
            matches!(
                result,
                Err(ReassemblyError::TotalChunksMismatch {
                    got: 1,
                    expected: 3,
                })
            ),
            "fast path must refuse substitution; got {:?}",
            result
        );
        // The original in-flight reassembly must still exist; the
        // attempted substitution must not have evicted it.
        assert_eq!(reassembler.pending_count(), 1);
    }

    #[test]
    fn test_regression_reassembler_evicts_older_seq_per_daemon() {
        // Regression: `pending` was keyed by (daemon_origin, seq_through)
        // and a fresh seq_through did NOT evict older pending reassemblies
        // for the same daemon. A peer could open unbounded in-flight
        // entries by incrementing seq_through forever.
        //
        // Fix: at most one in-flight reassembly per daemon. A newer
        // seq_through evicts older ones; older seq_through values are
        // rejected as stale.
        let mut reassembler = SnapshotReassembler::new();

        reassembler.feed(0xAAAA, vec![1; 10], 10, 0, 3).unwrap();
        reassembler.feed(0xAAAA, vec![1; 10], 11, 0, 3).unwrap();
        reassembler.feed(0xAAAA, vec![1; 10], 12, 0, 3).unwrap();

        assert_eq!(
            reassembler.pending_count(),
            1,
            "only the newest seq_through for a daemon should remain in flight"
        );

        // A stale seq_through is rejected — not silently dropped on the floor.
        let stale = reassembler.feed(0xAAAA, vec![1; 10], 5, 0, 3);
        assert!(
            matches!(
                stale,
                Err(ReassemblyError::StaleSeqThrough { got: 5, latest: 12 })
            ),
            "stale seq_through must be rejected, got {:?}",
            stale
        );
        assert_eq!(reassembler.pending_count(), 1);
    }

    /// Regression: a peer-driven reassembly that declares a large
    /// `total_chunks` and ships chunks just up to the per-entry
    /// byte cap is refused, rather than silently parking memory
    /// indefinitely. Pre-fix `MAX_TOTAL_CHUNKS × MAX_SNAPSHOT_CHUNK_SIZE`
    /// could buffer ~4.3 GiB per `(origin, seq)` key forever
    /// because the eviction at `seq_through > latest` doesn't fire
    /// when an attacker re-uses the same `seq_through`.
    #[test]
    fn reassembler_refuses_chunk_that_overflows_pending_byte_cap() {
        let mut reassembler = SnapshotReassembler::new();

        // Pre-fill to just under the cap. Each chunk is the max
        // legal chunk size; we send unique indices so no chunk is
        // displaced.
        let chunk_full = vec![0xCCu8; MAX_SNAPSHOT_CHUNK_SIZE];
        let chunks_to_fill = MAX_PENDING_REASSEMBLY_BYTES / MAX_SNAPSHOT_CHUNK_SIZE;
        // Choose a `total_chunks` that fits the prefill + at least
        // two more — so the entry is still incomplete after prefill
        // and the next chunk lands in the same key.
        let total_chunks = (chunks_to_fill as u32) + 2;
        for i in 0..(chunks_to_fill as u32) {
            reassembler
                .feed(0xAAAA, chunk_full.clone(), 1, i, total_chunks)
                .unwrap();
        }

        // The next chunk would push buffered past the cap. It must
        // be refused with `TooManyPendingBytes`, not silently
        // accepted.
        let next_idx = chunks_to_fill as u32;
        let result = reassembler.feed(0xAAAA, chunk_full.clone(), 1, next_idx, total_chunks);
        assert!(
            matches!(result, Err(ReassemblyError::TooManyPendingBytes { .. })),
            "chunk that would overflow the per-entry cap must be refused, got {:?}",
            result,
        );

        // Re-sending an index that is ALREADY buffered must succeed
        // (the displaced chunk's bytes are subtracted before the cap
        // re-check). Pin this so the cap doesn't break legitimate
        // duplicate-chunk delivery.
        let resend = reassembler.feed(0xAAAA, chunk_full.clone(), 1, 0, total_chunks);
        assert!(
            resend.is_ok(),
            "re-sending an already-buffered chunk index must succeed, got {:?}",
            resend
        );
    }

    /// Regression: an entry parked at the per-entry byte cap could
    /// stay in `pending` forever because the `seq_through > latest`
    /// eviction never fires while a hostile peer re-uses the same
    /// `seq_through`. The age sweep is the second line of defense:
    /// any entry whose last-progress is older than `max_age` is
    /// dropped on the next `sweep_stale` call.
    #[test]
    fn reassembler_sweep_stale_drops_quiet_entries() {
        let mut reassembler = SnapshotReassembler::new();
        reassembler.feed(0xAAAA, vec![1; 10], 1, 0, 3).unwrap();
        assert_eq!(reassembler.pending_count(), 1);

        // Wait until the entry's last_progress_at is older than the
        // sweep age, then sweep.
        std::thread::sleep(Duration::from_millis(20));
        let evicted = reassembler.sweep_stale(Duration::from_millis(10));
        assert_eq!(evicted, 1, "stale entry must be evicted");
        assert_eq!(reassembler.pending_count(), 0);
    }

    /// Pin the slow-but-progressing legitimate peer case: every
    /// chunk that lands resets `last_progress_at`, so the sweep
    /// only kills entries that have actually gone quiet — not ones
    /// that are simply slow to receive every chunk.
    #[test]
    fn reassembler_sweep_keeps_progressing_entries() {
        let mut reassembler = SnapshotReassembler::new();
        reassembler.feed(0xAAAA, vec![1; 10], 1, 0, 3).unwrap();
        std::thread::sleep(Duration::from_millis(20));

        // A second chunk lands — last_progress_at refreshes.
        reassembler.feed(0xAAAA, vec![1; 10], 1, 1, 3).unwrap();

        // Sweep with an age that would have killed the entry from
        // its original creation, but is generous relative to the
        // fresh chunk's timestamp.
        let evicted = reassembler.sweep_stale(Duration::from_millis(15));
        assert_eq!(
            evicted, 0,
            "entry that received a chunk within max_age must survive"
        );
        assert_eq!(reassembler.pending_count(), 1);
    }

    /// Pin the cross-daemon healing path: even if a particular
    /// daemon's hostile entry never sees another chunk, the
    /// opportunistic sweep at the head of every `feed` call drops
    /// it on the next traffic from ANY daemon.
    #[test]
    fn reassembler_opportunistic_sweep_in_feed_drops_quiet_entries() {
        // Use a tiny opportunistic-sweep age so the in-`feed`
        // sweep fires within test timescales.
        let mut reassembler = SnapshotReassembler::with_max_pending_age(Duration::from_millis(10));

        // Hostile daemon parks an entry under (origin=0xBAD, seq=1).
        reassembler.feed(0xBAD, vec![0xFF; 10], 1, 0, 3).unwrap();
        assert_eq!(reassembler.pending_count(), 1);

        // Time passes; hostile peer never sends anything else.
        std::thread::sleep(Duration::from_millis(25));

        // A completely unrelated daemon's chunk arrives. The
        // opportunistic sweep at the head of `feed` must drop the
        // hostile entry as a side effect — not just the
        // explicit-`sweep_stale` driver.
        reassembler.feed(0xC0DE, vec![1; 10], 5, 0, 3).unwrap();

        // Only the new daemon's entry remains; the hostile
        // 0xBAD entry was swept.
        assert_eq!(reassembler.pending_count(), 1);
    }

    /// Sweeping a stale buffer must not amnesia the daemon's
    /// `latest_seq`: a peer that comes back later trying to
    /// re-open the same `seq_through` is still rejected as stale,
    /// so the sweep can't be turned into a snapshot-replacement
    /// amplifier.
    #[test]
    fn reassembler_sweep_stale_preserves_latest_seq() {
        let mut reassembler = SnapshotReassembler::new();
        reassembler.feed(0xAAAA, vec![1; 10], 100, 0, 3).unwrap();

        std::thread::sleep(Duration::from_millis(20));
        let evicted = reassembler.sweep_stale(Duration::from_millis(10));
        assert_eq!(evicted, 1);

        // Old seq_through must still be rejected as stale even
        // though the in-flight buffer was dropped.
        let stale = reassembler.feed(0xAAAA, vec![1; 10], 50, 0, 3);
        assert!(
            matches!(
                stale,
                Err(ReassemblyError::StaleSeqThrough {
                    got: 50,
                    latest: 100,
                })
            ),
            "post-sweep replay of an older seq_through must still be rejected, got {:?}",
            stale,
        );
    }

    /// Pin the at-cap + quiet attack: a peer fills an entry to
    /// just under `MAX_PENDING_REASSEMBLY_BYTES` and goes silent.
    /// Pre-fix the per-entry byte cap blocked further amplification
    /// but the parked bytes stayed forever; the sweep closes that
    /// hole.
    #[test]
    fn reassembler_sweep_releases_buffer_parked_at_byte_cap() {
        let mut reassembler = SnapshotReassembler::new();
        let chunk_full = vec![0xCCu8; MAX_SNAPSHOT_CHUNK_SIZE];
        let chunks_to_fill = MAX_PENDING_REASSEMBLY_BYTES / MAX_SNAPSHOT_CHUNK_SIZE;
        let total_chunks = (chunks_to_fill as u32) + 2;
        for i in 0..(chunks_to_fill as u32) {
            reassembler
                .feed(0xAAAA, chunk_full.clone(), 1, i, total_chunks)
                .unwrap();
        }
        assert_eq!(reassembler.pending_count(), 1);

        // Peer goes silent. Pre-fix the entry would stay parked
        // at ~MAX_PENDING_REASSEMBLY_BYTES indefinitely.
        std::thread::sleep(Duration::from_millis(20));
        let evicted = reassembler.sweep_stale(Duration::from_millis(10));
        assert_eq!(evicted, 1, "parked-at-cap entry must be released by sweep");
        assert_eq!(reassembler.pending_count(), 0);
    }

    #[test]
    fn test_regression_reassembler_distinct_daemons_coexist() {
        // Eviction is per-daemon, not global — parallel migrations of
        // different daemons must be able to share the reassembler.
        let mut reassembler = SnapshotReassembler::new();
        reassembler.feed(0x1111, vec![1; 10], 1, 0, 3).unwrap();
        reassembler.feed(0x2222, vec![2; 10], 7, 0, 3).unwrap();
        reassembler.feed(0x3333, vec![3; 10], 9, 0, 3).unwrap();
        assert_eq!(reassembler.pending_count(), 3);
    }

    #[test]
    fn test_regression_wire_decode_rejects_zero_total_chunks() {
        // Regression: the wire decoder accepted any u32 for total_chunks
        // and chunk_index, including nonsense like total_chunks=0. A
        // defensive validation at the wire boundary stops malformed
        // messages from ever reaching the reassembler.
        use bytes::BufMut;
        let mut buf = Vec::new();
        buf.put_u8(wire::MSG_SNAPSHOT_READY);
        buf.put_u64_le(0xAAAA); // daemon_origin
        buf.put_u64_le(1); // seq_through
        buf.put_u32_le(0); // chunk_index
        buf.put_u32_le(0); // total_chunks — invalid
        buf.put_u32_le(0); // len
        let err = wire::decode(&buf).expect_err("total_chunks=0 must be rejected");
        let err_msg = format!("{}", err);
        assert!(
            err_msg.contains("total_chunks"),
            "error must mention total_chunks, got {:?}",
            err_msg
        );
    }

    #[test]
    fn test_regression_wire_decode_rejects_chunk_index_out_of_range() {
        use bytes::BufMut;
        let mut buf = Vec::new();
        buf.put_u8(wire::MSG_SNAPSHOT_READY);
        buf.put_u64_le(0xAAAA);
        buf.put_u64_le(1);
        buf.put_u32_le(5); // chunk_index
        buf.put_u32_le(3); // total_chunks — index out of range
        buf.put_u32_le(0);
        let err = wire::decode(&buf).expect_err("chunk_index >= total_chunks must be rejected");
        let err_msg = format!("{}", err);
        assert!(
            err_msg.contains("chunk_index"),
            "error must mention chunk_index, got {:?}",
            err_msg
        );
    }

    #[test]
    fn test_regression_wire_decode_rejects_total_chunks_overflow() {
        use bytes::BufMut;
        let mut buf = Vec::new();
        buf.put_u8(wire::MSG_SNAPSHOT_READY);
        buf.put_u64_le(0xAAAA);
        buf.put_u64_le(1);
        buf.put_u32_le(0);
        buf.put_u32_le(u32::MAX); // total_chunks — exceeds MAX_TOTAL_CHUNKS
        buf.put_u32_le(0);
        let err = wire::decode(&buf).expect_err("total_chunks > MAX_TOTAL_CHUNKS must be rejected");
        let err_msg = format!("{}", err);
        assert!(
            err_msg.contains("MAX_TOTAL_CHUNKS"),
            "error must mention MAX_TOTAL_CHUNKS, got {:?}",
            err_msg
        );
    }

    #[test]
    fn test_regression_reassembler_end_to_end_forged_chunk_cannot_complete() {
        // Integration: simulate an attacker who learns total_chunks=4 from a
        // legitimate first chunk and then tries to race ahead with forged
        // content at indices beyond the range. Even if indices {0,5,7} each
        // carry attacker-chosen bytes, the reassembler must never "complete"
        // a snapshot without receiving every real index in 0..total_chunks.
        let mut reassembler = SnapshotReassembler::new();

        // Real chunk 0 — opens the reassembly at total_chunks=4.
        let r0 = reassembler.feed(0xDEAD, vec![0xA0; 10], 1, 0, 4).unwrap();
        assert!(r0.is_none());

        // Forged out-of-range chunks — all rejected, none completes.
        for bad_idx in [4, 5, 7, 999] {
            let r = reassembler.feed(0xDEAD, vec![0xFF; 10], 1, bad_idx, 4);
            assert!(
                matches!(r, Err(ReassemblyError::ChunkIndexOutOfRange { .. })),
                "index {} must be rejected, got {:?}",
                bad_idx,
                r
            );
        }

        // A snapshot-like "complete" signal can only come from filling
        // real indices 1, 2, 3.
        assert!(reassembler
            .feed(0xDEAD, vec![0xA1; 10], 1, 1, 4)
            .unwrap()
            .is_none());
        assert!(reassembler
            .feed(0xDEAD, vec![0xA2; 10], 1, 2, 4)
            .unwrap()
            .is_none());
        let full = reassembler
            .feed(0xDEAD, vec![0xA3; 10], 1, 3, 4)
            .unwrap()
            .expect("all four real chunks received — reassembly must complete");
        // Concatenation order is by chunk_index ascending, so the payload
        // is exactly the legitimate chunks 0,1,2,3 — not a forgery.
        assert_eq!(full.len(), 40);
        assert!(full[..10].iter().all(|&b| b == 0xA0));
        assert!(full[10..20].iter().all(|&b| b == 0xA1));
        assert!(full[20..30].iter().all(|&b| b == 0xA2));
        assert!(full[30..].iter().all(|&b| b == 0xA3));
    }

    #[test]
    fn test_wire_roundtrip_chunked_snapshot() {
        let msg = MigrationMessage::SnapshotReady {
            daemon_origin: 0xCCCC,
            snapshot_bytes: vec![42; 100],
            seq_through: 99,
            chunk_index: 2,
            total_chunks: 5,
        };
        let encoded = wire::encode(&msg).unwrap();
        let decoded = wire::decode(&encoded).unwrap();
        match decoded {
            MigrationMessage::SnapshotReady {
                chunk_index,
                total_chunks,
                ..
            } => {
                assert_eq!(chunk_index, 2);
                assert_eq!(total_chunks, 5);
            }
            _ => panic!("expected SnapshotReady"),
        }
    }

    #[test]
    fn test_wire_roundtrip_failed() {
        // Round-trip every variant of MigrationFailureReason to
        // pin the wire-layout contract. A future bump that drops
        // or adds a variant without updating the match will trip
        // the exhaustive match below.
        for reason in [
            MigrationFailureReason::NotReady,
            MigrationFailureReason::FactoryNotFound,
            MigrationFailureReason::ComputeNotSupported,
            MigrationFailureReason::StateFailed("something broke".into()),
            MigrationFailureReason::AlreadyMigrating,
            MigrationFailureReason::IdentityTransportFailed("seal failed".into()),
            MigrationFailureReason::NotReadyTimeout { attempts: 5 },
        ] {
            let msg = MigrationMessage::MigrationFailed {
                daemon_origin: 0xCCCC,
                reason: reason.clone(),
            };
            let encoded = wire::encode(&msg).unwrap();
            let decoded = wire::decode(&encoded).unwrap();
            match decoded {
                MigrationMessage::MigrationFailed {
                    daemon_origin,
                    reason: r,
                } => {
                    assert_eq!(daemon_origin, 0xCCCC);
                    assert_eq!(r, reason);
                }
                _ => panic!("expected MigrationFailed"),
            }
        }
    }

    #[test]
    fn test_wire_roundtrip_buffered_events() {
        let events = vec![
            CausalEvent {
                link: CausalLink::genesis(0xAAAA, 0),
                payload: Bytes::from_static(b"event1"),
                received_at: 100,
            },
            CausalEvent {
                link: CausalLink {
                    origin_hash: 0xAAAA,
                    horizon_encoded: 1,
                    sequence: 1,
                    parent_hash: 12345,
                },
                payload: Bytes::from_static(b"event2"),
                received_at: 200,
            },
        ];
        let msg = MigrationMessage::BufferedEvents {
            daemon_origin: 0xAAAA,
            events,
        };
        let encoded = wire::encode(&msg).unwrap();
        let decoded = wire::decode(&encoded).unwrap();
        match decoded {
            MigrationMessage::BufferedEvents {
                daemon_origin,
                events,
            } => {
                assert_eq!(daemon_origin, 0xAAAA);
                assert_eq!(events.len(), 2);
                assert_eq!(events[0].payload, Bytes::from_static(b"event1"));
                assert_eq!(events[0].received_at, 100);
                assert_eq!(events[1].link.sequence, 1);
                assert_eq!(events[1].link.parent_hash, 12345);
                assert_eq!(events[1].payload, Bytes::from_static(b"event2"));
                assert_eq!(events[1].received_at, 200);
            }
            _ => panic!("expected BufferedEvents"),
        }
    }

    #[test]
    fn test_wire_encode_rejects_oversized_failure_reason() {
        // Regression: `reason.len() as u16` previously truncated silently when
        // the reason exceeded u16::MAX, producing a stream the decoder
        // misparses. Encoding must now return an error.
        let oversized = "x".repeat(u16::MAX as usize + 1);
        let msg = MigrationMessage::MigrationFailed {
            daemon_origin: 0xDEAD,
            reason: MigrationFailureReason::StateFailed(oversized),
        };
        let result = wire::encode(&msg);
        assert!(
            matches!(result, Err(MigrationError::StateFailed(_))),
            "encode of oversized reason must error, got {:?}",
            result
        );
    }

    #[test]
    fn test_wire_rejects_unknown_failure_code() {
        // Manually-crafted `MSG_FAILED` with code 0xFFFF (unknown
        // variant). Decoder must refuse rather than mis-parse.
        let mut buf = Vec::new();
        buf.put_u8(wire::MSG_FAILED);
        buf.put_u64_le(0xBEEF);
        buf.put_u16_le(0xFFFF); // unknown code
        let err = wire::decode(&buf).expect_err("unknown code must reject");
        match err {
            MigrationError::StateFailed(msg) => {
                assert!(msg.contains("unknown MigrationFailureReason code"));
            }
            other => panic!("expected StateFailed, got {other:?}"),
        }
    }

    #[test]
    fn test_list_migrations() {
        let (reg, origin) = setup_registry();
        let orch = MigrationOrchestrator::new(reg, 0x1111);

        assert!(orch.list_migrations().is_empty());

        orch.start_migration(origin, 0x1111, 0x2222).unwrap();

        let list = orch.list_migrations();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].daemon_origin, origin);
        assert_eq!(list[0].source_node, 0x1111);
        assert_eq!(list[0].target_node, 0x2222);
        assert_eq!(list[0].retries, 0);
        assert_eq!(list[0].buffered_events, 0);
    }

    /// The conversion used at the `list_migrations` call site
    /// (a raw `as u32` would wrap silently). A stuck-in-Replay
    /// migration buffering past `u32::MAX` must report
    /// `u32::MAX`, not a wrapped small number, so operators
    /// reading the Deck don't mistake overflow for drain.
    #[test]
    fn buffered_events_saturates_at_u32_max() {
        let cast = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
        assert_eq!(cast(0), 0);
        assert_eq!(cast(1), 1);
        assert_eq!(cast(u32::MAX as usize), u32::MAX);
        // On 64-bit hosts these are strictly above u32::MAX;
        // on a hypothetical 32-bit host the saturating
        // conversion is a no-op and the assertion still
        // holds.
        assert_eq!(cast(usize::MAX), u32::MAX);
        if let Some(overflow) = (u32::MAX as usize).checked_add(1) {
            assert_eq!(cast(overflow), u32::MAX);
        }
    }
}
