# Net C SDK

One header, one shared library. This is the entire C SDK.

Unlocks every language that can call C: C++, Zig, Nim, Lua, Ruby, Java, C#, Dart, Swift, Kotlin, Haskell, Erlang, PHP.

Latest release: [v0.10 — "Killing Moon" Phase III](../docs/RELEASE_v0.10_KILLING_MOON_PHASE_III.md) is a hardening release. The FFI surface picked up several behavior changes that affect any C consumer; see [Behavior changes in v0.10 (FFI)](#behavior-changes-in-v010-ffi) below for the per-call summary.

## Files

- `net.h` — narrow event-bus surface (init / ingest / poll / stats / shutdown). Uses the `NET_SDK_H` include guard.
- `net.go.h` — broader mesh + compute surface (sessions, channels, capabilities, NAT, daemon dispatch, custom placement filters, daemon caps, predicate helpers, capability validation, predicate debug session). In-crate mirror of [`go/net.h`](../../../../go/net.h) at the repo root. Also uses `NET_SDK_H` and overlaps with the event-bus surface from `net.h`, but it is **not** a strict superset (`net_ingest_raw_ex`, `net_poll_ex`, and `net_stats_ex` are still `net.h`-only). Pick **one** of `net.h` or `net.go.h` per translation unit based on the symbols you need; if you need both surfaces in the same program, split across translation units.
- `net_rpc.h` — nRPC C SDK (request/response surface for the separate `libnet_rpc` cdylib). Independent header guard (`NET_RPC_H`); composes cleanly alongside whichever of `net.h` / `net.go.h` you chose.
- Libraries: `libnet.{so,dylib,dll}` (main) + `libnet_compute.{so,dylib,dll}` (compute) + `libnet_rpc.{so,dylib,dll}` (nRPC). Build with `cargo build --release --features ffi,net` for `libnet`; `-p net-compute-ffi` and `-p net-rpc-ffi` for the others.
- Examples: `examples/basic.c` (event-bus quickstart) + `examples/capability.c` (stateless capability / predicate / where-header helpers).

## Build

```bash
# Build the shared library
cargo build --release --features ffi,net

# The library is at:
# Linux:  target/release/libnet.so
# macOS:  target/release/libnet.dylib
# Windows: target/release/net.dll
```

## Quick Start

```c
#include "net.h"
#include <stdio.h>
#include <string.h>

int main(void) {
    // Create a node
    net_handle_t node = net_init("{\"num_shards\": 4}");
    if (!node) return 1;

    // Ingest
    const char* event = "{\"token\": \"hello\"}";
    net_receipt_t receipt;
    net_ingest_raw_ex(node, event, strlen(event), &receipt);
    printf("shard=%d ts=%llu\n", receipt.shard_id, (unsigned long long)receipt.timestamp);

    // Flush
    net_flush(node);

    // Poll (structured, no JSON parsing needed)
    net_poll_result_t result;
    net_poll_ex(node, 100, NULL, &result);
    for (size_t i = 0; i < result.count; i++) {
        printf("%.*s\n", (int)result.events[i].raw_len, result.events[i].raw);
    }
    net_free_poll_result(&result);

    // Stats (structured)
    net_stats_t stats;
    net_stats_ex(node, &stats);
    printf("ingested=%llu dropped=%llu\n",
        (unsigned long long)stats.events_ingested,
        (unsigned long long)stats.events_dropped);

    // Shutdown
    net_shutdown(node);
    return 0;
}
```

## Compile and Link

```bash
# GCC
gcc -o app app.c -L target/release -lnet -lpthread -ldl -lm

# Run
LD_LIBRARY_PATH=target/release ./app       # Linux
DYLD_LIBRARY_PATH=target/release ./app     # macOS
```

## API

### Lifecycle

| Function | Description |
|----------|-------------|
| `net_init(config_json)` | Create a node. NULL config for defaults. Returns handle. |
| `net_shutdown(handle)` | Shut down and free resources. |
| `net_version()` | Library version string (static, do not free). |
| `net_num_shards(handle)` | Number of active shards. |

### Ingestion

| Function | Description |
|----------|-------------|
| `net_ingest_raw(handle, json, len)` | Ingest raw JSON (fastest). |
| `net_ingest_raw_ex(handle, json, len, &receipt)` | Ingest with receipt (shard_id, timestamp). |
| `net_ingest(handle, json, len)` | Ingest with JSON validation. |
| `net_ingest_raw_batch(handle, jsons, lens, count)` | Batch ingest. Returns count. |
| `net_ingest_batch(handle, json_array)` | Ingest from JSON array string. |

### Consumption

| Function | Description |
|----------|-------------|
| `net_poll(handle, request_json, out_buffer, buffer_len)` | Poll (JSON interface). |
| `net_poll_ex(handle, limit, cursor, &result)` | Poll (structured, no JSON). Free with `net_free_poll_result`. |
| `net_free_poll_result(&result)` | Free a structured poll result. |

### Statistics

| Function | Description |
|----------|-------------|
| `net_stats(handle, out_buffer, buffer_len)` | Stats (JSON interface). |
| `net_stats_ex(handle, &stats)` | Stats (structured, no JSON). |

### Utilities

| Function | Description |
|----------|-------------|
| `net_flush(handle)` | Flush pending batches. |
| `net_generate_keypair()` | Generate mesh keypair. Free with `net_free_string`. |
| `net_free_string(s)` | Free a string from `net_generate_keypair`. |

### Redis Streams dedup helper (`redis` feature)

The Redis adapter writes a stable `dedup_id` field on every XADD
entry (`{producer_nonce:hex}:{shard_id}:{sequence_start}:{i}`) so
duplicate stream entries from the producer-side `MULTI/EXEC`
timeout race can be filtered at consume time. Helper API:

| Function | Description |
|----------|-------------|
| `net_redis_dedup_new(capacity)` | Create a helper. `0` selects the default 4096. Never returns NULL. |
| `net_redis_dedup_free(handle)` | Free a helper handle. NULL is a no-op. |
| `net_redis_dedup_is_duplicate(handle, dedup_id)` | Test-and-insert. Returns 1 = duplicate, 0 = new, -1 = NULL, -2 = invalid UTF-8. |
| `net_redis_dedup_len(handle)` | Number of distinct ids tracked. |
| `net_redis_dedup_capacity(handle)` | Configured LRU capacity. |
| `net_redis_dedup_is_empty(handle)` | 1 = empty, 0 = non-empty, -1 = NULL. |
| `net_redis_dedup_clear(handle)` | Drop all tracked ids (e.g. on consumer-group rebalance). |

Canonical consumer loop:

```c
net_redis_dedup_t* dedup = net_redis_dedup_new(0);

/* For each XRANGE / XREAD entry, extract the `dedup_id` field
 * from the field map and probe the helper. */
const char* dedup_id = ...; /* from your Redis client */
int rc = net_redis_dedup_is_duplicate(dedup, dedup_id);
if (rc == 0) {
    process(entry);     /* new — process AND we're now marked seen */
} else if (rc == 1) {
    /* duplicate — skip */
}

net_redis_dedup_free(dedup);
```

The helper is transport-agnostic — bring your own `hiredis` /
`redis-rs` / equivalent client. Sizing: ~10k events/sec at a 1
min dedup window → capacity ~600,000. Default 4096 fits
low-throughput / short-window deployments.

## Types

```c
net_handle_t        // Opaque node handle (void*)
net_receipt_t       // { shard_id, timestamp }
net_event_t         // { id, id_len, raw, raw_len, insertion_ts, shard_id }
net_poll_result_t   // { events, count, next_id, has_more }
net_stats_t         // { events_ingested, events_dropped, batches_dispatched }
net_error_t         // NET_SUCCESS (0), NET_ERR_* (negative)
```

## Error Codes

| Code | Name | Value |
|------|------|-------|
| `NET_SUCCESS` | Success | 0 |
| `NET_ERR_NULL_POINTER` | Null pointer | -1 |
| `NET_ERR_INVALID_UTF8` | Invalid UTF-8 | -2 |
| `NET_ERR_INVALID_JSON` | Invalid JSON | -3 |
| `NET_ERR_INIT_FAILED` | Init failed | -4 |
| `NET_ERR_INGESTION_FAILED` | Ingestion failed | -5 |
| `NET_ERR_POLL_FAILED` | Poll failed | -6 |
| `NET_ERR_BUFFER_TOO_SMALL` | Buffer too small | -7 |
| `NET_ERR_SHUTTING_DOWN` | Shutting down | -8 |
| `NET_ERR_UNKNOWN` | Unknown error | -99 |

## Thread Safety

All functions are thread-safe. Handles can be shared across threads.

## Subscription Pattern

The C SDK does not manage threads. Use `net_poll_ex` in your own loop:

```c
char* cursor = NULL;
while (running) {
    net_poll_result_t result;
    int rc = net_poll_ex(node, 100, cursor, &result);
    if (rc < 0) break;

    for (size_t i = 0; i < result.count; i++) {
        process(&result.events[i]);
    }

    // Copy cursor before freeing the result.
    free(cursor);
    cursor = result.next_id ? strdup(result.next_id) : NULL;
    net_free_poll_result(&result);
}
free(cursor);
```

## Mesh transport

The header in this directory (`include/net.h`) is intentionally a
**narrow, public, event-bus-only** surface — every symbol declared
here is a stability commitment.

The mesh transport (encrypted peer sessions, channels, NAT
traversal, capability discovery) is implemented in the same
shared library but lives behind a **separate, broader header**:
[`go/net.h`](../../../../go/net.h) at the repo root, which the Go
cgo bindings cargo-include directly. That header is the
de-facto reference for C consumers who want the mesh API. Symbols
are stable in practice but not committed in the same way as
`include/net.h`. An identical-content mirror lives in this
directory at [`net.go.h`](./net.go.h) — it exists so the parity
test (`cr22_c_header_parity_with_rust_neterror`) can `include_str!`
both headers without escaping the crate root, and it's a
convenient drop-in for C consumers who want a copy that ships
with the crate.

**One header per translation unit.** All three files use the same
`#ifndef NET_SDK_H` include guard, so including more than one in
the same `.c` file silently drops the second include — symbols
only declared there will fail to compile. The narrow / broad
split is also **not a strict superset**:

- `include/net.h` declares `net_ingest_raw_ex`, `net_poll_ex`,
  `net_stats_ex` (structured no-JSON paths) that the broader
  mesh header does not.
- `go/net.h` (and its `net.go.h` mirror) declares the entire
  mesh surface (sessions, streams, channels, capabilities, NAT)
  that `include/net.h` does not.

Pick the header that matches the surface your translation unit
actually uses. If a single program needs both — the structured
`_ex` poll path *and* the mesh API — split them across translation
units: one `.c` file includes `include/net.h` and exposes a thin
internal API to the rest of your program, another includes the
mesh header. The resulting object files link against the same
`libnet.{so,dylib,dll}` regardless of which header declared each
symbol.

A mesh node is its own handle (`net_meshnode_t*`), created via
`net_mesh_new` and torn down via `net_mesh_shutdown` — independent
of the bus handle (`net_handle_t`). A single process can hold both
simultaneously regardless of how the headers are included.

The Go bindings (under repo-root `go/`) wrap this surface; their
README has runnable examples for every function family. The
section below is a function inventory — for usage prose, see
[`go/README.md`](../../../../go/README.md).

### Quick start (mesh)

```c
#include "net.go.h"   /* broader header — adjacent to net.h in this directory */

net_meshnode_t* mesh = NULL;
const char* cfg =
    "{\"bind_addr\":\"127.0.0.1:9000\",\"psk_hex\":\"42424242...\"}";
if (net_mesh_new(cfg, &mesh) != 0) return 1;
net_mesh_start(mesh);

/* Announce hardware/software/tag fingerprints. */
net_mesh_announce_capabilities(mesh, "{\"tags\":[\"gpu\",\"prod\"]}");

/* Query the local capability index. Result is a JSON array of
 * node ids; free with net_free_string. */
char* result = NULL;
size_t result_len = 0;
net_mesh_find_nodes(mesh, "{\"require_tags\":[\"gpu\"]}",
                    &result, &result_len);
printf("matches: %.*s\n", (int)result_len, result);
net_free_string(result);

net_mesh_shutdown(mesh);
```

### Mesh function families

| Family | Functions | Purpose |
|--------|-----------|---------|
| Lifecycle | `net_mesh_new`, `net_mesh_shutdown`, `net_mesh_start`, `net_mesh_public_key_hex`, `net_mesh_entity_id` | Create / start / tear down a mesh node. |
| Connections | `net_mesh_connect`, `net_mesh_accept`, `net_mesh_connect_direct` | Establish encrypted peer sessions. |
| Streams | `net_mesh_open_stream`, `net_mesh_send`, `net_mesh_send_with_retry`, `net_mesh_send_blocking`, `net_mesh_stream_stats`, `net_mesh_recv_shard` | Per-peer ordered byte streams. |
| Channels | `net_mesh_register_channel`, `net_mesh_subscribe_channel`, `net_mesh_subscribe_channel_with_token`, `net_mesh_unsubscribe_channel`, `net_mesh_publish` | Topic-based pub/sub over the mesh. |
| Capabilities | `net_mesh_announce_capabilities`, `net_mesh_find_nodes`, `net_mesh_find_nodes_scoped`, `net_mesh_find_best_node`, `net_mesh_find_best_node_scoped` | Capability discovery + scored placement. |
| Predicate evaluation | `net_predicate_evaluate` | Stateless local evaluator (Phase 9c). Returns `1` / `0` for a wire-format predicate against `(tags, metadata)`; same boolean every binding produces. Cross-binding contract pinned by `tests/cross_lang_capability/predicate_eval.json`. |
| Predicate `where:` header | `net_predicate_to_where_header` | Encode a predicate as the canonical `net-where:` request-header pair (Phase 9b). Mirror of the Go SDK's `WhereHeader`; pairs directly with the `*_with_headers` calls in `libnet_rpc` (`net_rpc_call_with_headers` / `net_rpc_call_service_with_headers` / `net_rpc_call_streaming_with_headers` — see the nRPC table below). Wire format pinned by `tests/cross_lang_capability/predicate_nrpc_envelope.json`. |
| Capability validation | `net_validate_capabilities` | Stateless `CapabilitySet` validator (Phase 9a). Wire-format caps in, JSON `ValidationReport` (`errors` + `warnings`) out; same shape every binding produces. Cross-binding contract pinned by `tests/cross_lang_capability/capability_validation.json`. |
| Predicate debug session | `net_predicate_evaluate_with_trace`, `net_predicate_aggregate_debug_report`, `net_predicate_redact_metadata_keys` | Stateless debug helpers (Phase 9d). Single-eval clause trace; corpus-wide per-clause aggregation; host-side label redaction. Cross-binding contracts pinned by `tests/cross_lang_capability/predicate_trace.json`, `predicate_debug_report.json`, `predicate_debug_report_redacted.json`. |
| Daemon capability authoring | `net_compute_set_daemon_caps_dispatcher` | Optional per-daemon `required` / `optional` `CapabilitySet` declaration; without it daemons advertise empty sets (back-compat). See "Daemon capability authoring (Phase 6)" below. |
| Custom placement filters | `net_compute_set_placement_filter_dispatcher`, `net_compute_register_placement_filter`, `net_compute_unregister_placement_filter`, `net_compute_has_placement_filter` | Plug a host-language predicate into `StandardPlacement.custom_filter_id` — substrate calls back per candidate. See "Custom placement-filter callback (Phase 7)" below. |
| NAT traversal | `net_mesh_nat_type`, `net_mesh_reflex_addr`, `net_mesh_peer_nat_type`, `net_mesh_probe_reflex`, `net_mesh_reclassify_nat`, `net_mesh_traversal_stats`, `net_mesh_set_reflex_override`, `net_mesh_clear_reflex_override` | Optional optimization — routed-handshake fallback always works. |

### Scoped capability discovery

`scope:*` reserved tags on a `CapabilitySet` narrow *who finds whom*
at query time. The wire format and forwarders are unchanged —
enforcement is purely query-side.

| Tag form               | Effect                                                          |
|------------------------|-----------------------------------------------------------------|
| _(none)_               | `Global` (default) — visible to every query that doesn't opt out. |
| `scope:subnet-local`   | Visible only under `{"kind":"same_subnet"}` queries.            |
| `scope:tenant:<id>`    | Visible to `{"kind":"tenant","tenant":"<id>"}` queries (and to permissive global queries). |
| `scope:region:<name>`  | Visible to `{"kind":"region","region":"<name>"}` queries.       |

```c
// GPU pool advertised to one tenant only.
net_mesh_announce_capabilities(mesh,
    "{\"tags\":[\"model:llama3-70b\",\"scope:tenant:oem-123\"]}");

// Tenant-scoped query.
char* result = NULL; size_t result_len = 0;
net_mesh_find_nodes_scoped(mesh,
    "{\"require_tags\":[\"model:llama3-70b\"]}",
    "{\"kind\":\"tenant\",\"tenant\":\"oem-123\"}",
    &result, &result_len);
net_free_string(result);

// Scored placement — pick the highest-scoring node within a scope.
uint64_t winner = 0;
int has_match = 0;
net_mesh_find_best_node_scoped(mesh,
    "{\"filter\":{\"require_gpu\":true},\"prefer_more_vram\":1.0}",
    "{\"kind\":\"tenant\",\"tenant\":\"oem-123\"}",
    &winner, &has_match);
if (has_match) printf("placement -> %llu\n", (unsigned long long)winner);
```

`scope.kind` accepts `any` (default) | `global_only` | `same_subnet`
| `tenant` (with `tenant`) | `tenants` (with `tenants`) | `region`
(with `region`) | `regions` (with `regions`). Both snake_case
(`global_only`) and camelCase (`globalOnly`) are accepted so
fixtures round-trip across SDKs. Strictest scope wins —
`scope:subnet-local` dominates tenant/region tags on the same set.

`net_mesh_find_best_node[_scoped]` use an out-param contract: the
return code is 0 on both hit and miss; `*out_has_match` is `1` on
hit (with `*out_node_id` populated) or `0` on miss. The boolean
disambiguates from `node_id == 0`, which is a valid id.

Full design + cross-SDK rationale:
[`docs/SCOPED_CAPABILITIES_PLAN.md`](../docs/SCOPED_CAPABILITIES_PLAN.md).

### Daemon capability authoring (Phase 6)

Optional per-daemon `requiredCapabilities` / `optionalCapabilities` declaration. Wires the substrate's `MeshDaemon::required_capabilities` / `optional_capabilities` (Phase G slice 2) through the C ABI so daemons spawned via the Go-style factory dispatcher can declare what hardware / region / runtime they need before placement decisions run.

Without this dispatcher installed, daemons advertise empty cap sets and `StandardPlacement` treats them as "runs anywhere" — back-compat with pre-Phase-6 consumers. Phase 6 of `docs/plans/CAPABILITY_SYSTEM_SDK_PLAN.md`.

**Lifecycle:**

```c
/* 1. At process init: install the dispatcher ONCE. First-call-wins. */
static int my_daemon_caps(
    uint64_t daemon_id,
    char** out_required_json, size_t* out_required_len,
    char** out_optional_json, size_t* out_optional_len)
{
    /* Look up your daemon by daemon_id. Allocate UTF-8 JSON
     * buffers via C.malloc / libc::malloc — Rust frees them via
     * libc::free after parsing. NULL / zero-length means "no
     * caps declared for this side" (either side may be omitted
     * independently).
     *
     * Wire shape: {"tags": ["hardware.gpu", ...],
     *              "metadata": {"intent": "ml-training", ...}} */
    const char* req = "{\"tags\":[\"hardware.gpu\"],\"metadata\":{}}";
    size_t req_len = strlen(req);
    char* req_buf = (char*)malloc(req_len);
    memcpy(req_buf, req, req_len);
    *out_required_json = req_buf;
    *out_required_len = req_len;

    *out_optional_json = NULL;  /* no optional caps declared */
    *out_optional_len = 0;
    return NET_COMPUTE_OK;
}

net_compute_set_daemon_caps_dispatcher(my_daemon_caps);

/* 2. Subsequent net_compute_spawn / migration reconstruction
 *    queries the dispatcher once per daemon construction; the
 *    bridge stores the parsed sets for the daemon's lifetime. */
```

The dispatcher is invoked at BOTH the initial-spawn path and the migration-target reconstruction path — same caps shape applies on every reincarnation. Idempotent: parsed once, stored on the bridge, never re-fetched on event processing.

`StandardPlacement` consumes the declared caps via the in-tree resource / intent / scope axes plus the hard-required check (artifact's required tags must be a subset of the candidate's tags). Combine this with Phase 7's custom-filter callback for full control over placement decisions.

### Custom placement-filter callback (Phase 7)

Path A (`StandardPlacement` config-driven scoring) is the default; this is the **escape hatch** when the in-tree axes don't capture the placement decision the operator needs. The substrate calls back into the consumer (C / language X) once per candidate when scoring; the consumer returns keep / drop. Phase 7 of `docs/plans/CAPABILITY_SYSTEM_SDK_PLAN.md` — full prose lives in the plan.

Symbols are in `libnet_compute` (separate cdylib), declared in `net.go.h` next to the existing daemon dispatcher.

**Lifecycle:**

```c
/* 1. At process init: install the trampoline ONCE. First-call-wins;
 *    subsequent calls are no-ops. */
static int my_placement_filter(
    const char* filter_id_ptr, size_t filter_id_len,
    uint64_t node_id,
    const char* candidate_json_ptr, size_t candidate_json_len);

net_compute_set_placement_filter_dispatcher(my_placement_filter);

/* 2. After the mesh node is live, register a filter id. The id must
 *    match what the daemon spec / `StandardPlacement.custom_filter_id`
 *    references on the substrate side. The mesh_arc is NOT consumed. */
const char* id = "pf-gpu-must-be-loaded";
int rc = net_compute_register_placement_filter(
    mesh_arc,            /* from net_mesh_arc_clone — caller still owns */
    id, strlen(id));
if (rc != NET_COMPUTE_OK) { /* handle error — see net.go.h for codes */ }

/* 3. Scoring fires the trampoline per candidate. Return:
 *      1 — keep candidate (placement-score 1.0 in Rust)
 *      0 — drop candidate (placement_score returns None)
 *      negative — error; treated as veto. Log the detail yourself.
 *
 *    Wire shape: candidate_json_ptr is a JSON string of length
 *    candidate_json_len:
 *      {"node_id": uint64, "tags": [string], "metadata": {key:value}}
 *    Buffers are owned by Rust for the call's duration; copy if needed.
 */
static int my_placement_filter(
    const char* filter_id_ptr, size_t filter_id_len,
    uint64_t node_id,
    const char* candidate_json_ptr, size_t candidate_json_len)
{
    /* parse candidate_json_ptr with your JSON library of choice
     * (cjson, jansson, RapidJSON, etc.) and apply your predicate */
    return /* 1 | 0 | negative */;
}

/* 4. On shutdown: drop the registration. Existing in-flight scoring
 *    calls already holding the Arc complete normally. */
net_compute_unregister_placement_filter(id, strlen(id));
```

**Counter:** every successful trampoline invocation increments `dataforts_placement_callback_invocations_total{binding}` on the substrate side, where `binding` is set per-language-SDK at register-time. C consumers see `binding="<your-binding-label>"` if you register through a language-specific SDK; raw C consumers calling `net_compute_register_placement_filter` directly inherit the default.

**Same JSON wire shape across all bindings.** The Node TSFN bridge marshals candidates natively; the Python `Py<PyAny>` bridge does the same; the Go cgo bridge uses this exact JSON. Cross-binding compat fixture: `tests/cross_lang_capability/predicate_eval.json` (each binding wraps the predicate as a placement filter and asserts the kept/vetoed verdict matches direct evaluation).

### Mesh types

```c
net_meshnode_t      // Opaque mesh-node handle (separate from net_handle_t).
net_mesh_stream_t   // Opaque per-peer stream handle.
```

### Where to look for full prose

- [`net.go.h`](./net.go.h) (or the repo-root [`go/net.h`](../../../../go/net.h)
  — identical content) — every function has a doc-comment
  with input shapes, error codes, and ownership rules.
- [`go/README.md`](../../../../go/README.md) — runnable
  examples for the full mesh surface (the Go bindings are a thin
  wrapper over `net.h`, so the example translation back to C is
  near-1:1).
- [`net/README.md`](../README.md) — architectural overview, NAT
  traversal design, channel visibility model.

## nRPC (request / response over the mesh)

nRPC is the request/response convention layer (deadlines,
queue-group fan-out, response streaming, end-to-end cancellation)
riding on top of the pub/sub mesh. Lives in a separate cdylib at
[`bindings/go/rpc-ffi`](../bindings/go/rpc-ffi) — the Go binding
consumes it, but the ABI is callable from any C-ABI consumer.

**Library:** `libnet_rpc` (cdylib + staticlib). Build:

```bash
cargo build --release -p net-rpc-ffi
```

**Header:** [`net_rpc.h`](./net_rpc.h) — the canonical C SDK
header for nRPC. Drop-in for C / C++ / Zig / Swift / Java JNI /
etc.; identical declarations to the cgo block in
`bindings/go/net/mesh_rpc.go`. Same one-header-per-translation-
unit discipline as `net.h` / `net.go.h` (different `#ifndef`
guard — `NET_RPC_H` — so combining with the mesh headers in one
TU is fine).

```c
#include "net_rpc.h"
gcc -o app app.c -L target/release -lnet_rpc -lpthread -ldl -lm
```

**ABI version:** consumers SHOULD call `net_rpc_abi_version() ->
uint32_t` at process init and refuse to load on mismatch. Version
`0x0001` covers Phase B5 (lifecycle + unary call + serve +
service discovery) plus B6 (streaming + ABI version stamp).

**Entry-point families** (full per-function doc-comments in
`net_rpc.h`):

| Family | Functions |
|---|---|
| Lifecycle | `net_rpc_abi_version`, `net_rpc_new`, `net_rpc_free`, `net_rpc_id` |
| Free helpers | `net_rpc_free_cstring`, `net_rpc_response_free`, `net_rpc_find_service_nodes_free` |
| Cancellation | `net_rpc_reserve_cancel_token`, `net_rpc_cancel_call` |
| Handler dispatcher | `net_rpc_set_handler_dispatcher`, `net_rpc_reserve_handler_id`, `RpcHandlerFn` typedef |
| Unary calls | `net_rpc_call`, `net_rpc_call_service` |
| Header-bearing calls | `net_rpc_call_with_headers`, `net_rpc_call_service_with_headers`, `net_rpc_call_streaming_with_headers` (Phase 9b end-to-end — accept a `net_rpc_header_t[]`; pair with `net_predicate_to_where_header`) |
| Service discovery | `net_rpc_find_service_nodes` |
| Serve | `net_rpc_serve`, `net_rpc_serve_handle_id`, `net_rpc_serve_handle_close`, `net_rpc_serve_handle_free` |
| Streaming | `net_rpc_call_streaming`, `net_rpc_stream_next`, `net_rpc_stream_grant`, `net_rpc_stream_call_id`, `net_rpc_stream_close`, `net_rpc_stream_free` |

Ownership: every `uint8_t*` / `char*` / `uint64_t*` returned
out-of-band is freed via the matching
`net_rpc_response_free` / `net_rpc_free_cstring` /
`net_rpc_find_service_nodes_free`.

**Error codes (`int` return):**

| Code | Constant                       | Meaning                                              |
| ---- | ------------------------------ | ---------------------------------------------------- |
|  `0` | `NET_RPC_OK`                   | Success.                                             |
| `-1` | `NET_RPC_ERR_NULL`             | NULL pointer where a handle was expected.            |
| `-2` | `NET_RPC_ERR_CALL_FAILED`      | Generic — structured detail in `**out_err` CString.  |
| `-3` | `NET_RPC_ERR_ALREADY_SERVING`  | `serve` rejected — handler already registered.       |
| `-4` | `NET_RPC_ERR_NO_DISPATCHER`    | `set_handler_dispatcher` was never called.           |
| `-5` | `NET_RPC_ERR_INVALID_UTF8`     | Non-UTF-8 bytes where a string was expected.         |
| `-6` | `NET_RPC_ERR_STREAM_DONE`      | Stream produced its terminal item; release handle.   |

**Structured error format:** `format_rpc_error` emits
`<kind>: <detail>` (no `nrpc:` prefix; consumers add it). Kinds:
`no_route`, `timeout`, `server_error` (`status=0xNNNN`),
`transport`, `codec_encode`, `codec_decode`. Application-defined
status codes are in `0x8000..=0xFFFF`; the SDK stables
`NRPC_TYPED_BAD_REQUEST = 0x8000` and
`NRPC_TYPED_HANDLER_ERROR = 0x8001` for typed-handler decode /
runtime errors.

For the canonical cross-binding contract spec — including the
`cross_lang_echo_sum` service used by every binding's wire-format
compat test — see [`net/README.md#nrpc`](../README.md#nrpc).

## Behavior changes in v0.10 (FFI)

### Panics no longer unwind across the FFI boundary

The cdylib is built with `panic = "abort"` and every `extern "C"`
body is wrapped in `catch_unwind`. A Rust panic that was previously
*partially* completing the call before unwinding (and silently
corrupting your process across the cgo / N-API / cffi boundary) now
either returns a defined error code or aborts the process cleanly.
Callers that depended on partial-completion no longer get it.

### Length validation on every wide-input entry point

`net_ingest`, `net_ingest_raw`, `net_ingest_raw_batch`,
`net_ingest_raw_ex`, `net_mesh_publish`, `net_redex_file_append`,
`net_identity_sign`, `net_identity_install_token`, and
`net_parse_token` now reject `len > isize::MAX as usize` (i.e.
`SSIZE_MAX` on 64-bit; `INT_MAX` on 32-bit) before constructing the
slice. A C caller passing a stray sign-extended `-1` previously
triggered immediate UB before any guard fired — now it returns an
error.

### Alignment checks on handle dereferences

Every FFI handle accessor now checks `is_aligned_to::<HandleType>()`
before dereferencing. A misaligned `*mut` returned from a wrapper
that allocated through a non-Rust allocator returns a defined error
instead of UB.

### `net_free_poll_result` is idempotent

After freeing, the function now nulls `result->events`,
`result->next_id`, and zeros `result->count` / `result->has_more`.
Subsequent calls on the same struct are no-ops; passing `NULL` is
also a no-op. Callers that ran their own field-nulling defensively
can drop it.

### `net_ingest_raw_batch` surfaces dropped indices

The function takes two new optional out-params:

```c
int net_ingest_raw_batch(
    net_handle_t handle,
    const char* const* jsons,
    const size_t* lens,
    size_t count,
    size_t* out_failed_indices,   /* nullable; up to `count` u32 indices */
    size_t* out_failed_len        /* nullable; written to with the count */
);
```

A null entry pointer or an invalid-UTF-8 entry no longer silently
disappears from the accepted count — the index is appended to
`out_failed_indices`. Callers passing `NULL` for both new params
keep the old "count returned" semantics, but should treat
`returned_count < count` as "drops happened, you don't know which."

### `net_poll` rejects undersized buffers up front

Buffers below `MIN_RESPONSE_BUFFER` (256 bytes) are now rejected
with `NET_ERR_BUFFER_TOO_SMALL` *before* the cursor is advanced.
Pre-fix the cursor was advanced first and then the response was
dropped — every event in the failed serialization was silently
lost. Sizing rule: `4 KB` is comfortable; the structured
`net_poll_ex` path is unaffected.

### Strict config parsing

`parse_config_json` (the JSON dialect every FFI `net_init`-shaped
call accepts) now errors instead of silently falling back:

- Unknown `backpressure_mode` strings (typos like `"DropOldset"`,
  retired names like `"FailProduce"`) return
  `NET_ERR_INVALID_JSON`. Pre-fix they silently selected
  `"drop_newest"` and you got a different durability profile with
  no signal.
- Zero values for `retention_max_events`, `retention_max_bytes`,
  `retention_max_age_ms` are rejected (they previously meant
  "evict everything immediately on first append" — almost always
  unintended). Use `null` or omit the field for "no limit."
- Zero values for `heartbeat_interval_ms`, `session_timeout_ms`
  (Net adapter), and mesh `heartbeat_ms` are rejected. A 0 ms
  heartbeat busy-loops a CPU.
- A new `Sample { rate }` arm is accepted on `backpressure_mode`
  with `rate` validation.

### `net_mesh_find_*` modality strings strictly validated

`parse_modality_cap` (called from `net_mesh_announce_capabilities`,
`net_mesh_find_nodes[_scoped]`, `net_mesh_find_best_node[_scoped]`)
now returns `NET_ERR_CHANNEL` on unknown modality strings instead of
silently falling back to `Modality::Text`. A typo in
`require_modalities` previously returned wrong nodes with no error.

### `net_generate_keypair` / `net_free_string` always linkable

Both symbols are now exported in builds without the `net` feature
(via no-op stubs) so consumers linking against a `net`-less cdylib
no longer hit load-time missing-symbol errors despite the header
promising the symbol.

### `MigrationError::NoTargetAvailable`

Auto-placement (the wrappers that take a capability filter and
pick a target node) returns the typed `NoTargetAvailable` variant
when the scheduler finds no candidate, instead of fabricating
`TargetUnavailable(0)` (which surfaced "target node 0x0 unavailable"
to operators). C consumers that string-matched on the rendered
error need to add the new arm.

### Concurrent `net_shutdown` is serialized

A second/third caller of `net_shutdown` no longer returns `Success`
while the first caller is still inside `runtime.block_on(bus.shutdown())`.
The shutdown is now atomic across concurrent callers; only one
caller observes the actual shutdown result, the others see a
defined "already shutting down" return.

## License

Apache-2.0
