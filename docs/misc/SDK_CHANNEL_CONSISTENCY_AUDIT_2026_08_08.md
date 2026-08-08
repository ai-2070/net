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

## Resolution status

**Remediated:** 2026-08-08, on `master` from `51641125d`. The findings below are unedited — they record what was true at the audited commit, and the stale values they quote (161-byte tokens, `A-Z` in channel names) are evidence, not claims. Each `Required closure` now carries a status line.

| # | Finding | Status | Commit |
|---|---|---|---|
| 1 | Name-validation bypass | Closed | `90cbd4d1e` |
| 2 | Docs conflate the two surfaces | Closed | `4e3263ba1` |
| 3 | Prefix subscription unimplemented | Closed | `4e3263ba1` |
| 4 | Creation / replay / roster not universal | Closed | `4e3263ba1` |
| 5 | Memory transport cannot deliver | Closed | `15565832b` |
| 6 | Delegated credentials unreachable | Closed as documented gap | `36afee7e9` |
| 7 | Prefix + queue-group policy Rust-only | Closed as documented gap | `36afee7e9` |
| 8 | Go fail-closed switch without roots | Closed; cgo tests unverified locally | `a44397916` |
| 9 | TS leaks `_channel` into payloads | Closed | `439f1e1b4` |
| 10 | Python model/parser contract broken | Closed | `a30d635ef` |
| 11 | Publish return/error contracts conflated | Closed | `bd57d5285` |
| 12 | Batch fan-out Rust-only | Closed as documented gap | `36afee7e9` |
| 13 | Rejection taxonomy collapses | **Outstanding** — recorded, not fixed | `36afee7e9` |
| 14 | Token wire-size drift | Closed, with a canary | `b7cc155e3` |
| 15 | TS quickstart inverts `emit()` failure | Closed | `bd57d5285` |
| 16 | TS package versions contradict peer | Gate added; **bump outstanding** | `2cd16f781` |
| 17 | Stale lifecycle obligations | Closed | `02ee556e7` |
| 18 | Registry permissiveness asymmetric | Closed as documented gap | `36afee7e9` |

**"Closed as documented gap"** means the finding offered "expose it consistently, or mark it explicitly and remove the impossible guidance" and the second branch was taken: the capability record now carries a cell per gap, every positive cell anchored to a symbol CI resolves, and the prose that told readers to call unreachable APIs is gone. Exposing these is new public API across five languages and is deliberately not in this pass.

### What is genuinely still open

1. **Finding 13 offered no alternative branch and was not closed.** Its required closure — expose a stable machine-readable rejection reason across every binding — is a wire-to-binding change in C, Go, Node, and Python. The taxonomy is now *recorded* as `partial` outside Rust, so a reader is no longer misled, but a C or Go caller still cannot distinguish rate-limited from unknown-channel from too-many-channels, which are three different remediations.
2. **Finding 16's version bump.** The strict gate fails today, by design. Publishing needs `@net-mesh/core` 0.35.0 to land first; the manifests are untouched because that is a release-ordering decision, not a code fix.
3. **Finding 8's end-to-end tests are unverified in this tree.** `go vet ./...` does not build here — `meshos_test.go` needs the generated native binding configuration, the same limitation this audit hit. `gofmt` is clean and the JSON-marshalling tests are pure Go; the four cgo-backed registration tests need a CI run.
4. **No semantic canary for API homonyms.** Three new checkers landed (below); none is the homonym check the "Checker and release propagation gap" section asks for. Items 1–3 and 5–8 of that list remain unautomated.

### Guards added

The audit's central point — that copy-equality checks propagated a false interpretation into released artifacts — produced checkers rather than only one-off edits:

- `.github/scripts/check-token-wire-size.py` derives `PermissionToken::WIRE_SIZE` from `token.rs` and fails on any disagreeing claim across the tracked tree. It found two stale comments the manual sweep missed. Wired into `check-docs.sh`; release notes, internal plans, and this document are excluded as dated records.
- `.github/scripts/check-npm-peer-range.py` verifies `@net-mesh/sdk`'s peer range admits the in-tree `@net-mesh/core`. Non-strict in CI, strict in `release-npm-sdk.yml`.
- Nine new capability-record operations replace the single coarse `Channels — pub/sub with capability auth` row, each positive cell anchored.

### Verification run on the remediated tree

