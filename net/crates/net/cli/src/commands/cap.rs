//! `net cap (show|query|nodes|announce)` — capability advertisement
//! + discovery from the local snapshot, plus offline compose-and-sign
//! for v0.4 capability-auth (see `docs/plans/CAPABILITY_AUTH_PLAN.md`).
//!
//! `show` / `query` / `nodes` read `DeckClient::status()` and filter
//! the snapshot's per-peer `capability_set`. `announce` builds a
//! signed [`CapabilityAnnouncement`] with the supplied allow-lists
//! and emits the JSON bytes to stdout (or `--out`); the operator
//! ships those bytes through any pub/sub path that calls
//! `CapabilityIndex::index` on receipt. Direct broadcast through
//! the CLI is deferred until the SDK exposes a mesh handle on the
//! daemon runtime — that's tracked separately and doesn't block
//! the operator from issuing announcements today.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use net_sdk::capabilities::{
    CapabilityAnnouncement, CapabilityGroupId as GroupId, CapabilitySet,
    CapabilitySubnetId as SubnetId, Tag, MAX_ALLOW_LIST_LEN,
};
use serde::Serialize;

use crate::context::{load_identity_keypair, resolve_profile, CliContext};
use crate::error::{generic, invalid_args, CliError};
use crate::prelude::{emit_value, OutputFormat};

#[derive(Subcommand, Debug)]
pub enum CapCommand {
    /// Show capabilities for the local node (default) or a
    /// specific peer via `--node`.
    Show(ShowArgs),
    /// Find nodes whose advertised capability set contains
    /// every supplied tag.
    Query(QueryArgs),
    /// List every (node, capabilities) tuple known to the local
    /// capability index.
    Nodes(NodesArgs),
    /// Build a signed `CapabilityAnnouncement` with the supplied
    /// allow-lists and emit the JSON bytes to stdout (or `--out`).
    ///
    /// Revocation: re-run with a tighter `--allow-node` /
    /// `--allow-subnet` / `--allow-group` set + a bumped
    /// `--version`; the new bytes supersede the old at any
    /// receiver that folds them. There is no separate `revoke`
    /// verb — that's the locked design (see
    /// `docs/plans/CAPABILITY_AUTH_PLAN.md` §"Locked design points").
    Announce(AnnounceArgs),
}

#[derive(Args, Debug)]
pub struct ShowArgs {
    /// Peer node id. Defaults to the local node configured by
    /// `--node`.
    #[arg(long, value_name = "PEER_NODE")]
    pub peer: Option<u64>,

    #[arg(long)]
    pub identity: Option<PathBuf>,

    #[arg(long, default_value_t = crate::prelude::DEFAULT_SUPERVISOR_NODE)]
    pub node: u64,
}

#[derive(Args, Debug)]
pub struct QueryArgs {
    /// One or more required tags. A node matches when its
    /// advertised capability set contains every tag listed.
    #[arg(long = "tag", required = true, num_args = 1.., value_name = "TAG")]
    pub tags: Vec<String>,

    #[arg(long)]
    pub identity: Option<PathBuf>,

    #[arg(long, default_value_t = crate::prelude::DEFAULT_SUPERVISOR_NODE)]
    pub node: u64,
}

#[derive(Args, Debug)]
pub struct NodesArgs {
    #[arg(long)]
    pub identity: Option<PathBuf>,

    #[arg(long, default_value_t = crate::prelude::DEFAULT_SUPERVISOR_NODE)]
    pub node: u64,
}

#[derive(Args, Debug)]
pub struct AnnounceArgs {
    /// One or more capability tags to carry on the announcement
    /// (e.g. `nrpc:my-service`, `dataforts.blob.overflow`).
    /// Reserved-prefix tags (`causal:` / `fork-of:` / `heat:` /
    /// `scope:`) are silently dropped by the parser — use the
    /// dedicated builders for those.
    #[arg(long = "tag", required = true, num_args = 1.., value_name = "TAG")]
    pub tags: Vec<String>,

