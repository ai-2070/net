# Net v0.27.4 — "Purple Rain"

## A maintenance release — dependency bumps only

After two substantive Purple Rain turns — v0.27.2 (security) and v0.27.3 (performance + the `ring` AEAD swap) — v0.27.4 is a quiet maintenance release. There are **no source changes to the Rust core, no API/ABI changes, and no wire-format change**. The substance lives entirely in `net/crates/net/Cargo.lock`: the Python-binding stack steps to **pyo3 0.29**, `zeroize` to **1.9**, and the rest is routine transitive churn. Drop-in for everyone; honest v0.27.3 / v0.27.2 / v0.27.1 peers are unaffected.

---

## The Python binding completes its pyo3 0.29 migration

The headline of the lock diff: the whole pyo3 stack steps from **0.28.3 → 0.29.0** — `pyo3`, `pyo3-ffi`, `pyo3-macros`, `pyo3-macros-backend`, and `pyo3-async-runtimes` (0.28.0 → 0.29.0). The only consumer is the `net-python` crate; the Rust core, the C/Go/Node FFI, and the wire are all untouched.

This **finishes what v0.27.3 started.** That release bumped only the build-time helper `pyo3-build-config` to 0.29.0, which left the lock carrying *two* copies side by side — 0.28.3 (for the still-0.28.3 `pyo3`) and 0.29.0 (for `net-python`'s direct dependency). v0.27.4 brings the rest of the stack up to 0.29.0, which collapses the lock onto a **single `pyo3-build-config 0.29.0`** and drops `pyo3-macros-backend`'s now-unneeded build-config dependency.

**For Python wheel builders:** rebuild against pyo3 0.29. There is no change to the Python-facing API surface in this release — it's a transitive consequence of the lock bump, not a binding-API change on our side.

---

## Secret-zeroing and build tooling

- **`zeroize` 1.8.2 → 1.9.0** (+ `zeroize_derive` 1.4.3 → 1.5.0) — the crate backing the secure-wipe discipline on identity keys, PSKs, and other secret material. A routine minor bump that keeps the secret-hygiene path current.
- **`cc` 1.2.63 → 1.2.64** — the C-compiler driver. Worth a line only because v0.27.3's `ring` swap put C compilation on the build path (the `zig cc` musl-cross and aarch64-windows-`clang` jobs called out in that release's notes); this keeps that toolchain current. Patch bump.

---

## Routine transitive bumps

None of these reach the datapath; all are pulled transitively by the WASM/browser targets and low-level utilities.

- **WASM / browser toolchain:** `wasm-bindgen` 0.2.123 → 0.2.125 (with `-macro` / `-macro-support` / `-shared`), `js-sys` 0.3.100 → 0.3.102, `web-sys` 0.3.100 → 0.3.102, `wasip2` 1.0.3+wasi-0.2.9 → 1.0.4+wasi-0.2.12.
- **`memchr`** 2.8.1 → 2.8.2.

---

## Breaking changes

**None.** No wire-format change, no API/ABI change, no config change. v0.27.4 interoperates with honest v0.27.3 / v0.27.2 / v0.27.1 peers freely.

---

## How to upgrade

Bump the dependency to `0.27.4` — drop-in, no atomic peer roll, no config changes. The only consumers with anything to do are those building the **Python wheels**, who should rebuild against pyo3 0.29 (a transitive effect of the lock bump, not an API change).

---

## Dependency updates

All in `net/crates/net/Cargo.lock`:

| Crate | From | To | Note |
|---|---|---|---|
| `pyo3` (+ `-ffi`, `-macros`, `-macros-backend`) | 0.28.3 | 0.29.0 | Python binding (`net-python`) |
| `pyo3-async-runtimes` | 0.28.0 | 0.29.0 | — |
| `pyo3-build-config` | 0.28.3 + 0.29.0 | 0.29.0 | duplicate collapsed |
| `zeroize` / `zeroize_derive` | 1.8.2 / 1.4.3 | 1.9.0 / 1.5.0 | secret-zeroing |
| `cc` | 1.2.63 | 1.2.64 | C-compiler driver |
| `wasm-bindgen` (family) | 0.2.123 | 0.2.125 | WASM target |
| `js-sys` / `web-sys` | 0.3.100 | 0.3.102 | WASM target |
| `wasip2` | 1.0.3 | 1.0.4 | WASM target |
| `memchr` | 2.8.1 | 2.8.2 | transitive |

Web/docs-side renovate bumps (eslint, tailwindcss, posthog, better-auth) also landed in the same window via `package-lock.json`; none carry runtime or wire impact and they are out of scope for the Rust crate.

---

Released 2026-06-13.

## License

See [LICENSE](../../LICENSE).
