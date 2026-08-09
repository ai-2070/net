# Pending breaking changes for 0.35

Staging file, not a release note. `RELEASE_STEPS.md` picks the codename
at step 2 and drafts the note at step 4; fold these entries in then and
delete this file.

Because it lives beside the real notes, it is listed in
`NOT_PUBLISHED` in `web/scripts/sync-releases.mjs` — everything else in
this directory is mirrored to the site, and a half-assembled changelog
under a version that has not shipped should not be. **Remove that entry
when you delete this file**, or `npm run check:releases` will keep
excusing a name that no longer exists.

Each entry is written to be pasted into the note's breaking-changes
section as-is.

---

## nRPC C ABI synchronizes at `0x0004`, and the compatibility check
becomes exact

**Who is affected:** anyone compiling C or cgo against `net_rpc.h`.

The public header shipped at `NET_RPC_ABI_VERSION 0x0002` while the
implementation was `0x0004`. The two cancellation functions gained a
leading `MeshRpcHandle*` at `0x0004` — so cancellation routes through
the substrate's per-mesh `CancelRegistry`, which is what makes
mid-stream cancel work across every streaming shape — and the header
never followed:

```c
/* was */ uint64_t net_rpc_reserve_cancel_token(void);
/* was */ void     net_rpc_cancel_call(uint64_t token);

/* now */ uint64_t net_rpc_reserve_cancel_token(MeshRpcHandle* handle);
/* now */ void     net_rpc_cancel_call(MeshRpcHandle* handle, uint64_t token);
```

A consumer built against the old header passed whatever happened to be
in the first argument register as a mesh pointer.

**`net_rpc_check_abi_version` now requires exact equality.** It compared
`runtime >= expected`, so a stale `0x0002` header checking a `0x0004`
library *passed* — the one guard that existed was blind to precisely
the change it was there to catch. Equality can reject a future
purely-additive release unnecessarily; that failure is safe, and
additive compatibility can be reintroduced later behind an explicit
major/minor or supported-range contract.

**What to do:** rebuild against the new header, and pass your
`MeshRpcHandle*` to both cancellation functions. Reserve and cancel must
use the same handle.

`tests/cross_lang_nrpc/golden_vectors.json` moves to
`"abi_version_expected": 4`; the Node and Python fixture assertions
follow.

Drift is now checked mechanically by
`.github/scripts/check-rpc-abi-parity.py` (ABI constant, function
names, argument count and order, pointer-versus-value, return types),
wired into the Go FFI job in `ci.yml`. Reverting either half of this
change reproduces a failure there.

---

## Node nanosecond timestamps become `bigint`

**Who is affected:** every TypeScript and JavaScript consumer that reads
`Receipt.timestamp` or `StoredEvent.insertionTs`.

Both were `number`. Unix-epoch nanoseconds crossed JavaScript's
exact-integer ceiling (`2^53 - 1`) around 104 days past 1970, so every
realistic value on these fields had already lost its low-order digits
— `9007199254740993` read back as `9007199254740992`. The native path
also cast the core `u64` through `i64`, a second narrowing.

```typescript
// was
interface Receipt { shardId: number; timestamp: number }
interface StoredEvent { insertionTs: number; /* ... */ }

// now
interface Receipt { shardId: number; timestamp: bigint }
interface StoredEvent { insertionTs: bigint; /* ... */ }
```

No compatibility alias was added. One would have preserved incorrect
data rather than compatibility, and Node already used `BigInt` for
large counters and stream timestamps — these fields were the
inconsistency.

**What to do:** arithmetic on these fields needs `bigint` literals
(`ts - start` where both are `bigint`; `1_000_000n`, not `1_000_000`).
Mixing `bigint` and `number` in one expression is a `TypeError`.

**`JSON.stringify` throws on a `bigint`.** Convert explicitly where you
serialize or log:

```typescript
const timestampMs = Number(insertionTs / 1_000_000n);
```

Sub-microsecond latency deltas are now trustworthy, which they were not
before.

---

## `StoredEvent` gains `rawBytes` (TypeScript)

**Who is affected:** TypeScript callers who construct or structurally
type a `StoredEvent`.

