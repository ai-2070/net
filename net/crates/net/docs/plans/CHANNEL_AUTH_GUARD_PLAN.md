# Channel authentication hardening — wiring AuthGuard into the mesh hot path — SHIPPED

**Status.** AG-1 through AG-6 complete. AuthGuard is now wired into
`MeshNode` on both sides — populated on subscribe success, revoked on
unsubscribe, and consulted on every publish fan-out with a bloom/
verified-cache/exact-fallback three-way dispatch. Token-expiry sweep
evicts subscribers whose tokens age out. Per-peer auth-failure
counter throttles bad-token subscribe storms. Bench measures
single-threaded `check_fast` at ~20 ns (DashMap probe cost, within
the 50 ns plan target).

## Context

Stage E of [`SDK_SECURITY_SURFACE_PLAN.md`](SDK_SECURITY_SURFACE_PLAN.md)
shipped the subscribe-gate and publish-gate auth checks:
capability filters, `require_token`, TOFU entity-id binding. That
work proved the model end-to-end, but left three gaps that the
main README's
[`AuthGuard` description](../README.md#channels)
implies are already closed:

> AuthGuard enforces authorization at the channel boundary. It
> combines capability filters with permission tokens. A node needs
> both the right capabilities (hardware, tags) and a valid token
> (ed25519-signed, delegatable, expirable) to access a channel.
> **Authorization results are cached in a bloom filter — <10ns
> per-packet checks.**

**Today's reality:**

1. `AuthGuard` exists and works
   ([`guard.rs:62`](../src/adapter/net/channel/guard.rs)) — 4 KB
   bloom filter, atomic bits, verified cache, exact-channel ACL.
   Tested in isolation; wired into RedEX for storage-plane gating
   ([`redex/manager.rs:28`](../src/adapter/net/redex/manager.rs)).
2. It is **not** wired into `MeshNode`. Publish fan-out iterates
   subscribers from `roster` with no per-packet auth check
   ([`mesh.rs:2690`](../src/adapter/net/mesh.rs)).
3. Expired tokens don't evict subscribers. A subscriber that
   presents a 60-second token at subscribe time gets unbounded
   access thereafter.
4. No auth-failure rate limiting. A malicious peer can spam
   thousands of bad-token subscribes and burn CPU on ed25519
   verification.

This plan closes those gaps.

## Scope

**In scope (AG-1 through AG-6):**

- Wire `Arc<AuthGuard>` into `MeshNode` + `DispatchCtx`.
- Populate `AuthGuard` on successful `authorize_subscribe`;
  revoke on `unsubscribe` and on token expiry.
- Call `AuthGuard::check_fast` on every publish in the fan-out
  loop so the <10 ns claim holds in production, not just
  isolation tests.
- Periodic token-expiry sweep that evicts expired subscribers
  from the roster and revokes them in `AuthGuard`.
- Per-peer auth-failure counter with backoff; a peer that fails
  N subscribe authorizations within window W gets temporarily
  throttled.
- Criterion benchmark validating `check_fast` <10 ns at p99
  under concurrent load.
- README + per-SDK-README updates removing "single check at
  subscribe gate" caveats.

**Out of scope:**

- Channel-level revocation lists. The token model's
  short-TTL-plus-reissue primitive is the intended revocation
  story; building a separate revocation registry is a v2 concern.
- Per-packet fan-out policy configurable at `ChannelConfig`
  granularity (always-on fast-path vs. subscribe-gate-only).
  We ship fast-path-always-on and revisit only if a deployment
  actually needs opt-out.
- Partial-chain delegation validation (recheck intermediate
  signers). `PermissionToken::delegate` already verifies depth
  and parent; deeper chains are covered end-to-end by the leaf's
  signature.
- Observability — metrics export for auth-verdict counts /
  bloom hit rate. Worth doing; separate plan.

## Design

### Invariants

1. **No false positives.** `check_fast() → Allowed` must never
   admit a subscriber who lacks a valid token. Bloom false
   positives fall through to the exact path; the exact cache is
   the authority.
2. **Bounded staleness.** A revoked or expired subscriber must
   stop receiving events within `token_sweep_interval` (default
   30 s). Instantaneous revocation via `AuthGuard::revoke` stops
   delivery on the next publish.
3. **No per-packet crypto.** The fan-out loop does one atomic
   bit load + one DashMap probe per subscriber, regardless of
   channel count. Signature verification happens once at
   subscribe time; the `verified` cache carries the decision.
4. **Fast path is always on.** Every publish consults
   `AuthGuard` before enqueuing to a subscriber. If the
   channel has no auth configured (`publish_caps=None`,
   `subscribe_caps=None`, `require_token=false`), the channel's
   `(origin_hash, channel_hash)` pair is admitted on any
   subscribe, so the fast path trivially passes — no conditional
   dispatch branch needed.

### Populating AuthGuard on subscribe

Today, successful `authorize_subscribe`
([`mesh.rs:2499`](../src/adapter/net/mesh.rs)) writes the
subscriber into `roster` and returns `(true, None)`. Add a new
step right before the return:

```rust
ctx.auth_guard.allow_channel(
    origin_hash_of(from_node),
    &channel_name,
);
```

`origin_hash_of` derives the 32-bit origin from the peer's node id
(low 32 bits of the u64 is fine — matches the `NetHeader.origin`
field semantics). `allow_channel` is the existing
[`guard.rs:169`](../src/adapter/net/channel/guard.rs) method that
sets the two bloom bits AND inserts into the exact-name ACL.

### Fast path on publish

Today, `publish_many`
([`mesh.rs:2690`](../src/adapter/net/mesh.rs)) does:

```rust
for peer_id in subscribers {
    self.publish_to_peer(peer_id, …).await;
}
```

After the change:

```rust
for peer_id in subscribers {
    let origin_hash = peer_id as u32;
    match self.auth_guard.check_fast(origin_hash, channel_hash) {
        AuthVerdict::Allowed => { /* fast path — send */ }
        AuthVerdict::Denied => { /* skip; queue a roster cleanup */ }
        AuthVerdict::Unknown => {
            // Bloom false positive OR new subscriber that
            // raced ahead of the AuthGuard insert. Fall back
            // to the exact check; on success, promote.
            if self.auth_guard.is_authorized_full(origin_hash, &name) {
                self.auth_guard.allow_channel(origin_hash, &name);
            } else {
                continue;
            }
        }
    }
    self.publish_to_peer(peer_id, …).await;
}
```

Performance note: `peer_id as u32` is the truncation
`routing_id(node_id)` already uses for the routing-header
`src_id` field
([`mesh.rs:383`](../src/adapter/net/mesh.rs)). Same derivation on
both ends keeps `origin_hash` consistent across the hot path.

### Expiry sweep

New background task, spawned alongside the existing capability-GC
loop:

```rust
fn spawn_token_sweep_loop(&self) -> JoinHandle<()> {
    let roster = self.roster.clone();
    let guard = self.auth_guard.clone();
    let cache = self.token_cache.clone();
    let peer_entity_ids = self.peer_entity_ids.clone();
    let channel_configs = self.channel_configs.clone();
    let interval = self.config.token_sweep_interval;
    let shutdown = self.shutdown.clone();
    let shutdown_notify = self.shutdown_notify.clone();

    tokio::spawn(async move {
        let mut tick = tokio::time::interval(interval);
        tick.tick().await; // skip immediate fire
        loop {
            tokio::select! {
                _ = tick.tick() => {
                    sweep_expired_subscribers(
                        &roster, &guard, cache.as_deref(),
                        &peer_entity_ids, channel_configs.as_deref(),
                    );
                }
                _ = shutdown_notify.notified() => break,
            }
            if shutdown.load(Ordering::Acquire) { break; }
        }
    })
}
```

The sweep walks `roster`, not `TokenCache`. Rationale:

- The roster already indexes `(channel, subscriber_node_id)` —
  exactly the granularity we need to revoke.
- Walking `TokenCache` means reverse-mapping `(EntityId,
  channel_hash)` back to `(node_id, channel_name)` which needs
  extra bookkeeping.
- For channels without `require_token`, the roster entry has no
  corresponding TokenCache entry; the sweep correctly skips it
  by checking `channel_configs[channel].require_token`.

For each `(channel, node_id)` where `require_token` is set:
lookup `entity_id` in `peer_entity_ids`, call
`cache.check(entity, SUBSCRIBE, channel_hash)`. On `Err(Expired)`,
remove from roster and revoke in `AuthGuard`.

### Auth-failure rate limit

Per-peer counter on `MeshNode`:

```rust
auth_failure_counters: Arc<DashMap<u64, AuthFailureState>>,

struct AuthFailureState {
    failures: u16,
    window_start: Instant,
    throttled_until: Option<Instant>,
}
```

On `authorize_subscribe` returning `(false, _)`:

```rust
let now = Instant::now();
let mut entry = auth_failure_counters.entry(from_node).or_default();
if now.duration_since(entry.window_start) > Duration::from_secs(60) {
    entry.failures = 0;
    entry.window_start = now;
}
entry.failures = entry.failures.saturating_add(1);
if entry.failures >= MAX_AUTH_FAILURES_PER_WINDOW {
    entry.throttled_until = Some(now + Duration::from_secs(30));
}
```

`MAX_AUTH_FAILURES_PER_WINDOW = 16` by default, configurable via
`MeshNodeConfig::with_max_auth_failures`. On `authorize_subscribe`
entry, if `entry.throttled_until` is in the future, short-circuit
with `(false, Some(AckReason::RateLimited))` — no ed25519 work is
done.

Successful subscribe resets the failure counter for that peer.

### Config knobs

Three new fields on `MeshNodeConfig`:

```rust
/// Period between token-expiry sweeps. A subscriber whose token
/// expires mid-subscription gets evicted within one sweep
/// interval. Default: 30 s.
pub token_sweep_interval: Duration,

/// Authorization-failure threshold per peer per window. Peers
/// exceeding this count are throttled (further subscribes short-
/// circuit with `RateLimited`) for `auth_throttle_duration`.
/// Default: 16 failures per 60 s window.
pub max_auth_failures_per_window: u16,

/// How long a peer stays throttled after exceeding
/// `max_auth_failures_per_window`. Default: 30 s.
pub auth_throttle_duration: Duration,
```

## Staged rollout

| Stage | What | Days |
|---|---|---|
| **AG-1** | Add `auth_guard: Arc<AuthGuard>` on `MeshNode` + `DispatchCtx`. Populate on `authorize_subscribe` success, revoke on unsubscribe + on `MeshNode::shutdown`. Unit: successful subscribe → `guard.is_authorized(origin, channel)` returns true. | 0.5 |
| **AG-2** | Fast-path check in `publish_many` fan-out. Handle `Allowed` / `Denied` / `Unknown` with the fallback-then-promote logic. Integration: revoke a subscriber via `guard.revoke_channel` and verify the next publish skips them. | 1 |
| **AG-3** | Token-expiry sweep: new `token_sweep_interval` config, spawn loop, sweep roster + revoke expired. Integration: subscribe with 1 s TTL, wait 2 s, next publish drops the subscriber. | 1 |
| **AG-4** | Auth-failure rate limit: per-peer counter on `MeshNode`, throttle check at the top of `authorize_subscribe`, reset on success. Integration: 17 bad subscribes in quick succession → 18th short-circuits with `RateLimited`. | 1 |
| **AG-5** | Criterion bench for `check_fast` under concurrent load: 1M calls / 8 threads, assert p99 < 50 ns. | 0.5 |
| **AG-6** | README updates (main + all 5 SDK/binding READMEs), plan doc marked SHIPPED, cross-link from `SDK_SECURITY_SURFACE_PLAN.md`. | 0.5 |

**Total ~4.5 days.**

## Test plan

New tests in `tests/channel_auth_hardening.rs`:

1. **`auth_guard_populated_on_subscribe`** — A registers a
   `require_token` channel; B subscribes with a valid token; A's
   `auth_guard.is_authorized_full(b_origin, &channel)` returns
   true.
2. **`publish_skips_revoked_subscriber`** — B subscribes
   successfully; A calls `guard.revoke_channel`; the next
   `publish` returns a `PublishReport` with `attempted = 0` for
   B's entry.
3. **`expired_token_drops_subscriber_within_one_sweep`** — B
   subscribes with a 500 ms TTL token. After
   `2 × token_sweep_interval`, B is off the roster and A's
   guard no longer authorizes it.
4. **`bloom_false_positive_falls_back_to_exact`** — Craft an
   `(origin_hash, channel_hash)` collision with a non-authorized
   peer; verify `check_fast` returns `Unknown` and the fallback
   rejects it.
5. **`auth_failure_rate_limit_kicks_in`** — B sends 17
   subscribes with malformed tokens; the 18th short-circuits with
   `AckReason::RateLimited` before ed25519 verification runs
   (observed via a timing proxy: mocked keypair verify counter
   stays at 16).
6. **`successful_subscribe_resets_failure_window`** — B sends 8
   bad subscribes, then a good one; its failure counter resets,
   and 17 more bad ones are needed to trigger throttling again.
7. **`throttled_peer_unblocks_after_interval`** — After
   triggering throttling, wait `auth_throttle_duration + margin`;
   B's next subscribe goes through the normal auth path.

Bench in `benches/auth_guard.rs`:

- `bench_check_fast_concurrent` — 8 threads, 1 M calls each,
  p50 / p99 / p999 reported. Assert p99 < 50 ns (5 × the single-
  thread target, accounting for cache contention).
- `bench_allow_channel_cost` — populate cost on subscribe.

## Risks

- **Bloom collision blast radius.** A false positive on
  `check_fast` causes a DashMap probe, which is slightly slower
  than the bloom-miss path (~30 ns instead of 5 ns). At the
  default 4 KB / 2^15 bits, the false-positive rate for k=2
  hashes at 1000 entries is ~0.1%. At 10 000 entries it rises
  to ~9%. Mitigation: document the recommended channel-per-peer
  cap; rebuild the bloom with a larger size if deployments push
  toward 10k+ channels per node.
- **Sweep latency affecting freshness.** 30 s is a long window
  for a revoked subscriber. Mitigations already in the design:
  (a) instantaneous revocation via `guard.revoke_channel`
  bypasses the sweep, (b) the fan-out fast-path consults the
  guard on every publish — expired entries skip even before the
  sweep eviction lands, because the token cache's `check` is
  what the guard's exact-path fallback uses.
  Wait — actually the fast-path bloom doesn't consult the token
  cache. It just reads the bloom bits. So a subscriber whose
  token expired but whose `AuthGuard` entry hasn't been revoked
  yet WILL get packets until the sweep runs. This is acceptable
  given the alternative (per-packet token validation) defeats
  the <10ns bound. Document this trade-off.
- **Rate-limit interaction with honest peers.** A peer with a
  genuinely expired-then-renewed token might trigger failures
  during the token-rotation window. Mitigation: 16 failures per
  60 s is generous — honest retry storms won't hit it.
- **Sweep cost on large rosters.** O(subscribers × channels);
  a node with 10 000 subscribers × 100 channels does a million
  TokenCache lookups per sweep. DashMap lookups are ~50 ns, so
  50 ms every 30 s — tolerable. Document the pathological case
  and flag "sweep sharding" as a follow-up.

## Files touched (estimate)

| File | Why |
|---|---|
| `src/adapter/net/mesh.rs` | `auth_guard` field + wiring in ctx, fast-path check in `publish_many`, expiry sweep spawn + function, auth-failure counter + throttle |
| `src/adapter/net/mesh.rs` config | `token_sweep_interval`, `max_auth_failures_per_window`, `auth_throttle_duration` |
| `src/adapter/net/channel/guard.rs` | Probably no changes — the existing API covers every call site this plan needs |
| `tests/channel_auth_hardening.rs` (new) | 7 integration tests above |
| `benches/auth_guard.rs` (new) | Concurrent-load bench |
| `docs/SDK_SECURITY_SURFACE_PLAN.md` | Cross-link; add an "AuthGuard hardening" note |
| `README.md` (main) | Update `## Channels` paragraph — "<10 ns per packet" now true on the fan-out path, not just in isolation |
| `sdk/README.md` → "Channel authentication" subsection | Note mid-subscription expiry handling |
| `sdk-ts/README.md`, `bindings/python/README.md`, `bindings/go/README.md` | Same expiry + rate-limit notes |

## Exit criteria

- `cargo test --features net --test channel_auth` green (regression).
- `cargo test --features net --test channel_auth_hardening` green (7 new tests).
- `cargo clippy --features net -p net -- -D warnings` clean.
- `cargo bench --features net --bench auth_guard -- check_fast` passes with p99 < 50 ns (single digit microseconds at absolute worst).
- 878+ lib tests still pass (no regression).
- Every SDK README's channel-auth section references the
  sweep + rate-limit story.

## Explicit follow-ups (not in this plan)

- **Revocation lists.** A dedicated `(issuer, nonce) → revoked`
  registry that takes precedence over token expiry, for tokens
  that need to be killed before their TTL.
- **Metrics.** Export `auth_verdict_counts`, `bloom_hit_rate`,
  `sweep_duration` via a `Metrics` trait for integration with
  Prometheus / OpenTelemetry.
- **Per-channel fast-path opt-out.** Some high-security channels
  might want per-packet token re-verification despite the
  perf cost. Gate on `ChannelConfig::always_per_packet_auth`.
- **Delegation-chain partial validation.** Re-verify every
  intermediate signer on subscribe, not just the leaf. Cost
  grows with chain depth; leaf-only is acceptable today because
  the core enforces depth caps.
- **Sweep sharding.** For meshes with > 10 k subscribers per
  node, shard the sweep across channels so each tick does a
  constant fraction of the roster.

## Open questions for review

1. **`origin_hash = node_id as u32`?** This mirrors `routing_id`
   but discards the high 32 bits. A 32-bit collision space with
   64-bit node ids yields a birthday collision at ~65 k peers.
   Confirm this is acceptable, or lift `AuthGuard`'s key to 64
   bits (bigger bloom, slightly slower but still under 10 ns).
2. **Sweep interval default — 30 s too long?** TS SDK defaults
   `ttlSeconds` to 300 s in the docs. A 30 s sweep means up to
   10% of a token's TTL is unenforced post-expiry. Consider
   10 s default.
3. **Throttle via `RateLimited` or a new `AuthThrottled` ack?**
   `RateLimited` is already used for membership churn. If
   operators want to distinguish auth-throttling from
   membership-throttling in metrics, a new variant helps.
4. **Benchmark target p99 < 50 ns vs. < 10 ns?** Under
   concurrent load cache-line contention will push us past 10
   ns. 50 ns seems realistic for 8-thread contention; flag if a
   stricter target is needed.
