# Code review — `net-cli-phase1` branch

**Date:** 2026-05-17
**Branch:** `net-cli-phase1`
**Base:** `master`
**Scope:** 27 files, +4,353 / −16 LOC, 12 commits ahead.

New `net/crates/net/cli` crate adding the `net` binary — the unified operator CLI planned in `NET_CLI_PLAN.md`. Phase 1 ships the read-only surface (identity, snapshot, audit, log/failures, cap, peer ls, daemon ls), Phase 2 layers in admin commits + netdb mutations, Phase 3 stubs in ICE; rpc / port / db / blob / daemon-run modules ship as documented design stubs.

Overall structure is sound: `clap` derives drive argv, typed `CliError` maps onto a locked exit-code table, lazy `CliContext` builds the SDK + identity once per invocation, `--output (json|yaml|ndjson|table|text)` auto-detects TTY behavior, integration tests pin the help surface + a subset of exit codes. The plan-vs-implementation traceability is unusually disciplined — every deferred surface has a stub module explaining the SDK gap.

---

## Concerns

### B1. `identity.rs:489` — test assertion contradicts its own comment

```rust
#[test]
fn iso8601_formats_known_timestamp() {
    // 2026-05-17T12:34:56Z = 1763382896
    assert_eq!(format_iso8601_utc(1763382896), "2025-11-17T12:34:56Z");
}
```

`1763382896` seconds since the Unix epoch is `2025-11-17T12:34:56Z` (the assertion is correct); the comment claims `2026-05-17T12:34:56Z`. The test passes, but the documentation lies. Either correct the comment to `2025-11-17` or pick a real `2026-05-17` epoch (`1779208496`) and update the assertion.

---

### B2. `netdb.rs:522–525, 552–555` — `TaskRow`/`MemoryRow` shove debug-printed structs into named fields

```rust
guard
    .all()
    .map(|t| TaskRow {
        id: format!("{:?}", t.id),
        title: format!("{:?}", t),       // <-- whole task struct, not title
    })
    .collect()
```

```rust
guard
    .all()
    .map(|m| MemoryRow {
        id: format!("{:?}", m.id),
        summary: format!("{:?}", m),     // <-- whole memory struct, not summary
    })
    .collect()
```

The JSON column called `title` actually contains the full task struct's debug repr; `summary` contains the full memory struct. Almost certainly not what `net netdb tasks ls --output json | jq '.[].title'` consumers expect.

Fix: pull the real fields (`t.title`, a real summary projection of `m`). If those types already derive `Serialize`, just emit the SDK value directly.

---

### B3. `ice.rs:296–312` — comment and code disagree on `--yes` semantics on TTY

```rust
// Confirmation gate. Non-TTY without `--yes` → exit 8.
// TTY: require typed `YES` even with `--yes` to keep the
// dual-key feel; `--yes` skips the prompt only when stdout
// is non-TTY.
let stdin_is_tty = is_terminal::IsTerminal::is_terminal(&io::stdin());
if !common.yes {
    if !stdin_is_tty {
        return Err(_CE::new(
            ExitCodeKind::ConfirmationRefused,
            "stdin is not a TTY; pass --yes to skip the interactive confirm prompt",
        ));
    }
    // Stdin is a TTY: prompt for typed YES.
    if !prompt_for_yes()? {
        return Err(crate::error::confirmation_refused());
    }
}
```

The comment says *"TTY: require typed `YES` even with `--yes`"*. The code says `if !common.yes { … }`, so `--yes` *always* skips the typed prompt regardless of TTY-ness. For an ICE break-glass surface the dual-key behavior matters; pick one and make the comment + code agree.

---

### B4. `context.rs:71–78` — ephemeral keypair fallback silently applies to admin/ICE writes

```rust
let keypair = match identity_override.or(profile.identity.as_deref()) {
    Some(path) => load_identity_keypair(path).await?,
    None => {
        tracing::warn!(
            "no operator identity configured; using an ephemeral \
             keypair. Run `net identity generate --out <PATH>` and \
             point your profile at the result for stable operator id."
        );
        EntityKeypair::generate()
    }
};
```

Read-only Phase 1 commands tolerate this. But `admin.rs` and `ice.rs` build the same `CliContext`, so a missing identity produces a `warn!` and proceeds to sign + commit with a throwaway key — and `-q/--quiet` suppresses the warning entirely. The audit ring then carries commits attributed to an operator id nobody can trust.

Fix: add a `require_identity: bool` parameter (or a separate constructor) so `admin::handle` and `ice::run_ice` refuse to proceed without a real identity. Reads keep the ephemeral fallback.

---

## Design notes

### D1. Debug-formatted enums leaking into stable JSON output

