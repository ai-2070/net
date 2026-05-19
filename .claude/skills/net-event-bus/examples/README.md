# Sanity-check examples

Each file in this directory is a **minimal, runnable** example that proves install → publish → subscribe works end-to-end. Use these as the first thing a developer runs after `npm install` / `pip install` / `cargo add` — before they write any application code.

All examples use the **memory transport** (no network, no peers needed) and run in a single process. Once these work, the developer knows the SDK is wired up correctly and can move on to mesh transport, channels, persistence, etc.

| File | SDK | Run |
|---|---|---|
| `hello.ts` | `@net-mesh/sdk` | `npx tsx hello.ts` |
| `hello.py` | `net-sdk` | `python hello.py` |
| `hello.rs` | `net-sdk` | `cargo run --example hello` (drop into a crate's `examples/` dir) |
| `hello.go` | `github.com/ai-2070/net/go` | `go run hello.go` |
| `hello.c` | `net.h` | `gcc hello.c -lnet -lpthread -ldl -lm && ./a.out` |

Each prints exactly one line: the event it emitted, received, and round-tripped. If you see that line, the SDK is working.
