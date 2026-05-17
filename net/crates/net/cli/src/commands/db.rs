//! `net db (run|latest|between|tail|filter|aggregate|plan)` —
//! MeshDB federated query plane.
//!
//! **Phase 1 status: surface stub.** The clap router intentionally
//! does not yet expose these subcommands. The MeshDB executor
//! (`LocalMeshQueryExecutor`) needs a `ChainReader` impl that
//! tails the substrate's chain store; the SDK doesn't expose
//! `MeshOsRuntime::chain_reader()` (or equivalent) today, so the
//! CLI can't construct a working executor against the running
//! supervisor without reaching past the SDK's customer-facing
//! surface.
//!
//! Follow-up: add `MeshOsRuntime::chain_reader() -> Arc<dyn
//! ChainReader>` to `net_sdk::meshos`, then wire this module's
//! subcommands behind a `Command::Db` variant. The shape is
//! pinned in `NET_CLI_PLAN.md §8`.

#![allow(dead_code)]
