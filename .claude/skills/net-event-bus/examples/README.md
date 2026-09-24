# Sanity-check examples

Each file in this directory is a **minimal, runnable** example. Use these as the first thing a developer runs after `npm install` / `pip install` / `cargo add` — before they write any application code.

The two install-check routes (`hello.*`, `observe.*`) use the **memory
transport** — no network, no peers needed — and run in a single process. The
other routes here do not: `a2a_paid.*` stands up live mesh nodes, and so does
`net_org_streaming.c`. See below.

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

Four routes. The first two are in all five bindings; `a2a_paid.*` is in the two
that have the paid surface; `net_org_streaming.c` is C-only, because C is the
one binding without its own org test suite.

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

**`a2a_paid.*` — prepare · unpaid submit refused · purchase · submit.** Not an
install check, and the one example here that is not memory-transport: it
stands up two live mesh nodes over loopback UDP, a provider with one
`PaymentEngine` behind both the quote/pay wire and the admission gate plus a
durable admission journal on disk, and a caller with a spend policy and a
durable purchase store. It asserts the two properties the paid path exists
for, read out of production artifacts rather than inferred from a state label:
the executor's own run counter is exactly 1, and the provider's billing log
holds exactly one charge.

It is **mock-settled**. The lifecycle is real; no value moves. A runnable
example cannot settle on a real rail without funded keys and a testnet, so
this one says so instead of implying otherwise.

| Binding | Status |
|---|---|
| `a2a_paid.rs` | the full surface: `serve_a2a_configured` + `A2aCallerFlow` |
| `a2a_paid.py` | the same flow through `PaymentProvider` / `CapabilityGateway` |
| TypeScript, Go, C | no paid A2A surface exists to exercise — Go and C have no A2A at all, and Node has none in either direction: paid serving is an explicit non-goal, and no *paid* caller verbs are bound — `submitTask` is exported, but it is the free, uncharged verb, and there is no `describeA2a`, prepare/purchase pair or `submitTaskPaid`. `docs/data/examples.yaml` records the reason per binding. |

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

- **A re-announcement inside the broadcast window is coalesced, not lost.** `min_announce_interval` (10 s by default) rate-limits the broadcast: an announce inside the window returns `Ok(())` and ships nothing *at that moment*, then one trailing-edge flush at the end of the window carries the newest capability set. Two earlier readings of this were wrong — first that only a node's first announcement ever reaches peers, then that an in-window announce is discarded outright. The first generalised from the window; the second was true only of the old bare-`start()` path, which could not schedule the flush and dropped it. `start` now takes the node's `Arc`, so every started node can flush and the drop arm is gone. What remains is latency, not loss: a change made inside the window reaches peers by the end of it, and `with_min_announce_interval` tightens that if an example needs to observe it sooner. Measured at `20e69e388` with a two-node probe: the first announce reaches a peer in ~29 ms, and an add or a removal issued 300 ms later lands at ~9.7 s — one window, exactly as described above.
- **Multi-hop propagation is deferred on the SDK `Mesh`.** Announcements reach directly-connected peers only, which is why the caller in `registry.rs` connects to every provider it wants to see rather than relying on a relay.
- **One real binding gap was found and closed, one reported gap was not real.** `live-config` could not be ported at first because the TypeScript SDK `MeshNode` wrapped `registerChannel` / `subscribeChannel` / `publish` but none of the napi receive verbs — a subscriber could join a roster and never read a payload. `MeshNode` now forwards `recv` / `recvShard` / `numShards` / `shardForStream`, and `liveconfig.ts` reads its revisions through them. The `job-queue` nRPC report did **not** reproduce: a producer calling two workers over `TypedMeshRpc` succeeds with the caller as responder or as initiator, with the service registered before or after the handshake, and with a reply-channel ACL pinned to the caller's EntityId. What does bite in TypeScript is lifecycle, not admission — every `node.rpc()` handle must be closed (`rpc.raw.close()`) before `shutdown()`, and two nodes built from the same `identitySeed` share a node id, so calls to "the second worker" silently land on whichever peer entry won.

### Wave 2 — logs and credentials

Two more routes, both portable to **all five bindings** with no absent binding and no caveat.

| File | Bindings | The service it replaces | Route | Expected line |
|---|---|---|---|---|
| `eventlog.*` | all five ✓ | Kafka + ZooKeeper/raft | append records to a local log · replay them all · a consumer checkpoint · replay from the offset | `RESULT ok records=8 replayed=8 resumed=3` |
| `tokenchannel.*` | all five ✓ | an ACL file plus a sidecar auth service | a channel gated on token roots · a subscribe-only token bound to one entity · refused bare, admitted with it | `RESULT ok granted=1 refused=1` |

Both are worth reading before building on them, for opposite reasons:

