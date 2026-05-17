# Bug audit — 2026-05-17 — `net-cli`

Multi-agent bug hunt across `net/crates/net/cli/src/` (~3.6k LOC, 23 Rust files) on branch `netdb-watcher`. Scope was the in-tree net-cli surface as of `21d6f697` (post `net-cli-phase1` merge). Findings are demonstrable defects, not style. Overlaps with `CODE_REVIEW_2026_05_17_NET_CLI_PHASE1.md` are called out where relevant; this doc adds the items that review did not surface.

## Status

- **Remaining Critical:** none — but #1, #3, #4, #5 are data-loss / silent-corruption hazards and should be treated next.
- **Remaining High:** #1, #2, #3, #4, #5, #6, #7
- **Remaining Medium:** #8, #9, #10, #11
- **Remaining Low:** #12, #13, #14, #15, #16
- **Overlap with `CODE_REVIEW_2026_05_17_NET_CLI_PHASE1`:** #9 ↔ B3 (--yes doc/code mismatch).
- **Clean (audited and looked good):** `parse_u64_flexible`, humantime grammar + `checked_mul` on unit multipliers, ICE confirm-gate truth table (TTY/non-TTY × `--yes`), `pin`/`unpin` boolean dispatch in `run_memories_pin`, exit-code mapping table, `DEFAULT_SUPERVISOR_NODE = 1` consistency across the 11 subcommand call-sites, clap subcommand wiring, output JSON/text dispatch, config precedence (`--config` > env > default-path).

## High

1. **`commands/netdb.rs:431-440` — `restore` safety gate swallows I/O errors and proceeds.** The non-empty-store check is `iter.next_entry().await.unwrap_or(None).is_some()`, and the outer arm is `Err(_) => false`. Any transient read_dir failure (permission denied, FS jitter, dest is a regular file, etc.) makes a populated store look empty and lets `restore` overwrite it without `--force`. Match the error explicitly and refuse — the safety gate cannot rely on "treat error as empty."

2. **`commands/netdb.rs:117-118, 457` — `--origin` defaulted to `0` hides cross-origin restore.** `default_value_t = 0` means clap cannot tell "operator forgot `--origin`" from "operator typed `--origin 0`". The docstring (`117-118`) already warns that a mismatched origin causes a silent fold against the wrong chain; the CLI never warns or requires confirmation when origin is defaulted. Fix: change to `origin: Option<u64>` and require an explicit value (or gate the implicit zero behind `--allow-origin-zero`). Until then, the documented "silent cross-origin restore" hazard is reachable by a stray missing flag.

3. **`commands/netdb.rs:575-577` — snapshot write is non-atomic.** `tokio::fs::write(&args.out, &bytes)` overwrites the destination in place. A crash, SIGKILL, full disk, or signal mid-write leaves a truncated postcard blob at the documented path; the next `restore` rejects it and the operator has lost their previous snapshot. Fix: write to `args.out.with_extension("tmp.<pid>")` then `rename` for atomic publish.

4. **`commands/identity.rs:134-140` — seed bytes hit disk before perms are tightened.** `tokio::fs::write(&path, &toml_text)` creates the file with the process umask (typically 0o644 — world-readable) and only afterwards does `enforce_strict_permissions` chmod it 0o600. A concurrent `cat` / backup agent / inotify-watcher in that window reads the seed. Fix on Unix: `OpenOptions::new().mode(0o600).create_new(true).open(...)` so the fd is restrictive at creation, then write into the already-restricted handle. Same atomic-rename hazard as #3 applies on top.

5. **`commands/identity.rs:299-306` — `check_strict_permissions` is a Windows no-op, so the strict-perm contract is silently unenforced on Windows.** Every consumer (`read_identity_file`, `run_show`, `run_fingerprint`, `context::load_identity_keypair`) relies on this gate, but on Windows it returns `Ok(())` regardless of ACLs even when `--insecure-permissions` was NOT passed. The module-header doc that promises the seed-protection contract ("refuses to read a seed file someone else can read, mirroring `ssh`'s permission gate") is a lie on Windows. Minimum fix: stderr-warn unconditionally on Windows reads without `--insecure-permissions`; better fix: consult `GetFileSecurityW` and refuse if the DACL is open.

6. **`commands/netdb.rs:423-440` vs help text:124-129 — `--force restore` actually *merges* the snapshot.** With `--force` the code skips the non-empty check and calls `build_from_snapshot`, which folds chains into whatever's already in `dest`. The verb implies restore-from-snapshot, the help text implies same; the behavior is closer to `merge-snapshot`. Operators who reach for `--force` on a populated store of a *different* origin (combined with #2) will silently merge cross-origin chains. Fix: rename to `merge-snapshot`, or clear `dest` (or refuse) before the fold.

