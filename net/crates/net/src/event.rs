//! Event types for the Net event bus.
//!
//! Events are opaque JSON values - the event bus performs no schema validation
//! or interpretation of event content.
//!
//! # Performance Optimization
//!
//! Since Net is schema-agnostic, we offer multiple event representations:
//!
//! - `Event`: Standard wrapper around `serde_json::Value` (convenient but slower)
//! - `RawEvent`: Pre-serialized bytes with cached hash (fastest for high-throughput)
//!
//! For maximum performance, use `RawEvent::from_bytes()` when you already have
//! JSON bytes (e.g., from a network buffer or file).

use bytes::Bytes;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

/// An opaque event - any valid JSON value.
///
/// The event bus does not validate, interpret, or enforce any schema.
/// Events are treated as opaque binary blobs internally.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[repr(transparent)]
pub struct Event(pub JsonValue);

impl Event {
    /// Create a new event from a JSON value.
    #[inline]
    pub fn new(value: JsonValue) -> Self {
        Self(value)
    }

    /// Create an event from a JSON string.
    #[inline]
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(s).map(Self)
    }

    /// Create an event from raw bytes.
    #[inline]
    pub fn from_slice(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(bytes).map(Self)
    }

    /// Get the inner JSON value.
    #[inline]
    pub fn into_inner(self) -> JsonValue {
        self.0
    }

    /// Get a reference to the inner JSON value.
    #[inline]
    pub fn as_value(&self) -> &JsonValue {
        &self.0
    }

    /// Convert to a raw event (serializes once, caches the result).
    #[inline]
    pub fn into_raw(self) -> RawEvent {
        RawEvent::from_value(self.0)
    }
}

impl From<JsonValue> for Event {
    #[inline]
    fn from(value: JsonValue) -> Self {
        Self(value)
    }
}

impl From<Event> for JsonValue {
    #[inline]
    fn from(event: Event) -> Self {
        event.0
    }
}

/// A pre-serialized event with cached hash.
///
/// This is the high-performance event type for schema-agnostic ingestion.
/// By storing the raw bytes and pre-computed hash, we avoid:
/// - Re-serialization on every operation
/// - Repeated hashing for shard selection
///
/// # Example
///
/// ```rust,ignore
/// // From network buffer or file (zero-copy if Bytes is used)
/// let raw = RawEvent::from_bytes(network_buffer);
///
/// // From existing JSON value (serializes once)
/// let raw = RawEvent::from_value(json!({"key": "value"}));
///
/// // Ingestion uses cached hash - no re-serialization
/// bus.ingest_raw(raw)?;
/// ```
#[derive(Clone)]
pub struct RawEvent {
    /// Pre-serialized JSON bytes.
    bytes: Bytes,
    /// Pre-computed hash for shard selection.
    hash: u64,
}

impl RawEvent {
    /// Create a raw event from bytes.
    ///
    /// The bytes must be valid JSON. No validation is performed for performance.
    /// Use `from_bytes_validated` if you need validation.
    #[inline]
    pub fn from_bytes(bytes: impl Into<Bytes>) -> Self {
        let bytes = bytes.into();
        let hash = xxhash_rust::xxh3::xxh3_64(&bytes);
        Self { bytes, hash }
    }

    /// Create a raw event from bytes with a pre-computed hash.
    ///
    /// Use this when you've already computed the xxhash (e.g., for reused events).
    /// The caller is responsible for ensuring the hash matches the bytes.
    #[inline]
    pub fn from_bytes_with_hash(bytes: impl Into<Bytes>, hash: u64) -> Self {
        Self {
            bytes: bytes.into(),
            hash,
        }
    }

    /// Create a raw event from bytes with JSON validation.
    #[inline]
    pub fn from_bytes_validated(bytes: impl Into<Bytes>) -> Result<Self, serde_json::Error> {
        let bytes = bytes.into();
        // Validate it's valid JSON by attempting to parse
        let _: JsonValue = serde_json::from_slice(&bytes)?;
        let hash = xxhash_rust::xxh3::xxh3_64(&bytes);
        Ok(Self { bytes, hash })
    }

