# SDK Channel Consistency Audit

**Date:** 2026-08-08  
**Repository:** `ai-2070/net`  
**Audited commit:** `51641125d0a57514fd2fde24f327bb4b32b60269`  
**Branch:** `sdk-bugs`  
**Scope:** Rust core and SDK, Node/NAPI and TypeScript SDK, PyO3 and Python SDK, Go, C, the `net-event-bus` skill corpus, package READMEs, and relevant public SDK documentation.

## Executive summary

The reported Python channel-name validation bypass is valid, and the same bypass exists in the high-level TypeScript SDK. The immediate defect is broader than two missing constructor checks: the documentation presents two independently implemented concepts as one channel API.

1. The TypeScript and Python `NetNode.channel()` wrappers tag generic EventBus JSON with `_channel` and apply a consumer-side filter.
2. Distributed mesh channels use canonical `ChannelName`, publisher registration, membership acknowledgements, rosters, authorization, and per-peer publish reports.

The tagged wrappers do not invoke the distributed channel protocol. As a result, claims about validation, rosters, capability authorization, prefix subscription, implicit creation, replay, no-subscriber behavior, return values, and transport transparency are applied to surfaces where they do not hold. At the same time, the documentation incorrectly says distributed named channels are absent from Rust, Go, and C even though all five language surfaces expose them.

The audit also found binding-level gaps in delegated credentials, prefix and queue-group policy, token-root configuration, batch fan-out, error taxonomy, payload shape, token wire-size documentation, lifecycle guidance, and package versioning.

**Recommended disposition:** treat this as a structural SDK-contract and documentation repair, not as an isolated Python validation patch.

---

## 1. High — High-level Python and TypeScript bypass canonical channel-name validation

Rust's canonical constructor validates channel names at:

- `net/crates/net/src/adapter/net/channel/name.rs:42-65,108-156`

It rejects:

- empty names;
- names longer than 255 bytes;
- leading or trailing `/`;
- `//`;
- uppercase ASCII;
- characters outside lowercase ASCII letters, digits, `.`, `_`, `-`, and `/`;
- whole `.` and `..` path segments.

The high-level wrappers do not call it:

- TypeScript: `net/crates/net/sdk-ts/src/channel.ts:37-41,54-59`
- Python: `net/crates/net/sdk-py/src/net_sdk/channel.py:39-53,60-64`

They retain the unchecked string, insert it as `_channel` in generic JSON, and call EventBus ingestion. Recording-backend reproductions accepted empty, uppercase, spaced, traversal-like, leading-slash, trailing-slash, double-slash, and overlength names.

This is not accurately described as passing a channel name into a channel FFI. The wrappers send generic event JSON through `ingestFire` / `ingest_raw`; `ChannelName::new` is absent from the path.

The lower distributed channel entrypoints do validate names. Examples include:

- Python `NetMesh.publish` / `AsyncNetMesh.publish`: `net/crates/net/bindings/python/src/lib.rs:2157-2215,3405-3451`
- C subscribe and publish: `net/crates/net/src/ffi/mesh.rs:2134-2140,2174-2180,2258-2264`

### Required closure

- Reuse or expose one canonical validator in both high-level wrappers.
- Validate inside `TypedChannel` constructors so direct construction is covered.
- Add inverse tests for every Rust boundary.
- Decide whether these wrappers are meant to represent distributed channels or only tagged EventBus topics.

---

## 2. High — Documentation conflates tagged EventBus topics with distributed mesh channels

The `net-event-bus` skill attributes distributed behavior to `node.channel()`:

- `.claude/skills/net-event-bus/apis.md:42-47`
- `.claude/skills/net-event-bus/gotchas.md:15-19`

The package READMEs similarly direct readers from the high-level wrappers toward hierarchical pub/sub with capability authorization:

- `net/crates/net/sdk-ts/README.md:199-208`
- `net/crates/net/sdk-py/README.md:176-185`

