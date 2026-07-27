# Payloads — Size, Encoding, Large Blobs

Short reference for what to put on the bus and what to keep off it.

---

## Wire format

JSON bytes. Always. There is no schema registry, no Protobuf, no Avro, no MessagePack on the wire. The producer serializes to JSON; the consumer parses from JSON.

This is true across all five SDKs, including C (which expects valid JSON in `net_ingest_raw`).

## Practical size guidance

| Payload size | Verdict | Notes |
|---|---|---|
| < 1 KB | Ideal | Single UDP packet on mesh transport, fits any ring buffer slot, lowest latency. |
| 1 KB – 64 KB | Fine | Default per-stream credit window is 64 KB. Above that, mesh transport will backpressure on a single event. |
| 64 KB – 1 MB | Works but slow | Many UDP fragments on mesh, stresses the ring buffer, increases drop probability under load. Tune `window_bytes` upward if you must. |
| > 1 MB | Don't | The bus is a coordination layer, not a file transfer. Use the patterns below. |

These are guidelines. The SDK does not enforce a hard maximum — but the further right you go, the more you fight the system.

## Patterns for large data

### Reference, don't embed

If you have a 50 MB payload (a video frame, a model checkpoint, a CSV), put it where it belongs (S3, MinIO, a local file, a CDN URL) and emit a small event with the reference:

```json
{"frame_id": "abc123", "url": "s3://bucket/frames/abc123.bin", "sha256": "..."}
```

The bus carries the coordination signal at full speed; the bulk data moves through whatever bulk-data path your infrastructure already has.

### Chunk + reassemble

If the data must traverse the mesh (you have no shared storage layer), chunk on the producer and reassemble on the consumer. Each chunk is its own event with `(blob_id, chunk_index, total_chunks, payload)`. Consumer accumulates by `blob_id` and emits the reassembled blob to its own channel when complete. Add a timeout so partial blobs don't accumulate forever.

This is your code, not the SDK's. Keep it simple — Net does not provide a built-in chunking layer.

### Stream-of-events instead of one big event

If the "payload" is naturally a sequence (sensor readings, log lines, token stream), emit each unit as its own event. Subscribers process incrementally. This is the design Net is optimized for.

```python
# Bad: 10 MB JSON array of readings
node.emit({"readings": all_ten_thousand})

# Good: 10,000 small events
for reading in all_ten_thousand:
    node.emit(reading)
```

The "good" version uses more total bytes (more JSON envelope per event) but is far healthier for backpressure, latency, and consumer parallelism.

## Encoding considerations

- **UTF-8.** JSON requires it. Don't put binary in a JSON string field unless you base64-encode it (and at that point, see "Reference, don't embed").
- **Numbers.** JSON numbers are floats by default. For `u64` IDs and timestamps, the SDKs already handle the precision boundary (BigInt in TS, native int in Python, u64 in Rust/Go/C) — but if you're constructing JSON manually, pass IDs as strings to avoid silent precision loss.
- **TS-specific gotcha:** `Receipt.timestamp` and `StoredEvent.insertionTs` are **both** typed as `number` (`net/crates/net/sdk-ts/src/types.ts:52-63`), not `bigint`. Nanosecond epoch values cross `Number.MAX_SAFE_INTEGER` (≈ 9.0×10^15) at roughly **104 days past 1970** — every realistic timestamp on these fields has *already* lost precision. They round-trip the relative ordering fine, but don't trust the low-order digits and don't compute nanosecond/sub-microsecond latency deltas from them. If you need that, get the timestamp from a bigint-preserving source (the underlying NAPI `StoredEvent` exposes BigInts pre-narrowing — read `node.bus()` directly) or work in millisecond resolution.
- **Reserved field: `_channel`.** TS and Python channel APIs inject this on publish and filter on it during subscribe. Don't put a `_channel` field in your own payload.
- **No null-padding, no fixed-width encoding.** This is JSON, not binary protocol — payloads are variable-length.

## Schema interop across language boundaries

The wire format is JSON bytes. There is no schema registry. Producer and consumer must agree on the shape — but the language each side speaks has its own JSON-mapping quirks that bite at the boundary. The traps below are the ones that survive review and only show up in production.

1. **`u64` fields and `Number.MAX_SAFE_INTEGER`.** JSON's `number` type and JS's `Number` are IEEE 754 doubles; values above 2^53 lose precision silently. Net's NAPI binding handles `u64` ids/timestamps/counters as `BigInt` on the TS side — `Stats.eventsIngested` / `eventsDropped` (`net/crates/net/bindings/node/index.d.ts:1890-1895`), `NetStreamStats.lastActivityNs` / `txSeq` / `rxSeq` (`:1667-1678`), peer ids on `MeshNode` (`:645,653,680`). On the wire, however, `serde_json` writes a plain integer literal for a Rust `u64`. A TS consumer that calls `JSON.parse` on a u64 over 2^53 gets a wrong number with no error. Mitigation: serialize `u64`s as strings (`#[serde(serialize_with = "...")]`) or parse with `json-bigint`. For ids that fit in u53, you're fine.

