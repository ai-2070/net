//! `--output (json|yaml|ndjson|table|text)` dispatch.
//!
//! Auto-detection rule from `NET_CLI_PLAN.md`: when `--output` is
//! not specified, TTY stdout picks `table`/`text` and non-TTY
//! picks `json`/`ndjson`. The choice between `table` vs `text` and
//! `json` vs `ndjson` is per-subcommand — a one-shot read defaults
//! to `json`/`table`, a streaming read defaults to `ndjson`/`text`.
//!
//! Subcommands call [`OutputFormat::resolve_oneshot`] or
//! `resolve_stream` to pick the effective format from the
//! user-supplied flag + stdout's TTY-ness.

use std::io::{self, Write};

use clap::ValueEnum;
use is_terminal::IsTerminal;
use serde::Serialize;

/// User-facing format choice. The `Default` variant means "let
/// the binary auto-detect"; it's never the chosen format
/// internally — subcommands resolve it via
/// [`Self::resolve_oneshot`] / `resolve_stream`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    /// Single JSON value on stdout, terminated by newline.
    Json,
    /// One JSON object per line. Streaming-friendly.
    Ndjson,
    /// YAML output for large + human-readable structures.
    Yaml,
    /// ASCII / unicode bordered table.
    Table,
    /// Plain text lines.
    Text,
}

impl OutputFormat {
    /// Resolve the effective format for a one-shot read (single
    /// value): user choice wins, otherwise TTY → `table`,
    /// non-TTY → `json`.
    pub fn resolve_oneshot(user: Option<Self>) -> Self {
        match user {
            Some(fmt) => fmt,
            None if io::stdout().is_terminal() => Self::Table,
            None => Self::Json,
        }
    }

    /// Resolve the effective format for a streaming read (one
    /// row per event): TTY → `text`, non-TTY → `ndjson`. Explicit
    /// `table` from the user is honoured (the subcommand
    /// accumulates rows + flushes at end-of-stream).
    pub fn resolve_stream(user: Option<Self>) -> Self {
        match user {
            Some(fmt) => fmt,
            None if io::stdout().is_terminal() => Self::Text,
            None => Self::Ndjson,
        }
    }
}

/// Emit a single serializable value on stdout in the chosen
/// format. `Table` falls back to JSON for arbitrary serde
/// payloads — typed subcommands that want a real table reach
/// for [`emit_table`] directly with `comfy-table`-built rows.
pub fn emit_value<T: Serialize>(fmt: OutputFormat, value: &T) -> io::Result<()> {
    let stdout = io::stdout();
    let mut lock = stdout.lock();
    match fmt {
        OutputFormat::Json | OutputFormat::Table => {
            serde_json::to_writer_pretty(&mut lock, value)
                .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
            writeln!(&mut lock)?;
        }
        OutputFormat::Ndjson => {
            serde_json::to_writer(&mut lock, value)
                .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
            writeln!(&mut lock)?;
        }
        OutputFormat::Yaml => {
            serde_yaml::to_writer(&mut lock, value)
                .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        }
        OutputFormat::Text => {
            // Plain text: render Display via JSON intermediary
            // (so structs still render usefully).
            let s = serde_json::to_string_pretty(value)
                .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
            writeln!(&mut lock, "{}", s)?;
        }
    }
    Ok(())
}

/// Emit one row of a stream. NDJSON / JSON forms write one line;
/// Text / Table forms render the value via Display through a
/// JSON intermediary so subcommands don't need to hand-roll a
/// formatter per payload type.
pub fn emit_stream_row<T: Serialize>(fmt: OutputFormat, row: &T) -> io::Result<()> {
    let stdout = io::stdout();
    let mut lock = stdout.lock();
    match fmt {
        OutputFormat::Ndjson | OutputFormat::Json => {
            serde_json::to_writer(&mut lock, row)
                .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
            writeln!(&mut lock)?;
        }
        OutputFormat::Yaml => {
            // YAML's `---` document separator makes streams
            // human-readable.
            writeln!(&mut lock, "---")?;
            serde_yaml::to_writer(&mut lock, row)
                .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        }
        OutputFormat::Text | OutputFormat::Table => {
            // Stream payloads in TTY mode default to a compact
            // one-line JSON dump — readable without padding the
            // terminal width.
            let s =
                serde_json::to_string(row).map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
            writeln!(&mut lock, "{}", s)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_honours_explicit_choice() {
        assert_eq!(
            OutputFormat::resolve_oneshot(Some(OutputFormat::Yaml)),
            OutputFormat::Yaml
        );
        assert_eq!(
            OutputFormat::resolve_stream(Some(OutputFormat::Json)),
            OutputFormat::Json
        );
    }
}