- `peer.rs:45–46`: `health: p.health.map(|h| format!("{h:?}"))`, same for `maintenance`.
- `daemon.rs:64`: `name: format!("{:?}", d)` — entire daemon snapshot stuffed into the `name` field.

`PeerSnapshot` and `DaemonSnapshot` already `derive(Serialize)` (see `src/adapter/net/behavior/meshos/snapshot.rs:210, 360`). The CLI can either emit them directly or define an explicit string mapping like the Node binding does at `bindings/node/src/meshos.rs:296` (where `health: Option<String>` is filled by an explicit conversion, not a `Debug` impl). `format!("{:?}", _)` is not a stable serialization contract.

### D2. `identity.rs:364–475` reimplements SHA-256 in 100+ lines to avoid pulling `sha2`

The justification (`// pulling sha2 would be the production choice…`) is candid but the cost/benefit doesn't add up. `sha2` is already in the SDK's tree, the `SimpleSha256` wrapper around `sha256_oneshot` adds a `Box<dyn FnMut>` for no reason, and a hand-rolled crypto primitive is worse than a vetted one even when not on a hot path. Either pull `sha2` or reuse `blake3` (already in the workspace).

### D3. `version.rs:19` — `sdk_version: "0.17.0"` is a hardcoded string literal

```rust
// Embed the SDK version the binary is linked against.
// The SDK's `Cargo.toml:version` always tracks the
// workspace version, so this is also the substrate
// version.
sdk_version: "0.17.0",
```

The comment claims it "tracks the workspace version" — it doesn't; nothing keeps it in sync. The first SDK bump without a CLI bump silently lies to `net version` consumers.

Fix: re-export `pub const VERSION: &str = env!("CARGO_PKG_VERSION");` from the SDK crate and reference it here, or pull the value through a `build.rs` constant.

### D4. `admin.rs:14, 373–375` — `SystemTime` import + dummy suppressor function

```rust
use std::time::{Duration, SystemTime, UNIX_EPOCH};
// ...
#[allow(dead_code)]
fn _systemtime_use(_t: SystemTime) {}
```

`SystemTime` isn't referenced by name (only `UNIX_EPOCH` is, plus the value returned by `committed_at()`). Drop the import; drop `_systemtime_use`.

### D5. `logs.rs:60–62` parses `--min-level` after spinning up the substrate

```rust
let profile = resolve_profile(config_path, profile_name).await?;
let ctx = CliContext::build(&profile, args.identity.as_deref(), args.node).await?;

let mut filter = LogFilter::new();
if let Some(level_str) = args.min_level.as_deref() {
    filter = filter.min_level(parse_log_level(level_str)?);
}
```

The corresponding test (`exit_codes.rs:65 — code_2_on_invalid_log_level`) pays the SDK boot cost just to fail flag validation. Move `parse_log_level` above `CliContext::build`; the SDK only needs to start once the filter is known-valid.

### D6. `netdb.rs:599 open_netdb` creates the store directory on every call, including read-only `ls`

```rust
tokio::fs::create_dir_all(&path).await.map_err(|e| {
    generic(format!(
        "failed to create netdb directory {}: {e}",
        path.display()
    ))
})?;
```

A typo'd `--store /var/tmp/typo` silently produces an empty store and `net netdb tasks ls` returns zero rows with no diagnostic. Read paths should fail explicitly when the dir doesn't exist; only mutations / snapshot / restore should create.

### D7. `netdb.rs:528, 558` vs `253, 273, …` — `try_tasks() == None` is silent on reads, hard error on writes

Reads (`run_tasks_ls`, `run_memories_ls`) treat a missing adapter as an empty list:

```rust
let tasks: Vec<TaskRow> = match netdb.try_tasks() {
    Some(adapter) => { /* ... */ }
    None => Vec::new(),
};
```

Writes raise `sdk("NetDB has no tasks adapter wired")`. Inconsistent — pick one. Either reads emit a diagnostic on stderr explaining the adapter isn't enabled, or writes silently succeed when the adapter is absent.

### D8. `main.rs:244–335` ships a hand-rolled `humantime` parser with a misleading rationale

```rust
// `humantime` is brought in transitively via tracing-subscriber's
// env-filter machinery; expose it as a tiny shim so the `--timeout`
// flag's `value_parser` resolves without an extra direct dep.
mod humantime { /* ... */ }
```

`tracing-subscriber` does not re-export `humantime` — this is a fresh hand-rolled parser. The parser itself is fine and has unit-test coverage. Either add the real `humantime` crate to `[dependencies]` (negligible build cost) or drop the misleading comment.

### D9. `ice.rs:289–290 + 364` — preview JSON and the `Type YES to confirm:` prompt both go to stdout

