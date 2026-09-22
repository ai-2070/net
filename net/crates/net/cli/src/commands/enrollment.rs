//! Enrollment on an `up`-owned node: `net-mesh up --enroll`, `net-mesh enrollment
//! init`, and `net-mesh invite create|inspect|status|revoke|approve|deny`.
//!
//! There is no separate enrollment process. `up --enroll` loads the issuer
//! identity, takes the ledger's exclusive lock and — on the same port number as
//! the mesh's UDP socket — serves PSK-free Noise redemption over TCP, delivering
//! membership-only bundles that carry this node's PSK and public contact.
//! `invite` commands are clients of that node's authenticated local control
//! endpoint; they never open the ledger themselves. `invite inspect` is offline.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use clap::{Args, Subcommand};
use net_sdk::bootstrap_credential::{Psk, TrustDomainId};
use net_sdk::enrollment::bundle::{MembershipIssuer, MeshContact};
use net_sdk::enrollment::invite::{
    EnrollmentEndpoint, EnrollmentKey, InviteSpec, MembershipInvite, Relation,
};
use net_sdk::enrollment::policy::{ApprovalMode, InvitationPolicy, DEFAULT_INVITATION_TTL};
use net_sdk::enrollment::service::{EnrollmentService, ServiceConfig, SharedLedger};
use net_sdk::enrollment::store::{
    EnrollmentLedger, LedgerError, LedgerLimits, OfferId, OfferState,
};
use net_sdk::identity::{EntityId, Identity};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::lifecycle::{control_call, now_unix, state_dir, NODE_SUBDIR};
use crate::error::{connection_failure, generic, invalid_args, CliError};
use crate::prelude::{emit_value, OutputFormat};

const LEDGER_SUBDIR: &str = "ledger";

// ---- up --enroll -------------------------------------------------------------

/// Validated `up --enroll` inputs. Built before any filesystem or network effect.
pub(crate) struct EnrollPlan {
    endpoint: EnrollmentEndpoint,
    issuer_path: PathBuf,
    ledger: PathBuf,
    domain_name: String,
    bind: SocketAddr,
}

impl EnrollPlan {
    pub(crate) fn validate(
        public_addr: Option<String>,
        issuer_identity: Option<PathBuf>,
        ledger: Option<PathBuf>,
        domain_name: Option<String>,
        bind: SocketAddr,
        state: &Path,
        profile: &str,
    ) -> Result<Self, CliError> {
        let public_addr = public_addr.ok_or_else(|| {
            invalid_args("--enroll requires --public-addr <host:port>, the address joiners reach")
        })?;
        let endpoint = EnrollmentEndpoint::parse(&public_addr)
            .map_err(|e| invalid_args(format!("--public-addr: {e}")))?;
        if bind.port() == 0 {
            return Err(invalid_args(
                "--enroll requires a fixed --bind port: join tokens and bundles point at it",
            ));
        }
        let issuer_path = issuer_identity.ok_or_else(|| {
            invalid_args(
                "--enroll requires --issuer-identity <PATH> (the key that signs invitations)",
            )
        })?;
        let ledger = ledger.unwrap_or_else(|| state.join(LEDGER_SUBDIR));
        if !ledger.is_dir() {
            return Err(invalid_args(format!(
                "no enrollment ledger at {}; run `net-mesh enrollment init` first",
                ledger.display()
            )));
        }
        let domain_name = domain_name.unwrap_or_else(|| profile.to_string());
        let usable = !domain_name.is_empty()
            && domain_name.len() <= 64
            && domain_name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b));
        if !usable {
            return Err(invalid_args(
                "--domain-name must be 1-64 characters of [A-Za-z0-9._-]",
            ));
        }
        Ok(Self {
            endpoint,
            issuer_path,
            ledger,
            domain_name,
            bind,
        })
    }

    /// Load the issuer and take the ledger's exclusive lock (before any bind).
    pub(crate) async fn open(self) -> Result<EnrollOwner, CliError> {
        let issuer = crate::context::load_operator_identity(&self.issuer_path).await?;
        let (dir, entity) = (self.ledger.clone(), issuer.entity_id().clone());
        let ledger = tokio::task::spawn_blocking(move || {
            EnrollmentLedger::open(&dir, entity, LedgerLimits::default())
        })
        .await
        .map_err(|e| generic(format!("ledger task failed: {e}")))?
        .map_err(|e| ledger_open_error(&self.ledger, e))?;
        Ok(EnrollOwner {
            plan: self,
            issuer,
            ledger: Arc::new(parking_lot::Mutex::new(ledger)),
        })
    }
}

