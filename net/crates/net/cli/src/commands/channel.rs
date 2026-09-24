//! `net-mesh channel (visibility|ls)` — surface the local mesh's
//! `ChannelConfigRegistry` to operators.
//!
//! `visibility <name>` — look up a single channel's
//! [`Visibility`] config (`SubnetLocal` / `ParentVisible` /
//! `Exported` / `Global`).
//!
//! `ls` — enumerate every registered channel as
//! `(name, visibility)` rows for inventory.
//!
//! When no `MeshNode` is attached to the `DeckClient`, both
//! commands report sensible empties. Shape pinned in
//! `SCALING_SUBNET_SPEC.md` Phase A.
//!
//! Delegated channel credentials (NET_CLI_PLAN_V3 V3-2A):
//!
//! `issue-grant` — OFFLINE, on the operator's machine: the channel root
//! (an operator identity file) signs one DELEGATE grant on one canonical
//! channel to an issuing node's identity. The root never reaches the node.
//!
//! `serve <name> --token-root <ENTITY>` — on a running node: gate the
//! channel so only chains anchored at that root may subscribe to it (and
//! this node may publish on it only with its own managed chain). Persisted
//! in the state directory and re-registered on every `up`.

use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use net::adapter::net::channel::{ChannelId, ChannelName};
use net::adapter::net::identity::{EntityId, PermissionToken, TokenScope};
use net_sdk::subnets::Visibility;
use serde::Serialize;
use serde_json::{json, Value};

use crate::context::{resolve_profile, CliContext};
use crate::error::{connection_failure, generic, invalid_args, CliError};
use crate::prelude::{emit_value, OutputFormat};

#[derive(Subcommand, Debug)]
pub enum ChannelCommand {
    /// Show a single channel's `Visibility` config.
    Visibility(VisibilityArgs),
    /// List every registered channel.
    Ls(LsArgs),
    /// OFFLINE: sign a channel grant with the channel root (an operator
    /// identity file) delegating publish/subscribe on one channel to an
    /// issuing node, which then mints device credentials at `join`.
    IssueGrant(IssueGrantArgs),
    /// Gate a channel on the running node: only chains anchored at the
    /// given token root may subscribe (persisted; re-applied on `up`).
    Serve(ServeArgs),
    /// Show the channels this node serves and, for a joined device, its
    /// channel credential: subscribe ACK and publish readiness. Never a
    /// roster of other members.
    Status(NodeDirArgs),
    /// Leave one channel relation of this device (named, or the only active
    /// one): recorded durably, then an acknowledged unsubscribe and removal
    /// of exactly the installed publish credential. The mesh membership is
    /// untouched; rejoining takes a new link.
    Leave(ChannelLeaveArgs),
    /// Create a standalone channel link on the running `up --enroll` node:
    /// the channel relation only, for a device already on this mesh,
    /// redeemed over its session. Needs a `--channel-grant` for the channel.
    Invite(ChannelInviteArgs),
    /// Join a channel with a standalone link, through this device's running
    /// `up`: it redeems the link over its session with the node it enrolled
    /// with, keeps the chain and starts using it (subscribe / publish).
    Join(ChannelJoinArgs),
    /// Publish one payload on a channel from the running node: the node's
    /// own local channel gate decides (`gate: passed`), or denies it; an
    /// ungated channel is reported `gate: open`. Delivery counts are this
    /// node's sends, not subscriber receipts.
    Publish(PublishArgs),
}

/// `channel publish` arguments.
#[derive(Args, Debug)]
pub struct PublishArgs {
    /// The canonical channel name.
    pub channel: String,
    /// The payload (UTF-8 text, at most 16 KiB).
    #[arg(long)]
    pub data: String,
    /// Node state directory (as given to `net-mesh up`).
    #[arg(long, value_name = "DIR")]
    pub state_dir: Option<PathBuf>,
}

