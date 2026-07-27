# Security Audit — `crates/net` (2026-06-09)

Scope: the `net` crate (mesh networking library — pub/sub channels, nRPC,
capability auth, NAT traversal, blob/dir transfer, RedEX/Dataforts persistence).
Method: four parallel deep-dive passes (FFI/unsafe, untrusted wire input & DoS,
auth/capabilities/crypto, filesystem/path/injection). The critical finding was
re-verified by hand against the live code paths.

Overall the crate is unusually well-hardened — most classic hazards already have
named, tested mitigations. One **critical authorization bypass** stands out and
should be fixed before anything else.

---

## 🔴 CRITICAL — Capability fold never binds wire `node_id` to the signature-verified publisher

An authenticated mesh peer can forge capability/authz state for **any other
node** and bypass the nRPC capability allow-list entirely. The same primitive
also enables reservation/lock hijacking.

### Locations
- `src/adapter/net/behavior/fold/wire.rs:303-338` — `SignedAnnouncement::verify`
  verifies the Ed25519 signature over a transcript that *includes* `node_id`,
  but never checks `ann.node_id == publisher.node_id()`.
- `src/adapter/net/behavior/fold/dispatch.rs:96-124` — `dispatch` calls
  `decode_and_verify(bytes, publisher)` and does not bind the node either.
- `src/adapter/net/behavior/fold/mod.rs:336-361` — `Fold::apply` keys all state
  on the attacker-supplied `ann.node_id` (`by_node.entry(ann.node_id)`). Comment
  at `mod.rs:333` says "Signature verification is the dispatch layer's job; this
  method trusts the caller" — but dispatch only proves the signature is
  *internally valid*, never that the claimed node is the signer.
- `src/adapter/net/behavior/fold/capability.rs:358-359` — `key_for` =
  `(payload.class_hash, node_id)`.
- `src/adapter/net/behavior/fold/capability_bridge.rs:309-374` — `may_execute`
  reads `state.by_node.get(&target_node)`.
- `src/adapter/net/mesh_rpc.rs:1922-1929` — callee gate consumes `may_execute`.
- `src/adapter/net/mesh.rs:4494-4508` — wire entry point.

### Exploit chain (all four links confirmed against the code)
1. Peer A (legitimately authenticated via PSK + Noise) signs a
   `CapabilityMembership` envelope with **its own** entity key, but sets the
   internal `node_id` field to victim C's node id.
2. `verify` passes — it is a valid signature by A over those bytes; nothing
   requires the node id to be A's.
3. `apply` installs the entry under key `(class_hash, C)` — the forged entry now
   lives in C's own capability state, carrying e.g.
   `tags:[nrpc:<service>], allowed_nodes:[A]`.
4. When A calls the gated service, the callee gate
   `may_execute(fold, target_node=C, tag, caller=A)` reads `by_node[C]`, finds
   the forged entry, sees the tag and `allowed_nodes` containing A → returns
   `true`.

### Impact
- Complete bypass of the per-node nRPC capability allow-list: any authenticated
  mesh participant can invoke any capability-gated service on any node.
- Forge/overwrite/strip other nodes' advertised capabilities globally (DoS by
  cap-stripping; poisoning scheduler placement and channel `subscribe_caps`
  synthesis).
- **Same unbound-`node_id` primitive hits `ReservationFold`**
  (`src/adapter/net/behavior/fold/reservation.rs:199` —
  `let publisher = incoming.node_id;`, with the generation check dropped on
  cross-publisher transitions at `:217-222`): a peer can claim/steal
  reservations and locks on behalf of arbitrary node_ids.

Confidence: **high**. Doc comments at `capability.rs:18-24` and
`dispatch.rs:239-242` explicitly *assume* this invariant but no code enforces it.

### Fix
One check, in `verify` / `decode_and_verify` (and the reservation path): reject
when `ann.node_id != publisher.node_id()`. Closes capability injection and
reservation hijack simultaneously. Add a regression test asserting a mismatched
`node_id` envelope is rejected.

---

## 🟡 MEDIUM — Aggregator FFI handles drop the crate's use-after-free protection

- `src/ffi/aggregator.rs:163-169` (`net_registry_client_free`) and
  `:463-470` (`net_fold_query_client_free`) do an unconditional
  `drop(Box::from_raw(handle))`.