fn ledger_open_error(dir: &Path, e: LedgerError) -> CliError {
    match e {
        LedgerError::IssuerMismatch => invalid_args(format!(
            "ledger {} belongs to a different issuer than --issuer-identity",
            dir.display()
        )),
        LedgerError::Storage(
            net::adapter::net::behavior::enrollment_storage::StorageError::Busy,
        ) => generic(format!(
            "ledger {} is already owned by another process",
            dir.display()
        )),
        other => generic(format!("ledger {}: {other}", dir.display())),
    }
}

/// Issuer and locked ledger, not yet serving.
pub(crate) struct EnrollOwner {
    plan: EnrollPlan,
    issuer: Identity,
    ledger: SharedLedger,
}

impl EnrollOwner {
    /// Bind the enrollment listener on the mesh's port number (TCP) and start
    /// delivering this node's PSK and public contact.
    pub(crate) async fn start(
        self,
        mesh: &net_sdk::Mesh,
        psk: Psk,
    ) -> Result<RunningEnrollment, CliError> {
        let contact_addr = tokio::net::lookup_host(self.plan.endpoint.as_str())
            .await
            .ok()
            .and_then(|mut addrs| addrs.next())
            .ok_or_else(|| {
                invalid_args(format!(
                    "--public-addr {} does not resolve",
                    self.plan.endpoint.as_str()
                ))
            })?;
        let contact = MeshContact {
            addr: contact_addr,
            noise_pubkey: *mesh.public_key(),
            node_id: mesh.node_id(),
        };
        let trust_domain = psk.trust_domain();
        let issuer_bundles = MembershipIssuer::new(self.issuer.clone(), psk, contact);
        let service = EnrollmentService::bind(
            self.plan.bind,
            &self.issuer,
            self.ledger.clone(),
            Arc::new(issuer_bundles),
            ServiceConfig::default(),
        )
        .await
        .map_err(|e| {
            connection_failure(format!(
                "enrollment listener on TCP {}: {e}",
                self.plan.bind
            ))
        })?;
        let context = Arc::new(EnrollContext {
            issuer: self.issuer,
            ledger: self.ledger,
            key: service.enrollment_key(),
            endpoint: self.plan.endpoint,
            trust_domain,
            domain_name: self.plan.domain_name,
        });
        Ok(RunningEnrollment { service, context })
    }
}

/// A serving enrollment listener plus the context control operations use.
pub(crate) struct RunningEnrollment {
    service: EnrollmentService,
    context: Arc<EnrollContext>,
}

/// Non-secret enrollment facts reported by `up` and `node status`.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub(crate) struct EnrollmentReport {
    endpoint: String,
    listen: String,
    enrollment_key: String,
    issuer: String,
    issuer_fingerprint: String,
    domain_name: String,
}

impl RunningEnrollment {
    pub(crate) fn report(&self) -> EnrollmentReport {
        let c = &self.context;
        EnrollmentReport {
            endpoint: c.endpoint.as_str().to_string(),
            listen: self.service.local_addr().to_string(),
            enrollment_key: hex::encode(c.key.0),
            issuer: hex::encode(c.issuer.entity_id().as_bytes()),
            issuer_fingerprint: net_sdk::enrollment::fingerprint(c.issuer.entity_id()),
            domain_name: c.domain_name.clone(),
        }
    }

    pub(crate) fn context(&self) -> Arc<EnrollContext> {
        self.context.clone()
    }

    pub(crate) async fn shutdown(self) {
        self.service.shutdown().await;
    }
}

