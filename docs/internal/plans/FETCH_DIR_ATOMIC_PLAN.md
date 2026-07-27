# Atomic directory reconstruction for `fetch_dir`

## Status

**Planned, not started.** Substrate-side correctness fix to `dataforts::dir::fetch_dir`. Small change, ~50-80 LoC plus tests. Earns its existence because the current implementation leaves the destination in a partial state on any failure mid-fetch, which is a real correctness issue that every language tier inherits regardless of how the SDK wraps the function.

## The gap

Current `fetch_dir` flow (verified by reading `dir.rs`):

1. Fetch and decode the manifest.
2. Pass 1: create directory tree directly under the caller-provided `dest`, including `dest` itself via `create_dir_all`.
3. Pass 2: fetch and write files concurrently into the tree at `dest/<entry.path>`.
4. Pass 3: create symlinks under `dest/`.
5. Return.

Failure modes the current flow doesn't handle:

- A file fetch fails partway through Pass 2 → `dest` exists with some files present, some missing, no indication to the caller (beyond the error) of which.
- The substrate node loses connectivity mid-fetch → same partial state.
- The process is killed mid-fetch → partial state persists across restarts.
- A concurrent reader of `dest` (another process, an agent watching the directory, a build system tailing files) sees intermediate states where some files exist and others don't.
- Re-running `fetch_dir` against the same `dest` writes over the previous partial state but doesn't remove files that were created by an earlier successful fetch and aren't in the new manifest, so cross-version stale files accumulate.

The substrate's job is to make `fetch_dir` succeed-or-leave-untouched. Currently it doesn't; the SDK exposing `fetch_dir` in each language inherits this. Adding helpers at the SDK layer to wrap `fetch_dir` in temp-dir-and-rename logic would push the responsibility to every language binding and to application code, which is the wrong layer. The fix belongs in the substrate where `fetch_dir` lives.

## Design

Replace direct writes to `dest` with the standard temp-dir-and-atomic-rename pattern. The destination either becomes the complete new tree (success) or remains exactly as it was before the call (failure).

The flow becomes:

1. Fetch and decode the manifest (unchanged).
2. Construct a sibling temp path: `<parent_of_dest>/.<basename>.fetch_<random>` or similar — sibling of `dest` on the same filesystem, prefixed with `.` to keep it hidden, suffixed with randomness to avoid collisions across concurrent fetches into adjacent destinations.
3. Pass 1 (dirs), Pass 2 (files), Pass 3 (symlinks) all write into the temp path instead of `dest`. Existing concurrency, byte budget, semaphore logic is unchanged — the only difference is the root path.
4. After all three passes complete successfully:
    - If `dest` exists: rename `dest` to a backup sibling path (`<parent>/.<basename>.replaced_<random>`), then rename the temp path to `dest`, then remove the backup. The two renames-with-cleanup pattern ensures that a crash between the renames leaves either the new tree or the old tree at `dest`, never neither and never both.
    - If `dest` doesn't exist: rename the temp path to `dest` directly.
5. On any error during passes 1-3:
    - Remove the temp tree (best effort; if removal fails, log and continue — the temp tree's `.`-prefix and unique suffix mean it won't collide with future runs).
    - Return the original error to the caller.
    - `dest` is untouched.

The sibling-temp-path constraint matters. Atomic rename requires source and destination on the same filesystem. If the temp path were placed in `/tmp` or `$TMPDIR`, it might cross filesystem boundaries and the rename would silently become a copy-and-delete, breaking atomicity. Placing the temp directory as a sibling of `dest` keeps the rename atomic by construction.

## Implementation notes

The pattern lives in `dataforts::dir::fetch_dir`. The function's signature and return type don't change. The change is internal: replace `let dest = dest.to_path_buf();` with a temp-path allocation that becomes the working root, then at success the final rename(s) make it the user-facing `dest`.

The random suffix should use the same RNG primitive the substrate uses elsewhere (probably `rand::random::<u64>()` formatted as hex) — keep dependencies consistent rather than adding a new one for this fix.

The two-rename swap-and-cleanup for the case where `dest` exists is the only subtle part. The sequence must be:

```
rename(temp, .tmp_swap)    // not strictly required but makes the swap atomic on both sides
rename(dest, .old)         // dest now empty, old tree preserved
rename(.tmp_swap, dest)    // new tree visible; if this fails, .old can be renamed back
remove_dir_all(.old)       // best effort
```

