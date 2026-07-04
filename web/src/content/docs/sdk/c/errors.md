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

## Beyond the bus

Recovery strategies (retry/hedge/failover) and the agentic loop live above the C
ABI. To use them from a C program, drive the mesh through the `net-mesh` CLI
([CLI Reference](/docs/reference/cli)) or embed a fuller binding. The
[C SDK overview](/docs/sdk/c) explains the scope.
