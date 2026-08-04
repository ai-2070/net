//! `net gateway (stats|exports|export)` — surface the local
//! mesh node's `SubnetGateway` state.
//!
//! `stats` rolls up `local_subnet`, forwarded/dropped counters,
//! peer-subnet list, and export-rule count into a single
//! [`GatewayStats`] row.
//!
//! `exports` enumerates the gateway's export table as
//! `(channel_hash, channel_name?, target_subnets[])` rows.
//!
//! `export <channel> <target-subnet>...` adds (or replaces) an
//! export rule. The channel argument is the canonical name
//! (preferred — hashed directly to its canonical `ChannelHash`;
//! digit-only names like `66` are names, never decimal literals),
//! or the one literal form: exactly lowercase `0x` + 16 lowercase
//! hex digits, as `exports` renders. Hex-looking input at any
//! other width or case is refused — a short value is
//! indistinguishable from the collidable 16-bit wire hint, and
//! export rules are channel policy.
//!
//! Shape pinned in `SCALING_SUBNET_SPEC.md` Phase A.

use std::path::PathBuf;

use clap::{Args, Subcommand};
use net_sdk::deck::GatewayStats;
use net_sdk::subnets::SubnetId;
use net_sdk::ChannelName;
use serde::Serialize;

use crate::context::{resolve_profile, CliContext};
use crate::error::{generic, invalid_args, CliError};
use crate::prelude::{emit_value, OutputFormat};

#[derive(Subcommand, Debug)]
pub enum GatewayCommand {
    /// Aggregate gateway counters + local subnet + peer-subnet list.
    Stats(StatsArgs),
    /// Enumerate the gateway's export table.
    Exports(ExportsArgs),
    /// \[preview\] Add an explicit export rule for a channel.
    /// Today this validates flags then errors — the mutate path
    /// needs a write-capable mesh handle that the read-only CLI
    /// doesn't own.
    Export(ExportArgs),
}

#[derive(Args, Debug)]
pub struct StatsArgs {
    #[arg(long)]
    pub identity: Option<PathBuf>,

    #[arg(long, default_value_t = crate::prelude::DEFAULT_SUPERVISOR_NODE)]
    pub node: u64,
}

#[derive(Args, Debug)]
pub struct ExportsArgs {
    #[arg(long)]
    pub identity: Option<PathBuf>,

    #[arg(long, default_value_t = crate::prelude::DEFAULT_SUPERVISOR_NODE)]
    pub node: u64,
}

#[derive(Args, Debug)]
pub struct ExportArgs {
    /// Channel name (preferred; digit-only names are names) OR the
    /// full canonical hash exactly as `exports` renders it:
    /// lowercase `0x` + 16 lowercase hex digits. Other hex-looking
    /// forms are refused.
    pub channel: String,
    /// Target subnets to export to. At least one required.
    /// Format: `region.fleet.unit[.subsystem]` (e.g. `3.7.2`) or
    /// `global`.
    #[arg(required = true)]
    pub targets: Vec<String>,

    #[arg(long)]
    pub identity: Option<PathBuf>,

    #[arg(long, default_value_t = crate::prelude::DEFAULT_SUPERVISOR_NODE)]
    pub node: u64,
}

pub async fn run(
    cmd: GatewayCommand,
    output: Option<OutputFormat>,
    config_path: Option<&std::path::Path>,
    profile_name: &str,
) -> Result<(), CliError> {
    match cmd {
        GatewayCommand::Stats(args) => run_stats(args, output, config_path, profile_name).await,
        GatewayCommand::Exports(args) => run_exports(args, output, config_path, profile_name).await,
        GatewayCommand::Export(args) => run_export(args, output, config_path, profile_name).await,
    }
}

