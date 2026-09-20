# Stage 0 slice 0.5 — browser and SDK implementation mapping

Read-only source trace at head `85ecc77c9` for the two runtimes the owner
ruled into the release matrix: Q5 (`@net-mesh/browser` / `net-mesh-leaf`) and
Q6 (the pure SDKs `@net-mesh/sdk` and `net-mesh-sdk`). No files were changed
and no build was run to produce it; every claim is `[observed]` with a
`path:line` or `[inferred]` with its reasoning. This is a mapping, not an
implementation, and it carries no runtime acceptance claim.

## Portable core

All five org modules live inside the tokio-linked core crate
(`net/crates/net/src/adapter/net/behavior/`).

| Module | wasm32-unknown-unknown blockers |
|---|---|
| `org_call.rs` | None intrinsic (`ed25519_dalek`, `serde`, `blake3::hash`/`derive_key` at `:82,151`). Blocked only transitively through `current_timestamp` and the `identity` re-export chain. |
| `org_grant.rs` | `getrandom::fill` (`:313,863,1206`) against **getrandom 0.4** (`Cargo.toml:315`), with no wasm backend opt-in; the leaf and wire pin 0.3. |
| `org.rs` | `current_timestamp()` is `SystemTime::now()` (`:962-966`) — on wasm32 this **compiles and then panics**, documented in-repo at `wire/src/time.rs:9-12` and `leaf/src/clock.rs:3-8`, so `cargo check --target wasm32` cannot see it. Plus getrandom. |
| `org_admission.rs` | Inherits the above plus `org_revocation` and `Instant` (`admission_clock.rs:19,62,68`, `org_admission_replay.rs:42`). |
| `org_revocation.rs` | **Not portable.** `std::fs` throughout (`:1050,1467,2308,2458,2571,2645,2752`), `parking_lot::Condvar`, and `#[cfg(windows)] windows_file_identity` (`:1095`). |

Two transitive facts: importing `MAX_TOKEN_CLOCK_SKEW_SECS` (`identity/token.rs:1072`)
naively drags the whole token subsystem (`token.rs:8-12`: DashMap, atomics,
`SystemTime`); but `identity/entity.rs` — the `EntityId`/`EntityKeypair` the
codecs actually need — depends only on `blake2`, `ed25519_dalek` and
`getrandom` (`:7-11,216`).

A tokio-free boundary already exists: `net-mesh-wire` (lib `net_wire`),
described in its own manifest as "tokio-free and wasm32-clean", with 18
modules including `clock`, `crypto`, `session`, `stream_window`, `test_vectors`
(`wire/src/lib.rs:49-67`). It carries **no `blake3` and no `ed25519-dalek`` —
both would be new deps there.

`[inferred]` Extraction target: a tokio-free module set holding `entity.rs`'s
key types, the skew constant, `org.rs`'s cert/keypair types, `org_grant.rs`,
`org_call.rs` and the pure verification steps of `org_admission.rs`, with every
time read routed through `net_wire::clock::{Clock, SystemClock}` (the pattern
`wire/src/time.rs:15` and `leaf/src/clock.rs:16-40` already use) and randomness
taken as caller-supplied bytes. `org_revocation.rs` stays native; the leaf is
**fed** revocation facts over its existing control-plane/announce seam.

## Leaf gaps (caller)

Present today: sessions with `handshake_hash()` (`leaf/src/session.rs:409-411`)
and `incarnation()` (`:423-425`) — the one piece the opening-proof binding
needs and the leaf already has; streams; `stream_ownership.rs`'s
`ResolvedIdentity{wire_id, peer, incarnation}` (`:27-34`); a `CallTable` with
`CallOwner{peer, incarnation}` and `fail_incarnation`/`fail_peer` (`rpc.rs:82-95,286`);
capability announce/query.

**There is no runtime.** `futures-channel` is the only async primitive,
chosen deliberately — "the leaf is single-threaded and has no runtime"
(`leaf/Cargo.toml`) — with `Rc<RefCell<Inner>>` state (`wasm.rs:1441-1443`).

Only unary exists: `RpcRequestPayload::unary` is the sole request builder
(`node.rs:1793`) and `rpc_wire.rs:57-70` declares four dispatch bytes
(`REQUEST/RESPONSE/CANCEL/DEADLINE_EXCEEDED`); its own table says streaming
chunks, grants and the four folds are explicitly not there (`:25-33`).

| Missing | File | Depends on | Size |
|---|---|---|---|
| Streaming dispatch bytes + chunk/grant/END codec | `leaf/src/rpc_wire.rs` | the core's `cortex/rpc.rs` codec head, which that file says is not a contiguous extractable region (`rpc_wire.rs:11-18`) | medium — a second copy pinned by fixtures |
| Per-shape caller state machines (SS/CS/DX) | new `leaf/src/rpc_stream.rs` | the codec; `net_wire::stream_window` | large — three shapes with no runtime to lean on |
| `CallTable` from one-shot to multi-item sink/stream | `leaf/src/rpc.rs` | `futures-channel` mpsc; fencing shape already correct | medium |
| Caller org proof minting + admission header | new `leaf/src/org.rs` | the portable extraction; the leaf has ed25519 and blake2 but **not blake3** | medium (large if the extraction does not land — a second crypto implementation is what §4.5 forbids) |
| Session binding into the transcript | `leaf/src/session.rs` + `org.rs` | already present | small |

## Leaf gaps (provider)

Structural, not stylistic: `rpc_wire.rs:29` states "a leaf serves nothing";
`dispatch.rs:188-208` has no inbound-RPC arm; the only `DISPATCH_RPC_RESPONSE`
construction in `node.rs` is `#[cfg(test)]` scaffolding (`:3238,4041`); and
`node.rs:4454-4458` pins a caller-only call-id seeding assumption.

