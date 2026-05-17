//! Common imports for command modules.
//!
//! Each `commands/<name>.rs` does `use crate::prelude::*;` to
//! avoid restating the SDK-type import block at the top of every
//! file. Keep this module a re-export curtain only — no actual
//! logic. The SDK exposure here is intentionally narrow: just
//! the types the read-only Phase 1 surface needs.

#[allow(unused_imports)]
pub(crate) use crate::error::{CliError, ExitCodeKind};
#[allow(unused_imports)]
pub(crate) use crate::output::{emit_stream_row, emit_value, OutputFormat};
