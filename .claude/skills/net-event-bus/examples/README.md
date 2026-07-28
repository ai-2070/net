# Sanity-check examples

Each file in this directory is a **minimal, runnable** example that proves install → publish → subscribe works end-to-end. Use these as the first thing a developer runs after `npm install` / `pip install` / `cargo add` — before they write any application code.

All examples use the **memory transport** (no network, no peers needed) and run in a single process. Once these work, the developer knows the SDK is wired up correctly and can move on to mesh transport, channels, persistence, etc.

| File | Install as | Import as | Run |
|---|---|---|---|
| `hello.ts` | `@net-mesh/sdk` | `@net-mesh/sdk` | `npx tsx hello.ts` |
| `hello.py` | `net-mesh-sdk` | `net_sdk` | `python hello.py` |
| `hello.rs` | `net-mesh-sdk` | `net_sdk` | `cargo run --example hello` (drop into a crate's `examples/` dir) |
| `hello.go` | `github.com/ai-2070/net/go` | `net` | `go run hello.go` |
| `hello.c` | — | `net.h` | `gcc hello.c -lnet -lpthread -ldl -lm && ./a.out` |

**The Rust and Python packages publish under a different name than they import.** `cargo add net-mesh-sdk` then `use net_sdk::…`; `pip install net-mesh-sdk` then `from net_sdk import …`. There is no package called `net-sdk` — don't install one.

Each prints exactly one line: the event it emitted, received, and round-tripped. If you see that line, the SDK is working.

## What CI checks here

Every file in this directory is **compiled or type-checked** on each pull request against the current tree — so a renamed method or a changed signature breaks the build rather than reaching you. `hello.c`, `hello.go`, `hello.rs` and `hello.py` run through `.github/scripts/check-skill-examples.sh`; `hello.ts` is checked in the TypeScript SDK job instead, because it needs the napi-generated type declarations that job already builds.

That is a compile floor, not a promise that the commands above run. Executing them needs built artifacts (the napi module, the Python wheel, the C shared library), so the "prints exactly one line" claim is verified in the release pipelines rather than per-PR. If one of these fails to run for you against a released build, that is a bug worth reporting.
