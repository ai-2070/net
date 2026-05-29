# Code Review — `security-2` branch (2026-05-29)

Scope: channel token-authorization hardening — replacing the
self-consistency-only `TokenCache::check` gate with a root-anchored
`TokenChain::verify_authorizes`. 11 files / ~1.2k insertions vs `master`.
Touches `adapter/net/channel/config.rs`, `adapter/net/identity/token.rs`,
`adapter/net/mesh.rs`, the FFI + Node + Python bindings, and the
`channel_auth*` integration tests. Companion to
[`SECURITY_AUDIT_2026_05_29_CHANNEL_AUTH.md`](SECURITY_AUDIT_2026_05_29_CHANNEL_AUTH.md).

## Summary

The core change is sound: it closes a real privilege-escalation hole
(any peer could self-issue `issuer = subject = self` and pass) by
anchoring every presented `TokenChain` to a channel-configured root of
trust. The chain logic itself — root anchor, leaf binding, per-link
revocation, link continuity, monotonic scope/channel narrowing — is
correct and well covered by the new tests.

The findings below are in the surrounding plumbing: the publish path,
the bindings surface, and lifecycle cleanup. Nothing undermines the
security property the branch establishes; the highest-impact items are
two availability regressions (publishers and binding-registered channels
that now fail closed) and one resource leak.

Findings grouped by impact. Line numbers are approximate (post-branch).

| Tag | Impact | What |
|---|---|---|
| C-1 | Correctness | Publish self-check uses scope-agnostic `TokenCache::get` → denies legitimate publisher |
| C-2 | Availability | Node + C-FFI bindings expose `require_token` but no `token_roots` → channel fails closed, no API to fix |
| C-3 | Resource | `subscriber_chains` leaks on peer failure/disconnect |
| C-4 | Availability / migration | `with_require_token(true)` is now a silent deny-all; no config-time guard; SDK README example broken |
| A-1 | Altitude | `require_token` + `token_roots` are two independent `pub` fields — invariant can drift open |
| E-1 | Efficiency | Publish fan-out re-verifies up to 8 ed25519 signatures per subscriber, per packet |
| A-2 | Altitude | Publish path supports only single-link chains — delegated PUBLISH credentials can never publish |
| D-1 | Maintainability | `verify_authorizes` plumbing duplicated across three call sites, already diverging |
| T-1 | Test quality | `rejected_subscribe_does_not_leak_token_into_shared_cache` is now vacuous |
| B-1 | Behavior change | Removed shared-cache fallback changes the re-subscribe contract |

---

## Correctness

### C-1. Publish self-check uses scope-agnostic `TokenCache::get`, denying legitimate publishers

`adapter/net/mesh.rs` — publish fan-out, where the publisher builds its
own credential:

```rust
let chain = cache
    .get(&self_entity, cfg.channel_id.hash())
    .map(TokenChain::single);
```

A `TokenCache` slot is keyed by `(subject, channel_hash)` and holds
tokens of **distinct scopes side-by-side** — `TokenCache::len` and
`insert_unchecked` both document "one `PUBLISH` and one `SUBSCRIBE` for
the same peer-on-channel." `get()` returns the first *currently-valid*
token in the slot regardless of scope, and its own doc warns: "Callers
that need a specific scope should use `check` instead."

If `get` returns the SUBSCRIBE token, the resulting one-link chain is
handed to `can_publish` → `verify_authorizes`, where
`authorizes(PUBLISH, hash)` is `false`. The node's own publish is then
denied with "publish denied by channel ACL" even though it holds a valid
PUBLISH grant in the same slot. The failure is **intermittent** —
dependent on DashMap iteration order within the slot.

This is the one clear correctness bug the diff introduces.

**Fix:** select by scope on the publish path — either add a
`get_scoped(subject, channel_hash, TokenScope::PUBLISH)` to `TokenCache`
that filters with `authorizes`, or have the publish path iterate the
slot and pick the PUBLISH-authorizing token. Mirror the same on the
subscribe side if it relies on `get`.

---

## Availability / migration

### C-2. Node and C-FFI bindings expose `require_token` but no `token_roots`

