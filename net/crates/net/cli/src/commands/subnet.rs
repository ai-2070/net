//! `net-mesh subnet` — two deliberately distinct groups under one verb.
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
    /// Remove one subject's named rights inside one scope: sign a subject
    /// floor, hand it to each named verifier, and report — per verifier,
    /// from its own signed attestation — whether it applied and persisted
    /// it. Unnamed verifiers are never assumed; `complete` is true only
    /// when every named verifier attested a persisted floor.
    Remove(RemoveArgs),
    /// Create a standalone subnet link: the subnet relation only, for a
    /// device already on this mesh. It is redeemed over the device's own
    /// session with this node (no PSK is delivered). Needs `up --enroll`
    /// started with a subnet issuer whose grant covers the scope.
    Invite(SubnetInviteArgs),
    /// Join a subnet with a standalone link, through this device's running
    /// `up`: it redeems the link over its session with the node it enrolled
    /// with, keeps the credentials, and presents them (the verifier's
    /// verdict is reported). The node renews and re-presents them itself.
    Join(SubnetJoinArgs),
    /// What the node of `--state-dir` issued for this scope (and inside it)
    /// and which peers are admitted to it at that node right now — explicitly
    /// not a claim about other verifiers.
    Members(SubnetMembersArgs),
    /// Leave one subnet relation of this device (the join's own, or a
    /// standalone membership) through its running `up`: recorded durably,
    /// never presented or renewed again, and the verifier asked over the
    /// session to drop this device's admission there (acknowledged or
    /// reported unconfirmed). The credential is not revoked.
    Leave(SubnetLeaveArgs),
    /// Make one stored subnet relation of this device the ACTIVE attachment
    /// at its verifier (one is active per verifier; only it is presented).
    /// The previous one is withdrawn there and stays stored.
    Activate(SubnetLeaveArgs),
}

/// `subnet leave` arguments.
#[derive(Args, Debug)]
pub struct SubnetLeaveArgs {
    /// The scope (dotted path) of the relation to leave.
    pub scope: String,
    /// State directory of this device's running `net-mesh up`.
    #[arg(long, value_name = "DIR")]
    pub state_dir: Option<PathBuf>,
}

/// `subnet members` arguments.
#[derive(Args, Debug)]
pub struct SubnetMembersArgs {
    /// The subnet scope (dotted path); its subtree is included.
    pub scope: String,
    /// State directory of the node to ask (as given to `net-mesh up`).
    #[arg(long, value_name = "DIR")]
    pub state_dir: Option<PathBuf>,
    /// Also ask these nodes for their signed observations: `self` or
    /// `ENTITY_HEX@HOST:PORT#NOISE_PUBKEY_HEX`. Needs `--root-key` and
    /// `--authority`: only the subnet authority may read its inventory.
    #[arg(long = "verifier", value_name = "NODE", requires_all = ["root_key", "authority"])]
    pub verifiers: Vec<String>,
    /// The subnet authority root key that signs the requests (stays here).
    #[arg(long, value_name = "PATH")]
    pub root_key: Option<PathBuf>,
    /// The subnet authority (64-hex entity id).
    #[arg(long, value_name = "HEX")]
    pub authority: Option<String>,
    /// How long to wait for each node's answer.
    #[arg(long, value_name = "DURATION", default_value = "10s", value_parser = crate::humantime::parse_duration)]
    pub wait: std::time::Duration,
    /// Accept a group/world-readable root key file (Unix).
    #[arg(long)]
    pub insecure_permissions: bool,
}

/// `subnet invite` arguments.
#[derive(Args, Debug)]
pub struct SubnetInviteArgs {
    /// The subnet scope (dotted path) inside this node's issuer grant.
    pub scope: String,
    /// Rights to offer (default `attach`; others only when named).
    #[arg(long, value_name = "RIGHTS")]
    pub rights: Option<String>,
    /// Node state directory (as given to `net-mesh up`).
    #[arg(long, value_name = "DIR")]
    pub state_dir: Option<PathBuf>,
    /// Lifetime of the unredeemed link (default 24h).
    #[arg(long, value_name = "DURATION", value_parser = crate::humantime::parse_duration)]
    pub ttl: Option<std::time::Duration>,
    /// Require an operator decision (`invite approve`) before issuing.
    #[arg(long)]
    pub require_approval: bool,
    /// Bind the link to one device's full 64-hex entity id.
    #[arg(long = "for", value_name = "ENTITY")]
    pub for_subject: Option<String>,
    /// Write the link to this new owner-only file instead of stdout.
    #[arg(long, value_name = "PATH")]
    pub out: Option<PathBuf>,
}

/// `subnet join` arguments.
#[derive(Args, Debug)]
pub struct SubnetJoinArgs {
    /// The standalone subnet link, or `-` to read it from stdin (then
    /// `--yes` is required).
    pub token: String,
    /// State directory of this device's running `net-mesh up`.
    #[arg(long, value_name = "DIR")]
    pub state_dir: Option<PathBuf>,
    /// Skip the interactive confirmation (scripts and agent tool use).
    #[arg(long)]
    pub yes: bool,
    /// Make this subnet the ACTIVE attachment at the verifier even though
    /// another is active there (that one stays stored). Without it, joining
    /// a second scope at the same verifier is refused.
    #[arg(long)]
    pub switch: bool,
}