    /// Create a raw event from a JSON value.
    ///
    /// This serializes the value once and caches the result.
    ///
    /// `serde_json::to_vec(&JsonValue)` is infallible by
    /// construction — the value tree is a known-good JSON
    /// structure with no fallible serializer in the path —
    /// modulo OOM, which the global allocator handles via
    /// abort. The `unwrap_or_default()` fallback keeps the
    /// non-panic contract for a hypothetical future serde-json
    /// change that introduced a fallible path on `Value`. Pre-
    /// fix `expect("Value serialization is infallible")` panicked
    /// at the call site if the assumption ever broke; the panic
    /// would unwind across `from_value`'s callers (bus ingest,
    /// FFI ingest paths) where the contract is non-panicking.
    #[inline]
    pub fn from_value(value: JsonValue) -> Self {
        let bytes = Bytes::from(serde_json::to_vec(&value).unwrap_or_default());
        let hash = xxhash_rust::xxh3::xxh3_64(&bytes);
        Self { bytes, hash }
    }

    /// Creates a RawEvent from a string. No validation is performed for
    /// performance — see `from_bytes_validated` for a validating alternative.
    #[inline]
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Self {
        Self::from_bytes(Bytes::copy_from_slice(s.as_bytes()))
    }

    /// Get the raw bytes.
    #[inline]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Get the bytes (clone is cheap - reference counted).
    #[inline]
    pub fn bytes(&self) -> Bytes {
        self.bytes.clone()
    }

    /// Get the pre-computed hash.
    #[inline]
    pub fn hash(&self) -> u64 {
        self.hash
    }

    /// Parse the bytes into a JSON value (for when you need to inspect).
    #[inline]
    pub fn parse(&self) -> Result<JsonValue, serde_json::Error> {
        serde_json::from_slice(&self.bytes)
    }

    /// Get the byte length.
    #[inline]
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    /// Check if empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
}

impl std::fmt::Debug for RawEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RawEvent")
            .field("len", &self.bytes.len())
            .field("hash", &self.hash)
            .finish()
    }
}

impl From<Event> for RawEvent {
    #[inline]
    fn from(event: Event) -> Self {
        event.into_raw()
    }
}

impl From<JsonValue> for RawEvent {
    #[inline]
    fn from(value: JsonValue) -> Self {
        RawEvent::from_value(value)
    }
}

/// Internal event representation with metadata assigned at ingestion.
///
/// This is the canonical form of an event within the event bus.
/// The `insertion_ts` provides deterministic ordering within a shard.
///
/// Uses `Bytes` for zero-copy, reference-counted storage.
#[derive(Debug, Clone)]
pub struct InternalEvent {
    /// Pre-serialized JSON payload (reference-counted, zero-copy clone).
    pub raw: Bytes,

    /// Monotonically increasing insertion timestamp (nanoseconds).
    /// Strictly ordered within a shard, not globally.
    pub insertion_ts: u64,

    /// Shard this event was assigned to.
    pub shard_id: u16,
}

impl InternalEvent {
    /// Create a new internal event from raw bytes.
    #[inline]
    pub fn new(raw: Bytes, insertion_ts: u64, shard_id: u16) -> Self {
        Self {
            raw,
            insertion_ts,
            shard_id,
        }
    }

    /// Create from a JSON value (serializes once).
    ///
    /// See `RawEvent::from_value` for the rationale on
    /// `unwrap_or_default()` instead of `expect()`.
    #[inline]
    pub fn from_value(value: JsonValue, insertion_ts: u64, shard_id: u16) -> Self {
        let raw = Bytes::from(serde_json::to_vec(&value).unwrap_or_default());
        Self {
            raw,
            insertion_ts,
            shard_id,
        }
    }

    /// Parse the raw bytes into a JSON value.
    #[inline]
    pub fn parse(&self) -> Result<JsonValue, serde_json::Error> {
        serde_json::from_slice(&self.raw)
    }

    /// Get the raw bytes as a slice.
    #[inline]
    pub fn as_bytes(&self) -> &[u8] {
        &self.raw
    }
}

/// A batch of events for adapter dispatch.
///
/// Batches are formed by shard workers and contain strictly ordered
/// events from a single shard.
#[derive(Debug, Clone)]
pub struct Batch {
    /// The shard this batch belongs to.
    pub shard_id: u16,

