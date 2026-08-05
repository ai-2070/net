//! `net subnet` — two deliberately distinct groups under one verb.
//!
//! **Topology inspection** (`show|ls|tree`): operator-facing views of
//! the local mesh node's hierarchical subnet state, routed through
//! `net_sdk::deck::DeckClient`'s subnet accessors. When the
//! `DeckClient` doesn't have a `MeshNode` wired in (current
//! [`CliContext::build`] path), the commands return their natural
//! "empty" shape — `show` reports `local_subnet = null`, `ls` and
//! `tree` print empty arrays. That keeps the JSON shape stable
//! against the eventual remote-attach path landing in Phase 5.
//! Shape pinned in `SCALING_SUBNET_SPEC.md` Phase A.
//!
//! **Authority provisioning** (`keygen|issue-*|inspect`, SSDK S3,
//! `SUBNET_AUTH_SDK_PLAN.md` §5): OFFLINE ceremonies over files that
//! author the subnet AUTHORITY plane — grants, issuer grants,
//! revocation floors, and control facts. Topology is not authority;
//! the two groups share a noun and nothing else.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use net_sdk::subnets::SubnetId;
use serde::Serialize;

use crate::context::{resolve_profile, CliContext};
use crate::error::{generic, invalid_args, CliError};
use crate::prelude::{emit_value, OutputFormat};

#[derive(Subcommand, Debug)]
pub enum SubnetCommand {
    /// Show this node's `SubnetId` and the policy that derived it.
    Show(ShowArgs),
    /// List every subnet known to this node, with the member
    /// `node_id` set per subnet.
    Ls(LsArgs),
    /// Render the subnet hierarchy as an indented tree.
    Tree(TreeArgs),
    /// Generate a fresh subnet authority keypair (OFFLINE key
    /// material, SSDK S3). Usable as an authority root or as a
    /// delegated issuer; signs grants, floors, and control facts and
    /// never touches a mesh node. Prints the public entity id — never
    /// the seed.
    Keygen(SubnetKeygenArgs),
    /// Issue one DIRECT credential set: authority root → subject.
    /// Writes the framed `SubnetCredentialSet` wire bytes a gateway
    /// installs.
    IssueDirect(IssueDirectArgs),
    /// Issue one bounded ISSUER grant: authority root → delegated
    /// issuer. Writes the signed intermediate artifact
    /// `issue-delegated` later consumes; one-hop depth is structural.
    IssueIssuer(IssueIssuerArgs),
    /// Issue one DELEGATED credential set: a leaf signed by a
    /// delegated issuer, framed together with its issuer grant.
    ///
    /// Validates STRUCTURE and ATTENUATION only — that the leaf scope
    /// stays inside the issuer grant's scope and its rights do not
    /// exceed the maximum. It does NOT verify the issuer grant's
    /// signature against an authority root, because no trusted root is
    /// supplied to this offline ceremony. Successful issuance therefore
    /// does not prove deployability: a forged issuer grant frames
    /// cleanly here and every verifier rejects the result.
    IssueDelegated(IssueDelegatedArgs),
    /// Issue one signed control fact (descriptor, gateway
    /// advertisement, export policy, or revocation floor), written as
    /// the outer `SubnetControlFact` wire frame that
    /// `apply_subnet_control_fact` consumes.
    IssueControlFact(IssueControlFactArgs),
    /// Decode and summarize a subnet artifact file (credential set,
    /// issuer grant, or control fact) WITHOUT private material.
    /// Exits non-zero for malformed or non-canonical data.
    Inspect(InspectArgs),
}

#[derive(Args, Debug)]
pub struct ShowArgs {
    #[arg(long)]
    pub identity: Option<PathBuf>,

    #[arg(long, default_value_t = crate::prelude::DEFAULT_SUPERVISOR_NODE)]
    pub node: u64,
}

#[derive(Args, Debug)]
pub struct LsArgs {
    #[arg(long)]
    pub identity: Option<PathBuf>,

    #[arg(long, default_value_t = crate::prelude::DEFAULT_SUPERVISOR_NODE)]
    pub node: u64,
}

#[derive(Args, Debug)]
pub struct TreeArgs {
    #[arg(long)]
    pub identity: Option<PathBuf>,

    #[arg(long, default_value_t = crate::prelude::DEFAULT_SUPERVISOR_NODE)]
    pub node: u64,
}

