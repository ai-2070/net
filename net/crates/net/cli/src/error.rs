//! Typed exit-code surface. Every subcommand returns
//! `Result<(), CliError>`; `main` maps the typed kind onto the
//! documented exit-code table at `NET_CLI_PLAN.md:§"Exit codes
//! (locked)"`.
//!
//! The kind ↔ code mapping is the consumer contract — scripts
//! `case $? in 3) ...; 4) ...` reliably on it. Broadening a code's
//! meaning requires a major version bump.
//!
//! # Subcommand-specific codes
//!
//! Codes 10–19 are reserved per subcommand. Each subcommand picks
//! a base from this table and offsets within:
//!
//! - `net daemon` — 10 = factory-not-found
//! - `net db`     — 11 = query-parse-failed
//! - `net db`     — 12 = predicate-DSL-parse-failed
//!
//! New subcommand-specific codes get a documented offset under
//! the subcommand's module + a row in this table.

use std::fmt;

/// Documented exit-code table.
///
/// 0 = success; 1 = generic; 2 = invalid args; 3 = SDK error;
/// 4 = ICE simulation blocked; 5 = operator-policy reject;
/// 6 = connection failure; 7 = timeout; 8 = confirmation refused;
/// 10+ = subcommand-specific.
///
/// Every variant is part of the locked operator contract: the
/// numeric discriminators are what consumer scripts `case $?` on.
/// Variants that no current command path constructs (`Success`,
/// `ConnectionFailure`, the `Db*` codes, etc.) still reserve
/// their slot, so the enum carries `#[allow(dead_code)]` to keep
/// the unused-variant lint quiet without weakening the contract.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ExitCodeKind {
    Success = 0,
    Generic = 1,
    InvalidArgs = 2,
    SdkError = 3,
    IceSimulationBlocked = 4,
    OperatorPolicyRejected = 5,
    ConnectionFailure = 6,
    Timeout = 7,
    ConfirmationRefused = 8,
    /// `net daemon` — factory id not registered.
    DaemonFactoryNotFound = 10,
    /// `net db` — query JSON failed to parse.
    DbQueryParseFailed = 11,
    /// `net db` — predicate DSL (`--where` / `--filter`) failed
    /// to parse.
    DbPredicateParseFailed = 12,
    /// `net ice` — a supplied operator signature failed
    /// cryptographic verification. Pre-fix this surfaced under
    /// `OperatorPolicyRejected` (5), conflating
    /// signature-verification failure with policy / quorum
    /// failure. Audit readers seeing exit code 5 expected a
    /// policy gate; the verifier failure shape is distinct
    /// (operator's key + signature pair did not validate against
    /// the simulated envelope).
    IceSignatureInvalid = 13,
}

/// Typed CLI error. Carries the exit-code discriminator + a
/// human-readable message. Subcommands construct these directly;
/// `main` turns the kind into the process exit code.
#[derive(Debug)]
pub struct CliError {
    kind: ExitCodeKind,
    message: String,
}

impl CliError {
    pub fn new(kind: ExitCodeKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// Exit-code discriminator. Returned by `main` after printing
    /// the message.
    pub fn code(&self) -> u8 {
        self.kind as u8
    }

    #[allow(dead_code)]
    pub fn kind(&self) -> ExitCodeKind {
        self.kind
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for CliError {}

// Convenience constructors per kind. Kept as free fns so call
// sites read naturally: `Err(error::generic("foo"))`.

#[allow(dead_code)]
pub fn generic(msg: impl Into<String>) -> CliError {
    CliError::new(ExitCodeKind::Generic, msg)
}

#[allow(dead_code)]
pub fn invalid_args(msg: impl Into<String>) -> CliError {
    CliError::new(ExitCodeKind::InvalidArgs, msg)
}

#[allow(dead_code)]
pub fn sdk(msg: impl Into<String>) -> CliError {
    CliError::new(ExitCodeKind::SdkError, msg)
}

#[allow(dead_code)]
pub fn timeout(msg: impl Into<String>) -> CliError {
    CliError::new(ExitCodeKind::Timeout, msg)
}

#[allow(dead_code)]
pub fn connection_failure(msg: impl Into<String>) -> CliError {
    CliError::new(ExitCodeKind::ConnectionFailure, msg)
}

#[allow(dead_code)]
pub fn confirmation_refused() -> CliError {
    CliError::new(
        ExitCodeKind::ConfirmationRefused,
        "operator declined the confirmation prompt",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_code_table_matches_documented_values() {
        // Locked decisions — see `NET_CLI_PLAN.md:§Exit codes`.
        // Any rename here breaks every consumer script; the test
        // is intentional.
        assert_eq!(ExitCodeKind::Success as u8, 0);
        assert_eq!(ExitCodeKind::Generic as u8, 1);
        assert_eq!(ExitCodeKind::InvalidArgs as u8, 2);
        assert_eq!(ExitCodeKind::SdkError as u8, 3);
        assert_eq!(ExitCodeKind::IceSimulationBlocked as u8, 4);
        assert_eq!(ExitCodeKind::OperatorPolicyRejected as u8, 5);
        assert_eq!(ExitCodeKind::ConnectionFailure as u8, 6);
        assert_eq!(ExitCodeKind::Timeout as u8, 7);
        assert_eq!(ExitCodeKind::ConfirmationRefused as u8, 8);
        assert_eq!(ExitCodeKind::DaemonFactoryNotFound as u8, 10);
        assert_eq!(ExitCodeKind::DbQueryParseFailed as u8, 11);
        assert_eq!(ExitCodeKind::DbPredicateParseFailed as u8, 12);
        assert_eq!(ExitCodeKind::IceSignatureInvalid as u8, 13);
    }
}