    /// Events in insertion order.
    pub events: Vec<InternalEvent>,

    /// Sequence number of the first event in this batch.
    /// Used for idempotent retry handling.
    pub sequence_start: u64,

    /// Per-process nonce sampled once at process start. Adapters
    /// that persist `(shard_id, sequence_start)` for dedup
    /// (JetStream `Nats-Msg-Id`, Redis stream MAXLEN keys, etc.)
    /// must include this in the dedup key — otherwise a producer
    /// that restarts within the backend's dedup window collides
    /// with its prior incarnation on `(shard, 0, 0)` and the new
    /// batches are silently discarded as duplicates.
    ///
    /// `BatchWorker::next_sequence` is process-local and resets to
    /// zero on restart; the nonce is the global discriminator that
    /// makes the composite key globally unique across restarts
    /// even though the per-process counter is not durable.
    pub process_nonce: u64,
}

/// Per-process nonce used by [`Batch::process_nonce`]. Sampled once
/// (lazy) from a mix of entropy sources so two processes launched on
/// the same machine within a single nanosecond tick are still
/// distinguishable.
///
/// We don't use `getrandom` here because it's a feature-gated
/// optional dep — `event.rs` is in the always-compiled core. Instead
/// we run xxh3 over multiple sources whose joint state is effectively
/// never identical across two adjacent process starts: wall-clock
/// nanos, monotonic-clock nanos (resilient to wall-clock skew),
/// pid, the address of a stack-local (gives an ASLR component),
/// and the current thread id. Plain XOR of two sources (the
/// previous implementation) collapses to zero whenever the
/// components happen to share bit patterns; xxh3 mixes them so any
/// single non-degenerate source dominates the output.
pub fn batch_process_nonce() -> u64 {
    use std::sync::OnceLock;
    static NONCE: OnceLock<u64> = OnceLock::new();
    *NONCE.get_or_init(|| {
        use std::hash::{Hash, Hasher};
        use std::time::Instant;

        let wall_nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        // `Instant::now()` is monotonic — distinct entropy from
        // `SystemTime` (the OS may slew wall-clock backwards but
        // never the monotonic source). `Instant` doesn't expose a
        // public u64 accessor, so we mix the bytes of its `Debug`
        // repr.
        let mono_marker = format!("{:?}", Instant::now());
        let pid = std::process::id() as u64;
        // Address of a stack local — adds an ASLR-derived
        // component that differs across process starts even when
        // the time / pid components happen to collide.
        let stack_marker: usize = &pid as *const u64 as usize;
        let mut tid_hasher = std::collections::hash_map::DefaultHasher::new();
        std::thread::current().id().hash(&mut tid_hasher);
        let tid = tid_hasher.finish();

        let mut buf = [0u8; 64];
        buf[..8].copy_from_slice(&wall_nanos.to_le_bytes());
        buf[8..16].copy_from_slice(&pid.to_le_bytes());
        buf[16..24].copy_from_slice(&(stack_marker as u64).to_le_bytes());
        buf[24..32].copy_from_slice(&tid.to_le_bytes());
        // Pack as much of the monotonic marker bytes as fits.
        let mono_bytes = mono_marker.as_bytes();
        let n = mono_bytes.len().min(32);
        buf[32..32 + n].copy_from_slice(&mono_bytes[..n]);

        let nonce = xxhash_rust::xxh3::xxh3_64(&buf);
        // Refuse `0` — some consumers treat 0 as a sentinel.
        // Probability of xxh3 returning exactly 0 is 2^-64; we
        // map it to 1.
        if nonce == 0 {
            1
        } else {
            nonce
        }
    })
}

impl Batch {
    /// Create a new batch using the per-process nonce
    /// ([`batch_process_nonce`]). Convenience for tests and for
    /// callers that don't thread a custom producer nonce through.
    /// Production paths constructed via the bus go through
    /// [`Self::with_nonce`] with the bus's loaded
    /// `producer_nonce_path` value so retries dedup across
    /// process restart.
    #[inline]
    pub fn new(shard_id: u16, events: Vec<InternalEvent>, sequence_start: u64) -> Self {
        Self::with_nonce(shard_id, events, sequence_start, batch_process_nonce())
    }

