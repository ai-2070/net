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

## Resolution (2026-07-07)

All six high-severity findings, all nine medium-severity findings, and the
actionable lower-severity items are **fixed on this branch**, each in its own
commit with a regression test (`12dc46573..1dcbd3db7`). Per-finding status is
annotated inline below (`> **Fixed** (commit)`). The three **informational**
notes at the bottom are documented tradeoffs and were deliberately left
unchanged. Everything in the "Recommended gate before merge" list is resolved.

---

## 🔴 High severity

### H-1. `renew` mints a fresh full-capability `root→device` grant from *any* valid chain — privilege escalation + revocation-hierarchy bypass
**`sdk/src/enrollment.rs:847`** (`EnrollmentAuthority::renew`)

> **Fixed** (`12dc46573`): `renew` now refuses any chain longer than the bare
> `root→device` link (`Unrenewable`); gateway/subagent chains can no longer be
> promoted. Test covers both extended shapes plus the still-renewable bare grant.

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

> **Fixed** (`407e53e09`): a `#[cfg(windows)]` step strips ACL inheritance and
> grants only the current user (`icacls /inheritance:r /grant:r`) on the temp
> file before the rename, failing closed (save aborts) if the ACL can't be
> applied; docs state the per-platform behavior. Windows branch is
> hand-verified (no Windows target available in this environment).

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

> **Fixed** (`1d275ac67`): `owner_origin=None` now scopes to the publishing
> node's own `origin_hash` (fail closed); `OwnerScope::any()` requires the new
> explicit `allow_any_caller=True` opt-in. The Hermes provider opts in
> explicitly (its operator-approval gate still governs dangerous invokes).
> New binding test proves a remote caller is denied under the default.

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

> **Fixed** (`eda347255`): `CoroCancelGuard` is constructed and armed at
> dispatch (before the async block), so dropping the future — polled or not —
> cancels the coroutine; the a2a `select!` is `biased` with the executor first
> so an already-arrived result beats a simultaneous cancel. Immediate-cancel
> binding test asserts no zombie completion behind a reported cancel.

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

> **Fixed** (`5e020792a`): the spawned task now `select!`s the executor future
> against `cancel.cancelled()` (biased toward the executor), dropping a
> non-cooperative executor when the token trips — the documented guarantee
> holds. Test proves a token-ignoring executor is dropped and transitions to
> `Cancelled` promptly.

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

> **Fixed** (`aebc06fb7`): verification passes
> `TOKEN_CLOCK_SKEW_SECS_RECOMMENDED` (60s, newly re-exported through
> `net_sdk::identity`); `mesh_enroll` join/renew inherit it. Test re-stamps a
> grant 30s into the future (accepted) and 10min (still refused).

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

> **Fixed** (`38c6b74a6`): `if decision is not True:` denies — anything but an
> explicit `True` fails closed. Parametrized test covers dict/str/int/list/object.

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

