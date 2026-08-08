# C binding

Read `../apis.md` first for the four surfaces and the cross-SDK rules. This page
is only what is C-specific.

## Headers and libraries — there is no single "C SDK"

C is **ten headers over six shared libraries**, and picking the wrong pair is the
first thing that goes wrong.

| Header | Guard | Surface | Link against |
|---|---|---|---|
| `net.h` | `NET_SDK_H` | Event bus | `libnet` |
| `net.go.h` | `NET_SDK_H` | Mesh, capabilities, channels, compute | `libnet` |
| `net_cortex.h` | `NET_CORTEX_H` | RedEX, CortEX, NetDb | `libnet` |
| `net_transport.h` | `NET_TRANSPORT_H` | Blob + directory transfer | `libnet` |
| `net_rpc.h` | `NET_RPC_H` | nRPC | `libnet` |
| `net_meshdb.h` | `NET_MESHDB_H` | Federated queries | `libnet` |
| `net_meshos.h` | `NET_MESHOS_H` | Daemon authoring | `libnet` |
| `net_deck.h` | `NET_DECK_H` | Operator surface | `libnet` |
| `net_org.h` | `NET_ORG_H` | Organization capability auth | `libnet` |
| `net_mcp.h` | `NET_MCP_H` | MCP bridge, consent / pin surface | `libnet` |

**`net.h` and `net.go.h` share the `NET_SDK_H` guard, and `net.go.h` is not a
superset.** Include one and the other silently vanishes. `net_ingest_raw_ex`,
`net_poll_ex` and `net_stats_ex` are `net.h`-only; every `net_mesh_*` symbol —
announce, discovery, channels, streams, the island scheduler — is in `net.go.h`.
Needing both means splitting across two translation units. The symptom is an
implicit-declaration error that reads like a missing feature flag.

```bash
cargo build --release --features ffi,net       # libnet
cargo build --release -p net-ffi               # libnet — every surface
gcc -o app app.c -L target/release -lnet -lpthread -ldl -lm
```

## Construction and lifecycle

```c
#include "net.h"

net_handle_t node = net_init("{\"num_shards\": 4}");
const char* json = "{\"sensor_id\":\"A1\",\"celsius\":22.5}";
net_ingest_raw(node, json, strlen(json));

net_poll_result_t result;
if (net_poll_ex(node, 100, NULL, &result) == 0) {
    for (size_t i = 0; i < result.count; i++) {
        printf("%.*s\n", (int)result.events[i].raw_len, result.events[i].raw);
    }
    net_free_poll_result(&result);            // MUST free
}

net_shutdown(node);
```

Configuration is a JSON string to `net_init`, which returns `NULL` on failure.

## The runtime model — synchronous polling only

No async, no callbacks, no subscribe. A live consumer is this loop on an
interval. Pass `NULL` as the cursor on the first call.

**The cursor trap:** `net_free_poll_result` frees `next_id` along with the
events. Copy the cursor *before* freeing, or you have a use-after-free that will
usually appear to work. `strdup(result.next_id)` and `free()` it yourself.

All functions are thread-safe, and handles can be shared across threads. Two
exceptions: `net_redis_dedup_t` is per-thread (one helper per consumer thread),
and concurrent `net_shutdown` on the same handle is serialised rather than
double-freeing.

## Ownership — three rules

| You got it from | You free it with |
|---|---|
| `net_init()` | `net_shutdown()` |
| `net_poll_ex()` | `net_free_poll_result()` |
| `net_generate_keypair()` and similar string returns | `net_free_string()` |

`net_version()` returns a **static** string — do not free it.
`net_free_poll_result` is idempotent and `NULL`-safe.

## The buffer-capacity rule no compiler enforces

`ring_buffer_capacity` must be a **power of two and at least 1024**. It is
validated in the shared core config at construction, so every binding raises the
same way — and no compile or type check catches it. The default is 1,048,576
(1M events per shard), which is also why a "demonstrate backpressure" snippet
that emits a few thousand events into a default node drops nothing at all.

Spelt `net_init("{\"ring_buffer_capacity\": 1024}")` in this binding.

## Errors

Return codes: `0` = success, negative = error (`NET_ERR_*`). There is no
exception or `Result` path.

Two boundary guarantees worth knowing: panics do not unwind into your process
(the cdylib is `panic = "abort"` and every `extern "C"` body is wrapped), and
`(ptr, len)` pairs are length-validated, so a stray sign-extended `-1` returns an
error rather than undefined behaviour. `net_poll` rejects buffers under 256
bytes with `NET_ERR_BUFFER_TOO_SMALL` **without advancing the cursor** — size for
4 KB and stop thinking about it. The structured `net_poll_ex` path is unaffected.

## Gaps

`bindings/coverage.md` is authoritative. The one to know: **no A2A**, in any
header. Payments likewise — there is no `net_payment_*` or `net_x402_*` symbol
and no payments cdylib.

## Where to look when this page is not enough

- **Authoritative source:** `net/crates/net/include/` — the headers are the
  contract, and `README.md` there carries the guard and library table above.
- **Checked examples:** `../examples/hello.c` and `../examples/observe.c` — the
  second uses `net_stats_ex` and notes why it cannot also include `net.go.h`.
  Syntax-checked against the public headers in CI. Not linked, not run.

## Never infer from another binding

- There is **no async anything**. No subscribe, no iterator, no callback.
- Memory is yours to free, on the three rules above. Every other binding manages
  it for you.
- A symbol existing in one header does not make it reachable from your
  translation unit — see the guard collision above.