    /// Allow-listed caller node ids. Accept decimal or `0x`-prefixed
    /// hex. Empty = permissive for this axis. Lists capped at 64
    /// entries per axis (`MAX_ALLOW_LIST_LEN`); past that operators
    /// should use a group.
    #[arg(long = "allow-node", num_args = 0.., value_name = "NODE_ID")]
    pub allow_nodes: Vec<String>,

    /// Allow-listed subnet ids — `<hex32>` or `subnet:<hex32>`.
    #[arg(long = "allow-subnet", num_args = 0.., value_name = "SUBNET")]
    pub allow_subnets: Vec<String>,

    /// Allow-listed group ids — `<hex64>` or `group:<hex64>`.
    #[arg(long = "allow-group", num_args = 0.., value_name = "GROUP")]
    pub allow_groups: Vec<String>,

    /// Operator identity TOML containing `seed_hex = "..."` (32
    /// bytes of hex). The keypair's derived `node_id` is used as
    /// the announcement's `node_id` unless `--node-id` overrides it.
    #[arg(long, value_name = "PATH")]
    pub key: PathBuf,

    /// Monotonic version. Receivers honor strictly-increasing
    /// versions per `node_id` — bumps on every revocation /
    /// policy change.
    #[arg(long, default_value_t = 1)]
    pub version: u64,

    /// TTL in seconds. The receiver caps its local lifetime at
    /// `min(local_ttl, origin_remaining)` so a replayed late
    /// announcement doesn't get a fresh local lease.
    #[arg(long = "ttl-secs", default_value_t = 300)]
    pub ttl_secs: u32,

    /// Override the derived `node_id` (decimal or `0x` hex). The
    /// default — `EntityKeypair::node_id()` — is the right
    /// answer for self-issued announcements; the override is for
    /// operator scenarios where the key signs on behalf of a
    /// different node identity.
    #[arg(long = "node-id", value_name = "NODE_ID")]
    pub node_id: Option<String>,

    /// Write the JSON announcement bytes here. Defaults to
    /// stdout.
    #[arg(long, value_name = "PATH")]
    pub out: Option<PathBuf>,
}

pub async fn run(
    cmd: CapCommand,
    output: Option<OutputFormat>,
    config_path: Option<&std::path::Path>,
    profile_name: &str,
) -> Result<(), CliError> {
    match cmd {
        CapCommand::Show(args) => run_show(args, output, config_path, profile_name).await,
        CapCommand::Query(args) => run_query(args, output, config_path, profile_name).await,
        CapCommand::Nodes(args) => run_nodes(args, output, config_path, profile_name).await,
        CapCommand::Announce(args) => run_announce(args).await,
    }
}

