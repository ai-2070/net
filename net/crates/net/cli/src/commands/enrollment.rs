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
    EnrollmentEndpoint, EnrollmentKey, InviteSpec, MembershipInvite, Relation, RelayLocator,
};
use net_sdk::enrollment::policy::{ApprovalMode, InvitationPolicy, DEFAULT_INVITATION_TTL};
use net_sdk::enrollment::redeem::Refusal;
use net_sdk::enrollment::service::{
    BundleIssuer, EnrollmentService, ServiceConfig, SessionSink, SharedLedger,
};
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

/// The project-run default relay for `up --enroll`. Deliberately empty until a
/// relay is actually deployed: an invented address would send every device's
/// registration to whoever holds it. `--relay` or the profile `relay` apply.
pub(crate) const DEFAULT_RELAY: Option<&str> = None;

// ---- up --enroll -------------------------------------------------------------

/// Validated `up --enroll` inputs. Built before any filesystem or network effect.
pub(crate) struct EnrollPlan {
    public: Option<EnrollmentEndpoint>,
    relay: Option<EnrollmentEndpoint>,
    issuer_path: Option<PathBuf>,
    /// `Some` when the operator named a ledger; it must already exist.
    explicit_ledger: Option<PathBuf>,
    default_ledger: PathBuf,
    domain_name: String,
    port_mapping: bool,
}

impl EnrollPlan {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn validate(
        public_addr: Option<String>,
        relay: Option<String>,
        issuer_identity: Option<PathBuf>,
        ledger: Option<PathBuf>,
        domain_name: Option<String>,
        port_mapping: bool,
        state: &Path,
        profile: &str,
    ) -> Result<Self, CliError> {
        let public = match public_addr {
            Some(raw) => Some(
                EnrollmentEndpoint::parse(&raw)
                    .map_err(|e| invalid_args(format!("--public-addr: {e}")))?,
            ),
            None => None,
        };
        let relay = match relay {
            Some(raw) => Some(
                EnrollmentEndpoint::parse(&raw)
                    .map_err(|e| invalid_args(format!("--relay: {e}")))?,
            ),
            None => None,
        };
        if let Some(ledger) = &ledger {
            if !ledger.is_dir() {
                return Err(invalid_args(format!(
                    "no enrollment ledger at {}; run `net-mesh enrollment init` or omit --ledger",
                    ledger.display()
                )));
            }
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
            public,
            relay,
            issuer_path: issuer_identity,
            explicit_ledger: ledger,
            default_ledger: state.join(LEDGER_SUBDIR),
            domain_name,
            port_mapping,
        })
    }

    /// Whether the mesh should also map its UDP port on the router.
    pub(crate) fn port_mapping(&self) -> bool {
        self.port_mapping
    }

    /// Provision everything enrollment needs before any bind: a fixed port
    /// (persisted on first use), the issuer (explicit file, or an issuer seed
    /// created once and persisted with the node state) and the ledger (explicit,
    /// or created once at the default path). Returns the bind address to use.
    pub(crate) async fn provision(
        self,
        bind: SocketAddr,
        secrets: &mut super::lifecycle::NodeSecrets,
        persist: impl FnOnce(&super::lifecycle::NodeSecrets) -> Result<(), CliError>,
    ) -> Result<(SocketAddr, EnrollOwner), CliError> {
        let mut changed = false;
        let mut created = Vec::new();
        let port = match (bind.port(), secrets.enroll_port) {
            (0, Some(port)) => port,
            (0, None) => {
                let port = free_port(bind.ip())?;
                secrets.enroll_port = Some(port);
                changed = true;
                created.push("port");
                port
            }
            (port, _) => port,
        };
        let issuer = match &self.issuer_path {
            Some(path) => crate::context::load_operator_identity(path).await?,
            None => {
                let seed = match secrets.issuer {
                    Some(seed) => seed,
                    None => {
                        let mut seed = [0u8; 32];
                        getrandom::fill(&mut seed)
                            .map_err(|_| generic("operating-system CSPRNG unavailable"))?;
                        secrets.issuer = Some(seed);
                        changed = true;
                        created.push("issuer");
                        seed
                    }
                };
                Identity::from_seed(seed)
            }
        };
        if changed {
            persist(secrets)?;
        }
        let ledger_dir = self
            .explicit_ledger
            .clone()
            .unwrap_or_else(|| self.default_ledger.clone());
        let create = self.explicit_ledger.is_none() && !ledger_dir.exists();
        let (dir, entity) = (ledger_dir.clone(), issuer.entity_id().clone());
        let ledger = tokio::task::spawn_blocking(move || {
            if create {
                EnrollmentLedger::create(&dir, entity, LedgerLimits::default())
            } else {
                EnrollmentLedger::open(&dir, entity, LedgerLimits::default())
            }
        })
        .await
        .map_err(|e| generic(format!("ledger task failed: {e}")))?
        .map_err(|e| ledger_open_error(&ledger_dir, e))?;
        if create {
            created.push("ledger");
        }
        Ok((
            SocketAddr::new(bind.ip(), port),
            EnrollOwner {
                plan: self,
                issuer,
                ledger: Arc::new(parking_lot::Mutex::new(ledger)),
                bind: SocketAddr::new(bind.ip(), port),
                created,
            },
        ))
    }
}

