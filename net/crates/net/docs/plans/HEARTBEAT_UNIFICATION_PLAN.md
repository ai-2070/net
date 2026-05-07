# Heartbeat Unification Plan — 2026-04-30 (revised)

The same primitive — "AEAD-tagged keep-alive packet for a session" — used to have three independent implementations in the tree, with the same wire shape but inconsistent verification logic. Audit findings #11, #85, and #97 were all symptoms.

**Update (post-implementation):** the security-critical correctness bugs have been fixed. Heartbeats are now built and verified consistently on the wire. The remaining unification work is cleanup — removing duplicated code paths and locking the API door so the bugs cannot recur. This document tracks what changed vs. the original plan and what's still open.

## Current state (post-fix)

Send side, both call sites identical:

```rust
let mut pooled = session.thread_local_pool().get();
let packet = pooled.build_heartbeat();
```

- Legacy: `adapter/net/mod.rs:888-889` (in `spawn_heartbeat`).
- Mesh: `adapter/net/mesh.rs:3380-3381` (in `spawn_heartbeat_loop`).

Both now use the session's `thread_local_pool` — the same pool the data path uses, so heartbeats and data share a single TX counter. This closes both the wrong-key half of #97 and the counter-conflict variant: a fresh `PacketBuilder::new(&[0u8;32], ...)` builder owned its own counter starting at 0, so successive heartbeats reused counter=0 and the receiver replay-rejected every heartbeat after the first. Pinned by `aead_authenticated_heartbeat_passes_verification` (`mesh.rs:7407`) and the `process_packet` heartbeat suite (`mod.rs:1786`).

Receive side, two implementations of the same logic:

| Path | Location | Shape |
|---|---|---|
| Legacy `NetAdapter` | `mod.rs:659-680` (22 lines, inline) | full AEAD verify: source check → counter validity → decrypt → counter commit → touch |
| Mesh `MeshNode` | `mesh.rs:2500-2522` calls free fn `verify_heartbeat_aead(&parsed, &session)` at `mesh.rs:760-771` | same logic minus the source check (handled upstream by session lookup) and minus the touch (caller does it explicitly so `failure_detector.heartbeat` runs first) |

Both pass identical regression tests: `unauthenticated_heartbeat_fails_verification` (`mesh.rs:7424`) and the `process_packet` heartbeat suite. **The bugs are closed.** What remains is two implementations that should be one.

`Session` API surface (`adapter/net/session.rs`) post-fix:
- `rx_cipher()` exposed at line 201.
- `thread_local_pool()` exposed at line 622.
- `tx_cipher()` **removed** — no longer in the public API.
- `packet_pool()` getter **removed** — there is now exactly one pool reachable per session.

The two latent footguns behind #106 (parallel pools, parallel ciphers) are gone. The remaining heartbeat unification is purely cosmetic: collapse the two-line build pattern into one method and the duplicated verify into one call.

## Outstanding work

The four steps below are *cleanup* — they do not fix new bugs, but they remove ~30 lines of duplication and prevent regression.

### Step 1 — Replace `verify_heartbeat_aead` (predicate) with `NetSession::verify_and_touch_heartbeat` (mutating method)

The current shape — a free `verify_heartbeat_aead(&parsed, &session) -> bool` predicate where each caller is then responsible for calling `session.touch()` — has a footgun: a caller can verify and forget to touch (or touch before verify completes, or touch on a failed verify). The type system doesn't prevent it. Both current callers happen to do it correctly, but a future third caller has no structural guarantee.

Move to a mutating method on `NetSession` that fuses verify + touch atomically:

```rust
impl NetSession {
    /// Verify an inbound heartbeat's AEAD tag against this session's
    /// RX cipher, commit the counter into the replay window, and
    /// refresh `last_activity`. Returns `true` if the packet was
    /// accepted; the session is mutated only on success.
    ///
    /// Source-address validation and any failure-detector observation
    /// remain the caller's responsibility — those policies vary by
    /// adapter (legacy has 1:1 source/session; mesh has
    /// session-id-keyed lookup) and don't belong inside the helper.
    pub fn verify_and_touch_heartbeat(&self, parsed: &ParsedPacket) -> bool {
        let aad = parsed.header.aad();
        let counter = u64::from_le_bytes(
            parsed.header.nonce[4..12].try_into().unwrap_or([0u8; 8])
        );
        if !self.rx_cipher.is_valid_rx_counter(counter) { return false; }
        if self.rx_cipher.decrypt(counter, &aad, &parsed.payload).is_err() {
            return false;
        }
        if !self.rx_cipher.update_rx_counter(counter) { return false; }
        self.touch();
        true
    }
}
```

