# Code Review — `hermes-plan-2` vs `master` (2026-07-06)

**Branch:** `hermes-plan-2`
**Base:** `master` (merge-base `72aa147d6`)
**Scope:** 39 commits, ~10,548 insertions / 63 deletions across 49 files — Hermes V2
device enrollment (invite → join → approve → revoke + long-lived grants + silent
renewal + persistence), source-agnostic tool publication (`publish_tools` +
`ToolInvoker` + schema-by-content-hash), and A2A task handoff
(`TaskBrief`/`TaskRegistry`/cooperative cancellation), spanning the Rust SDK, the
MCP adapter, the PyO3 bindings, and the Hermes Python plugin.

**Method:** extra-high-effort recall pass — six per-subsystem finder agents
(enrollment SDK, devices/operator SDK, A2A SDK, MCP adapter, PyO3 bindings,
Hermes Python) + one gap sweep, each candidate re-verified against source.

**Overall:** the core crypto primitives (delegation derivation, single-use nonce
ledger, atomic-rename persistence with advisory locks, GIL discipline in the
invokers) are well-built and well-tested. Defects cluster where new code meets
**auth-boundary policy**, **cancellation/lifecycle**, and **FFI/persistence
glue**.

Line numbers are as of the branch tip at review time and may drift as fixes land.

---

## 🔴 High severity

### H-1. `renew` mints a fresh full-capability `root→device` grant from *any* valid chain — privilege escalation + revocation-hierarchy bypass
**`sdk/src/enrollment.rs:847`** (`EnrollmentAuthority::renew`)

`renew` accepts any presented chain that verifies (`request.root == root`,
self-signature valid, `floor(device) == 0`, `chain.verify(device, root, …)` OK)
and then calls `DelegationChain::derive_device(root, request.device, grant_ttl,
max_depth)`, which mints a **fresh single-link grant with `INVOKE ∪ DELEGATE`
and a full `max_depth` budget** (`delegation.rs:177`). It never checks that the
presented chain is a bare `root→device` grant (`chain.len() == 1`) or that
`request.device` is a registered device.

A deep leaf — e.g. an INVOKE-only, depth-exhausted `root→device→gateway→subagent`
subagent (its leaf minted by `extend_to_subagent`, which drops `DELEGATE`) — can
present its **full** chain with `request.device = subagent_id`. `verify` passes
(leaf == subagent, all links unrevoked), and `derive_device` promotes it to a
direct `root→subagent` grant *with* `DELEGATE` and fresh depth. The subagent
gains a capability it never had **and** is now anchored directly under root:
revoking its former parent (gateway) no longer cuts it off, defeating the
"revoke the gateway kills its subtree" guarantee.

