---
title: Errors
description: The C ABI has no exceptions — it communicates failure through return values and puts memory ownership entirely in your hands.
---
# C — Errors and Ownership

The C ABI has no exceptions — it communicates failure through **return values** and
puts memory ownership entirely in your hands.

## Return-code convention

- **Functions returning `int`** (`net_ingest_raw`, `net_poll_ex`, `net_shutdown`,
  …) return **`0` on success** and **nonzero on error**. Always check.
- **Functions returning a handle or pointer** (`net_init`, keypair generation)
  return **`NULL` on failure**.

Backpressure — the ring buffer being full — surfaces as a **nonzero return** from
`net_ingest_raw`. That's the one condition a retry can fix; treat other nonzero
returns as bugs or state changes, the same rule as every binding
([Error Codes](/docs/reference/error-codes)).

```c
if (net_ingest_raw(node, ev, len) != 0) {
    // full ring buffer (backpressure) or a rejected event — back off and retry,
    // or check your config. Do not spin on a non-backpressure failure.
}
```

## Ownership (leaks are yours)

Nothing is garbage-collected. Every allocation the ABI hands you has exactly one
free function:

| You got it from | Free it with |
|---|---|
| `net_init()` | `net_shutdown()` |
| `net_poll_ex()` (the `net_poll_result_t`) | `net_free_poll_result()` |
| `net_generate_keypair()` and similar strings | `net_free_string()` |

Free a poll result once you've copied out the event bytes you need — the `raw`
pointers inside it are owned by the result, not by you, so don't hold them past the
`net_free_poll_result` call.

## The `NET_ERR_*` table

The `int` returns from `net.h` are drawn from this set:

| Name | Value | Meaning |
|---|---|---|
| `NET_SUCCESS` | 0 | Success |
| `NET_ERR_NULL_POINTER` | -1 | A required pointer argument was `NULL` |
| `NET_ERR_INVALID_UTF8` | -2 | Input bytes were not valid UTF-8 |
| `NET_ERR_INVALID_JSON` | -3 | Input did not parse as JSON |
| `NET_ERR_INIT_FAILED` | -4 | Node construction failed |
| `NET_ERR_INGESTION_FAILED` | -5 | Event rejected — includes backpressure |
| `NET_ERR_POLL_FAILED` | -6 | Poll failed |
| `NET_ERR_BUFFER_TOO_SMALL` | -7 | Output buffer below the 256-byte minimum |
| `NET_ERR_SHUTTING_DOWN` | -8 | Handle is shutting down |
| `NET_ERR_UNKNOWN` | -99 | Unclassified failure |

The separate libraries add their own ranges — `NET_ERR_REDEX` and friends in
`net_cortex.h`, the MeshDB codes in `net_meshdb.h`. Each header declares the
codes for its own surface.

## Beyond the bus

Recovery strategies (retry, hedge, failover) and the agentic loop are available
from C — capability discovery lives in `net.go.h` and nRPC in `net_rpc.h`. See
[Headers and Linking](/docs/sdk/c/headers-and-linking) for which library each
surface resolves against, and the [C SDK overview](/docs/sdk/c) for scope.
