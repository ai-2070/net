//! `net port (gateway|probe-peer|try-map)` — port-mapping +
//! reachability helpers.
//!
//! **Phase 2 status: design stub.** Same SDK gap as
//! `commands/rpc.rs` — every variant needs a live `Mesh`
//! instance (port-mapper subscribes to the same classifier
//! task; `probe-peer` wraps `MeshAdapter::probe_reflex`).
//! Lands when the `MeshContext` plumbing arrives.
//!
//! Notes:
//! - `net port gateway` is the simplest variant — it just calls
//!   `default_ipv4_gateway()` + `local_ipv4_for_gateway()` and
//!   doesn't need a Mesh. We could ship that one ahead of the
//!   others by linking through `net_sdk::traversal` re-exports.
//!   The deferred status pins the whole `port` subcommand
//!   together so the operator-visible surface lands as a unit;
//!   shipping `gateway` alone would invite "why doesn't try-map
//!   work?" tickets.
//!
//! Shape pinned in `NET_CLI_PLAN.md §7`.

#![allow(dead_code)]
