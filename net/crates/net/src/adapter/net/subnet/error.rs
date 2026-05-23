//! Errors returned by fallible subnet constructors.
//!
//! Pre-existing `SubnetPolicy::add_rule`, `SubnetRule::map`,
//! and `SubnetId::new` panic on out-of-range input. Subnet
//! configuration typically comes from config / FFI / JSON and a
//! malformed entry should not crash the daemon loader. The
//! `try_*` constructors return [`SubnetError`] instead.

use thiserror::Error;

/// Validation error returned by the fallible subnet constructors.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SubnetError {
    /// Hierarchy level is outside `[0, 3]`.
    ///
    /// Returned by [`super::SubnetPolicy::try_add_rule`].
    #[error("subnet rule level must be 0..=3, got {got}")]
    LevelOutOfRange {
        /// The out-of-range level value the caller supplied.
        got: u8,
    },

    /// `level_value == 0` is reserved for "unmatched / no
    /// restriction at this level" and must not appear as an
    /// explicit mapping.
    ///
    /// Returned by [`super::SubnetRule::try_map`].
    #[error("subnet rule level_value must be in 1..=255 (0 is reserved for unmatched levels)")]
    LevelValueReserved,

    /// More hierarchy levels were supplied than the encoding
    /// supports (`MAX_DEPTH = 4`).
    ///
    /// Returned by [`super::SubnetId::try_new`].
    #[error("SubnetId supports at most {max} levels, got {got}")]
    TooManyLevels {
        /// Number of levels the caller supplied.
        got: usize,
        /// Hard cap from `super::id::MAX_DEPTH`.
        max: u8,
    },

    /// String form did not parse as a [`super::SubnetId`].
    ///
    /// Returned by `SubnetId::from_str`. Carries the original
    /// input and a short reason so the operator sees what
    /// rejected the value.
    #[error("SubnetId could not parse `{input}`: {reason}")]
    ParseFailed {
        /// The string the caller supplied.
        input: String,
        /// Short human-readable reason for the rejection.
        reason: String,
    },
}