```rust
emit_value(OutputFormat::resolve_oneshot(output), &preview)
    .map_err(|e| generic(format!("write ICE preview: {e}")))?;
// ...
write!(stdout, "Type YES to confirm ICE commit: ")
```

Mixing prompts with structured data on the same stream breaks `net ice … | jq` pipelines and confuses readers when the typed `YES` echoes between JSON output and the commit result. Send the prompt to stderr; keep stdout pure data.

---

## Smaller / cosmetic

- **C1.** `output.rs:44` uses the `is-terminal` crate; `std::io::IsTerminal` has been in stdlib since Rust 1.70 — one fewer dep.
- **C2.** `netdb.rs:419` `use net_sdk::cortex::{NetDb, Redex};` inside `run_restore` shadows the file-top import on line 17.
- **C3.** `logs.rs:55` accepts `-f/--follow` then `let _ = args.follow;` (no-op). Reasonable for `tail -f` parity, but document the always-on behavior more visibly in `--help` — silent acceptance can surprise.
- **C4.** `admin.rs:207–219` clones `chains` twice; once is enough.
- **C5.** `exit_codes.rs:34–44` uses a Unix-style absolute path `/this/path/definitely/does/not/exist.toml`. Works on Windows (resolves to current drive root, still nonexistent) but a weird choice for a cross-platform suite. `tempfile::tempdir()` + a path inside it would be portable and immune to a future Windows operator named "this" with a "path" folder.
- **C6.** `parse_u64_flexible` is duplicated across `admin.rs`, `ice.rs`, and `netdb.rs`. Centralize in `prelude` or a small `parsers.rs` if a fourth caller appears — three is borderline.
- **C7.** Phase 1 exit-code coverage gaps (codes 4–8, 10–12) are acknowledged in `exit_codes.rs`'s header. Codes 4 (`IceSimulationBlocked`) and 8 (`ConfirmationRefused`) are pinnable today using the in-process substrate + a piped non-TTY stdin; worth adding before Phase 2 grows the surface.

---

## Strengths to preserve

- Per-stub module documentation: every deferred surface (`rpc.rs`, `port.rs`, `db.rs`, `blob.rs`, parts of `daemon.rs`) explains its SDK gap and the unblocking work. Keep doing this in later phases — it's load-bearing for follow-up authors.
- `OutputFormat::resolve_oneshot` / `resolve_stream` correctly auto-detects TTY behavior for one-shot vs streaming reads.
- Exit-code table is locked behind a test (`error.rs:122–139`) that pins the discriminator values — protects scripting consumers from accidental renames.
- `CliContext::build` is one-shot per invocation; long-running watches reuse the same context for their lifetime without re-spinning the substrate.

---

## Priority order before merge

1. **B2** — broken JSON shape; `tasks ls` / `memories ls` are user-visible Phase 1 surfaces.
2. **B3** — `--yes` ambiguity on an ICE break-glass surface.
3. **B4** — admin/ICE writes proceeding under an ephemeral key.
4. **B1** — test docstring lies about what it asserts.

Everything else is design polish that can be addressed alongside Phase 2.

---

# Second pass — 2026-05-17

All 21 first-pass items addressed (B1–B4, D1–D9, C1–C7). The fresh pass below re-reads the tree post-fixes and surfaces what the first pass missed.

## Fresh concerns

### F1. `ice.rs:341–343` — `e_kind` wraps a value that's never `None`

```rust
fn e_kind(e: &net_sdk::deck::IceError) -> Option<&'static str> {
    Some(e.kind)
}
```

`IceError` is an alias for `DeckError`, which has `pub kind: &'static str` (`adapter/net/behavior/deck.rs:145–149`). The `Option` layer is dead — `map_ice_error` matches `Some(...)` for every dispatched arm. Take `&'static str` directly and inline `e.kind` at the two call sites.

### F2. `version.rs:30` ignores the global `--output` flag

```rust
emit_value(OutputFormat::Json, &info)
```

`main.rs:193` dispatches `Command::Version => commands::version::run().await` with no `output` argument, so `--output yaml net version` silently emits JSON. Either thread `output: Option<OutputFormat>` through and call `resolve_oneshot`, or update the misleading "predates the `--output` global flag" comment.

### F3. `tests/help.rs:38` hardcodes `0.17.0`

```rust
.stdout(predicate::str::contains("0.17.0"));
```

The next `Cargo.toml:version` bump silently breaks the test. Substitute `env!("CARGO_PKG_VERSION")` so the assertion tracks the crate version.

### F4. `Cargo.toml:71` — `bytes = "1"` is unused

No file under `cli/src/**` references `bytes::` or `use bytes`. The comment claims "Bytes / hex utilities for identity material" but only `hex` is actually used. Drop the dep.

### F5. `main.rs:169` — `color-eyre` is installed but never propagates anything