/// `net-mesh subnet remove` arguments.
#[derive(Args, Debug)]
pub struct RemoveArgs {
    /// Path to the AUTHORITY ROOT key file.
    #[arg(long = "root-key", value_name = "PATH")]
    pub root_key: PathBuf,
    /// The authority id (64 hex chars).
    #[arg(long)]
    pub authority: String,
    /// The scope the subject is removed from: dotted path or `global`.
    #[arg(long)]
    pub scope: String,
    /// Topology epoch (explicit).
    #[arg(long = "topology-epoch")]
    pub topology_epoch: u32,
    /// Subject-floor revision for `(scope, subject)`; must exceed any
    /// earlier one for the verifiers to apply it.
    #[arg(long)]
    pub revision: u64,
    /// The removed subject's full entity id (64 hex chars).
    #[arg(long)]
    pub subject: String,
    /// Rights removed. Defaults to `attach` only.
    #[arg(long, default_value = "attach")]
    pub rights: String,
    /// Only a root-direct grant at or above this generation re-admits.
    #[arg(long = "minimum-generation")]
    pub minimum_generation: u32,
    /// An enforcement point: `ENTITY_HEX@HOST:PORT#NOISE_PUBKEY_HEX`.
    /// Repeat for each verifier; only these are checked.
    #[arg(long = "verifier", value_name = "CONTACT", required = true)]
    /// With `--state-dir`, `self` names the operator's own node.
    pub verifiers: Vec<String>,
    /// Mesh PSK (64 hex chars); defaults to the profile `psk_hex`.
    /// Not needed with `--state-dir`.
    #[arg(long = "psk-hex", value_name = "HEX", conflicts_with = "state_dir")]
    pub psk_hex: Option<String>,
    /// Reach the verifiers through this operator's running `up` node (its
    /// state directory) instead of attaching with the PSK. The requests are
    /// still signed here and the attestations verified here; the node only
    /// carries them. `--verifier self` names that node.
    #[arg(long, value_name = "DIR")]
    pub state_dir: Option<PathBuf>,
    /// Query each verifier's current state without applying anything.
    #[arg(long)]
    pub dry_run: bool,
    /// Per-verifier bound on connecting and answering.
    #[arg(long, value_name = "DURATION", default_value = "10s", value_parser = crate::humantime::parse_duration)]
    pub wait: std::time::Duration,
    /// Allow permissive root-key file modes on Unix.
    #[arg(long)]
    pub insecure_permissions: bool,
}

#[derive(Args, Debug)]
pub struct ShowArgs {
    #[command(flatten)]
    pub scope: super::scope::InspectableLocalScope,

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

#[derive(Args, Debug)]
pub struct TreeArgs {
    #[command(flatten)]
    pub scope: super::scope::InspectableLocalScope,

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
    if inspect_offline_target(&cmd, output, config_path, profile_name).await? {
        return Ok(());
    }
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
        SubnetCommand::Remove(args) => {
            let profile = resolve_profile(config_path, profile_name).await?;
            run_remove(args, profile.psk_hex, profile_name, output).await
        }
        SubnetCommand::Invite(args) => {
            parse_subnet_path(&args.scope)?;
            super::enrollment::run_invite(
                super::enrollment::InviteCommand::Create(super::enrollment::CreateArgs {
                    state_dir: args.state_dir,
                    ttl: args.ttl,
                    require_approval: args.require_approval,
                    for_subject: args.for_subject,
                    out: args.out,
                    addr: None,
                    subnet: Some(args.scope),
                    subnet_rights: args.rights,
                    org: None,
                    channel: None,
                    channel_rights: None,
                    standalone: true,
                }),
                output,
                profile_name,
            )
            .await
        }
        SubnetCommand::Join(args) => run_subnet_join(args, output, profile_name).await,
        SubnetCommand::Activate(args) => {
            parse_subnet_path(&args.scope)?;
            let node_dir = super::lifecycle::state_dir(args.state_dir, profile_name)?
                .join(super::lifecycle::NODE_SUBDIR);
            let (_, reply) = super::lifecycle::control_call_within(
                &node_dir,
                serde_json::json!({ "op": "subnet_activate", "scope": args.scope }),
                std::time::Duration::from_secs(25),
            )
            .await
            .map_err(|e| {
                crate::error::connection_failure(format!(
                    "{e}; is this device's `net-mesh up` running?"
                ))
            })?;
            if let Some(err) = reply["error"].as_str() {
                return Err(generic(err.to_string()));
            }
            emit_value(OutputFormat::resolve_oneshot(output), &reply)
                .map_err(|e| generic(format!("write result: {e}")))
        }
        SubnetCommand::Leave(args) => {
            parse_subnet_path(&args.scope)?;
            let node_dir = super::lifecycle::state_dir(args.state_dir, profile_name)?
                .join(super::lifecycle::NODE_SUBDIR);
            let (_, reply) = super::lifecycle::control_call(
                &node_dir,
                serde_json::json!({ "op": "subnet_leave", "scope": args.scope }),
            )
            .await
            .map_err(|e| {
                crate::error::connection_failure(format!(
                    "{e}; is this device's `net-mesh up` running?"
                ))
            })?;
            if let Some(err) = reply["error"].as_str() {
                return Err(generic(err.to_string()));
            }
            emit_value(OutputFormat::resolve_oneshot(output), &reply)
                .map_err(|e| generic(format!("write result: {e}")))
        }
        SubnetCommand::Members(args) => {
            parse_subnet_path(&args.scope)?;
            let remote = match (&args.root_key, args.verifiers.is_empty()) {
                (Some(key), false) => Some(super::lifecycle::RemoteMembers {
                    root: load_subnet_key(key, args.insecure_permissions).await?,
                    authority: Some(parse_entity_hex(
                        args.authority.as_deref().unwrap_or_default(),
                    )?),
                    verifiers: args.verifiers.clone(),
                    wait: args.wait,
                }),
                _ => None,
            };
            super::lifecycle::run_members(
                "subnet",
                args.scope,
                args.state_dir,
                remote,
                output,
                profile_name,
            )
            .await
        }
    }
}