async fn run_stats(
    args: StatsArgs,
    output: Option<OutputFormat>,
    config_path: Option<&std::path::Path>,
    profile_name: &str,
) -> Result<(), CliError> {
    let profile = resolve_profile(config_path, profile_name).await?;
    let ctx = CliContext::build(&profile, args.identity.as_deref(), args.node, false).await?;
    let view = match ctx.deck().gateway_stats() {
        Some(stats) => StatsView::installed(&stats),
        None => StatsView::not_installed(),
    };
    emit_value(OutputFormat::resolve_oneshot(output), &view)
        .map_err(|e| generic(format!("write gateway stats: {e}")))?;
    Ok(())
}

async fn run_exports(
    args: ExportsArgs,
    output: Option<OutputFormat>,
    config_path: Option<&std::path::Path>,
    profile_name: &str,
) -> Result<(), CliError> {
    let profile = resolve_profile(config_path, profile_name).await?;
    let ctx = CliContext::build(&profile, args.identity.as_deref(), args.node, false).await?;
    let deck = ctx.deck();
    let rows: Vec<ExportRow> = deck
        .gateway_exports()
        .into_iter()
        .map(|(channel_hash, targets)| ExportRow {
            // Full canonical width, always: `0x` + 16 hex digits.
            // This is the exact string `export` accepts back, so an
            // operator can round-trip a row without hand-widening.
            channel_hash: format!("{channel_hash:#018x}"),
            target_count: targets.len() as u64,
            targets: targets.iter().map(|s| s.to_string()).collect(),
        })
        .collect();
    emit_value(OutputFormat::resolve_oneshot(output), &rows)
        .map_err(|e| generic(format!("write gateway exports: {e}")))?;
    Ok(())
}

async fn run_export(
    args: ExportArgs,
    _output: Option<OutputFormat>,
    _config_path: Option<&std::path::Path>,
    _profile_name: &str,
) -> Result<(), CliError> {
    // Validate inputs up-front so the operator sees concrete
    // parse errors rather than a generic "not supported." The
    // mutate path itself is gated on a write-capable mesh handle
    // that the read-only `DeckClient` doesn't carry today.
    let _channel_hash = parse_channel_hash(&args.channel)?;
    for target in &args.targets {
        let _ = parse_subnet(target)?;
    }
    Err(invalid_args(
        "net gateway export is read-validation-only today: arguments parse but \
         the substrate mutate path requires a write-capable mesh handle that the \
         CLI's read-only DeckClient doesn't own. Set the export rule via the \
         operator daemon's config or the substrate's `SubnetGateway::export_channel` \
         API directly until the write-attach surface lands.",
    ))
}

/// Parse a channel arg into a CANONICAL `ChannelHash` (u64).
///
/// Export rules are channel policy, and policy is keyed on the
/// canonical `u64` — never the 16-bit wire hint, which is documented
/// as a fast-path filter with routine collisions. Accepting a hint
/// here would let an operator install one channel's targets onto
/// every other channel in the same bucket.
///
/// The rule, unambiguous by construction:
///
/// - **Exactly** lowercase `0x` + 16 lowercase hex digits — the one
///   string `exports` renders — is a canonical-hash literal.
/// - A `0x`-prefixed string of hex digits at any OTHER width or case
///   is refused with the width message: it reads as a mispasted or
///   hand-shortened hash (e.g. the collidable 16-bit wire hint), and
///   silently hashing it as a *name* would be worse than the paste
///   error it came from.
/// - **Everything else is a channel name**, including digit-only
///   strings — `66` and `65536` are valid channel names, and name
///   space takes precedence over any would-be decimal literal
///   syntax. A name IS the canonical identity: the hash is a pure
///   function of it, no mesh attachment needed.
fn parse_channel_hash(raw: &str) -> Result<u64, CliError> {
    let s = raw.trim();
    if s.is_empty() {
        return Err(invalid_args("channel cannot be empty"));
    }
    if let Some(hex) = s.strip_prefix("0x") {
        let canonical =
            hex.len() == 16 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
        if canonical {
            return u64::from_str_radix(hex, 16)
                .map_err(|e| invalid_args(format!("channel `{raw}`: {e}")));
        }
        if !hex.is_empty() && hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(invalid_args(format!(
                "channel `{raw}`: a hash literal must be EXACTLY lowercase `0x` \
                 followed by 16 lowercase hex digits, as `net gateway exports` \
                 renders it. Short, long, or uppercase forms are refused rather \
                 than widened or reinterpreted (a short value is indistinguishable \
                 from the collidable 16-bit wire hint). Prefer the channel name: \
                 the canonical hash derives from it directly."
            )));
        }
        // `0x` followed by non-hex content is not a literal attempt —
        // fall through to name resolution.
    }
    ChannelName::new(s)
        .map(|n| n.hash())
        .map_err(|e| invalid_args(format!("channel `{raw}`: {e}")))
}