After the move, both receive paths collapse:

```rust
// legacy — mod.rs:659-680
if parsed.header.flags.is_heartbeat() {
    if source != session.peer_addr() { return; }
    if !session.verify_and_touch_heartbeat(&parsed) { return; }
    return;
}

// mesh — mesh.rs:2500-2522
if parsed.header.flags.is_heartbeat() {
    if !session.verify_and_touch_heartbeat(&parsed) { return; }
    failure_detector.heartbeat(peer_node_id, source);
    return;
}
```

The legacy path drops from 22 lines to 4. Mesh drops the inline `session.touch()`. The two genuinely-different parts (source check / failure detector) remain at the call sites where they belong.

Delete `verify_heartbeat_aead` (`mesh.rs:760-771`) and its tests' usages migrate to the new method.

### Step 2 — Wrap the build-side two-liner in `NetSession::build_heartbeat()`

Both call sites do:
```rust
let mut pooled = session.thread_local_pool().get();
let packet = pooled.build_heartbeat();
```

Replace with:
```rust
impl NetSession {
    pub fn build_heartbeat(&self) -> Bytes {
        self.thread_local_pool().get().build_heartbeat()
    }
}
```

Each call site becomes `let packet = session.build_heartbeat();`. Two lines saved per site, but more importantly the next person who needs to build a heartbeat doesn't have to know about `thread_local_pool` — and won't accidentally reach for `PacketBuilder::new(&[0u8; 32], ...)` again.

### Step 3 — Demote `PacketBuilder::new` from `pub` to `pub(crate)`

After Steps 1–2, `PacketBuilder::new` has no remaining heartbeat callers in production. The remaining `PacketBuilder::new(&[0u8; 32], 0)` call sites are all handshake builders (e.g. `mod.rs:484, 596`; `mesh.rs:2637`) — handshakes don't have a session key yet, so the zero-key construction is correct there. Those handshake call sites are all inside `adapter/net/`, so demoting `pub` → `pub(crate)` does not break them.

This is the structural guarantee that #97-shape bugs cannot recur from outside the crate. Verify nothing in `bindings/`, `sdk/`, `sdk-py/`, `sdk-ts/`, or `cli/` calls `PacketBuilder::new` directly before flipping the modifier.

### Step 4 — Static check

Add a CI grep (or a `compile_fail` test) asserting that no `*.rs` file outside `adapter/net/` constructs a `PacketBuilder::new` and that no file outside `session.rs` and `pool.rs` calls `build_heartbeat()` on anything other than the session helper. Catches future drift cheaply.

## What's already done