/// State the node's control endpoint uses for `invite_*` operations.
pub(crate) struct EnrollContext {
    issuer: Identity,
    ledger: SharedLedger,
    key: EnrollmentKey,
    endpoint: EnrollmentEndpoint,
    trust_domain: TrustDomainId,
    domain_name: String,
}

impl EnrollContext {
    /// Handle one `invite_*` control request. Blocking (ledger I/O).
    pub(crate) fn handle(&self, op: &str, request: &Value) -> Value {
        let result = match op {
            "invite_create" => self.create(request),
            "invite_status" => self.status(request),
            "invite_revoke" => self.revoke(request),
            "invite_approve" => self.decide(request, true),
            "invite_deny" => self.decide(request, false),
            _ => Err("unknown invite operation".to_string()),
        };
        result.unwrap_or_else(|e| json!({ "error": e }))
    }

    fn create(&self, request: &Value) -> Result<Value, String> {
        let now = now_unix();
        let ttl = match request["ttl_secs"].as_u64() {
            Some(secs) => Duration::from_secs(secs),
            None => DEFAULT_INVITATION_TTL,
        };
        let mode = if request["require_approval"] == Value::Bool(true) {
            ApprovalMode::RequireApproval
        } else {
            ApprovalMode::Preauthorized
        };
        let intended = match request["subject"].as_str() {
            Some(hex) => Some(parse_entity(hex)?),
            None => None,
        };
        let policy = InvitationPolicy::with_options(now, ttl, mode).map_err(|e| e.to_string())?;
        let invite = MembershipInvite::sign(
            &self.issuer,
            InviteSpec {
                trust_domain_name: self.domain_name.clone(),
                trust_domain: self.trust_domain,
                endpoint: self.endpoint.clone(),
                enrollment_key: self.key,
                relations: vec![Relation::Mesh],
                intended_subject: intended,
                policy,
            },
        )
        .map_err(|e| e.to_string())?;
        let offer = self
            .ledger
            .lock()
            .offer(invite.offer_spec(), now)
            .map_err(|e| e.to_string())?;
        Ok(json!({
            "token": invite.encode(),
            "offer_id": offer.to_string(),
            "expires_at": policy.expires_at(),
            "approval": approval_name(mode),
            "bearer": invite.is_bearer(),
            "endpoint": self.endpoint.as_str(),
            "issuer_fingerprint": invite.issuer_fingerprint(),
        }))
    }

    fn status(&self, request: &Value) -> Result<Value, String> {
        let ledger = self.ledger.lock();
        let all = ledger.statuses().map_err(|e| e.to_string())?;
        let wanted = match request["offer_id"].as_str() {
            Some(id) => Some(parse_offer(id)?),
            None => None,
        };
        let rows: Vec<Value> = all
            .iter()
            .filter(|s| wanted.is_none_or(|w| s.offer_id == w))
            .map(|s| {
                let (state, subject, receipt) = match &s.state {
                    OfferState::Offered => ("offered", None, None),
                    OfferState::PendingApproval { subject } => ("pending_approval", Some(subject), None),
                    OfferState::Ready { subject } => ("ready", Some(subject), None),
                    OfferState::Issued {
                        subject,
                        receipt_id,
                        ..
                    } => ("issued", Some(subject), Some(receipt_id.to_string())),
                    OfferState::Revoked { subject } => ("revoked", subject.as_ref(), None),
                    OfferState::Denied { subject } => ("denied", Some(subject), None),
                };
                json!({
                    "offer_id": s.offer_id.to_string(),
                    "state": state,
                    "subject": subject.map(|e| hex::encode(e.as_bytes())),
                    "receipt_id": receipt,
                    "approval": approval_name(s.approval),
                    "expires_at": s.expires_at,
                    "intended_subject": s.intended_subject.as_ref().map(|e| hex::encode(e.as_bytes())),
                })
            })
            .collect();
        if wanted.is_some() && rows.is_empty() {
            return Err("unknown offer".to_string());
        }
        Ok(json!({ "offers": rows }))
    }

    fn revoke(&self, request: &Value) -> Result<Value, String> {
        let offer = parse_offer(request["offer_id"].as_str().unwrap_or_default())?;
        self.ledger
            .lock()
            .revoke(&offer, now_unix())
            .map_err(|e| e.to_string())?;
        Ok(json!({ "offer_id": offer.to_string(), "state": "revoked" }))
    }