/// Parse a subnet arg into a `SubnetId` via its `FromStr` impl.
/// Wraps the typed `SubnetError` as a `CliError` so the operator
/// sees a clear message.
fn parse_subnet(raw: &str) -> Result<SubnetId, CliError> {
    use std::str::FromStr;
    SubnetId::from_str(raw).map_err(|e| invalid_args(format!("subnet `{raw}`: {e}")))
}

#[derive(Serialize)]
struct StatsView {
    /// `false` when no `SubnetGateway` is installed on the local
    /// mesh — happens when `set_channel_configs` hasn't been
    /// called (or the deck has no mesh attached).
    gateway_installed: bool,
    local_subnet: Option<String>,
    forwarded: u64,
    dropped: u64,
    peer_subnet_count: u64,
    peer_subnets: Vec<String>,
    export_rules: u64,
}

impl StatsView {
    fn installed(stats: &GatewayStats) -> Self {
        Self {
            gateway_installed: true,
            local_subnet: Some(stats.local_subnet.to_string()),
            forwarded: stats.forwarded,
            dropped: stats.dropped,
            peer_subnet_count: stats.peer_subnets.len() as u64,
            peer_subnets: stats.peer_subnets.iter().map(|s| s.to_string()).collect(),
            export_rules: stats.export_rules,
        }
    }
    fn not_installed() -> Self {
        Self {
            gateway_installed: false,
            local_subnet: None,
            forwarded: 0,
            dropped: 0,
            peer_subnet_count: 0,
            peer_subnets: Vec::new(),
            export_rules: 0,
        }
    }
}

#[derive(Serialize)]
struct ExportRow {
    channel_hash: String,
    target_count: u64,
    targets: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_subnet_accepts_global_and_dotted_levels() {
        assert_eq!(parse_subnet("global").unwrap(), SubnetId::GLOBAL);
        assert_eq!(parse_subnet("GLOBAL").unwrap(), SubnetId::GLOBAL);
        assert_eq!(parse_subnet("3").unwrap(), SubnetId::new(&[3]));
        assert_eq!(parse_subnet("3.7").unwrap(), SubnetId::new(&[3, 7]));
        assert_eq!(parse_subnet("3.7.2").unwrap(), SubnetId::new(&[3, 7, 2]));
        assert_eq!(
            parse_subnet("3.7.2.1").unwrap(),
            SubnetId::new(&[3, 7, 2, 1])
        );
    }

    #[test]
    fn parse_subnet_rejects_overflow_and_garbage() {
        assert!(parse_subnet("256").is_err()); // u8 overflow
        assert!(parse_subnet("3.7.2.1.0").is_err()); // > MAX_DEPTH
        assert!(parse_subnet("not-a-number").is_err());
        assert!(parse_subnet("").is_err());
    }