- Handle structs at `:131-134` and `:436-441` lack the `HandleGuard` embedded in
  every other opaque handle in the crate (`RedexHandle`, `MeshNodeHandle`, etc.,
  see `src/ffi/handle_guard.rs:1-46`), which leak-on-free and gate ops on
  `try_enter()` — the explicit fix for prior FFI UAF audits (#23/#24/#25).

Scenario: these handles' own docs (`aggregator.rs:124-125`) advertise
multi-threaded use. Thread A in `net_fold_query_client_query_latest` while
thread B calls `net_fold_query_client_free` on the same handle → `Box::from_raw`
deallocates the lock/client out from under A's in-flight `h.client.read()` →
use-after-free. Confidence: medium (requires caller to race free-against-op, a
contract the preamble discourages, but the docs invite the concurrency).

Fix: give `RegistryClientHandle` / `FoldQueryClientHandle` the same
`HandleGuard` + `ManuallyDrop` + leak-on-free treatment as the sibling handles,
and gate each op on `try_enter()`.

Related (LOW, `aggregator.rs:369-382`): `net_registry_last_error_detail` returns
a pointer into a `Mutex`-owned `CString` that a concurrent erroring op on
another thread can free (dangling read). Matches its documented single-threaded
contract but is reachable under the advertised multi-threaded usage.

---

## 🟢 LOW / informational

### Dir-transfer symlink target unvalidated (LOW, confidence high)
`src/adapter/net/dataforts/blob/dir.rs:548-555`, `:791-802`. On `fetch_dir`, the
symlink *link path* is sanitized via `safe_join` (rejects `..`/absolute/root),
but the symlink *target* is written verbatim from the attacker-controlled
manifest — the standard "symlink in archive" exposure (a peer can plant
`link -> /etc/passwd`). Cannot be turned into a traversal *write* (reconstruction
is strictly ordered: dirs, then files, then symlinks last; no file is written
*through* an attacker symlink), so residual risk is on whatever later reads the
reconstructed tree. Fix if desired: reject absolute targets / targets that
normalize outside the tree root, or gate symlink creation behind an opt-in flag.

### Non-constant-time comparison of secret group/subnet ids (LOW, low exploitability)
`behavior/group.rs` / `behavior/subnet.rs`. `GroupId` (32-byte) / `SubnetId`
(16-byte) are documented bearer secrets compared via derived `PartialEq` /
`Vec::contains` (early-exit, data-dependent timing) in
`capability_bridge.rs:340,363,368`. Remote timing recovery of a 128/256-bit
secret is impractical; flagged for completeness. Use a constant-time compare.

### `subscribe_caps` / `publish_caps` are self-asserted, not an access boundary (LOW, by-design — document it)
`src/adapter/net/channel/config.rs:164-201`. A peer's capabilities are whatever
it self-advertises under its own node_id, so a channel guarded only by a
cap-filter (e.g. `with_subscribe_caps("role:admin")`) is bypassable by any peer
self-advertising that cap. Acceptable **only** because the real boundary is
`require_token` + `token_roots` (root-anchored `TokenChain`, implemented
correctly). Recommend documenting prominently that cap-filters are advisory
matchmaking, not access control.

### Permissive `may_execute` default compounds the critical finding (LOW)
`capability_bridge.rs:337-339`: a target carrying a capability tag but with all
allow-lists empty is callable by anyone. On its own this is a documented
"open service" default, but combined with Finding 1 it widens the blast radius.
Recommend `serve_rpc` services default fail-closed (require an explicit
allow-list).

### Fuzz coverage gaps (LOW, defense-in-depth)
The fuzz suite covers 5 decoders (RoutingHeader, CapabilityAnnouncement,
migration `wire::decode`, natpmp, SnapshotReassembler). Several equally
attacker-reachable, manually-hardened decoders have **no** fuzz target, so their
bounds checks lack a continuous regression guard:
- `cortex/rpc.rs:513/628/708` — nRPC request/chunk/response decode
- `channel/membership.rs:193` — subscribe/unsubscribe/ack decode
- `subprotocol/stream_window.rs:110/159/194` — window/nack/reset
- `redex/replication.rs:432/507/596/667` — Sync{Request,Response,Heartbeat,Nack}
- `compute/bindings.rs:113` — subscription-ledger decode (migration target side)
- `dataforts/blob/transfer.rs` — `process_event` / `TransferHeader` decode
- `behavior/meshdb/protocol.rs:228` — `MeshDbFrame` postcard decode
- `state/snapshot.rs:368` — snapshot `from_bytes_v2`

Recommend adding fuzz targets mirroring the existing scaffold for at least
`cortex/rpc` decode, `membership::decode`, `bindings::from_bytes`, and
`blob::transfer::process_event`.

### Aggregator PSK in plaintext config, no permission check (INFO)
`aggregator-daemon/src/lib.rs:293,302` reads `psk_hex` from the operator TOML
with no file-permission check — unlike `cli/identity.rs`, which enforces `0600`
on its seed file. Operator-controlled path, but consider mirroring
`identity::check_strict_permissions` to warn on a world-readable config holding
the mesh PSK.

### Dependency CVE scan NOT performed (INFO)
`cargo audit` is not installed in this environment, so known-CVE dependency
scanning did not run. Install with `cargo install cargo-audit` and run
`cargo audit`.

---

## Dimensions reviewed and found clean (verified, not assumed)

- **Untrusted wire parsing / remote DoS** — every length-prefixed allocation is
  bounded before `with_capacity` (compute bindings/orchestrator, redex
  replication R-36 cap, cortex rpc, protocol event frames); offset arithmetic
  uses `checked_add` (replication L-9); no reachable unwrap/expect/index panic on
  malformed input in the sampled decoders; reassembly bombs capped (blob
  transfer, snapshot reassembler, meshdb 1 MiB frame cap); serde/postcard paths
  bounded by upstream frame-size caps; no small-request→large-response
  amplification (capability re-broadcast hop count capped at 16).
- **Token / chain auth** — `verify_strict` rejects ed25519 malleability; strict
  fail-closed expiry, re-checked on the publish hot path and a periodic sweep;
  root anchoring via `token_roots` + leaf-binding to the AEAD-verified presenter;
  delegation strictly narrows scope/channel/expiry/depth; `token_gate` fails
  closed when roots are empty or no chain is presented; no "alg=none"/unsigned
  branch.
- **Randomness / nonces** — `getrandom::fill` with `process::abort()` on failure
  (no weak fallback); AEAD tx counter atomic-monotonic, per-direction keys, no
  nonce reuse; replay window implemented and rejects `u64::MAX` edges.
- **Handshake identity binding** — Noise prologue binds `(src,dst)` node ids;
  legacy cap-announcement path TOFU-pins and rejects rebinds / forwarded pins;
  `require_signed_capabilities` defaults to true.
- **Secret hygiene** — no derived `Debug` on `EntityKeypair`; `SigningKey`
  zeroizes on drop; `SessionKeys` / `PermissionToken` Debug redacts secret
  material.
- **FFI memory safety** — every `slice::from_raw_parts` gates `len > isize::MAX`;
  raw-handle derefs gate on null + alignment; `NetHandle` uses an intentional
  box-leak + quiesce handshake; `CString` paths handle UTF-8 and interior-NUL
  errors; no `transmute`; `Send`/`Sync` impls sound; alloc/free layouts
  symmetric; panic-prone boundaries wrap in `catch_unwind`. (Exception: the
  aggregator handles above.)
- **Filesystem / path / injection** — RedEX channel→disk path validated by
  `ChannelName::validate` (charset allow-list, rejects `.`/`..`); blob storage
  paths keyed on BLAKE3 hash; blob read/write canonicalizes + confines to root;
  CLI identity seed written `create_new(true).mode(0o600)` atomically;
  `net-blob get --out` uses `create_new` (no clobber/symlink follow); only two
  `Command::new` sites, both fixed-arg (no shell, no interpolation); no
  arbitrary-graph deserialization of remote/lower-privilege-writable files.

---

## Priority

1. **Fix the critical fold finding** — add `ann.node_id == publisher.node_id()`
   binding in `verify`/`decode_and_verify` + the reservation path, with a
   regression test. Small, surgical, outsized impact.
2. Adopt `HandleGuard` for the two aggregator FFI handles.
3. Backlog: dir-transfer symlink target validation, constant-time secret
   compares, fuzz-target gaps, doc the self-asserted cap-filters, `cargo audit`
   in CI.