- ✅ Send-side bug (#97): both call sites route through `session.thread_local_pool()`. Heartbeat counter is shared with the data path.
- ✅ Receive-side bug (#85): mesh dispatch verifies AEAD before touching `failure_detector` or session state.
- ✅ Dual-pool / dual-cipher footgun (#106 surface): `Session::tx_cipher()` and `Session::packet_pool()` removed.
- ✅ Regression tests: `aead_authenticated_heartbeat_passes_verification`, `unauthenticated_heartbeat_fails_verification` (mesh side); the `process_packet` heartbeat suite at `mod.rs:1786` covering legitimate / no-tag / garbage-tag / `session.touch()` invariants.

## What's deliberately not done

- **#56** (JetStream cross-process nonce) — different layer.
- **Subnet-gateway heartbeat handling** — gateways don't process heartbeats today; if that changes, the new code must use `NetSession::verify_and_touch_heartbeat` rather than re-rolling the verify+touch sequence.

## Risks

- **`pub` → `pub(crate)` on `PacketBuilder::new`.** Any external caller breaks. The constructor is not in the documented SDK surface, but the bindings (`bindings/`, `sdk*/`) need a quick grep before flipping.
- **Replacing `verify_heartbeat_aead` with a mutating method.** Tests at `mesh.rs:7407, 7424, 7676, 7682, 7698, 7702` currently call the predicate and assert its return value. After Step 1 the equivalent assertions read `assert!(session.verify_and_touch_heartbeat(&parsed))` — but they now also need to assert `last_activity` advanced (round-trip) or did NOT advance (forged) since touch is fused into the call. The existing legacy-side test at `mod.rs:1786` already covers exactly that pattern, so the mesh-side tests can mirror it.

## Acceptance criteria for the remaining work

- Both receive paths call `session.verify_and_touch_heartbeat(&parsed)` — verify + touch fused, no caller can forget one half.
- Both build sites read `let packet = session.build_heartbeat();` (single line each).
- `PacketBuilder::new` is `pub(crate)`.
- `verify_heartbeat_aead` (free function in `mesh.rs`) is gone.
- Mesh-side tests assert `last_activity` advances on accept and stays put on reject (mirrors existing legacy-side coverage at `mod.rs:1786`); other regression tests for #85 and #97 continue to pass.

## Status

| Step | State |
|---|---|
| Send-side fix (#97) — route through `thread_local_pool` | ✅ done |
| Mesh receive-side fix (#85) — AEAD verify before touching session | ✅ done |
| Drop `Session::tx_cipher` / `Session::packet_pool` getters | ✅ done |
| Regression tests for #85 and #97 | ✅ done |
| 1 — `NetSession::verify_and_touch_heartbeat` (verify + touch fused), both receive sites ported, free `verify_heartbeat_aead` deleted | ✅ done |
| 2 — `NetSession::build_heartbeat()` wrapper, both build sites ported | ✅ done |
| 3 — `PacketBuilder::new` → `pub(crate)` | ✅ done |
| 4 — Static drift-check tests (`heartbeat_api_drift_check`) | ✅ done |

Closed by the implementation: **#11**, **#85**, **#97**, surface area for **#106**.

## Final shape (post-implementation)

**`NetSession` (`adapter/net/session.rs`)** owns the heartbeat primitive:
- `build_heartbeat() -> Bytes` — routes through `thread_local_pool` so heartbeats share a TX counter with data-path packets.
- `verify_and_touch_heartbeat(&ParsedPacket) -> bool` — fuses AEAD verify + counter commit + `touch()` so a caller cannot observe a heartbeat without verifying it, or touch a session whose verify failed.

**Production callers** (one line each):
- Legacy adapter send: `mod.rs:863` — `let packet = session.build_heartbeat();`
- Legacy adapter receive: `mod.rs:660-662` — source check, then `session.verify_and_touch_heartbeat(&parsed)`.
- Mesh send: `mesh.rs:3345` — `let packet = session.build_heartbeat();`
- Mesh receive: `mesh.rs:2487-2491` — `session.verify_and_touch_heartbeat(&parsed)`, then `failure_detector.heartbeat(...)` on success.

**Locked-down surface:**
- `PacketBuilder::new` is `pub(crate)` — external callers cannot construct a builder with a raw key. Verified: no callers exist outside `adapter/net/` (grep across `sdk/`, `bindings/`, `tests/`, `examples/`).
- `verify_heartbeat_aead` (free function) is deleted. The single AEAD-verify implementation lives on `NetSession`.

**Drift tripwires** (`adapter::net::session::heartbeat_api_drift_check`):
- `mod_rs_production_callers_match_allowlist`: scans the production prefix of `mod.rs` and asserts the only `.build_heartbeat(` call is `let packet = session.build_heartbeat();`.
- `mesh_rs_production_callers_match_allowlist`: same for `mesh.rs`. A future contributor adding a new production caller has to update the allowlist deliberately, forcing review of the design.

**Test coverage:** 11 heartbeat-related tests, all green:
- `mod.rs::tests::heartbeat_is_aead_authenticated` — legitimate / no-tag / garbage-tag verify outcomes (legacy path).
- `mesh::heartbeat_aead_tests::aead_authenticated_heartbeat_passes_verification_and_touches_session` — verify+touch fusion on success.
- `mesh::heartbeat_aead_tests::unauthenticated_heartbeat_fails_verification_and_does_not_touch` — verify+touch fusion on failure (the structural guarantee).
- `mesh::heartbeat_aead_tests::pooled_heartbeat_builds_succeed_in_sequence_and_verify` — back-to-back builds via `Session::build_heartbeat` produce verifiable packets with monotonic counters.
- `mesh::heartbeat_aead_tests::replay_of_authenticated_heartbeat_fails_verification_on_second_try` — replay rejected by counter window.
- `mesh::heartbeat_aead_tests::heartbeat_and_data_share_tx_counter_strictly_monotonic` — interleaves heartbeat / data / heartbeat / data / heartbeat builds and asserts strictly-increasing TX counters across all five. Pins the BUG #106 invariant: a future re-introduction of an independent per-purpose pool/counter would fail this test because both sequences would restart at 0.
- `session::heartbeat_api_drift_check::*` — the two drift tripwires.
