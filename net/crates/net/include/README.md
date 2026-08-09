# Net C SDK

C ABI for the Net mesh — a latency-first encrypted mesh where services and
agents announce capabilities, discover each other, and invoke work over typed
RPC.

Unlocks every language that can call C: C++, Zig, Nim, Lua, Ruby, Java, C#,
Dart, Swift, Kotlin, Haskell, Erlang, PHP.

**Docs: <https://ai2070.net/docs/sdk/c/quickstart>** ·
[Headers and linking](https://ai2070.net/docs/sdk/c/headers-and-linking) ·
[Memory and threading](https://ai2070.net/docs/sdk/c/memory-and-threading)

## Eleven headers, one library

This is not one header. It is eleven — and every one of them resolves against
the same `libnet`. The full map, with the surface each header covers, is in
[Headers and Linking](https://ai2070.net/docs/sdk/c/headers-and-linking).

| Header | Guard | Surface | Library |
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
| `net_subnet.h` | `NET_SUBNET_H` | Subnet authority (provision / serve / call) | `libnet` |
| `net_mcp.h` | `NET_MCP_H` | MCP bridge, consent / pin surface | `libnet` |

> **Every header resolves out of one `libnet`.** The surfaces are built as
> rlibs and linked together by `bindings/go/net-ffi`, which is the crate that
> emits the cdylib. Net used to ship one cdylib per surface; each embedded its
> own copy of the core, so linking two put two copies of the core's `static`s
> in a process — including the registry `parking_lot` uses to track blocked
> threads, which made a lock releasable without waking its waiter. One library
> makes that unreachable. Do not add a second `-l`; there is nothing to add.

> **`net_subnet.h` reuses the org surface** — the org handler dispatcher,
> `net_org_caller_t`, and the `Arc<MeshNode>` contract — and `#include`s
> `net_org.h`.

> **`net.h` and `net.go.h` share the `NET_SDK_H` guard** — only one can be
> active per translation unit, and `net.go.h` is *not* a superset
> (`net_ingest_raw_ex`, `net_poll_ex` and `net_stats_ex` are `net.h`-only).
> Need both? Split across translation units.

## Build

```bash
cargo build --release -p net-ffi               # libnet — every surface
```

```bash
gcc -o app app.c -L target/release -lnet -lpthread -ldl -lm
LD_LIBRARY_PATH=target/release ./app           # DYLD_LIBRARY_PATH on macOS
```

## Quickstart

This program prints one line and exits 0, and the line says the poll was
empty. `net_init` with no adapter configured selects the default memory
adapter, which counts events and discards them — so ingest succeeds, poll
succeeds, and `out.count` is 0. What the snippet teaches is the call shape,
the return codes, and who owns which allocation; it is not evidence that an
event travelled anywhere. Configure a Redis, JetStream, or mesh adapter
before treating a non-empty poll as the success condition.

```c
#include "net.h"
#include <stdio.h>
#include <stdlib.h>   /* malloc, free */
#include <string.h>   /* strlen, memcpy */

int main(void) {
    net_handle_t node = net_init("{\"num_shards\": 4}");   // NULL on failure
    if (!node) return 1;

    const char *ev = "{\"sensor\":\"lidar\",\"range_m\":12.5}";
    if (net_ingest_raw(node, ev, strlen(ev)) != 0) {
        fprintf(stderr, "ingest rejected (backpressure?)\n");
    }

    net_poll_result_t out;
    if (net_poll_ex(node, 100, NULL, &out) == 0) {
        if (out.count == 0) {
            printf("polled 0 events: the default adapter discards them\n");
        }
        for (size_t i = 0; i < out.count; i++) {
            printf("event: %.*s\n", (int)out.events[i].raw_len, out.events[i].raw);
        }
        /* next_id is owned by `out` — copy it BEFORE freeing to page forward.
         * `strdup` is POSIX, not ISO C, so it vanishes under `-std=c11`. */
        char *cursor = NULL;
        if (out.next_id) {
            size_t n = strlen(out.next_id) + 1;
            cursor = malloc(n);
            if (cursor) memcpy(cursor, out.next_id, n);
        }
        net_free_poll_result(&out);
        free(cursor);
    }

    net_shutdown(node);
    return 0;
}
```

## The three memory rules

| You got it from | Free it with |
|---|---|
| `net_init()` | `net_shutdown()` |
| `net_poll_ex()` | `net_free_poll_result()` |
| `net_generate_keypair()` and similar | `net_free_string()` |

`net_version()` returns a static string — do not free it. The polling cursor
trap, the threading rules, and the guarantees the boundary makes (no unwinding,
length validation, alignment checks, idempotent free) are in
[Memory and Threading](https://ai2070.net/docs/sdk/c/memory-and-threading).

## Examples

| File | Shows |
|---|---|
| `examples/basic.c` | The event-bus loop above |
| `examples/capability.c` | Capability, predicate and where-header helpers |
| `examples/meshdb.c` | MeshDB factory AST, runner, iterator, sentinel decoder |

## Claude Code Skill

Net looks like Kafka or NATS from the outside, and the model underneath is
different enough that an agent working from surface familiarity will write
integration code that runs and is quietly wrong. Install the skills first:

```bash
npx skills add ai-2070/net-claude-skill -g
```

Drop `-g` to install into the current project only. To update to the latest
version:

```bash
npx skills update -g
```

Restart Claude Code and run `/skills` — **net-event-bus** and **net-payments**
should be listed. `net-event-bus` covers pub/sub, nRPC, the MCP bridge,
organization capability auth, the gang-claim scheduler, and RedEX / CortEX /
Dataforts. `net-payments` covers x402 pricing, quotes, settlement and spend
policy. Full install options in
[Claude Skills](https://ai2070.net/docs/start/claude-skills).

## What's in the box

| Surface | Guide |
|---|---|
| Errors — the `NET_ERR_*` table | [Errors](https://ai2070.net/docs/sdk/c/errors) |
| Event bus | [Event bus](https://ai2070.net/docs/guides/event-bus) |
| Capabilities — announce and discover | [Discover and invoke](https://ai2070.net/docs/guides/discover-and-invoke) |
| nRPC | [Typed RPC](https://ai2070.net/docs/guides/nrpc) |
| RedEX / CortEX / NetDB | [Durable logs](https://ai2070.net/docs/guides/durable-logs), [Folds](https://ai2070.net/docs/guides/cortex-folds) |
| MeshDB — federated queries | [NetDB](https://ai2070.net/docs/guides/netdb-queries#federated-queries-meshdb) |
| Dataforts — blobs, cache, gravity | [Blob storage](https://ai2070.net/docs/guides/dataforts) |
| Deck — the operator surface | [Deck](https://ai2070.net/docs/reference/deck) |
| Redis Streams dedup | [Deduplication](https://ai2070.net/docs/reference/redis-dedup) |

## License

MIT OR Apache-2.0