- Node: `bindings/node/src/lib.rs:1147` — `cfg = cfg.with_require_token(req)`
- C-FFI: `ffi/mesh.rs:1771` — `cfg = cfg.with_require_token(t)`
- Python: `bindings/python/src/lib.rs:1639` — same call; the docstring
  (`lib.rs:1598`) says "v1 only supports `False`," but the code still
  forwards `True` to the same path.

None of the three expose a way to set `token_roots`. Post-change,
`token_gate` returns `false` whenever `token_roots.is_empty()`, so **any
channel a C / Go / Node caller registers with `require_token = true` now
denies all publish and subscribe** — including the publisher's own
fan-out — with no API to repair it. This is a hard regression of a
shipped, documented binding feature.

**Fix:** add a `token_roots` parameter (list of entity-id bytes /
hex) to `register_channel` in all three bindings, wiring it to
`with_token_roots`. Until then, surface `require_token = true` with no
roots as an error at the binding boundary rather than a silent deny-all.

### C-4. `with_require_token(true)` is now a silent deny-all

`adapter/net/channel/config.rs` — `token_gate` short-circuits:

```rust
if self.token_roots.is_empty() {
    return false;
}
```

That fail-closed posture is correct, but the public builder
`with_require_token(true)` (no roots) remains, and now denies *every*
authorization. Any persisted config, downstream caller, or example that
used it breaks on upgrade — including the in-repo SDK round-trip in
`sdk/README.md` (`with_require_token(true)` + `subscribe_channel_with`),
which no longer works.

**Fix:** reject `require_token == true && token_roots.is_empty()` at
channel-registration time (a typed error), so the dangerous combination
surfaces loudly instead of as a total auth outage. Consider deprecating
the bare `with_require_token` setter in favor of `with_token_roots`, and
update the README example.

---

## Resource

### C-3. `subscriber_chains` leaks on peer failure / disconnect

`adapter/net/mesh.rs:2031` — the failure-detector `on_failure` callback
clears `roster`, `peer_subnets`, `peer_entity_ids`,
`origin_hash_to_node`, and `capability_fold` — but **not**
`subscriber_chains`. The periodic sweep can't reclaim the entry either:
`sweep_expired_subscribers` only iterates nodes still present in
`peer_entity_ids` (`mesh.rs:1234`), which the failure path just removed.

So a token-gated subscriber that drops without an explicit unsubscribe
(failure detection, transient disconnect) leaks its retained chain (up
to 8 × 169-byte tokens per channel) for the lifetime of the node;
unbounded under peer churn. Entries are only removed on explicit
unsubscribe (`mesh.rs:6233`) or sweep eviction (`mesh.rs:1271`), neither
of which fires here. There is also a latent stale-authorization concern
if a `node_id` is ever reused for a new peer whose entity matches the
stored leaf subject.

**Fix:** remove all `subscriber_chains` entries for the failed
`node_id` inside the `on_failure` closure (capture the `Arc` the same
way the other maps are). A `(node_id, _)` prefix scan or a secondary
`node_id -> Vec<ChannelHash>` index avoids a full-map walk.

---

## Altitude

### A-1. `require_token` and `token_roots` are two independent `pub` fields

`adapter/net/channel/config.rs:42-54` — both fields are `pub`, and only
the `with_token_roots` builder couples them. A struct-literal or direct
field assignment can set `token_roots = vec![owner]` while leaving
`require_token = false`; `token_gate` then short-circuits at
`if !self.require_token { return true }` and performs **no** token check,
despite the channel looking root-anchored. The invalid state is
representable and silent.

**Fix:** model enforcement as a single field that makes the invalid
state unrepresentable, e.g.
`token_enforcement: Option<Vec<EntityId>>` where `None` = open,
`Some(empty)` = fail-closed, `Some(roots)` = anchored. Both the C-4
guard and this drift disappear.

### A-2. Publish path supports only single-link chains

`adapter/net/mesh.rs` — the publish path only ever builds
`TokenChain::single` from the local cache, so a node granted PUBLISH via
delegation (owner → … → node) holds a token whose issuer isn't a channel
root, and the one-link chain fails the root anchor. The code comment
acknowledges this as a deliberate fail-closed limitation, but it's a
real asymmetry: subscribe supports full delegation chains, publish does
not. Worth tracking as a feature gap rather than shipping it as a
silent denial.

**Status:** acknowledged in-code; needs a held-chain store for local
publish credentials before delegated publishers work.

