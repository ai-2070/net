//! `StoredEvent` — the queue element `NetSession` carries.
//!
//! Moved here in Stage 2 (S0a §3.1): `NetSession` holds
//! `inbound: SegQueue<StoredEvent>` and its `push_event` / `pop_event`
//! pair, and never reads a field. The alternative — making the queue
//! element a type parameter — ripples `NetSession<E>` through
//! hundreds of core sites for a payload the session never inspects.
//!
//! `crate::event::StoredEvent` in the core re-exports this type, so no
//! consumer path changed.
//!
//! The JSON conveniences (`from_value`, `parse`) and the `Serialize`
//! impl ride the `json` feature. The core enables it; the wasm32
//! build does not, so a browser leaf pays no `serde_json`.

use bytes::Bytes;
#[cfg(feature = "json")]
use serde::Serialize;
#[cfg(feature = "json")]
use serde_json::Value as JsonValue;

/// An event retrieved from storage with its backend-specific ID.
#[derive(Debug, Clone)]
pub struct StoredEvent {
    /// Backend-specific identifier.
    pub id: String,

    /// Raw JSON payload bytes (deferred parsing for performance).
    pub raw: Bytes,

    /// Insertion timestamp from ingestion.
    pub insertion_ts: u64,

    /// Shard this event belongs to.
    pub shard_id: u16,

    /// Application-level idempotency key as written by the producer.
    /// Adapters carry an opaque dedup token on the wire (Redis Streams
    /// uses a `dedup_id` field; JetStream uses `Nats-Msg-Id`). The
    /// trait-level consumer (`net::adapter::Adapter::poll_shard` in the core)
    /// surfaces it here so callers can drive their own dedup table
    /// without re-reading the raw broker payload. `None` when the
    /// adapter or the wire entry doesn't carry one.
    pub dedup_id: Option<String>,
}

impl StoredEvent {
    /// Create a new stored event from raw bytes.
    #[inline]
    pub fn new(id: String, raw: Bytes, insertion_ts: u64, shard_id: u16) -> Self {
        Self {
            id,
            raw,
            insertion_ts,
            shard_id,
            dedup_id: None,
        }
    }

    /// Create a new stored event from a JSON value (serializes once).
    ///
    /// Requires the `json` feature (on by default for the core).
    ///
    /// See `RawEvent::from_value` for the rationale on
    /// `unwrap_or_default()` instead of `expect()`.
    #[cfg(feature = "json")]
    #[inline]
    pub fn from_value(id: String, value: JsonValue, insertion_ts: u64, shard_id: u16) -> Self {
        let raw = Bytes::from(serde_json::to_vec(&value).unwrap_or_default());
        Self {
            id,
            raw,
            insertion_ts,
            shard_id,
            dedup_id: None,
        }
    }

    /// Attach an application-level dedup identifier (the producer's
    /// `dedup_id` / `Nats-Msg-Id`). Returns `self` for chaining.
    #[inline]
    #[must_use]
    pub fn with_dedup_id(mut self, dedup_id: Option<String>) -> Self {
        self.dedup_id = dedup_id;
        self
    }

    /// Parse the raw bytes into a JSON value on demand.
    #[cfg(feature = "json")]
    #[inline]
    pub fn parse(&self) -> Result<JsonValue, serde_json::Error> {
        serde_json::from_slice(&self.raw)
    }

    /// Get the raw bytes as a string slice (for serialization).
    ///
    /// Returns `Err` if the raw bytes are not valid UTF-8, rather than
    /// silently substituting data.
    #[inline]
    pub fn raw_str(&self) -> Result<&str, std::str::Utf8Error> {
        std::str::from_utf8(&self.raw)
    }
}

#[cfg(feature = "json")]
impl Serialize for StoredEvent {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        // Always emit `dedup_id` (as `null` when absent) so the
        // on-wire shape is stable. Pre-fix the field count flipped
        // between 4 and 5 based on whether `dedup_id` was populated;
        // downstream Node / Python / Go consumers using
        // `deny_unknown_fields` (or strict schema validators)
        // accepted the 4-field shape but rejected events whose
        // adapter populated the 5th, making the wire compatibility
        // *data-dependent*. Stable shape > byte-optimal: an extra
        // `"dedup_id":null` per legacy event is cheap, the rejection
        // hazard is not.
        let field_count = 5;
        let mut state = serializer.serialize_struct("StoredEvent", field_count)?;
        state.serialize_field("id", &self.id)?;
        // Serialize raw bytes as a `RawValue` so the on-wire JSON
        // is byte-for-byte the same as the input. Pre-fix the
        // bytes were parsed into a `JsonValue` tree and re-
        // serialized; the round-trip discarded original
        // whitespace, normalized number formatting (`1.0` → `1`),
        // and (without `preserve_order`) re-ordered map keys
        // alphabetically. Any downstream that hashed or signed
        // the serialized form and expected byte-equality with the
        // input silently failed verification — a sneaky failure
        // mode in audit / signing pipelines that look at the
        // re-emitted JSON.
        //
        // `from_str::<&RawValue>` validates the JSON (so the
        // pre-existing "invalid raw JSON returns a serde error,
        // not a silent null" guarantee is preserved) AND borrows
        // the input bytes — no allocation.
        //
        // PERF_AUDIT §1.8 — pre-fix this used
        // `RawValue::from_string(raw_str.to_string())`, which
        // allocated a fresh `String` copy of the entire payload
        // per serialized event. The borrowed form is byte-for-byte
        // identical on the wire but skips the copy.
        let raw_str = std::str::from_utf8(&self.raw)
            .map_err(|e| serde::ser::Error::custom(format!("invalid raw UTF-8: {}", e)))?;
        let raw_value: &serde_json::value::RawValue = serde_json::from_str(raw_str)
            .map_err(|e| serde::ser::Error::custom(format!("invalid raw JSON: {}", e)))?;
        state.serialize_field("raw", raw_value)?;
        state.serialize_field("insertion_ts", &self.insertion_ts)?;
        state.serialize_field("shard_id", &self.shard_id)?;
        // Always emit `dedup_id` to keep the wire shape stable —
        // `None` serializes as JSON `null`.
        state.serialize_field("dedup_id", &self.dedup_id)?;
        state.end()
    }
}