    /// Approve or deny exactly the pending claim whose subject the operator names.
    fn decide(&self, request: &Value, approve: bool) -> Result<Value, String> {
        let offer = parse_offer(request["offer_id"].as_str().unwrap_or_default())?;
        let subject = parse_entity(request["subject"].as_str().unwrap_or_default())?;
        let mut ledger = self.ledger.lock();
        let claim = ledger
            .pending_claim(&offer)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "offer has no claim awaiting approval".to_string())?;
        if claim.subject != subject {
            return Err("the pending claim is from a different subject".to_string());
        }
        let now = now_unix();
        if approve {
            ledger.approve(&offer, &claim, now)
        } else {
            ledger.deny(&offer, &claim, now)
        }
        .map_err(|e| e.to_string())?;
        Ok(json!({
            "offer_id": offer.to_string(),
            "state": if approve { "approved" } else { "denied" },
        }))
    }
}

fn approval_name(mode: ApprovalMode) -> &'static str {
    match mode {
        ApprovalMode::Preauthorized => "preauthorized",
        ApprovalMode::RequireApproval => "require_approval",
    }
}

fn parse_entity(hex_id: &str) -> Result<EntityId, String> {
    let bytes = hex::decode(hex_id.trim_start_matches("0x"))
        .ok()
        .and_then(|b| <[u8; 32]>::try_from(b).ok())
        .ok_or_else(|| "subject must be a full 64-hex entity id".to_string())?;
    Ok(EntityId::from_bytes(bytes))
}

fn parse_offer(hex_id: &str) -> Result<OfferId, String> {
    let bytes = hex::decode(hex_id)
        .ok()
        .and_then(|b| <[u8; 16]>::try_from(b).ok())
        .ok_or_else(|| "offer id must be 32 hex characters".to_string())?;
    Ok(OfferId::from_bytes(bytes))
}

// ---- CLI: enrollment init ------------------------------------------------------

/// `net-mesh enrollment ...`
#[derive(Subcommand, Debug)]
pub enum EnrollmentCommand {
    /// Create an empty enrollment ledger bound to an issuer identity.
    Init(InitArgs),
}

/// `net-mesh enrollment init` arguments.
#[derive(Args, Debug)]
pub struct InitArgs {
    /// Node state directory; the ledger defaults to `<state-dir>/ledger`.
    #[arg(long, value_name = "DIR")]
    pub state_dir: Option<PathBuf>,

    /// Issuer identity file that will sign this ledger's invitations.
    #[arg(long, value_name = "PATH")]
    pub issuer_identity: PathBuf,

    /// Ledger directory (must not exist yet).
    #[arg(long, value_name = "DIR")]
    pub ledger: Option<PathBuf>,
}

pub async fn run_enrollment(
    cmd: EnrollmentCommand,
    output: Option<OutputFormat>,
    profile_name: &str,
) -> Result<(), CliError> {
    match cmd {
        EnrollmentCommand::Init(args) => {
            let state = state_dir(args.state_dir, profile_name)?;
            let issuer = crate::context::load_operator_identity(&args.issuer_identity).await?;
            let ledger = args.ledger.unwrap_or_else(|| state.join(LEDGER_SUBDIR));
            if let Some(parent) = ledger.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| generic(format!("create {}: {e}", parent.display())))?;
            }
            let (dir, entity) = (ledger.clone(), issuer.entity_id().clone());
            tokio::task::spawn_blocking(move || {
                EnrollmentLedger::create(&dir, entity, LedgerLimits::default()).map(drop)
            })
            .await
            .map_err(|e| generic(format!("ledger task failed: {e}")))?
            .map_err(|e| match e {
                LedgerError::Storage(
                    net::adapter::net::behavior::enrollment_storage::StorageError::AlreadyExists,
                ) => invalid_args(format!("ledger {} already exists", ledger.display())),
                other => generic(format!("ledger {}: {other}", ledger.display())),
            })?;
            emit_value(
                OutputFormat::resolve_oneshot(output),
                &json!({
                    "ledger": ledger.display().to_string(),
                    "issuer": hex::encode(issuer.entity_id().as_bytes()),
                    "issuer_fingerprint": net_sdk::enrollment::fingerprint(issuer.entity_id()),
                }),
            )
            .map_err(|e| generic(format!("write result: {e}")))
        }
    }
}

