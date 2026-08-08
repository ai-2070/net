# Pending breaking changes for 0.35

Staging file, not a release note. `RELEASE_STEPS.md` picks the codename
at step 2 and drafts the note at step 4; fold these entries in then and
delete this file.

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
