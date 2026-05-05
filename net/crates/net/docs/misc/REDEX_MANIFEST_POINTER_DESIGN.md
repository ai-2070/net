# RedEX `compact_to` manifest-pointer atomic flip — design

Closes audit #1: `compact_to`'s three-rename sequence has a cross-file
mixed-state window. After rename N succeeds and rename N+1 hasn't
fired, recovery sees `idx` at gen K+1 paired with `dat`/`ts` still at
gen K — a state the existing recovery walk cannot distinguish from a
clean half-finished compact. The per-rename durability fix in commit
`50ba6ae5` (`MoveFileExW(MOVEFILE_WRITE_THROUGH)` on Windows) closes
the within-rename gap; the cross-rename gap is what this rework
closes.

Design follows the sketch in
`BUG_AUDIT_2026_05_03_REMAINING_PLAN.md` § long-term follow-up.

## On-disk layout

```text
<base>/<channel_path>/manifest                  # 16 B pointer file
<base>/<channel_path>/v0000000001/{idx,dat,ts}  # generation N
<base>/<channel_path>/v0000000002/{idx,dat,ts}  # generation N+1 (mid-compact)
```

The channel root holds two kinds of entry: the single `manifest` file
and zero-or-more generation directories named `v` + 10 zero-padded
decimal digits (`v0000000001` through `v9999999999`). Recovery walks
the channel root to discover generations; nothing else matters.

### Manifest wire format (16 B fixed)

```text
┌────────┬─────────┬─────────────┬──────────┬───────────┐
│ magic  │ version │ generation  │ reserved │ checksum  │
│ 4 B    │  1 B    │    4 B      │   3 B    │    4 B    │
└────────┴─────────┴─────────────┴──────────┴───────────┘
```

- `magic`: `REDM` (`0x52 0x45 0x44 0x4D`).
- `version`: `1`. Future versions may extend the record.
- `generation`: little-endian `u32`. Points at the live
  `v<generation>/` directory. Generation `0` is reserved (never
  written) so a torn all-zero manifest is unambiguously invalid.
- `reserved`: must be zero on write, ignored on read.
- `checksum`: little-endian `u32` xxh3 over bytes `[0..12]`.

Total 16 bytes — fits comfortably inside any filesystem's atomic-write
boundary, but we don't rely on that. Atomicity comes from the
rename-over of `manifest.tmp → manifest`, which is the single
linearizing point of the whole compact.

## `compact_to` flow

Pre: manifest exists pointing at generation `N`; `<channel>/v<N>/`
holds the live `{idx,dat,ts}`. Append path's cached file handles are
open against `<channel>/v<N>/{idx,dat,ts}`.

```text
1.  Compute next generation: N+1 = manifest.generation + 1.
2.  mkdir <channel>/v<N+1>/.
3.  Write <channel>/v<N+1>/{idx,dat,ts} (full content, not tmp;
    these files are unreferenced by any manifest until step 7
    succeeds, so they're effectively a write-ahead log).
4.  fsync each of v<N+1>/{idx,dat,ts}.
5.  fsync_dir on v<N+1>/.
6.  Build new manifest bytes pointing at N+1. Write to
    <channel>/manifest.tmp. fsync.
7.  durable_rename(manifest.tmp → manifest).  ←── atomic flip
8.  fsync_dir on <channel>/.
9.  Reopen the appender + worker file-handle slots against
    v<N+1>/{idx,dat,ts}.
10. Schedule v<N>/ for deletion (best-effort; recovery sweeps it
    if the post-flip cleanup is interrupted).
```

Step 7 is the linearizing event. Before it, recovery sees the old
manifest and uses `v<N>/`. After it, recovery sees the new manifest
and uses `v<N+1>/`. There is no "mixed state" — either every file in
the live generation matches what the manifest says, or recovery
detects the manifest is torn (checksum mismatch / impossible
generation) and falls back to the highest validated generation
directory.

## Recovery flow

```text
1.  Read <channel>/manifest. Three outcomes:
    (a) Present, valid checksum, generation > 0 →  use that gen.
    (b) Present, invalid (torn, bad checksum, gen 0) →  fall back.
    (c) Missing →  fall back.
2.  Fallback: enumerate <channel>/v<NNN>/ directories, pick the
    highest gen that contains all three of {idx,dat,ts}. If found,
    write a fresh manifest pointing at it (best-effort; if the
    write fails, recovery still proceeds against that gen, the
    next compact will refresh the manifest).
3.  If neither manifest nor any complete generation directory
    exists, fall through to the legacy flat-layout path (see
    "Migration" below).
4.  Open <channel>/v<gen>/{idx,dat,ts} and run the existing
    recovery walks (torn-tail trim, dat-trim, checksum filter,
    ts compaction).
5.  Sweep orphan generation directories: any v<M>/ with M != gen
    is either a stale older generation (compact succeeded long
    ago, cleanup never ran) or a half-written newer generation
    (compact crashed before flip, recovery picked older gen). In
    both cases, delete v<M>/ and fsync_dir.
```

