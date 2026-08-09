---
title: Quickstart
description: Ingest events and page through the poll API, freeing what the ABI hands you.
---
# C — Quickstart

Ingest events and page through the poll API, freeing what the ABI hands you.

**This program prints nothing and exits 0.** That is the correct result, and
it is worth understanding before you run it: `net_init` with no adapter
configured selects the default memory adapter, which counts events and
discards them. So `net_ingest_raw` succeeds, `net_poll_ex` succeeds, and
`out.count` is `0` — there is nothing to read back. Point the node at a Redis,
JetStream, or mesh adapter and the same loop starts returning events.

What the snippet below teaches is the *shape*: the call sequence, the return
codes to check, and who owns which allocation. It is not a demonstration that
an event travelled anywhere.

```c
#include "net.h"
#include <stdio.h>
#include <stdlib.h>   /* malloc, free */
#include <string.h>   /* strlen, memcpy */

int main(void) {
    // net_init returns NULL on failure. Pass NULL for defaults.
    net_handle_t node = net_init("{\"num_shards\": 4}");
    if (!node) {
        fprintf(stderr, "net_init failed\n");
        return 1;
    }

    // Ingest raw JSON. Returns 0 on success, nonzero on error — always check.
    const char *ev = "{\"sensor\":\"lidar\",\"range_m\":12.5}";
    if (net_ingest_raw(node, ev, strlen(ev)) != 0) {
        fprintf(stderr, "ingest rejected (full buffer / backpressure?)\n");
    }

    // Poll. `out` is owned by you and MUST be freed with net_free_poll_result.
    // A NULL cursor starts from the earliest buffered event.
    //
    // out.count is 0 on the default adapter — it discarded the event above.
    // Say so, rather than letting an empty loop read as "it worked".
    net_poll_result_t out;
    if (net_poll_ex(node, 100, NULL, &out) == 0) {
        if (out.count == 0) {
            printf("polled 0 events: the default adapter discards them\n");
        }
        for (size_t i = 0; i < out.count; i++) {
            printf("event: %.*s\n", (int)out.events[i].raw_len, out.events[i].raw);
        }
        // `out.next_id` is owned by `out`. To page forward you MUST copy it BEFORE
        // freeing — net_free_poll_result frees next_id too, so using it after the
        // free is a use-after-free.
        char *cursor = NULL;
        if (out.next_id) {
            size_t n = strlen(out.next_id) + 1;
            cursor = malloc(n);
            if (cursor) memcpy(cursor, out.next_id, n);
        }
        net_free_poll_result(&out);          // frees events AND next_id
        // ... pass `cursor` to the next net_poll_ex, then eventually:
        free(cursor);
    }

    net_shutdown(node);                       // frees the handle
    return 0;
}
```

`net_ingest_raw` accepting the event means it was placed in the local ring buffer —
acceptance, not delivery (see
[Submitted Is Not Completed](/docs/guides/submitted-is-not-completed)).

Polling is cursor-paginated: `net_poll_ex(handle, limit, cursor, &out)` fills
`out.events` / `out.count`, sets `out.next_id` (the next cursor) and `out.has_more`.
A `NULL` cursor starts from the earliest buffered event; **copy `out.next_id`
before calling `net_free_poll_result`**, then pass the copy as the `cursor` to page
forward. `strdup` does the same job in one line, but it is POSIX rather than ISO C
and disappears under `-std=c11`; the `malloc` + `memcpy` above compiles anywhere.
There is no async subscribe — poll on an interval for a live loop. The full paging
loop is in the C header's `README.md`.

This program compiles clean under `-std=c11 -Wall -Wextra -Werror`, and CI checks
that on every commit.

## The three memory rules

The header states them, and they're the whole discipline of the C ABI:

- Handles from `net_init()` are freed with **`net_shutdown()`**.
- Poll results from `net_poll_ex()` are freed with **`net_free_poll_result()`**.
- Strings from `net_generate_keypair()` (and similar) are freed with
  **`net_free_string()`**.