2. **Casing conventions.** Rust idiomatic: `snake_case`. TS idiomatic: `camelCase`. Python idiomatic: `snake_case`. Net's NAPI bindings auto-rename Rust struct fields to camelCase **for binding-level types** (`Stats.eventsIngested`, `NetStreamStats.txCreditRemaining`, etc. — visible across `index.d.ts`; the convention is napi-rs default for `#[napi(object)]` structs). Your own event payload keeps whatever casing you put in the JSON. Pick one casing and stick to it across all SDKs. `snake_case` is the cheapest default — Rust and Python agree natively, and TS adapts.

3. **Optional vs absent vs null.** Rust `Option<T>` serializes to `null` (or omitted with `#[serde(skip_serializing_if = "Option::is_none")]`). TS distinguishes `undefined` vs `null` — `JSON.stringify` drops `undefined`, keeps `null`. Python's `Optional[T]` deserializes both `null` and missing-key the same way (both → `None`) with `dataclasses` and Pydantic. Standardize on **omit absent fields** (Rust `skip_serializing_if` + TS `undefined` + Python missing key all line up). Don't use `null` as a sentinel — it's a portability landmine.

4. **Floats and precision.** JSON has no `f32` distinction; floats round-trip as `f64`. A Rust `f32` serialized through the bus and read back into `f32` may lose the last bit of mantissa (ULP loss). Either standardize on `f64` end-to-end or accept the ULP loss and document it.

5. **Timestamps.** Net itself uses nanosecond `u64` epoch counts internally (`TimestampGenerator`, `net/crates/net/src/timestamp.rs:42`), which collides with the u64-as-string concern above. For event payloads, prefer ISO-8601 strings (`"2026-05-01T12:34:56.789Z"`) — unambiguous, lexicographically sortable, well-supported in every language. Reserve the bus's own `insertion_ts` for bus bookkeeping; don't conflate it with your application timestamp. (See `payloads.md` § Encoding for the TS-side `insertionTs: number` precision caveat.)

6. **Binary blobs in JSON.** base64-encode if the blob has to ride inside a JSON event. Rust: `base64::engine::general_purpose::STANDARD.encode(...)`. TS: `Buffer.from(...).toString('base64')`. Python: `base64.b64encode(...).decode()`. But for anything > 256 KB, fall back to the reference pattern above — the bus is a coordination layer.

7. **Pydantic vs dataclass on the Python side.** Both work with the typed channel API. `net_sdk.channel._to_dict` (`net/crates/net/sdk-py/src/net_sdk/channel.py:16-25`) duck-types: it calls `model_dump()` if present (Pydantic v2), else copies `__dict__` (dataclass / plain class), else wraps as `{"_value": event}`. **Deserialize** is the asymmetric side: `TypedChannel.subscribe` uses `model(**data)` (`channel.py:97`) — works for dataclasses and plain `__init__`-accepting types. **For Pydantic v2 you want `model_validate(...)`, not `model(**...)`** — pass an explicit `parse=` callable to `node.channel('name', MyPydanticModel, parse=lambda raw: MyPydanticModel.model_validate_json(raw))` to use Pydantic's coercion/validation pipeline.

8. **TypeScript's structural type check is producer-side only.** `node.channel<TempReading>('name')` constrains the producer's `publish` calls at compile time. It does **not** validate inbound events at the SDK boundary — the consumer's iterator yields whatever JSON arrived, cast to `T`. If a producer sends a different shape, your `for await` loop sees a wrong-shaped object at runtime with no error. Pass a runtime validator (`node.channel<T>('name', validator)`, `net/crates/net/sdk-ts/src/channel.ts:31-40`) when consuming from a producer you don't fully control.

9. **The `_channel` reserved field.** Already covered in `apis.md` § Cross-SDK gotchas and `payloads.md` § Encoding. Don't put your own `_channel` field on a payload going through the TS or Python channel API.

10. **Schema evolution recipe.** Don't try to install a schema registry. Two patterns work:
    - **Versioned envelope:** `{"v": 2, "data": {...}}`. Consumers branch on `v`. Producers can fan out both shapes during a rollout while old consumers stay on `v=1`.
    - **Channel name versioning:** `sensors/temperature/v2` vs `sensors/temperature/v1`. Consumers subscribe to whichever they support. No branching in code; you double the channel count.
    Pick one. Mixing both is harder than either alone.

See `gotchas.md` § "How do I do schema evolution?" for the broker-migration framing.

## Throughput vs latency trade-off

Smaller events = lower latency, higher per-event overhead.
Bigger events = higher throughput, higher per-event tail latency.

For coordination signals (intents, state changes, decisions), favor small. For bulk telemetry where you want high MB/s, batch a few hundred readings into one event — but stop when one event approaches the ring-buffer slot size.

`emit_batch` / `emitBatch` / `IngestRawBatch` is the right tool for batching: it amortizes the per-call overhead while keeping each event individually addressable on the wire.

## Further reading

- [Wire Format](https://ai2070.net/docs/reference/wire-format)
- [Using the Event Bus](https://ai2070.net/docs/guides/event-bus)