/// Largest payload `channel publish` sends.
const MAX_PUBLISH_BYTES: usize = 16 * 1024;

/// `channel leave` arguments.
#[derive(Args, Debug)]
pub struct ChannelLeaveArgs {
    /// The channel to leave (needed when several are active).
    pub channel: Option<String>,
    /// Node state directory (as given to `net-mesh up`).
    #[arg(long, value_name = "DIR")]
    pub state_dir: Option<PathBuf>,
}

/// `channel invite` arguments.
#[derive(Args, Debug)]
pub struct ChannelInviteArgs {
    /// The canonical channel name.
    pub channel: String,
    /// `publish`, `subscribe` or `publish,subscribe`.
    #[arg(long)]
    pub rights: String,
    /// Node state directory (as given to `net-mesh up`).
    #[arg(long, value_name = "DIR")]
    pub state_dir: Option<PathBuf>,
    /// Lifetime of the unredeemed link (default 24h). The credential's own
    /// lifetime is the grant's.
    #[arg(long, value_name = "DURATION", value_parser = crate::humantime::parse_duration)]
    pub ttl: Option<std::time::Duration>,
    /// Require an operator decision (`invite approve`) before issuing.
    #[arg(long)]
    pub require_approval: bool,
    /// Bind the link to one device's full 64-hex entity id.
    #[arg(long = "for", value_name = "ENTITY")]
    pub for_subject: Option<String>,
    /// Write the token to this new owner-only file instead of stdout.
    #[arg(long, value_name = "PATH")]
    pub out: Option<PathBuf>,
}

/// `channel join` arguments.
#[derive(Args, Debug)]
pub struct ChannelJoinArgs {
    /// The standalone channel link.
    pub token: String,
    /// State directory of this device's running `net-mesh up`.
    #[arg(long, value_name = "DIR")]
    pub state_dir: Option<PathBuf>,
    /// Skip the interactive confirmation (scripts and agent tool use).
    #[arg(long)]
    pub yes: bool,
}

/// Arguments naming a running node.
#[derive(Args, Debug)]
pub struct NodeDirArgs {
    /// Node state directory (as given to `net-mesh up`).
    #[arg(long, value_name = "DIR")]
    pub state_dir: Option<PathBuf>,
}

/// `channel issue-grant` arguments.
#[derive(Args, Debug)]
pub struct IssueGrantArgs {
    /// The channel root: an operator identity file (`identity generate`).
    /// It stays on this machine.
    #[arg(long = "root-identity", value_name = "PATH")]
    pub root_identity: PathBuf,
    /// The issuing node's enrollment issuer: the full 64-hex `issuer` that
    /// `up --enroll` and `node status` report.
    #[arg(long, value_name = "ENTITY")]
    pub issuer: String,
    /// The canonical channel name.
    #[arg(long)]
    pub channel: String,
    /// The rights the node may grant: `publish`, `subscribe` or
    /// `publish,subscribe`.
    #[arg(long, default_value = "publish,subscribe")]
    pub rights: String,
    /// Lifetime of the grant; every device credential expires with it.
    #[arg(long, value_name = "DURATION", default_value = "30d", value_parser = crate::humantime::parse_duration)]
    pub ttl: std::time::Duration,
    /// Where to write the grant (not secret, but it is this node's authority
    /// to issue: hand it only to that node).
    #[arg(long, value_name = "PATH")]
    pub out: PathBuf,
    /// Replace an existing file at `--out`.
    #[arg(long)]
    pub force: bool,
}

/// `channel serve` arguments.
#[derive(Args, Debug)]
pub struct ServeArgs {
    /// The canonical channel name.
    pub channel: String,
    /// Token root(s) whose chains this node accepts (64-hex entity ids).
    #[arg(long = "token-root", value_name = "ENTITY", required = true)]
    pub token_roots: Vec<String>,
    /// Node state directory (as given to `net-mesh up`).
    #[arg(long, value_name = "DIR")]
    pub state_dir: Option<PathBuf>,
}

