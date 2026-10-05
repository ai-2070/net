# Net v0.40 — "Highway Star"

*Deep Purple, 1972: nobody gonna take my car. v0.40 puts paid agent tasks in Node, the full blob surface in Go, and a C SDK that CI actually builds and runs. A write token now says which channel it came from.*

## What's in it

- **Paid A2A in Node.** The paid task lifecycle (prepare → purchase → submit) works end to end from Node and TypeScript, at parity with Python.
- **Go reaches the whole Dataforts blob surface**, and the uncompiled Go reference tree is gone.
- **The C SDK is verified.** CI builds the production library, stages it with its headers, and runs about 20 real C consumer programs against it. The audits behind that found and fixed real defects, including a leak that kept every started mesh node allocated after teardown.
- **Breaking: a write token carries its channel.** A token from one CortEX channel can no longer be waited on through another.
- **The browser matrix is no longer flaky.** The harness's timing problems are fixed, and so is one real race in the RTC responder.

---

## Write tokens carry their channel (breaking)

A `WriteToken` was `(origin, seq)`. Sequence numbers are per channel, so a Tasks token waited on through a Memories adapter with the same origin succeeded as soon as Memories passed that number, without the write ever having happened. The token is now `(origin, channel, seq)`, in every binding, with no compatibility path:

- **Core:** `WriteToken{origin_hash, channel_hash, seq}`, where the channel hash is `ChannelName::hash()`. Every wait and poll refuses another channel's token with `WaitForTokenError::WrongChannel`, before taking a permit.
- **The string form is now `<origin>:<channel>:<seq>`.** The old two-part form is refused, so tokens you logged or stored before 0.40 no longer parse.
- **C:** `net_{tasks,memories}_wait_for_token(handle, origin, channel, seq, timeout)` takes the channel. New `net_{tasks,memories}_channel_hash` accessors return it. A wrong-channel token is `NET_ERR_WRONG_CHANNEL` (**−160**).
- **Go:** `WriteToken{OriginHash, ChannelHash, Seq}`. `Token(seq)` stamps the channel, and `ChannelHash()` and `ErrWrongChannel` are new.
- **Node, TypeScript, Python:** `WriteToken` loses its public constructor (`new WriteToken(origin, seq)`, `WriteToken(origin, seq)`). Get tokens from the new `adapter.token(seq)`, or parse the three-part string with `fromString` / `from_string`. `channelHash` / `channel_hash` is new on the token and on both adapters.
- **Precedence:** origin is checked before channel, so a token wrong on both is `WrongOrigin`.
- **Python fix found on the way:** `net_sdk.cortex.tasks_cm` / `memories_cm` passed a `channel=` argument the native `open` never took, so both raised `TypeError` against a real wheel. The argument is gone; the channel is fixed per adapter.

---

## Paid A2A in Node

- **Provider:** `PaymentProvider.serveA2aConfigured` serves a catalog of free and paid A2A services, owning its journal exclusively for its lifetime, with an operator queue for admissions left unresolved.
- **Caller:** `CapabilityGateway` runs prepare (no funds move), a durable and resumable purchase, then submit. It shares one identity, spend store and signer set with the invoke gate.
- **Raw verbs on `NetMesh`:** `describeA2a`, `submitTaskPaid`, `setA2aOrgCaller`, and typed errors `PaymentRefusedError`, `JournalOwnedElsewhereError` and `A2aInvalidArgumentError`.
- **From the SDK alone:** `@net-mesh/sdk` and `net_sdk` gain factories that adapt a `MeshNode` to the native `PaymentProvider` / `CapabilityGateway`, so paid A2A needs neither `@net-mesh/core` nor `net`.
- **One contract for both bindings:** the paid-A2A JSON documents now live in `net_payments::flow::a2a::json`, and shape fixtures captured from Python pin both bindings to the same file.
- A provider serving under `principal: "same_org"` protects every A2A service it serves.

---

## Go: the full blob surface

