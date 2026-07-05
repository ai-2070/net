# Code review — `hermes-plan` branch (embedded Hermes node + Phase-3 delegation/revocation)

**Date:** 2026-07-05
**Branch:** `hermes-plan`
**Base:** `master`
**Scope:** 49 files, +7,109 / −164 LOC (62 commits).
**Plans:** [`HERMES_INTEGRATION_PLAN.md`](../plans/HERMES_INTEGRATION_PLAN.md)

The branch lands the Hermes integration as a **first-class embedded Net node**
(no daemon, no `net mcp serve` shim in the middle) plus the **Phase-3 delegation
+ revocation** surface. Concretely:

- **`root → machine → gateway → subagent` delegation chains** for
  capability-invoke *attribution*, built on the existing `TokenChain`
  (`sdk/src/delegation.rs`), with a per-invoke signed challenge that defeats
  origin spoofing (`adapters/mcp/src/wrap/delegation.rs`).
- A **persistent, machine-shared revocation store** so an operator can revoke a
  delegated gateway without restarting the provider (`sdk/src/revocation.rs`,
  `net identity revoke`, `net wrap --owner-root/--revocation-store`).
- A **pin-change subscription** (`PinStore::watch`, OS file watcher) that
  promotes approved pins to first-class Hermes tools (`sdk/src/pins.rs`).
- **Native consent-gated `CapabilityGateway`** (sync + async) exposed to Python
  (`bindings/python/src/{capability_gateway,delegation,consent}.rs`), and the
  **pure-Python Hermes plugin** (`integrations/hermes/*`).
- A consolidation of the `describe → validate → consent → invoke` gate into one
  shared `gated_invoke` (`adapters/mcp/src/serve/gated.rs`) used by both the
  stdio shim and the native gateway.

---

## Overall assessment

**High-quality, defense-hardened code.** Extensive fail-closed tests, careful
atomic-write/fsync durability, length-prefixed domain-separated signing
challenges, and thoughtful concurrency (the pin-store lock-pool-starvation fix
is exactly right). The findings below cluster at the **caller-side integration
seam**, not the crypto core. Verified as correct (not findings):

- **The delegation gate is fail-closed and origin-spoof-resistant.**
  `DelegationGate::verify` (`wrap/delegation.rs:204`) parses before crypto,
  checks the freshness window, verifies the per-invoke leaf signature over a
  domain-separated length-prefixed challenge (`build_challenge`), then verifies
  the chain (root-anchor + continuity + scope + revocation) and only *then*
  touches the replay-nonce cache — so an unauthenticated peer can't grow it.
  Every failure path returns a typed rejection; no path falls through to admit.
- **The replay-nonce cache is keyed by `(leaf, nonce)`** so two distinct
  authenticated delegates can't false-replay each other, and it is hard-capped
  (`MAX_NONCES`) to fail-closed under a compromised-leaf flood rather than grow
  unbounded.
- **Revocation is monotonic and durable.** `RevocationStore` writes under a
  cross-process advisory lock on a stable sidecar via temp+fsync+rename, fsyncs
  the parent dir on Unix, and reads lock-free (a torn/missed read only lets one
  more invoke through, never resurrects access). Floors only ever rise.
- **The consent gate never trusts a wire status.** `gated_invoke`
  (`serve/gated.rs:74`) gates every capability whose `credential_status`
  requires approval unless an allowlist entry or an *approved* pin admits it; a
  broken pin store is passed as `None` (fail-closed), and a self-declared
  `"none"` still gates.
- **The pin store is integrity-safe.** Atomic `0o600`-from-creation save,
  fail-closed `Corrupt` on parse error (never a silent reset), and every
  read-modify-write goes through the locked `mutate` transaction. The
  lock-acquire polls `try_lock_exclusive` with async backoff (no blocking-pool
  starvation) — with a regression test.
- **The PyO3 surface is a disciplined thin delegation.** No re-implemented
  consent/delegation logic; H8 holds (no private key material crosses into
  Python — `derive_child_identity` derives inside Rust and returns an opaque
  handle); GIL released around blocking ops; the async gateway spawns on the
  mesh runtime with an `AbortOnDrop` cancel guard.
- **The provider is the enforcement point.** `net wrap --owner-root` wires a
  `DelegationGate` with `with_revocation_store`, which reloads the shared floors
  on every verify — so an operator revocation reaches a *running* provider.

---

## Findings

### F1 — [Medium] Caller-side delegation self-check never observes store-based revocation

**File:** `integrations/hermes/delegation.py:175` (and `node.py:190`, `node.py:234`)

`GatewayDelegation.__init__` builds a **fresh in-memory** `RevocationRegistry()`
that `verify()` consults, and nothing on the Python/caller side ever loads the
machine-shared `RevocationStore` file into a registry (confirmed: no
`apply_to` / `RevocationStore` reference anywhere under `integrations/hermes` or
`bindings/python`; the PyO3 `RevocationRegistry` exposes only
`revoke_below`/`revoke`/`floor`, no store-load).