#[derive(Args, Debug)]
pub struct VisibilityArgs {
    #[command(flatten)]
    pub scope: super::scope::InspectableLocalScope,

    /// Channel name (canonical, exact match — falls back through
    /// the registry's prefix table via `get_by_name`).
    pub channel: String,

    #[arg(long)]
    pub identity: Option<PathBuf>,

    #[arg(long, default_value_t = crate::prelude::DEFAULT_SUPERVISOR_NODE)]
    pub node: u64,
}

#[derive(Args, Debug)]
pub struct LsArgs {
    #[command(flatten)]
    pub scope: super::scope::InspectableLocalScope,

    #[arg(long)]
    pub identity: Option<PathBuf>,

    #[arg(long, default_value_t = crate::prelude::DEFAULT_SUPERVISOR_NODE)]
    pub node: u64,
}

pub async fn run(
    cmd: ChannelCommand,
    output: Option<OutputFormat>,
    config_path: Option<&std::path::Path>,
    profile_name: &str,
) -> Result<(), CliError> {
    match cmd {
        ChannelCommand::Visibility(args) => {
            run_visibility(args, output, config_path, profile_name).await
        }
        ChannelCommand::Ls(args) => run_ls(args, output, config_path, profile_name).await,
        ChannelCommand::IssueGrant(args) => run_issue_grant(args, output).await,
        ChannelCommand::Serve(args) => run_serve(args, output, profile_name).await,
        ChannelCommand::Status(args) => {
            node_op(
                args.state_dir,
                profile_name,
                json!({ "op": "channel_status" }),
                output,
            )
            .await
        }
        ChannelCommand::Leave(args) => {
            let root = super::lifecycle::state_dir(args.state_dir.clone(), profile_name)?;
            // No node running: record the departure offline.
            if !super::lifecycle::node_running(&root)? {
                if let Some(name) = &args.channel {
                    parse_channel_name(name)?;
                }
                let reply =
                    super::lifecycle::offline_channel_leave(&root, args.channel.as_deref())?;
                return emit_value(OutputFormat::resolve_oneshot(output), &reply)
                    .map_err(|e| generic(format!("write result: {e}")));
            }
            let mut request = json!({ "op": "channel_leave" });
            if let Some(name) = &args.channel {
                parse_channel_name(name)?;
                request["channel"] = json!(name);
            }
            node_op(args.state_dir, profile_name, request, output).await
        }
        ChannelCommand::Invite(args) => {
            parse_channel_name(&args.channel)?;
            parse_channel_rights(&args.rights)?;
            super::enrollment::run_invite(
                super::enrollment::InviteCommand::Create(super::enrollment::CreateArgs {
                    state_dir: args.state_dir,
                    ttl: args.ttl,
                    require_approval: args.require_approval,
                    for_subject: args.for_subject,
                    out: args.out,
                    addr: None,
                    subnet: None,
                    subnet_rights: None,
                    org: None,
                    channel: Some(args.channel),
                    channel_rights: Some(args.rights),
                    standalone: true,
                }),
                output,
                profile_name,
            )
            .await
        }
        ChannelCommand::Join(args) => {
            if !args.yes {
                return Err(invalid_args(
                    "channel join redeems the link for this device; confirm with --yes                      (inspect it first with `invite inspect`)",
                ));
            }
            node_op(
                args.state_dir,
                profile_name,
                json!({ "op": "channel_join", "token": args.token.trim() }),
                output,
            )
            .await
        }
        ChannelCommand::Publish(args) => {
            parse_channel_name(&args.channel)?;
            if args.data.len() > MAX_PUBLISH_BYTES {
                return Err(invalid_args(format!(
                    "--data is larger than {MAX_PUBLISH_BYTES} bytes"
                )));
            }
            node_op(
                args.state_dir,
                profile_name,
                json!({ "op": "channel_publish", "channel": args.channel, "data": args.data }),
                output,
            )
            .await
        }
    }
}