The simpler version `rename(dest, .old); rename(temp, dest); remove_dir_all(.old)` has a race window where `dest` doesn't exist between the two renames. For most callers this is fine; for callers with concurrent observers it might not be. The implementation should pick one based on what guarantee is being made and document it. The minimal version (the simpler two-rename) is probably sufficient for the substrate's current use cases — `fetch_dir` callers aren't typically racing against concurrent observers — but worth being explicit about which guarantee `fetch_dir` provides.

Worth being explicit in the doc comment: `fetch_dir` provides "atomic directory replacement" in the sense that `dest` either contains the complete new tree or is untouched (or contains the old tree, in the replace case). It does not provide atomicity against concurrent observers reading individual files — if a concurrent process is reading files inside `dest` during the swap, it might see the old file's contents one moment and an error opening the same path the next, because the rename invalidates open file handles. Applications that need observer-atomicity have to coordinate access at a higher layer.

The error path needs to handle a few cases the current code doesn't:

- Permission to create the temp path. If `<parent_of_dest>` isn't writable, the function should fail fast with a clear error rather than failing mid-pass.
- The temp path already existing from a previous crashed run. The random suffix makes this very unlikely, but if it happens the function should retry with a new suffix rather than refusing to run.
- Cleanup of the temp tree on error. Best-effort `remove_dir_all`; if it fails (permissions changed mid-run, race with another process), log and continue. The temp tree's prefix makes orphans visible to operators.

## Tests

The existing `tests/dir_transfer.rs` covers the happy path. New tests to add:

**Failure mid-fetch leaves dest untouched.** Set up a `fetch_dir` against a manifest where one of the source peers is going to fail. Verify that after the failure, `dest` is in exactly the state it was in before the call. If `dest` didn't exist before, it shouldn't exist after. If `dest` existed with content, that content should be unchanged.

**Concurrent fetch into adjacent destinations doesn't collide.** Run two `fetch_dir` calls into `dest_a` and `dest_b` in parallel where `dest_a` and `dest_b` are siblings. Verify both succeed and don't see each other's temp paths.

**Replacement of existing destination preserves old content on failure.** Create `dest` with known content. Run `fetch_dir` against a manifest where the fetch will fail partway. Verify that after the failure, `dest` still contains the original content (not the partial new content, not a missing directory).

**Successful replacement removes old content.** Create `dest` with files `a.txt` and `b.txt`. Run `fetch_dir` with a manifest containing only `a.txt` (different content). After success, `dest` should contain only `a.txt` with the new content, not `b.txt`.

**Temp path cleanup on success and failure.** After `fetch_dir` returns (success or failure), the parent of `dest` should not contain any `.fetch_*` or `.replaced_*` orphans.

The tests run on the existing localhost paired-node setup that `tests/dir_transfer.rs` uses; no new infrastructure.

## What this fix does NOT add

- **No transactional semantics across multiple `fetch_dir` calls.** Each call is atomic on its own; coordinating multiple calls is application-layer concern.
- **No rollback to previous versions.** Once `fetch_dir` succeeds and `.old` is removed, the previous content is gone. Versioning is application-layer concern (probably built later as a composition layer if customers need it; not part of this fix).
- **No concurrent-observer guarantees.** Documented as outside scope above.
- **No Windows-specific rename semantics.** POSIX `rename` is atomic; Windows `rename` has different semantics around existing-destination behavior. The current substrate is POSIX-first; Windows support is best-effort. Worth flagging the platform difference in doc comments but not blocking the fix on it.

## Scope and timing

This is ~50-80 LoC of changes to `fetch_dir`, ~150-200 LoC of new test code. Half a day to a day of focused work, including the doc comment updates and the test phase.

Worth doing before the directory transfer demo at `node_modules` scale, because the demo will run under realistic conditions where partial-state failure modes can happen and the demo's credibility depends on the substrate doing the right thing under failure. Engineers reading the demo and noticing that `fetch_dir` can leave partial state would flag this immediately; fixing it before the demo is what avoids that.

Probably the right time to do this is alongside the SDK plan execution — the SDK exposes `fetch_dir` in five language tiers, and exposing a non-atomic operation through all five would propagate the gap. Better to fix the substrate first, then expose the now-atomic operation through the SDKs.

## Origin

Identified during the SDK plan v2 conversation when checking whether `fetch_dir` already used temp-dir-plus-atomic-rename internally. It does not — Pass 1 creates directories directly at `dest`, Pass 2 writes files directly at `dest/<path>`, Pass 3 creates symlinks directly at `dest/<path>`. Adding atomic semantics is a substrate fix, not an SDK helper, because the gap exists at the substrate layer and every language binding inherits it.
