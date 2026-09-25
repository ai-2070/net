# Stage 0 — executable lifecycle and ownership models

**Authorized by the Stage 0 section of
`docs/internal/plans/ORG_SCOPED_STREAMING_PLAN.md` (revision of 2026-09-19,
Q1–Q7 resolved under delegated authority). Stage 0 only — no Stage 1, no
production wire, behaviour or export change.** Source of truth, in this order:

1. The plan's §2.1/§2.2/§2.6/§2.7 (lifetime, supervision, state model,
   accounting), §3 (the exact-incarnation transaction), the Q1 limits table,
   the Compatibility ledger, and the Stage 0 table plus its "Model check" rows.
2. `AGENTS.md` — the feature-flag trap, the silent-skip trap, witness floors,
   the Windows `cargo fmt` trap, the single-cdylib rule.
3. The source the models stand in for: `cortex/rpc.rs` folds,
   `behavior/org_admission*.rs`, `org_revocation.rs`, `org_admission_gate.rs`,
   `mesh_rpc.rs:1001-1300`.

## Slices as dispatched

| Slice | Owner | Deliverable |
|---|---|---|
| 0.1 Baseline benches | `S0Bench` | `Pair::protected()` in `sdk/benches/nrpc_common`, an `org_unary_open` group in `nrpc_unary.rs`, recorded numbers beside the May-19/June-13 audits, opening cost named separately from steady state |
| 0.2 External consumer probe | `S0Probe` | `guards/org_api_probe/` — own workspace, committed lockfile, pinning the org facade, the typed streaming veneer and the ledger's struct-literal surfaces (C1/C3/C4). CI step text reported to Main, who owns `ci.yml` |
| 0.3 Lifecycle model | Main | `behavior/org_stream_lifecycle.rs` — §2.1 deadline resolution, §2.6 state machine, §2.2 supervisor, plus a runtime-free pull driver |
| 0.4 Transaction model | `S0Registry` | `behavior/org_stream_registry.rs` — reserve/verify/rollback/install/confirm/retire/complete with incarnations, requalification, quotas and byte reservations |
| 0.5 Browser and SDK mapping | `S0Mapping` (read-only) | `docs/internal/spikes/org-streaming/S0_MAPPING.md` |

## Constraints given to every lane

- Rust from `net/crates/net/`; warm aliases only (`cargo tfl` / `cargo tf`) —
  never a hand-written `--features` list.
- `--no-tests=fail` and `--retries 0` on every focused run.
- Windows: `cargo fmt -p <crate>`, never `--all` from the workspace root.
- Ownership: Main owns `ci.yml`, `.config/nextest.toml`, `docs/internal/plans/**`
  and `spikes/**`; each lane owns exactly the files named in its slice.
- Evidence: a raw inverse receipt per property — the bounded diff, the exact
  command, the exit code, the verbatim failure, then the restored pass. A
  described mutation is not a receipt. Legs that never ran on this host are
  stated as such.
- Findings beat workarounds: a plan instruction that is wrong against the
  source is reported with a `path:line` citation, not coded around.

## Stage 0 exit

Both models accepted (not merely delivered), baselines recorded, probe green at
head, mapping delivered, and `docs/internal/spikes/org-streaming/S0_REPORT.md`
written with the executed evidence. Stage 1 remains gated on that acceptance.
