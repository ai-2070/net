# C SDK

The C ABI (`net.h`) is the smallest, most explicit binding. It exposes the **event
bus** — ingest and poll — with manual memory management, and it's what you use to
embed Net in a C/C++ program or bind a language that isn't one of the first-class
SDKs.

```bash
# build the shared library + bundle the header
cargo build --release --features ffi,net
# then link against the cdylib and include net.h
```

## Scope (stated plainly)

The C ABI covers the **bus** (`net_init`, `net_ingest_raw`, `net_poll_ex`,
`net_shutdown`) plus keypair and dedup helpers. The higher-level **agentic mesh
surface** — capability announce/discover, tools, nRPC invoke, blob transfer — is
**not exposed in the C ABI**. For that loop, use one of the fuller bindings
([Rust](/docs/sdk/rust), [TypeScript](/docs/sdk/typescript),
[Python](/docs/sdk/python), [Go](/docs/sdk/go)), or drive the mesh from C by
shelling out to the `net-mesh` CLI ([CLI Reference](/docs/reference/cli)).

So the C spine is two pages, not seven — pretending otherwise would be fiction:

1. **[Quickstart](/docs/sdk/c/quickstart)** — ingest and poll, with memory rules.
2. **[Errors](/docs/sdk/c/errors)** — return codes and ownership.
