# Net v0.29.1 — "Summer of '69"

*Same Bryan Adams first-real-six-string as [v0.29](RELEASE_v0.29_SUMMER_OF_69.md) — one more coat of wax on the guitar, not a new song. A maintenance patch: version hygiene, a dependency refresh, one SDK rename, and comment/doc cleanup. No new mesh behavior.*

## What's in it

v0.29.1 is a **patch release with no functional change to the scheduler or any mesh code path.** The drift scorer that headlined v0.29 is byte-identical here — the only Rust `src` edits are comment corrections. Everything else is version bumps, dependencies, docs, and one TypeScript type rename.

---

## Version bump

`0.29.0 → 0.29.1`, propagated across every manifest — crate, CLI, deck, SDK, and the Go/Node/Python bindings — and their lockfiles (`Cargo.lock`, `fuzz/Cargo.lock`, `uv.lock`, `web/package-lock.json`).

---

## Dependency updates

Where v0.29 declared *"None,"* this patch is mostly the opposite — a routine dependency refresh is the bulk of the release.

**Rust**

- `chacha20poly1305` `0.10 → 0.11` — the optional packet-path AEAD (RFC 8439 ChaCha20-Poly1305).
- `redis` → `1.3.0`
- `arc-swap` → `1.9.2`
- `indicatif` → `0.18.5`
- Rust toolchain `1.96.0 → 1.96.1` (`rust-toolchain.toml`).

---

## Docs & comments

- Dropped the stale *"in Phase 2"* future-tense framing from the drift-scorer comments in `event_loop.rs` / `scheduler.rs` — cadence + dirty-bit gating already shipped in v0.29, so the deferral wording was wrong. Comment-only; the code is unchanged.
- Synced the `LocalScheduler` / `ScoreHistory` / `ScoreSnapshot` sketch in [`MESH_SCHEDULER_IMPL_PLAN.md`](../plans/MESH_SCHEDULER_IMPL_PLAN.md) to the as-built types.

---

## Breaking changes

One, and it affects **TypeScript SDK consumers only**:

- The exported type `GpuSelectionPolicy` is renamed to **`SelectionPolicy`** (`@net-mesh/sdk`, `mesh.ts` / `index.ts`) — finishing the "gang" rename away from GPU-specific naming. The underlying union (`'least_loaded' | 'pack' | 'load_band' | 'lowest_id'`) and the runtime behavior are unchanged; only the type's name moved. If you import it by name, update the import:

  ```diff
  - import type { GpuSelectionPolicy } from '@net-mesh/sdk';
  + import type { SelectionPolicy } from '@net-mesh/sdk';
  ```

---

## How to upgrade

Bump the version and rebuild. No scorer changes, no config migration, no wire-format concern. TypeScript SDK users importing `GpuSelectionPolicy` rename it to `SelectionPolicy`; everyone else pulls clean.

---

Released 2026-06-30.

## License

See [LICENSE](../../LICENSE).