- `check-skills.sh`, `check-docs.sh`, `capability_records.py --check`: pass.
- `sdk-py` pytest: 327 passed (was 303; +24 new).
- `sdk-ts` vitest on the channel suites: 94 passed. `tsc` build clean.
- `go vet ./...`: does not build, for the pre-existing reason in item 3 above.

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

**Status: closed** (`90cbd4d1e`). `validate_channel_name` / `validateChannelName` mirror the Rust grammar and run in the `TypedChannel` constructor, so direct construction is covered and not only `NetNode.channel()`. Both are exported so a caller can pre-check a name without building a channel. Inverse tests cover every Rust boundary — empty, over 255 bytes (byte-counted, so a 128-character two-byte name fails), leading and trailing `/`, `//`, uppercase, invalid characters, and `.` / `..` segments — 52 in Python, 74 in TypeScript. The fourth question is answered by finding 2: these are tagged EventBus topics, and are now named that way everywhere.

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

**Status: closed** (`4e3263ba1`). `concepts.md` § Channel opens with a side-by-side table of the two surfaces and states which one the rest of the section describes. `apis.md` lists five surfaces rather than four, with the tagged and distributed rows separate and a routing question that sends cross-process work to the mesh surface. The false "Rust has no named channels" prose is corrected in `bindings/rust.md`, `go.md`, and `c.md` — what those bindings lack is the tagged wrapper, not distributed pub/sub. Python is marked `core-only`. Both package README surface tables now read "Distributed mesh channels".

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

**Status: closed** (`4e3263ba1`) — removed. Gone from `concepts.md` and from `web/src/content/docs/concepts/channels.md`, in both the naming section and the page intro that promised subscribing to whole subtrees. Replaced with an explicit denial (subscribing to `sensors/lidar` does *not* deliver `sensors/lidar/front`, because rosters key on the exact `ChannelId` and the tagged filter is an exact match) and a separate paragraph on `register_channel_prefix` as publisher-side ACL resolution: it decides whether a join is allowed, never what a subscriber receives.

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

**Status: closed** (`4e3263ba1`). The surface table covers creation, state ownership, authorization, and network crossing per surface. The implicit-creation bullet now separates the missing *cluster-wide* step from the required *local* one, and notes that creation really is implicit on the tagged surface. § Subscriber states that hot-not-cold holds on both, that joining a roster is not a replay, and that the tagged surface has no membership step to reject and nothing that can evict you. § Publisher notes there is no publisher role on the tagged surface at all.

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

**Status: closed** (`15565832b`). Both package READMEs, `web/src/content/docs/guides/event-bus.md` (all four language snippets), and the TS/Python skill quickstarts configure a delivering transport, with the reason inline in the snippet rather than in a caveat further down. `SKILL.md`'s transport-transparency claim and its "default to memory for tests" step are reconciled with `apis.md`, `testing.md`, and the runnable `examples/hello.*`, which already had it right; `runtime.md` § Tests likewise. The `concepts.md` transport table now states that memory does not deliver, and that transparency is a statement about the code rather than the behaviour.

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

**Status: closed as documented gap** (`36afee7e9`) — second branch. Both operations are `not exposed` in all five bindings in the capability record, with the rationale in `coverage.md`. The impossible guidance is gone: `concepts.md` no longer says a full chain is surfaced as `SubscribeOptions.token`, and now states that every published surface carries exactly one `PermissionToken`, with two workarounds — mint a token issued directly to the subscriber by a root the channel trusts, or drive the core `MeshNode` from Rust. Exposing `TokenChain` across the spine remains open work.

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

**Status: closed as documented gap** (`36afee7e9`) — second branch. Three new record rows: prefix ACL registration (Rust-only), origin binding plus queue-group policy (Rust-only, reachable because the SDK re-exports the core `ChannelConfig` builder), and queue-group subscription (`not exposed` everywhere including the Rust SDK — it is on the core `MeshNode`). `coverage.md` notes that the auto-installed nRPC defaults stay protected either way; what a non-Rust operator cannot do is install custom policy of their own.

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

