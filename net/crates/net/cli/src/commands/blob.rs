//! `net blob (put|get|ls|rm)` — dataforts blob CAS operator
//! surface.
//!
//! **Phase 4 status: design stub.** The clap router intentionally
//! does not yet expose `net blob` because the existing `net-blob`
//! binary already serves the full operator surface
//! (`crates/net/src/bin/net-blob.rs`). Per
//! `NET_CLI_PLAN.md:§"Locked decisions"` #10, this CLI is the
//! single consolidation point; `net-blob` becomes a forwarding
//! shim once absorbed.
//!
//! The absorption shape is straightforward — the SDK already
//! exposes `MeshBlobAdapter` (via `net_sdk::dataforts`), so the
//! Phase-4 commit:
//!
//! 1. Builds a `Redex::new().with_persistent_dir(<path>)`.
//! 2. Wraps it in `MeshBlobAdapter::new(...)`.
//! 3. Dispatches to the matching adapter method per subcommand.
//! 4. Emits a typed JSON result on stdout (BlobRef hex on `put`,
//!    raw bytes / file on `get`, inventory rows on `ls`, etc.).
//!
//! The `net-blob` binary at `src/bin/net-blob.rs` does exactly
//! this for the standalone surface; absorption is a routing
//! exercise that copies the per-subcommand handlers into this
//! module + adds them to the clap dispatch in `main.rs`. Pinned
//! shape, no SDK gap.

#![allow(dead_code)]
