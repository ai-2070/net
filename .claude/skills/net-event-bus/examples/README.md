# Sanity-check examples

Each file in this directory is a **minimal, runnable** example. Use these as the first thing a developer runs after `npm install` / `pip install` / `cargo add` — before they write any application code.

All examples use the **memory transport** (no network, no peers needed) and run
in a single process.

**Memory transport does not deliver events, and that is by design.** It selects
the Noop adapter, which counts batches and discards them — `adapter/noop.rs`
says "Just count, don't store", and its `poll_shard` returns an empty result.
Events flow producer → ring buffer → drain worker → adapter, so with Noop
there is nothing to read: `subscribe()` never yields and `poll()` always
returns zero.

These examples therefore prove **ingestion**, not round-trip. That is the right
scope for an install check — it exercises the whole path a developer can get
wrong (package name, import name, construction, config validation, shutdown)
without needing a broker or a second host. To actually receive events you need
an adapter that retains them: Redis, JetStream, or the mesh transport between
two nodes. See `mesh.md`.

Two routes, each in all five bindings.

**`hello.*` — construct · publish · subscribe · shutdown.** The install check.

| File | Install as | Import as | Run |
|---|---|---|---|
| `hello.ts` | `@net-mesh/sdk` | `@net-mesh/sdk` | `npx tsx hello.ts` |
| `hello.py` | `net-mesh-sdk` | `net_sdk` | `python hello.py` |
| `hello.rs` | `net-mesh-sdk` | `net_sdk` | `cargo run --example hello` (drop into a crate's `examples/` dir) |
| `hello.go` | `github.com/ai-2070/net/go` | `net` | `go run hello.go` |
| `hello.c` | — | `net.h` | `gcc hello.c -lnet -lpthread -ldl -lm && ./a.out` |

**`observe.*` — ingest under backpressure · read stats · handle one failure.**
The counterpart, and the one worth reading before going to production: under the
default backpressure modes drops are **silent**, and `events_dropped` is the only
evidence you get. Each file also pins that binding's own stats shape, which is
where they differ most:

| Binding | The trap it pins |
|---|---|
| `observe.rs` | `FailProducer` is the one mode that returns a structured error instead of dropping quietly |
| `observe.ts` | counters are `bigint` — compare against `0n`; no batch counter exists |
| `observe.py` | `events_ingested` / `events_dropped` only; no batch counter |
| `observe.go` | Go-cased fields, and `BatchesDispathed` is misspelled in the shipped module |
| `observe.c` | `net_stats_ex`, and why `net.h` cannot be combined with `net.go.h` |

**The Rust and Python packages publish under a different name than they import.** `cargo add net-mesh-sdk` then `use net_sdk::…`; `pip install net-mesh-sdk` then `from net_sdk import …`. There is no package called `net-sdk` — don't install one.

`hello.*` prints one line reporting that the bus accepted the event. If you see it, the SDK is installed and wired up correctly.

## What CI checks here

Every file here is **compiled or type-checked** on each pull request against the
current tree, so a renamed method or a changed signature breaks the build rather
than reaching you. `hello.c`, `hello.go`, `hello.rs` and `hello.py` go through
`.github/scripts/check-skill-examples.sh`; the `.ts` files go through
`.github/scripts/check-skill-example-ts.sh`, run by the two jobs that build the
napi type declarations it needs.

**Every example is also executed**, in all five bindings, with its stdout
matched against a contract and bounded by a timeout. That is not belt-and-braces: a compile floor cannot
catch an example that builds and then hangs, and both `hello.rs` and `hello.ts`
did exactly that for months — clean compile, blocked forever on a subscribe that
could never yield — while this README promised they printed one line. Nothing
short of running them would have found it.

Each runs where its artifacts already exist, so the marginal cost is the
execution itself: Rust in `skills.yml`'s `examples` job, TypeScript in the two
jobs that build the napi module, Python in `ci.yml`'s `python-tests` (the only
job with both the maturin binding and the `net_sdk` wrapper), and Go and C in
`ci.yml`'s `go-tests`, the only job that produces a linkable `libnet`.

Both are driven from `.github/skill-examples.json`, which requires every binding
to be listed for every route as either a checked file or an explicit, reasoned
absence — and, for execution, records which bindings run where. Its coverage
report prints **▶** for executed against **✓** for compiled-only, so a partially
executed route can never read as a fully executed one. A source file sitting in
this directory but missing from that manifest is an error; otherwise it would
ship to users with nothing compiling it.
