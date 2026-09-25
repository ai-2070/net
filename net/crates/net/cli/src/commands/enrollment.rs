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
                org: Arc::new(OrgBook::new(ledger_dir.with_extension("org"))),
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
    /// Org membership state beside the ledger: the certificates the operator
    /// approved per claim, and which org each org invite offers.
    org: Arc<OrgBook>,
}

impl EnrollOwner {
    /// The enrollment issuer identity: it signs invites and receipts, and a
    /// channel grant (`channel issue-grant --issuer`) must name it.
    pub(crate) fn issuer(&self) -> &Identity {
        &self.issuer
    }
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
        subnet: Option<net_sdk::enrollment::bundle::SubnetLeafIssuer>,
        channels: Vec<ChannelIssuer>,
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
            subnet: subnet.clone(),
            channels: channels.iter().map(|(_, i)| i.clone()).collect(),
            org: self.org.clone(),
        });
        // R2 phase 5: announce the direct address tokens name, so a device
        // that attached through the relay upgrades to it once it answers.
        // Only a hint: it never marks this node's NAT open, and a wrong one
        // fails an authenticated handshake while the relay keeps serving.
        if let Some(addr) = default_endpoint
            .as_ref()
            .and_then(|e| bundles.contact_addr(e))
        {
            let _ = mesh.node().set_direct_hint(Some(addr)).await;
        }
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
        // Subnet leaves expire; enrolled devices renew them here, and only
        // their own (the ledger names the device each invite was issued to).
        let renewal = match &subnet {
            Some(issuer) => Some(
                net_sdk::enrollment::renew::serve_subnet_renewal(
                    mesh.node(),
                    self.ledger.clone(),
                    issuer.clone(),
                )
                .map_err(|e| generic(format!("subnet renewal service: {e}")))?,
            ),
            None => None,
        };
        // Standalone subnet and org links are redeemed over the device's
        // session.
        let standalone = Some(
            net_sdk::enrollment::standalone::serve_standalone_redeem(
                mesh.node(),
                self.ledger.clone(),
                subnet.clone(),
                Some(self.org.stash.clone()),
                channels.iter().map(|(_, i)| i.clone()).collect(),
            )
            .map_err(|e| generic(format!("standalone redemption service: {e}")))?,
        );
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
            subnet,
            channels,
            channel_configs: mesh.node().channel_configs().cloned(),
            trust_domain,
            domain_name: self.plan.domain_name,
            bundles,
            org: self.org,
        });
        Ok(RunningEnrollment {
            service,
            context,
            tcp_mapping,
            relay,
            _renewal: renewal,
            _standalone: standalone,
            port_mapping: self.plan.port_mapping,
            created: self.created,
        })
    }
}