`rawBytes: Buffer` is now a required member. The native binding always
preserved the payload bytes; this wrapper dropped them from both
`poll()` and the streaming projection, so binary accepted through the
wrapper's own `emitBuffer()` could not be read back through the same
wrapper at all.

Reading is unaffected — the field is additive there. Anything that
builds a `StoredEvent` literal (test fixtures, mocks, adapters) needs
the extra field.

Note also that `raw` is deliberately **empty** when the payload is not
valid UTF-8; the binding does not substitute a lossy decode. Check
`rawBytes` rather than treating an empty `raw` as an empty event.

---

## Go: `StreamConfig.WindowBytes` becomes `*uint32`

**Who is affected:** every Go caller that sets `WindowBytes`.

```go
// was
WindowBytes uint32 `json:"window_bytes,omitempty"`
// now
WindowBytes *uint32 `json:"window_bytes,omitempty"`
```

A plain `uint32` under `omitempty` cannot express zero. The documented
"0 disables backpressure entirely" was erased from the JSON before it
reached the C parser, which then applied the 64 KiB default — so a
caller asking for unbounded mode silently got bounded mode, and the one
value the field documented as special was the one value it could not
send.

**What to do:** `net.WindowBytesOf(n)` for an explicit size,
`net.UnboundedWindow()` for the pointer-to-zero that disables
backpressure, `nil` to inherit the default. This is a compile error at
every existing call site, which is the intent — there is no silent
pointer conversion, so nothing changes meaning without you seeing it.

---

## Go: `New` rejects an unrecognized `Reliability`

**Who is affected:** Go callers passing anything outside
`{"", "none", "light", "full"}` to `NetConfig.Reliability`.

The value was matched with a `_ => None` fallback, so `"FULL"`,
`"ful"`, `"reliable"` and every other near miss constructed
successfully and silently downgraded delivery from acknowledged and
retransmitted to fire-and-forget. Role and backpressure already
rejected unknown values; reliability was the inconsistent one, and it
is the one where the failure stays invisible until you go looking for
messages that were never delivered.

`New` now returns an error naming the value, and the C parser refuses
it independently — Go reaches that parser with an unchecked string, so
the server-side guard is also the only guard for callers that build
config JSON by hand.

**What to do:** the vocabulary is case-sensitive. Use
`net.ReliabilityModes` if you are validating caller input yourself.

---

## Go: `IngestBatch` and `IngestBatchChecked` differ deliberately

**Who is affected:** nobody upgrading from 0.34 — this documents a
distinction rather than a change. It is here because the two functions
look interchangeable and are not.

`IngestBatch` skips values whose `json.Marshal` fails and ingests the
rest, returning the accepted count. That count can be lower than
`len(events)` for two different reasons — a marshal failure or a
ring-buffer drop — which its signature cannot distinguish.

`IngestBatchChecked` is the same work with the marshal failure reported
instead of swallowed: nothing is ingested, the error names the index,
and the count is 0. That matches Rust, TypeScript and Python, which all
serialize the whole batch before ingesting.

**What to do:** prefer `IngestBatchChecked` for new code. A drop is
backpressure and may be retried; a marshal failure is a payload bug
that will fail identically forever, and the two want different
responses.

---

## The C ABI fails closed on values it used to ignore

**Who is affected:** C and cgo callers of `net_poll`,
`net_mesh_announce_capabilities`, `net_mesh_find_nodes`,
`net_mesh_find_best_node` and their scoped variants.

Each of these accepted input it then discarded, which is the worst
shape for an option: the call succeeded, so the caller had no way to
learn its intent had been dropped.

- **`net_poll` rejects unrecognized keys and non-object requests.** The
  parser read only `limit` and `cursor` while the docs advertised
  `ordering`, so a caller asking for cross-shard ordering got an
  unordered response and a success code. `ordering`, `filter` and
  `shards` are now honoured; a typo like `"order"` is `InvalidJson`
  rather than a silent revert to the default; and a request that is not
  a JSON object at all (`[]`, `42`, `"limit"`) is refused instead of
  being read as all-defaults. `{}` remains valid — every key is
  optional.