- **New:** directory transfer (`StoreDir`, `FetchDir`, `DirManifestRead`), blob trees with Reed-Solomon erasure coding, range reads, repair, a process-wide blob adapter registry, read-your-writes tokens, and RedEX replication.
- **Greedy Dataforts and data gravity, now observable:** `Redex.GreedyCacheFor` reads through the greedy cache, so gravity heat can be measured from Go, Node and the TypeScript SDK.
- **Blob adapters written in Go** (`BlobAdapter`, `RegisterBlobAdapter`). The release callback fires exactly once, after unregister and the last in-flight call have both returned.
- **Every C buffer copy in `go/`** now goes through checked helpers. `C.GoBytes` / `C.GoStringN` wrapped lengths above 2³¹−1 negative through `C.int`.
- The Go suite runs under `-race` in CI.
- **Breaking:** `net/crates/net/bindings/go/net/`, a reference package with no `go.mod` that never compiled, is deleted. Its resilience helpers, capability builders, Deck streams, MeshDB operators, placement filters and MeshOS vtable path are not available from Go until their own plans port them. The 56 `NET_*` constants it defined are all still defined, with the same values, where consumers read them.

---

## The C SDK, verified

Until now the C examples were never built by CI, and some no longer compiled. Now:

- **CI builds the production library** (`net-ffi`, default features), stages it as a header + library bundle, and runs about 20 C consumer programs against it: lifecycle, transfer, trees, callbacks, capabilities, MCP, MeshDB, compute, Deck, streams, islands, aggregator, MeshOS, org and subnet. Each program proves which library it loaded and is held to a floor of named checks.
- **Audits hold the headers, the exports and the compiled Rust to one model:** signatures, constant values, struct layout and compatibility with the last release. Sanitizer, debug-CRT and Application Verifier lanes run every program.
- **Defects the audits found, fixed:**
  - Every `net_mesh_*` entry point leaked one reference to the node, so a started node stayed allocated after shutdown and free. A dropped node's components (failure detector, reroute policy, session routing, roster and more) are now reclaimed too.
  - MeshDB envelope decoding accepted a valid prefix, so a window starting at sequence 0 decoded as an aggregate with a nonsense value. Decoding now requires the whole payload. A new `net_meshdb_decode_payload_json_as(kind, …)` decodes by the operator you name, and the untagged decoder returns NULL when it cannot tell the kinds apart.
- **Headers now declare return codes the library already returned:** the blob band, `NET_ERR_GANG_INVALID`, the registry codes 8–11 and the CortEX codes. If your code defined those names itself, it now collides with the headers.
- **Matched pair:** a C program must be rebuilt against the 0.40 headers and library together. The bundle is not binary-compatible with an older one; the token-wait signatures above are one reason.

---

## Smaller changes

- **The browser matrix CI job is stable.** Follower tabs wait for their node id instead of failing with `got None`, Firefox's reconnect backoff is off in the test driver, and a late retransmission is handled.
  - One real fix, in the RTC responder: its handshake inbox is now registered before the answer leaves. A browser sends its first handshake message once, so a late registration dropped it and the session timed out.
- **The README** describes the zero-copy ring-buffer transport and sharded ingestion. It also lists the headline microbenchmarks near the top, with the caveat that they exclude NIC and wire time.
- Dependency updates, including tokio, str0m, libc and `igd-next` 0.18.

---

## Version bump

Everything published moves to **0.40.0**:

- every manifest: crate, wire, leaf, CLI, deck, SDK, payments, and the Go, Node and Python bindings;
- the `@net-mesh/*` pins and the `net-mesh` Python bound (now `>=0.40.0,<0.41.0`);
- the skills' `net-version`;
- the Hermes integration pin;
- the lockfiles.

---

## Breaking changes

- **Write tokens** carry their channel. Old token strings no longer parse; the Node, TypeScript and Python constructors are gone; and the C waits take a channel argument. See the first section.
- **Go:** the uncompiled `bindings/go/net/` reference tree is deleted.
- **C:** headers now declare codes that consumers may have defined themselves, and C programs must be rebuilt against the 0.40 headers and library together.

---

## How to upgrade

Bump to 0.40.0 and rebuild. Then:

- Replace any `WriteToken(origin, seq)` with `adapter.token(seq)`, and throw away token strings stored by an older version.
- In C, pass the channel hash (`net_tasks_channel_hash` / `net_memories_channel_hash`) to the token waits, and rebuild against the 0.40 bundle.
- In Go, use the blob surface from the `go/` module.

---

Released 2026-10-05.

## License

See [LICENSE](../../LICENSE-APACHE).