/// A port currently free for both UDP and TCP on `ip`: the OS-chosen TCP port
/// first, then random ports from the IANA dynamic range. Some platforms
/// (Windows with Hyper-V/WSL) reserve large TCP-only and UDP-only blocks inside
/// the ephemeral range and allocate ephemeral ports sequentially, so retrying
/// the OS's choice alone can keep landing in the same reserved block.
fn free_port(ip: std::net::IpAddr) -> Result<u16, CliError> {
    for attempt in 0..64 {
        let candidate = if attempt == 0 {
            0
        } else {
            let r: [u8; 2] = super::lifecycle::random()?;
            49_152 + u16::from_be_bytes(r) % 16_384
        };
        let Ok(tcp) = std::net::TcpListener::bind((ip, candidate)) else {
            continue;
        };
        let port = tcp
            .local_addr()
            .map_err(|e| generic(format!("choose enrollment port: {e}")))?
            .port();
        if std::net::UdpSocket::bind((ip, port)).is_ok() {
            return Ok(port);
        }
    }
    Err(generic(
        "could not find a port free for both UDP and TCP; pass --bind",
    ))
}

fn ledger_open_error(dir: &Path, e: LedgerError) -> CliError {
    match e {
        LedgerError::IssuerMismatch => invalid_args(format!(
            "ledger {} belongs to a different issuer",
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
    bind: SocketAddr,
    created: Vec<&'static str>,
}

/// How long `up --enroll` waits for the router to answer mapping requests.
const MAPPING_WAIT: Duration = Duration::from_secs(4);

/// The direct address signed into tokens by default: the operator's
/// `--public-addr`, else the router-mapped address (only when both TCP and UDP
/// were mapped), else a concrete bind address. `None` means no direct path is
/// known; `invite create --addr` can still supply one.
pub(crate) fn select_endpoint(
    public: Option<&EnrollmentEndpoint>,
    mapped_tcp: Option<SocketAddr>,
    mapped_udp: Option<SocketAddr>,
    bind: SocketAddr,
) -> Option<EnrollmentEndpoint> {
    if let Some(public) = public {
        return Some(public.clone());
    }
    if let (Some(tcp), Some(_udp)) = (mapped_tcp, mapped_udp) {
        return EnrollmentEndpoint::parse(&tcp.to_string()).ok();
    }
    if bind.ip().is_unspecified() {
        return None;
    }
    EnrollmentEndpoint::parse(&bind.to_string()).ok()
}

impl EnrollOwner {
    /// Map ports (unless disabled), bind the enrollment listener on the mesh's
    /// port number (TCP), and start delivering this node's PSK and contact.
    pub(crate) async fn start(
        self,
        mesh: &net_sdk::Mesh,
        psk: Psk,
    ) -> Result<RunningEnrollment, CliError> {
        let (tcp_mapping, udp_mapped) = if self.plan.port_mapping {
            let udp = async {
                let deadline = tokio::time::Instant::now() + MAPPING_WAIT;
                loop {
                    if let Some(addr) = mesh.traversal_stats().port_mapping_external {
                        return Some(addr);
                    }
                    if tokio::time::Instant::now() >= deadline {
                        return None;
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            };
            let tcp = tokio::time::timeout(
                MAPPING_WAIT,
                net_sdk::enrollment::portmap::TcpMapping::establish(self.bind.port()),
            );
            let (tcp, udp) = tokio::join!(tcp, udp);
            (tcp.ok().flatten(), udp)
        } else {
            (None, None)
        };
        let tcp_mapped = tcp_mapping.as_ref().and_then(|m| m.external());
        let default_endpoint =
            select_endpoint(self.plan.public.as_ref(), tcp_mapped, udp_mapped, self.bind);
        let trust_domain = psk.trust_domain();
        let bundles = Arc::new(NodeBundles {
            issuer: self.issuer.clone(),
            psk,
            noise_pubkey: *mesh.public_key(),
            node_id: mesh.node_id(),
            tcp_mapped,
            udp_mapped,
            contacts: parking_lot::Mutex::new(std::collections::HashMap::new()),
        });
        let service = EnrollmentService::bind(
            self.bind,
            &self.issuer,
            self.ledger.clone(),
            bundles.clone(),
            ServiceConfig::default(),
        )
        .await
        .map_err(|e| {
            connection_failure(format!("enrollment listener on TCP {}: {e}", self.bind))
        })?;
        // Relay fallback: register (and keep re-trying) in the background;
        // the node serves direct joiners whether or not the relay is up.
        let relay = match self.plan.relay {
            Some(endpoint) => {
                Some(RelayLink::start(mesh.node().clone(), endpoint, service.session_sink()).await)
            }
            None => None,
        };
        let context = Arc::new(EnrollContext {
            issuer: self.issuer,
            ledger: self.ledger,
            key: service.enrollment_key(),
            default_endpoint,
            relay: relay.as_ref().map(|r| r.locator.clone()),
            trust_domain,
            domain_name: self.plan.domain_name,
            bundles,
        });
        Ok(RunningEnrollment {
            service,
            context,
            tcp_mapping,
            relay,
            port_mapping: self.plan.port_mapping,
            created: self.created,
        })
    }
}

/// How long `up --enroll` waits for the first relay registration before it
/// reports readiness (it keeps retrying in the background either way).
const RELAY_REGISTER_WAIT: Duration = Duration::from_secs(4);
/// Pause between relay registration attempts while the relay is unreachable.
const RELAY_RETRY: Duration = Duration::from_secs(15);
/// Spliced enrollment streams buffered for the service.
const RELAY_SPLICE_BACKLOG: usize = 16;

/// This node's registration with its blind relay. Registration is retried in
/// the background until it succeeds (and then refreshed by the core, which
/// re-registers after a relay restart); each spliced enrollment stream is
/// handed to the enrollment service like an accepted connection.
struct RelayLink {
    locator: RelayLocator,
    state: Arc<parking_lot::Mutex<RelayLinkState>>,
    task: tokio::task::JoinHandle<()>,
}

#[derive(Default)]
struct RelayLinkState {
    registered: Option<SocketAddr>,
    error: Option<String>,
}

impl RelayLink {
    async fn start(
        node: Arc<net::adapter::net::MeshNode>,
        endpoint: EnrollmentEndpoint,
        sink: SessionSink,
    ) -> Self {
        let locator = RelayLocator {
            endpoint: endpoint.clone(),
            registration: net::adapter::net::traversal::blind_relay::registration_id(
                node.entity_id(),
            ),
        };
        let state = Arc::new(parking_lot::Mutex::new(RelayLinkState::default()));
        let (first_tx, first) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(relay_loop(node, endpoint, sink, state.clone(), first_tx));
        let _ = tokio::time::timeout(RELAY_REGISTER_WAIT, first).await;
        Self {
            locator,
            state,
            task,
        }
    }
}

impl Drop for RelayLink {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn relay_loop(
    node: Arc<net::adapter::net::MeshNode>,
    endpoint: EnrollmentEndpoint,
    sink: SessionSink,
    state: Arc<parking_lot::Mutex<RelayLinkState>>,
    first: tokio::sync::oneshot::Sender<()>,
) {
    let mut first = Some(first);
    loop {
        let attempt = async {
            let addr = tokio::net::lookup_host(endpoint.as_str())
                .await
                .map_err(|e| format!("resolve {}: {e}", endpoint.as_str()))?
                .next()
                .ok_or_else(|| format!("{} does not resolve", endpoint.as_str()))?;
            node.relay_register(addr).await.map_err(|e| e.to_string())
        };
        match attempt.await {
            Ok(mut registration) => {
                *state.lock() = RelayLinkState {
                    registered: Some(registration.relay()),
                    error: None,
                };
                if let Some(first) = first.take() {
                    let _ = first.send(());
                }
                let mut streams = registration.accept_splices(RELAY_SPLICE_BACKLOG);
                while let Some(stream) = streams.recv().await {
                    sink.serve(stream);
                }
                return;
            }
            Err(e) => {
                state.lock().error = Some(e);
                if let Some(first) = first.take() {
                    let _ = first.send(());
                }
                tokio::time::sleep(RELAY_RETRY).await;
            }
        }
    }
}

/// Delivers bundles whose mesh contact matches the paths each token named.
struct NodeBundles {
    issuer: Identity,
    psk: Psk,
    noise_pubkey: [u8; 32],
    node_id: u64,
    tcp_mapped: Option<SocketAddr>,
    udp_mapped: Option<SocketAddr>,
    contacts: parking_lot::Mutex<std::collections::HashMap<String, SocketAddr>>,
}

impl NodeBundles {
    /// The UDP mesh address matching a token's enrollment endpoint: the
    /// router's UDP mapping for the mapped TCP address, otherwise the same
    /// host and port the token names (TCP and UDP share the port number).
    fn contact_addr(&self, endpoint: &EnrollmentEndpoint) -> Option<SocketAddr> {
        if let Some(addr) = self.contacts.lock().get(endpoint.as_str()) {
            return Some(*addr);
        }
        let addr = match (self.tcp_mapped, self.udp_mapped) {
            (Some(tcp), Some(udp)) if tcp.to_string() == endpoint.as_str() => udp,
            _ => {
                use std::net::ToSocketAddrs as _;
                endpoint.as_str().to_socket_addrs().ok()?.next()?
            }
        };
        self.contacts
            .lock()
            .insert(endpoint.as_str().to_string(), addr);
        Some(addr)
    }

    /// The mesh contact for a token: the direct UDP address matching its
    /// enrollment endpoint (if it named one) and the relay it named (if any).
    fn contact_for(&self, invite: &MembershipInvite) -> Option<MeshContact> {
        let addr = match invite.endpoint() {
            Some(endpoint) => Some(self.contact_addr(endpoint)?),
            None => None,
        };
        let relay = invite.relay().cloned();
        if addr.is_none() && relay.is_none() {
            return None;
        }
        Some(MeshContact {
            addr,
            noise_pubkey: self.noise_pubkey,
            node_id: self.node_id,
            relay,
        })
    }

    fn issuer_for(&self, contact: MeshContact) -> MembershipIssuer {
        MembershipIssuer::new(self.issuer.clone(), self.psk.clone(), contact)
    }
}

impl BundleIssuer for NodeBundles {
    fn issue(
        &self,
        invite: &MembershipInvite,
        intent: &net_sdk::enrollment::invite::RedemptionIntent,
    ) -> Result<Vec<u8>, Refusal> {
        let contact = self.contact_for(invite).ok_or(Refusal::Unavailable)?;
        self.issuer_for(contact).issue(invite, intent)
    }

    fn may_recover(
        &self,
        invite: &MembershipInvite,
        intent: &net_sdk::enrollment::invite::RedemptionIntent,
    ) -> Result<(), Refusal> {
        let unspecified = MeshContact {
            addr: Some(SocketAddr::from(([0, 0, 0, 0], 0))),
            noise_pubkey: self.noise_pubkey,
            node_id: self.node_id,
            relay: None,
        };
        self.issuer_for(unspecified).may_recover(invite, intent)
    }
}

/// A serving enrollment listener plus the context control operations use.
pub(crate) struct RunningEnrollment {
    service: EnrollmentService,
    context: Arc<EnrollContext>,
    tcp_mapping: Option<net_sdk::enrollment::portmap::TcpMapping>,
    relay: Option<RelayLink>,
    port_mapping: bool,
    created: Vec<&'static str>,
}

/// Non-secret enrollment facts reported by `up` and `node status`.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub(crate) struct EnrollmentReport {
    /// Default direct address signed into tokens; `None` when none is known.
    endpoint: Option<String>,
    listen: String,
    port_mapping: String,
    mapped_tcp: Option<String>,
    mapped_udp: Option<String>,
    enrollment_key: String,
    issuer: String,
    issuer_fingerprint: String,
    domain_name: String,
    /// The relay tokens name as the fallback path; `None` when none is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    relay: Option<String>,
    /// `registered` or `unavailable` (retrying) when a relay is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    relay_state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    relay_error: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    created: Vec<String>,
}

impl RunningEnrollment {
    pub(crate) fn report(&self) -> EnrollmentReport {
        let c = &self.context;
        let b = &c.bundles;
        let port_mapping = match (self.port_mapping, b.tcp_mapped, b.udp_mapped) {
            (false, _, _) => "disabled",
            (true, Some(_), Some(_)) => "active",
            (true, None, None) => "unavailable",
            (true, _, _) => "partial",
        };
        EnrollmentReport {
            endpoint: c.default_endpoint.as_ref().map(|e| e.as_str().to_string()),
            listen: self.service.local_addr().to_string(),
            port_mapping: port_mapping.to_string(),
            mapped_tcp: b.tcp_mapped.map(|a| a.to_string()),
            mapped_udp: b.udp_mapped.map(|a| a.to_string()),
            enrollment_key: hex::encode(c.key.0),
            issuer: hex::encode(c.issuer.entity_id().as_bytes()),
            issuer_fingerprint: net_sdk::enrollment::fingerprint(c.issuer.entity_id()),
            domain_name: c.domain_name.clone(),
            relay: self
                .relay
                .as_ref()
                .map(|r| r.locator.endpoint.as_str().to_string()),
            relay_state: self.relay.as_ref().map(|r| {
                if r.state.lock().registered.is_some() {
                    "registered".to_string()
                } else {
                    "unavailable".to_string()
                }
            }),
            relay_error: self
                .relay
                .as_ref()
                .and_then(|r| r.state.lock().error.clone()),
            created: self.created.iter().map(|s| s.to_string()).collect(),
        }
    }

    pub(crate) fn context(&self) -> Arc<EnrollContext> {
        self.context.clone()
    }

    pub(crate) async fn shutdown(self) {
        drop(self.relay);
        self.service.shutdown().await;
        if let Some(mapping) = self.tcp_mapping {
            mapping.shutdown().await;
        }
    }
}

/// State the node's control endpoint uses for `invite_*` operations.
pub(crate) struct EnrollContext {
    issuer: Identity,
    ledger: SharedLedger,
    key: EnrollmentKey,
    default_endpoint: Option<EnrollmentEndpoint>,
    relay: Option<RelayLocator>,
    trust_domain: TrustDomainId,
    domain_name: String,
    bundles: Arc<NodeBundles>,
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
        let endpoint = match request["addr"].as_str() {
            Some(addr) => {
                Some(EnrollmentEndpoint::parse(addr).map_err(|e| format!("--addr: {e}"))?)
            }
            None => self.default_endpoint.clone(),
        };
        if endpoint.is_none() && self.relay.is_none() {
            return Err(
                "no direct address is known for this node (no --public-addr, no router \
                 mapping, wildcard bind) and no relay is set; pass --addr <host:port> reachable \
                 by the joiner, or run `up --enroll --relay <host:port>`"
                    .to_string(),
            );
        }
        // Resolve the matching mesh contact now so an unusable address fails
        // here, not at redemption.
        if let Some(endpoint) = &endpoint {
            self.bundles
                .contact_addr(endpoint)
                .ok_or_else(|| format!("address {} does not resolve", endpoint.as_str()))?;
        }
        let policy = InvitationPolicy::with_options(now, ttl, mode).map_err(|e| e.to_string())?;
        let invite = MembershipInvite::sign(
            &self.issuer,
            InviteSpec {
                trust_domain_name: self.domain_name.clone(),
                trust_domain: self.trust_domain,
                endpoint: endpoint.clone(),
                relay: self.relay.clone(),
                enrollment_key: self.key,
                subnet: None,
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
            "endpoint": endpoint.as_ref().map(|e| e.as_str()),
            "relay": self.relay.as_ref().map(|r| r.endpoint.as_str()),
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
    /// Address the joiner reaches this node at (`host:port`), overriding the
    /// node's default direct address for this token.
    #[arg(long, value_name = "HOST:PORT")]
    pub addr: Option<String>,
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
            if let Some(addr) = &args.addr {
                EnrollmentEndpoint::parse(addr).map_err(|e| invalid_args(format!("--addr: {e}")))?;
                request["addr"] = json!(addr);
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
                "endpoint": invite.endpoint().map(|e| e.as_str()),
                "relay": invite.relay().map(|r| r.endpoint.as_str()),
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

// ---- CLI: join ------------------------------------------------------------------

/// `net-mesh join` arguments.
#[derive(Args, Debug)]
pub struct JoinArgs {
    /// The join token, or `-` to read it from stdin (keeps it out of shell
    /// history; `--yes` is then required because stdin cannot also answer the
    /// confirmation prompt).
    pub token: String,
    /// State directory for this device's join and its later `net-mesh up`.
    #[arg(long, value_name = "DIR")]
    pub state_dir: Option<PathBuf>,
    /// Skip the interactive confirmation (scripts and agent tool use).
    #[arg(long)]
    pub yes: bool,
    /// Bound on the redemption session and on the live attach check.
    #[arg(long, value_name = "DURATION", default_value = "15s", value_parser = crate::humantime::parse_duration)]
    pub wait: Duration,
    /// Join again after `net-mesh leave`. The credentials are fetched from the
    /// issuer again and must pass its current authorization.
    #[arg(long)]
    pub rejoin: bool,
}

pub(crate) const JOIN_SUBDIR: &str = "join";

/// How long a direct mesh attach may take before the relay fallback, when the
/// contact also names a relay. Without a relay the direct path has the whole
/// wait.
const DIRECT_ATTACH_WAIT: Duration = Duration::from_secs(5);

/// Attach `mesh` to `contact` via the routed handshake: **direct first**, the
/// contact's blind relay only if the direct attempt fails. The session
/// authenticates the contact's key end-to-end on either path. Returns the path
/// used (`"direct"` or `"relay"`).
pub(crate) async fn attach_contact(
    mesh: &net_sdk::Mesh,
    contact: &MeshContact,
    wait: Duration,
) -> Result<&'static str, String> {
    let deadline = tokio::time::Instant::now() + wait;
    let mut direct_failure = None;
    if let Some(addr) = contact.addr {
        let budget = if contact.relay.is_some() {
            DIRECT_ATTACH_WAIT.min(wait)
        } else {
            wait
        };
        let attempt = tokio::time::timeout(
            budget,
            mesh.connect_via(&addr.to_string(), &contact.noise_pubkey, contact.node_id),
        )
        .await;
        match attempt {
            Ok(Ok(())) => return Ok("direct"),
            Ok(Err(e)) => direct_failure = Some(format!("direct {addr}: {e}")),
            Err(_) => direct_failure = Some(format!("direct {addr}: timed out")),
        }
    }
    let Some(relay) = &contact.relay else {
        return Err(direct_failure.unwrap_or_else(|| "the contact names no address".to_string()));
    };
    let relayed = tokio::time::timeout_at(deadline, async {
        let addr = tokio::net::lookup_host(relay.endpoint.as_str())
            .await
            .map_err(|e| format!("resolve {}: {e}", relay.endpoint.as_str()))?
            .next()
            .ok_or_else(|| format!("{} does not resolve", relay.endpoint.as_str()))?;
        let via = mesh
            .node()
            .relay_bind(addr, relay.registration)
            .await
            .map_err(|e| e.to_string())?;
        mesh.node()
            .connect_via_endpoint(via, &contact.noise_pubkey, contact.node_id)
            .await
            .map(drop)
            .map_err(|e| e.to_string())
    })
    .await;
    let relay_failure = match relayed {
        Ok(Ok(())) => return Ok("relay"),
        Ok(Err(e)) => format!("relay {}: {e}", relay.endpoint.as_str()),
        Err(_) => format!("relay {}: timed out", relay.endpoint.as_str()),
    };
    Err(match direct_failure {
        Some(direct) => format!("{direct}; {relay_failure}"),
        None => relay_failure,
    })
}

/// Build a mesh as `identity` with `psk` and attach it to `contact`
/// ([`attach_contact`]). The local socket binds loopback for a loopback
/// contact and the wildcard otherwise (outbound only).
pub(crate) async fn attach_mesh(
    identity: Identity,
    psk: &Psk,
    contact: &MeshContact,
    bind: Option<SocketAddr>,
    wait: Duration,
) -> Result<(net_sdk::Mesh, &'static str), String> {
    let hint = contact.addr.map(|a| a.ip()).or_else(|| {
        contact
            .relay
            .as_ref()
            .and_then(|r| r.endpoint.as_str().parse::<SocketAddr>().ok())
            .map(|a| a.ip())
    });
    let bind = bind.unwrap_or_else(|| match hint {
        Some(ip) if ip.is_loopback() => SocketAddr::new(ip, 0),
        Some(ip) if ip.is_ipv6() => SocketAddr::from(([0u16; 8], 0)),
        _ => SocketAddr::from(([0, 0, 0, 0], 0)),
    });
    let mesh = net_sdk::MeshBuilder::new(&bind.to_string(), psk.expose_bytes())
        .map_err(|e| e.to_string())?
        .identity(identity)
        .build()
        .await
        .map_err(|e| e.to_string())?;
    mesh.start();
    match attach_contact(&mesh, contact, wait).await {
        Ok(path) => Ok((mesh, path)),
        Err(e) => {
            let _ = mesh.shutdown().await;
            Err(e)
        }
    }
}

/// Redeem a join token: confirm the issuer, persist identity and intent,
/// redeem, verify and install the bundle, then prove live admission separately.
pub async fn run_join(
    args: JoinArgs,
    output: Option<OutputFormat>,
    profile_name: &str,
) -> Result<(), CliError> {
    use net_sdk::enrollment::device::{DeviceJoin, DeviceJoinError, JoinStatus};
    use net_sdk::enrollment::redeem::RedeemError;

    let state = state_dir(args.state_dir, profile_name)?;
    let from_stdin = args.token == "-";
    let token = if from_stdin {
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
    if now_unix() >= policy.expires_at() {
        return Err(invalid_args(
            "this join token has expired; ask for a new one",
        ));
    }

    // Show what is being trusted before anything is written or sent.
    let direct = invite.endpoint().map_or("none", |e| e.as_str());
    let enroll_via = match invite.relay() {
        Some(relay) => format!("{direct} (relay fallback {})", relay.endpoint.as_str()),
        None => direct.to_string(),
    };
    eprintln!(
        "Joining trust domain '{}' (id {}) run by issuer {}\n  enrollment address: {}\n  token: {}, expires at unix {}",
        invite.trust_domain_name(),
        invite.trust_domain(),
        invite.issuer_fingerprint(),
        enroll_via,
        if invite.is_bearer() {
            "bearer (the first redeemer joins)"
        } else {
            "bound to one device"
        },
        policy.expires_at(),
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

    std::fs::create_dir_all(&state)
        .map_err(|e| generic(format!("create state directory {}: {e}", state.display())))?;
    let dir = state.join(JOIN_SUBDIR);
    let storage_err = |e: DeviceJoinError| match e {
        DeviceJoinError::Storage(
            net::adapter::net::behavior::enrollment_storage::StorageError::Busy,
        ) => generic(format!(
            "join state {} is in use (is `net-mesh up` running with it?)",
            dir.display()
        )),
        other => generic(format!("join state {}: {other}", dir.display())),
    };
    let mut join = if dir.exists() {
        let mut join = DeviceJoin::open(&dir).map_err(storage_err)?;
        if join.invite().digest() != invite.digest() {
            return Err(invalid_args(format!(
                "{} already holds a join from a different token; use another --state-dir",
                state.display()
            )));
        }
        if let Some(at) = join.left_at() {
            if !args.rejoin {
                return Err(invalid_args(format!(
                    "this device left the mesh at unix {at}; pass --rejoin to join again \
                     (the issuer must still authorize it)"
                )));
            }
            join.rejoin().map_err(storage_err)?;
        }
        join
    } else {
        DeviceJoin::begin(&dir, &invite, Identity::generate()).map_err(storage_err)?
    };
    let device = hex::encode(join.identity().entity_id().as_bytes());
    let fmt = OutputFormat::resolve_oneshot(output);
    let status = join.redeem(args.wait).await.map_err(|e| match e {
        DeviceJoinError::Redeem(RedeemError::Refused(r)) => {
            generic(format!("the issuer refused this join: {r}"))
        }
        DeviceJoinError::Redeem(
            RedeemError::Io(_)
            | RedeemError::Timeout
            | RedeemError::Handshake
            | RedeemError::Relay(_),
        ) => connection_failure(format!(
            "could not complete enrollment via {enroll_via}: {e}"
        )),
        other => generic(format!("join failed: {other}")),
    })?;
    if status == JoinStatus::PendingApproval {
        return emit_value(
            fmt,
            &json!({
                "state": "pending_approval",
                "device": device,
                "detail": "the operator must approve this device; run the same join again afterwards",
            }),
        )
        .map_err(|e| generic(format!("write result: {e}")));
    }
    let Some(bundle) = join.bundle() else {
        return Err(generic("join reported installed without a bundle"));
    };
    let contact = bundle.contact().clone();
    let trust_domain = bundle.psk().trust_domain().to_string();
    let enroll_path = join.last_path().map(|p| p.as_str());
    // Installation is credential state; live admission is observed separately.
    let attach_path = match attach_mesh(
        join.identity().clone(),
        bundle.psk(),
        &contact,
        None,
        args.wait,
    )
    .await
    {
        Ok((mesh, path)) => {
            let _ = mesh.shutdown().await;
            path
        }
        Err(e) => {
            return Err(connection_failure(format!(
            "credentials are installed, but the live attach failed ({e}); `net-mesh up` retries it"
        )))
        }
    };
    emit_value(
        fmt,
        &json!({
            "state": "joined",
            "device": device,
            "issuer_fingerprint": invite.issuer_fingerprint(),
            "domain_name": invite.trust_domain_name(),
            "trust_domain": trust_domain,
            "contact": contact.addr.map(|a| a.to_string()),
            "relay": contact.relay.as_ref().map(|r| r.endpoint.as_str()),
            "installed": true,
            "attached": true,
            // `null` when the bundle was already installed before this run.
            "enroll_path": enroll_path,
            "attach_path": attach_path,
        }),
    )
    .map_err(|e| generic(format!("write result: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ep(s: &str) -> EnrollmentEndpoint {
        EnrollmentEndpoint::parse(s).unwrap()
    }

    #[test]
    fn default_endpoint_prefers_public_then_full_mapping_then_concrete_bind() {
        let bind_any: SocketAddr = "0.0.0.0:7443".parse().unwrap();
        let bind_lan: SocketAddr = "192.168.1.20:7443".parse().unwrap();
        let tcp: SocketAddr = "203.0.113.9:40001".parse().unwrap();
        let udp: SocketAddr = "203.0.113.9:40002".parse().unwrap();
        let public = ep("node.example.net:7443");

        let pick = |p: Option<&EnrollmentEndpoint>, t, u, b| {
            select_endpoint(p, t, u, b).map(|e| e.as_str().to_string())
        };
        assert_eq!(
            pick(Some(&public), Some(tcp), Some(udp), bind_lan).as_deref(),
            Some("node.example.net:7443")
        );
        assert_eq!(
            pick(None, Some(tcp), Some(udp), bind_any).as_deref(),
            Some("203.0.113.9:40001")
        );
        // A TCP-only mapping is not a usable direct path (the mesh is UDP).
        assert_eq!(pick(None, Some(tcp), None, bind_any), None);
        assert_eq!(
            pick(None, None, Some(udp), bind_lan).as_deref(),
            Some("192.168.1.20:7443")
        );
        // Wildcard bind and no mapping: no direct address is claimed.
        assert_eq!(pick(None, None, None, bind_any), None);
    }
}
