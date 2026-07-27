---
title: Memory and Threading
description: Who owns what, the three free functions, the polling loop's cursor trap, and the safety guarantees the FFI boundary makes.
---

# C — Memory and Threading

The C ABI hands you memory and expects it back. There are exactly three
ownership rules, one non-obvious trap in the polling loop, and a set of
guarantees the boundary makes so that a mistake returns an error instead of
corrupting your process.

## The three rules

| You got it from | You free it with |
|---|---|
| `net_init()` | `net_shutdown()` |
| `net_poll_ex()` | `net_free_poll_result()` |
| `net_generate_keypair()` and similar string returns | `net_free_string()` |

`net_version()` returns a **static** string. Do not free it.

## The cursor trap

`net_free_poll_result` frees `next_id` along with the events. Paging forward
means reading `next_id` *after* the free unless you copy it first — a
use-after-free that will usually appear to work.

```c
char *cursor = NULL;
while (running) {
    net_poll_result_t result;
    int rc = net_poll_ex(node, 100, cursor, &result);
    if (rc < 0) break;

    for (size_t i = 0; i < result.count; i++) {
        process(&result.events[i]);
    }

    /* Copy the cursor BEFORE freeing — net_free_poll_result frees next_id. */
    free(cursor);
    cursor = result.next_id ? strdup(result.next_id) : NULL;
    net_free_poll_result(&result);
}
free(cursor);
```

A `NULL` cursor starts from the earliest buffered event. There is no async
subscribe in the C ABI — the SDK does not manage threads, so a live consumer is
this loop on an interval.

`net_free_poll_result` is idempotent: it nulls `events` and `next_id` and zeros
`count` / `has_more`, so a second call is a no-op and `NULL` is a no-op. If you
wrote defensive field-nulling around it, you can drop it.

## Threading

All functions are thread-safe, and handles can be shared across threads.

Two exceptions worth knowing:

- **`net_redis_dedup_t` is per-thread.** Use one helper per consumer thread
  rather than sharing one. See
  [Redis Streams Deduplication](/docs/reference/redis-dedup).
- **Concurrent `net_shutdown` is serialized.** Two threads racing to shut the
  same handle down will not double-free; one wins and the other is a no-op.

## What the boundary guarantees

These hold at the FFI edge, so an error in your C code surfaces as a return
code rather than undefined behaviour.

**Panics do not unwind into your process.** The cdylib is built with
`panic = "abort"` and every `extern "C"` body is wrapped in `catch_unwind`. A
Rust panic returns a defined error code or aborts cleanly — it never
half-completes a call and corrupts state across the boundary.

**Lengths are validated.** Every entry point that builds a slice from a
caller-supplied `(ptr, len)` rejects `len > SSIZE_MAX` before touching memory.
A stray sign-extended `-1` returns an error instead of triggering immediate
undefined behaviour. This covers the ingest family, `net_mesh_publish`,
`net_redex_file_append`, `net_netdb_open_from_snapshot`,
`net_mesh_subscribe_channel_with_token`, the identity and token functions, and
the blob functions.

**Handles are alignment-checked.** Every handle accessor checks alignment
before dereferencing, so a misaligned pointer from a wrapper that allocated
through a non-Rust allocator returns an error rather than reading garbage.

**Undersized poll buffers are rejected before the cursor moves.**
`net_poll` rejects buffers below 256 bytes with `NET_ERR_BUFFER_TOO_SMALL`
*without* advancing the cursor. Size for 4 KB and you will not think about it
again. The structured `net_poll_ex` path is unaffected.

## Batch ingest reports what it dropped

`net_ingest_raw_batch` takes two optional out-params:

```c
int net_ingest_raw_batch(
    net_handle_t handle,
    const char* const* jsons,
    const size_t* lens,
    size_t count,
    size_t* out_failed_indices,   /* nullable; up to `count` indices */
    size_t* out_failed_len        /* nullable; receives the count */
);
```

A null entry or invalid UTF-8 no longer vanishes from the accepted count — its
index is appended to `out_failed_indices`. Passing `NULL` for both keeps the
older "just return a count" behaviour, but then `returned_count < count` only
tells you drops happened, not which.

## Next

- [Errors](/docs/sdk/c/errors) — the return-code table
- [Headers and Linking](/docs/sdk/c/headers-and-linking)