---

## Efficiency

### E-1. Publish fan-out re-verifies up to 8 ed25519 signatures per subscriber, per packet

`adapter/net/mesh.rs:7646` — the per-publish re-check now calls
`chain.verify_authorizes`, which runs `is_valid_with_skew` (an ed25519
verify) for **every link** of **every subscriber** on **every publish**.
The old hot path did a single `TokenCache::check` (slot lookup + one
time/signature check), sitting behind the bloom-filter + verified-cache
fast path. On a high-fanout, deep-delegation channel this is N × up-to-8
signature verifications per packet — a throughput cliff on exactly the
channels operators chose to secure.

**Fix:** the periodic sweep already re-verifies chains against the clock
+ revocation floors. The per-packet path can consult a cached
"verified-until" timestamp (or a verified-epoch counter bumped on
revocation changes) instead of re-running crypto that cannot have
changed since subscribe.

---

## Maintainability

### D-1. `verify_authorizes` plumbing duplicated across three call sites

The same `verify_authorizes(action, channel_hash, entity, roots,
revocation, skew)` invocation, with hand-rolled extraction of `roots` /
`revocation` / `skew`, appears in three places:

- `adapter/net/channel/config.rs` — `token_gate`
- `adapter/net/mesh.rs:1254` — `sweep_expired_subscribers`
- `adapter/net/mesh.rs:7646` — publish re-check

They already diverge: the sweep passes `cfg.token_roots` directly, while
the publish re-check uses `cfg_snapshot…token_roots.as_slice().unwrap_or(&[])`.
A future change to the verification contract must be made in three
hand-synced spots; a miss reopens the root-of-trust gap this branch set
out to close. The `transient_revocation` + match-on-`token_cache` borrow
dance is likewise copy-pasted between `authorize_subscribe` and the
publish path.

**Fix:** a shared `SubscriberAuthCtx` (owning `roots` / `revocation` /
`skew`, derived once from the channel config + token cache) that exposes
a single `authorizes(node, channel, action)` method. Centralizes the
contract and removes the duplicated borrow trick.

---

## Test quality

### T-1. `rejected_subscribe_does_not_leak_token_into_shared_cache` is now vacuous

`tests/channel_auth.rs` (~line 262 onward) — the subscribe path no
longer inserts into the shared `TokenCache` under any outcome (the
promote-on-success block was deleted), so the test's
`shared_cache.len() == 0` assertion is trivially true regardless of
correctness. Its stated regression guard is dead.

**Fix:** re-point the assertion at the new `subscriber_chains` store —
assert that a rejected subscribe leaves it empty for the rejected
`(node_id, channel_hash)`.

---

## Behavior change (by design — flag for changelog)

### B-1. Removed shared-cache fallback changes the re-subscribe contract

`adapter/net/mesh.rs` — `authorize_subscribe` previously fell back to the
shared cache (`passed_with_scratch || shared.can_subscribe(...)`),
letting a peer re-subscribe on a previously-stored delegation **without**
re-presenting. The new path requires a chain in every subscribe. A
client that subscribed once with a token and later calls plain
`subscribe_channel` (no token) on reconnect — or any peer dropped and
restored via churn — is now rejected.

This is intended hardening, but it's a behavioral break for callers
relying on the old reuse. **Action:** changelog note + a one-line
mention in the SDK subscribe docs that token-gated channels require a
credential on every subscribe.

---

## Checked and not flagged

- Depth/`not_before` nesting and `issuer_generation` inheritance are
  correct and match the documented design (`token.rs` `delegate` +
  `is_revoked`).
- Root-anchor / leaf-binding are checked before signatures, but this is
  safe: every link's signature is verified in the same pass, so a forged
  `first.issuer` / `last.subject` still fails.
- `TokenChain::from_bytes` length math is sound and enforces
  `MAX_CHAIN_DEPTH`.
- `TokenChain::to_bytes` truncates the link count with `as u8`, but only
  a caller hand-building a >255-link chain via the public `tokens` field
  could trigger it, and `from_bytes` caps at 8 anyway — low enough to
  skip.
- WILDCARD-scoped root tokens authorize across channels that name the
  same root, but this is opt-in and roots are explicitly trusted
  per-channel — by design, not a finding.