But `NetNode.channel()` only embeds `_channel` and installs an exact payload filter:

- TypeScript: `net/crates/net/sdk-ts/src/channel.ts:37-59,82-103`
- Python: `net/crates/net/sdk-py/src/net_sdk/channel.py:39-64,75-116`

It does not:

- construct canonical `ChannelName`;
- register publisher policy;
- send membership messages;
- receive membership acknowledgements;
- maintain a subscriber roster;
- apply capability or token authorization;
- use mesh publish options;
- return a per-peer `PublishReport`.

Distributed mesh channels are separately exposed across all five language surfaces:

- Rust: `net/crates/net/sdk/src/mesh.rs:709-896`
- TypeScript: `net/crates/net/sdk-ts/src/mesh.ts:583-687`
- Python low-level binding: `net/crates/net/bindings/python/src/lib.rs:2101-2215`
- Go: `go/mesh.go:981-1102`
- C: `net/crates/net/include/net.go.h:449-491`

The skill nevertheless says Rust has no named channels and directs Rust, Go, and C users to firehose or polling alternatives:

- `.claude/skills/net-event-bus/concepts.md:57-60`
- `.claude/skills/net-event-bus/bindings/rust.md:147-167`
- `.claude/skills/net-event-bus/bindings/go.md:51-55`
- `.claude/skills/net-event-bus/bindings/c.md:59-62`

The generated coverage matrix contradicts that prose by marking distributed channels supported everywhere:

- `.claude/skills/net-event-bus/bindings/coverage.md:81-86,113-118`

Python should additionally be marked `core-only` for distributed channels: the ergonomic `net_sdk.MeshNode` does not expose channel registration, membership, or publish methods, while the lower `net.NetMesh` binding does.

### Required closure

Split all documentation into two explicitly named surfaces:

1. **Tagged EventBus topics** — TypeScript/Python convenience filtering.
2. **Authenticated distributed mesh channels** — canonical mesh membership, policy, and fan-out.

Do not let examples, behavior claims, or coverage anchors for one surface stand as evidence for the other.

---

## 3. High — Prefix subscription is documented but unimplemented

`.claude/skills/net-event-bus/concepts.md:48` claims that subscribing to `sensors/lidar` receives events from descendant names such as `sensors/lidar/front`.

No production subscription path implements that behavior:

- TypeScript and Python tagged topics use exact `_channel == name` filters.
- Distributed rosters are keyed by exact `ChannelId`: `net/crates/net/src/adapter/net/channel/roster.rs:322-335,462-482`.
- `ChannelName::is_prefix_of` exists at `channel/name.rs:100-106` but has no production call site.

Rust's `Mesh::register_channel_prefix` is real:

- `net/crates/net/sdk/src/mesh.rs:762-789`

However, it performs publisher-side ACL/config resolution for dynamically named channels. It is not wildcard membership and does not make a subscriber receive descendant channels.

### Required closure

Remove the prefix-subscription claim unless wildcard membership and delivery are intentionally implemented. Document prefix ACL resolution separately.

---

## 4. High — Implicit creation, replay-on-join, and roster behavior are not universal contracts

Claims in `.claude/skills/net-event-bus/concepts.md:31-37,66` combine properties that do not hold consistently on either channel surface.

Distributed channels normally require publisher registration:

- `net/crates/net/sdk/src/mesh.rs:711-723`

A subscriber sends a membership request and waits for an ACK:

- `net/crates/net/src/adapter/net/mesh.rs:21788-21805`

Unknown or unauthorized channels may be rejected. A successful subscribe does not replay events from the publisher's local ring. Publish fans out only to the current roster:

- `net/crates/net/sdk/src/mesh.rs:873-896`
- `net/crates/net/src/adapter/net/channel/publisher.rs:1-18`

Conversely, tagged TypeScript/Python topics are implicit labels over EventBus events but have no publisher-side roster at all.

### Required closure