pub async fn run(
    cmd: SubnetCommand,
    output: Option<OutputFormat>,
    config_path: Option<&std::path::Path>,
    profile_name: &str,
) -> Result<(), CliError> {
    match cmd {
        SubnetCommand::Show(args) => run_show(args, output, config_path, profile_name).await,
        SubnetCommand::Ls(args) => run_ls(args, output, config_path, profile_name).await,
        SubnetCommand::Tree(args) => run_tree(args, output, config_path, profile_name).await,
        // The issuance verbs are OFFLINE: no profile, no node, no mesh.
        SubnetCommand::Keygen(args) => run_subnet_keygen(args, output).await,
        SubnetCommand::IssueDirect(args) => run_issue_direct(args, output).await,
        SubnetCommand::IssueIssuer(args) => run_issue_issuer(args, output).await,
        SubnetCommand::IssueDelegated(args) => run_issue_delegated(args, output).await,
        SubnetCommand::IssueControlFact(args) => run_issue_control_fact(args, output).await,
        SubnetCommand::Inspect(args) => run_inspect(args, output).await,
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
    let deck = ctx.deck();
    let view = ShowView {
        local_subnet: deck.local_subnet().map(format_subnet),
        depth: deck.local_subnet().map(|s| s.depth()),
        known_peer_count: deck.known_subnets().len() as u64,
    };
    emit_value(OutputFormat::resolve_oneshot(output), &view)
        .map_err(|e| generic(format!("write subnet show: {e}")))?;
    Ok(())
}

async fn run_ls(
    args: LsArgs,
    output: Option<OutputFormat>,
    config_path: Option<&std::path::Path>,
    profile_name: &str,
) -> Result<(), CliError> {
    let profile = resolve_profile(config_path, profile_name).await?;
    let local_node_id = args.node;
    let ctx = CliContext::build(&profile, args.identity.as_deref(), local_node_id, false).await?;
    let deck = ctx.deck();
    // The deck handles the bucket-by-subnet grouping so the deck
    // SUBNETS tab and this CLI surface stay in sync. Pass the
    // local node id so the local subnet's row carries it as a
    // member (the substrate's `cfg.this_node` uses the same value).
    let rows: Vec<SubnetRow> = deck
        .subnets_with_members(Some(local_node_id))
        .into_iter()
        .map(|r| SubnetRow {
            subnet: format_subnet(r.subnet),
            depth: r.subnet.depth(),
            member_count: r.members.len() as u64,
            members: r.members,
        })
        .collect();
    emit_value(OutputFormat::resolve_oneshot(output), &rows)
        .map_err(|e| generic(format!("write subnet ls: {e}")))?;
    Ok(())
}

async fn run_tree(
    args: TreeArgs,
    output: Option<OutputFormat>,
    config_path: Option<&std::path::Path>,
    profile_name: &str,
) -> Result<(), CliError> {
    let profile = resolve_profile(config_path, profile_name).await?;
    let ctx = CliContext::build(&profile, args.identity.as_deref(), args.node, false).await?;
    let deck = ctx.deck();
    let mut all_subnets: BTreeSet<u32> = BTreeSet::new();
    if let Some(local) = deck.local_subnet() {
        all_subnets.insert(local.raw());
    }
    for (_node_id, subnet) in deck.known_subnets() {
        all_subnets.insert(subnet.raw());
    }
    // For every subnet, also include every ancestor — so a tree
    // render shows the full path even when only deep subnets have
    // members.
    let mut closure: BTreeSet<u32> = BTreeSet::new();
    for &raw in &all_subnets {
        let mut cursor = SubnetId::from_raw(raw);
        loop {
            closure.insert(cursor.raw());
            if cursor.is_global() {
                break;
            }
            cursor = cursor.parent();
        }
    }
    // Convert to depth-then-raw-sorted rendering.
    let mut nodes: Vec<SubnetId> = closure.into_iter().map(SubnetId::from_raw).collect();
    nodes.sort_by_key(|s| (s.depth(), s.raw()));
    let rows: Vec<TreeRow> = nodes
        .into_iter()
        .map(|s| TreeRow {
            subnet: format_subnet(s),
            depth: s.depth(),
            parent: if s.is_global() {
                None
            } else {
                Some(format_subnet(s.parent()))
            },
            is_local: deck.local_subnet() == Some(s),
        })
        .collect();
    emit_value(OutputFormat::resolve_oneshot(output), &rows)
        .map_err(|e| generic(format!("write subnet tree: {e}")))?;
    Ok(())
}

/// Render a `SubnetId` for operator-facing output. Stable string
/// that round-trips through human inspection (e.g. `"3.7.2"` for
/// `SubnetId::new(&[3, 7, 2])`, `"global"` for `SubnetId::GLOBAL`).
fn format_subnet(subnet: SubnetId) -> String {
    subnet.to_string()
}

#[derive(Serialize)]
struct ShowView {
    /// `Some("3.7.2")` when a mesh is attached, `None` otherwise.
    local_subnet: Option<String>,
    /// Subnet hierarchy depth (0 for `SubnetId::GLOBAL`).
    depth: Option<u8>,
    /// How many peers this node has cached a subnet for. Reflects
    /// `MeshNode::known_subnets().len()`.
    known_peer_count: u64,
}

#[derive(Serialize)]
struct SubnetRow {
    subnet: String,
    depth: u8,
    member_count: u64,
    members: Vec<u64>,
}

#[derive(Serialize)]
struct TreeRow {
    subnet: String,
    depth: u8,
    /// `None` for `SubnetId::GLOBAL`; otherwise the parent
    /// subnet's rendered form.
    parent: Option<String>,
    /// `true` when this row matches the local node's `SubnetId`.
    is_local: bool,
}

// =========================================================================
// SSDK S3 — offline authority provisioning (SUBNET_AUTH_SDK_PLAN.md §5)
// =========================================================================
//
// The subnet authority key is OFFLINE key material, exactly like the org
// root: it signs grants, issuer grants, revocation floors, and control
// facts on an operator machine and never touches a mesh node. Key files
// are TOML (0600 on Unix, ssh-style permission gate on read); every
// SIGNED artifact is written as its framed CANONICAL WIRE BYTES via the
// core `to_bytes` — never a JSON mirror — and published through the same
// race-free stage-beside/no-clobber pipeline as the org verbs.

use crate::commands::identity::{
    check_strict_permissions, enforce_strict_permissions, now_iso8601, parse_entity_hex,
};
use crate::commands::org::{
    publish_staged, publish_staged_replace, refuse_aliased_paths, refuse_existing,
    refuse_replacing_foreign_seed, stage_beside, warn_secret_permissions, SeedArtifact,
};
use crate::secret::{zeroize_slice, zeroize_string, ScrubbedBytes, ScrubbedString};
use net::adapter::net::identity::EntityKeypair;
use net::adapter::net::subnet::{
    GatewayAdvertisement, SubnetAuthError, SubnetControlFact, SubnetCredentialSet,
    SubnetDescriptor, SubnetExportPolicy, SubnetGrant, SubnetIssuerGrant, SubnetRef,
    SubnetRevocationFloor, SubnetRights, TopologySubnetId,
};

/// Default credential TTL — 7 days, the org-grant cadence; the core
/// hard-caps at 30 days, rejected at issue AND at every verifier.
const SUBNET_TTL_SECS_DEFAULT: u64 = 7 * 24 * 60 * 60;

/// Default `not_before` headroom: sixty seconds in the past, so a
/// freshly issued credential is usable immediately across small clock
/// skew (the verifier applies its own bounded skew tolerance on top).
const NOT_BEFORE_HEADROOM_SECS: u64 = 60;

#[derive(Args, Debug)]
pub struct SubnetKeygenArgs {
    /// Output path. Defaults to
    /// `$XDG_CONFIG_HOME/net-mesh/subnets/subnet-<id>.toml`.
    #[arg(long)]
    pub out: Option<PathBuf>,

    /// Free-form note saved alongside the key.
    #[arg(long)]
    pub note: Option<String>,

    /// Overwrite an existing SUBNET key at this path. Refuses by
    /// default, and always refuses to replace a different kind of
    /// secret (an org key or operator identity), however the path is
    /// spelled.
    #[arg(long)]
    pub force: bool,

    /// Acknowledge that the key's 0600 mode is not enforced on Windows
    /// and suppress the warning.
    #[arg(long = "accept-windows-dacl")]
    pub accept_windows_dacl: bool,
}

#[derive(Args, Debug)]
pub struct IssueDirectArgs {
    /// Path to the AUTHORITY ROOT key file (from `subnet keygen`).
    #[arg(long = "root-key", value_name = "PATH")]
    pub root_key: PathBuf,

    /// The authority id this grant belongs to (64 hex chars).
    /// EXPLICIT on purpose: an authority may trust multiple roots, so
    /// the id is never silently derived from the signing key.
    #[arg(long)]
    pub authority: String,

    /// The subject entity granted the rights (64 hex chars).
    #[arg(long)]
    pub subject: String,

    /// Grant scope: a dotted path (`3.9`) or `global`. `global` is the
    /// WHOLE-AUTHORITY root scope covering every present and future
    /// path — never an "unscoped" default.
    #[arg(long)]
    pub scope: String,

    /// Rights, comma-separated from `attach`, `route`, `export`.
    #[arg(long)]
    pub rights: String,

    /// Topology epoch the grant is minted under.
    #[arg(long = "topology-epoch", default_value_t = 0)]
    pub topology_epoch: u32,

    /// Revocation generation. Issue at ≥ the authority's current floor
    /// for this scope; raise floors via `issue-control-fact
    /// revocation-floor` to retire outstanding grants.
    #[arg(long, default_value_t = 1)]
    pub generation: u32,

    /// Validity start (unix seconds). Defaults to now minus a small
    /// clock-skew headroom.
    #[arg(long = "not-before")]
    pub not_before: Option<u64>,

    /// Validity width in seconds. Defaults to 7 days; hard-capped at
    /// 30 days by the core.
    #[arg(long = "ttl-secs", default_value_t = SUBNET_TTL_SECS_DEFAULT)]
    pub ttl_secs: u64,

    /// Output path for the framed credential-set wire bytes.
    #[arg(long)]
    pub out: PathBuf,

    /// Overwrite an existing file (atomic replace). Refuses by default.
    #[arg(long)]
    pub force: bool,

    /// Allow permissive root-key file modes on Unix.
    #[arg(long)]
    pub insecure_permissions: bool,
}

#[derive(Args, Debug)]
pub struct IssueIssuerArgs {
    /// Path to the AUTHORITY ROOT key file (from `subnet keygen`).
    #[arg(long = "root-key", value_name = "PATH")]
    pub root_key: PathBuf,

    /// The authority id (64 hex chars). Explicit, never derived.
    #[arg(long)]
    pub authority: String,

    /// The delegated ISSUER entity (64 hex chars) permitted to sign
    /// leaves inside this envelope.
    #[arg(long)]
    pub issuer: String,

    /// Issuer scope ceiling: dotted path or `global`.
    #[arg(long)]
    pub scope: String,

    /// Maximum rights the issuer may put on a leaf, comma-separated
    /// from `attach`, `route`, `export`.
    #[arg(long = "max-rights")]
    pub max_rights: String,

    /// Topology epoch.
    #[arg(long = "topology-epoch", default_value_t = 0)]
    pub topology_epoch: u32,

    /// Revocation generation.
    #[arg(long, default_value_t = 1)]
    pub generation: u32,

    /// Validity start (unix seconds); defaults to now minus headroom.
    #[arg(long = "not-before")]
    pub not_before: Option<u64>,

    /// Validity width in seconds; defaults to 7 days.
    #[arg(long = "ttl-secs", default_value_t = SUBNET_TTL_SECS_DEFAULT)]
    pub ttl_secs: u64,

    /// Output path for the signed issuer-grant wire bytes.
    #[arg(long)]
    pub out: PathBuf,

    /// Overwrite an existing file (atomic replace). Refuses by default.
    #[arg(long)]
    pub force: bool,

    /// Allow permissive root-key file modes on Unix.
    #[arg(long)]
    pub insecure_permissions: bool,
}

#[derive(Args, Debug)]
pub struct IssueDelegatedArgs {
    /// Path to the issuer-grant wire bytes (from `issue-issuer`).
    #[arg(long = "issuer-grant", value_name = "PATH")]
    pub issuer_grant: PathBuf,

    /// Path to the DELEGATED ISSUER's key file (from `subnet keygen`)
    /// — the key that signs the leaf.
    #[arg(long = "issuer-key", value_name = "PATH")]
    pub issuer_key: PathBuf,

    /// The subject entity granted the rights (64 hex chars).
    #[arg(long)]
    pub subject: String,

    /// Leaf scope: dotted path or `global`. Must stay inside the
    /// issuer grant's scope (checked with the core containment
    /// predicate here for a clear early error; every verifier
    /// re-checks).
    #[arg(long)]
    pub scope: String,

    /// Leaf rights, comma-separated; must not exceed the issuer
    /// grant's maximum rights.
    #[arg(long)]
    pub rights: String,

    /// Revocation generation.
    #[arg(long, default_value_t = 1)]
    pub generation: u32,

    /// Validity start (unix seconds); defaults to now minus headroom.
    #[arg(long = "not-before")]
    pub not_before: Option<u64>,

    /// Validity width in seconds; defaults to 7 days.
    #[arg(long = "ttl-secs", default_value_t = SUBNET_TTL_SECS_DEFAULT)]
    pub ttl_secs: u64,

    /// Output path for the framed delegated credential-set wire bytes
    /// (issuer grant + leaf, one file).
    #[arg(long)]
    pub out: PathBuf,

    /// Overwrite an existing file (atomic replace). Refuses by default.
    #[arg(long)]
    pub force: bool,

    /// Allow permissive issuer-key file modes on Unix.
    #[arg(long)]
    pub insecure_permissions: bool,
}

#[derive(Args, Debug)]
pub struct IssueControlFactArgs {
    #[command(subcommand)]
    pub kind: ControlFactKindCommand,
}

#[derive(Subcommand, Debug)]
pub enum ControlFactKindCommand {
    /// A root-signed "this path is live under epoch E" declaration.
    Descriptor(FactDescriptorArgs),
    /// A root-signed gateway advertisement for a scope.
    GatewayAdvertisement(FactGatewayArgs),
    /// A root-signed export policy naming canonical channels.
    ExportPolicy(FactExportPolicyArgs),
    /// A root-signed revocation floor, distributed as a fact.
    RevocationFloor(FactFloorArgs),
}

#[derive(Args, Debug)]
pub struct FactCommonArgs {
    /// Path to the AUTHORITY ROOT key file.
    #[arg(long = "root-key", value_name = "PATH")]
    pub root_key: PathBuf,

    /// The authority id (64 hex chars). Explicit, never derived.
    #[arg(long)]
    pub authority: String,

    /// The fact's scope path: dotted path or `global`.
    #[arg(long)]
    pub scope: String,

    /// Topology epoch. EXPLICIT: a fact never invents authority
    /// movement — reparenting is an operator decision recorded by a
    /// new epoch, not a side effect of issuing a fact.
    #[arg(long = "topology-epoch")]
    pub topology_epoch: u32,

    /// Monotonic revision within `(scope, fact kind)`.
    #[arg(long)]
    pub revision: u64,

    /// Output path for the framed control-fact wire bytes.
    #[arg(long)]
    pub out: PathBuf,

    /// Overwrite an existing file (atomic replace). Refuses by default.
    #[arg(long)]
    pub force: bool,

    /// Allow permissive root-key file modes on Unix.
    #[arg(long)]
    pub insecure_permissions: bool,
}

#[derive(Args, Debug)]
pub struct FactDescriptorArgs {
    #[command(flatten)]
    pub common: FactCommonArgs,
}

#[derive(Args, Debug)]
pub struct FactGatewayArgs {
    #[command(flatten)]
    pub common: FactCommonArgs,

    /// The advertised gateway's entity id (64 hex chars).
    #[arg(long)]
    pub gateway: String,

    /// The gateway's routing node id (u64; decimal or 0x-hex).
    #[arg(long = "gateway-node")]
    pub gateway_node: String,

    /// Validity start (unix seconds); defaults to now minus headroom.
    #[arg(long = "not-before")]
    pub not_before: Option<u64>,

    /// Validity width in seconds; defaults to 7 days.
    #[arg(long = "ttl-secs", default_value_t = SUBNET_TTL_SECS_DEFAULT)]
    pub ttl_secs: u64,
}

#[derive(Args, Debug)]
pub struct FactExportPolicyArgs {
    #[command(flatten)]
    pub common: FactCommonArgs,

    /// An exported channel — the canonical NAME (preferred; hashed
    /// directly) or exactly lowercase `0x` + 16 lowercase hex digits.
    /// Repeatable.
    #[arg(long = "channel", required = true)]
    pub channels: Vec<String>,

    /// Validity start (unix seconds); defaults to now minus headroom.
    #[arg(long = "not-before")]
    pub not_before: Option<u64>,

    /// Validity width in seconds; defaults to 7 days.
    #[arg(long = "ttl-secs", default_value_t = SUBNET_TTL_SECS_DEFAULT)]
    pub ttl_secs: u64,
}

#[derive(Args, Debug)]
pub struct FactFloorArgs {
    #[command(flatten)]
    pub common: FactCommonArgs,

    /// Grants scoped to this subtree with generation BELOW this value
    /// are revoked, monotonically.
    #[arg(long = "minimum-generation")]
    pub minimum_generation: u32,
}

#[derive(Args, Debug)]
pub struct InspectArgs {
    /// Path to a subnet artifact file (credential set, issuer grant,
    /// or control fact wire bytes).
    pub file: PathBuf,
}

// -------------------------------------------------------------------------
// keygen
// -------------------------------------------------------------------------

async fn run_subnet_keygen(
    args: SubnetKeygenArgs,
    output: Option<OutputFormat>,
) -> Result<(), CliError> {
    let keypair = EntityKeypair::generate();
    let entity_id_hex = hex::encode(keypair.entity_id().as_bytes());

    let path = match args.out {
        Some(explicit) => explicit,
        None => default_subnet_key_path(&entity_id_hex).ok_or_else(|| {
            crate::error::invalid_args(
                "cannot resolve the platform config directory, and refusing to fall back to \
                 the working directory — this file holds the SUBNET AUTHORITY SEED. Pass an \
                 explicit --out."
                    .to_string(),
            )
        })?,
    };
    refuse_existing(&path, args.force).await?;
    if args.force {
        // `--force` replaces a SUBNET key only — never an org key or an
        // operator identity, however the path is spelled.
        refuse_replacing_foreign_seed(&path, SeedArtifact::SubnetKey).await?;
    }

    let mut seed = *keypair.secret_bytes();
    let file = SubnetKeyFile {
        kind: SUBNET_KEY_KIND.to_string(),
        entity_id_hex: entity_id_hex.clone(),
        seed_hex: hex::encode(seed),
        created_at: now_iso8601(),
        note: args.note.clone(),
    };
    zeroize_slice(&mut seed);
    let toml_text = ScrubbedString::new(
        toml::to_string_pretty(&file)
            .map_err(|e| generic(format!("failed to serialize subnet key TOML: {e}")))?,
    );

    let tmp = stage_beside(&path, toml_text.as_bytes(), true).await?;
    if args.force {
        publish_staged_replace(&tmp, &path).await?;
    } else {
        publish_staged(&tmp, &path).await?;
    }
    enforce_strict_permissions(&path).await?;
    warn_secret_permissions(&path, args.accept_windows_dacl);

    // Public summary only — never the seed. The authority id is an
    // ISSUANCE input, not derived from this key: print the entity id
    // under its own name.
    let summary = SubnetKeySummary {
        path: path.display().to_string(),
        entity_id_hex,
        created_at: file.created_at.clone(),
        note: file.note.clone(),
    };
    emit_value(OutputFormat::resolve_oneshot(output), &summary)
        .map_err(|e| generic(format!("write summary: {e}")))?;
    Ok(())
}

// -------------------------------------------------------------------------
// issue-direct / issue-issuer / issue-delegated
// -------------------------------------------------------------------------

async fn run_issue_direct(
    args: IssueDirectArgs,
    output: Option<OutputFormat>,
) -> Result<(), CliError> {
    let keypair = load_subnet_key(&args.root_key, args.insecure_permissions).await?;
    let authority = parse_entity_hex(&args.authority)?;
    let subject = parse_entity_hex(&args.subject)?;
    let scope = parse_subnet_path(&args.scope)?;
    let rights = parse_subnet_rights(&args.rights)?;
    let not_before = args
        .not_before
        .unwrap_or_else(|| unix_now().saturating_sub(NOT_BEFORE_HEADROOM_SECS));

    let grant = SubnetGrant::try_issue(
        &keypair,
        authority.clone(),
        scope,
        args.topology_epoch,
        subject.clone(),
        rights,
        args.generation,
        not_before,
        args.ttl_secs,
    )
    .map_err(|e| invalid_args(format!("issue-direct: subnet:{e}")))?;
    let set = SubnetCredentialSet::Direct(grant);

    publish_wire_artifact(
        &set.to_bytes(),
        &args.out,
        args.force,
        &[("--root-key", &args.root_key)],
    )
    .await?;

    let summary = IssueCredentialOutput {
        path: args.out.display().to_string(),
        artifact: "credential-set-direct".to_string(),
        authority_hex: hex::encode(authority.as_bytes()),
        subject_hex: hex::encode(subject.as_bytes()),
        scope: format_subnet(scope),
        rights: format_subnet_rights(rights),
        topology_epoch: args.topology_epoch,
        generation: args.generation,
        not_before,
        not_after: not_before.saturating_add(args.ttl_secs),
    };
    emit_value(OutputFormat::resolve_oneshot(output), &summary)
        .map_err(|e| generic(format!("write summary: {e}")))?;
    Ok(())
}

async fn run_issue_issuer(
    args: IssueIssuerArgs,
    output: Option<OutputFormat>,
) -> Result<(), CliError> {
    let keypair = load_subnet_key(&args.root_key, args.insecure_permissions).await?;
    let authority = parse_entity_hex(&args.authority)?;
    let issuer = parse_entity_hex(&args.issuer)?;
    let scope = parse_subnet_path(&args.scope)?;
    let max_rights = parse_subnet_rights(&args.max_rights)?;
    let not_before = args
        .not_before
        .unwrap_or_else(|| unix_now().saturating_sub(NOT_BEFORE_HEADROOM_SECS));

    let grant = SubnetIssuerGrant::try_issue(
        &keypair,
        authority.clone(),
        scope,
        args.topology_epoch,
        issuer.clone(),
        max_rights,
        args.generation,
        not_before,
        args.ttl_secs,
    )
    .map_err(|e| invalid_args(format!("issue-issuer: subnet:{e}")))?;

    publish_wire_artifact(
        &grant.to_bytes(),
        &args.out,
        args.force,
        &[("--root-key", &args.root_key)],
    )
    .await?;

    let summary = IssueCredentialOutput {
        path: args.out.display().to_string(),
        artifact: "issuer-grant".to_string(),
        authority_hex: hex::encode(authority.as_bytes()),
        subject_hex: hex::encode(issuer.as_bytes()),
        scope: format_subnet(scope),
        rights: format_subnet_rights(max_rights),
        topology_epoch: args.topology_epoch,
        generation: args.generation,
        not_before,
        not_after: not_before.saturating_add(args.ttl_secs),
    };
    emit_value(OutputFormat::resolve_oneshot(output), &summary)
        .map_err(|e| generic(format!("write summary: {e}")))?;
    Ok(())
}

async fn run_issue_delegated(
    args: IssueDelegatedArgs,
    output: Option<OutputFormat>,
) -> Result<(), CliError> {
    let issuer_grant_bytes = tokio::fs::read(&args.issuer_grant).await.map_err(|e| {
        generic(format!(
            "failed to read issuer grant {}: {e}",
            args.issuer_grant.display()
        ))
    })?;
    let issuer_grant = SubnetIssuerGrant::from_bytes(&issuer_grant_bytes)
        .map_err(|e| invalid_args(format!("issuer grant does not decode: subnet:{e}")))?;
    let issuer_kp = load_subnet_key(&args.issuer_key, args.insecure_permissions).await?;
    if issuer_kp.entity_id() != &issuer_grant.issuer {
        return Err(invalid_args(
            "the --issuer-key does not match the issuer named by the --issuer-grant".to_string(),
        ));
    }

    let subject = parse_entity_hex(&args.subject)?;
    let scope = parse_subnet_path(&args.scope)?;
    let rights = parse_subnet_rights(&args.rights)?;
    // Early, clear refusals via the CORE predicates (every verifier
    // re-checks; nothing is decided here that the verifier would not).
    if !issuer_grant.scope.is_ancestor_or_self_of(scope) {
        return Err(invalid_args(format!(
            "leaf scope {} escapes the issuer scope {} (subnet:scope_not_ancestor)",
            format_subnet(scope),
            format_subnet(issuer_grant.scope),
        )));
    }
    if !issuer_grant.maximum_rights.contains(rights) {
        return Err(invalid_args(format!(
            "leaf rights {} exceed the issuer maximum {} (subnet:issuer_attenuation_broadened)",
            format_subnet_rights(rights),
            format_subnet_rights(issuer_grant.maximum_rights),
        )));
    }
    let not_before = args
        .not_before
        .unwrap_or_else(|| unix_now().saturating_sub(NOT_BEFORE_HEADROOM_SECS));

    let leaf = SubnetGrant::try_issue(
        &issuer_kp,
        issuer_grant.authority.clone(),
        scope,
        issuer_grant.topology_epoch,
        subject.clone(),
        rights,
        args.generation,
        not_before,
        args.ttl_secs,
    )
    .map_err(|e| invalid_args(format!("issue-delegated: subnet:{e}")))?;
    let authority_hex = hex::encode(issuer_grant.authority.as_bytes());
    let topology_epoch = issuer_grant.topology_epoch;
    let set = SubnetCredentialSet::OneHop { issuer_grant, leaf };

    publish_wire_artifact(
        &set.to_bytes(),
        &args.out,
        args.force,
        &[
            ("--issuer-grant", &args.issuer_grant),
            ("--issuer-key", &args.issuer_key),
        ],
    )
    .await?;

    let summary = IssueCredentialOutput {
        path: args.out.display().to_string(),
        artifact: "credential-set-delegated".to_string(),
        authority_hex,
        subject_hex: hex::encode(subject.as_bytes()),
        scope: format_subnet(scope),
        rights: format_subnet_rights(rights),
        topology_epoch,
        generation: args.generation,
        not_before,
        not_after: not_before.saturating_add(args.ttl_secs),
    };
    emit_value(OutputFormat::resolve_oneshot(output), &summary)
        .map_err(|e| generic(format!("write summary: {e}")))?;
    Ok(())
}

// -------------------------------------------------------------------------
// issue-control-fact
// -------------------------------------------------------------------------

async fn run_issue_control_fact(
    args: IssueControlFactArgs,
    output: Option<OutputFormat>,
) -> Result<(), CliError> {
    let (common, fact) = match args.kind {
        ControlFactKindCommand::Descriptor(a) => {
            let (kp, scope) = fact_prelude(&a.common).await?;
            let fact = SubnetDescriptor::try_issue(
                &kp,
                scope,
                a.common.topology_epoch,
                a.common.revision,
                unix_now(),
            )
            .map_err(|e| invalid_args(format!("descriptor: subnet:{e}")))?;
            (a.common, SubnetControlFact::Descriptor(fact))
        }
        ControlFactKindCommand::GatewayAdvertisement(a) => {
            let (kp, scope) = fact_prelude(&a.common).await?;
            let gateway = parse_entity_hex(&a.gateway)?;
            let gateway_node = parse_u64_arg("--gateway-node", &a.gateway_node)?;
            let not_before = a
                .not_before
                .unwrap_or_else(|| unix_now().saturating_sub(NOT_BEFORE_HEADROOM_SECS));
            let fact = GatewayAdvertisement::try_issue(
                &kp,
                scope,
                a.common.topology_epoch,
                gateway,
                gateway_node,
                a.common.revision,
                not_before,
                not_before.saturating_add(a.ttl_secs),
            )
            .map_err(|e| invalid_args(format!("gateway-advertisement: subnet:{e}")))?;
            (a.common, SubnetControlFact::GatewayAdvertisement(fact))
        }
        ControlFactKindCommand::ExportPolicy(a) => {
            let (kp, scope) = fact_prelude(&a.common).await?;
            let mut channels = Vec::with_capacity(a.channels.len());
            for raw in &a.channels {
                channels.push(crate::commands::gateway::parse_channel_hash(raw)?);
            }
            let not_before = a
                .not_before
                .unwrap_or_else(|| unix_now().saturating_sub(NOT_BEFORE_HEADROOM_SECS));
            let fact = SubnetExportPolicy::try_issue(
                &kp,
                scope,
                a.common.topology_epoch,
                channels,
                a.common.revision,
                not_before,
                not_before.saturating_add(a.ttl_secs),
            )
            .map_err(|e| invalid_args(format!("export-policy: subnet:{e}")))?;
            (a.common, SubnetControlFact::ExportPolicy(fact))
        }
        ControlFactKindCommand::RevocationFloor(a) => {
            let (kp, scope) = fact_prelude(&a.common).await?;
            let fact = SubnetRevocationFloor::try_issue(
                &kp,
                scope,
                a.common.topology_epoch,
                a.minimum_generation,
                a.common.revision,
                unix_now(),
            )
            .map_err(|e| invalid_args(format!("revocation-floor: subnet:{e}")))?;
            (a.common, SubnetControlFact::RevocationFloor(fact))
        }
    };

    publish_wire_artifact(
        &fact.to_bytes(),
        &common.out,
        common.force,
        &[("--root-key", &common.root_key)],
    )
    .await?;

    let summary = IssueFactOutput {
        path: common.out.display().to_string(),
        artifact: "control-fact".to_string(),
        kind: net_sdk::subnet::fact_kind_wire(fact.kind()).to_string(),
        authority_hex: hex::encode(fact.scope().authority.as_bytes()),
        scope: format_subnet(fact.scope().path),
        topology_epoch: common.topology_epoch,
        revision: common.revision,
    };
    emit_value(OutputFormat::resolve_oneshot(output), &summary)
        .map_err(|e| generic(format!("write summary: {e}")))?;
    Ok(())
}

/// Shared prelude for every fact kind: load the signing key and parse
/// the authority-qualified scope.
async fn fact_prelude(common: &FactCommonArgs) -> Result<(EntityKeypair, SubnetRef), CliError> {
    let kp = load_subnet_key(&common.root_key, common.insecure_permissions).await?;
    let authority = parse_entity_hex(&common.authority)?;
    let path = parse_subnet_path(&common.scope)?;
    Ok((kp, SubnetRef { authority, path }))
}

// -------------------------------------------------------------------------
// inspect
// -------------------------------------------------------------------------

async fn run_inspect(args: InspectArgs, output: Option<OutputFormat>) -> Result<(), CliError> {
    let bytes = tokio::fs::read(&args.file)
        .await
        .map_err(|e| generic(format!("failed to read {}: {e}", args.file.display())))?;

    // Try each strict decoder in frame-specificity order. Signatures
    // are summarized, not verified — verification belongs to the
    // consuming node against ITS configured roots.
    let view = if let Ok(fact) = SubnetControlFact::from_bytes(&bytes) {
        serde_json::json!({
            "artifact": "control-fact",
            "kind": net_sdk::subnet::fact_kind_wire(fact.kind()),
            "authority_hex": hex::encode(fact.scope().authority.as_bytes()),
            "scope": format_subnet(fact.scope().path),
        })
    } else if let Ok(set) = SubnetCredentialSet::from_bytes(&bytes) {
        let leaf = set.leaf();
        let mut v = serde_json::json!({
            "artifact": match &set {
                SubnetCredentialSet::Direct(_) => "credential-set-direct",
                SubnetCredentialSet::OneHop { .. } => "credential-set-delegated",
            },
            "authority_hex": hex::encode(leaf.authority.as_bytes()),
            "subject_hex": hex::encode(leaf.subject.as_bytes()),
            "scope": format_subnet(leaf.scope),
            "rights": format_subnet_rights(leaf.rights),
            "topology_epoch": leaf.topology_epoch,
            "generation": leaf.generation,
            "not_before": leaf.not_before,
            "not_after": leaf.not_after,
        });
        if let SubnetCredentialSet::OneHop { issuer_grant, .. } = &set {
            v["issuer_hex"] = serde_json::json!(hex::encode(issuer_grant.issuer.as_bytes()));
            v["issuer_scope"] = serde_json::json!(format_subnet(issuer_grant.scope));
            v["issuer_max_rights"] =
                serde_json::json!(format_subnet_rights(issuer_grant.maximum_rights));
        }
        v
    } else if let Ok(grant) = SubnetIssuerGrant::from_bytes(&bytes) {
        serde_json::json!({
            "artifact": "issuer-grant",
            "authority_hex": hex::encode(grant.authority.as_bytes()),
            "issuer_hex": hex::encode(grant.issuer.as_bytes()),
            "scope": format_subnet(grant.scope),
            "max_rights": format_subnet_rights(grant.maximum_rights),
            "topology_epoch": grant.topology_epoch,
            "generation": grant.generation,
            "not_before": grant.not_before,
            "not_after": grant.not_after,
        })
    } else {
        return Err(invalid_args(format!(
            "{} is not a recognized subnet artifact (subnet:{})",
            args.file.display(),
            SubnetAuthError::InvalidFormat,
        )));
    };

    emit_value(OutputFormat::resolve_oneshot(output), &view)
        .map_err(|e| generic(format!("write inspect view: {e}")))?;
    Ok(())
}

// -------------------------------------------------------------------------
// Disk shapes + helpers
// -------------------------------------------------------------------------

const SUBNET_KEY_KIND: &str = "subnet-authority-key";

#[derive(Serialize, serde::Deserialize)]
struct SubnetKeyFile {
    /// Explicit kind marker so `classify_seed_artifact` never confuses
    /// this with an operator identity (both carry a bare seed).
    kind: String,
    entity_id_hex: String,
    seed_hex: String,
    created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    note: Option<String>,
}

impl Drop for SubnetKeyFile {
    fn drop(&mut self) {
        // The authority seed rides in `seed_hex`; scrub on drop. No
        // `Debug` derive — this struct must never render into a log.
        zeroize_string(&mut self.seed_hex);
    }
}

#[derive(Serialize)]
struct SubnetKeySummary {
    path: String,
    entity_id_hex: String,
    created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
}

#[derive(Serialize)]
struct IssueCredentialOutput {
    path: String,
    artifact: String,
    authority_hex: String,
    subject_hex: String,
    scope: String,
    rights: String,
    topology_epoch: u32,
    generation: u32,
    not_before: u64,
    not_after: u64,
}

#[derive(Serialize)]
struct IssueFactOutput {
    path: String,
    artifact: String,
    kind: String,
    authority_hex: String,
    scope: String,
    topology_epoch: u32,
    revision: u64,
}

fn default_subnet_key_path(entity_id_hex: &str) -> Option<PathBuf> {
    let short = &entity_id_hex[..entity_id_hex.len().min(16)];
    Some(
        dirs::config_dir()?
            .join("net-mesh")
            .join("subnets")
            .join(format!("subnet-{short}.toml")),
    )
}

/// Load + parse a subnet authority key file, honoring the ssh-style
/// permission gate. Mirrors `load_org_key` exactly — the seed text is
/// scrubbed on EVERY exit, parse errors are sanitized (never
/// interpolated: `toml::de::Error` embeds the offending source line,
/// i.e. the seed), and a hand-edited `entity_id_hex` that disagrees
/// with the seed refuses.
async fn load_subnet_key(
    path: &Path,
    insecure_permissions: bool,
) -> Result<EntityKeypair, CliError> {
    if !insecure_permissions {
        check_strict_permissions(path).await?;
    }
    let mut text = tokio::fs::read_to_string(path).await.map_err(|e| {
        generic(format!(
            "failed to read subnet key file {}: {e}",
            path.display()
        ))
    })?;
    let outcome = load_subnet_key_from_text(&text, path);
    zeroize_string(&mut text);
    outcome
}

fn load_subnet_key_from_text(text: &str, path: &Path) -> Result<EntityKeypair, CliError> {
    let parsed: SubnetKeyFile = toml::from_str(text).map_err(|_| {
        invalid_args(format!(
            "subnet key file {} is not valid TOML (kind: parse_error)",
            path.display()
        ))
    })?;
    if parsed.kind != SUBNET_KEY_KIND {
        return Err(invalid_args(format!(
            "{} is not a subnet authority key file (kind: wrong_kind)",
            path.display()
        )));
    }
    let seed_bytes = ScrubbedBytes::new(hex::decode(parsed.seed_hex.as_bytes()).map_err(|_| {
        invalid_args(format!(
            "subnet key file {} seed_hex is not valid hex (kind: bad_seed_encoding)",
            path.display()
        ))
    })?);
    if seed_bytes.as_slice().len() != 32 {
        return Err(invalid_args(format!(
            "subnet key file {} seed must be 32 bytes (64 hex chars), got {} (kind: bad_seed_length)",
            path.display(),
            seed_bytes.as_slice().len()
        )));
    }
    let mut seed = [0u8; 32];
    seed.copy_from_slice(seed_bytes.as_slice());
    let keypair = EntityKeypair::from_bytes(seed);
    zeroize_slice(&mut seed);
    let derived = hex::encode(keypair.entity_id().as_bytes());
    if !parsed.entity_id_hex.eq_ignore_ascii_case(&derived) {
        return Err(invalid_args(format!(
            "subnet key file {}: entity_id_hex does not match the key derived from seed_hex",
            path.display()
        )));
    }
    Ok(keypair)
}

/// Publish framed CANONICAL WIRE BYTES at `out` through the race-free
/// stage-beside pipeline: never truncates in place, never follows a
/// leaf symlink, refuses aliasing any input path, and with `--force`
/// replaces atomically after refusing to replace seed-bearing files.
async fn publish_wire_artifact(
    bytes: &[u8],
    out: &Path,
    force: bool,
    inputs: &[(&str, &Path)],
) -> Result<(), CliError> {
    let mut paths: Vec<(&str, &Path)> = inputs.to_vec();
    paths.push(("--out", out));
    refuse_aliased_paths(&paths)?;
    refuse_existing(out, force).await?;
    if force {
        refuse_replacing_foreign_seed(out, SeedArtifact::None).await?;
    }
    let tmp = stage_beside(out, bytes, false).await?;
    if force {
        publish_staged_replace(&tmp, out).await
    } else {
        publish_staged(&tmp, out).await
    }
}

/// Parse a dotted subnet path (`3.9.1`) or `global` into the compact
/// hierarchy id — the inverse of [`format_subnet`], through the core's
/// strict constructor.
fn parse_subnet_path(raw: &str) -> Result<TopologySubnetId, CliError> {
    if raw.eq_ignore_ascii_case("global") {
        return Ok(TopologySubnetId::GLOBAL);
    }
    let mut levels = Vec::new();
    for part in raw.split('.') {
        let level: u8 = part.parse().map_err(|_| {
            invalid_args(format!(
                "subnet path `{raw}`: `{part}` is not a level in 0..=255; expected a dotted \
                 path like `3.9` or `global`"
            ))
        })?;
        levels.push(level);
    }
    TopologySubnetId::try_new(&levels)
        .map_err(|_| invalid_args(format!("subnet path `{raw}`: more than four levels")))
}

/// Parse comma-separated rights names into the strict core mask.
fn parse_subnet_rights(raw: &str) -> Result<SubnetRights, CliError> {
    let mut bits: u8 = 0;
    for part in raw.split(',') {
        let part = part.trim();
        bits |= match part.to_ascii_lowercase().as_str() {
            "attach" => SubnetRights::ATTACH.bits(),
            "route" => SubnetRights::ROUTE.bits(),
            "export" => SubnetRights::EXPORT.bits(),
            other => {
                return Err(invalid_args(format!(
                    "unknown right `{other}`; expected a comma-separated subset of \
                     attach, route, export"
                )))
            }
        };
    }
    SubnetRights::try_from_bits(bits)
        .map_err(|e| invalid_args(format!("rights `{raw}`: subnet:{e}")))
}

/// Render a rights mask as the canonical comma-separated names.
fn format_subnet_rights(rights: SubnetRights) -> String {
    let mut parts = Vec::new();
    if rights.contains(SubnetRights::ATTACH) {
        parts.push("attach");
    }
    if rights.contains(SubnetRights::ROUTE) {
        parts.push("route");
    }
    if rights.contains(SubnetRights::EXPORT) {
        parts.push("export");
    }
    parts.join(",")
}

fn parse_u64_arg(flag: &str, raw: &str) -> Result<u64, CliError> {
    let parsed = if let Some(hex) = raw.strip_prefix("0x") {
        u64::from_str_radix(hex, 16)
    } else {
        raw.parse()
    };
    parsed.map_err(|_| invalid_args(format!("{flag} `{raw}` is not a u64")))
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