**Status: closed; cgo tests unverified locally** (`a44397916`). Go's `ChannelConfig` gains `TokenRoots []string` carrying the `token_roots` JSON tag the C FFI's `ChannelConfigInput` already parsed, with `omitempty` so an untouched config marshals byte-identically to before. Both hand-maintained C headers document the field and state that `require_token` without roots is a permanently closed channel; `mesh.md` says the same and gives the spelling per binding. A new capability-record row covers token roots across all five. Tests cover the JSON key (a tag typo silently drops the roots and leaves the channel closed with no error anywhere), the omitempty path, and registration with valid and malformed roots — but `go vet ./...` does not build in this tree for an unrelated reason, so the cgo-backed cases need a CI run.

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

**Status: closed** (`439f1e1b4`) — strip rather than envelope, for parity with Python's existing behaviour. Both TypeScript parse paths strip the tag; `subscribeRaw()` still yields the stored event verbatim and is documented as the escape hatch for readers that want it. `CHANNEL_TAG_KEY` is exported so callers can name it. Tests cover the default cast, the validator path, a strict validator that rejects unknown properties, batch publishes, the raw passthrough, and a non-object payload.

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

**Status: closed** (`a30d635ef`) — all three, and expose rather than remove. `_to_dict` checks `is_dataclass` before `__dict__` (so `slots=True` works) and uses `asdict`, which also recurses into nested dataclasses; plain slotted classes gather their declared slots; anything else raises a named `TypeError` at publish rather than building a payload that dies one frame deeper inside `json.dumps`. Custom parsers receive payload-only JSON, matching the model and default paths and the TypeScript SDK. The `payloads.md` rationale is corrected too: `Model(**data)` does run Pydantic validation — what it skips is the coercion and validators `model_validate_json` applies.

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

**Status: closed** (`bd57d5285`). `concepts.md` replaces the single-contract sentence with a four-row table — tagged TS, tagged Python, bus ingestion, distributed mesh — and notes that even a `PublishReport` reports what was sent, not what arrived. Python's `TypedChannel.publish` returns the `Receipt` it was discarding; `Receipt` moved to a small `net_sdk.types` module so `channel` can use it without an import cycle, and is still re-exported from `net_sdk.node`. `bindings/typescript.md` narrows "never throws" to ingestion only and names the `JSON.stringify` throw that precedes it.

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

**Status: closed as documented gap** (`36afee7e9`) — second branch. New record row, Rust-only. `coverage.md` notes that looping single publishes changes batching, per-call overhead, and report granularity, so it is not a drop-in substitute, and that the ergonomic `publishBatch` / `publish_batch` is local ingestion rather than this.

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

**Status: OUTSTANDING** — `36afee7e9` records it, and does not fix it. This finding offered no alternative branch, and the required closure is a wire-to-binding change across C, Go, Node, and Python that was out of scope for this pass. What landed is a capability-record row marking the taxonomy `partial` outside Rust, plus `coverage.md` prose spelling out the cost: a caller outside Rust cannot tell "retry later" (rate-limited) from "fix your config" (unknown channel) from "raise a limit" (too many channels). Readers are no longer misled; callers still cannot branch correctly.

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

**Status: closed** (`b7cc155e3`) — both halves. Every claim now reads 169: the Python binding and its `.pyi` stub, the Node binding and its `index.d.ts`, the C header, Go (from 159), and the docs site's security model and glossary. The C headers and Go also note that a full `TokenChain` (`1 + count * 169` bytes) will not fit where one token is accepted — the two shapes were being used interchangeably. `check-token-wire-size.py` derives the size from `token.rs` rather than hardcoding it and runs over the tracked tree; it found two stale comments in the Go and Python channel-auth tests that the manual sweep had missed.

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

**Status: closed** (`bd57d5285`) — narrow, not restore. `emit()` and `emitRaw()` are typed `Receipt`. The quickstart now says a backpressure drop arrives as a throw rather than a `null`, and points at `fire()` for genuine fire-and-forget. `bindings/typescript.md`'s error table is updated and no longer describes the `| null` as vestigial, because it is gone.

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

**Status: gate added; bump outstanding** (`2cd16f781`). The manifests are untouched — which version to publish is a release decision, not a code fix. What landed is the verification this closure asks for. `check-npm-peer-range.py` compares the peer range against the in-tree core rather than the `file:` devDependency that carries no version and made this invisible to every build step. Non-strict in the `sdk-ts-tests` CI job, accepting a floor aimed at an unreleased version when the SDK CHANGELOG declares it — the current state of the tree. Strict in `release-npm-sdk.yml`, where the manifest version is final by definition: **it fails today, which is the point**. The README also now tells callers not to `--force` past an unmet peer warning.

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