Consequence: `node.check_net_available()` and `node.delegation_valid_for_invoke()`
— both of which gate on `delegation.verify()` — catch only **TTL expiry** (the
chain's own `not_after`), **not** an operator's `net identity revoke <machine>`
(which writes the JSON store). Only the in-process `revoke_gateway()` mutates
that registry.

But `node.py:190` claims *"a revoked or expired delegation removes the tools
rather than letting the model invoke under an invalid chain … never a silent
degrade,"* and `node.py:234-243` claims the invoke path *"never signs + sends
under an invalid chain … fails fast at the source."* The **revoked** half does
not hold — a store-revoked gateway keeps its tools model-visible and the model
attempts invokes.

**Security is intact** — the provider enforces via `DelegationGate::with_revocation_store`
(`wrap.rs:230`), and the plan (line 152) is explicit that *"the provider retains
authority; caller consent alone is never sufficient."* This is a **contract /
behavior mismatch + a missing caller-side fail-fast**, not a vulnerability.

**Fix options:**
- Have `verify()` (or a `refresh()` before it) load the shared
  `RevocationStore` floors into `self._registry` — i.e. expose a
  `RevocationRegistry.load_store(path)` (or `apply_store`) on the PyO3 surface
  and call it in `check_net_available` / `delegation_valid_for_invoke`; **or**
- Soften the `node.py` docstrings to state the caller-side check is
  **expiry-only** and that revocation is enforced provider-side, so the
  advertised contract matches behavior.

---

### F2 — [Low-Medium] `ERR_DELEGATION` is surfaced to the model as a tool-level error, not a denial

**File:** `adapters/mcp/src/serve/mesh_gateway.rs:363-382` (`invoke_on`)

`invoke_on` maps `ERR_OWNER_SCOPE` → `GatewayError::Denied`, but `ERR_DELEGATION`
(`0x8005`) falls through to the generic `ServerError` catch-all →
`Ok(CallToolResult::text_error("provider error 0x008005: …"))`.

So a delegation/revocation rejection reaches the model as
`{"status":"ok","is_error":true,"text":"provider error 0x008005: …"}` (both the
native path via `outcome_to_json` and the shim path), rather than
`{"status":"denied"}`. That is:

- **Inconsistent with its sibling owner-scope path** (`ERR_OWNER_SCOPE → Denied`).
  Delegation is the confused-deputy defense sibling of owner-scope and should
  surface identically.
- **Contradicts the documented `net_invoke` contract** (`tools.py:96`) that
  `is_error` means *"a tool-level failure reported by the remote tool itself."*
  A revoked gateway's invokes look like remote *tool bugs*, so the model may
  retry instead of requesting re-approval.

This **compounds F1**: when the provider rejects a store-revoked gateway
(`ERR_DELEGATION`), the model gets an opaque hex tool-error instead of a clean
denial.

**Fix:** add an arm in `invoke_on`:
```rust
Err(RpcError::ServerError { status, message }) if status == ERR_DELEGATION => {
    Err(GatewayError::Denied(message))
}
```
(`ERR_DELEGATION` is already re-exported from `wrap`; import it alongside the
other `ERR_*` codes.)

---

### F3 — [Low] `PinPromotionService.stop()` is not cleanly idempotent

**File:** `integrations/hermes/pins.py:249-264`

After a successful stop, `_thread` is cleared but `_loop` / `_task` still
reference the now-closed event loop. A second `stop()` call runs
`loop.call_soon_threadsafe(task.cancel)` on a **closed** loop → `RuntimeError`,
out of a path the `_on_session_end` docstring calls *"idempotent; swallows
errors."* Harmless in the normal flow (`__init__.py:59-61` nulls `_promotion`
after the first call), but the method's own contract isn't met.

**Fix:** clear `_loop` / `_task` on the success branch (where `_thread` is set
to `None`), or guard the `call_soon_threadsafe` on `not loop.is_closed()`.

---

## Minor nits (not worth blocking)

- **`check_and_record_nonce` prunes O(n) per verify** — `wrap/delegation.rs:303`:
  `cache.retain(...)` scans up to `MAX_NONCES` (100k) on every delegated invoke.
  Fine at expected volumes; just linear on the hot path if the cache ever fills.
  A future refinement could prune amortized (e.g. only when
  `len == cap`, or a bucketed expiry wheel).
- **Nonce-expiry vs timestamp-window boundary** — `wrap/delegation.rs:297` +
  `:237`: expiry is `now + 2*window` stamped at first-see, and the prune uses
  `exp > now` with inclusive window bounds, leaving a ~1s theoretical replay
  window *only* for an envelope whose `ts` is exactly `window` seconds in the
  future, replayed at the exact prune tick. Requires attacker-controlled clock
  skew of exactly `window` plus sub-second timing; practically unreachable.
  Noting for completeness.
- **Doc path inconsistency** — `cli/src/commands/identity.rs:6`: the module
  header says identity files live at `$XDG_CONFIG_HOME/net/identities/`, but
  `default_identity_path` (`identity.rs:487`) uses `net-mesh/identities`.
  Cosmetic.

---

## Recommendation

Ship-worthy. The Rust core (delegation crypto, revocation durability, replay
gate, consent composition, pin-store locking) and the PyO3 surface are excellent
— no correctness or safety defects found there. The two worth acting on before
merge are **F1** (align `node.py`'s revocation claim with reality) and **F2**
(map `ERR_DELEGATION → Denied`), which are cheap and reinforce each other. **F3**
and the nits are optional cleanup.