    #[test]
    fn parse_channel_hash_accepts_only_the_exact_render_format_as_a_literal() {
        // The one accepted literal shape: lowercase `0x` + exactly 16
        // lowercase hex digits — the string `exports` renders,
        // round-tripped.
        assert_eq!(
            parse_channel_hash("0xeb3ad2bc323f22f2").unwrap(),
            0xeb3a_d2bc_323f_22f2,
        );
        assert_eq!(
            parse_channel_hash("0x00000000000000ff").unwrap(),
            0xff,
            "leading zeros are how a small canonical value is written at full width",
        );

        // Hex-looking forms at any other width or case are REFUSED,
        // not widened and not silently hashed as names: an operator's
        // accidentally pasted 16-bit wire hint must not become a
        // canonical identity that matches nothing.
        assert!(parse_channel_hash("0x42").is_err());
        assert!(parse_channel_hash("0x1FFFF").is_err());
        // 15 and 17 digits bracket the accepted width exactly.
        assert!(parse_channel_hash("0xeb3ad2bc323f22f").is_err());
        assert!(parse_channel_hash("0xeb3ad2bc323f22f21").is_err());
        // Full width but uppercase digits — not the render format.
        assert!(parse_channel_hash("0xEB3AD2BC323F22F2").is_err());
        // Uppercase `0X` prefix is not a literal; as a NAME it fails
        // validation (uppercase is rejected in channel names), so the
        // lowercase-only promise holds end to end.
        assert!(parse_channel_hash("0X00000000000000ff").is_err());
    }

    /// Digit-only strings are channel NAMES, not literals. `66` is a
    /// valid channel name; reserving ambiguous decimal literal syntax
    /// must not sacrifice it.
    #[test]
    fn parse_channel_hash_treats_digit_only_strings_as_names() {
        for name in ["66", "65536", "18446744073709551615"] {
            let expected = net_sdk::ChannelName::new(name).expect("valid name").hash();
            assert_eq!(
                parse_channel_hash(name).unwrap(),
                expected,
                "`{name}` must resolve as a channel name, never as a decimal literal",
            );
        }
        // A `0x`-prefixed string with non-hex content is a name too
        // (it was never a literal attempt).
        let expected = net_sdk::ChannelName::new("0xgateway")
            .expect("valid name")
            .hash();
        assert_eq!(parse_channel_hash("0xgateway").unwrap(), expected);
    }

    #[test]
    fn parse_channel_hash_resolves_a_name_to_its_canonical_hash() {
        // A name is the PREFERRED form: the canonical hash is a pure
        // function of it, so no mesh attachment is needed and the
        // operator never handles a collidable hint.
        let expected = net_sdk::ChannelName::new("internal/metrics")
            .expect("name")
            .hash();
        assert_eq!(parse_channel_hash("internal/metrics").unwrap(), expected);
        // A malformed name is refused rather than silently hashed.
        assert!(parse_channel_hash("not a valid name!!").is_err());
    }

    #[test]
    fn parse_channel_hash_rejects_empty_and_overflow_widths() {
        assert!(parse_channel_hash("").is_err());
        // 17 hex digits (would overflow u64) — refused on width.
        assert!(parse_channel_hash("0xFFFFFFFFFFFFFFFFF").is_err());
        // `0x`-prefixed NON-hex content was never a literal attempt:
        // it resolves as an ordinary channel name (see the digit-only
        // names test), not as a refused literal.
        let expected = net_sdk::ChannelName::new("0xeb3ad2bc323f22zz")
            .expect("valid name")
            .hash();
        assert_eq!(parse_channel_hash("0xeb3ad2bc323f22zz").unwrap(), expected);
    }

    /// The exports view renders the exact literal the export parser
    /// accepts, so a row can be copy-pasted back into `gateway
    /// export` without hand-editing.
    #[test]
    fn exports_render_width_round_trips_through_the_parser() {
        for hash in [0u64, 0xff, 0xeb3a_d2bc_323f_22f2, u64::MAX] {
            let rendered = format!("{hash:#018x}");
            assert_eq!(
                parse_channel_hash(&rendered).unwrap(),
                hash,
                "rendered `{rendered}` must parse back to the same canonical hash",
            );
        }
    }
}