- **A token-gated subscribe used to require an announcement first; that prerequisite is gone.** A token's leaf binds to the subscribing peer's `EntityId`, and the only paths that installed that binding were a signature-verified capability announcement or verified subnet admission — so a consumer with no services to publish had to advertise capabilities and poll a discovery index purely to use a credential issued to it. Its absence surfaced as `Unauthorized`, which points at the credential rather than at the missing binding, and every binding's example encoded an announce-and-poll warm-up to work around it. Identity establishment is now separate from capability discovery: `SUBPROTOCOL_IDENTITY_PROOF` (`0x0A01`) is a verifier-nonce challenge/response over the existing encrypted session, and the binding is established as part of the token-bearing subscribe itself. The examples no longer announce or poll; the comment where that warm-up used to be says why, and `net/crates/net/docs/IDENTITY.md` states the gate.
- **Retention is Rust-only, so the log route proves replay and offsets instead.** `sweep_retention()` is not exposed in the TypeScript, Python, Go or C bindings, so `event-log` demonstrates the part every binding can do — append, full replay, a consumer-owned checkpoint, resume from it, and a stable second replay — rather than a retention sweep that only one language can show. The offset living in the consumer is the route's point anyway.

### Wave 3 — one route, and two routes that earned a `no`

One route, portable to **all five bindings**, plus two candidates we measured and dropped. The drops are recorded here because a route that looks obviously good and is not is the more useful finding.

| File | Bindings | The service it replaces | Route | Expected line |
|---|---|---|---|---|
| `failover.*` | all five ✓ | a service registry plus a client-side retry shim | two providers behind one service name · the caller addresses the **service**, never a node id · the answering provider dies mid-flight · the next call lands on the survivor | `RESULT ok providers=2 moved=1 served=2` |

The route's point is what nRPC buys that a `host:port` does not: the caller never learned the provider's identity, so the provider's death is a retry, not a redial. It is `sidecar-mesh` — the same one-line `call_service` in every binding.

- **The retry helper is Rust-only, so four ports write the loop by hand.** `call_service_typed_with_retry` (attempts, backoff, a retry predicate) exists in the Rust SDK and nowhere else. `failover.ts`, `failover.py`, `failover.go` and `failover.c` each carry a bounded loop of six attempts at 250 ms under a 500 ms call deadline, and each says so in a comment. The loop is not decoration: the roster still lists the dead provider until the capability fold converges, so the first attempt after the kill may be spent on a corpse. If your binding has no retry helper, that loop — not just the call — is the thing you are missing.
- **The dropped gang-claim route — a measured negative, and worth more than the route.** `claim_island` / `reserve_island` / `match_islands` / `find_islands` are typed in all five bindings, so "two nodes contend for one mutually-exclusive resource" looked like a natural third route. It does not hold on the flat SDK: **no exclusion is observable.** Measured directly on one mesh — two nodes each `claim_island` the *same* island and **both** get `Some`; `reserve_island` on an island a peer already holds also returns `Some`; and a node holding an island still appears in its own `match_islands` candidate list. The exclusion lives one layer down, in the quorum promotion to the `Active` state, which is core-only and unreachable from the bindings. So a `lock` example would have taught a mutex that is not one — a reader would ship mutual exclusion they never had. Recorded, not shipped.
- **The dropped subprotocol route.** Custom-subprotocol registration has no typed exposure in any of the five SDKs — the registry is not a binding-level surface — so an example there would be Rust-and-core only, which this grid does not run. Recorded, not shipped.

### Wave 4 — protected org streaming

One route, and it is **C-only**. `net_org_streaming.c` serves *and* calls a
protected streaming service from one file, linked against the single `libnet`:
it boots a throwaway cross-org issuance chain with the in-repo scenario
generator (credentials are issued material — the example mints none of its
own), serves a `Granted` streaming service, calls it cross-org, and asserts
that every item the handler emitted arrived (handler-sent and caller-received
counts agree), explicit completion, and handler-side attribution of the
**verified** caller.

| File | Bindings | Route | Expected line |
|---|---|---|---|
| `net_org_streaming.c` | C only — see below | mint a throwaway org chain · serve a `Granted` streaming service · call it cross-org · verified caller attribution at the handler · explicit completion | `RESULT ok chunks=3 attribution=verified` |

The other four bindings are absent **with a reason, not unwritten** — the
manifest's words, verbatim in substance:

- **Rust** — its org streaming cells live in the SDK's own live suite
  (`sdk/tests/org_streaming.rs`, same-org and granted, all four shapes); a skill
  example would duplicate a suite that already owns the fixtures.
- **TypeScript** — the Node binding exercises its org verbs live in
  `bindings/node/test/org_live.test.ts`.
- **Python** — likewise, in `bindings/python/tests/test_org_live.py`.
- **Go** — `go/org_test.go` covers the same generated scenario, both authority
  modes, all four shapes. C is the surface without a binding test suite, so this
  route *is* C's consumer-side evidence — including that every `net_org_*` /
  `net_rpc_*` / `net_mesh_*` symbol resolves out of the one `libnet`.

It runs at `level: run` with a 900 s timeout: the first run compiles the
scenario generator against the fixtures feature; the live call itself is
seconds. Without a Rust toolchain the example fails loudly rather than printing
an unearned `ok`.

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