> **Fixed** (`14beb3ef9`): `_OWN_TOOLSETS` now includes
> `federate.FEDERATED_TOOLSET` (imported, so the two can't drift). Test drives
> the production `list_tools` seam against a stub Hermes registry.

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

> **Fixed** (`47b386bdc`): a failed post-approve commit rolls the nonce back
> (new `EnrollmentAuthority::unspend_nonce`), keeping the invite redeemable for
> the retry; `approve_with` mirrors it. Test sabotages the registry path and
> proves the same request approves once the store recovers.

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

> **Fixed** (`d5ca88b4e`): a drop-guard armed across the executor await records
> `Failed { "executor panicked" }` (or `Cancelled` when the token tripped) on
> unwind. Test drives a panicking executor to `Failed` and forgets the entry.

If `executor.run(...).await` panics, the spawned task unwinds; `set_state(&inner,
&id_run, final_state)` (line 309) never runs and there is no `catch_unwind`. The
entry is stuck non-terminal, `cancel` returns `true` but nothing transitions,
`task_status`/`wait_terminal` poll `Running` forever, and the entry leaks.

**Fix:** wrap `run` in `catch_unwind` (or use a drop-guard) that marks the task
`Failed` if the executor future does not resolve normally.

### M-5. First-run enrollment commits the join before persisting → strands the device and loses its key
**`integrations/hermes/node.py:337`**

> **Fixed** (`834fce73a`): a write probe proves the enrollment path is writable
> *before* the join burns the invite; a post-join save failure is retried, then
> surfaced at ERROR while the session continues on the in-memory enrollment
> (the renewal loop re-attempts persistence). Tests cover pre-join abort
> (invite never spent) and the in-memory continuation.

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

> **Fixed** (`0d10bcacf`): `parse_search_result` maps an errored search to
> `None` (distinct from ok-but-empty `[]`) and `_poll_once` skips reconcile on
> `None`, leaving the current set intact. Tests cover the `None`/`[]`
> distinction and that an errored poll retires nothing.

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

> **Fixed** (`150b79b5e`): re-records carry the existing `revoked_at` forward
> (and re-stamp from a raised floor when the old record was pruned) — the
> inventory keeps matching enforcement. Deliberate re-admission of a revoked
> device needs a floor-aware re-issue surface (documented follow-up).

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

> **Fixed** (`43b48bbd2`): exactly one override now raises a `RuntimeError`
> naming the missing var (honoring half would split the inventory across an
> override and the production default). Parametrized test covers both halves.

The device-store / revocation-store overrides are used only under `if dev and
rev`. Setting only `NET_MESH_DEVICE_STORE` (expecting isolation, e.g. in a test)
silently falls back to `with_default_paths`, writing device records and
revocations to the real machine-shared `~/net-mesh/{devices,revocations}.json` —
corrupting or leaking the production inventory the comment claims tests avoid.

**Fix:** honor each override independently, or fail loudly if only one is set.

### M-9. `revoke_at(device, 0, now)` stamps "revoked" but leaves the device fully authorized
**`sdk/src/operator.rs:257`** (`revoke_at`; `revocation.rs:178`)

> **Fixed** (`5220b2222`): `generation == 0` returns the new
> `OperatorError::NoOpRevocation` before touching either store. Test asserts
> the error, untouched stores, and that generation 1 still revokes.

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
  > **Fixed** (`8c8149b10`): `submit` evicts terminal records older than
  > `TERMINAL_RECORD_TTL_SECS` (1h); `evict_terminal(ttl, now)` is public for
  > tighter housekeeping. In-flight tasks are never touched.
- **Duplicate `submit` (nRPC retransmit) overwrites the Entry** —
  `sdk/src/a2a.rs:287`. A retried submit with the same `task_id` replaces the
  Entry (new cancel token) and spawns a *second* executor; the first keeps
  running uncancellable, and both race on `set_state`. Guard against re-insert of
  a known id.
  > **Fixed** (`8c8149b10`): `submit` is idempotent per task id — a known id is
  > acked without a second spawn.
- **`RenewalRequest` carries no nonce/timestamp** — `sdk/src/enrollment.rs:847`.
  The renewal message is replayable (impact bounded: the reissued grant binds to
  the device key the replayer lacks), unlike the join path which has single-use
  freshness. Add an anti-replay binding.
  > **Fixed** (`ef8552871`): a signed `issued_at` rides in the request; the
  > authority refuses stamps outside `RENEWAL_FRESHNESS_SECS` (5 min, + skew
  > tolerance) with the new `StaleRenewal` → `BAD_REQUEST`. Wire format extends
  > NMR1 (unreleased on this branch).
- **Silent-renewal loop has no interval floor** —
  `integrations/hermes/renewal.py:130`. `NET_MESH_RENEWAL_INTERVAL=0`/negative →
  `_stop.wait(0)` returns instantly → 100%-CPU busy-loop (`_env_int` guards only
  `ValueError`, not `<= 0`). Clamp to a sane minimum.
  > **Fixed** (`4ced0199a`): clamped to a 60s floor with a warning.
- **`renew` re-records a `forget()`-pruned device** — `sdk/src/operator.rs:377`.
  A device pruned via `forget` (floor left at 0) is silently resurrected as an
  active inventory record on its next silent renewal (`existing.get` → `None` →
  `unwrap_or_default` → `record`).
  > **Fixed** (`ef8552871`): renewal now requires inventory membership —
  > `forget()` also stops silent renewal; the device keeps its grant until
  > expiry and must re-enroll to reappear.
- **`_on_session_end` has no per-call guard** —
  `integrations/hermes/__init__.py:131`. One service `.stop()` raising skips the
  rest of teardown, including `node.shutdown()`, leaking the mesh node + the
  silent-renewal daemon thread + served RPC handles. Wrap each teardown step.
  > **Fixed** (`4ced0199a`): each stop is guarded individually and the handles
  > clear up front; `node.shutdown()` always runs.
- **`DeviceEnrollment::save` temp file is PID-only** —
  `sdk/src/enrollment.rs:1057`. `enrollment.tmp.<pid>` with no lock; two
  concurrent saves in one process (renewal loop + a manual renew) collide,
  risking a torn/missing file. `DeviceRegistry` locks; this store does not.
  > **Fixed** (`ef8552871`): temp names gain a per-save atomic sequence
  > (`tmp.<pid>.<seq>`); the atomic renames serialize to last-writer-wins.
- **Published tool handler must return a strict `bool`** —
  `bindings/python/src/publish.rs:73`. `(text, 1)` / `(text, 0)` int-as-bool
  (idiomatic Python) is rejected and surfaced as a *transport* error, dropping
  both the tool's `is_error` flag and its text (misclassified as a transport
  failure).
  > **Fixed** (`b08cd169f`): `py_to_result` falls back to `(String, i64)`,
  > mapping non-zero to `is_error`.
- **`context_refs` isn't list-checked** — `integrations/hermes/tools.py:520`. A
  bare-string value is iterated into per-character refs
  (`[str(r) for r in "artifact://…"]`). Normalize a string to a single-element
  list.
  > **Fixed** (`4ced0199a`): a bare string becomes a one-element list.
- **MCP adapter (latent / efficiency):**
  > **All three fixed** (`1dcbd3db7`): arguments parse before the policy hook
  > (a structurally invalid call never consults an approval policy);
  > `descriptor_fingerprint` folds schemas in via the order-invariant
  > `schema_hash`; `canonicalize_json` sorts set-semantic `required` arrays
  > (other arrays keep their order — position is meaningful, e.g. `prefixItems`).
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
- **Informational:** *(acknowledged — documented tradeoffs, deliberately
  unchanged)*
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

> **Gate cleared (2026-07-07):** every item above — the full high and medium
> sets and the actionable lower tier — is fixed and tested on this branch (see
> the per-finding annotations). One deliberate scope note: real re-admission of
> a floor-revoked device (a floor-aware re-issue that mints the fresh grant at
> the raised generation) remains a follow-up; until it exists, re-enrolled
> revoked devices stay marked revoked so the inventory never contradicts
> enforcement (M-7).