The orphan sweep is what makes the cross-rename window safe: a crash
between step 5 (write `v<N+1>/` files) and step 7 (manifest flip)
leaves the old manifest pointing at `v<N>/`, recovery uses `v<N>/`,
and the sweep deletes the half-written `v<N+1>/`. No mixed state
ever lands in the live data.

## Migration from flat layout

Existing channels written by v0.10 / v0.11 have flat
`<channel>/{idx,dat,ts}` files with no manifest. On first open:

```text
1.  If <channel>/manifest exists →  new layout, normal recovery.
2.  Else if <channel>/{idx,dat,ts} exists →  legacy flat layout.
    a. mkdir <channel>/v0000000001/.
    b. durable_rename each of {idx,dat,ts} from <channel>/ into
       <channel>/v0000000001/.
    c. fsync_dir on v0000000001/ then on <channel>/.
    d. Write initial manifest pointing at gen 1, fsync,
       durable_rename(manifest.tmp → manifest), fsync_dir.
    e. Continue with the new-layout recovery flow.
3.  Else →  brand-new channel. mkdir v0000000001/, write initial
    manifest, no files yet.
```

Migration is one-shot per channel and idempotent (re-running observes
the manifest from step 2.d and takes the new-layout branch). Failure
mid-migration leaves the flat files in place (everything is renamed
into a directory the manifest doesn't yet reference), so recovery
re-runs the migration on the next open.

## Concurrency / lock discipline

The cached `appender_{idx,dat,ts}` and `worker_{idx,dat,ts}` file
handles inside `DiskSegment` continue to point at the live
generation. `compact_to` swaps them in the same dance the current
implementation uses (placeholder files in temp dir while the new
generation is opened, then atomic swap of the slots). The lock
acquisition order documented in the module rustdoc
(appender-dat → idx → ts → worker-dat → idx → ts) is unchanged.

The `placeholder_*` temp-dir files used during the swap can be
dropped — there's no need to swap to placeholders before the rename
because the rename targets are inside the new generation directory,
not the live paths. We just open the new generation's files, swap
the cached handles to them, then unlink the old generation. (The
placeholder dance was only needed because the old layout reused the
same paths across generations.)

## Crash-injection coverage

Per failure point, recovery must restore the channel to a consistent
state:

| #  | Crash point                                              | Expected post-recovery state                          |
| -- | -------------------------------------------------------- | ----------------------------------------------------- |
|  1 | Before mkdir `v<N+1>/`                                  | Live: `v<N>/`. Manifest unchanged. No orphan.         |
|  2 | After mkdir, before any file write                      | Live: `v<N>/`. Sweep removes empty `v<N+1>/`.         |
|  3 | Mid-write of `v<N+1>/idx`                               | Live: `v<N>/`. Sweep removes partial `v<N+1>/`.       |
|  4 | After all `v<N+1>/{idx,dat,ts}` written, before fsync   | Live: `v<N>/`. Sweep removes `v<N+1>/` (may be torn). |
|  5 | After fsync of `v<N+1>/`, before manifest.tmp written   | Live: `v<N>/`. Sweep removes `v<N+1>/`.               |
|  6 | After manifest.tmp written, before rename               | Live: `v<N>/`. manifest.tmp removed by sweep.         |
|  7 | After rename of manifest                                | Live: `v<N+1>/`. Sweep removes `v<N>/`.               |
|  8 | After rename, before reopen of cached handles           | Live: `v<N+1>/`. Cached handles re-opened on restart. |
|  9 | Mid-migration (flat → `v<1>/`), partial rename          | Recovery re-runs migration. Idempotent.               |
| 10 | Manifest itself corrupted (cosmic-ray bit-flip)         | Fallback enumerates `v<NNN>/`, picks highest valid.   |

The implementation will pin tests for each. Items 1–6 collapse onto
"recovery uses the old generation, sweep removes the orphan" — three
representative tests cover the family. Item 9 has its own family
(partial rename of one of the three flat files; partial migration of
the manifest write).

## Out of scope

- **Multi-generation history.** Only one generation is ever live;
  older generations are deleted by the sweep. Snapshot-style
  multi-generation retention is not in this rework.
- **Online format upgrade.** v0.11 → v0.12 callers re-tail or run
  the migration shim once at first open per channel; there is no
  online "convert in place while serving" path.
- **Cross-channel atomicity.** A compact spans one channel only;
  inter-channel consistency is a higher-layer concern (the bus
  flushes per-channel).