Document creation, membership, history, and delivery independently for each surface. Do not describe joining a roster as gaining replay or describe tagged ingestion as roster fan-out.

---

## 5. High — Default memory transport cannot support documented publish/subscribe examples

The default memory configuration selects `NoopAdapter`, which counts and discards batches and always polls empty:

- `net/crates/net/src/adapter/noop.rs:62-87`

Nevertheless, default-memory nodes are used in publish/subscribe examples that attempt to consume emitted events:

- `net/crates/net/sdk-ts/README.md:102-130`
- `net/crates/net/sdk-py/README.md:96-124`
- `web/src/content/docs/guides/event-bus.md:247-274`
- `.claude/skills/net-event-bus/bindings/typescript.md:28-39`
- `.claude/skills/net-event-bus/bindings/python.md:35-42`

The examples wait forever. Internal guidance is self-contradictory: `.claude/skills/net-event-bus/apis.md:80-87` correctly warns that memory does not deliver, while `SKILL.md:63,78,92` and `concepts.md:101-114` claim the same publish/subscribe code works across memory and mesh and recommend memory for subscriber tests.

### Required closure

Use Redis, JetStream, or a connected mesh pair for round-trip examples. Restrict memory examples to ingestion, batching, backpressure, counters, and lifecycle behavior.

---

## 6. High — Delegated channel credentials are unreachable from every public SDK/binding

Core supports full delegated credentials:

- `MeshNode::subscribe_channel_with_chain(TokenChain)`: `net/crates/net/src/adapter/net/mesh.rs:21826-21847`
- `MeshNode::set_publish_chain`: `net/crates/net/src/adapter/net/mesh.rs:21849-21864`

The Rust SDK exposes only `SubscribeOptions { token: Option<PermissionToken> }` and calls the single-token path:

- `net/crates/net/sdk/src/mesh.rs:100-107,831-848`

Python, Node, C, and Go also accept or parse one `PermissionToken`:

- Python: `net/crates/net/bindings/python/src/lib.rs:2115-2141`
- Node: `net/crates/net/bindings/node/src/lib.rs:2023-2047`
- C: `net/crates/net/src/ffi/mesh.rs:2112-2157`
- Go: `go/mesh.go:1012-1050`

Therefore:

- owner → delegator → subscriber chains cannot subscribe through a public SDK;
- delegated publishers cannot install their chain through a public SDK.

This contradicts `.claude/skills/net-event-bus/concepts.md:135`, which says a full chain is surfaced as `SubscribeOptions.token`, and tells delegated publishers to call `set_publish_chain`.

### Required closure

Expose full `TokenChain` subscription and delegated publish-chain installation across the SDK spine, or mark both operations core-only and remove impossible workflow guidance.

---

## 7. High — Prefix and queue-group security controls are Rust-only

Rust exposes prefix registration:

- `net/crates/net/sdk/src/mesh.rs:762-789`

Core channel security also includes:

- `subscriber_origin_binding`: `net/crates/net/src/adapter/net/channel/config.rs:229-237`
- `queue_group_policy`: `config.rs:238-241`
- queue-group subscription methods: `net/crates/net/src/adapter/net/mesh.rs:21866-21902`

Python, Node, C, and Go registration DTOs omit the policy fields and expose neither prefix registration nor queue-group subscribe:

- Python: `net/crates/net/bindings/python/src/lib.rs:2032-2098`
- Node: `net/crates/net/bindings/node/src/lib.rs:1262-1337`
- C: `net/crates/net/src/ffi/mesh.rs:1993-2015`
- Go: `go/mesh.go:246-263`

Non-Rust operators therefore cannot install custom dynamic-prefix ACLs or configure token-bound/deny worker admission. Auto-installed nRPC defaults may remain protected internally, but the bindings cannot express equivalent custom policy.

### Required closure