**Reachable with no operator gate:** `serve_renewal_auto`
(`mesh_enroll.rs:215`) is the only renewal serve path and has no `approver` hook
(unlike join's `serve_enrollment` + `serve_enrollment_auto`), so any in-root
peer that can reach `RENEWAL_SERVICE` and present a valid chain triggers it.

**Fix:** enforce that the presented chain is exactly one link (`root→device`),
or that `request.device` appears in the device registry, before minting.

---

### H-2. Device private key seed written world-readable on Windows
**`sdk/src/enrollment.rs:1062`** (`DeviceEnrollment::save`)

`save` persists `device_seed` — the device's 32-byte ed25519 **private** seed as
lowercase hex (line 1048; field doc at 958 says "**secret**") — and both the fn
doc (line 1035) and the type doc (line 918) claim it is written `0600`. But the
`opts.mode(0o600)` call is inside `#[cfg(unix)]` (lines 1062–1065). On the
documented Windows target the file (`enrollment.json`) is created with default
inherited ACLs.

Another local user on a shared Windows box can read `device_seed`, reconstruct
the device `Identity` via `Identity::from_bytes`, and impersonate the enrolled
device. This is the only store that persists key material; the gap defeats the
H8 "seed-confinement at rest" claim on Windows.

**Fix:** add a `#[cfg(windows)]` branch that applies a restrictive ACL
(owner-only) to the temp file before the rename (e.g. via a Windows ACL crate or
an `icacls`-equivalent), and correct the docs to state the Windows behavior.

---

### H-3. `publish_tools` default `owner_origin = None` → `OwnerScope::any()` (fail-open)
**`bindings/python/src/publish.rs:141`**

When `owner_origin` is omitted (the `lib.rs` binding default is `None`), the code
sets `config.scope = OwnerScope::any()` — every mesh peer may invoke the
published tools, which are backed by an **arbitrary Python callback**
(`PyToolInvoker`, capable of shell/file access). A caller who simply doesn't pass
`owner_origin` silently exposes local execution mesh-wide.

The docstring frames `None` as "in-root/testing" and defers the delegation gate
"to a follow-up," and the Hermes provider (`provider.py`) adds its own approval
routing on top — but the *binding default* should fail closed, not open.

**Fix:** default to an owner-only / deny-all scope; require an explicit opt-in
for `any()`.

---

### H-4. A2A cancel can drop the coroutine future before its guard is armed → zombie remote task, false `Cancelled`
**`bindings/python/src/a2a.rs:55`** (`PyTaskExecutor::run`)

`dispatch_handler_coro` submits the Python coroutine **synchronously** via
`asyncio.run_coroutine_threadsafe` (`async_bridge.rs:199`), but the
`CoroCancelGuard` that cancels it on drop is constructed only inside the returned
future's async block — i.e. on its **first poll** (`async_bridge.rs:226`).

In `run`, the returned `fut` is wrapped in `tokio::select!` against
`cancel.cancelled()`. If the cancel token is already tripped when the select
first runs (a real window: the token can trip any time after `registry.submit`
returns, while `run` is still doing GIL work to dispatch the coroutine) and
tokio's randomized branch order polls the cancel branch first, `fut` is dropped
**without ever being polled** → the guard is never constructed → `cf_future.cancel()`
never fires → the Python coroutine runs to completion as a zombie. Meanwhile the
SDK registry records `Cancelled` (because `cancel.is_cancelled()` is true), so
the requester falsely believes the remote work stopped.

The inline comment at `a2a.rs:52-53` asserts the opposite ("this branch wins and
`fut` drops — the guard cancels the Python coroutine").

**Fix:** arm the guard eagerly (construct `CoroCancelGuard` before returning the
future, outside the async block), or make the `select!` `biased;` and order
`fut` first so it is always polled at least once.

---

### H-5. `TaskRegistry::submit` never races the executor against cancel — contradicts the documented guarantee
**`sdk/src/a2a.rs:300`** (`TaskRegistry::submit` spawned task)

`CancelToken`'s doc (`a2a.rs:193-195`) explicitly promises: *"A non-cooperative
executor's future is dropped by the registry's `select!`, which also stops it."*
No such `select!` exists — the spawned task does `let r = executor.run(brief,
cancel.clone()).await;` directly.

A non-cooperative or hung executor (one that ignores the token) is therefore
never stopped: `cancel_task` trips the token and returns `true`, but the task
stays `Running` forever and both the tokio task and the registry entry leak.
Cancellation only works for *cooperative* executors that poll the token
themselves. The module's headline "demonstrably stops" guarantee holds only for
cooperative executors.

**Fix:** wrap the `run` future in `select!` against `cancel.cancelled()` (drop it
on cancel), or downgrade the doc to state cancellation is cooperative-only.

---

### H-6. `into_chain` verifies the grant with zero clock skew → a clock-lagging device rejects a valid grant
**`sdk/src/enrollment.rs:703`** (`JoinOutcome::into_chain`)

`chain.verify(device, invite_root, &reg, 0)` hardcodes `skew_secs = 0`. The
operator mints the grant with `not_before = operator_now`; a device whose wall
clock lags by a few seconds (routine on containerized / edge fleets) computes
`device_now < not_before` → `NotYetValid` → `verify` errors → `into_chain`
returns `JoinError::UntrustedGrant`. A legitimate enrollment fails and is
misreported as "the operator returned a grant for a different mesh/device."

The codebase ships `TOKEN_CLOCK_SKEW_SECS_RECOMMENDED` (=60) for exactly this.
`mesh_enroll.rs` `join` and `renew` both call `into_chain` with skew 0 and
inherit the bug.

**Fix:** pass the recommended skew to `verify`.

---

## 🟠 Medium severity

### M-1. Provider approval fails open on a truthy non-`bool` decision
**`integrations/hermes/provider.py:136`** (`LocalToolProvider._callback`)

`_callback` denies on `decision is None` (line 126) and `not decision` (line
136), then proceeds. The production `approve` adapter (`provider.py:271`) returns
`request_operator_approval(name, args)` verbatim with no bool coercion. If that
surface returns a truthy non-bool — a `{'decision': 'deny'}` dict, a `'no'`
string — both guards are False and the mesh-originated tool **runs despite the
operator declining**. The contract is `Optional[bool]`; anything unexpected must
deny.

**Fix:** `if decision is not True: <deny>` (or coerce/validate to strict bool in
the adapter).

### M-2. Federation loop: `_OWN_TOOLSETS` omits the federated toolset
**`integrations/hermes/provider.py:164`**

`_OWN_TOOLSETS = frozenset({"net", "net-pinned"})`, but federated capabilities
live in `FEDERATED_TOOLSET = "net-federated"` (`federate.py:41`). The local-tool
provider's `list_tools` (line 236) skips only `_OWN_TOOLSETS`, so it
re-publishes *other machines'* federated proxy tools (e.g.
`net_mesh__b__terminal_run`) as **this** node's own mesh capability. Peers then
re-federate them under this node's identity
(`net_mesh__a__net_mesh__b__…`) — the exact loop the code comment says it
prevents. Requires both `NET_MESH_PUBLISH_LOCAL_TOOLS` and
`NET_MESH_FEDERATE_TOOLS`.

**Fix:** add `"net-federated"` to `_OWN_TOOLSETS`.

### M-3. `approve_at` spends the single-use nonce before persisting — a record failure permanently bricks the invite
**`sdk/src/operator.rs:172`** (`OperatorEnrollment::approve_at`)

`authority.approve` burns the nonce and mints the chain, *then*
`DeviceRegistry::record(...)?` runs. If `record` errors (disk full, permissions),
`?` returns before `pending.remove` (line 182): the nonce is spent in the
authority ledger, but the invite lingers in `pending`. The device's retry finds
the invite but `authority.approve` now returns `Replay` → permanently rejected,
and `pending_invites()` keeps advertising the dead invite until TTL. (`approve_with`
at line 231/240 has the same ordering.)

**Fix:** make the nonce-spend and the record commit atomic, or record before
spending / roll back the nonce on a record failure.

### M-4. Executor panic strands the task in `Running` permanently
**`sdk/src/a2a.rs:300`** (`TaskRegistry::submit` spawned task)

If `executor.run(...).await` panics, the spawned task unwinds; `set_state(&inner,
&id_run, final_state)` (line 309) never runs and there is no `catch_unwind`. The
entry is stuck non-terminal, `cancel` returns `true` but nothing transitions,
`task_status`/`wait_terminal` poll `Running` forever, and the entry leaks.

**Fix:** wrap `run` in `catch_unwind` (or use a drop-guard) that marks the task
`Failed` if the executor future does not resolve normally.

### M-5. First-run enrollment commits the join before persisting → strands the device and loses its key
**`integrations/hermes/node.py:337`**

`mesh_handle.join(...)` burns the single-use invite and gets the device admitted,
then `enrollment.save(path)` runs; a transient save error is swallowed by the
surrounding `try/except`. On next start, `DeviceEnrollment.load` returns `None`,
the same spent `NET_MESH_INVITE` is replayed → `Replay` rejection forever, and
each run generates a fresh `Identity`, so the admitted key is discarded. (Same
class as M-3, at the Hermes node layer.)

**Fix:** persist before/atomically-with the join commit, or surface the save
failure and retry rather than swallowing it.

### M-6. A gateway search error retires *all* federated tools
**`integrations/hermes/federate.py:288`** (`start_federation._search`)

`_search` returns `[]` whenever `status != "ok"`, so a transient gateway/fold
error is indistinguishable from a genuinely empty mesh.
`FederationPromoter.reconcile([])` then deregisters every currently-registered
federated tool; the next ~30s poll re-describes and re-promotes them — tool
flicker plus a full describe-storm across all federated caps on every transient
blip.

**Fix:** skip `reconcile` on an errored search (leave the current set intact);
only reconcile against a successful (`status == "ok"`) result.

### M-7. Re-enrolling a floor-revoked device flips inventory back to "active" while enforcement still denies
**`sdk/src/operator.rs:174`** (`approve_at`; also `renew` at 377)

`approve_at`/`renew` re-record via `DeviceRecord::new`, which unconditionally
sets `revoked_at: None`, but neither resets the `RevocationStore` floor. A device
revoked (floor 1, `revoked_at = T`) that re-joins with a fresh invite shows as
active/healthy in `mesh.devices()` while its floor is still 1 — the device list
contradicts enforcement, and an operator may believe the revocation was undone
when it was not.

**Fix:** on re-record, carry forward the existing `revoked_at`/floor, or reset
the floor deliberately as part of re-admission.

### M-8. Store-path override honored only when *both* env vars are set → silently writes production inventory
**`integrations/hermes/node.py:383`** (`_build_operator`)

The device-store / revocation-store overrides are used only under `if dev and
rev`. Setting only `NET_MESH_DEVICE_STORE` (expecting isolation, e.g. in a test)
silently falls back to `with_default_paths`, writing device records and
revocations to the real machine-shared `~/net-mesh/{devices,revocations}.json` —
corrupting or leaking the production inventory the comment claims tests avoid.

**Fix:** honor each override independently, or fail loudly if only one is set.

### M-9. `revoke_at(device, 0, now)` stamps "revoked" but leaves the device fully authorized
**`sdk/src/operator.rs:257`** (`revoke_at`; `revocation.rs:178`)

`RevocationStore::revoke_below` raises the floor only `if generation > *entry`
(`revocation.rs:178`), so `generation = 0` is a no-op on the floor while
`mark_revoked` still stamps `revoked_at`. The current grant *is* generation 0, so
an off-by-one caller passing 0 (an easy mistake — killing the current grant
requires floor 1 = `DEFAULT_REVOKE_GENERATION`) gets a device that shows
"revoked" in the inventory yet still verifies and can silently auto-renew a fresh
grant. `revoke_at` is `pub` with an "explicit floor generation" doc.

**Fix:** reject `generation == 0` (or clamp to `DEFAULT_REVOKE_GENERATION`).

---

## 🟡 Lower severity / latent

- **Registry never auto-evicts terminal tasks** — `sdk/src/a2a.rs:361`. Only
  manual `forget()` removes an entry; a long-lived executor's `HashMap` grows
  without bound over its lifetime. Consider TTL-based eviction of terminal tasks.
- **Duplicate `submit` (nRPC retransmit) overwrites the Entry** —
  `sdk/src/a2a.rs:287`. A retried submit with the same `task_id` replaces the
  Entry (new cancel token) and spawns a *second* executor; the first keeps
  running uncancellable, and both race on `set_state`. Guard against re-insert of
  a known id.
- **`RenewalRequest` carries no nonce/timestamp** — `sdk/src/enrollment.rs:847`.
  The renewal message is replayable (impact bounded: the reissued grant binds to
  the device key the replayer lacks), unlike the join path which has single-use
  freshness. Add an anti-replay binding.
- **Silent-renewal loop has no interval floor** —
  `integrations/hermes/renewal.py:130`. `NET_MESH_RENEWAL_INTERVAL=0`/negative →
  `_stop.wait(0)` returns instantly → 100%-CPU busy-loop (`_env_int` guards only
  `ValueError`, not `<= 0`). Clamp to a sane minimum.
- **`renew` re-records a `forget()`-pruned device** — `sdk/src/operator.rs:377`.
  A device pruned via `forget` (floor left at 0) is silently resurrected as an
  active inventory record on its next silent renewal (`existing.get` → `None` →
  `unwrap_or_default` → `record`).
- **`_on_session_end` has no per-call guard** —
  `integrations/hermes/__init__.py:131`. One service `.stop()` raising skips the
  rest of teardown, including `node.shutdown()`, leaking the mesh node + the
  silent-renewal daemon thread + served RPC handles. Wrap each teardown step.
- **`DeviceEnrollment::save` temp file is PID-only** —
  `sdk/src/enrollment.rs:1057`. `enrollment.tmp.<pid>` with no lock; two
  concurrent saves in one process (renewal loop + a manual renew) collide,
  risking a torn/missing file. `DeviceRegistry` locks; this store does not.
- **Published tool handler must return a strict `bool`** —
  `bindings/python/src/publish.rs:73`. `(text, 1)` / `(text, 0)` int-as-bool
  (idiomatic Python) is rejected and surfaced as a *transport* error, dropping
  both the tool's `is_error` flag and its text (misclassified as a transport
  failure).