**Status: closed** (`02ee556e7`). `bindings/rust.md` no longer claims `shutdown()` reports outstanding references — it contradicted its own `runtime.md` section — and the troubleshooting entry that existed to handle that error now describes the real hazard: events left undrained by a stream that outlived the call. The error does exist, on `MeshNode` shutdown where an un-closed client holds a genuine strong reference, and the entry points there. `bindings/typescript.md` states there is nothing to close first: a `TypedChannel` is a name, a prebuilt filter string, and an optional validator, all owned by the node; a live subscription is what needs `stop()`.

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

**Status: closed as documented gap** (`36afee7e9`). New record row: `permissive_channels` / `permissiveChannels` is `supported · core-only` in Python and Node, `not exposed` in Rust, Go, and C. `coverage.md` states the full asymmetry — raw core permissive when no registry is installed, Rust SDK and C and Go strict, Python and Node strict with an opt-out — and names the flag a bootstrap and test affordance for the dynamic channel names the enrollment nRPC path needs, not ordinary channel behaviour. It is documented rather than code-gated; gating it is separate work.

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

### Sequence outcome

| Step | Outcome |
|---|---|
| 1. Split the two surfaces in documentation | Done — `4e3263ba1` |
| 2. Canonical name validation + inverse tests | Done — `90cbd4d1e` |
| 3. Repair the parity matrix | Done — `36afee7e9`, nine new record rows replacing one coarse row |
| 4. Close security configuration dead ends | Partly — Go token roots done (`a44397916`); delegated workflows explicitly rejected in docs rather than exposed |
| 5. Normalize payload and parser semantics | Done — `439f1e1b4` (strip, not envelope) and `a30d635ef` |
| 6. Correct returns, failure, lifecycle, token size | Done — `bd57d5285`, `02ee556e7`, `b7cc155e3` |
| 7. Replace non-delivering memory examples | Done — `15565832b` |
| 8. Coordinated package versioning | Gate added (`2cd16f781`); the bump itself is a release action and is not done |
| 9. Semantic documentation gates | Partly — two new checkers; the API-homonym canary is not built |

The sequence was followed in a different order than listed: the code fixes (steps 2, 5, 6) went first because they are independently testable and their tests pin the contracts the documentation then describes, and the documentation split (step 1) landed once those contracts were settled. The mental-model-first ordering is right for a reader; commit-first ordering was right for verification.

## Audit conclusion

The original channel-name report should be accepted. It understated the scope: both ergonomic SDKs bypass canonical validation, and the public documentation merges that tagged-event abstraction with a separate authenticated distributed channel protocol. Several security and delivery features are implemented only in core or Rust while the cross-SDK prose presents them as generally available. The repair must establish separate, testable contracts for the two surfaces and then make binding parity explicit rather than inferred from shared method names.

### Remediation conclusion (2026-08-08)

Fifteen findings are closed, five of those by explicitly recording a gap rather than filling it — the branch each of those findings offered. Three items remain genuinely open and are listed under **What is genuinely still open** above: the rejection-reason taxonomy (finding 13, the only closure with no alternative branch that was not implemented), the coordinated version bump (finding 16), and the API-homonym canary.

The audit's own framing held up under repair. Every code defect it named was real and reproducible, and the two it flagged as scope-understating — the TypeScript validation bypass and the surface conflation — were the ones that cascaded furthest. Its one imprecision, noted in the original report on this repository, was mechanical rather than substantive: the wrappers do not pass an unchecked name into a channel FFI, they serialize it into generic event JSON, so `ChannelName::new` was never on the path at all. That distinction is what made the fix a constructor-side validator in each SDK rather than a plumbing change.

What the remediation added beyond the findings is the guard layer. The audit's sharpest observation was structural rather than per-defect: the green checks proved that symbols resolve, generated copies match, and mirrors are synchronized, while proving nothing about ownership, transport, policy, return values, or lifecycle — and so propagated a false interpretation into released artifacts. Two of the three new checkers exist because a stale number and an unsatisfiable version range each survived a full audit cycle behind exactly that kind of green check. Both fail on real drift today: `check-token-wire-size.py` found two stale comments the manual sweep missed, and `check-npm-peer-range.py --strict` fails on the shipped peer range by design, and will keep failing until the release lands.
