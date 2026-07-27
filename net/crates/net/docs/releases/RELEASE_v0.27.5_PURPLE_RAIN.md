# Net v0.27.5 — "Purple Rain"

## A version-stamp release — no Rust dependency changes

v0.27.5 is the smallest kind of release. The `net/crates/net/Cargo.lock` diff against v0.27.4 contains **nothing but the workspace version stamp** — every `net-*` crate (`net-mesh`, `net-mesh-sdk`, `net-node`, `net-cli`, `net-python`, `net-aggregator-daemon`, the FFI crates, …) steps 0.27.4 → 0.27.5. **No third-party Rust dependency was added, removed, or upgraded.** The Rust core, the C/Go/Node/Python FFI, the SDK surface, and the wire are byte-for-byte unchanged. Drop-in for everyone.

---

## What actually moved (and why it's out of scope here)

The only dependency churn in this window was documentation/web, via `package-lock.json`: `react-hook-form` → 7.79.0 and a routine renovate lock-file-maintenance pass. Neither touches the Rust crate or any runtime path, so — per the `Cargo.lock`-only scope — there is no release-relevant change to record.

---

## Breaking changes

**None.** No wire-format change, no API/ABI change, no config change. v0.27.5 interoperates with honest v0.27.4 / v0.27.3 / earlier peers freely.

---

## How to upgrade

Bump the dependency to `0.27.5` — pure drop-in. No atomic peer roll, no config changes, nothing to rebuild beyond the version stamp.

---

## Dependency updates

**None in `net/crates/net/Cargo.lock`** beyond the internal workspace version bump (0.27.4 → 0.27.5). Web/docs-side, in `package-lock.json`: `react-hook-form` → 7.79.0 and a lock-file-maintenance pass — out of scope for the Rust crate and without runtime or wire impact.

---

Released 2026-06-13.

## License

See [LICENSE](../../LICENSE-APACHE).