/// The on-disk kind marker of a channel grant file.
const GRANT_KIND: &str = "channel-grant";
/// Served channels, under the state root.
pub(crate) const SERVED_FILE: &str = "channels.json";

/// Parse `publish`, `subscribe` or `publish,subscribe` (either order).
pub(crate) fn parse_channel_rights(text: &str) -> Result<TokenScope, CliError> {
    let mut rights = TokenScope::NONE;
    for part in text.split(',').map(str::trim) {
        rights = rights.union(match part {
            "publish" => TokenScope::PUBLISH,
            "subscribe" => TokenScope::SUBSCRIBE,
            other => {
                return Err(invalid_args(format!(
                    "channel rights are `publish` and/or `subscribe`, not `{other}`"
                )))
            }
        });
    }
    Ok(rights)
}

/// `publish`, `subscribe` or `publish,subscribe`.
pub(crate) fn format_channel_rights(rights: TokenScope) -> String {
    let mut parts = Vec::new();
    if rights.contains(TokenScope::PUBLISH) {
        parts.push("publish");
    }
    if rights.contains(TokenScope::SUBSCRIBE) {
        parts.push("subscribe");
    }
    parts.join(",")
}

pub(crate) fn parse_channel_name(name: &str) -> Result<ChannelName, CliError> {
    ChannelName::new(name).map_err(|e| invalid_args(format!("channel `{name}`: {e}")))
}

fn parse_entity(hex_text: &str, what: &str) -> Result<EntityId, CliError> {
    let bytes: [u8; 32] = hex::decode(hex_text.trim().trim_start_matches("0x"))
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| invalid_args(format!("{what}: expected a 64-hex entity id")))?;
    Ok(EntityId::from_bytes(bytes))
}

async fn run_issue_grant(
    args: IssueGrantArgs,
    output: Option<OutputFormat>,
) -> Result<(), CliError> {
    let channel = parse_channel_name(&args.channel)?;
    let issuer = parse_entity(&args.issuer, "--issuer")?;
    let rights = parse_channel_rights(&args.rights)?;
    if args.out.exists() && !args.force {
        return Err(invalid_args(format!(
            "{} already exists (pass --force to replace it)",
            args.out.display()
        )));
    }
    let root = crate::context::load_operator_identity(&args.root_identity).await?;
    if root.entity_id() == &issuer {
        return Err(invalid_args(
            "the issuer must be the node's identity, not the channel root".to_string(),
        ));
    }
    let grant = PermissionToken::try_issue(
        root.keypair(),
        issuer.clone(),
        rights.union(TokenScope::DELEGATE),
        channel.hash(),
        args.ttl.as_secs(),
        1,
    )
    .map_err(|e| invalid_args(format!("issue grant: {e}")))?;
    let file = json!({
        "kind": GRANT_KIND,
        "channel": channel.as_str(),
        "grant_hex": hex::encode(grant.to_bytes()),
    });
    let text = serde_json::to_vec_pretty(&file).map_err(|e| generic(format!("encode: {e}")))?;
    let tmp = args.out.with_extension("tmp-channel-grant");
    std::fs::write(&tmp, &text)
        .and_then(|()| std::fs::rename(&tmp, &args.out))
        .map_err(|e| generic(format!("write {}: {e}", args.out.display())))?;
    emit_value(
        OutputFormat::resolve_oneshot(output),
        &json!({
            "path": args.out.display().to_string(),
            "channel": channel.as_str(),
            "root": hex::encode(root.entity_id().as_bytes()),
            "issuer": hex::encode(issuer.as_bytes()),
            "rights": format_channel_rights(rights),
            "not_after": grant.not_after,
        }),
    )
    .map_err(|e| generic(format!("write result: {e}")))
}