    /// Create a new batch with an explicit producer nonce. Used by
    /// the bus's `BatchWorker` and `remove_shard_internal`'s
    /// stranded-flush so adapters keying dedup on
    /// `(producer_nonce, shard, sequence_start, i)` see the same
    /// nonce across process restart when the bus is configured
    /// with a `producer_nonce_path`.
    ///
    /// A `producer_nonce == 0` is coerced to `1` to preserve the
    /// non-zero invariant that `batch_process_nonce` and
    /// `dedup_state::PersistentProducerNonce::create_new` already
    /// uphold (each generates non-zero u64s and re-rolls on the
    /// astronomical 1-in-2^64 zero draw).
    ///
    /// The zero coercion is **defense-in-depth against future
    /// codecs**: a downstream caller that constructs a
    /// `Batch::with_nonce(..., 0)` directly (e.g. tests, hand-built
    /// fixtures) would otherwise emit `dedup_id` keys starting
    /// `0:` — collision-prone with any future codec that reserves
    /// `0` as "no nonce, use the legacy path." Today's
    /// `adapter/jetstream.rs::on_batch` just formats
    /// `process_nonce` as `{:x}` with no special-casing, so the
    /// hazard is latent rather than active. Coercing to 1 keeps
    /// the invariant that every shipped batch has a non-zero
    /// producer nonce regardless of caller hygiene.
    #[inline]
    pub fn with_nonce(
        shard_id: u16,
        events: Vec<InternalEvent>,
        sequence_start: u64,
        producer_nonce: u64,
    ) -> Self {
        Self {
            shard_id,
            events,
            sequence_start,
            process_nonce: if producer_nonce == 0 {
                1
            } else {
                producer_nonce
            },
        }
    }

    /// Returns the number of events in this batch.
    #[inline]
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// Returns true if this batch is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}

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
        }
    }

    /// Create a new stored event from a JSON value (serializes once).
    ///
    /// See `RawEvent::from_value` for the rationale on
    /// `unwrap_or_default()` instead of `expect()`.
    #[inline]
    pub fn from_value(id: String, value: JsonValue, insertion_ts: u64, shard_id: u16) -> Self {
        let raw = Bytes::from(serde_json::to_vec(&value).unwrap_or_default());
        Self {
            id,
            raw,
            insertion_ts,
            shard_id,
        }
    }

    /// Parse the raw bytes into a JSON value on demand.
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

impl Serialize for StoredEvent {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("StoredEvent", 4)?;
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
        // `RawValue::from_string` validates the JSON (so the
        // pre-existing "invalid raw JSON returns a serde error,
        // not a silent null" guarantee is preserved), but emits
        // the original bytes verbatim instead of round-tripping
        // through a value tree.
        let raw_str = std::str::from_utf8(&self.raw)
            .map_err(|e| serde::ser::Error::custom(format!("invalid raw UTF-8: {}", e)))?;
        let raw_value = serde_json::value::RawValue::from_string(raw_str.to_string())
            .map_err(|e| serde::ser::Error::custom(format!("invalid raw JSON: {}", e)))?;
        state.serialize_field("raw", &*raw_value)?;
        state.serialize_field("insertion_ts", &self.insertion_ts)?;
        state.serialize_field("shard_id", &self.shard_id)?;
        state.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_event_new() {
        let value = json!({"key": "value"});
        let event = Event::new(value.clone());
        assert_eq!(event.as_value(), &value);
    }