- **`context_refs` isn't list-checked** — `integrations/hermes/tools.py:520`. A
  bare-string value is iterated into per-character refs
  (`[str(r) for r in "artifact://…"]`). Normalize a string to a single-element
  list.
- **MCP adapter (latent / efficiency):**
  - `InvokePolicy` runs before `parse_arguments` (`wrap/invoke.rs:312`) — a real
    approval policy would prompt the operator for structurally-invalid calls that
    can never execute. Harmless under the allow-all preset.
  - `descriptor_fingerprint` hashes order-sensitive `input_schema.to_string()`
    (`serve/grouping.rs:107`) instead of the new order-invariant `schema_hash`;
    if `serde_json`'s `preserve_order` is ever unified in, substitutable
    providers with different key order fail to collapse.
  - `canonicalize_json` recurses into arrays but never sorts them
    (`wrap/descriptor.rs:131`), so set-like schema arrays (`required: ["a","b"]`
    vs `["b","a"]`) hash differently — a dedup miss (not a correctness collision).
- **Informational:**
  - Root fingerprint is truncated to 64 bits (`sdk/src/enrollment.rs:167`); the
    evil-twin eyeball check is only 64-bit preimage-strong (documented tradeoff).
  - `serde_json::to_string` failure fabricates `"{}"` args
    (`bindings/python/src/publish.rs:49`) — near-infallible, but a silent swallow.
  - `OperatorEnrollment::pending` is an unbounded in-memory map pruned only on
    access (`operator.rs`, `invite_at`), unlike the authority's capped ledger.

---

## Recommended gate before merge

- **H-1** (renew escalation) and **H-2** (Windows key exposure) are auth-boundary
  defects on the headline feature — block on these.
- **H-4 / H-5** (A2A cancel) undercut the "demonstrably stops" cancellation
  guarantee the plan advertises.
- **H-3 / H-6** and the medium set are fixable follow-ups; the lower tier can be
  triaged.
