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
| `hello.go` | `github.com/ai-2070/net/go` | `net` | `go run hello.go` — no `go.mod` ships here: run it in a module whose `go.mod` has `replace github.com/ai-2070/net/go => …`, build `libnet` first (`cargo build --release -p net-ffi`), and put its directory on the loader path (`LD_LIBRARY_PATH`, `DYLD_LIBRARY_PATH` on macOS, `PATH` on Windows) |
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

### Wave 1 — the services you no longer run

Four routes that rebuild a service a developer already operates, on the substrate, in one file. Rust and TypeScript are both implemented and executed; Python, Go and C are declared absent in the manifest with a reason rather than silently missing, and the ports are tracked.

Unlike `hello`/`observe`, these stand up **two to four real mesh nodes over loopback UDP** and exchange events between them — no memory transport, no mocks. They are the first examples in this directory that prove a round trip.

| File | Bindings | The service it replaces | Route | Expected line |
|---|---|---|---|---|
| `registry.rs` / `registry.ts` / `registry.py` / `registry.go` / `registry.c` | all five ✓ | Consul / etcd + a health poller | providers announce a capability · a caller discovers and ranks them locally · a new provider appears and wins the next lookup | `RESULT ok providers=3 joined=1 best_moved=1` |
| `jobqueue.rs` / `jobqueue.ts` / `jobqueue.py` / `jobqueue.go` / `jobqueue.c` | all five ✓ | Celery / SQS + Redis | append jobs to a local log · dispatch each over nRPC · a refused job is re-issued to the peer · reconcile from the log | `RESULT ok jobs=6 done=6 retried=1 duplicates=0` |
| `objectstore.rs` / `objectstore.ts` / `objectstore.py` / `objectstore.go` / `objectstore.c` | all five ✓ | S3 / MinIO | store bytes · mint a content address · fetch them from another node · store the same bytes again for the same address | `RESULT ok dedup=1 readback=1 bytes=64` |
| `liveconfig.rs` / `liveconfig.ts` / `liveconfig.py` / `liveconfig.go` / `liveconfig.c` | all five ✓ | LaunchDarkly / Consul KV | register a channel · subscribers join by name · the publisher pushes two revisions · each applies them locally | `RESULT ok subscribers=2 applied=2 version=2` |

Three of the four routes run in **all five bindings**, executed in CI; the manifest carries a per-binding status, so a port that exists but is not proven cannot read as one that is.

```bash
cargo run --example registry
cargo run --example jobqueue
cargo run --example objectstore
cargo run --example liveconfig
```

Worth knowing before you build on them:

- **A capability announcement is not re-delivered on re-announce.** In the current SDK mesh path, only a node's *first* announcement reaches its directly-connected peers: re-announcing with a changed tag set was measured to leave peers' folds unchanged (added tags never appear, removed tags never clear). `registry.rs` therefore demonstrates membership growing, not a provider retiring. Verify this against your own version before designing a withdrawal-based scheme on it.
- **Multi-hop propagation is deferred on the SDK `Mesh`.** Announcements reach directly-connected peers only, which is why the caller in `registry.rs` connects to every provider it wants to see rather than relying on a relay.
- **One real binding gap was found and closed, one reported gap was not real.** `live-config` could not be ported at first because the TypeScript SDK `MeshNode` wrapped `registerChannel` / `subscribeChannel` / `publish` but none of the napi receive verbs — a subscriber could join a roster and never read a payload. `MeshNode` now forwards `recv` / `recvShard` / `numShards` / `shardForStream`, and `liveconfig.ts` reads its revisions through them. The `job-queue` nRPC report did **not** reproduce: a producer calling two workers over `TypedMeshRpc` succeeds with the caller as responder or as initiator, with the service registered before or after the handshake, and with a reply-channel ACL pinned to the caller's EntityId. What does bite in TypeScript is lifecycle, not admission — every `node.rpc()` handle must be closed (`rpc.raw.close()`) before `shutdown()`, and two nodes built from the same `identitySeed` share a node id, so calls to "the second worker" silently land on whichever peer entry won.