async fn run_show(
    args: ShowArgs,
    output: Option<OutputFormat>,
    config_path: Option<&std::path::Path>,
    profile_name: &str,
) -> Result<(), CliError> {
    let profile = resolve_profile(config_path, profile_name).await?;
    let ctx = CliContext::build(&profile, args.identity.as_deref(), args.node, false).await?;
    let snapshot = ctx.deck().status();
    let target = args.peer.unwrap_or(args.node);
    let caps = snapshot
        .peers
        .get(&target)
        .map(|p| p.capability_set.iter().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    let info = CapShow {
        node: target,
        capabilities: caps,
    };
    emit_value(OutputFormat::resolve_oneshot(output), &info)
        .map_err(|e| generic(format!("write cap show: {e}")))?;
    Ok(())
}

async fn run_query(
    args: QueryArgs,
    output: Option<OutputFormat>,
    config_path: Option<&std::path::Path>,
    profile_name: &str,
) -> Result<(), CliError> {
    let profile = resolve_profile(config_path, profile_name).await?;
    let ctx = CliContext::build(&profile, args.identity.as_deref(), args.node, false).await?;
    let snapshot = ctx.deck().status();
    let required: BTreeSet<String> = args.tags.into_iter().collect();
    let matches: Vec<u64> = snapshot
        .peers
        .iter()
        .filter(|(_, p)| required.iter().all(|t| p.capability_set.contains(t)))
        .map(|(id, _)| *id)
        .collect();
    let info = CapQuery {
        required: required.into_iter().collect(),
        matched_nodes: matches,
    };
    emit_value(OutputFormat::resolve_oneshot(output), &info)
        .map_err(|e| generic(format!("write cap query: {e}")))?;
    Ok(())
}

async fn run_nodes(
    args: NodesArgs,
    output: Option<OutputFormat>,
    config_path: Option<&std::path::Path>,
    profile_name: &str,
) -> Result<(), CliError> {
    let profile = resolve_profile(config_path, profile_name).await?;
    let ctx = CliContext::build(&profile, args.identity.as_deref(), args.node, false).await?;
    let snapshot = ctx.deck().status();
    let rows: Vec<CapNodesRow> = snapshot
        .peers
        .iter()
        .map(|(id, p)| CapNodesRow {
            node: *id,
            capabilities: p.capability_set.iter().cloned().collect(),
        })
        .collect();
    emit_value(OutputFormat::resolve_oneshot(output), &rows)
        .map_err(|e| generic(format!("write cap nodes: {e}")))?;
    Ok(())
}

async fn run_announce(args: AnnounceArgs) -> Result<(), CliError> {
    // 1. Identity. Reuses the same TOML loader the live
    //    `CliContext::build` path uses so an operator can point
    //    `--key` at the same file they already configured for
    //    other write-side subcommands.
    let keypair = load_identity_keypair(&args.key).await?;

    // 2. Allow-list parsing — fail loudly on any malformed entry
    //    before signing anything. Operators get a typed error per
    //    flag rather than a silent drop.
    if args.allow_nodes.len() > MAX_ALLOW_LIST_LEN
        || args.allow_subnets.len() > MAX_ALLOW_LIST_LEN
        || args.allow_groups.len() > MAX_ALLOW_LIST_LEN
    {
        return Err(invalid_args(format!(
            "allow-list axes are capped at {MAX_ALLOW_LIST_LEN} entries each; \
             operators above that limit should use a group instead of an \
             inline node enumeration (see CAPABILITY_AUTH_PLAN.md §\"What ships\")"
        )));
    }
    let allowed_nodes = parse_node_ids(&args.allow_nodes)?;
    let allowed_subnets = parse_subnets(&args.allow_subnets)?;
    let allowed_groups = parse_groups(&args.allow_groups)?;

    // 3. Resolve target node_id. The keypair's derived `node_id`
    //    is the only value that round-trips through the receiver's
    //    `handle_capability_announcement` — receivers re-derive the
    //    expected NodeId from the signed `entity_id` and reject
    //    announcements where the carried `node_id` doesn't match.
    //    Allow `--node-id` only as an explicit confirmation (must
    //    equal the derived value); a mismatch is an operator error
    //    that would otherwise produce unusable bytes.
    let derived = keypair.node_id();
    let node_id = match args.node_id.as_deref() {
        Some(s) => {
            let supplied = parse_node_id(s)?;
            if supplied != derived {
                return Err(invalid_args(format!(
                    "--node-id {supplied:#x} does not match the signing key's \
                     derived node id {derived:#x}; receivers re-derive the \
                     expected NodeId from the signed entity_id and reject \
                     announcements with mismatched bindings. Drop the flag \
                     to use the derived value, or sign with the keypair that \
                     produces {supplied:#x}."
                )));
            }
            supplied
        }
        None => derived,
    };

    // 4. Build the CapabilitySet with the user-supplied tags.
    //    Validate each tag via `Tag::parse_user` directly — the
    //    pre-fix length-delta heuristic on `caps.tags.len()`
    //    couldn't distinguish "parser rejected the tag" from
    //    "tag was a duplicate already in the set", so a perfectly
    //    legal `--tag nrpc:echo --tag nrpc:echo` invocation errored
    //    out with the reserved-prefix message. Using the parser
    //    result directly: invalid tags fail, duplicates dedupe
    //    silently via the underlying `HashSet<Tag>`.
    let mut caps = CapabilitySet::new();
    for tag in &args.tags {
        if let Err(e) = Tag::parse_user(tag) {
            return Err(invalid_args(format!(
                "tag {tag:?} rejected: {e}. Reserved-prefix tags \
                 (`causal:` / `fork-of:` / `heat:` / `scope:`) are not \
                 admissible via this subcommand — use the dedicated \
                 builders for those.",
            )));
        }
        caps = caps.add_tag(tag.clone());
    }

    // 5. Build + sign.
    let mut ann =
        CapabilityAnnouncement::new(node_id, keypair.entity_id().clone(), args.version, caps)
            .with_ttl(args.ttl_secs);
    ann.allowed_nodes = allowed_nodes;
    ann.allowed_subnets = allowed_subnets;
    ann.allowed_groups = allowed_groups;
    ann.sign(&keypair);

    // 6. Emit JSON bytes. Operators pipe stdout or save via
    //    `--out`; downstream tooling parses with
    //    `CapabilityAnnouncement::from_bytes` and folds via
    //    `CapabilityIndex::index`.
    let bytes = ann.to_bytes();
    write_announcement_output(args.out.as_deref(), &bytes).await?;
    Ok(())
}

fn parse_node_ids(values: &[String]) -> Result<Vec<u64>, CliError> {
    values.iter().map(|v| parse_node_id(v)).collect()
}

fn parse_node_id(value: &str) -> Result<u64, CliError> {
    let trimmed = value.trim();
    let parsed = if let Some(hex) = trimmed.strip_prefix("0x").or_else(|| trimmed.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16)
    } else {
        trimmed.parse::<u64>()
    };
    parsed.map_err(|_| {
        invalid_args(format!(
            "node id {value:?} must be decimal or `0x`-prefixed hex (u64)"
        ))
    })
}