/// Bound on the whole `subnet join` exchange with the running node (its
/// redemption and presentation are bounded inside it).
const SUBNET_JOIN_CONTROL_WAIT: std::time::Duration = std::time::Duration::from_secs(28);

/// `subnet join`: show what is being joined, confirm, and hand the link to
/// the running node over its authenticated control endpoint.
async fn run_subnet_join(
    args: SubnetJoinArgs,
    output: Option<OutputFormat>,
    profile_name: &str,
) -> Result<(), CliError> {
    use net_sdk::enrollment::standalone::is_standalone_subnet;
    let from_stdin = args.token == "-";
    if from_stdin && !args.yes {
        return Err(invalid_args(
            "reading the link from stdin needs --yes (stdin cannot also answer the prompt)",
        ));
    }
    let token = if from_stdin {
        use tokio::io::AsyncReadExt as _;
        let mut buf = String::new();
        tokio::io::stdin()
            .take(4096)
            .read_to_string(&mut buf)
            .await
            .map_err(|e| generic(format!("read link from stdin: {e}")))?;
        crate::secret::ScrubbedString::new(buf.trim().to_string())
    } else {
        crate::secret::ScrubbedString::new(args.token)
    };
    let invite = net_sdk::enrollment::invite::MembershipInvite::decode(token.as_str())
        .map_err(|e| invalid_args(format!("not a valid link: {e}")))?;
    let offer = match invite.subnet() {
        Some(offer) if is_standalone_subnet(&invite) => offer.clone(),
        _ => {
            return Err(invalid_args(
                "not a standalone subnet link (a mesh invite is redeemed with `net-mesh join`)",
            ))
        }
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if now >= invite.policy().expires_at() {
        return Err(invalid_args("this link has expired; ask for a new one"));
    }
    eprintln!(
        "Joining subnet {} ({}) of authority {} under issuer {}\n  link: {}, expires at unix {}",
        format_subnet(offer.scope.path),
        format_subnet_rights(offer.rights),
        hex::encode(offer.scope.authority.as_bytes()),
        invite.issuer_fingerprint(),
        if invite.is_bearer() {
            "bearer (the first redeemer joins)"
        } else {
            "bound to one device"
        },
        invite.policy().expires_at(),
    );
    let tty = {
        use std::io::IsTerminal as _;
        std::io::stdin().is_terminal() && !from_stdin
    };
    let yes = args.yes;
    tokio::task::spawn_blocking(move || {
        super::ice::check_confirm_gate(tty, yes, || {
            use std::io::{BufRead as _, Write as _};
            let mut err = std::io::stderr();
            write!(
                err,
                "Confirm the issuer fingerprint is the one you expect. Type YES to join: "
            )
            .and_then(|()| err.flush())
            .map_err(|e| generic(format!("prompt: {e}")))?;
            let mut line = String::new();
            std::io::stdin()
                .lock()
                .read_line(&mut line)
                .map_err(|e| generic(format!("prompt: {e}")))?;
            Ok(line.trim() == "YES")
        })
    })
    .await
    .map_err(|e| generic(format!("confirmation task failed: {e}")))??;

    let node_dir = super::lifecycle::state_dir(args.state_dir, profile_name)?
        .join(super::lifecycle::NODE_SUBDIR);
    let (_, reply) = super::lifecycle::control_call_within(
        &node_dir,
        serde_json::json!({ "op": "subnet_join", "token": token.as_str(), "switch": args.switch }),
        SUBNET_JOIN_CONTROL_WAIT,
    )
    .await
    .map_err(|e| {
        crate::error::connection_failure(format!(
            "this device's node is not reachable ({e:?}); `subnet join` runs through a running \
             `net-mesh up`"
        ))
    })?;
    if let Some(e) = reply["error"].as_str() {
        return Err(generic(e.to_string()));
    }
    emit_value(OutputFormat::resolve_oneshot(output), &reply)
        .map_err(|e| generic(format!("write result: {e}")))
}

/// One named enforcement point.
pub(crate) struct VerifierContact {
    pub(crate) entity: net::adapter::net::identity::EntityId,
    pub(crate) addr: std::net::SocketAddr,
    pub(crate) noise_pubkey: [u8; 32],
}

pub(crate) fn parse_verifier(raw: &str) -> Result<VerifierContact, CliError> {
    let bad = || {
        invalid_args(format!(
            "--verifier `{raw}`: expected ENTITY_HEX@HOST:PORT#NOISE_PUBKEY_HEX"
        ))
    };
    let (entity, rest) = raw.split_once('@').ok_or_else(bad)?;
    let (addr, pubkey) = rest.split_once('#').ok_or_else(bad)?;
    let entity = parse_entity_hex(entity)?;
    let addr = addr.parse().map_err(|_| bad())?;
    let noise_pubkey = crate::parsers::hex_decode_32(pubkey).map_err(|_| bad())?;
    Ok(VerifierContact {
        entity,
        addr,
        noise_pubkey,
    })
}

/// Ask one verifier, over its own attached session, and classify the
/// answer from its signed attestation only.
async fn ask_verifier(
    contact: &VerifierContact,
    request: &net::adapter::net::subnet::floor_status::FloorStatusRequest,
    psk: [u8; 32],
    rights: SubnetRights,
    minimum_generation: u32,
    wait: std::time::Duration,
) -> serde_json::Value {
    use net::adapter::net::SubnetFloorQueryError;
    let entity = hex::encode(contact.entity.as_bytes());
    let remote = crate::context::RemoteAttach {
        bind: if contact.addr.ip().is_loopback() {
            std::net::SocketAddr::new(contact.addr.ip(), 0)
        } else if contact.addr.is_ipv4() {
            std::net::SocketAddr::from(([0, 0, 0, 0], 0))
        } else {
            std::net::SocketAddr::from(([0u16; 8], 0))
        },
        addr: contact.addr,
        public_key: contact.noise_pubkey,
        node_id: contact.entity.node_id(),
        psk,
    };
    let outcome = tokio::time::timeout(wait, async {
        let mesh = crate::context::build_attached_mesh(None, &remote)
            .await
            .map_err(|e| SubnetFloorQueryError::NoAnswer(e.to_string()))?;
        let result = mesh
            .node()
            .query_subnet_floor_status(contact.entity.node_id(), request, wait)
            .await;
        let _ = mesh.shutdown().await;
        result
    })
    .await
    .unwrap_or_else(|_| Err(SubnetFloorQueryError::NoAnswer("timed out".into())));
    classify(&entity, outcome, rights, minimum_generation)
}

/// Ask one verifier through the operator's own `up` node. The node carries
/// the already-signed request; the attestation is verified HERE against
/// this exact request, so the node cannot vouch for anything.
async fn ask_verifier_via_node(
    node_dir: &Path,
    contact: Option<&VerifierContact>,
    request: &net::adapter::net::subnet::floor_status::FloorStatusRequest,
    rights: SubnetRights,
    minimum_generation: u32,
    wait: std::time::Duration,
) -> serde_json::Value {
    use net::adapter::net::subnet::floor_status::FloorStatusAttestation;
    use net::adapter::net::SubnetFloorQueryError;
    let entity = hex::encode(request.verifier.as_bytes());
    let mut call = serde_json::json!({
        "op": "subnet_floor_query",
        "verifier": entity,
        "request": hex::encode(request.to_bytes()),
        "wait_ms": wait.as_millis() as u64,
    });
    if let Some(c) = contact {
        call["addr"] = serde_json::json!(c.addr.to_string());
        call["noise_pubkey"] = serde_json::json!(hex::encode(c.noise_pubkey));
    }
    let outcome = match super::lifecycle::control_call_within(
        node_dir,
        call,
        wait + std::time::Duration::from_secs(5),
    )
    .await
    {
        Err(e) => Err(SubnetFloorQueryError::NoAnswer(format!(
            "operator node: {e}"
        ))),
        Ok((_, reply)) => {
            if let Some(hex_a) = reply["attestation"].as_str() {
                hex::decode(hex_a)
                    .ok()
                    .and_then(|b| FloorStatusAttestation::from_bytes(&b).ok())
                    .ok_or_else(|| SubnetFloorQueryError::BadAttestation("undecodable".into()))
                    .and_then(|a| {
                        a.verify_for(request)
                            .map(|()| a)
                            .map_err(|e| SubnetFloorQueryError::BadAttestation(e.to_string()))
                    })
            } else if let Some(m) = reply["refused"].as_str() {
                Err(SubnetFloorQueryError::Refused(m.to_string()))
            } else {
                Err(SubnetFloorQueryError::NoAnswer(
                    reply["no_answer"]
                        .as_str()
                        .or_else(|| reply["error"].as_str())
                        .unwrap_or("no answer")
                        .to_string(),
                ))
            }
        }
    };
    classify(&entity, outcome, rights, minimum_generation)
}

fn classify(
    entity: &str,
    outcome: Result<
        net::adapter::net::subnet::floor_status::FloorStatusAttestation,
        net::adapter::net::SubnetFloorQueryError,
    >,
    rights: SubnetRights,
    minimum_generation: u32,
) -> serde_json::Value {
    use net::adapter::net::subnet::floor_status::FloorApplyOutcome;
    use net::adapter::net::SubnetFloorQueryError;
    match outcome {
        Ok(a) => {
            let covers = a.covers(rights, minimum_generation);
            let state = match (a.apply, covers, a.persisted) {
                (FloorApplyOutcome::Refused(_), _, _) => "refused",
                (_, true, true) => "applied",
                (_, true, false) => "applied_not_persisted",
                (_, false, _) => "not_applied",
            };
            serde_json::json!({
                "verifier": entity,
                "state": state,
                "apply": a.apply.as_str(),
                "reason": match a.apply {
                    FloorApplyOutcome::Refused(e) => Some(format!("subnet:{e}")),
                    _ => None,
                },
                "revision": a.revision,
                "generations": { "attach": a.generations[0], "route": a.generations[1], "export": a.generations[2] },
                "persisted": a.persisted,
                "attested": true,
            })
        }
        Err(e) => serde_json::json!({
            "verifier": entity,
            "state": match e {
                SubnetFloorQueryError::Refused(_) => "request_refused",
                _ => "no_attestation",
            },
            "reason": e.to_string(),
            "attested": false,
        }),
    }
}

async fn run_remove(
    args: RemoveArgs,
    profile_psk: Option<String>,
    profile_name: &str,
    output: Option<OutputFormat>,
) -> Result<(), CliError> {
    use net::adapter::net::subnet::floor_status::FloorStatusRequest;
    let subject = parse_entity_hex(&args.subject)?;
    let rights = parse_subnet_rights(&args.rights)?;
    if args.minimum_generation == 0 {
        return Err(invalid_args("--minimum-generation 0 removes nothing"));
    }
    let node_dir = match &args.state_dir {
        Some(dir) => Some(
            super::lifecycle::state_dir(Some(dir.clone()), profile_name)?
                .join(super::lifecycle::NODE_SUBDIR),
        ),
        None => None,
    };
    // `self` (with --state-dir) is the operator's own node: its entity from
    // its authenticated status, answered locally.
    let self_entity = if node_dir.is_some() && args.verifiers.iter().any(|v| v == "self") {
        let (_, reply) = super::lifecycle::control_call(
            node_dir.as_deref().unwrap_or(Path::new(".")),
            serde_json::json!({ "op": "status" }),
        )
        .await
        .map_err(|e| crate::error::connection_failure(format!("operator node: {e}")))?;
        Some(parse_entity_hex(
            reply["node"]["entity_id"].as_str().unwrap_or_default(),
        )?)
    } else {
        None
    };
    // Each verifier: a full contact, or `self` (via the node only).
    let mut targets: Vec<(
        net::adapter::net::identity::EntityId,
        Option<VerifierContact>,
    )> = Vec::new();
    for v in &args.verifiers {
        if v == "self" {
            let entity = self_entity
                .clone()
                .ok_or_else(|| invalid_args("--verifier self needs --state-dir"))?;
            targets.push((entity, None));
        } else {
            let contact = parse_verifier(v)?;
            targets.push((contact.entity.clone(), Some(contact)));
        }
    }
    let psk = match &node_dir {
        Some(_) => None,
        None => {
            let psk_raw = args.psk_hex.clone().or(profile_psk).ok_or_else(|| {
                invalid_args(
                    "the mesh PSK is required to reach verifiers: pass --psk-hex, set the \
                     profile psk_hex, or go through the operator's node with --state-dir",
                )
            })?;
            Some(crate::context::parse_psk_hex(&psk_raw)?)
        }
    };
    let root = load_subnet_key(&args.root_key, args.insecure_permissions).await?;
    let scope = SubnetRef {
        authority: parse_entity_hex(&args.authority)?,
        path: parse_subnet_path(&args.scope)?,
    };
    let floor = SubnetSubjectFloor::try_issue(
        &root,
        scope.clone(),
        args.topology_epoch,
        subject.clone(),
        rights,
        args.minimum_generation,
        args.revision,
        unix_now(),
    )
    .map_err(|e| invalid_args(format!("subject-floor: subnet:{e}")))?;

    let mut rows = Vec::with_capacity(targets.len());
    for (verifier, contact) in &targets {
        let mut nonce = [0u8; 16];
        getrandom::fill(&mut nonce).map_err(|_| generic("operating-system CSPRNG unavailable"))?;
        let request = FloorStatusRequest::try_issue(
            &root,
            scope.clone(),
            args.topology_epoch,
            subject.clone(),
            verifier.clone(),
            nonce,
            unix_now(),
            (!args.dry_run).then_some(&floor),
        )
        .map_err(|e| generic(format!("readback request: subnet:{e}")))?;
        let row = match (&node_dir, contact, psk) {
            (Some(dir), contact, _) => {
                ask_verifier_via_node(
                    dir,
                    contact.as_ref(),
                    &request,
                    rights,
                    args.minimum_generation,
                    args.wait,
                )
                .await
            }
            (None, Some(contact), Some(psk)) => {
                ask_verifier(
                    contact,
                    &request,
                    psk,
                    rights,
                    args.minimum_generation,
                    args.wait,
                )
                .await
            }
            _ => return Err(invalid_args("--verifier self needs --state-dir")),
        };
        rows.push(row);
    }
    let applied = rows.iter().filter(|r| r["state"] == "applied").count();
    let complete = !args.dry_run && applied == rows.len();
    emit_value(
        OutputFormat::resolve_oneshot(output),
        &serde_json::json!({
            "action": if args.dry_run { "dry_run" } else { "remove" },
            "subject_hex": hex::encode(subject.as_bytes()),
            "authority_hex": hex::encode(scope.authority.as_bytes()),
            "scope": format_subnet(scope.path),
            "rights": format_subnet_rights(rights),
            "minimum_generation": args.minimum_generation,
            "revision": args.revision,
            "verifiers": rows,
            "applied": applied,
            "pending": targets.len() - applied,
            // True only when EVERY named verifier attested a persisted floor.
            "complete": complete,
            "coverage": "only the named verifiers were checked; any other enforcement point is unknown",
        }),
    )
    .map_err(|e| generic(format!("write result: {e}")))
}

/// Resolve only offline artifact selections; never decode grants or sign facts.
async fn inspect_offline_target(
    cmd: &SubnetCommand,
    output: Option<OutputFormat>,
    config_path: Option<&Path>,
    profile_name: &str,
) -> Result<bool, CliError> {
    let selection = match cmd {
        SubnetCommand::Keygen(a) if a.inspect_target => {
            let profile = resolve_profile(config_path, profile_name).await?;
            let mut view = crate::target::TargetInspection::local(&profile, "offline");
            view.unavailable_identity(
                "execution generates a new subnet key; its public identity is not yet available",
            );
            view.provenance("identity", "runtime");
            if let Some(path) = &a.out {
                view.destination = Some(path.clone());
                view.provenance("destination", "flag");
            } else {
                view.destination_pattern = Some(
                    default_subnet_key_dir()
                        .ok_or_else(|| {
                            invalid_args("cannot resolve the platform config directory; pass --out")
                        })?
                        .join("subnet-<generated-entity-id-prefix>.toml"),
                );
                view.provenance("destination", "runtime");
            }
            view.emit(output)?;
            return Ok(true);
        }
        SubnetCommand::Inspect(a) if a.inspect_target => {
            let profile = resolve_profile(config_path, profile_name).await?;
            let mut view = crate::target::TargetInspection::local(&profile, "offline");
            view.source = Some(a.file.clone());
            view.provenance("source", "argument");
            view.emit(output)?;
            return Ok(true);
        }
        SubnetCommand::IssueDirect(a) if a.inspect_target => {
            Some((&a.root_key, a.insecure_permissions, &a.out, None))
        }
        SubnetCommand::IssueIssuer(a) if a.inspect_target => {
            Some((&a.root_key, a.insecure_permissions, &a.out, None))
        }
        SubnetCommand::IssueDelegated(a) if a.inspect_target => Some((
            &a.issuer_key,
            a.insecure_permissions,
            &a.out,
            Some(&a.issuer_grant),
        )),
        SubnetCommand::IssueControlFact(a) => {
            let common = match &a.kind {
                ControlFactKindCommand::Descriptor(a) => &a.common,
                ControlFactKindCommand::GatewayAdvertisement(a) => &a.common,
                ControlFactKindCommand::ExportPolicy(a) => &a.common,
                ControlFactKindCommand::RevocationFloor(a) => &a.common,
                ControlFactKindCommand::SubjectFloor(a) => &a.common,
            };
            if common.inspect_target {
                Some((
                    &common.root_key,
                    common.insecure_permissions,
                    &common.out,
                    None,
                ))
            } else {
                None
            }
        }
        _ => None,
    };
    let Some((key, insecure, destination, issuer_grant_source)) = selection else {
        return Ok(false);
    };
    let signer = load_subnet_key(key, insecure).await?;
    let profile = resolve_profile(config_path, profile_name).await?;
    let mut target = crate::target::TargetInspection::local(&profile, "offline");
    target.configured_identity(signer.entity_id().as_bytes());
    target.source = Some(key.clone());
    target.destination = Some(destination.clone());
    for field in ["identity", "source", "destination"] {
        target.provenance(field, "flag");
    }
    if issuer_grant_source.is_some() {
        target.provenance("issuer_grant_source", "flag");
    }
    emit_value(
        OutputFormat::resolve_oneshot(output),
        &SubnetInspection {
            target,
            issuer_grant_source,
        },
    )
    .map_err(|e| generic(format!("write inspection: {e}")))?;
    Ok(true)
}

#[derive(Serialize)]
struct SubnetInspection<'a> {
    #[serde(flatten)]
    target: crate::target::TargetInspection,
    #[serde(skip_serializing_if = "Option::is_none")]
    issuer_grant_source: Option<&'a PathBuf>,
}