    #[test]
    fn test_event_from_str() {
        let event = Event::from_str(r#"{"key": "value"}"#).unwrap();
        assert_eq!(event.as_value()["key"], "value");
    }

    #[test]
    fn test_event_from_str_invalid() {
        let result = Event::from_str("not valid json");
        assert!(result.is_err());
    }

    #[test]
    fn test_event_from_slice() {
        let bytes = br#"{"key": "value"}"#;
        let event = Event::from_slice(bytes).unwrap();
        assert_eq!(event.as_value()["key"], "value");
    }

    #[test]
    fn test_event_into_inner() {
        let value = json!({"key": "value"});
        let event = Event::new(value.clone());
        assert_eq!(event.into_inner(), value);
    }

    #[test]
    fn test_event_into_raw() {
        let event = Event::new(json!({"key": "value"}));
        let raw = event.into_raw();
        assert!(!raw.is_empty());
        assert!(raw.hash() != 0);
    }

    #[test]
    fn test_event_from_json_value() {
        let value = json!({"key": "value"});
        let event: Event = value.clone().into();
        assert_eq!(event.as_value(), &value);
    }

    #[test]
    fn test_event_into_json_value() {
        let value = json!({"key": "value"});
        let event = Event::new(value.clone());
        let result: JsonValue = event.into();
        assert_eq!(result, value);
    }

    #[test]
    fn test_raw_event_from_bytes() {
        let bytes = br#"{"key": "value"}"#;
        let raw = RawEvent::from_bytes(bytes.as_slice());
        assert_eq!(raw.as_bytes(), bytes);
        assert!(!raw.is_empty());
        assert_eq!(raw.len(), bytes.len());
    }

    #[test]
    fn test_raw_event_from_str() {
        let s = r#"{"key": "value"}"#;
        let raw = RawEvent::from_str(s);
        assert_eq!(raw.as_bytes(), s.as_bytes());
    }

    #[test]
    fn test_raw_event_from_value() {
        let value = json!({"key": "value"});
        let raw = RawEvent::from_value(value);
        let parsed = raw.parse().unwrap();
        assert_eq!(parsed["key"], "value");
    }

    #[test]
    fn test_raw_event_from_bytes_validated() {
        let valid = br#"{"key": "value"}"#;
        let result = RawEvent::from_bytes_validated(valid.as_slice());
        assert!(result.is_ok());

        let invalid = b"not valid json";
        let result = RawEvent::from_bytes_validated(invalid.as_slice());
        assert!(result.is_err());
    }

    #[test]
    fn test_raw_event_hash_consistency() {
        let raw1 = RawEvent::from_str(r#"{"key": "value"}"#);
        let raw2 = RawEvent::from_str(r#"{"key": "value"}"#);
        assert_eq!(raw1.hash(), raw2.hash());

        let raw3 = RawEvent::from_str(r#"{"key": "other"}"#);
        assert_ne!(raw1.hash(), raw3.hash());
    }

    #[test]
    fn test_raw_event_bytes_clone() {
        let raw = RawEvent::from_str(r#"{"key": "value"}"#);
        let bytes1 = raw.bytes();
        let bytes2 = raw.bytes();
        assert_eq!(bytes1, bytes2);
    }

    #[test]
    fn test_raw_event_debug() {
        let raw = RawEvent::from_str(r#"{"key": "value"}"#);
        let debug = format!("{:?}", raw);
        assert!(debug.contains("RawEvent"));
        assert!(debug.contains("len"));
        assert!(debug.contains("hash"));
    }

    #[test]
    fn test_raw_event_from_event() {
        let event = Event::new(json!({"key": "value"}));
        let raw: RawEvent = event.into();
        assert!(!raw.is_empty());
    }

    #[test]
    fn test_raw_event_from_json_value() {
        let value = json!({"key": "value"});
        let raw: RawEvent = value.into();
        assert!(!raw.is_empty());
    }

    #[test]
    fn test_internal_event_new() {
        let raw = Bytes::from(r#"{"key": "value"}"#);
        let event = InternalEvent::new(raw.clone(), 12345, 0);
        assert_eq!(event.raw, raw);
        assert_eq!(event.insertion_ts, 12345);
        assert_eq!(event.shard_id, 0);
    }

    #[test]
    fn test_internal_event_from_value() {
        let event = InternalEvent::from_value(json!({"key": "value"}), 12345, 0);
        assert_eq!(event.insertion_ts, 12345);
        assert_eq!(event.shard_id, 0);
        let parsed = event.parse().unwrap();
        assert_eq!(parsed["key"], "value");
    }

    #[test]
    fn test_internal_event_as_bytes() {
        let raw = Bytes::from(r#"{"key": "value"}"#);
        let event = InternalEvent::new(raw.clone(), 12345, 0);
        assert_eq!(event.as_bytes(), raw.as_ref());
    }

    #[test]
    fn test_batch_new() {
        let events = vec![
            InternalEvent::from_value(json!({"i": 0}), 1, 0),
            InternalEvent::from_value(json!({"i": 1}), 2, 0),
        ];
        let batch = Batch::new(0, events, 100);
        assert_eq!(batch.shard_id, 0);
        assert_eq!(batch.len(), 2);
        assert_eq!(batch.sequence_start, 100);
        assert!(!batch.is_empty());
    }

    #[test]
    fn test_batch_empty() {
        let batch = Batch::new(0, vec![], 0);
        assert!(batch.is_empty());
        assert_eq!(batch.len(), 0);
    }

    #[test]
    fn test_stored_event_new() {
        let raw = Bytes::from(r#"{"key":"value"}"#);
        let event = StoredEvent::new("stream-123".to_string(), raw, 12345, 0);
        assert_eq!(event.id, "stream-123");
        let parsed = event.parse().unwrap();
        assert_eq!(parsed["key"], "value");
        assert_eq!(event.insertion_ts, 12345);
        assert_eq!(event.shard_id, 0);
    }

    // Regression: raw_str() used to silently return "{}" for invalid UTF-8
    // instead of reporting an error (BUGS_3 #4).
    #[test]
    fn test_stored_event_raw_str_valid_utf8() {
        let raw = Bytes::from(r#"{"key":"value"}"#);
        let event = StoredEvent::new("id".to_string(), raw, 0, 0);
        assert_eq!(event.raw_str().unwrap(), r#"{"key":"value"}"#);
    }

    #[test]
    fn test_stored_event_raw_str_invalid_utf8_returns_err() {
        let raw = Bytes::from(vec![0xff, 0xfe, 0xfd]);
        let event = StoredEvent::new("id".to_string(), raw, 0, 0);
        assert!(event.raw_str().is_err());
    }

    // Regression: Serialize impl used to silently replace invalid raw bytes
    // with null. Now it returns a serialization error (BUGS_4 #3).
    #[test]
    fn test_stored_event_serialize_valid() {
        let raw = Bytes::from(r#"{"key":"value"}"#);
        let event = StoredEvent::new("id".to_string(), raw, 123, 0);
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"key\""));
        assert!(json.contains("\"value\""));
    }

    #[test]
    fn test_stored_event_serialize_invalid_raw_returns_error() {
        let raw = Bytes::from(b"not valid json".as_slice());
        let event = StoredEvent::new("id".to_string(), raw, 0, 0);
        let result = serde_json::to_string(&event);
        assert!(
            result.is_err(),
            "serializing invalid raw bytes should error, not silently return null"
        );
    }

    /// Regression: `StoredEvent::Serialize` must preserve the
    /// raw bytes byte-for-byte instead of round-tripping through
    /// `serde_json::Value`. Pre-fix the round-trip discarded
    /// original whitespace, normalized number formatting
    /// (`1.0` → `1`), and re-ordered map keys alphabetically.
    /// Any downstream that hashed or signed the serialized form
    /// and expected byte-equality with the input silently failed
    /// verification.
    #[test]
    fn stored_event_serialize_preserves_raw_byte_for_byte() {
        // Pin three cases where the JsonValue round-trip
        // demonstrably mutates the bytes:
        // 1. Whitespace (round-trip strips internal whitespace).
        // 2. Number formatting (`1.0` → `1`).
        // 3. Key ordering (BTreeMap default re-orders alphabetically).
        let cases: &[&[u8]] = &[
            // Whitespace: extra spaces inside the object literal.
            br#"{ "key" : "value" }"#,
            // Number formatting: 1.0 (would become 1 via Value).
            br#"{"x":1.0,"y":2.5}"#,
            // Key ordering: "z" before "a" (BTreeMap would re-order).
            br#"{"z":1,"a":2}"#,
        ];

        for raw_bytes in cases {
            let raw = Bytes::copy_from_slice(raw_bytes);
            let event = StoredEvent::new("id".into(), raw.clone(), 0, 0);
            let json = serde_json::to_string(&event).unwrap();

            // The serialized output must contain the input bytes
            // verbatim (as-is, not re-formatted by serde_json).
            let expected_raw = std::str::from_utf8(raw_bytes).unwrap();
            assert!(
                json.contains(expected_raw),
                "regression: StoredEvent serialization must contain the raw \
                 input verbatim (no whitespace stripping, no number \
                 normalization, no key re-ordering).\n\
                 input:  {expected_raw}\n\
                 output: {json}"
            );
        }
    }

    /// Pin that `Batch::with_nonce` writes the passed value into the
    /// `process_nonce` field. The bus relies on this to stamp the
    /// loaded persistent nonce on every emitted batch;
    /// a future refactor that ignored the parameter would silently
    /// regress JetStream cross-restart dedup.
    #[test]
    fn batch_with_nonce_round_trips_the_passed_value() {
        let events: Vec<InternalEvent> = (0..3)
            .map(|i| InternalEvent::from_value(serde_json::json!({"i": i}), i, 0))
            .collect();
        let nonce: u64 = 0xDEAD_BEEF_CAFE_F00D;
        let batch = Batch::with_nonce(7, events, 42, nonce);
        assert_eq!(batch.shard_id, 7);
        assert_eq!(batch.sequence_start, 42);
        assert_eq!(
            batch.process_nonce, nonce,
            "Batch::with_nonce must write the passed nonce verbatim",
        );
    }

    /// Regression: every `Batch` constructed in this process via
    /// `Batch::new` (the per-process-fallback constructor) must
    /// carry the same `process_nonce`. Adapters that persist
    /// `(shard_id, sequence_start)` for dedup compose it with this
    /// nonce so two processes that both happen to start sequencing
    /// at zero (the default after `BatchWorker::new`) don't collide
    /// on `(shard, 0, 0…)` in the backend's dedup window.
    ///
    /// We pin two contracts:
    /// 1. The nonce is non-zero (a process started at exactly
    ///    `UNIX_EPOCH` with pid 0 would defeat the XOR — defend
    ///    against trivially predictable values).
    /// 2. Multiple `Batch::new` calls in the same process yield
    ///    the same nonce (so retries within a process land on the
    ///    same dedup key).
    #[test]
    fn batch_process_nonce_is_stable_within_process() {
        let nonce_a = batch_process_nonce();
        let nonce_b = batch_process_nonce();
        assert_eq!(
            nonce_a, nonce_b,
            "within a single process the nonce must be stable"
        );

        // And it shows up on every Batch.
        let b1 = Batch::new(0, vec![], 0);
        let b2 = Batch::new(1, vec![], 100);
        assert_eq!(b1.process_nonce, nonce_a);
        assert_eq!(b2.process_nonce, nonce_a);

        // Best-effort: not-zero. UNIX_EPOCH+pid=0 would leave the
        // XOR at zero; vanishingly unlikely on any real host but a
        // cheap sanity check.
        assert_ne!(nonce_a, 0, "process nonce should be non-zero");
    }

    /// Cubic-ai P2: `Batch::with_nonce` must enforce the non-zero
    /// invariant `batch_process_nonce` already upholds. A caller
    /// that hands in `0` (e.g., uninitialized field, default-init
    /// of a `u64`, or a misconfigured fallback path) MUST NOT have
    /// `0` propagate into `process_nonce` — JetStream and other
    /// consumers treat `0` as a sentinel for "no nonce / legacy
    /// path", which silently disables cross-restart dedup.
    ///
    /// The post-fix behavior coerces `0 → 1`, mirroring the
    /// `PersistentProducerNonce::create_new` and
    /// `batch_process_nonce` policy.
    #[test]
    fn with_nonce_coerces_zero_to_one_to_preserve_dedup_sentinel() {
        // Zero in → one out (the canonical replacement, not
        // `batch_process_nonce` — we don't want the function to
        // silently route around an explicit caller error).
        let b = Batch::with_nonce(0, vec![], 0, 0);
        assert_eq!(
            b.process_nonce, 1,
            "with_nonce(producer_nonce=0) must coerce to 1 — \
             letting 0 through would silently disable JetStream \
             cross-restart dedup (consumers treat 0 as sentinel)",
        );

        // Non-zero passes through verbatim.
        let b = Batch::with_nonce(0, vec![], 0, 0xDEAD_BEEF);
        assert_eq!(
            b.process_nonce, 0xDEAD_BEEF,
            "non-zero producer_nonce must pass through unchanged",
        );
    }
}
