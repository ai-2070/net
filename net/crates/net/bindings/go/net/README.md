# Package net — Go binding for the libnet cdylib

Reference-implementation Go wrapper over the C ABI exported by the `net` crate's FFI module. No `go.mod` ships in this directory — downstream consumers vendor or copy the files into their own module tree and wire the cgo linker flags from there. See the header comment in [`redex.go`](redex.go) for the canonical build-prereq block this binding documents:

```go
// # Build prerequisites
//
//   - Build the main `net` crate as a cdylib with the cortex feature:
//
//     cd net/crates/net
//     cargo build --release --features "net netdb redex-disk"
//
//   - Add to your CGO flags:
//
//     #cgo LDFLAGS: -L/path/to/target/release -lnet
//     #cgo darwin LDFLAGS: -framework Security -framework CoreFoundation
```

The wrapper covers the operator-facing surface that landed alongside the Phase I Go binding work (RedEX + replication, Tasks / Memories adapters, NetDB, MeshDB query layer, MeshOS daemon SDK, Deck, capability / placement schema, mesh nRPC, resilience helpers). The exact symbol set is defined by the Cargo features the underlying cdylib was built with.

## Cargo features

The five feature flags that gate the storage / query / OS surfaces in `libnet`. A cdylib built without a feature silently omits its `extern "C"` entry points — the Go wrapper's corresponding methods then fail at runtime with the linker's missing-symbol error (or, for `dlopen`-based loaders, with a clean error from the FFI shim).

| Feature | C ABI symbols + Go surface enabled |
|---|---|
| `cortex` | `net_redex_*`, `net_redex_file_*`, `net_tasks_*`, `net_memories_*`, `net_netdb_*` entry points — i.e. `Redex`, `RedexFile`, `TasksAdapter`, `MemoriesAdapter`, `NetDb` on the Go side, plus the `Task` / `Memory` row types, watch iterators, and the `RedexError` / `CortexError` / `NetDbError` discriminants. |
| `redex-disk` | Disk-backed RedEX persistence — the `persistent_dir` config knob (`Persistent: true` on `RedexFileConfig`). Without it the persistent path returns `RedexError`. |
| `netdb` | `NetDb` composition (requires `cortex`); the `net_netdb_*` FFI entry points ship with this feature. |
| `meshdb` | `net_meshdb_*` entry points plus the `libnet_meshdb` cdylib — `MeshQuery`, `MeshQueryRunner`, `Predicate`, `InMemoryChainReader`. |
| `meshos` | `net_meshos_*` entry points plus the `libnet_meshos` cdylib — `MeshOsDaemonSdk`, `MeshOsDaemonHandle`. |

Build with the full surface enabled:

```bash
cd net/crates/net
cargo build --release --features "cortex netdb redex-disk meshdb meshos"
```

Then point cgo at the resulting `target/release` directory:

```bash
export CGO_LDFLAGS="-L$(pwd)/target/release -lnet"
# On macOS, also:
# export CGO_LDFLAGS="$CGO_LDFLAGS -framework Security -framework CoreFoundation"
go build ./...
```

Slim the build by dropping features you don't need — `cargo build --release --features "cortex redex-disk"` is enough for an event-log-only consumer, for example. There is no build-time warning when a feature is omitted; the absence shows up as missing symbols at link time (static cdylib) or `dlsym` failure at first call (dynamic load).

## License

Apache-2.0