7. **`commands/ice.rs:298, 364-381` — confirm prompt does a synchronous stdin read inside an async fn.** `prompt_for_yes` calls `io::stdin().lock().read_line(...)` while the Tokio runtime is live (started at `CliContext::build`, `257-263`). The blocking read parks a worker thread for as long as the operator stares at the prompt; background SDK tasks (logging dispatcher, mesh ticks) freeze. Fix: `tokio::io::stdin` or `tokio::task::spawn_blocking`.

## Medium

8. **`commands/ice.rs:308-310` — `--sig` parsed *after* the operator has already typed YES.** A malformed `--sig` JSON aborts with `InvalidArgs` post-confirmation, wasting the dual-key ceremony and producing confusing UX. Parse all `--sig` entries up front (ideally before `simulate`) so an argv typo doesn't survive past the gate.

9. **`commands/ice.rs:339-344` doc/code mismatch on `--yes` (overlap with B3 in `CODE_REVIEW_2026_05_17_NET_CLI_PHASE1`).** `CommonIceArgs::yes` (around line 136) doc says `--yes` is required when **stdout** is non-TTY; the code checks **stdin**. Code is right (you can prompt only if stdin is interactive); doc is wrong. Fix the help string.

10. **`commands/netdb.rs:554, 105` and `commands/identity.rs:105` — `Path::exists()` follows symlinks and lies on permission errors.** A symlink at `--out` pointing to a sensitive file is "non-existent" by `exists()` and gets overwritten; a permission error pretends the path is absent. Use `tokio::fs::try_exists` (already used on the read path at `netdb.rs:613`) and distinguish `Ok(false)` from `Err`.

11. **`commands/logs.rs:86-101` and `commands/audit.rs:136-155` — streaming output can deadlock on a stalled consumer.** `emit_stream_row` writes synchronously to `io::stdout()`. If the downstream pipe (e.g. `jq`) stops draining, the write blocks forever with no cancellation arm. Ctrl-C cleanly aborts `stream.next()` but not the stdout write. Fix: wrap stdout writes in a bounded `mpsc` + writer task with a cancellation `select!`, or at minimum document the wedge condition.

## Low

12. **`commands/admin.rs:130-238` — `--dry-run` envelope is built from CLI types, not from the SDK call path.** The recently-added smoke tests check the dry-run *shape*; they do not cross-check byte-for-byte against `deck.admin().<verb>()`'s real on-wire `AdminEvent`. If the SDK ever renames `drain_for_ms` to `drain_for_us` or adds a field, dry-run keeps printing the stale envelope with no compile-time check. Add a `#[test]` that runs a real commit in a test substrate and asserts equality with the dry-run envelope.

13. **`commands/admin.rs:132, :152` — `Duration::as_millis() as u64` is a silent truncating cast.** `as_millis()` returns `u128`. Today the humantime parser bounds the input safely, but `as u64` truncates if the upstream parser changes. Use `u64::try_from(...).unwrap_or(u64::MAX)`.

14. **`commands/daemon.rs:77-82` — `#[serde(flatten)]` collision guarded only by a comment.** If `DaemonSnapshot` ever grows an `id` field the wrapper's `id` silently last-writes-wins. A compile-time guard (build-script field check, or a `const _: () = assert!(...)` once the SDK exposes field reflection) would actually enforce what the comment promises.

15. **`commands/ice.rs:354-359` — `signature_invalid` mapped to `OperatorPolicyRejected` (exit code 5).** A cryptographic-verifier failure under the "policy" exit code will confuse audit readers. Either rename the variant to `VerifierRejected` (and update the doc on `error.rs`) or split a new code. Cheaper to fix now while the contract is fresh.

16. **`commands/ice.rs:36` — double import `CliError, CliError as _CE`.** Compiles, but the two aliases used inconsistently across the file signal an unfinished refactor; readers have to mentally unify them on every error site.

## Methodology

Two parallel general-purpose agents — one on `commands/{netdb,admin,ice,identity}.rs` (the high-churn surface), one on `commands/{audit,blob,cap,daemon,db,logs,peer,port,rpc,snapshot,version,mod}.rs` + `{main,config,context,error,output,parsers,prelude}.rs`. Both read each file in full and cross-referenced helpers when bugs spanned modules. Findings deduped and merged.
