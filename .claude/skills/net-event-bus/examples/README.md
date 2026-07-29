# Sanity-check examples

Each file in this directory is a **minimal, runnable** example that proves install → publish → subscribe works end-to-end. Use these as the first thing a developer runs after `npm install` / `pip install` / `cargo add` — before they write any application code.

All examples use the **memory transport** (no network, no peers needed) and run in a single process. Once these work, the developer knows the SDK is wired up correctly and can move on to mesh transport, channels, persistence, etc.

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

Each prints exactly one line: the event it emitted, received, and round-tripped. If you see that line, the SDK is working.

## What CI checks here

Every file in this directory is **compiled or type-checked** on each pull request against the current tree — so a renamed method or a changed signature breaks the build rather than reaching you. `hello.c`, `hello.go`, `hello.rs` and `hello.py` run through `.github/scripts/check-skill-examples.sh`; `hello.ts` goes through `.github/scripts/check-skill-example-ts.sh`, run by the two jobs that build the napi type declarations it needs.

Both are driven from `.github/skill-examples.json`, which requires every binding to be listed for every route as either a checked file or an explicit, reasoned absence. A source file sitting in this directory but missing from that manifest is an error — otherwise it would ship to users with nothing compiling it.

That is a compile floor, not a promise that the commands above run. Executing them needs built artifacts (the napi module, the Python wheel, the C shared library), so the "prints exactly one line" claim is verified in the release pipelines rather than per-PR. If one of these fails to run for you against a released build, that is a bug worth reporting.
