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