Add explicit parity records for prefix ACLs, origin binding, queue-group policy, and queue-group membership. Either expose them consistently or mark them Rust-only with concrete alternatives.

---

## 8. High — Go exposes a fail-closed token switch without token roots

Go's `ChannelConfig` exposes `RequireToken` but not `TokenRoots`:

- `go/mesh.go:246-263`

Core requires trusted roots for token authorization and rejects authorization when token enforcement has no roots (`net/crates/net/src/adapter/net/channel/config.rs:296-311`). Ordinary Go callers can therefore set `RequireToken: true` only to create a permanently closed channel.

The C JSON parser accepts `token_roots`:

- `net/crates/net/src/ffi/mesh.rs:1998-2005,2064-2076`

But the public C header omits that field from its documented config shape:

- `net/crates/net/include/net.go.h:449-460`

### Required closure

Expose `TokenRoots` in Go and document `token_roots` in C. Add positive end-to-end tests proving a token-gated channel can be configured and used through each binding.

---

## 9. Medium — TypeScript leaks `_channel` into typed payloads and validators

TypeScript passes the complete parsed JSON object to the validator or casts it to `T`:

- `net/crates/net/sdk-ts/src/channel.ts:82-92`
- `net/crates/net/sdk-ts/src/stream.ts:106-123`

Python strips `_channel` before constructing the model or returning a dictionary:

- `net/crates/net/sdk-py/src/net_sdk/channel.py:91-103`

A TypeScript reproduction declared `{ value: number }` but received:

```json
{"value":1,"_channel":"sensors/temp"}
```

A strict runtime validator that rejects unknown properties consequently rejects every tagged channel event. Equivalent Python code does not see the metadata field.

### Required closure

Strip `_channel` before TypeScript's default parser and runtime validator, or return an explicit envelope with payload and metadata separated.

---

## 10. Medium — Python's model and custom-parser contract contains broken paths

### Unsupported `parse=` argument

`.claude/skills/net-event-bus/payloads.md:81` recommends:

```python
node.channel(
    "name",
    MyPydanticModel,
    parse=lambda raw: MyPydanticModel.model_validate_json(raw),
)
```

But `NetNode.channel` accepts only `name` and `model`:

- `net/crates/net/sdk-py/src/net_sdk/node.py:267-279`

Only direct `TypedChannel(...)` construction exposes `parse`. The documented call reproduces:

```text
TypeError: NetNode.channel() got an unexpected keyword argument 'parse'
```

The rationale is also misleading: Pydantic's normal `Model(**data)` constructor performs Pydantic validation.

### Slotted dataclass failure

The Python guide says any dataclass works:

- `.claude/skills/net-event-bus/bindings/python.md:71-75`

Serialization recognizes `model_dump`, `dict`, or `__dict__`:

- `net/crates/net/sdk-py/src/net_sdk/channel.py:16-25`

A `@dataclass(slots=True)` has no `__dict__`, falls into the `_value` wrapper, and fails JSON serialization.

### Custom-parser metadata asymmetry

Direct custom parsers receive raw JSON containing `_channel`, while model/default paths remove it:

- `net/crates/net/sdk-py/src/net_sdk/channel.py:89-103`

### Required closure

Expose `parse` through `NetNode.channel` or remove the example; serialize dataclasses with `dataclasses.is_dataclass` / `asdict`; and define whether custom parsers receive payload-only JSON or an explicit envelope.

---

## 11. Medium — Publish return-value and error contracts are conflated

`.claude/skills/net-event-bus/concepts.md:76` says a publisher receives a `Receipt`, null/drop signal, or error.

Actual contracts differ:

| Surface | Single publish | Batch publish |
|---|---|---|
| TypeScript tagged topic | `boolean` | accepted count |
| Python tagged topic | `None` | accepted count |
| Distributed mesh channel | per-peer `PublishReport` | Rust-only `PublishReport` batch |

Sources:

- TypeScript tagged topic: `net/crates/net/sdk-ts/src/channel.ts:54-73`
- Python tagged topic: `net/crates/net/sdk-py/src/net_sdk/channel.py:60-73`
- Rust mesh: `net/crates/net/sdk/src/mesh.rs:873-897`
- TypeScript mesh: `net/crates/net/sdk-ts/src/mesh.ts:661-687`
- Python mesh: `net/crates/net/bindings/python/src/lib.rs:2157-2215`
- Go: `go/mesh.go:1065-1102`
- C: `net/crates/net/include/net.go.h:482-491`

Python discards the native `ingest_raw()` result for a single tagged publish and returns `None`.

`.claude/skills/net-event-bus/bindings/typescript.md:75` also says tagged `channel.publish` never throws. `JSON.stringify` can throw before ingestion for cyclic values, `bigint`, or throwing property accessors.

### Required closure

Document return and error behavior per surface and per binding. Do not use a generic `publish` contract across EventBus ingestion and distributed fan-out.

---

## 12. Medium — Distributed batch fan-out exists only in Rust

Rust exposes `publish_many`, which sends the payload slice as one batch per subscriber and returns one report:

- `net/crates/net/sdk/src/mesh.rs:887-897`

Python, Node, C, and Go expose only one-payload distributed publish:

- Python: `net/crates/net/bindings/python/src/lib.rs:2157-2215,3405-3451`
- Node: `net/crates/net/bindings/node/src/lib.rs:2069-2100`
- C: `net/crates/net/src/ffi/mesh.rs:2240-2315`
- Go: `go/mesh.go:1065-1103`

The ergonomic TypeScript/Python `publishBatch` methods perform local generic EventBus ingestion, not distributed mesh batch fan-out. Looping single mesh publishes changes batching, overhead, and report semantics.

### Required closure

Expose distributed `publish_many` consistently or mark mesh batch fan-out as Rust-only in the capability record and binding guides.

---

## 13. Medium — Membership rejection taxonomy collapses outside Rust

The wire/core protocol defines:

- `Unauthorized`
- `UnknownChannel`
- `RateLimited`
- `TooManyChannels`

at `net/crates/net/src/adapter/net/channel/membership.rs:20-37`.

Rust preserves the reason in:

- `SdkError::ChannelRejected(Option<AckReason>)`: `net/crates/net/sdk/src/error.rs:75-83`

Python and TypeScript preserve only authorization-versus-generic channel classes. C maps only unauthorized specially and maps all other reasons to `NET_ERR_CHANNEL`:

- `net/crates/net/src/ffi/mesh.rs:2193-2203`

Go therefore exposes only `ErrChannelAuth` and `ErrChannel`:

- `go/mesh.go:33-40,57-80`

Rate limiting, capacity rejection, unknown channels, and local validation are not distinguishable in C/Go, preventing callers from choosing correct retry or configuration behavior.

### Required closure

Expose a stable machine-readable rejection reason across every binding.

---

## 14. Low — Permission-token wire-size documentation disagrees with the implementation and itself

Canonical current size:

- `PermissionToken::WIRE_SIZE == 169`: `net/crates/net/src/adapter/net/identity/token.rs:162-167`

Stale binding claims:

- Python: 161 bytes — `net/crates/net/bindings/python/src/lib.rs:2105-2107`
- Python stub: 161 bytes — `bindings/python/python/net/_net.pyi:867,1070`
- Node: 161 bytes — `net/crates/net/bindings/node/src/identity.rs:217-219`, `bindings/node/src/lib.rs:2019-2021`
- C: 161 bytes — `net/crates/net/include/net.go.h:466`
- Go: 159 bytes — `go/identity.go:6,240`, `go/mesh.go:1012-1015`, `go/net.h:451`

A full serialized chain is a distinct shape: `1 + count × 169` bytes. Documentation must not use “token bytes” and “chain bytes” interchangeably.