fn parse_subnets(values: &[String]) -> Result<Vec<SubnetId>, CliError> {
    values
        .iter()
        .map(|v| {
            let tag_form = if v.starts_with("subnet:") {
                v.clone()
            } else {
                format!("subnet:{v}")
            };
            SubnetId::from_tag(&tag_form).ok_or_else(|| {
                invalid_args(format!(
                    "subnet id {v:?} must be 32 hex characters (16 bytes), \
                     optionally prefixed with `subnet:`"
                ))
            })
        })
        .collect()
}

fn parse_groups(values: &[String]) -> Result<Vec<GroupId>, CliError> {
    values
        .iter()
        .map(|v| {
            let tag_form = if v.starts_with("group:") {
                v.clone()
            } else {
                format!("group:{v}")
            };
            GroupId::from_tag(&tag_form).ok_or_else(|| {
                invalid_args(format!(
                    "group id {v:?} must be 64 hex characters (32 bytes), \
                     optionally prefixed with `group:`"
                ))
            })
        })
        .collect()
}

async fn write_announcement_output(out: Option<&Path>, bytes: &[u8]) -> Result<(), CliError> {
    match out {
        Some(path) => tokio::fs::write(path, bytes)
            .await
            .map_err(|e| generic(format!("write {}: {e}", path.display()))),
        None => {
            use std::io::Write;
            let mut stdout = std::io::stdout().lock();
            stdout
                .write_all(bytes)
                .map_err(|e| generic(format!("write stdout: {e}")))?;
            // Trailing newline so a piped consumer can read a clean
            // line if it wants one; the JSON bytes themselves don't
            // terminate with a newline.
            stdout
                .write_all(b"\n")
                .map_err(|e| generic(format!("write stdout: {e}")))?;
            Ok(())
        }
    }
}

#[derive(Serialize)]
struct CapShow {
    node: u64,
    capabilities: Vec<String>,
}

#[derive(Serialize)]
struct CapQuery {
    required: Vec<String>,
    matched_nodes: Vec<u64>,
}

#[derive(Serialize)]
struct CapNodesRow {
    node: u64,
    capabilities: Vec<String>,
}