| Missing | File | Depends on | Size |
|---|---|---|---|
| Inbound `RpcRequestPayload::decode` | `leaf/src/rpc_wire.rs` | mirror of the existing encode; `tests/cross_lang_wire/nrpc_frame.json` already pins the other direction | small |
| Service registry + inbound dispatcher keyed `(peer, incarnation, call_id)` | new `leaf/src/rpc_serve.rs` | `CallOwner`'s key shape; `stream_ownership.rs`'s "a wire id is not an open" rule | large — this is the whole missing half |
| Response/terminal emission, cancel, `DEADLINE_EXCEEDED` as a server | `leaf/src/rpc_serve.rs` | `leaf::clock::Deadline` exists (`clock.rs:49-72`) | medium |
| Provider-side org admission (mode, verification, typed denial) | new `leaf/src/org.rs` | the portable extraction; a revocation **input** (no `std::fs`) | large |
| Per-call supervisor with no runtime | `leaf/src/rpc_serve.rs` | `fail_incarnation` as the fencing primitive; everything else must be pull/callback-driven | large |
| JS handler trampoline (register a JS provider; sink/stream objects) | `leaf/src/wasm.rs` + `browser-ts/src/` | the `js_sys::Function` listener pattern (`wasm.rs:2950,3571`) | medium |

## Browser package

`net-mesh-leaf` is its own workspace, built for `wasm32-unknown-unknown` with
`crate-type = ["cdylib","rlib"]`, exported via wasm-bindgen 0.2.128 pinned
exactly to the CLI (`leaf/Cargo.toml`, `ci.yml:6175-6178`).

Exported to JS today (`wasm.rs`): `LeafNode` with `connect`, `call` (**unary
only**, `:1885`), pub/sub, `open_stream`, `announce`, `query`, signalling,
listeners, `enroll`, `close`, `retire`; plus `LeafStream` (`:3455`). **No serve
verb, no streaming verb, no org verb.**

A shared-session proxy already exists: `leaf/src/leader.rs` (`ProxyClient`,
`ProxyServer`, `ProxyEnvelope`, `ProxyBody`, `Admission`, `GenerationGate`,
`lib.rs:115-119`) and `leader_session.rs` (`MeshSession`, Web Lock +
`BroadcastChannel`, `lib.rs:57-61,121-122`), surfaced as `openSession` in
`browser-ts/src/leader/`.

`[inferred]` Org/streaming verbs therefore land on **three** surfaces, not two:
`LeafNode` (direct), `MeshSession` (leader), and the `ProxyBody`/`ProxyValue`
vocabulary a follower tab rides. CI builds both the wasm (`leaf-wasm`,
`ci.yml:5726`) and the package (`browser-package`, `:6147`, with the R11
ABI-through-the-real-package roster at `:6199-6247`); there is no release
workflow for either, which is inventory, not the support criterion (Q5).

## Pure SDK forwarding

TypeScript: `sdk-ts/src/mesh.ts:520-522` recovers the napi handle
(`getNapiMesh(this)`) and hands it to `@net-mesh/core`'s `TypedMeshRpc.fromMesh`.
The SDK already imports org **types** from the core (`mesh.ts:49-52,67,74`) and
already ships one org-engine verb, `serveSubnetExported` (`:1115+`). So both the
forwarding pattern and the org-type import path are established; only the org
client/serve verbs are absent.

`[observed]` `sdk-ts/package.json:7-16` lists only `"."` and `"./tool"` in
`exports`; an `"./org"` entry is a shipped-manifest edit that a "thin
re-export" framing easily misses.

Python: `sdk-py/src/net_sdk/mesh.py:139-173` has `serve_subnet_exported`
forwarding `self._native` to `net.subnet`. There is no `rpc()` equivalent and
no org surface. The async org forms must exist in `bindings/python` first —
`OrgClient.call` is sync-only today.

Consumer harnesses: `.github/scripts/check-ts-consumer.sh` (`ci.yml:3614-3615`)
is **type-check only** (`noEmit`, "Nothing here runs"). There is **no Python
equivalent**: `sdk-py-tests` stubs the native extension in `conftest.py`
(`ci.yml:3901-3909`), and the only executed Python-over-real-binding leg is
`run-skill-examples.sh --lang python` (`ci.yml:3804-3812`). Q6's "Python
consumer/runtime run" is therefore not satisfiable by mirroring the TS script.

