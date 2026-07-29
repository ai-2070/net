# Code review — `multifolds` branch (2026-05-22)

Branch base: `master`.
Branch tip: `23c955e9` ("multifold: dedupe publisher helpers + share micros-timestamp helper.") after the cleanup commit; previous tip `083432a9` ("multifold: per-fold publisher convenience helpers on MeshNode.").
Scope: ~7,200 LOC across 13 commits, adding the multifold framework. New module `net/crates/net/src/adapter/net/behavior/fold/` (15 files) plus targeted additions to `mesh.rs` (fold inbound dispatch arm, publisher helpers, admin API). Implements `SCALING_MULTIFOLD_PLAN.md` Phases 1–6b.

Three review agents (reuse / quality / efficiency) were dispatched in parallel. Findings below are organised by category, then severity. File paths are relative to repo root; line numbers reflect the branch tip after `23c955e9` and may drift.

---

## Already fixed in `23c955e9`

- **Publisher-helper copy-paste.** `publish_capability_membership` / `publish_route` / `publish_reservation` collapsed into a generic `publish_fold<K: FoldKind>(counter_class, envelope_class, payload)` with three thin wrappers. `mesh.rs:7095-7187`.
- **Five duplicate `SystemTime::now() → micros` blocks.** Promoted to `pub(crate) fn current_timestamp_micros()` in `adapter/net/mod.rs`. Removed `fold_publish_now_us` (mesh.rs), `unix_micros_now` (snapshot.rs), three inline blocks in `reservation.rs`.

Net change: −50 lines. All 80 fold tests + 4 publisher-helper tests pass.

---

## False positives noted during the pass