```rust
if let Err(e) = color_eyre::install() {
    eprintln!("net: failed to install error reporter: {e}");
    return ExitCode::from(1);
}
```

Nothing returns `eyre::Result` and nothing builds a `Report` — every error path uses the typed `CliError`. The installer + the `color-eyre` dep are unused. Drop it, or wire it to actually carry SDK error chains under `-vv`.

### F6. `tests/exit_codes.rs:85–110 code_8_on_ice_confirmation_refused_non_tty` pays substrate boot + couples to simulation

The test boots a `MeshOsDaemonSdk`, calls `proposal.simulate()`, renders the preview JSON, then fails the confirmation gate to assert code 8. If the substrate ever tightens its admin-verifier gate at simulate time, this test starts returning code 5 (OperatorPolicyRejected) without anything else changing. Cheaper fixture: pin code 8 against a path that only exercises the confirmation gate (e.g., a unit test on `prompt_for_yes()` reading `Stdio::null()`).

### F7. `humantime::parse_duration` accepts surprising inputs

```text
"10 5s"  → 105s   (whitespace eats the unit boundary)
"1m5"    → 65s    (trailing digits get a default `s` unit)
"1m1m"   → 120s   (silently sums duplicate units)
```

The `--help` text claims `1h30m`-style syntax, but the parser is lazier than the documented grammar. Tighten the grammar (no internal whitespace; require a unit on every numeric component) or expand the docstring. Also: `Duration::from_secs(value * 60 * 60 * 24)` wraps silently in release mode at extreme inputs — `checked_mul` would be cleaner.

### F8. Phase 1 / 2 / 3 mutations have zero integration-test coverage

Integration tests only pin exit codes 0, 1, 2, 8. Admin commits (9 verbs), netdb mutations (10 ops), and ICE commits (7 factories) have no end-to-end test. Add one smoke test per category using `--dry-run` round-trips against the in-process supervisor — cheap to write, pins the JSON envelope shape.

### F9. `daemon.rs:73–77` — `#[serde(flatten)]` collision risk worth a doc comment

```rust
#[derive(Serialize)]
struct DaemonRow {
    id: u64,
    #[serde(flatten)]
    snapshot: DaemonSnapshot,
}
```

`DaemonSnapshot` today (`adapter/net/behavior/meshos/snapshot.rs:211–229`) has no `id` field, so the flatten is safe right now. A future SDK bump that adds `pub id: u64` would produce duplicate JSON keys (serde silently allows — last write wins). Add a one-line guard comment so the next reader knows.

### F10. `ice.rs:298–308` — flatten the nested `if` for readability

clippy doesn't fire (the outer has an `else if`), but the nested-if-without-else reads awkwardly. Equivalent and flatter:

```rust
if !stdin_is_tty && !common.yes {
    return Err(...);
}
if stdin_is_tty && !prompt_for_yes()? {
    return Err(...);
}
```

## Smaller / cosmetic (second pass)

- **`admin.rs:100`, `ice.rs:154`, `netdb.rs:198`** — `use crate::parsers::parse_u64_flexible;` is buried mid-file, after the attribute macros that use it. Compiles fine (attribute-name resolution sees the import regardless of placement), but stylistically belongs in the file-top import block.
- **11 subcommands** each declare `#[arg(long, default_value_t = 0x0001)] pub node: u64`. A `pub(crate) const DEFAULT_SUPERVISOR_NODE: u64 = 1;` in `prelude` would document the convention.
- **`netdb.rs:418–467 run_restore`** never checks that `args.origin` matches the snapshot's origin — silent cross-origin restore. Add a doc-comment caveat at minimum (the SDK type doesn't expose the origin today).
- **`netdb.rs:603–612`** — `try_exists(...).unwrap_or(false)` swallows permission errors and reports them as "store does not exist". A permission denial gets the wrong remediation message.
- **`identity.rs:299–301`** — `check_strict_permissions` is a silent no-op on Windows. The companion `enforce_strict_permissions` documents the design ("Operators on Windows manage NTFS ACLs out-of-band"); the check-side should mirror it.

## Priority order before merge (second pass)

1. **F4** — drop unused `bytes` dep (one-line).
2. **F3** — switch the hardcoded `"0.17.0"` test assertion to `env!`.
3. **F2** — decide: thread `--output` into `version`, or update the misleading comment.
4. **F1** — flatten the no-op `Option` around `IceError::kind`.

Phase-2 friendly (no merge block):
- **F5** — decide whether to wire `color-eyre` end-to-end or remove it.
- **F8** — add `--dry-run` smoke tests for admin/netdb/ICE verbs.
- **F7** — tighten or document the humantime parser's accepted grammar.

Verified clean: every B/D/C item from the first pass.