## Fixtures

- Org error vocabulary: `tests/cross_lang_org/error_vectors.json`, generated by
  `sdk/examples/gen_org_error_fixtures.rs` from `render_error_vectors()`
  (`sdk/src/org/fixtures.rs:136`), guarded by `sdk/src/org/tests_fixture.rs:15-36`.
  Consumed by Rust, Node, Python and Go. **No leaf/browser consumer.**
- nRPC streaming golden vectors: `tests/cross_lang_nrpc/golden_vectors_streaming.json`,
  consumed **only** by `tests/integration_nrpc_cross_lang_streaming.rs`, whose
  header states it spawns no other runtime (`:4-8`).
- Wire vectors the leaf *does* consume: `tests/cross_lang_wire/*.json` mirrored
  into `leaf/src/test_vectors/` behind the `test-vectors` feature and replayed
  inside wasm (`leaf/tests/fixture_parity.rs`). `[inferred]` That is the only
  existing shape in this repo where a fixture is proven consumed by the shipped
  browser artifact, so a streaming-opening vector set plugs in the same way plus
  a JS consumer on the `abi_real_package.mjs` roster pattern.

## Obstacles and smallest resolutions

1. **getrandom major split** (core 0.4 vs leaf/wire 0.3). Resolution: the
   extracted codec takes randomness as caller-supplied bytes, so the portable
   half links no `getrandom` and each runtime keeps its major. This also removes
   `std::process::abort()` (`org.rs:213`, `org_grant.rs:317`) from wasm, where
   aborting a tab is not proportionate.
2. **`SystemTime`/`Instant` compile-then-panic on wasm32**, invisible to
   `cargo check --target wasm32` (`leaf/src/clock.rs:7-8`). Resolution: route
   time through `net_wire::clock` and take `now` as a parameter —
   `AdmissionReplayGuard::admit` already does (`org_admission_replay.rs:725`).
3. **`org_revocation.rs` cannot be extracted** (filesystem + Condvar), yet §4.5
   requires the browser to prove revocation. Resolution: split the query surface
   (floors, epoch, is-revoked) from the store; feed the leaf facts over
   `control_plane.rs`/`announce.rs`, keep the on-disk store native.
4. **The nRPC codec is deliberately duplicated and the duplicate is client-only**
   (`rpc_wire.rs:6-18`). Resolution: do the extraction that file already names —
   move the codec head plus `EventMeta` into `net-mesh-wire` so core and leaf
   link one copy, pinned by the existing `nrpc_frame.json` mechanism.
5. **The leaf has no executor, and per-call supervision is the one thing
   streaming needs one for.** Resolution, and **already applied in slice 0.3**:
   the lifecycle decision logic lives in a runtime-free `CallLifecycle`, and the
   model ships two drivers — the tokio `run_supervisor` and a pull-step
   `pull::PullCall::advance(now)` with no tasks, no semaphores and no timer.
   The runtime-free driver reproduces the blocked-drain, grant-completes,
   deadline-preempts, retirement-preempts, retained-sink and byte-release
   obligations. Honouring this now was cheap; retrofitting it would not have been.
6. **Three JS surfaces, not two** (`LeafNode`, `MeshSession`, `ProxyBody`).
   Resolution: define the proxy body as a transparent envelope over the same
   frame bytes so a new shape costs zero proxy vocabulary; attribution rides
   `ResolvedIdentity{wire_id, peer, incarnation}`.
7. **Q6's Python consumer evidence does not exist and cannot be faked by the
   current jobs.** Resolution: an executed `net_sdk`-only example under the
   skill-examples runner (which already installs the wrapper, `ci.yml:3802`),
   not a mirror of the type-check-only TS script.
8. **`sdk-ts` cannot expose an org entry point without a `package.json`
   `exports` change.** Trivial but easy to miss.

## Not covered

- No commands were run; nothing about compilation, wasm size or test outcomes
  is asserted as executed.
- `bindings/node/src/mesh_rpc.rs` and `bindings/python/src/mesh_rpc.rs` (the
  binding streaming surfaces Q6 depends on) were not read — slice 0.5 was
  scoped to the pure SDKs.
- `leaf/src/leader.rs` / `leader_session.rs` bodies were not opened; the proxy
  claims rest on the `lib.rs` re-exports, `index.ts` and the
  `stream_ownership.rs` header. The exact `ProxyBody` variant list is unverified.
- Whether `blake3` builds for `wasm32-unknown-unknown` in this graph was not
  resolved; treat "blake3 is portable" as `[inferred]`.
- Two plan claims were specifically attacked and both held: "a leaf serves
  nothing" (`rpc_wire.rs:29`) and "only Rust consumes
  `golden_vectors_streaming.json`" (`integration_nrpc_cross_lang_streaming.rs:4-8`).