- **Reservation `class` mismatch (Quality #2 / Efficiency #3).** Reviewer claimed `publish_reservation` keys the generation counter on `resource_id` but passes `class=0` to `SignedAnnouncement::sign`. This is intentional: the wire envelope's `class` is a *pool identifier* (unused in Phase 5, deliberately `0`), while the counter shards per-resource via `next_fold_generation(KIND, resource_id)`. Docstring is correct; code is correct.

---

## HIGH — scale-driven correctness risks

### H1 — `fold_generations` grows unbounded for `ReservationFold`

`net/crates/net/src/adapter/net/mesh.rs:1564-1593` (field doc) and `:7081-7094` (allocator). The map is `DashMap<(u16, u64), AtomicU64>` keyed `(KIND_ID, class)`. For `ReservationFold` the class is `resource_id: u64`, so every resource this node has ever published a state transition for retains a 32-byte counter entry forever. Over a node's months-long lifetime in a compute marketplace this is the genuine unbounded-growth path the field doc flagged in "Open question #2" but didn't address.

Fix options:
- Add a periodic GC sweep that drops counter entries whose corresponding fold entry has expired (TTL eviction). Cleanest is a `Fold<K>::on_evict` hook that signals the publisher map.
- Persist + GC together when the "Open question #2" persistence layer lands.

`RoutingFold` (class always `0`) and `CapabilityFold` (class = capability class hash, low cardinality) are safe; `ReservationFold` is the only fold with unbounded class space today, but the same risk applies to any future fold that keys class on per-request identifiers.

### H2 — O(N) peer→EntityId lookup on the inbound fold hot path

`net/crates/net/src/adapter/net/mesh.rs:3866-3871`. The fold inbound arm runs `ctx.peers.iter().find(|e| e.value().session.session_id() == session.session_id())` on every fold packet — DashMap full iteration with predicate. With 50–100 peers this is hundreds of ns; with 1000+ peers (the plan's scale ceiling) it's microseconds per packet, on the hot path.

This is the established convention — the REDEX (`mesh.rs:3783-3788`) and MESHDB (`mesh.rs:3823-3828`) subprotocol arms use the same pattern. The multifold branch faithfully mirrors it. Fix touches all three arms equally: add a `session_id → node_id` DashMap maintained at session establishment, or thread `from_node` through `DispatchCtx`.

Out of scope for this branch; flagging as a substrate-wide concern to address before the 1k-peer scaling milestone.

### H3 — Sequential broadcast fan-out in `publish_fold_broadcast`

`net/crates/net/src/adapter/net/mesh.rs:7039-7062`. Loops over peers and `await`s `send_subprotocol` sequentially. Total publish latency is `N × per_peer_send` where each per-peer send does encryption + UDP send.

The docstring states this mirrors `announce_capabilities_with` (`mesh.rs:7482-7489`), which is also sequential. That path is acceptable because announces are rate-limited by `min_announce_interval` (`mesh.rs:7462-7475`); **fold publishes have no equivalent rate-limit guard**, so a chatty `ReservationFold` publisher will block its own task on serial fan-out.

Fix: use `FuturesUnordered` (or `join_all`) to fan out concurrently. The single pre-encoded `bytes` buffer is shared by reference, so concurrent sends don't duplicate encode work. Behavior change worth a separate design decision — UDP send ordering on a single socket may matter for back-pressure heuristics elsewhere.

---

## MEDIUM — quality / API hygiene

### M1 — `FoldChannelRouter::stats()` default impl returns `Vec::new()`

`net/crates/net/src/adapter/net/behavior/fold/dispatch.rs:282-288` and the matching `MeshNode::fold_stats` doc at `mesh.rs:8093-8101` (which spells out the issue: "Returns an empty `Vec` ... AND when the installed router is a stub that doesn't track stats"). A stub router silently reporting "no folds" is indistinguishable from "no router installed".

Fix: drop the default impl — make `stats()` required, force test stubs to return their own `Vec`. Or split into a separate `FoldStatsProvider` trait that `set_fold_router` accepts via a second optional setter.

### M2 — `AuditEvent.kind: &'static str` is stringly-typed

`net/crates/net/src/adapter/net/behavior/fold/mod.rs:186-200`. The field carries one of `"created"`, `"replaced"`, `"rejected"`, `"evicted"`, `"expired"` — the docstring even enumerates them. Folds may emit additional kinds (`"reservation_takeover"`, etc.).

Fix: replace with `enum AuditKind { Created, Replaced, Rejected, Evicted, Expired, Custom(&'static str) }`. The `Custom` variant preserves extensibility.

### M3 — Trait name collision: `fold::AuditSink` vs `safety::AuditSink`

`net/crates/net/src/adapter/net/behavior/fold/audit.rs:36-41` and `net/crates/net/src/adapter/net/behavior/safety.rs:943-948`. Different `Event` types, different methods (`record` vs `write`/`flush`), but both named `AuditSink` under the same `behavior::` namespace. Navigability hazard.

Fix: rename `fold::AuditSink → FoldAuditSink`. Or unify both behind one generic `AuditSink<E>` (larger change).

### M4 — Expiry sweeper holds two write locks across the entire scan

`net/crates/net/src/adapter/net/behavior/fold/expiry.rs:71-124`. `sweep_expired` takes `state.write()` AND `index.write()`, then walks every entry to collect expired keys, then mutates. At 100k entries per fold (plan's scale) this is hundreds of microseconds with applies fully blocked.

The two-pass design rationale (avoiding `retain`-style mutation) is sound, but the lock-across-full-scan problem isn't addressed.

Fix: chunked sweep — read-lock to collect a bounded batch (~1k keys) of candidates, drop locks, re-acquire write locks to remove just those. With the existing 500ms cadence (`expiry.rs:49`), latency is unaffected; concurrent applies see micro-pauses instead of one long stall.

### M5 — `peek_kind` re-decodes the kind field

`net/crates/net/src/adapter/net/behavior/fold/dispatch.rs:347-350` runs `postcard::take_from_bytes::<u16>(bytes)` to peek `kind`, then `FoldDispatchAdapter::dispatch` runs a full `decode_and_verify` which decodes `kind` again from the same bytes. Minor waste, but it's the inbound hot path.

Fix: drop `peek_kind` and dispatch on the post-decode `kind` (one extra fold-table lookup per envelope, no extra decode). Or have `decode_and_verify` skip the first varint since the kind is already known.

### M6 — 9-argument `SignedAnnouncement::sign` with `#[allow(clippy::too_many_arguments)]`

`net/crates/net/src/adapter/net/behavior/fold/wire.rs:157-191`. Five of the nine args are envelope metadata (`kind`, `class`, `announced_at`, `ttl_secs`, `flags`) and four of the five are always passed as defaults from every caller in this branch (announced_at = `current_timestamp_micros()`, ttl_secs = `None`, flags = `0`).

Fix: introduce `struct EnvelopeMeta { announced_at: u64, ttl_secs: Option<u32>, flags: u8 }` with a `Default` impl. Drops `sign` to 6 args and kills the `#[allow(clippy::too_many_arguments)]` on both `sign` and `signing_bytes`. Pure cleanup, but a public-API signature change.

---

## LOW — comments / structure

### L1 — Phase-tracking comments will rot

The branch is heavily commented with references to the `SCALING_MULTIFOLD_PLAN.md` phase numbers. These are task-tracking metadata that will outlive their relevance. Specific offenders:

- `behavior/fold/mod.rs:1-34` — 34-line module doc with "Phase 1 scope (this commit)", "Deferred — Phase 1B", "Phase 3/4/5". Reduce to ~5 lines: what the module is.
- `behavior/fold/mod.rs:212-218` — `Phase 1B additions:` paragraph in `Fold` doc.
- `behavior/fold/mod.rs:266-271` — "The Phase 2B receiver path always has a runtime because `dispatch_packet`..." narrates history.
- `behavior/fold/mod.rs:474-486` — `debug_assert!` with a 12-line "for Phase 1 we surface..." narration.
- `behavior/fold/wire.rs:1-31` — "Phase 2's verifier rejects:" — reword without phase.
- `behavior/fold/wire.rs:152-156` — speculative future-proofing narration about "a future payload type that introduces a fallible encode".
- `behavior/fold/dispatch.rs:36-50` — 14-line `SUBPROTOCOL_FOLD` const doc with "Phase 2B reserves 0x1000" and "Available adjacent slots `0x1001..=0x10FF`". Keep ~2 lines.
- `behavior/fold/dispatch.rs:142-152` — `FoldRegistry` doc with "Phase 2 (this commit) ships ...; integration ... is Phase 2B".
- `behavior/fold/capability.rs:1-47` — 47-line module doc, half is "Phase 3a vs 3b" history and a "~20 call sites" cutover count that's project-management metadata.
- `behavior/fold/capability.rs:262-267` — 6-line comment explaining why no `merge` override; just delete (the absence is self-evident).
- `behavior/fold/routing.rs:1-40` and `behavior/fold/reservation.rs:1-69` — same Phase-history pattern.
- `mesh.rs:1577-1591` — 14-line `fold_generations` doc with "Phase 2C / 6b-min note" and "Open question #2".
- `mesh.rs:7039-7062` — `publish_fold_broadcast` has a "Phase 2C convenience: subscriber-aware publishing ... is a Phase 3 follow-up" narration.
- `mesh.rs:7072-7081` — 11-line `next_fold_generation` comment overcomments the atomic ordering.
- `mesh.rs:8056-8091` — `set_fold_router` doc duplicates the inbound dispatch arm's doc (`mesh.rs:3839-3852`); pick one.

Fix: focused cleanup pass before merge to master. Best done as a single commit after the framework is otherwise stabilized, to avoid mid-review merge conflicts.

### L2 — `RingAuditSink` duplicates the shape of `BufferingAdminAuditChainAppender`

`net/crates/net/src/adapter/net/behavior/fold/audit.rs:108-163` vs `net/crates/net/src/adapter/net/behavior/meshos/audit_chain.rs:77-126`. Both are `parking_lot::Mutex<VecDeque<...>>` + FIFO pop_front at capacity. The meshos appender already has a `dropped` counter the new sink lacks; ~4 more identical VecDeque rings exist (`meshos/log_chain.rs:76`, `meshos/failure_chain.rs:77`, `meshos/chain.rs:229`).

Fix: factor a shared `RingBufferStore<T>` primitive. Defer until at least 5–6 ring usages exist (currently at 5); this is the threshold where the abstraction pays off.

### L3 — `metrics::FoldMetrics::snapshot` takes `kind` + `channel_prefix` as args

`net/crates/net/src/adapter/net/behavior/fold/metrics.rs:258-279`. Every caller passes `K::KIND_ID` and `K::CHANNEL_PREFIX`. The static identity flows through both call sites needlessly.

Fix: make `FoldMetrics` generic on `K`, or have `Fold::stats` (which already knows `K`) own the construction and reduce `FoldMetrics::snapshot` to return identity-less `FoldStatsCounters`.

### L4 — Module count is high relative to conceptual surface

`behavior/fold/` has 13 files for ~7k lines. `announcement.rs` (138 lines) is one struct + one `placeholder` constructor — fold into `wire.rs`. `expiry.rs` is a single spawn + sweep fn — fold into `mod.rs`. `audit.rs` could merge with the `AuditEvent` definition in `mod.rs`. Target ~8 files.

Defer until comment cleanup (L1) lands — both touch the same files.

---

## Confirmed clean / no action

- **Ed25519 sign/verify correctly delegates** to `EntityKeypair::sign` / `EntityId::verify` (no raw `ed25519_dalek::SigningKey` reconstruction). `behavior/fold/wire.rs:179, 217-237`.
- **Channel-router shape correctly mirrors** the meshdb / replication pattern. Intentional symmetry, not duplication. `mesh.rs:382-383, 8073-8085` vs `:374-376, 8044-8057`.
- **`u16` `KIND_ID` is the right choice** — postcard-encoded as a varint at the head of every envelope, demuxed before payload decode. A `&'static str` or enum would force length-prefixed encoding on the dispatch hot path.
- **Encoding work is not duplicated.** `SignedAnnouncement::sign` builds the signing-bytes buffer (signature commits); `encode` builds the wire buffer (signature included). Distinct purposes.
- **`RingAuditSink` is correctly bounded** — `VecDeque::pop_front`/`push_back` are O(1), `snapshot()` is O(N) by intent (diagnostic surface).
- **`MeshNode::new` fold init is O(1)** — `RwLock::new(None)` and `DashMap::new()`. No startup hot-path bloat.
- **Concrete fold merge functions** (`capability.rs`, `routing.rs`, `reservation.rs`) are all O(1) per merge. The capability composite-query path at `capability.rs:340-419` is well-designed (sorts tags by selectivity before intersecting).
- **CapabilityFold vs legacy CapabilityIndex** duplication is documented and time-boxed — Phase 3b cutover is on the roadmap (`74a8b8f6`).

---

## Suggested next-pass priority

If a follow-up cleanup commit is in scope before merge to master:

1. **H1 — fold_generations GC** (correctness at scale; ReservationFold-specific).
2. **M1 — drop `FoldChannelRouter::stats()` default impl** (low-risk API fix; surfaces real "no stats" state).
3. **M3 — rename `fold::AuditSink → FoldAuditSink`** (mechanical; navigability).
4. **L1 — strip phase comments** (volume but low risk; single commit).

H2 and H3 are substrate-wide concerns better tackled in their own focused branches.
