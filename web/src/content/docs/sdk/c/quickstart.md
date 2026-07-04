# C — Quickstart

Ingest events and poll them back, freeing what the ABI hands you.

```c
#include "net.h"
#include <stdio.h>
#include <string.h>

int main(void) {
    // net_init returns NULL on failure. Pass NULL for defaults.
    net_handle_t node = net_init("{\"num_shards\": 4}");
    if (!node) {
        fprintf(stderr, "net_init failed\n");
        return 1;
    }

    // Ingest raw JSON. Returns 0 on success, nonzero on error (e.g. backpressure).
    const char *ev = "{\"sensor\":\"lidar\",\"range_m\":12.5}";
    net_ingest_raw(node, ev, strlen(ev));

    // Poll. `out` is owned by you and MUST be freed with net_free_poll_result.
    net_poll_result_t out;
    if (net_poll_ex(node, 100, "", &out) == 0) {
        for (size_t i = 0; i < out.count; i++) {
            printf("event: %.*s\n", (int)out.events[i].raw_len, out.events[i].raw);
        }
        // `out.next_id` is the cursor for the next poll (pass it back as the cursor).
        net_free_poll_result(&out);          // free the batch
    }

    net_shutdown(node);                       // frees the handle
    return 0;
}
```

`net_ingest_raw` accepting the event means it was placed in the local ring buffer —
acceptance, not delivery (see
[Submitted Is Not Completed](/docs/worldview/submitted-is-not-completed)).

Polling is cursor-paginated: `net_poll_ex(handle, limit, cursor, &out)` fills
`out.events` / `out.count`, sets `out.next_id` (the next cursor) and `out.has_more`.
Pass `out.next_id` back as the `cursor` to page forward; `""` starts from the
current tail. There is no async subscribe — poll on an interval for a live loop.

## The three memory rules

The header states them, and they're the whole discipline of the C ABI:

- Handles from `net_init()` are freed with **`net_shutdown()`**.
- Poll results from `net_poll_ex()` are freed with **`net_free_poll_result()`**.
- Strings from `net_generate_keypair()` (and similar) are freed with
  **`net_free_string()`**.

## Next

[Errors](/docs/sdk/c/errors).