- **An unknown capability modality rejects the whole call.** It used to
  fall back to `Text`, and later to being dropped with a warning.
  Falling back advertised a capability the node does not have; dropping
  shipped an announcement silently missing one. On a *filter* the drop
  is fail-open — it removes the constraint and widens the query to
  every otherwise-eligible node, so the scheduler can pick a node that
  cannot do the work.
- **An unknown `reliability` rejects the config**, as described under
  the Go entry above.

**What to do:** the vocabularies are case-sensitive and listed in
`net.go.h` beside each function. Requests that were already correct are
unaffected.

---

## `install_rpc_service_defaults` returns `Result`, and `serve_rpc*` can
fail on a long service name

**Who is affected:** Rust callers of
`ChannelConfigRegistry::install_rpc_service_defaults`, and anyone
serving an nRPC service whose name is close to the channel-name length
limit.

The function returned `()` and swallowed validation failures, so a
serve call succeeded against a registry that had silently installed no
policy — and every request to that service was then refused as an
unknown channel, with the refusal surfacing on the *caller's* side, far
from the registration that caused it. It now returns
`Err(ServeError::InvalidServiceName)` having mutated nothing.

The knock-on: core's four serve seams install this policy themselves
now, which is what gives Node, Python and Go/C any nRPC channel policy
at all — they call `MeshNode::serve_rpc*` directly and never reached
the SDK hop that used to do it. So `serve_rpc*` propagates that error,
and a service name long enough that `<service>.replies.<16 hex digits>`
exceeds the channel-name limit fails at registration instead of
registering successfully and refusing every request later.

**What to do:** the longest usable service name is
`MAX_NAME_LEN - len(".replies.0123456789abcdef")`. Names that fit are
unaffected.

---

## `PermissionToken::delegate` stamps the signer's generation, not the parent's

**Who is affected:** Rust callers of `PermissionToken::delegate` and
the SDK delegation builders.

A delegated child used to inherit `parent.issuer_generation`. The
child's issuer is the *signer*, so that stamped an epoch belonging to
an entity that did not sign the child, and the floor consulted at
verify time — the signer's — was compared against a number from someone
else's rotation history. In a chain `root -> machine -> gateway`, the
`machine -> gateway` link carried root's generation while being checked
against machine's floor.

Each link now carries its own signer's generation. Revocation stays
transitive: `TokenChain` checks every link against the floor for that
link's own issuer, so revoking the root still breaks the root-issued
link, which is the link the root actually signed.

`delegate()` itself stamps signer generation **zero**. With the public
issuance surface as it stood, every reachable parent was already at
generation zero, so observable behaviour is unchanged — which is
exactly why the old rule could be wrong for this long without anything
failing.

**What to do:** anything that maintains issuer state should use
`delegate_with_generation` and pass the signer's own generation. The
SDK builders already do.

---

## Python: `TypedChannel.publish` returns a `Receipt`, and `_to_dict` raises

**Who is affected:** `net_sdk` Python callers of `TypedChannel`.

`publish()` returned `None`, discarding the native `ingest_raw` result.
It now returns the same `Receipt` as `NetNode.emit`. Additive for
anything that ignored the return value.

The behaviour change is in serialization. An event that is not a dict,
dataclass, Pydantic model, or object with `__dict__` / `__slots__` used
to be wrapped as `{"_value": event}`, which then died inside
`json.dumps` with a message pointing at the wrapper rather than at the
payload. It now raises `TypeError` naming the type, at the point of the
call.

Two related fixes ride along:

- `@dataclass(slots=True)` instances are handled. The old duck-typed
  chain checked `__dict__` before dataclass-ness, and a slotted
  dataclass has no instance `__dict__` — so "any dataclass works" was
  only true without `slots=True`.
- The `_channel` routing tag is stripped before a custom `parse=`
  callable sees it, matching the `model` and default paths. A strict
  Pydantic model (`extra="forbid"`) previously rejected every event on
  the channel because of routing metadata it never declared.

**What to do:** if you were relying on the `{"_value": ...}` wrapper,
pass a mapping. `subscribe_raw()` still yields the tag.