/// How long `up --enroll` waits for the first relay registration before it
/// reports readiness (it keeps retrying in the background either way). Room
/// for the UDP attempts (~3 s) and then the TCP tunnel fallback.
const RELAY_REGISTER_WAIT: Duration = Duration::from_secs(6);
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
    node: Arc<net::adapter::net::MeshNode>,
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
        let task = tokio::spawn(relay_loop(
            node.clone(),
            endpoint,
            sink,
            state.clone(),
            first_tx,
        ));
        let _ = tokio::time::timeout(RELAY_REGISTER_WAIT, first).await;
        Self {
            locator,
            state,
            node,
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

/// What a device's channel credential may delegate: nothing.
const CHANNEL_DELEGATION: &str = "none: the device's credential cannot delegate further";

fn channel_json(o: &net_sdk::enrollment::invite::ChannelOffer) -> Value {
    json!({
        "channel": o.channel.as_str(),
        "canonical_hash": format!("{:#018x}", o.channel.hash()),
        "root": hex::encode(o.root.as_bytes()),
        "rights": super::channel::format_channel_rights(o.rights),
    })
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
    /// Delegated subnet leaf issuance, when `up` runs with a subnet issuer.
    subnet: Option<net_sdk::enrollment::bundle::SubnetLeafIssuer>,
    /// Delegated channel chain issuance (`up --channel-grant`).
    channels: Vec<net_sdk::channel_issuer::ChannelLeafIssuer>,
    /// Operator-approved org membership certificates.
    org: Arc<OrgBook>,
}

/// Org membership state an enrolling node keeps beside its ledger: the
/// certificates the operator signed (with the offline org root) at approval,
/// per exact claim, and the org each org invite offers (the ledger keeps
/// digests only, and approval must check the operator signed for that org).
pub(crate) struct OrgBook {
    stash: Arc<net_sdk::enrollment::org::OrgCertStash>,
    offers: PathBuf,
    /// What each invite grants (relations and scopes), per offer: the ledger
    /// keeps digests only, and the member inventory needs the relation.
    relations: PathBuf,
}

impl OrgBook {
    fn new(dir: PathBuf) -> Self {
        Self {
            stash: Arc::new(net_sdk::enrollment::org::OrgCertStash::new(
                dir.join("certs"),
            )),
            offers: dir.join("offers"),
            relations: dir.join("relations"),
        }
    }

    /// Record (or amend) what `offer` grants. Written, then renamed.
    fn record_relations(
        &self,
        offer: &net_sdk::enrollment::store::OfferId,
        record: &Value,
    ) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.relations)?;
        let path = self.relations.join(format!("{offer}.json"));
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, record.to_string())?;
        std::fs::rename(&tmp, &path)
    }

    fn relations_of(&self, offer: &net_sdk::enrollment::store::OfferId) -> Option<Value> {
        let bytes = std::fs::read(self.relations.join(format!("{offer}.json"))).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    fn record_offer(
        &self,
        offer: &net_sdk::enrollment::store::OfferId,
        org: &net::adapter::net::behavior::org::OrgId,
    ) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.offers)?;
        let path = self.offers.join(offer.to_string());
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, hex::encode(org.0))?;
        std::fs::rename(&tmp, &path)
    }

    /// The org an offer's invite names, if it is an org invite.
    fn offered_org(
        &self,
        offer: &net_sdk::enrollment::store::OfferId,
    ) -> Option<net::adapter::net::behavior::org::OrgId> {
        let text = std::fs::read_to_string(self.offers.join(offer.to_string())).ok()?;
        let bytes: [u8; 32] = hex::decode(text.trim()).ok()?.try_into().ok()?;
        Some(net::adapter::net::behavior::org::OrgId(bytes))
    }
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
        let issuer = MembershipIssuer::new(self.issuer.clone(), self.psk.clone(), contact)
            .with_org_certs(self.org.stash.clone())
            .with_channel_issuers(self.channels.clone());
        match &self.subnet {
            Some(subnet) => issuer.with_subnet_issuer(subnet.clone()),
            None => issuer,
        }
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
    /// Serves subnet leaf renewal while this node issues subnet credentials.
    _renewal: Option<net::adapter::net::mesh_rpc::ServeHandle>,
    /// Serves standalone subnet redemption while this node issues subnet
    /// credentials.
    _standalone: Option<net::adapter::net::mesh_rpc::ServeHandle>,
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
    /// How the relay is reached once registered: `udp`, or `tcp` (the tunnel
    /// fallback, when UDP to the relay went unanswered).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    relay_transport: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    created: Vec<String>,
    /// The subnet this node verifies and issues for, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    subnet: Option<serde_json::Value>,
    /// The channels this node mints device chains for (`--channel-grant`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    channels: Vec<serde_json::Value>,
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
            relay_transport: self.relay.as_ref().and_then(|r| {
                let registered = r.state.lock().registered?;
                Some(
                    if r.node.relay_tunneled(registered) {
                        "tcp"
                    } else {
                        "udp"
                    }
                    .to_string(),
                )
            }),
            created: self.created.iter().map(|s| s.to_string()).collect(),
            subnet: c.subnet.as_ref().map(|s| {
                let g = s.grant();
                serde_json::json!({
                    "authority": hex::encode(g.authority.as_bytes()),
                    "issuer_scope": super::subnet::format_subnet(g.scope),
                    "max_rights": super::subnet::format_subnet_rights(g.maximum_rights),
                    "topology_epoch": g.topology_epoch,
                    "verifier": true,
                })
            }),
            channels: c
                .channels
                .iter()
                .map(|(name, issuer)| {
                    serde_json::json!({
                        "channel": name.as_str(),
                        "root": hex::encode(issuer.root().as_bytes()),
                        "rights": super::channel::format_channel_rights(issuer.grantable()),
                        "not_after": issuer.not_after(),
                    })
                })
                .collect(),
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