// ---- CLI: invite -----------------------------------------------------------------

/// `net-mesh invite ...`
#[derive(Subcommand, Debug)]
pub enum InviteCommand {
    /// Create a join token on the running `up --enroll` node.
    Create(CreateArgs),
    /// Show what a join token grants and where it points (offline; redeems nothing).
    Inspect(InspectArgs),
    /// List invitations and their claim/issuance state.
    Status(OfferQueryArgs),
    /// Invalidate an unredeemed invitation (does not revoke issued credentials).
    Revoke(OfferArgs),
    /// Approve the pending claim of a `--require-approval` invitation.
    Approve(DecideArgs),
    /// Deny the pending claim of a `--require-approval` invitation.
    Deny(DecideArgs),
}

/// `invite create` arguments.
#[derive(Args, Debug)]
pub struct CreateArgs {
    /// Node state directory (as given to `net-mesh up`).
    #[arg(long, value_name = "DIR")]
    pub state_dir: Option<PathBuf>,
    /// Lifetime of the unredeemed invitation (default 24h).
    #[arg(long, value_name = "DURATION", value_parser = crate::humantime::parse_duration)]
    pub ttl: Option<Duration>,
    /// Require an operator decision (`invite approve`) before issuing.
    #[arg(long)]
    pub require_approval: bool,
    /// Bind the invitation to one device's full 64-hex entity id.
    #[arg(long = "for", value_name = "ENTITY")]
    pub for_subject: Option<String>,
    /// Write the token to this new owner-only file instead of stdout.
    #[arg(long, value_name = "PATH")]
    pub out: Option<PathBuf>,
}

/// `invite inspect` arguments.
#[derive(Args, Debug)]
pub struct InspectArgs {
    /// The join token, or `-` to read it from stdin (keeps it out of shell history).
    pub token: String,
}

/// `invite status` arguments.
#[derive(Args, Debug)]
pub struct OfferQueryArgs {
    /// Node state directory (as given to `net-mesh up`).
    #[arg(long, value_name = "DIR")]
    pub state_dir: Option<PathBuf>,
    /// Limit to one offer id.
    pub offer_id: Option<String>,
}

/// `invite revoke` arguments.
#[derive(Args, Debug)]
pub struct OfferArgs {
    /// Node state directory (as given to `net-mesh up`).
    #[arg(long, value_name = "DIR")]
    pub state_dir: Option<PathBuf>,
    /// Offer id from `invite create` / `invite status`.
    pub offer_id: String,
}

/// `invite approve|deny` arguments.
#[derive(Args, Debug)]
pub struct DecideArgs {
    /// Node state directory (as given to `net-mesh up`).
    #[arg(long, value_name = "DIR")]
    pub state_dir: Option<PathBuf>,
    /// Offer id from `invite status`.
    pub offer_id: String,
    /// The full 64-hex subject shown by `invite status`; must match the claim.
    #[arg(long, value_name = "ENTITY")]
    pub subject: String,
}

async fn node_request(
    state_dir_arg: Option<PathBuf>,
    profile: &str,
    request: Value,
) -> Result<Value, CliError> {
    let dir = state_dir(state_dir_arg, profile)?.join(NODE_SUBDIR);
    let (_, reply) = control_call(&dir, request)
        .await
        .map_err(|e| connection_failure(format!("{e}; is `net-mesh up --enroll` running?")))?;
    if let Some(err) = reply["error"].as_str() {
        return Err(generic(err.to_string()));
    }
    Ok(reply)
}

