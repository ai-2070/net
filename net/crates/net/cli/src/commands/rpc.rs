//! `net rpc (call|stream|discover|services)` — typed-RPC client
//! surface.
//!
//! **Phase 2 status: design stub.** The clap router intentionally
//! does not yet expose these subcommands. nRPC routing
//! (`MeshAdapter::call_service`, `find_service_nodes`,
//! `call_streaming`) requires a live `Mesh` instance — a real
//! UDP-bound socket plus the NAT-classifier task — not the
//! in-process `MeshOsDaemonSdk` Phase 1's commands use. The two
//! runtimes are structurally distinct:
//!
//! - `MeshOsDaemonSdk` (Phase 1) — a fold over substrate events;
//!   no socket, no peer transport. Suitable for snapshot reads
//!   and admin commits.
//! - `Mesh` — the actual transport: encrypted UDP + reflex
//!   probes + capability index + nRPC dispatcher. Required for
//!   any peer-touching command.
//!
//! Wiring the CLI to a real `Mesh` requires:
//! 1. An `endpoint` profile knob that points at the bind addr +
//!    PSK + bootstrap peers.
//! 2. A `MeshContext` analogue to `CliContext` that constructs
//!    via `MeshBuilder::bind(addr, &psk)`.
//! 3. Lifecycle handling so the mesh classifier + traversal tasks
//!    shut down cleanly on Ctrl-C.
//!
//! The same plumbing also unblocks `net cap announce`,
//! `net peer (reflex|nat|reclassify-nat|set-reflex|clear-reflex)`,
//! and `net port (probe-peer|try-map)` — those are all Mesh
//! operations.
//!
//! Phase 2 ships admin commits + netdb mutations (no Mesh
//! needed); this stub pins the design so a follow-up enables
//! the full operator surface once the `MeshContext` lands.
//!
//! Shape pinned in `NET_CLI_PLAN.md §5` (nRPC client surface).

#![allow(dead_code)]
