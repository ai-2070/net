//! Subcommand implementations.
//!
//! One module per top-level subcommand. Each module exposes a
//! `run(...)` function that takes the parsed argv struct and
//! returns `Result<(), CliError>`.
//!
//! Phase 1 wires `version` first so the binary builds + the clap
//! router is exercised; the read-only substrate-driven commands
//! (`identity`, `snapshot`, `audit`, `log`, `failures`, `daemon`,
//! `cap`, `peer`, `port`, `db`, `netdb`) land in subsequent
//! commits.

pub mod version;