pub async fn run_invite(
    cmd: InviteCommand,
    output: Option<OutputFormat>,
    profile_name: &str,
) -> Result<(), CliError> {
    let fmt = OutputFormat::resolve_oneshot(output);
    let emit = |v: &Value| emit_value(fmt, v).map_err(|e| generic(format!("write result: {e}")));
    match cmd {
        InviteCommand::Create(args) => {
            if let Some(subject) = &args.for_subject {
                parse_entity(subject).map_err(invalid_args)?;
            }
            if let Some(out) = &args.out {
                if out.exists() {
                    return Err(invalid_args(format!("{} already exists", out.display())));
                }
            }
            let mut request = json!({
                "op": "invite_create",
                "require_approval": args.require_approval,
            });
            if let Some(ttl) = args.ttl {
                request["ttl_secs"] = json!(ttl.as_secs());
            }
            if let Some(subject) = &args.for_subject {
                request["subject"] = json!(subject.trim_start_matches("0x"));
            }
            let mut reply = node_request(args.state_dir, profile_name, request).await?;
            if reply["bearer"] == Value::Bool(true) {
                eprintln!(
                    "note: this token is bearer authorization — whoever redeems it first joins. \
                     Send it privately, or bind it to a device with --for."
                );
            }
            if let Some(out) = &args.out {
                let token = crate::secret::ScrubbedString::new(
                    reply["token"].as_str().unwrap_or_default().to_string(),
                );
                let tmp = out.with_extension("tmp-netmesh-join");
                crate::commands::identity::write_identity_atomically(&tmp, out, token.as_bytes())
                    .await?;
                reply["token"] = Value::Null;
                reply["token_file"] = json!(out.display().to_string());
            }
            emit(&reply)
        }
        InviteCommand::Inspect(args) => {
            let token = if args.token == "-" {
                let mut buf = String::new();
                use tokio::io::AsyncReadExt as _;
                tokio::io::stdin()
                    .take(4096)
                    .read_to_string(&mut buf)
                    .await
                    .map_err(|e| generic(format!("read token from stdin: {e}")))?;
                crate::secret::ScrubbedString::new(buf)
            } else {
                crate::secret::ScrubbedString::new(args.token)
            };
            let invite = MembershipInvite::decode(token.as_str())
                .map_err(|e| invalid_args(format!("not a valid join token: {e}")))?;
            let policy = invite.policy();
            emit(&json!({
                "issuer": hex::encode(invite.issuer().as_bytes()),
                "issuer_fingerprint": invite.issuer_fingerprint(),
                "endpoint": invite.endpoint().as_str(),
                "enrollment_key": hex::encode(invite.enrollment_key().0),
                "domain_name": invite.trust_domain_name(),
                "trust_domain": invite.trust_domain().to_string(),
                "relations": invite.relations().iter().map(|r| format!("{r:?}").to_lowercase()).collect::<Vec<_>>(),
                "approval": approval_name(policy.approval_mode()),
                "created_at": policy.created_at(),
                "expires_at": policy.expires_at(),
                "expired": now_unix() >= policy.expires_at(),
                "bearer": invite.is_bearer(),
                "intended_subject": invite.intended_subject().map(|e| hex::encode(e.as_bytes())),
                "signature": "valid for the embedded issuer; confirm the fingerprint out of band",
            }))
        }
        InviteCommand::Status(args) => {
            let mut request = json!({ "op": "invite_status" });
            if let Some(id) = args.offer_id {
                request["offer_id"] = json!(id);
            }
            emit(&node_request(args.state_dir, profile_name, request).await?)
        }
        InviteCommand::Revoke(args) => emit(
            &node_request(
                args.state_dir,
                profile_name,
                json!({ "op": "invite_revoke", "offer_id": args.offer_id }),
            )
            .await?,
        ),
        InviteCommand::Approve(args) => emit(
            &node_request(
                args.state_dir,
                profile_name,
                json!({ "op": "invite_approve", "offer_id": args.offer_id, "subject": args.subject.trim_start_matches("0x") }),
            )
            .await?,
        ),
        InviteCommand::Deny(args) => emit(
            &node_request(
                args.state_dir,
                profile_name,
                json!({ "op": "invite_deny", "offer_id": args.offer_id, "subject": args.subject.trim_start_matches("0x") }),
            )
            .await?,
        ),
    }
}