/// Load a grant file (`channel issue-grant`) as `identity`'s leaf issuer.
pub(crate) async fn load_channel_issuer(
    path: &Path,
    identity: &net_sdk::identity::Identity,
) -> Result<(ChannelName, net_sdk::channel_issuer::ChannelLeafIssuer), CliError> {
    let bad = |why: &str| invalid_args(format!("--channel-grant {}: {why}", path.display()));
    let bytes = tokio::fs::read(path)
        .await
        .map_err(|e| bad(&e.to_string()))?;
    let file: Value =
        serde_json::from_slice(&bytes).map_err(|_| bad("not a channel grant file"))?;
    if file["kind"] != GRANT_KIND {
        return Err(bad("not a channel grant file"));
    }
    let channel = parse_channel_name(file["channel"].as_str().unwrap_or_default())?;
    let grant = hex::decode(file["grant_hex"].as_str().unwrap_or_default())
        .ok()
        .and_then(|b| PermissionToken::from_bytes(&b).ok())
        .ok_or_else(|| bad("the grant does not decode"))?;
    if grant.channel_hash != channel.hash() {
        return Err(bad("the grant is not for the channel it names"));
    }
    let issuer =
        net_sdk::channel_issuer::ChannelLeafIssuer::new(grant, (**identity.keypair()).clone())
            .map_err(|e| bad(&e.to_string()))?;
    Ok((channel, issuer))
}

/// One served channel: its name and the roots its chains anchor at.
#[derive(Serialize, serde::Deserialize, Clone, Debug, PartialEq, Eq)]
pub(crate) struct Served {
    pub channel: String,
    pub roots: Vec<String>,
}

/// The channels served from `state_root`, if any (an unreadable or corrupt
/// file is an error: serving must not silently fall open or closed).
pub(crate) fn read_served(state_root: &Path) -> Result<Vec<Served>, String> {
    match std::fs::read(state_root.join(SERVED_FILE)) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|e| format!("{}: {e}", state_root.join(SERVED_FILE).display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(format!("{}: {e}", state_root.join(SERVED_FILE).display())),
    }
}