The bindings currently pass opaque byte buffers, so this is primarily caller-contract drift rather than a hard-coded runtime-length mismatch. It can still make callers reject valid credentials or allocate against a stale format.

### Required closure

Replace stale lengths with the canonical size and add one generated cross-binding wire-format canary.

---

## 15. Medium — TypeScript quickstart documents the opposite `emit()` failure behavior

`web/src/content/docs/sdk/quickstart/typescript.md:26-29` says `emit()` returns `null` on backpressure and does not throw.

The wrapper unconditionally dereferences the result of `ingestRawSync`:

- `net/crates/net/sdk-ts/src/node.ts:66-70`

The native method returns an error on ingestion failure:

- `net/crates/net/bindings/node/src/lib.rs:541-555`

That becomes a thrown JavaScript exception. There is no branch that returns `null`, despite the declared `Receipt | null` type. Focused tests pin throw-on-failure and no-null-on-success:

- `net/crates/net/sdk-ts/test/ingest_failure.test.ts:38-61`

### Required closure

Correct the quickstart and narrow the return type to `Receipt`, unless null-on-backpressure is intentionally restored.

---

## 16. High release blocker — TypeScript package versions contradict their peer contract

Current manifests declare:

- `@net-mesh/sdk` version `0.34.0`: `net/crates/net/sdk-ts/package.json:2-3`
- peer dependency `@net-mesh/core >=0.35.0`: `sdk-ts/package.json:44-49`
- in-tree `@net-mesh/core` version `0.34.0`: `net/crates/net/bindings/node/package.json:2-3`

This contradicts the README instruction to upgrade and pin both packages together:

- `net/crates/net/sdk-ts/README.md:43-52`

Python's `0.34.0` wrapper currently requires `net-mesh>=0.34.0`:

- `net/crates/net/sdk-py/pyproject.toml:9-15`

The raised TypeScript minimum closes the prior compatibility hole with old core methods, but the staged manifests cannot satisfy the same-version guidance until the coordinated `0.35.0` version bump occurs.

### Required closure

Treat this as a release-ordering gate: publish matching core and SDK versions atomically, then verify the installed pair rather than only the file-linked development dependency.

---

## 17. Medium — Lifecycle guidance contains stale or impossible obligations

Rust guidance says outstanding subscription clones make shutdown return an “outstanding references” error:

- `.claude/skills/net-event-bus/bindings/rust.md:137-142`

Current `Net::shutdown(self)` tolerates those clones and shuts down through shared state:

- `net/crates/net/sdk/src/net.rs:235-246`
- `net/crates/net/src/bus.rs:1379-1388`

TypeScript guidance says to close channels before shutting down:

- `.claude/skills/net-event-bus/bindings/typescript.md:88-90`

But `TypedChannel` has no `close` method or independent lifecycle:

- `net/crates/net/sdk-ts/src/channel.ts:28-105`

### Required closure

Document lifecycle operations that actually exist. Explain that tagged channels are lightweight wrappers owned by the node rather than independently closable resources.

---

## 18. Medium — Channel-registry permissiveness differs across bindings

Defaults are not fully symmetric:

- Raw core `MeshNode` is permissive if no registry is installed.
- Rust SDK and C/Go install a strict empty registry by default.
- Python and Node are strict by default but expose `permissive_channels` opt-outs:
  - Python: `net/crates/net/bindings/python/src/lib.rs:1277-1287,1439-1445`
  - Node: `net/crates/net/bindings/node/src/lib.rs:1217-1230,1641-1650`
- C and Go expose no equivalent opt-out.

This difference may be intentional, but it is not represented clearly in binding guidance or the capability matrix.

### Required closure

Document strictness defaults and opt-outs per binding. If permissive mode is test-only, name and gate it accordingly rather than presenting it as ordinary channel behavior.

---

## Verification and evidence

The following checks were executed against the audited commit:

- Rust channel-name tests: **17 passed**.
- Rust permission-token tests: **63 passed**.
- Rust SDK distributed-channel integration: **3 passed**.
- TypeScript distributed-channel tests: **8 passed**.
- TypeScript channel-auth tests: **6 passed**.
- TypeScript ingestion-failure tests: **7 passed**.
- TypeScript SDK build: passed.
- Python focused tagged-channel option tests: **5 passed**.
- `.github/scripts/check-skills.sh`: passed.
- `.github/scripts/capability_records.py --check`: passed during the parallel audit.
- `git diff --check`: passed.

Additional recording-backend reproductions covered:

- invalid channel-name acceptance in tagged TypeScript and Python wrappers;
- TypeScript `_channel` metadata leakage;
- strict TypeScript validator rejection;
- Python metadata removal;
- Python's unsupported `parse=` argument;
- slotted-dataclass serialization failure;
- Python's discarded single-publish result.

### Verification limitations

- The native Python extension was unavailable for the high-level wrapper reproduction, so those checks used the checked-out Python source with a recording backend. This is sufficient to prove that the wrapper performs no validation and serializes the unchecked name before any native channel API can be reached.
- Direct `go test ./...` could not build without the repository's generated/native binding configuration; it failed on undefined binding symbols. Go findings are source/ABI contract findings rather than successful standalone Go execution.
- No TypeScript test directly exercises ergonomic `TypedChannel`; current Python coverage is limited to subscription-option copying. The reproduced wrapper behavior is therefore not protected by the existing SDK suites.

---

## Checker and release propagation gap

The public `ai-2070/net-claude-skill` mirror was byte-identical across all 43 `net-event-bus` files at the audited commit. Generated coverage artifacts also matched their canonical capability record.

The green checks prove:

- symbols and source paths resolve;
- generated copies match;
- vocabulary is closed;
- release mirrors are synchronized.

They do **not** prove that identically named APIs have the same ownership, transport, policy, return values, or lifecycle. In this case, the checks consistently propagated a false interpretation into released artifacts.

Add semantic canaries for:

1. API homonyms such as `channel`, `publish`, and `subscribe`;
2. actual construction and validation paths;
3. state owner: local EventBus versus publisher roster;
4. matching rule: exact payload filter versus exact membership versus prefix ACL resolution;
5. return/error shape;
6. default transport behavior;
7. binding-only policy omissions;
8. runnable examples with observable delivery.

---

## Recommended repair sequence

1. **Rename or structurally split the two channel surfaces in documentation.** Establish the correct mental model before patching examples.
2. **Add canonical name validation to tagged TypeScript/Python constructors.** Add exhaustive inverse tests in both SDKs.
3. **Repair the distributed-channel parity matrix.** Track full chains, delegated publisher chains, prefix ACLs, origin binding, queue-group policy and subscribe, token roots, mesh batch fan-out, rejection reasons, and permissive mode separately.
4. **Close security configuration dead ends.** In particular, make Go token roots configurable and expose or explicitly reject unsupported delegated workflows.
5. **Normalize payload and parser semantics.** Remove `_channel` before typed validation or use an explicit envelope; repair Python parser and dataclass handling.
6. **Correct return values, failure behavior, lifecycle, and token-size documentation.** Back each claim with focused executable tests.
7. **Replace non-delivering memory examples.** Use an adapter/topology that can produce the promised observable result.
8. **Complete coordinated package versioning before release.** Verify installed artifacts, not only file-linked development builds.
9. **Add semantic documentation gates.** Preserve copy-equality checks, but do not treat them as behavioral proof.

## Audit conclusion

The original channel-name report should be accepted. It understated the scope: both ergonomic SDKs bypass canonical validation, and the public documentation merges that tagged-event abstraction with a separate authenticated distributed channel protocol. Several security and delivery features are implemented only in core or Rust while the cross-SDK prose presents them as generally available. The repair must establish separate, testable contracts for the two surfaces and then make binding parity explicit rather than inferred from shared method names.
