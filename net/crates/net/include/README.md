# Net C SDK

C ABI for the Net mesh — a latency-first encrypted mesh where services and
agents announce capabilities, discover each other, and invoke work over typed
RPC.

Unlocks every language that can call C: C++, Zig, Nim, Lua, Ruby, Java, C#,
Dart, Swift, Kotlin, Haskell, Erlang, PHP.

**Docs: <https://ai2070.net/docs/sdk/c/quickstart>** ·
[Headers and linking](https://ai2070.net/docs/sdk/c/headers-and-linking) ·
[Memory and threading](https://ai2070.net/docs/sdk/c/memory-and-threading)

## Ten headers, five libraries

This is not one header. The full map — which header resolves against which
cdylib, and which surface each covers — is in
[Headers and Linking](https://ai2070.net/docs/sdk/c/headers-and-linking).

| Header | Guard | Surface | Library |
|---|---|---|---|
| `net.h` | `NET_SDK_H` | Event bus | `libnet` |
| `net.go.h` | `NET_SDK_H` | Mesh, capabilities, channels, compute | `libnet` |
| `net_cortex.h` | `NET_CORTEX_H` | RedEX, CortEX, NetDb | `libnet` |
| `net_transport.h` | `NET_TRANSPORT_H` | Blob + directory transfer | `libnet` |
| `net_rpc.h` | `NET_RPC_H` | nRPC | `libnet_rpc` |
| `net_meshdb.h` | `NET_MESHDB_H` | Federated queries | `libnet_meshdb` |
| `net_meshos.h` | `NET_MESHOS_H` | Daemon authoring | `libnet_meshos` |
| `net_deck.h` | `NET_DECK_H` | Operator surface | `libnet_deck` |
| `net_org.h` | `NET_ORG_H` | Organization capability auth | `libnet_org` |
| `net_mcp.h` | `NET_MCP_H` | MCP bridge, consent / pin surface | `libnet_mcp_ffi` |

> **`net.h` and `net.go.h` share the `NET_SDK_H` guard** — only one can be
> active per translation unit, and `net.go.h` is *not* a superset
> (`net_ingest_raw_ex`, `net_poll_ex` and `net_stats_ex` are `net.h`-only).
> Need both? Split across translation units.

## Build

```bash
cargo build --release --features ffi,net       # libnet
cargo build --release -p net-rpc-ffi           # libnet_rpc, etc.
```

```bash
gcc -o app app.c -L target/release -lnet -lpthread -ldl -lm
LD_LIBRARY_PATH=target/release ./app           # DYLD_LIBRARY_PATH on macOS
```

## Quickstart

```c
#include "net.h"
#include <stdio.h>
#include <string.h>

int main(void) {
    net_handle_t node = net_init("{\"num_shards\": 4}");   // NULL on failure
    if (!node) return 1;

    const char *ev = "{\"sensor\":\"lidar\",\"range_m\":12.5}";
    if (net_ingest_raw(node, ev, strlen(ev)) != 0) {
        fprintf(stderr, "ingest rejected (backpressure?)\n");
    }

    net_poll_result_t out;
    if (net_poll_ex(node, 100, NULL, &out) == 0) {
        for (size_t i = 0; i < out.count; i++) {
            printf("event: %.*s\n", (int)out.events[i].raw_len, out.events[i].raw);
        }
        /* next_id is owned by `out` — copy it BEFORE freeing to page forward. */
        char *cursor = out.next_id ? strdup(out.next_id) : NULL;
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

## Building with Claude Code

Net looks like Kafka or NATS from the outside, and the model underneath is
different enough that an agent working from surface familiarity will write
integration code that runs and is quietly wrong. Install the skills first:

```bash
git clone https://github.com/ai-2070/net-claude-skill.git /tmp/net-claude-skill
mkdir -p ~/.claude/skills
cp -R /tmp/net-claude-skill/net-event-bus /tmp/net-claude-skill/net-payments ~/.claude/skills/
```

Restart Claude Code and run `/skills` — **net-event-bus** and **net-payments**
should be listed. `net-event-bus` covers pub/sub, nRPC, the MCP bridge,
organization capability auth, the gang-claim scheduler, and RedEX / CortEX /
Dataforts. `net-payments` covers x402 pricing, quotes, settlement and spend
policy. Full install options — project-scoped, symlinked to stay current — in
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