/// One channel grant this node issues from, with the channel it names.
pub(crate) type ChannelIssuer = (
    net::adapter::net::channel::ChannelName,
    net_sdk::channel_issuer::ChannelLeafIssuer,
);

/// State the node's control endpoint uses for `invite_*` operations.
pub(crate) struct EnrollContext {
    issuer: Identity,
    ledger: SharedLedger,
    key: EnrollmentKey,
    default_endpoint: Option<EnrollmentEndpoint>,
    relay: Option<RelayLocator>,
    subnet: Option<net_sdk::enrollment::bundle::SubnetLeafIssuer>,
    /// Channel grants this node mints device chains from.
    channels: Vec<ChannelIssuer>,
    /// The live node's channel registry (a subscribe invite needs the
    /// channel served here, trusting the grant's root).
    channel_configs: Option<Arc<net::adapter::net::channel::ChannelConfigRegistry>>,
    trust_domain: TrustDomainId,
    domain_name: String,
    bundles: Arc<NodeBundles>,
    org: Arc<OrgBook>,
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
            "invite_org_pending" => self.org_pending(request),
            "invite_org_approve" => self.org_approve(request),
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
        let standalone = request["standalone"] == Value::Bool(true);
        let org = match request["org"].as_str() {
            Some(hex) => Some(net_sdk::enrollment::invite::OrgOffer {
                org: parse_org_id(hex)?,
            }),
            None => None,
        };
        let standalone_relations = usize::from(org.is_some())
            + usize::from(request["subnet"].is_string())
            + usize::from(request["channel"].is_string());
        if standalone && standalone_relations > 1 {
            return Err(
                "a standalone link carries one relation: a subnet, an org or a channel".to_string(),
            );
        }
        // An org invite always waits for the operator: only the offline org
        // root can sign the membership, at approval (`org approve`).
        let mode = if org.is_some() {
            ApprovalMode::RequireApproval
        } else {
            mode
        };
        let (mut relations, subnet) = match request["subnet"].as_str() {
            None if standalone && (org.is_some() || request["channel"].is_string()) => {
                (Vec::new(), None)
            }
            None if standalone => {
                return Err("a standalone link needs a subnet, an org or a channel".to_string())
            }
            None => (vec![Relation::Mesh], None),
            Some(path) => {
                let issuer = self.subnet.as_ref().ok_or_else(|| {
                    "this node has no subnet issuer; start it with --subnet-issuer-grant and \
                     --subnet-issuer-key"
                        .to_string()
                })?;
                let path = super::subnet::parse_subnet_path(path).map_err(|e| e.to_string())?;
                let rights = super::subnet::parse_subnet_rights(
                    request["subnet_rights"].as_str().unwrap_or("attach"),
                )
                .map_err(|e| e.to_string())?;
                let grant = issuer.grant();
                let offer = net_sdk::enrollment::invite::SubnetOffer {
                    scope: net::adapter::net::subnet::SubnetRef {
                        authority: grant.authority.clone(),
                        path,
                    },
                    topology_epoch: grant.topology_epoch,
                    rights,
                };
                if !issuer.covers(&offer) {
                    return Err(format!(
                        "subnet {} with {} is outside this node's issuer grant ({} with at most {})",
                        super::subnet::format_subnet(path),
                        super::subnet::format_subnet_rights(rights),
                        super::subnet::format_subnet(grant.scope),
                        super::subnet::format_subnet_rights(grant.maximum_rights),
                    ));
                }
                if standalone {
                    // The subnet relation only: for a device already on the
                    // mesh, redeemed over its session (no PSK is delivered).
                    (vec![Relation::Subnet], Some(offer))
                } else {
                    (vec![Relation::Mesh, Relation::Subnet], Some(offer))
                }
            }
        };
        if org.is_some() {
            relations.push(Relation::Org);
        }
        // The link shape is {channel, root, rights}: lifetime is the grant's,
        // the leaf never delegates and the publisher is this node. Overrides
        // are refused, never ignored.
        for unsupported in ["channel_ttl", "channel_depth", "channel_publisher"] {
            if !request[unsupported].is_null() {
                return Err(format!(
                    "{unsupported} is not supported: a channel credential lives exactly as long                      as this node's grant, cannot delegate, and names this node as publisher"
                ));
            }
        }
        let channel_expiry = request["channel"]
            .as_str()
            .and_then(|n| net::adapter::net::channel::ChannelName::new(n).ok())
            .and_then(|n| {
                self.channels
                    .iter()
                    .find(|(c, _)| c == &n)
                    .map(|(_, i)| i.not_after())
            });
        let channel = match request["channel"].as_str() {
            None => None,
            // Standalone: the channel relation alone, for a device already on
            // the mesh (redeemed over its session); otherwise with mesh.
            Some(name) => {
                let offer = self.channel_offer(name, request["channel_rights"].as_str())?;
                relations.push(Relation::Channel);
                Some(offer)
            }
        };
        let policy = InvitationPolicy::with_options(now, ttl, mode).map_err(|e| e.to_string())?;
        let invite = MembershipInvite::sign(
            &self.issuer,
            InviteSpec {
                trust_domain_name: self.domain_name.clone(),
                trust_domain: self.trust_domain,
                endpoint: endpoint.clone(),
                relay: self.relay.clone(),
                enrollment_key: self.key,
                subnet: subnet.clone(),
                org,
                channel: channel.clone(),
                relations,
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
        if let Some(offer_org) = &org {
            self.org
                .record_offer(&offer, &offer_org.org)
                .map_err(|e| format!("org offer record: {e}"))?;
        }
        self.org
            .record_relations(
                &offer,
                &json!({
                    "relations": invite.relations().iter().map(|r| format!("{r:?}").to_lowercase()).collect::<Vec<_>>(),
                    "org": org.as_ref().map(|o| hex::encode(o.org.0)),
                    "subnet": subnet.as_ref().map(|o| json!({
                        "authority": hex::encode(o.scope.authority.as_bytes()),
                        "scope": super::subnet::format_subnet(o.scope.path),
                        "rights": super::subnet::format_subnet_rights(o.rights),
                        "topology_epoch": o.topology_epoch,
                    })),
                    "standalone": standalone,
                    "channel": channel.as_ref().map(channel_json),
                }),
            )
            .map_err(|e| format!("offer relation record: {e}"))?;
        Ok(json!({
            "token": invite.encode(),
            "offer_id": offer.to_string(),
            "expires_at": policy.expires_at(),
            "approval": approval_name(mode),
            "bearer": invite.is_bearer(),
            "endpoint": endpoint.as_ref().map(|e| e.as_str()),
            "relay": self.relay.as_ref().map(|r| r.endpoint.as_str()),
            "subnet": subnet.as_ref().map(|o| json!({
                "scope": super::subnet::format_subnet(o.scope.path),
                "rights": super::subnet::format_subnet_rights(o.rights),
            })),
            "standalone": standalone,
            "org": org.as_ref().map(|o| hex::encode(o.org.0)),
            "channel": channel.as_ref().map(|o| {
                let mut v = channel_json(o);
                // Redemption expiry is `expires_at` above; this is the
                // credential's, fixed by the grant.
                v["credential_expires_at"] = json!(channel_expiry);
                v["delegation"] = json!(CHANNEL_DELEGATION);
                v["publisher"] = json!(if o.rights.contains(net::adapter::net::identity::TokenScope::SUBSCRIBE) {
                    "this node (the issuer)"
                } else {
                    "none: publish is local to the device"
                });
                v
            }),
            "issuer_fingerprint": invite.issuer_fingerprint(),
        }))
    }

    /// The channel offer for `invite create --channel`: this node must hold a
    /// grant for exactly that canonical channel covering the rights, and a
    /// subscribe right needs the channel served HERE trusting the grant's
    /// root (this node is the publisher a subscribing device is sent to).
    /// A publish right installs nothing here: the device's own runtime must
    /// trust the root before it can publish.
    fn channel_offer(
        &self,
        name: &str,
        rights: Option<&str>,
    ) -> Result<net_sdk::enrollment::invite::ChannelOffer, String> {
        let channel = net::adapter::net::channel::ChannelName::new(name)
            .map_err(|e| format!("channel `{name}`: {e}"))?;
        let rights = super::channel::parse_channel_rights(
            rights.ok_or("--channel needs --channel-rights (publish and/or subscribe)")?,
        )
        .map_err(|e| e.to_string())?;
        let (_, issuer) = self
            .channels
            .iter()
            .find(|(n, _)| n == &channel)
            .ok_or_else(|| {
                format!(
                    "this node has no grant for channel {}; start it with --channel-grant \
                     (from `channel issue-grant`)",
                    channel.as_str()
                )
            })?;
        let offer = net_sdk::enrollment::invite::ChannelOffer {
            channel: channel.clone(),
            root: issuer.root().clone(),
            rights,
        };
        if !issuer.covers(&offer) {
            return Err(format!(
                "{} on {} is outside this node's grant (at most {})",
                super::channel::format_channel_rights(rights),
                channel.as_str(),
                super::channel::format_channel_rights(issuer.grantable()),
            ));
        }
        if rights.contains(net::adapter::net::identity::TokenScope::SUBSCRIBE) {
            let served = self.channel_configs.as_ref().is_some_and(|registry| {
                registry
                    .get_by_name(channel.as_str())
                    .is_some_and(|config| {
                        config.channel_id.name() == &channel
                            && config.token_roots.contains(issuer.root())
                    })
            });
            if !served {
                return Err(format!(
                    "a subscribe link needs {} served here trusting root {}; run \
                     `net-mesh channel serve {} --token-root {}` first",
                    channel.as_str(),
                    hex::encode(issuer.root().as_bytes()),
                    channel.as_str(),
                    hex::encode(issuer.root().as_bytes()),
                ));
            }
        }
        Ok(offer)
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
        if approve && self.org.offered_org(&offer).is_some() {
            return Err(
                "this invite offers an org membership, which only the org root can sign: \
                 approve it with `net-mesh org approve <offer> --subject <entity> --org-key <file>`"
                    .to_string(),
            );
        }
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

impl EnrollContext {
    /// `org approve`, step one: the pending claim and the org its invite
    /// offers, so the operator's CLI can sign for exactly that device.
    fn org_pending(&self, request: &Value) -> Result<Value, String> {
        let offer = parse_offer(request["offer_id"].as_str().unwrap_or_default())?;
        let org = self
            .org
            .offered_org(&offer)
            .ok_or_else(|| "that offer is not an org invite".to_string())?;
        let claim = self
            .ledger
            .lock()
            .pending_claim(&offer)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "offer has no claim awaiting approval".to_string())?;
        Ok(json!({
            "offer_id": offer.to_string(),
            "org": hex::encode(org.0),
            "subject": hex::encode(claim.subject.as_bytes()),
        }))
    }

    /// `org approve`, step two: keep the certificate the operator signed for
    /// exactly the pending claim, then approve that claim.
    fn org_approve(&self, request: &Value) -> Result<Value, String> {
        let offer = parse_offer(request["offer_id"].as_str().unwrap_or_default())?;
        let subject = parse_entity(request["subject"].as_str().unwrap_or_default())?;
        let cert = hex::decode(request["cert"].as_str().unwrap_or_default())
            .ok()
            .and_then(|b| net::adapter::net::behavior::org::OrgMembershipCert::from_bytes(&b).ok())
            .ok_or_else(|| "malformed membership certificate".to_string())?;
        let org = self
            .org
            .offered_org(&offer)
            .ok_or_else(|| "that offer is not an org invite".to_string())?;
        if cert.org_id != org {
            return Err(
                "the certificate is for a different org than the invite offers".to_string(),
            );
        }
        let mut ledger = self.ledger.lock();
        let claim = ledger
            .pending_claim(&offer)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "offer has no claim awaiting approval".to_string())?;
        if claim.subject != subject || cert.member != subject {
            return Err("the pending claim is from a different subject".to_string());
        }
        // The org's shared audience, if the operator supplied it.
        let audience = match request["audience"].as_str() {
            Some(hex_a) => {
                let bytes = hex::decode(hex_a).map_err(|_| "malformed org audience".to_string())?;
                Some(
                    net::adapter::net::behavior::org_authority::OwnerAudienceCredential::decode_config(&bytes)
                        .map_err(|e| format!("org audience: {e}"))?,
                )
            }
            None => None,
        };
        // Durable before the claim can be issued against it.
        self.org
            .stash
            .put(&claim, &cert)
            .map_err(|e| format!("keeping the certificate: {e}"))?;
        if let Some(audience) = &audience {
            self.org
                .stash
                .put_audience(&claim, &org, audience)
                .map_err(|e| format!("keeping the org audience: {e}"))?;
        }
        ledger
            .approve(&offer, &claim, now_unix())
            .map_err(|e| e.to_string())?;
        // The generation signed, for the inventory's standing check.
        if let Some(mut record) = self.org.relations_of(&offer) {
            record["approved_generation"] = json!(cert.generation);
            let _ = self.org.record_relations(&offer, &record);
        }
        Ok(json!({
            "offer_id": offer.to_string(),
            "state": "approved",
            "org": hex::encode(org.0),
            "member": hex::encode(subject.as_bytes()),
            "generation": cert.generation,
            "not_after": cert.not_after,
            "audience": audience.is_some(),
        }))
    }
}

impl EnrollContext {
    /// Issuer inventory: every offer this node created whose recorded
    /// relation matches `wanted` (`org` = hex org id; `subnet` = an
    /// authority-local dotted scope, matching that scope and everything
    /// inside it), with its ledger state and subject. Offers created before
    /// relation records existed are counted, not guessed.
    pub(crate) fn issued_inventory(&self, kind: &str, wanted: &str) -> Result<Value, String> {
        let statuses = self.ledger.lock().statuses().map_err(|e| e.to_string())?;
        let mut issued = Vec::new();
        let mut unrecorded = 0usize;
        for status in statuses {
            let Some(record) = self.org.relations_of(&status.offer_id) else {
                unrecorded += 1;
                continue;
            };
            let matches = match kind {
                "org" => record["org"].as_str() == Some(wanted),
                _ => record["subnet"]["scope"].as_str().is_some_and(|scope| {
                    scope == wanted || scope.starts_with(&format!("{wanted}."))
                }),
            };
            if !matches {
                continue;
            }
            use net_sdk::enrollment::store::OfferState as S;
            let (state, subject, issued_at) = match &status.state {
                S::Offered => ("offered", None, None),
                S::PendingApproval { subject } => ("pending_approval", Some(subject), None),
                S::Ready { subject } => ("approved", Some(subject), None),
                S::Issued {
                    subject, issued_at, ..
                } => ("issued", Some(subject), Some(*issued_at)),
                S::Revoked { subject } => ("revoked_offer", subject.as_ref(), None),
                S::Denied { subject } => ("denied", Some(subject), None),
            };
            issued.push(json!({
                "offer_id": status.offer_id.to_string(),
                "state": state,
                "subject": subject.map(|s| hex::encode(s.as_bytes())),
                "issued_at": issued_at,
                "relations": record["relations"],
                "subnet": record["subnet"],
                "approved_generation": record["approved_generation"],
                "standalone": record["standalone"],
            }));
        }
        Ok(json!({ "issued": issued, "unrecorded_offers": unrecorded }))
    }
}

pub(crate) fn parse_org_id(
    hex_id: &str,
) -> Result<net::adapter::net::behavior::org::OrgId, String> {
    let bytes: [u8; 32] = hex::decode(hex_id.trim().trim_start_matches("0x"))
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| "an org id is 64 hex characters".to_string())?;
    Ok(net::adapter::net::behavior::org::OrgId(bytes))
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
    /// Also attach the device to this subnet scope (dotted path), with a
    /// delegated credential issued at redemption. Needs `up` started with a
    /// subnet issuer whose grant covers it.
    #[arg(long, value_name = "PATH")]
    pub subnet: Option<String>,
    /// Rights for `--subnet` (default `attach`; others only when named).
    #[arg(long, value_name = "RIGHTS", requires = "subnet")]
    pub subnet_rights: Option<String>,
    /// Also make the device a member of this organization (its 64-hex org
    /// id). Always approval-gated: `org approve --org-key` signs the
    /// membership for exactly the claiming device.
    #[arg(long, value_name = "ORG")]
    pub org: Option<String>,
    /// Also give the device a credential on this canonical channel, minted
    /// at redemption from this node's `--channel-grant` for it.
    #[arg(long, value_name = "NAME", requires = "channel_rights")]
    pub channel: Option<String>,
    /// Rights for `--channel`: `publish`, `subscribe` or `publish,subscribe`.
    /// Subscribe sends the device to THIS node as the publisher, so the
    /// channel must be served here (`channel serve`) trusting the root.
    #[arg(long, value_name = "RIGHTS", requires = "channel")]
    pub channel_rights: Option<String>,
    /// A standalone link (`subnet invite` / `org invite`): one relation
    /// only, for a device already on the mesh.
    #[arg(skip)]
    pub standalone: bool,
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

pub(crate) async fn node_request(
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
            if let Some(subnet) = &args.subnet {
                super::subnet::parse_subnet_path(subnet)?;
                request["subnet"] = json!(subnet);
            }
            if let Some(rights) = &args.subnet_rights {
                super::subnet::parse_subnet_rights(rights)?;
                request["subnet_rights"] = json!(rights);
            }
            if args.standalone {
                request["standalone"] = json!(true);
            }
            if let Some(org) = &args.org {
                parse_org_id(org).map_err(invalid_args)?;
                request["org"] = json!(org.trim_start_matches("0x"));
            }
            if let (Some(channel), Some(rights)) = (&args.channel, &args.channel_rights) {
                super::channel::parse_channel_name(channel)?;
                super::channel::parse_channel_rights(rights)?;
                request["channel"] = json!(channel);
                request["channel_rights"] = json!(rights);
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
                "subnet": invite.subnet().map(|o| json!({
                    "authority": hex::encode(o.scope.authority.as_bytes()),
                    "scope": super::subnet::format_subnet(o.scope.path),
                    "rights": super::subnet::format_subnet_rights(o.rights),
                })),
                "channel": invite.channel().map(channel_json),
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
/// Pause between direct handshake attempts inside one attach budget.
const DIRECT_RETRY_PAUSE: Duration = Duration::from_millis(500);

/// Attach `mesh` to `contact` via the routed handshake: **direct first**, the
/// contact's blind relay only if the direct attempt fails. The session
/// authenticates the contact's key end-to-end on either path. Returns the path
/// used (`"direct"`, `"relay"`, or `"relay_tcp"` when the relay is reached
/// through its TCP tunnel because UDP to it went unanswered).
pub(crate) async fn attach_contact(
    node: &Arc<net::adapter::net::MeshNode>,
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
        // Keep trying within the budget: a peer that just restarted can be
        // refused for a few heartbeats while its old session still looks
        // busy at the other end (C3), which outlasts one handshake's retries.
        let direct_deadline = tokio::time::Instant::now() + budget;
        loop {
            let attempt = tokio::time::timeout_at(
                direct_deadline,
                node.connect_via(addr, &contact.noise_pubkey, contact.node_id),
            )
            .await;
            match attempt {
                Ok(Ok(_)) => return Ok("direct"),
                Ok(Err(e)) => direct_failure = Some(format!("direct {addr}: {e}")),
                Err(_) => {
                    direct_failure = Some(format!("direct {addr}: timed out"));
                    break;
                }
            }
            if direct_deadline.saturating_duration_since(tokio::time::Instant::now())
                < DIRECT_RETRY_PAUSE
            {
                break;
            }
            tokio::time::sleep(DIRECT_RETRY_PAUSE).await;
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
        let via = node
            .relay_bind(addr, relay.registration)
            .await
            .map_err(|e| e.to_string())?;
        node.connect_via_endpoint(via, &contact.noise_pubkey, contact.node_id)
            .await
            .map(|_| addr)
            .map_err(|e| e.to_string())
    })
    .await;
    let relay_failure = match relayed {
        // Over the relay's TCP tunnel when UDP to the relay went unanswered.
        Ok(Ok(addr)) if node.relay_tunneled(addr) => return Ok("relay_tcp"),
        Ok(Ok(_)) => return Ok("relay"),
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
    match attach_contact(mesh.node(), contact, wait).await {
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
    let channel_expires_at = bundle
        .channel_chain()
        .and_then(|chain| chain.tokens.last().map(|leaf| leaf.not_after));
    let enroll_path = join.last_path().map(|p| p.as_str());
    // An org relation: adopt the delivered membership as this node's owner
    // org (validated, one owner org per node, durable); `up` installs it.
    let authority_dir = state.join(super::lifecycle::AUTHORITY_SUBDIR);
    let org = match bundle.org_membership() {
        // Left: re-running the original token must not restore membership;
        // only a new link approved with the org root does.
        Some(_) if super::lifecycle::read_org_left(&authority_dir).is_some() => Some(json!({
            "state": "left",
            "detail": "this device left the org; a new approved org link (`org join`) rejoins",
        })),
        Some(cert) => {
            let (cert, entity, root, audience) = (
                cert.clone(),
                join.identity().entity_id().clone(),
                state.clone(),
                bundle.org_audience(),
            );
            let adopted = tokio::task::spawn_blocking(move || {
                super::lifecycle::adopt_org_membership(&root, cert, &entity, audience.as_ref())
            })
            .await
            .map_err(|e| generic(format!("org adoption task failed: {e}")))?
            .map_err(|e| {
                generic(format!(
                    "credentials are installed, but adopting the org membership failed: {e}"
                ))
            })?;
            Some(adopted)
        }
        None => None,
    };
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
            // Credentials installed; admission is proven by joined `up`.
            "subnet": invite.subnet().map(|o| json!({
                "scope": super::subnet::format_subnet(o.scope.path),
                "rights": super::subnet::format_subnet_rights(o.rights),
                "credentials": "installed",
            })),
            // Stored; the device's runtime uses it from `up` (subscribe
            // needs the publisher's ACK, publish needs local trust).
            "channel": invite.channel().map(|o| {
                let mut v = channel_json(o);
                v["credential"] = json!("stored");
                // The credential's own lifetime (the grant's), distinct from
                // when the invitation stopped being redeemable.
                v["credential_expires_at"] = json!(channel_expires_at);
                v["delegation"] = json!(CHANNEL_DELEGATION);
                v
            }),
            "org": org,
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
