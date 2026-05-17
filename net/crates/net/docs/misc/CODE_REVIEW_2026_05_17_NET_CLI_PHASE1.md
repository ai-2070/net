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