async fn run_show(
    args: ShowArgs,
    output: Option<OutputFormat>,
    config_path: Option<&std::path::Path>,
    profile_name: &str,
) -> Result<(), CliError> {
    super::scope::validate_local(args.scope.local, "subnet show")?;
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
    super::scope::require_local(args.scope.local, "subnet show")?;
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
    super::scope::validate_local(args.scope.local, "subnet ls")?;
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
    super::scope::require_local(args.scope.local, "subnet ls")?;
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
    super::scope::validate_local(args.scope.local, "subnet tree")?;
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
    super::scope::require_local(args.scope.local, "subnet tree")?;
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
pub(crate) fn format_subnet(subnet: SubnetId) -> String {
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
    enforce_strict_permissions, now_iso8601, parse_entity_hex, read_secret_key_file,
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
    SubnetRevocationFloor, SubnetRights, SubnetSubjectFloor, TopologySubnetId,
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
    /// Inspect output selection without generating a key.
    #[arg(long)]
    pub inspect_target: bool,
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
    /// Inspect signer and artifact paths without issuing a credential.
    #[arg(long)]
    pub inspect_target: bool,
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
    /// Inspect signer and artifact paths without issuing a credential.
    #[arg(long)]
    pub inspect_target: bool,
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
    /// Inspect signer and paths without reading the issuer grant or signing.
    #[arg(long)]
    pub inspect_target: bool,
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
    /// A root-signed SUBJECT floor: remove one entity's named rights
    /// inside `--scope`, leaving every other subject untouched.
    SubjectFloor(FactSubjectFloorArgs),
}

#[derive(Args, Debug)]
pub struct FactCommonArgs {
    /// Inspect signer and artifact paths without signing a control fact.
    #[arg(long)]
    pub inspect_target: bool,
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
pub struct FactSubjectFloorArgs {
    #[command(flatten)]
    pub common: FactCommonArgs,

    /// The removed subject's full entity id (64 hex chars) — never a
    /// routing id.
    #[arg(long)]
    pub subject: String,

    /// Rights removed inside the scope. Defaults to `attach` only;
    /// `route` / `export` are removed only when named here.
    #[arg(long, default_value = "attach")]
    pub rights: String,

    /// The subject is re-admitted only by a root-direct grant at or
    /// above this generation; its older grants, and any delegated
    /// grant, lose the named rights inside the scope.
    #[arg(long = "minimum-generation")]
    pub minimum_generation: u32,
}

#[derive(Args, Debug)]
pub struct InspectArgs {
    /// Inspect the source path without reading or decoding the artifact.
    #[arg(long)]
    pub inspect_target: bool,
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
    // NESTED IN THE ISSUER'S WINDOW BY CONSTRUCTION, not by luck of
    // the clock.
    //
    // Both the issuer grant and this leaf default their window from
    // their OWN `unix_now()`, in two separate command invocations. A
    // second boundary between them made the leaf's `not_after`
    // exactly one second later than its issuer's, and the verifier
    // refuses that with `IssuerAttenuationBroadened` — a delegation
    // may not outlive the grant that empowered it. The chain was
    // then unusable, having been issued successfully: the CLI
    // reported code 0 and produced a credential no verifier accepts.
    //
    // Clamping is the right fix rather than widening the verifier's
    // tolerance, because the nesting rule is correct; what was wrong
    // was issuing outside it. An EXPLICIT `--not-before` is still
    // honoured as given and validated below, so an operator who asks
    // for a window outside the issuer's is told, not silently moved.
    let explicit_not_before = args.not_before.is_some();
    let not_before = args
        .not_before
        .unwrap_or_else(|| unix_now().saturating_sub(NOT_BEFORE_HEADROOM_SECS))
        .max(issuer_grant.not_before);
    let ttl_secs = if explicit_not_before {
        args.ttl_secs
    } else {
        // Never past the issuer's expiry, and never negative: a
        // grant with no remaining life is a refusal, not a
        // zero-length credential.
        let room = issuer_grant.not_after.saturating_sub(not_before);
        if room == 0 {
            return Err(invalid_args(format!(
                "the issuer grant expires at {} and is not able to empower a leaf from {} \
                 (subnet:issuer_attenuation_broadened)",
                issuer_grant.not_after, not_before,
            )));
        }
        args.ttl_secs.min(room)
    };

    let leaf = SubnetGrant::try_issue(
        &issuer_kp,
        issuer_grant.authority.clone(),
        scope,
        issuer_grant.topology_epoch,
        subject.clone(),
        rights,
        args.generation,
        not_before,
        ttl_secs,
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
        // The window the credential ACTUALLY carries: `ttl_secs` is
        // the clamped value, so a summary can never advertise a
        // lifetime the artifact does not have.
        not_after: not_before.saturating_add(ttl_secs),
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
        ControlFactKindCommand::SubjectFloor(a) => {
            let subject = parse_entity_hex(&a.subject)?;
            let rights = parse_subnet_rights(&a.rights)?;
            if a.minimum_generation == 0 {
                return Err(invalid_args(
                    "--minimum-generation 0 removes nothing; use the generation the subject's                      grants must reach to be re-admitted",
                ));
            }
            let (kp, scope) = fact_prelude(&a.common).await?;
            let fact = SubnetSubjectFloor::try_issue(
                &kp,
                scope,
                a.common.topology_epoch,
                subject,
                rights,
                a.minimum_generation,
                a.common.revision,
                unix_now(),
            )
            .map_err(|e| invalid_args(format!("subject-floor: subnet:{e}")))?;
            (a.common, SubnetControlFact::SubjectFloor(fact))
        }
    };

    publish_wire_artifact(
        &fact.to_bytes(),
        &common.out,
        common.force,
        &[("--root-key", &common.root_key)],
    )
    .await?;

    let subject_floor = match &fact {
        SubnetControlFact::SubjectFloor(f) => Some(f),
        _ => None,
    };
    let summary = IssueFactOutput {
        path: common.out.display().to_string(),
        artifact: "control-fact".to_string(),
        kind: net_sdk::subnet::fact_kind_wire(fact.kind()).to_string(),
        authority_hex: hex::encode(fact.scope().authority.as_bytes()),
        scope: format_subnet(fact.scope().path),
        topology_epoch: common.topology_epoch,
        revision: common.revision,
        subject_hex: subject_floor.map(|f| hex::encode(f.subject.as_bytes())),
        rights: subject_floor.map(|f| format_subnet_rights(f.rights)),
        minimum_generation: subject_floor.map(|f| f.minimum_generation),
        // Issuing signs the artifact; it removes nothing by itself.
        // Enforcement happens at each verifier that applies it, and a
        // verifier that predates this fact kind refuses it rather than
        // enforcing it — so every enforcement point starts pending.
        enforcement: subject_floor.map(|_| {
            "pending: signed only; each verifier enforces it once it applies this fact              (verifiers without subject-floor support refuse it)"
                .to_string()
        }),
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
    #[serde(skip_serializing_if = "Option::is_none")]
    subject_hex: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rights: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    minimum_generation: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    enforcement: Option<String>,
}

fn default_subnet_key_path(entity_id_hex: &str) -> Option<PathBuf> {
    let short = &entity_id_hex[..entity_id_hex.len().min(16)];
    Some(default_subnet_key_dir()?.join(format!("subnet-{short}.toml")))
}

fn default_subnet_key_dir() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("net-mesh").join("subnets"))
}

/// Load + parse a subnet authority key file, honoring the ssh-style
/// permission gate. Mirrors `load_org_key` exactly — the seed text is
/// scrubbed on EVERY exit, parse errors are sanitized (never
/// interpolated: `toml::de::Error` embeds the offending source line,
/// i.e. the seed), and a hand-edited `entity_id_hex` that disagrees
/// with the seed refuses.
/// Load the delegated subnet issuer `up --enroll` signs device leaves with:
/// the root-signed issuer grant (the wire bytes `subnet issue-issuer`
/// writes) and the issuer's key file, which must be the issuer the grant
/// names.
pub(crate) async fn load_subnet_leaf_issuer(
    grant: &Path,
    key: &Path,
    lifetime: std::time::Duration,
    generation: u32,
) -> Result<net_sdk::enrollment::bundle::SubnetLeafIssuer, CliError> {
    let bytes = tokio::fs::read(grant)
        .await
        .map_err(|e| invalid_args(format!("--subnet-issuer-grant {}: {e}", grant.display())))?;
    let grant = SubnetIssuerGrant::from_bytes(&bytes)
        .map_err(|e| invalid_args(format!("--subnet-issuer-grant: subnet:{e}")))?;
    let key = load_subnet_key(key, false).await?;
    net_sdk::enrollment::bundle::SubnetLeafIssuer::new(grant, key, generation, lifetime.as_secs())
        .map_err(|e| invalid_args(format!("subnet issuer: {e}")))
}

async fn load_subnet_key(
    path: &Path,
    insecure_permissions: bool,
) -> Result<EntityKeypair, CliError> {
    let mut text = read_secret_key_file(path, "subnet key file", insecure_permissions).await?;
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
pub(crate) fn parse_subnet_path(raw: &str) -> Result<TopologySubnetId, CliError> {
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
pub(crate) fn parse_subnet_rights(raw: &str) -> Result<SubnetRights, CliError> {
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
pub(crate) fn format_subnet_rights(rights: SubnetRights) -> String {
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
