//! `MemoriesFold` — decodes `EventMeta` + payload, routes on dispatch,
//! mutates [`super::state::MemoriesState`].

use super::super::super::redex::{RedexError, RedexEvent, RedexFold};
use super::super::meta::{
    compute_checksum, compute_checksum_with_meta, EventMeta, EVENT_META_SIZE,
};
use super::dispatch::{
    DISPATCH_MEMORY_DELETED, DISPATCH_MEMORY_PINNED, DISPATCH_MEMORY_RETAGGED,
    DISPATCH_MEMORY_STORED, DISPATCH_MEMORY_UNPINNED,
};
use super::state::MemoriesState;
use super::types::{
    Memory, MemoryDeletedPayload, MemoryPinTogglePayload, MemoryRetaggedPayload,
    MemoryStoredPayload,
};

/// Fold implementation for the memories model.
pub struct MemoriesFold;

impl RedexFold<MemoriesState> for MemoriesFold {
    fn apply(&mut self, ev: &RedexEvent, state: &mut MemoriesState) -> Result<(), RedexError> {
        // Per-event decode failures use `RedexError::Decode` (a
        // recoverable variant) so the `Stop` fold policy
        // skip-and-continues instead of permanently halting on a
        // single bad event. See `tasks/fold.rs` for the full
        // rationale.
        if ev.payload.len() < EVENT_META_SIZE {
            return Err(RedexError::Decode(format!(
                "memories payload too short: {} bytes (need >= {})",
                ev.payload.len(),
                EVENT_META_SIZE
            )));
        }
        let meta = EventMeta::from_bytes(&ev.payload[..EVENT_META_SIZE])
            .ok_or_else(|| RedexError::Decode("bad EventMeta prefix".into()))?;
        let tail = &ev.payload[EVENT_META_SIZE..];

        // Verify the corruption-detection checksum stamped at
        // ingest against the bytes we received from RedEX.
        //
        // Audit #8: try the v2 (header + tail) checksum first, fall
        // back to v1 (tail-only) for records written by pre-fix
        // adapters. New writes pass v2 and detect bit-flips in the
        // header (e.g. a `STORED → DELETED` dispatch flip); legacy
        // records pass v1 with the original undercoverage gap
        // documented as a known limitation. See `compute_checksum`'s
        // doc for the full scope and migration story.
        let v2_expected = compute_checksum_with_meta(&meta, tail);
        let valid = if meta.checksum == v2_expected {
            true
        } else {
            // Fallback for legacy records.
            meta.checksum == compute_checksum(tail)
        };
        if !valid {
            return Err(RedexError::Decode(format!(
                "memories fold: EventMeta checksum mismatch at seq {} (got {:#010x}, v2 expected {:#010x})",
                ev.entry.seq, meta.checksum, v2_expected
            )));
        }

        match meta.dispatch {
            DISPATCH_MEMORY_STORED => {
                let p: MemoryStoredPayload =
                    postcard::from_bytes(tail).map_err(|e| RedexError::Decode(e.to_string()))?;
                // Treat STORED as a content-update for an
                // existing id: preserve `pinned` and `created_ns`,
                // advance `updated_ns`, and overwrite the rest. A
                // blanket `insert` would silently replace any
                // existing entry, so `memories.store(42, "updated",
                // ...)` after `memories.pin(42)` would drop the pin
                // flag and overwrite the original creation
                // timestamp with no observable signal to the
                // operator.
                if let Some(existing) = state.memories.get_mut(&p.id) {
                    existing.content = p.content;
                    existing.tags = p.tags;
                    existing.source = p.source;
                    existing.updated_ns = p.now_ns;
                    // pinned + created_ns intentionally preserved.
                } else {
                    state.memories.insert(
                        p.id,
                        Memory {
                            id: p.id,
                            content: p.content,
                            tags: p.tags,
                            source: p.source,
                            created_ns: p.now_ns,
                            updated_ns: p.now_ns,
                            pinned: false,
                        },
                    );
                }
            }
            DISPATCH_MEMORY_RETAGGED => {
                let p: MemoryRetaggedPayload =
                    postcard::from_bytes(tail).map_err(|e| RedexError::Decode(e.to_string()))?;
                if let Some(m) = state.memories.get_mut(&p.id) {
                    m.tags = p.tags;
                    m.updated_ns = p.now_ns;
                }
            }
            DISPATCH_MEMORY_PINNED => {
                let p: MemoryPinTogglePayload =
                    postcard::from_bytes(tail).map_err(|e| RedexError::Decode(e.to_string()))?;
                if let Some(m) = state.memories.get_mut(&p.id) {
                    m.pinned = true;
                    m.updated_ns = p.now_ns;
                }
            }
            DISPATCH_MEMORY_UNPINNED => {
                let p: MemoryPinTogglePayload =
                    postcard::from_bytes(tail).map_err(|e| RedexError::Decode(e.to_string()))?;
                if let Some(m) = state.memories.get_mut(&p.id) {
                    m.pinned = false;
                    m.updated_ns = p.now_ns;
                }
            }
            DISPATCH_MEMORY_DELETED => {
                let p: MemoryDeletedPayload =
                    postcard::from_bytes(tail).map_err(|e| RedexError::Decode(e.to_string()))?;
                state.memories.remove(&p.id);
            }
            other => {
                tracing::debug!(
                    dispatch = other,
                    seq = ev.entry.seq,
                    "memories fold: ignoring unknown dispatch"
                );
            }
        }
        Ok(())
    }
}
