---
title: "v0.42.0 — Back in Black"
description: "Release notes for Net v0.42.0 — Back in Black — what shipped, what changed, and what it means for compatibility."
---
# Net v0.42 — "Back in Black"

*AC/DC, 1980: same band as [v0.41](/docs/releases/release-v0.41-thunderstruck), leaner and harder. v0.41 made the capability fold hold a million-node fleet without stalling; v0.42 makes it fit in less than half the memory, and makes an idle expiry sweep do no work at all.*

## What's in it

- **The capability fold in half the memory.** At a million resident entries the fold now retains about 1.9 GB, down from 4.2 GB. Tags are interned once per fold, index buckets are bitmaps over entry slots, and expiry runs on a timing wheel.
- **An idle expiry sweep costs nothing.** With nothing due, a sweep at 1M touched every entry twice a second (8.7 ms per tick). It now visits none.
- **Oversized announcements are refused whole**, at every intake path and before they are sent: more than 8,192 tags, a tag longer than 256 bytes, or more than 256 blob-heat tags.
- **Breaking, Rust only:** the fold's types and `ServeError` changed. See "Breaking changes".

---

## The capability fold, leaner

Every node keeps a fold of what its peers announce, and every capability query, route choice and placement filter reads it. v0.41 fixed its speed at fleet scale and left memory at 4.2 GB per million entries, with the work that would cut it on hold. That work is this release: three changes, landed as Slices 6–8 of [`CAPABILITY_FOLD_SCALE_PLAN.md`](https://github.com/ai-2070/net/blob/master/docs/internal/plans/CAPABILITY_FOLD_SCALE_PLAN.md).

- **Tags are interned.** A fleet announces the same few thousand tags millions of times. Each distinct tag is now stored once per fold, as a shared, immutable `TagStr`, and counted by a fold-owned dictionary. A reader's cloned handle never keeps a tag alive. Wire bytes, signatures and snapshots are unchanged: `TagStr` serializes exactly as `String`, and an old signature still verifies.
- **Index buckets are bitmaps.** Each tag, region and state bucket held a hash set of `(class, node)` pairs. Each entry now gets a dense `u32` slot, and every bucket is a roaring bitmap of slots: an AND starts from the smallest bucket, an OR is a bitmap union. Freed slots are reused before the table grows, and a full slot table refuses an insert rather than wrapping.
- **Expiry runs on a timing wheel.** Entries sit in 125 ms slots on a 4,096-slot ring. A sweep probes only the slots that came due since the last one, under the read lock, and returns without the write lock when nothing is due. A refresh relinks its node rather than allocating, so a refresh that changes nothing still makes no allocations (a test enforces it). Expiry is exactly as prompt as the full walk was.

### Measured

Intel i9-14900K, Windows 11, `fold_scale_report`. v0.40 is the baseline before any of the fold work. The v0.42 column is the latest measurement of each row; rows marked ¹ were last measured with v0.41's work and were not re-run after this release's changes.

**Memory**

| measure | v0.40 | v0.42 | change |
|---|---|---|---|
| retained per entry, 1M | 4,184 B (4.2 GB) | 1,875 B (~1.9 GB) | −55% |
| retained per entry, 100k | 3,293 B | 1,431 B | −57% |
| index buckets + slot table per entry, 1M | 1,280 B | 88 B | −93% |
| tag storage per entry, 100k | 1,641 B | 508 B | −69% |

**Time**

| measure | v0.40 | v0.42 | change |
|---|---|---|---|
| expiry sweep, 1M, nothing due | 8.7 ms per tick, 1M visited | 0.00 ms, 0 visited | eliminated |
| expiry sweep, 1M, 10% expired | 1.43 s, 99 walks | 0.91 s, 1 walk | −36% |
| expiry sweep, 1M, whole fleet expired | 14.1 s | 10.5 s | −26% |
| cache hit rate, 200-node hot set, fleet announcing¹ | 33.8% | 99.8% | — |
| insert / refresh / changed replace¹ | 5.99 / 1.40 / 16.4 µs | 2.86 / 1.21 / 10.4 µs | −52% / −14% / −37% |
| translate an announcement¹ | 9.79 µs | 3.7 µs | −62% |
| `query_complex` / `query_tag`, 50k¹ | 1.52 ms / 737 µs | 613 / 353 µs | about −55–60% |
| mixed workload: refresh p50 / p99 | 21.7 / 52.6 µs | 14.1 / 45.6 µs | −35% / −13% |
| mixed workload: broad query p50 (~500k matches) | 40.5 ms | 17.7 ms | −56% |

How to read these:

- **Every figure is a separate run.** The mass-expiry rows show how much runs vary: v0.41's notes printed 0.56 s and 7.0 s for them, and a later run of the same code measured 0.99 s and 12.1 s. Treat those two rows as "one walk instead of 99", not as a precise ratio.
- **Microsecond changes of ±20–30% are noise** on this machine, so read refresh −14% as flat.
- **One deliberate trade.** Bitmap buckets made a broad query about 11% slower than the hash-set buckets they replaced (15.75 → 17.68 ms, about 4 ns per match), in exchange for 98% less bucket memory. It is still well under v0.40, but above the 13.5 ms v0.41's notes reported.
- **Not re-measured:** the final review round (the 8,192-tag cap, the linear budget check, the expiry-wheel and restore limits).

---

## Oversized announcements are refused whole

The fold now has limits it enforces before merging anything, so one peer cannot grow every other node's fold without bound. Interning made these necessary: a shared dictionary has to have a ceiling.

- **Per announcement:** at most **8,192 tags** (duplicates count) and **256 bytes per tag**, which bounds one advertisement's tag data at 2 MiB. The decoder counts every encoded element and measures each tag's encoded length before parsing or deduplicating, so the whole announcement is refused before any filter runs. 8,192 admits a node with about 1,600 published tools; the first draft's 256 capped a node at around 40 tools and was raised.
- **Blob heat:** at most **256 blob-heat tags** per announcement. Over the cap, the announcement used to be silently cut to 256; it is now refused whole, before any intake side effect (dedup cache, key pin, routes, forwarding). On the sending side, a node tracking more hot blobs keeps its hottest 256, so it is never partitioned from its peers by its own heat.
- **Per fold:** a tag dictionary budget of **1,000,000 distinct tags / 64 MiB** by default, set at creation with `CapabilityIndexInner::with_tag_budget`, passed to `Fold::with_sweep_interval_and_index`. Admission is all-or-nothing, and a refusal changes nothing: no payload, index, expiry deadline, revision or cache entry moves.
- **Local publishes refuse first.** `announce_capabilities*` checks the set before it becomes the baseline, and `serve_rpc` (all four shapes) rolls the service back and returns the new `ServeError::CapabilityRefused` when its tag could not be announced, rather than registering a service no peer can discover.
- Refusals are counted: `FoldStats` gains `limit_rejections` and `budget_rejections` next to the dictionary's `interned`, `interned_bytes` and `interned_overhead_bytes`.

Mixed fleets: a v0.41 node that announces over these limits is accepted by v0.41 peers and refused by v0.42 peers. No real workload is known to come near 8,192 tags.

---

## Fixes

- **A replica's candidacy is no longer dropped when its channel reopens.** Closing and reopening a RedEX channel at once could withdraw the candidacy tag under a claim that had just registered, leaving the channel unadvertised. Release and claim now decide under one lock. A release that finds the announce lock busy retries the withdraw (3 attempts, 1 s apart) rather than leaving a stale tag. The race was on master before this release; the end-to-end test now passes 200 of 200 stress runs (it failed within 1–3).
- **A service is registered, self-indexed and rolled back under one announce lock.** A concurrent announce could briefly publish the `nrpc:` tag of a service that was about to be refused.
- **An announce that loses to a newer self-entry in the node's own fold is refused**, with nothing committed. It used to be treated as success.
- **A refused announce changes nothing**: it no longer consumes a version or edits the baseline.
- **An audit sink that reads `stats()` can no longer deadlock** the fold, during a refusal or with a sink replacement queued.
- **Restore refuses an oversized snapshot before materializing it**, and every fold kind's expiry wheel has a node limit, so a fold that is full refuses a new entry before any mutation.

---

## Smaller changes

- The tag-budget check is linear in the payload. It runs under both fold write locks and was quadratic (about 1.7 × 10⁸ comparisons at the 8,192-tag cap).
- New dependency: `roaring` 0.11 (std only). Neither the wire crate nor the browser leaf depends on it.
- Dependency updates, including `toml`.

---

## Version bump

Everything published moves to **0.42.0**:

- every manifest: crate, wire, leaf, CLI, deck, SDK, payments, and the Go, Node and Python bindings;
- the `@net-mesh/*` pins and the `net-mesh` Python bound (now `>=0.42.0,<0.43.0`);
- the skills' `net-version`;
- the Hermes integration pin;
- the lockfiles.

---

## Breaking changes

Rust source only. Nothing changed on the wire, except that the limits above are now enforced. The Go, Node, Python and C surfaces are unaffected.

- `CapabilityMembership::tags` is `Vec<TagStr>`, not `Vec<String>`.
- `FoldError` has two new variants, `PayloadRejected` and `RestoreRefused`, each carrying a `PayloadRejection`.
- `ServeError` has a new variant, `CapabilityRefused(String)`.
- `FoldStats` has five new public fields: `interned`, `interned_bytes`, `interned_overhead_bytes`, `limit_rejections` and `budget_rejections`.
- `FoldKind` gains `validate`, and `FoldIndex` gains `admit`, `release`, `preflight_restore`, `entry_capacity` and `admission_stats`. All have defaults, so existing implementations still compile.
- Reader signatures that took `&[String]` take `&[TagStr]` or are generic over `AsRef<str>` (for example `matches_any`).

---

## How to upgrade

Bump to 0.42.0 and rebuild. If you use the fold types directly:

- Read a tag as `&str` through `TagStr`'s `Deref`, `as_str()` or `AsRef<str>`; build one with `TagStr::from`. Code that only iterated `tags` and compared against strings usually compiles unchanged.
- Add arms for `FoldError::PayloadRejected`, `FoldError::RestoreRefused` and `ServeError::CapabilityRefused` to exhaustive matches. `CapabilityRefused` means the service was not registered.
- Set the five new fields in a `FoldStats` struct literal. Deserializing older JSON is unaffected.

---

## License

See [LICENSE](https://github.com/ai-2070/net/blob/master/net/crates/net/LICENSE-APACHE).
