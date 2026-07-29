## Verdict

**Unreachable due to cap.** The flagged `rebased as u32` cast at `disk.rs:1341` cannot wrap. `rebased` is computed as `entry.payload_offset as u64 - dat_base`, where `entry.payload_offset` is the on-disk u32 field re-widened to u64. The widened value is in `[0, u32::MAX]` by construction, so `rebased <= u32::MAX` is guaranteed and the narrowing cast is lossless. The real 4 GiB boundary is enforced upstream at write time by `offset_to_u32` (`file.rs:1560`), which returns `RedexError::SegmentOffsetOverflow` rather than truncating — every `payload_offset` that ever reaches disk has already cleared a `u32::try_from`. The 3 GiB hard-coded live-segment cap (`MAX_SEGMENT_BYTES`) prevents a single in-memory snapshot from getting close; the only path to 4 GiB cumulative is long-lifetime append+eviction, and that path is guarded too. The cast is fine; it could use a `debug_assert` and a one-line comment naming the invariant for the next reader, but it is not a corruption hazard.

## Evidence

- size cap: `MAX_SEGMENT_BYTES`:`segment.rs:20` = `3 * 1024 * 1024 * 1024` (3 GiB), `pub(super) const`, **not configurable** — no `with_*` setter, no public config field, no env override. Enforced in `segment.rs:67`, `segment.rs:88`, `file.rs:476`, `file.rs:617`, `file.rs:723`, `file.rs:820`.
- write-side guard: `offset_to_u32`:`file.rs:1560` returns `Err(SegmentOffsetOverflow { offset })` via `u32::try_from`. Called on every write-path before allocating a seq: `file.rs:486`, `file.rs:628`, `file.rs:733`, `file.rs:831`. Comment at `file.rs:1549-1558` explicitly names the 4 GiB threshold and notes "we surface the overflow instead of silently truncating." Regression test at `file.rs:2162-2196` (`test_regression_offset_to_u32_boundary`) pins this behavior at exactly `u32::MAX + 1`.
- read-side validation: `disk.rs:316-337` (recovery) walks the index, computes `end = payload_offset as u64 + payload_len as u64`, and truncates the index at the first `end > dat_len`. `disk.rs:413-418` (checksum pass) re-checks `off + len > payload_bytes.len()` defensively. Live reads go through `HeapSegment::read` (`segment.rs:105-116`), which rejects `offset < base_offset` or `rel + len > buf.len()` by returning `None`; `materialize` (`file.rs:1503-1520`) then drops the entry. A hypothetical wrap *would* be detected — collision-then-checksum-mismatch in the common case, range-violation otherwise — and surface as a dropped entry, not a silent misread.

## Bounded vs unbounded u32 fields

Casts inspected in `disk.rs` (production paths only; tests excluded).

| Field | Cast site(s) | Bounded by | Risk |
|---|---|---|---|
| `payload_offset` (rebased) | `disk.rs:1341` `rebased as u32` | Source is `entry.payload_offset as u64`, already in `[0, u32::MAX]`; `checked_sub` cannot widen | None — cast is lossless by construction |
| `payload_offset` (in-memory rebase) | `file.rs:1275` `(... as u64).saturating_sub(dat_base) as u32` | Same as above | None |
| `payload_offset` (write path) | `file.rs:492, 739` via `offset_to_u32` | `u32::try_from` returns `SegmentOffsetOverflow` on `> u32::MAX` | None — hard error, not a cast |
| `payload_len` | written from `payload.len() as u32` (tests only in `disk.rs`); production sets via `RedexEntry::new_heap` from `file.rs:492, 739, etc.` | `MAX_SEGMENT_BYTES = 3 GiB << u32::MAX` enforced before construction | None |
| manifest `checksum` | `disk.rs:1594, 1617` `xxh3_64(...) as u32` | Intentional 32-bit truncation of a 64-bit hash; symmetric (same on encode/decode) | None — design, not a bug |
| manifest `generation` | `u32` field, `checked_add(1)` at `disk.rs:1295` returns error at `u32::MAX` | Explicit overflow check; requires >4 billion compactions in one channel lifetime | None |

There are no unbounded `as u32` casts on the offset/size path in `disk.rs`. The 41 pedantic-clippy hits are dominated by test code (lines 2187 onward) where payload lengths are tiny literal `&[u8]` slices and the casts are trivially in range.

## Recommended fix

Lowest-effort, correct: add a one-line debug assertion next to the rebase cast and a comment naming the invariant, so the next reader doesn't repeat this audit.

```rust
let abs = entry.payload_offset as u64;
let rebased = abs.checked_sub(dat_base).ok_or_else(|| { /* ... */ })?;
debug_assert!(rebased <= u32::MAX as u64,
    "rebased <= abs <= u32::MAX by construction (payload_offset is u32)");
let mut e = *entry;
e.payload_offset = rebased as u32;
```

Same treatment for `file.rs:1275`. No `try_from`/error propagation is warranted because the upstream `offset_to_u32` write guard makes the invariant total: nothing with a `payload_offset > u32::MAX` can ever exist in `state.index` to begin with.

If a defense-in-depth posture is preferred, replacing both casts with `u32::try_from(rebased).expect("invariant: payload_offset fits u32")` is a wash on runtime cost and converts a silent miscompile (if the upstream guard ever regresses) into a loud panic. That is a style call, not a correctness one.

The 41-cast clippy noise should be silenced with a localized `#[allow(clippy::cast_possible_truncation)]` on the specific lines plus the debug_assert above, not by churning the whole file.