fn write_served(state_root: &Path, served: &[Served]) -> Result<(), String> {
    let path = state_root.join(SERVED_FILE);
    let tmp = path.with_extension("tmp");
    let bytes = serde_json::to_vec_pretty(served).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, bytes)
        .and_then(|()| std::fs::rename(&tmp, &path))
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// Register one served channel on `node`: gated, anchored at its roots.
pub(crate) fn register_served(
    node: &net::adapter::net::MeshNode,
    served: &Served,
) -> Result<(), String> {
    let channel = ChannelName::new(&served.channel).map_err(|e| e.to_string())?;
    let roots = served
        .roots
        .iter()
        .map(|r| parse_entity(r, "token root").map_err(|e| e.to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    let registry = node
        .channel_configs()
        .ok_or("this node has no channel registry")?;
    registry.insert(
        net::adapter::net::channel::ChannelConfig::new(ChannelId::new(channel))
            .with_token_roots(roots),
    );
    Ok(())
}

/// Control op `channel_serve`: validate, persist (replacing this channel's
/// entry), then register on the live node.
pub(crate) fn serve_op(
    node: &net::adapter::net::MeshNode,
    state_root: &Path,
    request: &Value,
) -> Value {
    let result = (|| {
        let name = request["channel"].as_str().unwrap_or_default();
        let channel = ChannelName::new(name).map_err(|e| format!("channel `{name}`: {e}"))?;
        let mut roots = Vec::new();
        for r in request["roots"].as_array().into_iter().flatten() {
            let root = parse_entity(r.as_str().unwrap_or_default(), "token root")
                .map_err(|e| e.to_string())?;
            let hex_root = hex::encode(root.as_bytes());
            if !roots.contains(&hex_root) {
                roots.push(hex_root);
            }
        }
        if roots.is_empty() {
            return Err("at least one token root is required".to_string());
        }
        let entry = Served {
            channel: channel.as_str().to_string(),
            roots,
        };
        let mut served = read_served(state_root)?;
        served.retain(|s| s.channel != entry.channel);
        served.push(entry.clone());
        write_served(state_root, &served)?;
        register_served(node, &entry)?;
        Ok::<_, String>(json!({
            "channel": entry.channel,
            "canonical_hash": format!("{:#018x}", channel.hash()),
            "token_roots": entry.roots,
            "served": true,
        }))
    })();
    result.unwrap_or_else(|e| json!({ "error": e }))
}

/// Control op `channel_publish`: one real publish through this node's own
/// production path, so its local channel gate decides. Returns the gate
/// verdict and this node's send counts, and whether the channel is gated
/// here at all.
pub(crate) async fn publish_op(node: &net::adapter::net::MeshNode, request: &Value) -> Value {
    let name = request["channel"].as_str().unwrap_or_default();
    let Ok(channel) = ChannelName::new(name) else {
        return json!({ "error": format!("channel `{name}` is not a canonical name") });
    };
    let data = request["data"].as_str().unwrap_or_default();
    if data.len() > MAX_PUBLISH_BYTES {
        return json!({ "error": "payload too large" });
    }
    let gated = node.channel_configs().is_some_and(|registry| {
        registry
            .get_by_name(channel.as_str())
            .is_some_and(|c| c.token_required() || c.publish_caps.is_some())
    });
    let publisher = net::adapter::net::ChannelPublisher::new(
        channel.clone(),
        net::adapter::net::PublishConfig::default(),
    );
    match node
        .publish(&publisher, net_sdk::Bytes::copy_from_slice(data.as_bytes()))
        .await
    {
        Ok(report) => json!({
            "channel": channel.as_str(),
            "gate": if gated { "passed" } else { "open: this node does not gate the channel" },
            "attempted": report.attempted,
            "delivered": report.delivered,
            "failed": report.errors.len(),
        }),
        Err(e) => json!({
            "error": format!("publish on {} refused: {e}", channel.as_str()),
            "gate": if gated { "denied" } else { "open" },
        }),
    }
}

/// One control request to the running node, its reply emitted.
async fn node_op(
    state_dir: Option<PathBuf>,
    profile_name: &str,
    request: Value,
    output: Option<OutputFormat>,
) -> Result<(), CliError> {
    let dir =
        super::lifecycle::state_dir(state_dir, profile_name)?.join(super::lifecycle::NODE_SUBDIR);
    let (_, reply) = super::lifecycle::control_call(&dir, request)
        .await
        .map_err(|e| connection_failure(format!("{e}; is `net-mesh up` running?")))?;
    if let Some(err) = reply["error"].as_str() {
        return Err(generic(err.to_string()));
    }
    emit_value(OutputFormat::resolve_oneshot(output), &reply)
        .map_err(|e| generic(format!("write result: {e}")))
}

async fn run_serve(
    args: ServeArgs,
    output: Option<OutputFormat>,
    profile_name: &str,
) -> Result<(), CliError> {
    parse_channel_name(&args.channel)?;
    let roots = args
        .token_roots
        .iter()
        .map(|r| parse_entity(r, "--token-root").map(|e| hex::encode(e.as_bytes())))
        .collect::<Result<Vec<_>, _>>()?;
    let dir = super::lifecycle::state_dir(args.state_dir, profile_name)?
        .join(super::lifecycle::NODE_SUBDIR);
    let (_, reply) = super::lifecycle::control_call(
        &dir,
        json!({ "op": "channel_serve", "channel": args.channel, "roots": roots }),
    )
    .await
    .map_err(|e| connection_failure(format!("{e}; is `net-mesh up` running?")))?;
    if let Some(err) = reply["error"].as_str() {
        return Err(generic(err.to_string()));
    }
    emit_value(OutputFormat::resolve_oneshot(output), &reply)
        .map_err(|e| generic(format!("write result: {e}")))
}

async fn run_visibility(
    args: VisibilityArgs,
    output: Option<OutputFormat>,
    config_path: Option<&std::path::Path>,
    profile_name: &str,
) -> Result<(), CliError> {
    super::scope::validate_local(args.scope.local, "channel visibility")?;
    let profile = resolve_profile(config_path, profile_name).await?;
    if args.scope.inspect_target {
        return super::scope::inspect_temporary(
            &profile,
            args.identity.as_deref(),
            args.node,
            output,
        )
        .await;
    }
    super::scope::require_local(args.scope.local, "channel visibility")?;
    let ctx = CliContext::build(&profile, args.identity.as_deref(), args.node, false).await?;
    let deck = ctx.deck();
    let view = VisibilityView {
        channel: args.channel.clone(),
        visibility: deck.channel_visibility(&args.channel).map(visibility_str),
        wire_hash: deck
            .channel_wire_hash(&args.channel)
            .map(|h| format!("{h:#06x}")),
        canonical_hash: deck
            .channel_canonical_hash(&args.channel)
            .map(|h| format!("{h:#018x}")),
    };
    emit_value(OutputFormat::resolve_oneshot(output), &view)
        .map_err(|e| generic(format!("write channel visibility: {e}")))?;
    Ok(())
}

async fn run_ls(
    args: LsArgs,
    output: Option<OutputFormat>,
    config_path: Option<&std::path::Path>,
    profile_name: &str,
) -> Result<(), CliError> {
    super::scope::validate_local(args.scope.local, "channel ls")?;
    let profile = resolve_profile(config_path, profile_name).await?;
    if args.scope.inspect_target {
        return super::scope::inspect_temporary(
            &profile,
            args.identity.as_deref(),
            args.node,
            output,
        )
        .await;
    }
    super::scope::require_local(args.scope.local, "channel ls")?;
    let ctx = CliContext::build(&profile, args.identity.as_deref(), args.node, false).await?;
    let deck = ctx.deck();
    let rows: Vec<ChannelRow> = deck
        .channels()
        .into_iter()
        .map(|(name, vis)| ChannelRow {
            channel: name,
            visibility: visibility_str(vis),
        })
        .collect();
    emit_value(OutputFormat::resolve_oneshot(output), &rows)
        .map_err(|e| generic(format!("write channel ls: {e}")))?;
    Ok(())
}

/// Stable lowercase string representation for the four
/// `Visibility` variants. Output is operator-facing and pinned
/// against external scripts; do NOT switch to Display unless its
/// rendering is also pinned.
fn visibility_str(vis: Visibility) -> String {
    match vis {
        Visibility::SubnetLocal => "subnet-local",
        Visibility::ParentVisible => "parent-visible",
        Visibility::Exported => "exported",
        Visibility::Global => "global",
    }
    .to_string()
}

#[derive(Serialize)]
struct VisibilityView {
    channel: String,
    /// `Some("global"|"parent-visible"|"subnet-local"|"exported")`
    /// when the channel is registered, `None` when it isn't (or
    /// no registry is installed).
    visibility: Option<String>,
    /// Wire `u16` hash that rides the packet header — formatted
    /// `0x____` for consistency with `gateway exports` output.
    wire_hash: Option<String>,
    /// Canonical `u64` hash that keys ACL + fold lookups.
    canonical_hash: Option<String>,
}

#[derive(Serialize)]
struct ChannelRow {
    channel: String,
    visibility: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visibility_str_round_trips_all_four_variants() {
        assert_eq!(visibility_str(Visibility::SubnetLocal), "subnet-local");
        assert_eq!(visibility_str(Visibility::ParentVisible), "parent-visible");
        assert_eq!(visibility_str(Visibility::Exported), "exported");
        assert_eq!(visibility_str(Visibility::Global), "global");
    }
}