### Binding gaps found while porting

- **`object-store` could not be written in C or Go — neither could mint a blob address, nor (Go) fetch one from a peer. Found; both halves fixed.** The ABI could `store`/`fetch` given an *encoded* ref and had no way to create one: `net_blob_publish` is declared in no shipped header and targets the external-hook adapter registry, not the substrate `MeshBlobAdapter` that `net_mesh_blob_adapter_*` uses. The C ABI gained `net_mesh_blob_adapter_publish` (BLAKE3 + store + encoded ref out) and `net_blob_ref_hash` (the 32-byte hash out of an encoded ref — the transport fetch addresses by hash, not by ref), declared in `net.go.h` and mirrored to `go/net.h`, with `NET_ERR_FEATURE_NOT_BUILT` stubs for builds without the `dataforts + netdb + redex-disk` triple; both feature configurations compile clean. The Go binding gained `MeshBlobAdapter.Publish`, `MeshNode.ServeBlobTransfer`, `MeshNode.FetchBlob`, `BlobRefHash`, a typed `ErrTransfer*` set, and the `net_transport.h` prototypes it was missing. `objectstore.c` and `objectstore.go` both run on them.
- **`net_rpc.h` did not parse on its own — found while writing `jobqueue.c`, fixed.** `RpcResponseSinkHandleC` was used inside the `net_rpc_streaming_handler_fn` typedef (~line 501) before its own forward typedef (~line 902), so a translation unit including only `net_rpc.h` failed with `unknown type name`. Reproduced against the pre-fix header, fixed by moving the two handle forward declarations above first use (no symbols added or removed; `check-rpc-abi-parity.py` and `check-header-count.py` still pass).
- **Python nRPC was unreachable from the wheel — found, fixed.** `jobqueue.py` could not be written at first: `net/crates/net/bindings/python/python/net/mesh_rpc.py` imports six streaming classes (`ClientStreamCall`, `DuplexCall`, `DuplexSink`, `DuplexStream`, `RequestStreamRecv`, `ResponseSinkSend`) in a single `from net._net import (...)`, and `bindings/python/src/lib.rs` registered only their `Async…` counterparts. The import failed, the module's `except ImportError` left `_RawMeshRpc = None`, and `TypedMeshRpc.from_mesh` raised `MeshRpc unavailable` under a wheel built *with* `cortex`. Registering the six classes fixes it, and the Python route then passes unchanged.
- **Python blob bindings were untested, not absent — found, fixed.** `objectstore.py` could not run at first: `dataforts` is in the binding's **default** feature set, so the published wheel carries `BlobRef` / `blob_publish` / `MeshBlobAdapter`, but CI's `maturin develop --no-default-features …` list omitted it — the tested artifact and the shipped one differed, and no blob path had Python coverage. `dataforts` is now in that list, and the route runs. Two stale sections of `net/crates/net/bindings/python/python/net/_net.pyi` were corrected alongside: `blob_publish` was declared two-arg (it takes `(adapter_id, uri, data)`) and `BlobRef` / `MeshBlobAdapter` were declared empty.
- **One typing stub was completed on the way.** `NetMesh.poll_shard` exists at runtime and is how `liveconfig.py` reads its revisions, but `net/crates/net/bindings/python/python/net/_net.pyi` did not declare it, so `mypy` rejected the example. The stub now declares it.

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

Both are driven from `docs/data/examples.yaml`, which requires every binding
to be listed for every route as either a checked file or an explicit, reasoned
absence — and, for execution, records which bindings run where. Its coverage
report prints **▶** for executed against **✓** for compiled-only, so a partially
executed route can never read as a fully executed one. A source file sitting in
this directory but missing from that manifest is an error; otherwise it would
ship to users with nothing compiling it.
