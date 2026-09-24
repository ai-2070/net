//! `net-mesh up` / `net-mesh down` / `net-mesh node status` — one long-lived
//! production node per profile, owned by an explicit foreground process.
//!
//! # State directory
//!
//! Everything lives under one per-profile state directory (`--state-dir`,
//! default `<platform data dir>/net-mesh/nodes/<profile>`), in a protected
//! `node/` subdirectory created with the core's owner-only directory rules
//! (0700 / owner-only inheritable DACL):
//!
//! ```text
//! node/enrollment.snapshot   generated node identity seed + generated PSK
//! node/enrollment.lock       store lock, held by the running `up`
//! node/up.lock               lifetime lock, held by the running `up`; probed
//!                            by `down` / `node status` to decide liveness
//! node/control.json          per-run control endpoint: port, incarnation and
//!                            a fresh secret (removed on clean shutdown)
//! ```
//!
//! Liveness comes from the lifetime lock, never from a PID or a file's
//! presence: a control file without a held lock is reported as stale metadata.
//!
//! # Local control endpoint
//!
//! Loopback TCP on a random port. The per-run 32-byte secret is only in the
//! owner-only control file. Both sides prove knowledge of it with keyed BLAKE3
//! over fresh nonces from each side; the secret never crosses the socket, and a
//! process squatting the port cannot pass as the node. Each subsequent message
//! is encrypted with a keyed-BLAKE3 keystream and authenticated with a keyed
//! MAC over the ciphertext (direction and sequence bound), both under keys
//! derived from the secret and both nonces: `invite create` returns a bearer
//! join token over this channel.
//!
//! # Secrets
//!
//! The PSK comes from `--psk-from file:<path>` (32 raw bytes or 64 hex through
//! the secret-file gate), `--psk-from stdin` (piped only; a terminal is refused
//! because nothing here can disable echo), or — when omitted — a CSPRNG value
//! generated on first start and persisted before bind, then reused. It is never
//! accepted on argv or printed; output shows only its public trust-domain id.
//! `kms:` sources are not implemented in this build and are refused.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use clap::Args;
use net::adapter::net::behavior::enrollment_storage::{EnrollmentStorage, StorageError};
use net_sdk::bootstrap_credential::Psk;
use net_sdk::identity::Identity;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, Semaphore};

use crate::error::{connection_failure, generic, invalid_args, timeout, CliError};
use crate::prelude::{emit_stream_row, emit_value, OutputFormat};
use crate::secret::{zeroize_slice, ScrubbedBytes};

pub(crate) const NODE_SUBDIR: &str = "node";
const LOCK_FILE: &str = "up.lock";
const CONTROL_FILE: &str = "control.json";
const SECRETS_MAGIC: [u8; 4] = *b"NMUP";
const SECRETS_VERSION: u16 = 3;
const SECRETS_CHECKSUM: &str = "net-mesh up node secrets v1";
const CONTROL_MAGIC: [u8; 4] = *b"NMCT";
const MAX_CONTROL_FRAME: usize = 16 * 1024;
/// Client-side bound on an ordinary control exchange.
const CONTROL_SESSION_TIMEOUT: Duration = Duration::from_secs(5);
/// Server-side bound on one control session. Longer than the client's
/// ordinary bound so an operation that reaches another mesh node (a floor
/// readback forward) can finish; loopback-only, authenticated, and bounded
/// by the session permit pool.
const CONTROL_SERVE_TIMEOUT: Duration = Duration::from_secs(30);
/// Upper bound a caller may ask a forwarded floor readback to wait.
const MAX_FLOOR_FORWARD_WAIT: Duration = Duration::from_secs(20);
const CONTROL_MAX_SESSIONS: usize = 8;
const MESH_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);
/// Bound on a joined node's first attach. The direct path gets at most
/// `DIRECT_ATTACH_WAIT` of it when a relay is named; the rest leaves the relay
/// room for the operator's ~5 s defer of a re-attaching identity (its previous
/// session, e.g. from `join`, must lapse first).
const JOIN_ATTACH_WAIT: Duration = Duration::from_secs(20);
/// How long a leave waits for the publisher to acknowledge an unsubscribe:
/// inside the caller's control session bound, so a silent publisher yields
/// an unconfirmed result, not a lost reply.
const CHANNEL_UNSUBSCRIBE_WAIT: Duration = Duration::from_secs(3);
/// The durable record that this device left its join's own subnet relation
/// (under the state root); the mesh membership is untouched.
const SUBNET_LEFT_FILE: &str = "subnet.left";
/// Bound on one subnet leaf renewal exchange.
const SUBNET_RENEW_WAIT: Duration = Duration::from_secs(10);
/// Retry pause after a failed background renewal.
const SUBNET_RENEW_RETRY: Duration = Duration::from_secs(30);
/// Floor between background renewals, whatever the leaf lifetime.
const SUBNET_RENEW_MIN_INTERVAL: u64 = 5;

/// When to renew a subnet leaf: once a third of its lifetime remains. The
/// issuer back-dates `not_before` by up to a minute for clock skew; that
/// minute is not lifetime, or a short-lived leaf would renew continuously.
fn subnet_renew_at(set: &net::adapter::net::subnet::SubnetCredentialSet) -> u64 {
    let leaf = set.leaf();
    let lifetime = leaf.not_after.saturating_sub(leaf.not_before);
    let lifetime = match lifetime.saturating_sub(60) {
        0 => lifetime,
        forward => forward,
    };
    leaf.not_after.saturating_sub((lifetime / 3).max(1))
}

/// First retry pause after a failed (re-)attach; doubles up to the max.
const REATTACH_MIN: Duration = Duration::from_secs(1);
const REATTACH_MAX: Duration = Duration::from_secs(30);
/// How often the link supervisor looks at the session.
const LINK_CHECK: Duration = Duration::from_secs(1);

/// Live link of a joined node to the node it enrolled with, as `node
/// status` reports it. The start report is only the first attempt.
#[derive(Clone, Debug, Default, Serialize)]
struct JoinedLink {
    attached: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
    /// Successful attaches after the start attempt.
    reattaches: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    subnet_admitted: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    subnet_detail: Option<String>,
    /// Subnet presentations admitted by the supervisor (each new session,
    /// or a renewed leaf) — not counting the start presentation.
    readmissions: u64,
    /// Standalone subnet memberships (`subnet join`), keyed by membership.
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    standalone: std::collections::BTreeMap<String, StandaloneLink>,
    /// Why the last attempt to complete a pending org link failed, if it did.
    #[serde(skip_serializing_if = "Option::is_none")]
    org_detail: Option<String>,
}

/// One standalone subnet membership's live state.
#[derive(Clone, Debug, Default, Serialize)]
struct StandaloneLink {
    scope: String,
    rights: String,
    /// `installed` once credentials are held; `pending_approval` until then.
    state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    admitted: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    expires_at: Option<u64>,
}

type SharedJoin = Arc<parking_lot::Mutex<Option<net_sdk::enrollment::device::DeviceJoin>>>;

/// A joined device's channel credential and its live state.
#[derive(Clone)]
struct JoinedChannel {
    cred: super::channel_link::ChannelCred,
    link: super::channel_link::SharedChannel,
    /// Set once `channel leave` recorded the departure.
    left: Arc<AtomicBool>,
    /// The publisher (the node the device enrolled with).
    publisher: u64,
}
type SharedMemberships =
    Arc<parking_lot::Mutex<Vec<net_sdk::enrollment::standalone::SubnetMembership>>>;

/// Directory (under the state root) holding standalone subnet memberships.
const SUBNETS_SUBDIR: &str = "subnets";
/// This node's adopted org authority (under the state root).
pub(crate) const AUTHORITY_SUBDIR: &str = "authority";
/// Standalone org links awaiting operator approval (under the state root).
const ORGS_PENDING_SUBDIR: &str = "orgs-pending";
/// Clock skew accepted when adopting an org membership.
const ORG_ADOPT_SKEW_SECS: u64 = 60;

/// Adopt `cert` as this node's org membership in `<state>/authority`: the
/// org authority ceremony validates it (for this entity, in its window,
/// signed by its org root), keeps one owner org per node, and persists it.
/// Returns what was adopted.
pub(crate) fn adopt_org_membership(
    state_root: &Path,
    cert: net::adapter::net::behavior::org::OrgMembershipCert,
    entity: &net::adapter::net::identity::EntityId,
    audience: Option<&net::adapter::net::behavior::org_authority::OwnerAudienceCredential>,
) -> Result<serde_json::Value, String> {
    use net::adapter::net::behavior::org_authority::NodeAuthority;
    let dir = state_root.join(AUTHORITY_SUBDIR);
    let (org, generation, not_after) = (cert.org_id, cert.generation, cert.not_after);
    match audience {
        // The org's shared audience: this member can open (and be found in)
        // the org's private announcements.
        Some(audience) => NodeAuthority::adopt_with_audience(
            &dir,
            cert,
            entity,
            ORG_ADOPT_SKEW_SECS,
            None,
            audience,
        ),
        None => NodeAuthority::adopt(&dir, cert, entity, ORG_ADOPT_SKEW_SECS, None),
    }
    .map_err(|e| e.to_string())?;
    // A fresh, authorized adoption is the rejoin; it ends a recorded leave.
    clear_org_left(&dir);
    Ok(serde_json::json!({
        "org": hex::encode(org.0),
        "generation": generation,
        "not_after": not_after,
        "adopted": true,
        "audience": if audience.is_some() { "org" } else { "node-local (no private discovery with other members)" },
    }))
}

/// The durable record that this node LEFT its org (`org leave`), kept beside
/// the authority directory (`<authority>.left`) so the authority files —
/// revocation floors included — stay intact for a later authorized rejoin.
pub(crate) fn org_left_marker(authority_dir: &Path) -> PathBuf {
    authority_dir.with_extension("left")
}

/// The recorded departure, if this node left its org.
pub(crate) fn read_org_left(authority_dir: &Path) -> Option<serde_json::Value> {
    let bytes = std::fs::read(org_left_marker(authority_dir)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Record the departure durably (written, synced, renamed into place).
fn record_org_left(authority_dir: &Path, org: &str, at: u64) -> std::io::Result<()> {
    use std::io::Write as _;
    let path = org_left_marker(authority_dir);
    let tmp = path.with_extension("left-tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(
            serde_json::json!({ "org": org, "left_at": at })
                .to_string()
                .as_bytes(),
        )?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, &path)
}

/// A fresh, authorized adoption ends a recorded departure.
pub(crate) fn clear_org_left(authority_dir: &Path) {
    let _ = std::fs::remove_file(org_left_marker(authority_dir));
}

/// The org an authority directory's membership names (hex), read from its
/// membership file without loading the authority.
fn owner_org_on_disk(authority_dir: &Path) -> Option<String> {
    let bytes = std::fs::read(authority_dir.join("owner-membership.json")).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    v["owner_org"].as_str().map(str::to_string)
}

/// The recorded departure from the join's own subnet relation, if any.
fn read_subnet_left(state_root: &Path) -> Option<serde_json::Value> {
    let bytes = std::fs::read(state_root.join(SUBNET_LEFT_FILE)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn record_subnet_left(state_root: &Path, scope: &str, at: u64) -> std::io::Result<()> {
    use std::io::Write as _;
    let path = state_root.join(SUBNET_LEFT_FILE);
    let tmp = path.with_extension("left-tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(
            serde_json::json!({ "scope": scope, "left_at": at })
                .to_string()
                .as_bytes(),
        )?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, &path)
}

/// Control op `subnet_leave`: leave one subnet relation of this device —
/// the join's own, or a standalone membership — at the named scope.
/// Recorded durably first (and fenced: never presented or renewed again),
/// then the verifier is asked over the session to drop this device's
/// admission there. The credential itself is not revoked.
async fn subnet_leave(state: &ControlState, request: &serde_json::Value) -> serde_json::Value {
    let Ok(path) = super::subnet::parse_subnet_path(request["scope"].as_str().unwrap_or_default())
    else {
        return serde_json::json!({ "error": "malformed scope" });
    };
    let now = now_unix();
    let scope_text = super::subnet::format_subnet(path);
    // (target at the verifier, verifier node, newly left, relation kind)
    let mut found = None;
    if let Some(join) = &state.joined {
        let guard = join.lock();
        if let Some(j) = guard.as_ref().filter(|j| j.left_at().is_none()) {
            if let (Some(offer), Some(bundle)) = (j.invite().subnet(), j.bundle()) {
                if offer.scope.path == path {
                    let newly = read_subnet_left(&state.state_root).is_none();
                    if newly {
                        // Recorded and fenced under the join lock, so a
                        // renewal cannot slip in between.
                        if let Err(e) = record_subnet_left(&state.state_root, &scope_text, now) {
                            return serde_json::json!({
                                "error": format!("the departure was not recorded: {e}")
                            });
                        }
                        state.subnet_left.store(true, Ordering::SeqCst);
                    }
                    found = Some((offer.scope.clone(), bundle.contact().node_id, newly, "join"));
                }
            }
        }
    }
    if found.is_none() {
        if let Some(memberships) = &state.memberships {
            let mut ms = memberships.lock();
            if let Some(m) = ms
                .iter_mut()
                .find(|m| m.offer().is_some_and(|o| o.scope.path == path))
            {
                let target = m.offer().map(|o| o.scope.clone());
                let newly = match m.leave(now) {
                    Ok(newly) => newly,
                    Err(e) => {
                        return serde_json::json!({
                            "error": format!("the departure was not recorded: {e}")
                        })
                    }
                };
                if let Some(target) = target {
                    found = Some((target, m.issuer_node(), newly, "standalone"));
                }
            }
        }
    }
    let Some((target, verifier, newly, relation)) = found else {
        return serde_json::json!({
            "error": format!("this device holds no subnet relation at {scope_text}")
        });
    };
    if let Some(link) = &state.link {
        let mut l = link.lock();
        if relation == "join" {
            l.subnet_admitted = None;
            l.subnet_detail = Some("left".to_string());
        }
        for entry in l.standalone.values_mut().filter(|e| e.scope == scope_text) {
            entry.state = "left".to_string();
            entry.admitted = None;
        }
    }
    let withdrawn = state
        .node
        .withdraw_own_subnet_admission(verifier, &target, CHANNEL_UNSUBSCRIBE_WAIT)
        .await;
    serde_json::json!({
        "left": true,
        "newly_left": newly,
        "scope": scope_text,
        "relation": relation,
        // Confirmed only on the verifier's acknowledgement that no
        // admission of this device remains there on this session.
        "withdrawal": if withdrawn.is_ok() { "confirmed" } else { "unconfirmed" },
        "withdrawal_detail": withdrawn.as_ref().err().map(|e| e.to_string()),
        "dropped": withdrawn.ok(),
        "credentials": if relation == "join" { "disabled: kept in the join, never presented or renewed" } else { "erased" },
        "credential_validity": "unchanged: valid until it expires or the operator runs `subnet remove`",
    })
}

/// Pending standalone org links, as stored under `<state>/orgs-pending`.
type PendingOrgs =
    Arc<parking_lot::Mutex<Vec<(String, net_sdk::enrollment::invite::MembershipInvite)>>>;

fn load_pending_orgs(dir: &Path) -> Vec<(String, net_sdk::enrollment::invite::MembershipInvite)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let key = e.file_name().to_string_lossy().to_string();
            let bytes = std::fs::read(e.path()).ok()?;
            let invite = net_sdk::enrollment::invite::MembershipInvite::from_bytes(&bytes).ok()?;
            Some((key, invite))
        })
        .collect()
}

/// A standalone org link was issued: adopt the membership and install it
/// on the running node (one owner org; a same-org renewal replaces).
fn adopt_and_install_org(
    state_root: &Path,
    node: &Arc<net::adapter::net::MeshNode>,
    cert: net::adapter::net::behavior::org::OrgMembershipCert,
    audience: Option<Vec<u8>>,
) -> Result<serde_json::Value, String> {
    let audience = match audience {
        Some(bytes) => Some(
            net::adapter::net::behavior::org_authority::OwnerAudienceCredential::decode_config(
                &bytes,
            )
            .map_err(|e| format!("org audience: {e}"))?,
        ),
        None => None,
    };
    let adopted = adopt_org_membership(state_root, cert, node.entity_id(), audience.as_ref())?;
    net_sdk::org::install_org_authority_node(node, &state_root.join(AUTHORITY_SUBDIR))
        .map_err(|e| format!("installing the org authority: {e}"))?;
    Ok(adopted)
}

/// A membership's directory name and status key: a one-way digest of the
/// signed link (the invitation id itself is treated as sensitive).
fn membership_key(invite: &net_sdk::enrollment::invite::MembershipInvite) -> String {
    let key = blake3::derive_key(
        "net-mesh standalone subnet membership dir v1",
        &invite.digest(),
    );
    hex::encode(&key[..12])
}

fn standalone_entry(
    offer: &net_sdk::enrollment::invite::SubnetOffer,
    state: &str,
) -> StandaloneLink {
    StandaloneLink {
        scope: super::subnet::format_subnet(offer.scope.path),
        rights: super::subnet::format_subnet_rights(offer.rights),
        state: state.to_string(),
        ..Default::default()
    }
}

/// Open every stored standalone membership (fail closed on a corrupt one).
fn load_memberships(
    dir: &Path,
) -> Result<Vec<net_sdk::enrollment::standalone::SubnetMembership>, CliError> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => {
            return Err(generic(format!(
                "subnet memberships {}: {e}",
                dir.display()
            )))
        }
    };
    let mut out = Vec::new();
    for entry in entries {
        let path = entry
            .map_err(|e| generic(format!("subnet memberships {}: {e}", dir.display())))?
            .path();
        if !path.is_dir() {
            continue;
        }
        let membership = net_sdk::enrollment::standalone::SubnetMembership::open(&path)
            .map_err(|e| generic(format!("subnet membership {}: {e}", path.display())))?;
        out.push(membership);
    }
    Ok(out)
}

/// Per-credential bookkeeping for [`keep_subnet_admitted`].
#[derive(Default)]
struct SubnetTrack {
    /// The session these credentials were last admitted on.
    presented: Option<u64>,
    present_retry_at: u64,
    renew_retry_at: u64,
}

/// What [`keep_subnet_admitted`] did.
enum Kept {
    /// The owner of the credentials is gone (shut down or left).
    Gone,
    /// Nothing was due.
    Quiet,
    /// Credentials were presented; the verifier's verdict.
    Presented(Result<(), String>),
}

/// Keep one subnet credential admitted on `session` with `issuer_node`:
/// renew it when due (persisting through `persist`, which re-checks it
/// against the signed offer), and present it when it has not been admitted
/// on this session yet (admission binds to the session).
#[allow(clippy::too_many_arguments)]
async fn keep_subnet_admitted(
    node: &Arc<net::adapter::net::MeshNode>,
    issuer_node: u64,
    session: u64,
    identity: &net_sdk::identity::Identity,
    invite: &net_sdk::enrollment::invite::MembershipInvite,
    offer: &net_sdk::enrollment::invite::SubnetOffer,
    mut set: net::adapter::net::subnet::SubnetCredentialSet,
    track: &mut SubnetTrack,
    persist: impl FnOnce(
        &net::adapter::net::subnet::SubnetCredentialSet,
    ) -> Option<Result<(), net_sdk::enrollment::device::DeviceJoinError>>,
) -> Kept {
    let now = now_unix();
    let mut fresh = false;
    if now >= subnet_renew_at(&set) && now >= track.renew_retry_at {
        track.renew_retry_at = now + SUBNET_RENEW_MIN_INTERVAL;
        match net_sdk::enrollment::renew::request_subnet_renewal(
            node,
            issuer_node,
            identity,
            invite,
            SUBNET_RENEW_WAIT,
        )
        .await
        {
            Ok(renewed) => match persist(&renewed) {
                Some(Ok(())) => {
                    set = renewed;
                    fresh = true;
                }
                Some(Err(e)) => {
                    tracing::warn!(error = %e, "renewed subnet credentials refused");
                    track.renew_retry_at = now + SUBNET_RENEW_RETRY.as_secs();
                }
                None => return Kept::Gone,
            },
            Err(e) => {
                tracing::warn!(error = %e, "subnet leaf renewal failed; retrying");
                track.renew_retry_at = now + SUBNET_RENEW_RETRY.as_secs();
            }
        }
    }
    if fresh || (track.presented != Some(session) && now >= track.present_retry_at) {
        let admitted = node
            .present_subnet_credentials(
                issuer_node,
                &set,
                offer.scope.clone(),
                offer.rights,
                JOIN_ATTACH_WAIT,
            )
            .await;
        return Kept::Presented(match admitted {
            Ok(_) => {
                track.presented = Some(session);
                Ok(())
            }
            Err(e) => {
                track.presented = None;
                track.present_retry_at = now_unix() + SUBNET_RENEW_RETRY.as_secs();
                Err(e.to_string())
            }
        });
    }
    Kept::Quiet
}

/// Keep every standalone membership redeemed at `issuer_node` admitted on
/// `session`: ask again for a pending one, renew and present an installed
/// one. Reported per membership in `link.standalone`.
#[allow(clippy::too_many_arguments)]
async fn keep_standalone_admitted(
    node: &Arc<net::adapter::net::MeshNode>,
    issuer_node: u64,
    session: u64,
    identity: &net_sdk::identity::Identity,
    memberships: &SharedMemberships,
    tracks: &mut std::collections::HashMap<String, SubnetTrack>,
    link: &Arc<parking_lot::Mutex<JoinedLink>>,
) {
    use net_sdk::enrollment::standalone::SubnetRedeemReply;
    let snapshot: Vec<_> = memberships
        .lock()
        .iter()
        .filter(|m| m.left_at().is_none() && m.issuer_node() == issuer_node)
        .filter_map(|m| {
            Some((
                membership_key(m.invite()),
                m.invite().clone(),
                m.offer()?.clone(),
                m.credentials().cloned(),
            ))
        })
        .collect();
    for (key, invite, offer, credentials) in snapshot {
        let track = tracks.entry(key.clone()).or_default();
        let install = |set: &net::adapter::net::subnet::SubnetCredentialSet| {
            memberships
                .lock()
                .iter_mut()
                .find(|m| membership_key(m.invite()) == key)
                .map(|m| m.install(set))
        };
        match credentials {
            // Approval-gated and not yet issued: ask again now and then.
            None => {
                let now = now_unix();
                if now < track.renew_retry_at {
                    continue;
                }
                track.renew_retry_at = now + SUBNET_RENEW_RETRY.as_secs();
                let reply = net_sdk::enrollment::standalone::request_subnet_redeem(
                    node,
                    issuer_node,
                    identity,
                    &invite,
                    SUBNET_RENEW_WAIT,
                )
                .await;
                let mut entry = standalone_entry(&offer, "pending_approval");
                match reply {
                    Ok(SubnetRedeemReply::Issued(set)) => match install(&set) {
                        Some(Ok(())) => {
                            // Present on the next pass.
                            track.renew_retry_at = 0;
                            entry.state = "installed".to_string();
                            entry.expires_at = Some(set.leaf().not_after);
                        }
                        Some(Err(e)) => entry.detail = Some(e.to_string()),
                        None => continue,
                    },
                    Ok(SubnetRedeemReply::PendingApproval) => {
                        entry.detail = Some("awaiting operator approval".to_string());
                    }
                    Ok(SubnetRedeemReply::OrgIssued { .. }) => {
                        entry.detail = Some("the node answered with an org membership".to_string());
                    }
                    Err(e) => entry.detail = Some(e),
                }
                link.lock().standalone.insert(key, entry);
            }
            Some(set) => {
                let kept = keep_subnet_admitted(
                    node,
                    issuer_node,
                    session,
                    identity,
                    &invite,
                    &offer,
                    set,
                    track,
                    install,
                )
                .await;
                let expires_at = memberships
                    .lock()
                    .iter()
                    .find(|m| membership_key(m.invite()) == key)
                    .and_then(|m| m.credentials().map(|c| c.leaf().not_after));
                let mut l = link.lock();
                let entry = l
                    .standalone
                    .entry(key.clone())
                    .or_insert_with(|| standalone_entry(&offer, "installed"));
                entry.state = "installed".to_string();
                entry.expires_at = expires_at;
                if let Kept::Presented(verdict) = kept {
                    entry.admitted = Some(verdict.is_ok());
                    entry.detail = verdict.err();
                }
            }
        }
    }
}

/// Keep a joined node attached and, for a subnet join, admitted:
/// - no live session with the enrolling node (never attached, or the
///   old session went silent, e.g. after that node restarted):
///   attach again, direct first
///   then relay, backing off `REATTACH_MIN`..`REATTACH_MAX`;
/// - a session the subnet credentials were not presented on (admission
///   binds to the session): present them;
/// - a leaf near expiry: renew it from the issuing node, persist it through
///   the join store (which re-checks it against the signed offer) and
///   present it.
///
/// Stops when the join is gone (shutdown) or has left: `leave` erases the
/// bundle, so there is nothing to attach with.
#[allow(clippy::too_many_arguments)]
fn spawn_joined_link(
    joined: SharedJoin,
    memberships: SharedMemberships,
    pending_orgs: PendingOrgs,
    state_root: PathBuf,
    node: Arc<net::adapter::net::MeshNode>,
    link: Arc<parking_lot::Mutex<JoinedLink>>,
    presented: Option<u64>,
    present_retry_at: u64,
    channel: Option<(JoinedChannel, Option<u64>)>,
    subnet_left: Arc<AtomicBool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut backoff = REATTACH_MIN;
        let mut channel = channel.map(|(c, on)| {
            let track = super::channel_link::ChannelTrack::new(on, c.left.clone());
            (c, track)
        });
        let mut track = SubnetTrack {
            presented,
            present_retry_at,
            renew_retry_at: 0,
        };
        let mut standalone_tracks = std::collections::HashMap::<String, SubnetTrack>::new();
        let mut org_retry_at = 0u64;
        loop {
            let snapshot = {
                let guard = joined.lock();
                guard
                    .as_ref()
                    .filter(|j| j.left_at().is_none())
                    .and_then(|j| {
                        let bundle = j.bundle()?;
                        // A left subnet relation is never presented or
                        // renewed again.
                        let subnet = j
                            .invite()
                            .subnet()
                            .cloned()
                            .zip(bundle.subnet_credentials())
                            .filter(|_| !subnet_left.load(Ordering::SeqCst));
                        Some((
                            bundle.contact().clone(),
                            j.identity().clone(),
                            j.invite().clone(),
                            subnet,
                        ))
                    })
            };
            let Some((contact, identity, invite, subnet)) = snapshot else {
                return;
            };
            // A dead peer keeps its table entry for a long while (so a
            // healed partition recovers without a handshake); a session
            // silent past the timeout is what says this link is down.
            let session = node
                .peer_session_id(contact.node_id)
                .filter(|_| !node.peer_session_is_silent(contact.node_id));
            let Some(session) = session else {
                {
                    // Admission is per session: none holds without one.
                    let mut l = link.lock();
                    l.attached = false;
                    if l.subnet_admitted.is_some() {
                        l.subnet_admitted = None;
                    }
                    for entry in l.standalone.values_mut() {
                        entry.admitted = None;
                    }
                }
                if let Some((c, track)) = channel.as_mut() {
                    super::channel_link::lost_session(track, &c.link);
                }
                match super::enrollment::attach_contact(&node, &contact, JOIN_ATTACH_WAIT).await {
                    Ok(path) => {
                        let mut l = link.lock();
                        l.attached = true;
                        l.path = Some(path.to_string());
                        l.detail = None;
                        l.reattaches += 1;
                        backoff = REATTACH_MIN;
                        track.presented = None;
                        track.present_retry_at = 0;
                        for t in standalone_tracks.values_mut() {
                            t.presented = None;
                            t.present_retry_at = 0;
                        }
                    }
                    Err(e) => {
                        link.lock().detail = Some(format!("attach failed: {e}"));
                        tokio::time::sleep(backoff).await;
                        backoff = (backoff * 2).min(REATTACH_MAX);
                    }
                }
                continue;
            };
            {
                let mut l = link.lock();
                if !l.attached {
                    l.attached = true;
                    l.detail = None;
                }
            }
            if let Some((offer, set)) = subnet {
                let kept = keep_subnet_admitted(
                    &node,
                    contact.node_id,
                    session,
                    &identity,
                    &invite,
                    &offer,
                    set,
                    &mut track,
                    |renewed| {
                        // A renewal completing after `subnet leave` (which
                        // records under this same lock) installs nothing.
                        let mut guard = joined.lock();
                        let join = guard.as_mut()?;
                        if subnet_left.load(Ordering::SeqCst) {
                            return Some(Err(net_sdk::enrollment::device::DeviceJoinError::Left {
                                at: now_unix(),
                            }));
                        }
                        Some(join.replace_subnet_credentials(renewed))
                    },
                )
                .await;
                // A presentation that raced `subnet leave` is withdrawn
                // again rather than left standing.
                if matches!(kept, Kept::Presented(Ok(()))) && subnet_left.load(Ordering::SeqCst) {
                    let _ = node
                        .withdraw_own_subnet_admission(
                            contact.node_id,
                            &offer.scope,
                            CHANNEL_UNSUBSCRIBE_WAIT,
                        )
                        .await;
                    continue;
                }
                match kept {
                    Kept::Gone => return,
                    Kept::Quiet => {}
                    Kept::Presented(verdict) => {
                        let mut l = link.lock();
                        l.subnet_admitted = Some(verdict.is_ok());
                        match verdict {
                            Ok(()) => {
                                l.subnet_detail = None;
                                l.readmissions += 1;
                            }
                            Err(e) => l.subnet_detail = Some(e),
                        }
                    }
                }
            }
            keep_standalone_admitted(
                &node,
                contact.node_id,
                session,
                &identity,
                &memberships,
                &mut standalone_tracks,
                &link,
            )
            .await;
            if let Some((c, track)) = channel.as_mut() {
                super::channel_link::keep(
                    &node,
                    contact.node_id,
                    session,
                    &c.cred,
                    track,
                    &c.link,
                    JOIN_ATTACH_WAIT,
                )
                .await;
            }
            if now_unix() >= org_retry_at && !pending_orgs.lock().is_empty() {
                org_retry_at = now_unix() + SUBNET_RENEW_RETRY.as_secs();
                keep_pending_orgs(
                    &node,
                    contact.node_id,
                    &identity,
                    &state_root,
                    &pending_orgs,
                    &link,
                )
                .await;
            }
            tokio::time::sleep(LINK_CHECK).await;
        }
    })
}

/// `net-mesh up` arguments.
#[derive(Args, Debug)]
pub struct UpArgs {
    /// Node state directory: identity, generated PSK, lifetime lock and control
    /// endpoint. Defaults to `<platform data dir>/net-mesh/nodes/<profile>`.
    #[arg(long, value_name = "DIR")]
    pub state_dir: Option<PathBuf>,

    /// Mesh bind address (IP:port). Defaults to the profile `bind`, else
    /// `0.0.0.0:0`.
    #[arg(long, value_name = "ADDR")]
    pub bind: Option<String>,

    /// PSK source: `file:<path>` (32 raw bytes or 64 hex characters) or
    /// `stdin` (piped, not a terminal). Omit to generate one on first start
    /// and reuse it. Never a literal PSK.
    #[arg(long, value_name = "SOURCE")]
    pub psk_from: Option<String>,

    /// Node identity file. Defaults to the profile `identity`, else an identity
    /// generated on first start and kept in the state directory.
    #[arg(long, value_name = "PATH")]
    pub identity: Option<PathBuf>,

    /// Make this node the enrollment owner: serve join-token redemption and
    /// accept `net-mesh invite` operations. On first run it creates and keeps
    /// an issuer key, a ledger and a fixed port (explicit flags override), and
    /// asks the router to forward that port (UPnP / NAT-PMP / PCP) unless
    /// `--no-port-mapping` is given.
    #[arg(long)]
    pub enroll: bool,

    /// Address joiners reach this node at (`host:port`), signed into join
    /// tokens by default. Optional: without it the router-mapped address, or a
    /// concrete bind address, is used. The same port number must reach the node
    /// over TCP (enrollment) and UDP (mesh).
    #[arg(long, value_name = "HOST:PORT", requires = "enroll")]
    pub public_addr: Option<String>,

    /// Issuer identity file that signs invitations and membership receipts.
    /// Defaults to an issuer key created once and kept in the state directory.
    #[arg(long, value_name = "PATH", requires = "enroll")]
    pub issuer_identity: Option<PathBuf>,

    /// Enrollment ledger directory (must exist). Defaults to
    /// `<state-dir>/ledger`, created on first `--enroll` run.
    #[arg(long, value_name = "DIR", requires = "enroll")]
    pub ledger: Option<PathBuf>,

    /// Do not ask the router to forward the enrollment/mesh port. Tokens then
    /// need `--public-addr`, a concrete `--bind` address or `invite create --addr`.
    #[arg(long, requires = "enroll")]
    pub no_port_mapping: bool,

    /// Trust-domain label shown to joiners (`[A-Za-z0-9._-]`, at most 64).
    /// Defaults to the profile name.
    #[arg(long, value_name = "NAME", requires = "enroll")]
    pub domain_name: Option<String>,

    /// Blind relay (`host:port`) to register with, so joiners that cannot
    /// reach this node directly fall back to it; tokens then carry both paths
    /// and joiners try the direct one first. Defaults to the profile `relay`.
    #[arg(long, value_name = "HOST:PORT", requires = "enroll")]
    pub relay: Option<String>,

    /// Do not register with any relay (overrides the profile and default).
    #[arg(long, requires = "enroll", conflicts_with = "relay")]
    pub no_relay: bool,

    /// Root-signed subnet issuer grant (from `subnet issue-issuer`). With
    /// `--subnet-issuer-key`, this node verifies subnet admission for the
    /// grant's authority (floors persisted, readback served) and issues
    /// delegated subnet credentials to devices joining with a subnet invite.
    /// The subnet root key never sits on this node.
    #[arg(long, value_name = "PATH", requires_all = ["enroll", "subnet_issuer_key"])]
    pub subnet_issuer_grant: Option<PathBuf>,

    /// The issuer key file named by `--subnet-issuer-grant`.
    #[arg(long, value_name = "PATH", requires = "subnet_issuer_grant")]
    pub subnet_issuer_key: Option<PathBuf>,

    /// Lifetime of each delegated subnet credential (never beyond the grant).
    #[arg(long, value_name = "DURATION", default_value = "24h", value_parser = crate::humantime::parse_duration)]
    pub subnet_leaf_ttl: Duration,

    /// Generation stamped on delegated subnet credentials.
    #[arg(long, default_value_t = 1)]
    pub subnet_generation: u32,

    /// Root-signed channel grant (from `channel issue-grant`) naming this
    /// node's enrollment issuer; repeat for several channels. Devices joining with a
    /// `--channel` invite receive a chain `root → this node → device`
    /// minted from it. The channel root never sits on this node.
    #[arg(long = "channel-grant", value_name = "PATH", requires = "enroll")]
    pub channel_grants: Vec<PathBuf>,
}

/// `net-mesh down` arguments.
#[derive(Args, Debug)]
pub struct DownArgs {
    /// Node state directory (as given to `net-mesh up`).
    #[arg(long, value_name = "DIR")]
    pub state_dir: Option<PathBuf>,

    /// How long to wait for the node to release its lifetime lock after it
    /// accepts the shutdown request.
    #[arg(long, value_name = "DURATION", default_value = "15s", value_parser = crate::humantime::parse_duration)]
    pub wait: Duration,
}

/// `net-mesh leave` arguments.
#[derive(Args, Debug)]
pub struct LeaveArgs {
    /// State directory of the joined device (as given to `join` and `up`).
    #[arg(long, value_name = "DIR")]
    pub state_dir: Option<PathBuf>,

    /// How long to wait for a running joined node to release its lifetime
    /// lock after it records the departure.
    #[arg(long, value_name = "DURATION", default_value = "15s", value_parser = crate::humantime::parse_duration)]
    pub wait: Duration,
}

/// `net-mesh node status` arguments.
#[derive(Args, Debug)]
pub struct StatusArgs {
    /// Node state directory (as given to `net-mesh up`).
    #[arg(long, value_name = "DIR")]
    pub state_dir: Option<PathBuf>,
}

pub(crate) fn state_dir(explicit: Option<PathBuf>, profile: &str) -> Result<PathBuf, CliError> {
    if let Some(dir) = explicit {
        return Ok(dir);
    }
    let usable = !profile.is_empty()
        && !profile.starts_with('.')
        && profile
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b));
    if !usable {
        return Err(invalid_args(
            "profile name is not usable as a directory name; pass --state-dir",
        ));
    }
    dirs::data_local_dir()
        .map(|d| d.join("net-mesh").join("nodes").join(profile))
        .ok_or_else(|| invalid_args("no platform data directory is available; pass --state-dir"))
}

pub(crate) fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub(crate) fn random<const N: usize>() -> Result<[u8; N], CliError> {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).map_err(|_| generic("operating-system CSPRNG unavailable"))?;
    Ok(b)
}

// ---- node secrets (identity seed, PSK, issuer seed, Noise key) -------------

pub(crate) struct NodeSecrets {
    seed: [u8; 32],
    psk: Option<[u8; 32]>,
    /// Auto-provisioned enrollment issuer seed (`up --enroll` without
    /// `--issuer-identity`). Distinct from the node identity.
    pub(crate) issuer: Option<[u8; 32]>,
    /// Fixed enrollment port chosen on first `up --enroll` with port 0.
    pub(crate) enroll_port: Option<u16>,
    /// The node's Noise static private key, generated on first start and
    /// reused: invites and bundles pin its public half, so a fresh key per
    /// start would strand every node that enrolled here.
    noise: Option<[u8; 32]>,
}

impl Drop for NodeSecrets {
    fn drop(&mut self) {
        zeroize_slice(&mut self.seed);
        if let Some(psk) = self.psk.as_mut() {
            zeroize_slice(psk);
        }
        if let Some(issuer) = self.issuer.as_mut() {
            zeroize_slice(issuer);
        }
        if let Some(noise) = self.noise.as_mut() {
            zeroize_slice(noise);
        }
    }
}

// v3: MAGIC | u16 3 | seed[32] | u8 has_psk [32] | u8 has_issuer [32] |
//     u16 enroll_port (0 = none) | u8 has_noise [32] | blake3 checksum[32].
// v2 (still read): v3 without the Noise key.
// v1 (still read): MAGIC | u16 1 | seed[32] | u8 has_psk [32] | checksum[32].
impl NodeSecrets {
    fn encode(&self) -> ScrubbedBytes {
        let mut out = Vec::with_capacity(4 + 2 + 32 + 33 + 33 + 2 + 33 + 32);
        out.extend_from_slice(&SECRETS_MAGIC);
        out.extend_from_slice(&SECRETS_VERSION.to_le_bytes());
        out.extend_from_slice(&self.seed);
        for field in [&self.psk, &self.issuer] {
            match field {
                Some(bytes) => {
                    out.push(1);
                    out.extend_from_slice(bytes);
                }
                None => out.push(0),
            }
        }
        out.extend_from_slice(&self.enroll_port.unwrap_or(0).to_le_bytes());
        match &self.noise {
            Some(bytes) => {
                out.push(1);
                out.extend_from_slice(bytes);
            }
            None => out.push(0),
        }
        let sum = blake3::derive_key(SECRETS_CHECKSUM, &out);
        out.extend_from_slice(&sum);
        ScrubbedBytes::new(out)
    }

    fn decode(bytes: &[u8]) -> Result<Self, CliError> {
        let corrupt = || {
            generic("node state is corrupt or from an unsupported version; refusing to replace it")
        };
        let body_len = bytes.len().checked_sub(32).ok_or_else(corrupt)?;
        let (body, sum) = bytes.split_at(body_len);
        if blake3::derive_key(SECRETS_CHECKSUM, body) != sum
            || body.len() < 4 + 2 + 32 + 1
            || body[..4] != SECRETS_MAGIC
        {
            return Err(corrupt());
        }
        let version = u16::from_le_bytes([body[4], body[5]]);
        let mut secrets = Self {
            seed: [0u8; 32],
            psk: None,
            issuer: None,
            enroll_port: None,
            noise: None,
        };
        secrets.seed.copy_from_slice(&body[6..38]);
        let mut pos = 38;
        let take32 = |pos: &mut usize| -> Option<Option<[u8; 32]>> {
            match *body.get(*pos)? {
                0 => {
                    *pos += 1;
                    Some(None)
                }
                1 => {
                    let raw = body.get(*pos + 1..*pos + 33)?;
                    let mut out = [0u8; 32];
                    out.copy_from_slice(raw);
                    *pos += 33;
                    Some(Some(out))
                }
                _ => None,
            }
        };
        secrets.psk = take32(&mut pos).ok_or_else(corrupt)?;
        match version {
            1 => {}
            2 | 3 => {
                secrets.issuer = take32(&mut pos).ok_or_else(corrupt)?;
                let port = body.get(pos..pos + 2).ok_or_else(corrupt)?;
                let port = u16::from_le_bytes([port[0], port[1]]);
                secrets.enroll_port = (port != 0).then_some(port);
                pos += 2;
                if version == 3 {
                    secrets.noise = take32(&mut pos).ok_or_else(corrupt)?;
                }
            }
            _ => return Err(corrupt()),
        }
        if pos != body.len() {
            return Err(corrupt());
        }
        Ok(secrets)
    }
}

fn storage_error(dir: &Path, e: StorageError) -> CliError {
    match e {
        StorageError::Busy => already_running(dir),
        StorageError::Security => invalid_args(format!(
            "node state directory {} failed ownership/permission validation",
            dir.display()
        )),
        other => generic(format!("node state {}: {other}", dir.display())),
    }
}

fn already_running(dir: &Path) -> CliError {
    let who = read_control_file(dir)
        .map(|c| format!(" (incarnation {}, pid {})", c.incarnation, c.pid))
        .unwrap_or_default();
    generic(format!(
        "a node is already running for {}{who}; see `net-mesh node status`",
        dir.display()
    ))
}

/// Open or initialize the node store, taking its exclusive lock. A generated
/// PSK is committed in the same initial snapshot, before any bind.
fn acquire(dir: &Path, generate_psk: bool) -> Result<(EnrollmentStorage, NodeSecrets), CliError> {
    match std::fs::symlink_metadata(dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let secrets = NodeSecrets {
                seed: random()?,
                psk: if generate_psk { Some(random()?) } else { None },
                issuer: None,
                enroll_port: None,
                noise: Some(random()?),
            };
            let storage = EnrollmentStorage::create(dir, secrets.encode().as_slice()).map_err(
                |e| match e {
                    StorageError::AlreadyExists => generic(format!(
                        "another `net-mesh up` is initializing {}",
                        dir.display()
                    )),
                    other => storage_error(dir, other),
                },
            )?;
            Ok((storage, secrets))
        }
        Err(e) => Err(generic(format!("node state {}: {e}", dir.display()))),
        Ok(_) => {
            let storage = EnrollmentStorage::open(dir).map_err(|e| storage_error(dir, e))?;
            let bytes = ScrubbedBytes::new(storage.read().map_err(|e| storage_error(dir, e))?);
            let secrets = NodeSecrets::decode(bytes.as_slice())?;
            Ok((storage, secrets))
        }
    }
}

/// Take the lifetime lock that `down` / `node status` probe. The store lock is
/// already held, so contention here is only a momentary status probe.
fn hold_lifetime_lock(dir: &Path) -> Result<File, CliError> {
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let file = opts
        .open(dir.join(LOCK_FILE))
        .map_err(|e| generic(format!("lifetime lock {}: {e}", dir.display())))?;
    for _ in 0..200 {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(std::fs::TryLockError::WouldBlock) => std::thread::sleep(Duration::from_millis(10)),
            Err(std::fs::TryLockError::Error(e)) => {
                return Err(generic(format!("lifetime lock {}: {e}", dir.display())))
            }
        }
    }
    Err(already_running(dir))
}

#[derive(Debug, PartialEq, Eq)]
enum Liveness {
    /// No state directory or lock file: never started here.
    Absent,
    /// Lock file present and free: no live owner.
    Free,
    /// Lock held: a live owner (starting, ready or draining).
    Held,
}

/// Probe the lifetime lock without creating anything.
fn probe(dir: &Path) -> Result<Liveness, CliError> {
    let file = match File::open(dir.join(LOCK_FILE)) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Liveness::Absent),
        Err(e) => return Err(generic(format!("lifetime lock {}: {e}", dir.display()))),
    };
    match file.try_lock() {
        // Dropping `file` releases the probe lock immediately.
        Ok(()) => Ok(Liveness::Free),
        Err(std::fs::TryLockError::WouldBlock) => Ok(Liveness::Held),
        Err(std::fs::TryLockError::Error(e)) => {
            Err(generic(format!("lifetime lock {}: {e}", dir.display())))
        }
    }
}

// ---- PSK sources -----------------------------------------------------------

enum PskSource {
    Generated,
    File(PathBuf),
    Stdin,
}

impl PskSource {
    fn parse(raw: Option<&str>) -> Result<Self, CliError> {
        match raw {
            None => Ok(Self::Generated),
            Some("stdin") => Ok(Self::Stdin),
            Some(s) if s.starts_with("file:") && s.len() > 5 => {
                Ok(Self::File(PathBuf::from(&s[5..])))
            }
            Some(s) if s.starts_with("kms:") => Err(invalid_args(
                "--psk-from kms: sources are not available in this build",
            )),
            Some(_) => Err(invalid_args(
                "--psk-from must be `file:<path>` or `stdin`; a literal PSK is never accepted",
            )),
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            Self::Generated => "generated",
            Self::File(_) => "file",
            Self::Stdin => "stdin",
        }
    }
}

/// Parse 32 raw bytes or 64 hex characters (surrounding whitespace allowed for
/// hex). Refuses an all-zero PSK. Never echoes the input.
fn parse_psk(bytes: &[u8]) -> Result<[u8; 32], CliError> {
    let mut psk = [0u8; 32];
    if bytes.len() == 32 {
        psk.copy_from_slice(bytes);
    } else {
        let text = std::str::from_utf8(bytes).ok().map(str::trim);
        let decoded = text
            .filter(|t| t.len() == 64)
            .and_then(|t| hex::decode(t).ok());
        let Some(decoded) = decoded.map(ScrubbedBytes::new) else {
            return Err(invalid_args(
                "PSK source must contain exactly 32 raw bytes or 64 hex characters",
            ));
        };
        psk.copy_from_slice(decoded.as_slice());
    }
    if psk == [0u8; 32] {
        return Err(invalid_args("an all-zero PSK is refused"));
    }
    Ok(psk)
}

async fn read_psk_file(path: &Path) -> Result<[u8; 32], CliError> {
    let owned = path.to_path_buf();
    let read = tokio::task::spawn_blocking(move || -> Result<ScrubbedBytes, String> {
        use std::io::Read as _;
        let file = ::net::adapter::net::secret_file::open_secret_file(&owned, false)
            .map_err(|e| format!("{e}"))?;
        let mut buf = Vec::with_capacity(130);
        file.take(130)
            .read_to_end(&mut buf)
            .map_err(|e| format!("read failed: {e}"))?;
        Ok(ScrubbedBytes::new(buf))
    })
    .await
    .map_err(|e| generic(format!("PSK file read task failed: {e}")))?
    .map_err(|e| invalid_args(format!("--psk-from file:{}: {e}", path.display())))?;
    parse_psk(read.as_slice())
}

async fn read_psk_stdin() -> Result<[u8; 32], CliError> {
    use std::io::IsTerminal as _;
    if std::io::stdin().is_terminal() {
        return Err(invalid_args(
            "--psk-from stdin needs piped input; a terminal is refused because echo cannot be disabled",
        ));
    }
    let mut buf = Vec::with_capacity(130);
    tokio::io::stdin()
        .take(130)
        .read_to_end(&mut buf)
        .await
        .map_err(|e| generic(format!("--psk-from stdin: {e}")))?;
    let buf = ScrubbedBytes::new(buf);
    parse_psk(buf.as_slice())
}

// ---- control file ------------------------------------------------------------

#[derive(Serialize, Deserialize)]
pub(crate) struct ControlFile {
    version: u32,
    pub(crate) incarnation: String,
    port: u16,
    secret: String,
    pid: u32,
    started_at: u64,
}

fn read_control_file(dir: &Path) -> Option<ControlFile> {
    let text = ::net::adapter::net::secret_file::read_secret_file_to_string(
        &dir.join(CONTROL_FILE),
        false,
    )
    .ok()?;
    let text = crate::secret::ScrubbedString::new(text);
    let parsed = serde_json::from_str::<ControlFile>(text.as_str()).ok();
    parsed.filter(|c| c.version == 1)
}

// ---- control protocol ----------------------------------------------------------

fn keyed(key: &[u8; 32], label: &[u8], a: &[u8], b: &[u8]) -> [u8; 32] {
    let mut h = blake3::Hasher::new_keyed(key);
    h.update(label);
    h.update(a);
    h.update(b);
    *h.finalize().as_bytes()
}

/// Constant-time tag comparison (`blake3::Hash` equality is constant-time).
fn tags_equal(a: [u8; 32], b: [u8; 32]) -> bool {
    blake3::Hash::from(a) == blake3::Hash::from(b)
}

/// Per-session keys: `mac` authenticates ciphertext, `enc` drives the keystream.
struct SessionKeys {
    mac: [u8; 32],
    enc: [u8; 32],
}

impl SessionKeys {
    fn derive(secret: &[u8; 32], node_nonce: &[u8], client_nonce: &[u8]) -> Self {
        Self {
            mac: keyed(secret, b"session", node_nonce, client_nonce),
            enc: keyed(secret, b"encrypt", node_nonce, client_nonce),
        }
    }

    /// XOR `data` with the keyed-BLAKE3 keystream for (direction, seq).
    fn apply_keystream(&self, direction: u8, seq: u64, data: &mut [u8]) {
        let mut h = blake3::Hasher::new_keyed(&self.enc);
        h.update(&[direction]);
        h.update(&seq.to_le_bytes());
        let mut stream = vec![0u8; data.len()];
        h.finalize_xof().fill(&mut stream);
        for (d, k) in data.iter_mut().zip(stream.iter()) {
            *d ^= k;
        }
        zeroize_slice(&mut stream);
    }
}

impl Drop for SessionKeys {
    fn drop(&mut self) {
        zeroize_slice(&mut self.mac);
        zeroize_slice(&mut self.enc);
    }
}

async fn write_msg(
    s: &mut TcpStream,
    keys: &SessionKeys,
    direction: u8,
    seq: u64,
    payload: &[u8],
) -> std::io::Result<()> {
    let mut sealed = payload.to_vec();
    keys.apply_keystream(direction, seq, &mut sealed);
    let tag = keyed(&keys.mac, &[direction], &seq.to_le_bytes(), &sealed);
    let len = u32::try_from(sealed.len() + 32).map_err(std::io::Error::other)?;
    s.write_all(&len.to_be_bytes()).await?;
    s.write_all(&sealed).await?;
    s.write_all(&tag).await?;
    s.flush().await
}

async fn read_msg(
    s: &mut TcpStream,
    keys: &SessionKeys,
    direction: u8,
    seq: u64,
) -> std::io::Result<Vec<u8>> {
    let bad = || std::io::Error::new(std::io::ErrorKind::InvalidData, "control message");
    let mut len = [0u8; 4];
    s.read_exact(&mut len).await?;
    let len = u32::from_be_bytes(len) as usize;
    if !(32..=MAX_CONTROL_FRAME).contains(&len) {
        return Err(bad());
    }
    let mut body = vec![0u8; len];
    s.read_exact(&mut body).await?;
    let payload_len = len - 32;
    let mut tag = [0u8; 32];
    tag.copy_from_slice(&body[payload_len..]);
    body.truncate(payload_len);
    if !tags_equal(
        tag,
        keyed(&keys.mac, &[direction], &seq.to_le_bytes(), &body),
    ) {
        return Err(bad());
    }
    keys.apply_keystream(direction, seq, &mut body);
    Ok(body)
}

const TO_NODE: u8 = 0;
const FROM_NODE: u8 = 1;

/// What the running node reports about itself.
#[derive(Serialize, Deserialize, Clone, Debug)]
struct NodeReport {
    incarnation: String,
    pid: u32,
    node_id: String,
    entity_id: String,
    public_key: String,
    bind: String,
    psk_source: String,
    trust_domain: String,
    started_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    enrollment: Option<super::enrollment::EnrollmentReport>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    joined: Option<JoinedReport>,
    /// The org this node is a member of (its installed, adopted authority).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    org: Option<String>,
    /// Why an adopted membership was not installed: `revoked` (below its
    /// org's floor) or `invalid` (e.g. expired), with the reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    org_state: Option<serde_json::Value>,
}

/// A node started from an installed join: whose mesh it joined and whether the
/// live attach to that issuer's node succeeded.
#[derive(Serialize, Deserialize, Clone, Debug)]
struct JoinedReport {
    /// The joined subnet attachment, when the join carried one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    subnet: Option<serde_json::Value>,
    /// The channel credential's first use, when the join carried one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    channel: Option<serde_json::Value>,
    issuer_fingerprint: String,
    domain_name: String,
    contact: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    relay: Option<String>,
    attached: bool,
    /// `direct` or `relay` when attached.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

struct ControlState {
    report: NodeReport,
    draining: AtomicBool,
    enroll: Option<Arc<super::enrollment::EnrollContext>>,
    /// The installed join this node runs from, owned (locked) by this process;
    /// `leave` records the departure through it.
    /// `None` inside once the node has shut down and released it.
    joined: Option<Arc<parking_lot::Mutex<Option<net_sdk::enrollment::device::DeviceJoin>>>>,
    /// Live link of a joined node (see [`spawn_joined_link`]).
    link: Option<Arc<parking_lot::Mutex<JoinedLink>>>,
    /// Standalone subnet memberships of a joined node, and where they live.
    memberships: Option<SharedMemberships>,
    subnets_dir: PathBuf,
    /// The state root (org authority and pending org links live under it).
    state_root: PathBuf,
    /// Standalone org links awaiting approval, asked again by the supervisor.
    pending_orgs: PendingOrgs,
    /// This node's mesh, for operations that reach other nodes.
    node: Arc<net::adapter::net::MeshNode>,
    /// A joined device's channel credential, when its join carried one.
    channel: Option<JoinedChannel>,
    /// Set once `subnet leave` recorded leaving the join's subnet relation.
    subnet_left: Arc<AtomicBool>,
}

/// Forward one root-signed floor readback request (built and signed by the
/// CLI; the root key never reaches this node) to the verifier it names:
/// answered locally when that is this node, otherwise over this node's own
/// mesh session, connecting first if needed. Returns the raw attestation;
/// the CLI decodes and verifies it against its own request, so this node
/// cannot vouch for anything.
/// Control op `org_floor_forward`: carry one already-built org floor request
/// to its named node (this node answers itself; another node is reached over
/// the mesh, connecting first when needed) and return that node's signed
/// attestation. The caller verifies it; this node only carries bytes.
async fn forward_org_floor(
    node: &Arc<net::adapter::net::MeshNode>,
    request: &serde_json::Value,
) -> serde_json::Value {
    use net_sdk::org::floors::{answer_org_floor, request_org_floor, OrgFloorRequest};
    let parsed = (|| {
        let bytes = hex::decode(request["request"].as_str()?).ok()?;
        let floor = OrgFloorRequest::from_bytes(&bytes).ok()?;
        let addr: Option<std::net::SocketAddr> =
            request["addr"].as_str().and_then(|a| a.parse().ok());
        let key = request["noise_pubkey"]
            .as_str()
            .and_then(|k| hex::decode(k).ok())
            .and_then(|k| <[u8; 32]>::try_from(k).ok());
        let wait = Duration::from_millis(request["wait_ms"].as_u64().unwrap_or(10_000))
            .min(MAX_FLOOR_FORWARD_WAIT);
        Some((bytes, floor, addr.zip(key), wait))
    })();
    let Some((bytes, floor, contact, wait)) = parsed else {
        return serde_json::json!({ "error": "malformed org floor request" });
    };
    if floor.verifier() == node.entity_id() {
        let (keypair, authority) = (node.entity_keypair_arc(), node.node_authority());
        let answered = tokio::task::spawn_blocking(move || {
            answer_org_floor(&bytes, &keypair, authority.as_deref(), now_unix())
        })
        .await
        .unwrap_or_else(|_| Err("org floor task failed".to_string()));
        return match answered {
            Ok(a) => serde_json::json!({ "attestation": hex::encode(a.to_bytes()) }),
            Err(e) => serde_json::json!({ "refused": e }),
        };
    }
    let target = floor.verifier().node_id();
    let outcome = tokio::time::timeout(wait, async {
        if node.peer_session_id(target).is_none() {
            let (addr, key) =
                contact.ok_or_else(|| "no session to that node and no contact".to_string())?;
            node.connect_via(addr, &key, target)
                .await
                .map_err(|e| e.to_string())?;
        }
        request_org_floor(node, target, &floor, wait).await
    })
    .await
    .unwrap_or_else(|_| Err("timed out".to_string()));
    match outcome {
        Ok(a) => serde_json::json!({ "attestation": hex::encode(a.to_bytes()) }),
        Err(e) => serde_json::json!({ "no_answer": e }),
    }
}

async fn forward_floor_query(
    node: &Arc<net::adapter::net::MeshNode>,
    request: &serde_json::Value,
) -> serde_json::Value {
    use net::adapter::net::subnet::floor_status::FloorStatusRequest;
    use net::adapter::net::SubnetFloorQueryError;
    struct Forward {
        bytes: Vec<u8>,
        verifier: net::adapter::net::identity::EntityId,
        contact: Option<(std::net::SocketAddr, [u8; 32])>,
        wait: Duration,
    }
    let parse = || -> Option<Forward> {
        let bytes = hex::decode(request["request"].as_str()?).ok()?;
        let entity: [u8; 32] = hex::decode(request["verifier"].as_str()?)
            .ok()?
            .try_into()
            .ok()?;
        let addr = request["addr"].as_str().and_then(|a| a.parse().ok());
        let key = request["noise_pubkey"]
            .as_str()
            .and_then(|k| hex::decode(k).ok())
            .and_then(|k| <[u8; 32]>::try_from(k).ok());
        let wait = Duration::from_millis(request["wait_ms"].as_u64().unwrap_or(10_000))
            .min(MAX_FLOOR_FORWARD_WAIT);
        Some(Forward {
            bytes,
            verifier: net::adapter::net::identity::EntityId::from_bytes(entity),
            contact: addr.zip(key),
            wait,
        })
    };
    let Some(Forward {
        bytes,
        verifier,
        contact,
        wait,
    }) = parse()
    else {
        return serde_json::json!({ "error": "malformed floor query" });
    };
    if &verifier == node.entity_id() {
        return match node.answer_subnet_floor_status(&bytes, now_unix()) {
            Ok(a) => serde_json::json!({ "attestation": hex::encode(a.to_bytes()) }),
            Err(e) => serde_json::json!({ "refused": format!("subnet:{e}") }),
        };
    }
    let Ok(parsed) = FloorStatusRequest::from_bytes(&bytes) else {
        return serde_json::json!({ "error": "malformed floor query" });
    };
    let target = verifier.node_id();
    let outcome = tokio::time::timeout(wait, async {
        if node.peer_session_id(target).is_none() {
            let (addr, key) = contact.ok_or_else(|| {
                SubnetFloorQueryError::NoAnswer("no session to the verifier and no contact".into())
            })?;
            node.connect_via(addr, &key, target)
                .await
                .map_err(|e| SubnetFloorQueryError::NoAnswer(e.to_string()))?;
        }
        node.query_subnet_floor_status(target, &parsed, wait).await
    })
    .await
    .unwrap_or_else(|_| Err(SubnetFloorQueryError::NoAnswer("timed out".into())));
    match outcome {
        Ok(a) => serde_json::json!({ "attestation": hex::encode(a.to_bytes()) }),
        Err(SubnetFloorQueryError::Refused(m)) => serde_json::json!({ "refused": m }),
        Err(e) => serde_json::json!({ "no_answer": e.to_string() }),
    }
}

/// Control op `members_forward`: carry one already-signed members request to
/// its named node (this node answers itself; another is reached over the
/// mesh, connecting first when needed) and return that node's signed
/// observation. The caller verifies it; this node only carries bytes.
async fn forward_members(
    node: &Arc<net::adapter::net::MeshNode>,
    request: &serde_json::Value,
) -> serde_json::Value {
    use net_sdk::members::{answer_members, request_members, MembersRequest};
    let parsed = (|| {
        let bytes = hex::decode(request["request"].as_str()?).ok()?;
        let members = MembersRequest::from_bytes(&bytes).ok()?;
        let addr: Option<std::net::SocketAddr> =
            request["addr"].as_str().and_then(|a| a.parse().ok());
        let key = request["noise_pubkey"]
            .as_str()
            .and_then(|k| hex::decode(k).ok())
            .and_then(|k| <[u8; 32]>::try_from(k).ok());
        let wait = Duration::from_millis(request["wait_ms"].as_u64().unwrap_or(10_000))
            .min(MAX_FLOOR_FORWARD_WAIT);
        Some((bytes, members, addr.zip(key), wait))
    })();
    let Some((bytes, members, contact, wait)) = parsed else {
        return serde_json::json!({ "error": "malformed members request" });
    };
    if members.verifier() == node.entity_id() {
        return match answer_members(&bytes, node, now_unix()) {
            Ok(o) => serde_json::json!({ "observation": hex::encode(o.to_bytes()) }),
            Err(e) => serde_json::json!({ "refused": e }),
        };
    }
    let target = members.verifier().node_id();
    let outcome = tokio::time::timeout(wait, async {
        if node.peer_session_id(target).is_none() {
            let (addr, key) =
                contact.ok_or_else(|| "no session to that node and no contact".to_string())?;
            node.connect_via(addr, &key, target)
                .await
                .map_err(|e| e.to_string())?;
        }
        request_members(node, target, &members, wait).await
    })
    .await
    .unwrap_or_else(|_| Err("timed out".to_string()));
    match outcome {
        Ok(o) => serde_json::json!({ "observation": hex::encode(o.to_bytes()) }),
        Err(e) => serde_json::json!({ "no_answer": e }),
    }
}

/// Control op `members`: what this node ISSUED for an org or subnet scope
/// (when it enrolls), and what it OBSERVES right now — never a claim about
/// other nodes. Org admission is per call (no session state), so for org
/// members this node reports standing against its own floors, not activity;
/// subnet admission is per session, so this node lists the peers admitted
/// to the scope at this moment.
async fn members(state: &ControlState, request: &serde_json::Value) -> serde_json::Value {
    let kind = request["kind"].as_str().unwrap_or_default().to_string();
    let wanted = request["target"].as_str().unwrap_or_default().to_string();
    if !matches!(kind.as_str(), "org" | "subnet") || wanted.is_empty() {
        return serde_json::json!({ "error": "malformed members request" });
    }
    let issued = match &state.enroll {
        Some(ctx) => {
            let (ctx, k, w) = (ctx.clone(), kind.clone(), wanted.clone());
            match tokio::task::spawn_blocking(move || ctx.issued_inventory(&k, &w)).await {
                Ok(Ok(v)) => Some(v),
                Ok(Err(e)) => return serde_json::json!({ "error": e }),
                Err(_) => return serde_json::json!({ "error": "inventory task failed" }),
            }
        }
        None => None,
    };
    let node = &state.node;
    let observed = if kind == "org" {
        let authority = node.node_authority();
        let same_org = authority
            .as_ref()
            .is_some_and(|a| hex::encode(a.owner_org().0) == wanted);
        let standing: Vec<serde_json::Value> = issued
            .as_ref()
            .and_then(|i| i["issued"].as_array().cloned())
            .unwrap_or_default()
            .iter()
            .filter_map(|row| {
                let subject = row["subject"].as_str()?;
                let authority = authority.as_ref().filter(|_| same_org)?;
                let bytes: [u8; 32] = hex::decode(subject).ok()?.try_into().ok()?;
                let entity = net::adapter::net::identity::EntityId::from_bytes(bytes);
                let floor = authority
                    .revocation
                    .floor_for(&authority.owner_org(), &entity);
                let standing = match row["approved_generation"].as_u64() {
                    Some(g) if u64::from(floor) > g => "revoked_here",
                    Some(_) => "admissible_here",
                    None => "unknown_generation",
                };
                Some(serde_json::json!({ "subject": subject, "floor_here": floor, "standing": standing }))
            })
            .collect();
        serde_json::json!({
            "enforces_this_org": same_org,
            "standing": standing,
        })
    } else {
        let admitted: Vec<serde_json::Value> = node
            .admitted_subnet_peers()
            .into_iter()
            .filter_map(|(peer, ctx)| {
                let at = super::subnet::format_subnet(ctx.attachment);
                (at == wanted || at.starts_with(&format!("{wanted}."))).then(|| {
                    serde_json::json!({
                        "peer_node": format!("0x{peer:016x}"),
                        "subject": hex::encode(ctx.subject.as_bytes()),
                        "attachment": at,
                        "rights": super::subnet::format_subnet_rights(ctx.rights),
                        "generation": ctx.generation,
                        "expires_at": ctx.expires_at,
                    })
                })
            })
            .collect();
        serde_json::json!({ "admitted_here": admitted })
    };
    serde_json::json!({
        "kind": kind,
        "target": wanted,
        "responder": hex::encode(node.entity_id().as_bytes()),
        "observed_at": now_unix(),
        "issued": issued.as_ref().map(|i| i["issued"].clone()),
        "unrecorded_offers": issued.as_ref().map(|i| i["unrecorded_offers"].clone()),
        "observed": observed,
        "completeness": {
            "issued": if issued.is_some() {
                "offers this node created that carry a relation record; older offers are counted in unrecorded_offers"
            } else {
                "unknown: this node does not enroll (run it with `up --enroll`)"
            },
            "observed": if kind == "org" {
                "standing against THIS node's floors only; org admission is per call, so whether a member is active is unknown; other nodes were not asked"
            } else {
                "sessions admitted at THIS node at observed_at; other verifiers were not asked; a member that is not connected here is absent, not removed"
            },
        },
    })
}

/// Control op `org_leave`: durably record that this node leaves its org, drop
/// its pending links to that org, then stop the node so its next start runs
/// on the mesh without the org. Nothing is sent to the org: the org still
/// accepts this node's certificate until the operator runs `org remove`.
async fn org_leave(state: &ControlState) -> serde_json::Value {
    let dir = state.state_root.join(AUTHORITY_SUBDIR);
    let installed = state
        .node
        .node_authority()
        .map(|a| hex::encode(a.owner_org().0));
    if let Some(left) = read_org_left(&dir) {
        if installed.is_none() {
            // Already left, and this runtime runs without the org.
            return serde_json::json!({
                "left": true,
                "newly_left": false,
                "org": left["org"],
                "left_at": left["left_at"],
                "stopping": false,
                "incarnation": state.report.incarnation,
            });
        }
    }
    let Some(org) = installed.or_else(|| owner_org_on_disk(&dir)) else {
        return serde_json::json!({ "error": "this node is not an org member; nothing to leave" });
    };
    let now = now_unix();
    let (d, o) = (dir.clone(), org.clone());
    let recorded = tokio::task::spawn_blocking(move || record_org_left(&d, &o, now))
        .await
        .map_err(|e| e.to_string())
        .and_then(|r| r.map_err(|e| e.to_string()));
    if let Err(e) = recorded {
        return serde_json::json!({ "error": format!("the departure was not recorded: {e}") });
    }
    // A pending link to the org just left must not complete later.
    let pending_dir = state.state_root.join(ORGS_PENDING_SUBDIR);
    state.pending_orgs.lock().retain(|(key, invite)| {
        let same = invite.org().is_some_and(|o| hex::encode(o.org.0) == org);
        if same {
            let _ = std::fs::remove_file(pending_dir.join(key));
        }
        !same
    });
    // Recorded durably; stop this runtime so nothing keeps acting as a member.
    state.draining.store(true, Ordering::SeqCst);
    serde_json::json!({
        "left": true,
        "newly_left": true,
        "org": org,
        "left_at": now,
        "stopping": true,
        "incarnation": state.report.incarnation,
    })
}

/// Control op `org_join`: redeem a standalone org link over this joined
/// node's session with the node it enrolled with. Pending until the operator
/// approves (the link is kept, and the link supervisor asks again); once
/// issued, the membership is adopted and installed live.
async fn org_join(state: &ControlState, request: &serde_json::Value) -> serde_json::Value {
    use net_sdk::enrollment::standalone::{
        is_standalone_org, request_subnet_redeem, SubnetRedeemReply,
    };
    let error = |m: String| serde_json::json!({ "error": m });
    let Some(join) = &state.joined else {
        return error(
            "this node did not join a mesh: a standalone org link extends an existing \
             membership (run `net-mesh join` first)"
                .to_string(),
        );
    };
    let Some(token) = request["token"].as_str() else {
        return error("malformed org_join request".to_string());
    };
    let invite = match net_sdk::enrollment::invite::MembershipInvite::decode(token) {
        Ok(invite) => invite,
        Err(e) => return error(format!("invalid link: {e}")),
    };
    if !is_standalone_org(&invite) {
        return error(
            "not a standalone org link (a mesh invite is redeemed with `net-mesh join`)"
                .to_string(),
        );
    }
    let Some(offer) = invite.org().copied() else {
        return error("not a standalone org link".to_string());
    };
    let joined = join.lock().as_ref().and_then(|j| {
        j.bundle().map(|b| {
            (
                j.identity().clone(),
                j.invite().issuer().clone(),
                b.contact().node_id,
            )
        })
    });
    let Some((identity, join_issuer, issuer_node)) = joined else {
        return error("this node's join holds no credentials".to_string());
    };
    if invite.issuer() != &join_issuer {
        return error(format!(
            "this link was issued by {}, not by the node this device enrolled with; standalone \
             links are redeemed only there",
            invite.issuer_fingerprint()
        ));
    }
    let key = membership_key(&invite);
    match request_subnet_redeem(
        &state.node,
        issuer_node,
        &identity,
        &invite,
        SUBNET_JOIN_REDEEM_WAIT,
    )
    .await
    {
        Ok(SubnetRedeemReply::PendingApproval) => {
            let dir = state.state_root.join(ORGS_PENDING_SUBDIR);
            let kept = std::fs::create_dir_all(&dir)
                .and_then(|()| std::fs::write(dir.join(&key), invite.to_bytes()));
            if let Err(e) = kept {
                return error(format!("keeping the pending link: {e}"));
            }
            let mut pending = state.pending_orgs.lock();
            if !pending.iter().any(|(k, _)| k == &key) {
                pending.push((key, invite));
            }
            serde_json::json!({
                "state": "pending_approval",
                "org": hex::encode(offer.org.0),
                "device": hex::encode(identity.entity_id().as_bytes()),
                "next": "the operator approves it with `org approve --org-key`; this node asks again by itself",
            })
        }
        Ok(SubnetRedeemReply::OrgIssued { cert, audience }) => {
            let (root, node) = (state.state_root.clone(), state.node.clone());
            let adopted = tokio::task::spawn_blocking(move || {
                adopt_and_install_org(&root, &node, *cert, audience)
            })
            .await
            .unwrap_or_else(|_| Err("org adoption task failed".to_string()));
            match adopted {
                Ok(mut adopted) => {
                    let _ =
                        std::fs::remove_file(state.state_root.join(ORGS_PENDING_SUBDIR).join(&key));
                    state.pending_orgs.lock().retain(|(k, _)| k != &key);
                    adopted["state"] = serde_json::json!("installed");
                    adopted["device"] =
                        serde_json::json!(hex::encode(identity.entity_id().as_bytes()));
                    adopted
                }
                Err(e) => error(format!("the membership was issued but not adopted: {e}")),
            }
        }
        Ok(SubnetRedeemReply::Issued(_)) => {
            error("the node answered with subnet credentials".to_string())
        }
        Err(e) => error(format!("redemption failed: {e}")),
    }
}

/// Ask again for every pending standalone org link redeemed at
/// `issuer_node`; adopt and install any that were issued.
async fn keep_pending_orgs(
    node: &Arc<net::adapter::net::MeshNode>,
    issuer_node: u64,
    identity: &net_sdk::identity::Identity,
    state_root: &Path,
    pending: &PendingOrgs,
    link: &Arc<parking_lot::Mutex<JoinedLink>>,
) {
    use net_sdk::enrollment::standalone::{request_subnet_redeem, SubnetRedeemReply};
    let snapshot = pending.lock().clone();
    for (key, invite) in snapshot {
        match request_subnet_redeem(node, issuer_node, identity, &invite, SUBNET_RENEW_WAIT).await {
            Ok(SubnetRedeemReply::OrgIssued { cert, audience }) => {
                let (root, node) = (state_root.to_path_buf(), node.clone());
                let adopted = tokio::task::spawn_blocking(move || {
                    adopt_and_install_org(&root, &node, *cert, audience)
                })
                .await
                .unwrap_or_else(|_| Err("org adoption task failed".to_string()));
                match adopted {
                    Ok(_) => {
                        let _ =
                            std::fs::remove_file(state_root.join(ORGS_PENDING_SUBDIR).join(&key));
                        pending.lock().retain(|(k, _)| k != &key);
                        link.lock().org_detail = None;
                    }
                    Err(e) => link.lock().org_detail = Some(e),
                }
            }
            Ok(SubnetRedeemReply::PendingApproval) => {}
            Ok(SubnetRedeemReply::Issued(_)) => {
                link.lock().org_detail = Some("unexpected subnet credentials".to_string());
            }
            Err(e) => link.lock().org_detail = Some(e),
        }
    }
}

/// Bound on redeeming a standalone subnet link over the session.
const SUBNET_JOIN_REDEEM_WAIT: Duration = Duration::from_secs(10);
/// Bound on presenting freshly redeemed standalone credentials.
const SUBNET_JOIN_PRESENT_WAIT: Duration = Duration::from_secs(10);

/// Control op `subnet_join`: redeem a standalone subnet link over this
/// joined node's session with the node it enrolled with, persist the
/// credentials as a membership, and present them (the verifier's verdict is
/// the admission). The link supervisor keeps it admitted from then on.
async fn subnet_join(state: &ControlState, request: &serde_json::Value) -> serde_json::Value {
    use net_sdk::enrollment::standalone::{
        is_standalone_subnet, request_subnet_redeem, SubnetMembership, SubnetRedeemReply,
    };
    let error = |m: String| serde_json::json!({ "error": m });
    let (Some(join), Some(memberships)) = (&state.joined, &state.memberships) else {
        return error(
            "this node did not join a mesh: a standalone subnet link extends an existing              membership (run `net-mesh join` first)"
                .to_string(),
        );
    };
    let Some(token) = request["token"].as_str() else {
        return error("malformed subnet_join request".to_string());
    };
    let invite = match net_sdk::enrollment::invite::MembershipInvite::decode(token) {
        Ok(invite) => invite,
        Err(e) => return error(format!("invalid link: {e}")),
    };
    if !is_standalone_subnet(&invite) {
        return error(
            "not a standalone subnet link (a mesh invite is redeemed with `net-mesh join`)"
                .to_string(),
        );
    }
    let Some(offer) = invite.subnet().cloned() else {
        return error("not a standalone subnet link".to_string());
    };
    let joined = join.lock().as_ref().and_then(|j| {
        j.bundle().map(|b| {
            (
                j.identity().clone(),
                j.invite().issuer().clone(),
                b.contact().node_id,
            )
        })
    });
    let Some((identity, join_issuer, issuer_node)) = joined else {
        return error("this node's join holds no credentials".to_string());
    };
    if invite.issuer() != &join_issuer {
        return error(format!(
            "this link was issued by {}, not by the node this device enrolled with; standalone              links are redeemed only there",
            invite.issuer_fingerprint()
        ));
    }
    let key = membership_key(&invite);
    // Record the intent first (or resume an earlier attempt).
    let existing = memberships
        .lock()
        .iter()
        .find(|m| membership_key(m.invite()) == key)
        .map(|m| (m.left_at(), m.credentials().cloned()));
    let installed = match existing {
        Some((Some(at), _)) => {
            return error(format!(
                "this device left that subnet membership at unix {at}; ask for a new link"
            ))
        }
        Some((None, credentials)) => credentials,
        None => {
            let dir = state.subnets_dir.join(&key);
            let created = std::fs::create_dir_all(&state.subnets_dir)
                .map_err(|e| e.to_string())
                .and_then(|()| {
                    SubnetMembership::begin(
                        &dir,
                        &invite,
                        identity.entity_id().clone(),
                        issuer_node,
                    )
                    .map_err(|e| e.to_string())
                });
            match created {
                Ok(m) => memberships.lock().push(m),
                Err(e) => return error(format!("subnet membership {}: {e}", dir.display())),
            }
            None
        }
    };
    let set = match installed {
        Some(set) => set,
        None => match request_subnet_redeem(
            &state.node,
            issuer_node,
            &identity,
            &invite,
            SUBNET_JOIN_REDEEM_WAIT,
        )
        .await
        {
            Ok(SubnetRedeemReply::PendingApproval) => {
                return serde_json::json!({
                    "state": "pending_approval",
                    "scope": super::subnet::format_subnet(offer.scope.path),
                    "rights": super::subnet::format_subnet_rights(offer.rights),
                    "next": "the operator approves it with `invite approve`; this node asks again by itself",
                })
            }
            Ok(SubnetRedeemReply::Issued(set)) => {
                let stored = memberships
                    .lock()
                    .iter_mut()
                    .find(|m| membership_key(m.invite()) == key)
                    .map(|m| m.install(&set));
                match stored {
                    Some(Ok(())) => *set,
                    Some(Err(e)) => return error(format!("credentials not installed: {e}")),
                    None => return error("node is draining".to_string()),
                }
            }
            Ok(SubnetRedeemReply::OrgIssued { .. }) => {
                return error("the node answered with an org membership".to_string())
            }
            Err(e) => return error(format!("redemption failed: {e}")),
        },
    };
    let admitted = state
        .node
        .present_subnet_credentials(
            issuer_node,
            &set,
            offer.scope.clone(),
            offer.rights,
            SUBNET_JOIN_PRESENT_WAIT,
        )
        .await
        .map(drop)
        .map_err(|e| e.to_string());
    serde_json::json!({
        "state": "installed",
        "scope": super::subnet::format_subnet(offer.scope.path),
        "rights": super::subnet::format_subnet_rights(offer.rights),
        "admitted": admitted.is_ok(),
        "detail": admitted.err(),
        "expires_at": set.leaf().not_after,
        "device": hex::encode(identity.entity_id().as_bytes()),
    })
}

async fn serve_control(
    listener: TcpListener,
    secret: [u8; 32],
    state: Arc<ControlState>,
    stop: mpsc::Sender<()>,
) {
    let permits = Arc::new(Semaphore::new(CONTROL_MAX_SESSIONS));
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            tokio::time::sleep(Duration::from_millis(50)).await;
            continue;
        };
        let Ok(permit) = permits.clone().try_acquire_owned() else {
            continue;
        };
        let (state, stop) = (state.clone(), stop.clone());
        tokio::spawn(async move {
            let _permit = permit;
            let _ = tokio::time::timeout(
                CONTROL_SERVE_TIMEOUT,
                control_session(stream, secret, &state, &stop),
            )
            .await;
        });
    }
}

async fn control_session(
    mut s: TcpStream,
    secret: [u8; 32],
    state: &ControlState,
    stop: &mpsc::Sender<()>,
) -> std::io::Result<()> {
    let bad = || std::io::Error::new(std::io::ErrorKind::PermissionDenied, "control auth");
    let server_nonce: [u8; 32] = random().map_err(|_| bad())?;
    let mut hello = CONTROL_MAGIC.to_vec();
    hello.extend_from_slice(&server_nonce);
    s.write_all(&hello).await?;
    let mut proof = [0u8; 64];
    s.read_exact(&mut proof).await?;
    let (client_nonce, client_tag) = proof.split_at(32);
    let mut tag = [0u8; 32];
    tag.copy_from_slice(client_tag);
    if !tags_equal(tag, keyed(&secret, b"client", &server_nonce, client_nonce)) {
        return Err(bad());
    }
    s.write_all(&keyed(&secret, b"server", &server_nonce, client_nonce))
        .await?;
    let keys = SessionKeys::derive(&secret, &server_nonce, client_nonce);

    let request = crate::secret::ScrubbedBytes::new(read_msg(&mut s, &keys, TO_NODE, 0).await?);
    let request: serde_json::Value = serde_json::from_slice(request.as_slice()).unwrap_or_default();
    let op = request["op"].as_str().unwrap_or_default().to_string();
    let draining = state.draining.load(Ordering::SeqCst);
    let reply = match op.as_str() {
        _ if op.starts_with("invite_") => match (&state.enroll, draining) {
            (_, true) => serde_json::json!({ "error": "node is draining" }),
            (None, false) => {
                serde_json::json!({ "error": "enrollment is not enabled on this node (start it with `up --enroll`)" })
            }
            (Some(ctx), false) => {
                let ctx = ctx.clone();
                tokio::task::spawn_blocking(move || ctx.handle(&op_owned(&request), &request))
                    .await
                    .unwrap_or_else(
                        |_| serde_json::json!({ "error": "enrollment operation failed" }),
                    )
            }
        },
        "status" => {
            // A joined node's current subnet leaf (it changes on renewal).
            let subnet_expires_at = state.joined.as_ref().and_then(|j| {
                j.lock()
                    .as_ref()
                    .and_then(|j| j.bundle())
                    .and_then(|b| b.subnet_credentials())
                    .map(|set| set.leaf().not_after)
            });
            serde_json::json!({
                "state": if draining { "draining" } else { "ready" },
                "node": state.report,
                "subnet_expires_at": subnet_expires_at,
                "link": state.link.as_ref().map(|l| l.lock().clone()),
                "org": state.node.node_authority().map(|a| hex::encode(a.owner_org().0)),
                "org_pending": state.pending_orgs.lock().len(),
                "channel": state.channel.as_ref().map(|c| c.link.lock().clone()),
            })
        }
        "shutdown" => {
            state.draining.store(true, Ordering::SeqCst);
            serde_json::json!({ "accepted": true, "incarnation": state.report.incarnation })
        }
        "members" => members(state, &request).await,
        "members_forward" => forward_members(&state.node, &request).await,
        "org_leave" if draining => serde_json::json!({ "error": "node is draining" }),
        "org_leave" => org_leave(state).await,
        "org_join" if draining => serde_json::json!({ "error": "node is draining" }),
        "org_join" => org_join(state, &request).await,
        "channel_status" => serde_json::json!({
            "served": super::channel::read_served(&state.state_root)
                .unwrap_or_else(|e| vec![super::channel::Served { channel: format!("<unreadable: {e}>"), roots: Vec::new() }]),
            "joined": state.channel.as_ref().map(|c| c.link.lock().clone()),
        }),
        "channel_leave" if draining => serde_json::json!({ "error": "node is draining" }),
        "channel_leave" => match &state.channel {
            None => serde_json::json!({
                "error": "this node holds no channel credential from a join; nothing to leave"
            }),
            Some(c) => {
                super::channel_link::leave(
                    &state.node,
                    &state.state_root,
                    &c.cred,
                    c.publisher,
                    &c.left,
                    &c.link,
                    CHANNEL_UNSUBSCRIBE_WAIT,
                )
                .await
            }
        },
        "channel_publish" if draining => serde_json::json!({ "error": "node is draining" }),
        "channel_publish" => {
            let reply = super::channel::publish_op(&state.node, &request).await;
            if reply["gate"] == "passed" && reply.get("error").is_none() {
                if let Some(c) = &state.channel {
                    super::channel_link::published(
                        &c.link,
                        request["channel"].as_str().unwrap_or_default(),
                    );
                }
            }
            reply
        }
        "subnet_leave" if draining => serde_json::json!({ "error": "node is draining" }),
        "subnet_leave" => subnet_leave(state, &request).await,
        "channel_serve" if draining => serde_json::json!({ "error": "node is draining" }),
        "channel_serve" => {
            let (node, root, request) = (
                state.node.clone(),
                state.state_root.clone(),
                request.clone(),
            );
            tokio::task::spawn_blocking(move || super::channel::serve_op(&node, &root, &request))
                .await
                .unwrap_or_else(|_| serde_json::json!({ "error": "channel serve failed" }))
        }
        "subnet_join" if draining => serde_json::json!({ "error": "node is draining" }),
        "subnet_join" => subnet_join(state, &request).await,
        "org_floor_forward" if draining => serde_json::json!({ "error": "node is draining" }),
        "org_floor_forward" => forward_org_floor(&state.node, &request).await,
        "subnet_floor_query" if draining => serde_json::json!({ "error": "node is draining" }),
        "subnet_floor_query" => forward_floor_query(&state.node, &request).await,
        "leave" => match (&state.joined, draining) {
            (_, true) => serde_json::json!({ "error": "node is draining" }),
            (None, false) => serde_json::json!({
                "error": "this node did not join a mesh; nothing to leave (stop it with `down`)"
            }),
            (Some(join), false) => {
                let (join, now) = (join.clone(), now_unix());
                let memberships = state.memberships.clone();
                let recorded = tokio::task::spawn_blocking(move || {
                    // Standalone subnet memberships go first: a failure here
                    // leaves the join (and this node) intact to retry.
                    if let Some(memberships) = &memberships {
                        for m in memberships.lock().iter_mut() {
                            m.leave(now).map_err(Some)?;
                        }
                    }
                    let mut guard = join.lock();
                    let join = guard.as_mut().ok_or(None)?;
                    join.leave(now)
                        .map(|newly| (newly, join.left_at()))
                        .map_err(Some)
                })
                .await;
                match recorded {
                    Ok(Ok((newly, left_at))) => {
                        // Recorded durably; now stop this runtime. A live
                        // channel subscription is withdrawn first (the
                        // publisher acknowledges), not left to time out.
                        let unsubscribed = match &state.channel {
                            Some(c)
                                if c.link.lock().subscribed == Some(true)
                                    && !c.left.load(Ordering::SeqCst) =>
                            {
                                c.left.store(true, Ordering::SeqCst);
                                Some(matches!(
                                    tokio::time::timeout(
                                        CHANNEL_UNSUBSCRIBE_WAIT,
                                        state.node.unsubscribe_channel(
                                            c.publisher,
                                            c.cred.offer.channel.clone(),
                                        ),
                                    )
                                    .await,
                                    Ok(Ok(()))
                                ))
                            }
                            _ => None,
                        };
                        state.draining.store(true, Ordering::SeqCst);
                        serde_json::json!({
                            "left": true,
                            "newly_left": newly,
                            "left_at": left_at,
                            "incarnation": state.report.incarnation,
                            "channel_unsubscribed": unsubscribed,
                        })
                    }
                    Ok(Err(Some(e))) => {
                        serde_json::json!({ "error": format!("the departure was not recorded: {e}") })
                    }
                    Ok(Err(None)) => serde_json::json!({ "error": "node is draining" }),
                    Err(_) => serde_json::json!({ "error": "the departure was not recorded" }),
                }
            }
        },
        _ => serde_json::json!({ "error": "unknown control operation" }),
    };
    let bytes = crate::secret::ScrubbedBytes::new(
        serde_json::to_vec(&reply).map_err(std::io::Error::other)?,
    );
    write_msg(&mut s, &keys, FROM_NODE, 0, bytes.as_slice()).await?;
    if op == "shutdown"
        || (op == "leave" && reply["left"] == serde_json::Value::Bool(true))
        || (op == "org_leave" && reply["stopping"] == serde_json::Value::Bool(true))
    {
        let _ = stop.try_send(());
    }
    Ok(())
}

fn op_owned(request: &serde_json::Value) -> String {
    request["op"].as_str().unwrap_or_default().to_string()
}

#[derive(Debug)]
pub(crate) enum ControlError {
    /// No readable control file.
    NoControlFile,
    /// Could not connect to the recorded port.
    Unreachable,
    /// The endpoint did not prove the secret (or rejected ours).
    Unauthenticated,
    /// Malformed exchange.
    Protocol,
}

impl std::fmt::Display for ControlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NoControlFile => "no running node found for this state directory",
            Self::Unreachable => "the node's control endpoint is unreachable",
            Self::Unauthenticated => "the node's control endpoint did not authenticate",
            Self::Protocol => "the control exchange failed",
        })
    }
}

/// Authenticate to the node recorded in `dir` and perform one request.
pub(crate) async fn control_call(
    dir: &Path,
    request: serde_json::Value,
) -> Result<(ControlFile, serde_json::Value), ControlError> {
    control_call_within(dir, request, CONTROL_SESSION_TIMEOUT).await
}

/// [`control_call`] with an explicit bound, for operations that reach
/// other mesh nodes.
pub(crate) async fn control_call_within(
    dir: &Path,
    request: serde_json::Value,
    bound: Duration,
) -> Result<(ControlFile, serde_json::Value), ControlError> {
    let control = read_control_file(dir).ok_or(ControlError::NoControlFile)?;
    let mut secret = [0u8; 32];
    let decoded = hex::decode(&control.secret)
        .ok()
        .filter(|d| d.len() == 32)
        .map(ScrubbedBytes::new)
        .ok_or(ControlError::NoControlFile)?;
    secret.copy_from_slice(decoded.as_slice());
    let result = tokio::time::timeout(bound, async {
        let mut s = TcpStream::connect(("127.0.0.1", control.port))
            .await
            .map_err(|_| ControlError::Unreachable)?;
        let mut hello = [0u8; 36];
        s.read_exact(&mut hello)
            .await
            .map_err(|_| ControlError::Unauthenticated)?;
        if hello[..4] != CONTROL_MAGIC {
            return Err(ControlError::Unauthenticated);
        }
        let server_nonce = &hello[4..];
        let client_nonce: [u8; 32] = random().map_err(|_| ControlError::Protocol)?;
        let mut proof = client_nonce.to_vec();
        proof.extend_from_slice(&keyed(&secret, b"client", server_nonce, &client_nonce));
        s.write_all(&proof)
            .await
            .map_err(|_| ControlError::Unauthenticated)?;
        let mut server_tag = [0u8; 32];
        s.read_exact(&mut server_tag)
            .await
            .map_err(|_| ControlError::Unauthenticated)?;
        if !tags_equal(
            server_tag,
            keyed(&secret, b"server", server_nonce, &client_nonce),
        ) {
            return Err(ControlError::Unauthenticated);
        }
        let keys = SessionKeys::derive(&secret, server_nonce, &client_nonce);
        let request = serde_json::to_vec(&request).map_err(|_| ControlError::Protocol)?;
        write_msg(&mut s, &keys, TO_NODE, 0, &request)
            .await
            .map_err(|_| ControlError::Protocol)?;
        let reply = crate::secret::ScrubbedBytes::new(
            read_msg(&mut s, &keys, FROM_NODE, 0)
                .await
                .map_err(|_| ControlError::Protocol)?,
        );
        serde_json::from_slice(reply.as_slice()).map_err(|_| ControlError::Protocol)
    })
    .await;
    zeroize_slice(&mut secret);
    let value = result.map_err(|_| ControlError::Unreachable)??;
    Ok((control, value))
}

// ---- up ------------------------------------------------------------------------

#[derive(Serialize)]
struct UpEvent<'a> {
    event: &'a str,
    #[serde(flatten)]
    node: &'a NodeReport,
    state_dir: String,
}

/// Run one production node in the foreground until Ctrl-C or `net-mesh down`.
pub async fn run_up(
    args: UpArgs,
    output: Option<OutputFormat>,
    config_path: Option<&Path>,
    profile_name: &str,
) -> Result<(), CliError> {
    // Resolve and validate everything before any filesystem or network effect.
    let profile = crate::context::resolve_profile(config_path, profile_name).await?;
    let state = state_dir(args.state_dir, profile_name)?;
    let source = PskSource::parse(args.psk_from.as_deref())?;
    let bind_raw = args
        .bind
        .or_else(|| profile.bind.clone())
        .unwrap_or_else(|| "0.0.0.0:0".to_string());
    let bind = crate::context::parse_bind_literal(&bind_raw)?;
    let explicit_identity = args.identity.is_some();
    let identity_path = args.identity.or_else(|| profile.identity.clone());
    let fmt = OutputFormat::resolve_stream(output);
    let enroll_plan = if args.enroll {
        let relay = if args.no_relay {
            None
        } else {
            args.relay
                .or_else(|| profile.relay.clone())
                .or_else(|| super::enrollment::DEFAULT_RELAY.map(str::to_string))
        };
        Some(super::enrollment::EnrollPlan::validate(
            args.public_addr,
            relay,
            args.issuer_identity,
            args.ledger,
            args.domain_name,
            !args.no_port_mapping,
            &state,
            profile_name,
        )?)
    } else {
        None
    };
    // A state directory holding a join runs as that joined device: its own
    // enrolled identity and delivered PSK, nothing that would contradict them.
    let join_dir = state.join(super::enrollment::JOIN_SUBDIR);
    let has_join = join_dir.exists();
    if has_join {
        if args.enroll {
            return Err(invalid_args(
                "this node joined another operator's mesh; --enroll would hand that mesh's PSK to others",
            ));
        }
        if args.psk_from.is_some() || explicit_identity {
            return Err(invalid_args(
                "this node runs from its installed join; --psk-from and --identity do not apply",
            ));
        }
    }
    // A supplied PSK is read and validated before any filesystem effect, so a
    // refused source leaves no state behind.
    let supplied = match &source {
        PskSource::Generated => None,
        PskSource::File(path) => Some(read_psk_file(path).await?),
        PskSource::Stdin => Some(read_psk_stdin().await?),
    };

    std::fs::create_dir_all(&state)
        .map_err(|e| generic(format!("create state directory {}: {e}", state.display())))?;
    let dir = state.join(NODE_SUBDIR);
    let generate = matches!(source, PskSource::Generated);
    let (dir_owned, gen) = (dir.clone(), generate);
    let (mut storage, mut secrets) = tokio::task::spawn_blocking(move || acquire(&dir_owned, gen))
        .await
        .map_err(|e| generic(format!("node state task failed: {e}")))??;
    let lifetime_lock = hold_lifetime_lock(&dir)?;
    // State from before v3 has no Noise key: commit one before any bind.
    let noise_key = match secrets.noise {
        Some(key) => key,
        None => {
            secrets.noise = Some(random()?);
            storage
                .replace(secrets.encode().as_slice())
                .map_err(|e| storage_error(&dir, e))?;
            secrets.noise.unwrap_or_default()
        }
    };
    let mut joined = if has_join {
        use net_sdk::enrollment::device::{DeviceJoin, DeviceJoinError};
        let join = DeviceJoin::open(&join_dir).map_err(|e| match e {
            DeviceJoinError::Storage(StorageError::Busy) => generic(format!(
                "join state {} is in use by another process",
                join_dir.display()
            )),
            other => generic(format!("join state {}: {other}", join_dir.display())),
        })?;
        if let Some(at) = join.left_at() {
            return Err(invalid_args(format!(
                "this device left the mesh at unix {at}; run `net-mesh join <token> --rejoin` to join again"
            )));
        }
        if join.bundle().is_none() {
            return Err(invalid_args(
                "the join in this state directory is still pending approval; run `net-mesh join` again once approved",
            ));
        }
        Some(join)
    } else {
        None
    };
    // Enrollment is provisioned (fixed port, issuer, ledger lock) before any bind.
    let port_mapping = enroll_plan.as_ref().is_some_and(|p| p.port_mapping());
    let (bind, enroll_owner) = match enroll_plan {
        Some(plan) => {
            let (bind, owner) = plan
                .provision(bind, &mut secrets, |s| {
                    storage
                        .replace(s.encode().as_slice())
                        .map_err(|e| storage_error(&dir, e))
                })
                .await?;
            (bind, Some(owner))
        }
        None => (bind, None),
    };

    let joined_psk = joined
        .as_ref()
        .and_then(|j| j.bundle())
        .map(|b| *b.psk().expose_bytes());
    let mut psk = match supplied.or(joined_psk) {
        Some(psk) => psk,
        None => match secrets.psk {
            Some(psk) => psk,
            None => {
                // First generated start after earlier supplied-source runs:
                // commit the new value before bind.
                secrets.psk = Some(random()?);
                storage
                    .replace(secrets.encode().as_slice())
                    .map_err(|e| storage_error(&dir, e))?;
                secrets.psk.unwrap_or_default()
            }
        },
    };
    let psk_value = Psk::new(psk);
    let trust_domain = psk_value.trust_domain().to_string();
    let identity = match (&joined, &identity_path) {
        (Some(join), _) => join.identity().clone(),
        (None, Some(path)) => crate::context::load_operator_identity(path).await?,
        (None, None) => Identity::from_seed(secrets.seed),
    };
    drop(secrets);

    // A subnet issuer makes this node the subnet verifier too: it trusts the
    // grant's authority, persists accepted floors and serves readback.
    let subnet_issuer = match (&args.subnet_issuer_grant, &args.subnet_issuer_key) {
        (Some(grant), Some(key)) => Some(
            super::subnet::load_subnet_leaf_issuer(
                grant,
                key,
                args.subnet_leaf_ttl,
                args.subnet_generation,
            )
            .await?,
        ),
        _ => None,
    };
    let built = net_sdk::MeshBuilder::new(&bind.to_string(), &psk)
        .map_err(|e| invalid_args(format!("mesh bind {bind}: {e}")));
    zeroize_slice(&mut psk);
    let mut builder = built?
        .identity(identity.clone())
        .noise_static_key(net::adapter::net::NoiseStaticKey::from_private(noise_key))
        .try_port_mapping(port_mapping);
    if let Some(issuer) = &subnet_issuer {
        let authority = issuer.grant().authority.clone();
        builder = builder
            .subnet_authority(net_sdk::subnet::SubnetAuthorityConfig {
                authority: authority.clone(),
                roots: vec![authority],
                maximum_grant_lifetime_secs:
                    net::adapter::net::subnet::auth::MAX_SUBNET_GRANT_LIFETIME_SECS,
            })
            .subnet_floor_store(dir.join("subnet-floors"));
    }
    let mesh = builder
        .build()
        .await
        .map_err(|e| connection_failure(format!("mesh start on {bind}: {e}")))?;
    mesh.start();
    // Channels served from this state directory (`channel serve`) are gated
    // again before anything is served; an unreadable record fails closed.
    for served in super::channel::read_served(&state).map_err(generic)? {
        super::channel::register_served(mesh.node(), &served)
            .map_err(|e| generic(format!("served channel {}: {e}", served.channel)))?;
    }
    let mut channel_issuers = Vec::new();
    for path in &args.channel_grants {
        // `--channel-grant` requires `--enroll`, so the owner exists.
        let Some(owner) = &enroll_owner else { break };
        let (name, issuer) = super::channel::load_channel_issuer(path, owner.issuer()).await?;
        if channel_issuers
            .iter()
            .any(|(n, _): &(net::adapter::net::channel::ChannelName, _)| n == &name)
        {
            return Err(invalid_args(format!(
                "two --channel-grant files name channel {}",
                name.as_str()
            )));
        }
        channel_issuers.push((name, issuer));
    }
    // An adopted org membership (from `join` / `org join`, or `node adopt`
    // into this state directory) is installed before anything is served.
    let authority_dir = state.join(AUTHORITY_SUBDIR);
    // A membership that has ENDED (revoked by a floor, or expired) leaves the
    // node running without it — removal from an org is not removal from the
    // mesh. Anything else wrong with the directory fails closed.
    let left = read_org_left(&authority_dir);
    let (org_owner, org_ended) = if let Some(left) = &left {
        (
            None,
            Some((
                "left",
                format!(
                    "left org {} at unix {}; a new approved org link rejoins",
                    left["org"].as_str().unwrap_or("?"),
                    left["left_at"].as_u64().unwrap_or(0)
                ),
            )),
        )
    } else if authority_dir.exists() {
        use net::adapter::net::behavior::org_authority::OrgAuthorityError;
        match mesh.install_org_authority(&authority_dir) {
            Ok(()) => (
                mesh.node()
                    .node_authority()
                    .map(|a| hex::encode(a.owner_org().0)),
                None,
            ),
            Err(net_sdk::org::OrgProvisionError::Authority(
                e @ OrgAuthorityError::CertBelowFloor { .. },
            )) => (None, Some(("revoked", e.to_string()))),
            Err(net_sdk::org::OrgProvisionError::Authority(
                e @ OrgAuthorityError::CertInvalid(_),
            )) => (None, Some(("invalid", e.to_string()))),
            Err(e) => {
                return Err(generic(format!(
                    "org authority {}: {e}",
                    authority_dir.display()
                )))
            }
        }
    } else {
        (None, None)
    };
    // Any node may be asked, by a holder of the authority, what it observes.
    let _members = net_sdk::members::serve_members(mesh.node())
        .map_err(|e| generic(format!("members service: {e}")))?;
    // Any node may be asked to apply a root-signed org floor (and attest).
    let _org_floor = net_sdk::org::floors::serve_org_floor(mesh.node())
        .map_err(|e| generic(format!("org floor service: {e}")))?;
    let _subnet_readback = match &subnet_issuer {
        Some(_) => Some(
            mesh.node()
                .serve_subnet_floor_status()
                .map_err(|e| generic(format!("subnet floor readback: {e}")))?,
        ),
        None => None,
    };

    let enrollment = match enroll_owner {
        Some(owner) => Some(
            owner
                .start(&mesh, psk_value, subnet_issuer.clone(), channel_issuers)
                .await?,
        ),
        None => None,
    };
    // A renewed leaf (at start) to persist once the join is mutable again.
    let mut renewed_at_start = None;
    // The channel credential's start state, handed to the supervisor.
    let mut channel_start = None;
    let joined_report = match joined.as_ref().and_then(|j| j.bundle().map(|b| (j, b))) {
        Some((join, bundle)) => {
            let c = bundle.contact();
            let (path, detail) =
                match super::enrollment::attach_contact(mesh.node(), c, JOIN_ATTACH_WAIT).await {
                    Ok(path) => (Some(path.to_string()), None),
                    Err(e) => (None, Some(format!("attach failed: {e}"))),
                };
            // Subnet admission is proven on the session, by the verifier's
            // verdict — not inferred from holding credentials.
            let subnet_was_left = read_subnet_left(&state).is_some();
            let subnet = match (join.invite().subnet(), bundle.subnet_credentials()) {
                (Some(offer), Some(_)) if subnet_was_left => Some(serde_json::json!({
                    "scope": super::subnet::format_subnet(offer.scope.path),
                    "rights": super::subnet::format_subnet_rights(offer.rights),
                    "state": "left",
                })),
                (Some(offer), Some(set)) => {
                    // Renew first when the leaf is near (or past) expiry.
                    let mut set = set;
                    let mut renew_error = None;
                    if detail.is_none() && now_unix() >= subnet_renew_at(&set) {
                        match net_sdk::enrollment::renew::request_subnet_renewal(
                            mesh.node(),
                            c.node_id,
                            join.identity(),
                            join.invite(),
                            SUBNET_RENEW_WAIT,
                        )
                        .await
                        {
                            Ok(fresh) => {
                                set = fresh.clone();
                                renewed_at_start = Some(fresh);
                            }
                            Err(e) => renew_error = Some(e),
                        }
                    }
                    let admitted = if detail.is_some() {
                        Err("not attached".to_string())
                    } else {
                        mesh.node()
                            .present_subnet_credentials(
                                c.node_id,
                                &set,
                                offer.scope.clone(),
                                offer.rights,
                                JOIN_ATTACH_WAIT,
                            )
                            .await
                            .map_err(|e| e.to_string())
                    };
                    Some(serde_json::json!({
                        "scope": super::subnet::format_subnet(offer.scope.path),
                        "rights": super::subnet::format_subnet_rights(offer.rights),
                        "admitted": admitted.is_ok(),
                        "detail": admitted.err(),
                        "expires_at": set.leaf().not_after,
                        "renewed": renewed_at_start.is_some(),
                        "renew_error": renew_error,
                    }))
                }
                _ => None,
            };
            // The channel credential: installed / subscribed once here, then
            // kept by the supervisor on every new session.
            let channel = match super::channel_link::ChannelCred::of(join) {
                Some(cred) => {
                    let left = super::channel_link::read_left(&state).is_some();
                    let link = super::channel_link::start(
                        mesh.node(),
                        &cred,
                        left,
                        detail.is_none().then_some(c.node_id),
                        JOIN_ATTACH_WAIT,
                    )
                    .await;
                    channel_start = Some((cred, link.clone(), left, c.node_id));
                    Some(serde_json::to_value(&link).unwrap_or_default())
                }
                None => None,
            };
            Some(JoinedReport {
                subnet,
                channel,
                issuer_fingerprint: join.invite().issuer_fingerprint(),
                domain_name: join.invite().trust_domain_name().to_string(),
                contact: c.addr.map(|a| a.to_string()),
                relay: c.relay.as_ref().map(|r| r.endpoint.as_str().to_string()),
                attached: detail.is_none(),
                path,
                detail,
            })
        }
        None => None,
    };

    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .map_err(|e| generic(format!("control endpoint bind: {e}")))?;
    let port = listener
        .local_addr()
        .map_err(|e| generic(format!("control endpoint: {e}")))?
        .port();
    let mut secret: [u8; 32] = random()?;
    let incarnation = hex::encode(random::<16>()?);
    let report = NodeReport {
        incarnation: incarnation.clone(),
        pid: std::process::id(),
        node_id: format!("0x{:016x}", mesh.node_id()),
        entity_id: hex::encode(identity.entity_id().as_bytes()),
        public_key: hex::encode(mesh.public_key()),
        bind: mesh.local_addr().to_string(),
        psk_source: if joined.is_some() {
            "joined".to_string()
        } else {
            source.kind().to_string()
        },
        trust_domain,
        started_at: now_unix(),
        enrollment: enrollment.as_ref().map(|e| e.report()),
        joined: joined_report,
        org: org_owner,
        org_state: org_ended
            .map(|(state, detail)| serde_json::json!({ "state": state, "detail": detail })),
    };
    let control = ControlFile {
        version: 1,
        incarnation: incarnation.clone(),
        port,
        secret: hex::encode(secret),
        pid: report.pid,
        started_at: report.started_at,
    };
    let control_text = crate::secret::ScrubbedString::new(
        serde_json::to_string(&control).map_err(|e| generic(format!("control file: {e}")))?,
    );
    let control_path = dir.join(CONTROL_FILE);
    crate::commands::identity::write_identity_atomically(
        &dir.join(format!("{CONTROL_FILE}.{incarnation}.tmp")),
        &control_path,
        control_text.as_bytes(),
    )
    .await?;
    drop(control_text);

    let joined_contact_node = joined
        .as_ref()
        .and_then(|j| j.bundle())
        .map(|b| b.contact().node_id);
    // Persist a leaf renewed at start (re-checked against the signed offer).
    if let (Some(join), Some(set)) = (joined.as_mut(), renewed_at_start.as_ref()) {
        if let Err(e) = join.replace_subnet_credentials(set) {
            tracing::warn!(error = %e, "renewed subnet credentials were not persisted");
        }
    }
    let joined = joined.map(|j| Arc::new(parking_lot::Mutex::new(Some(j))));
    let subnets_dir = state.join(SUBNETS_SUBDIR);
    let pending_orgs: PendingOrgs = Arc::new(parking_lot::Mutex::new(load_pending_orgs(
        &state.join(ORGS_PENDING_SUBDIR),
    )));
    let memberships: Option<SharedMemberships> = match &joined {
        Some(_) => Some(Arc::new(parking_lot::Mutex::new(load_memberships(
            &subnets_dir,
        )?))),
        None => None,
    };
    // Seed the live link from the start attempt, then keep it up.
    let start_link = report.joined.as_ref().map(|j| {
        let subnet = j.subnet.as_ref();
        JoinedLink {
            attached: j.attached,
            path: j.path.clone(),
            detail: j.detail.clone(),
            reattaches: 0,
            readmissions: 0,
            standalone: Default::default(),
            org_detail: None,
            subnet_admitted: subnet.and_then(|s| s["admitted"].as_bool()),
            subnet_detail: subnet.and_then(|s| s["detail"].as_str().map(str::to_string)),
        }
    });
    let link = start_link.map(|l| Arc::new(parking_lot::Mutex::new(l)));
    let subnet_left = Arc::new(AtomicBool::new(read_subnet_left(&state).is_some()));
    let joined_channel = channel_start.map(|(cred, link, left, publisher)| {
        // Subscribed at start: on the current session.
        let on = match link.subscribed {
            Some(true) => mesh.node().peer_session_id(publisher),
            _ => None,
        };
        (
            JoinedChannel {
                cred,
                link: Arc::new(parking_lot::Mutex::new(link)),
                left: Arc::new(AtomicBool::new(left)),
                publisher,
            },
            on,
        )
    });
    let joined_link = match (&joined, &link, &joined_contact_node, &memberships) {
        (Some(joined), Some(link), Some(contact_node), Some(memberships)) => {
            let admitted = link.lock().subnet_admitted;
            // Presented at start: on the current session if admitted; a
            // refused presentation is retried later, not immediately.
            let presented = match admitted {
                Some(true) => mesh.node().peer_session_id(*contact_node),
                _ => None,
            };
            let present_retry_at = match admitted {
                Some(false) => now_unix() + SUBNET_RENEW_RETRY.as_secs(),
                _ => 0,
            };
            Some(spawn_joined_link(
                joined.clone(),
                memberships.clone(),
                pending_orgs.clone(),
                state.clone(),
                mesh.node().clone(),
                link.clone(),
                presented,
                present_retry_at,
                joined_channel.clone(),
                subnet_left.clone(),
            ))
        }
        _ => None,
    };
    let state_ctl = Arc::new(ControlState {
        report: report.clone(),
        draining: AtomicBool::new(false),
        enroll: enrollment.as_ref().map(|e| e.context()),
        joined: joined.clone(),
        link: link.clone(),
        memberships: memberships.clone(),
        subnets_dir,
        state_root: state.clone(),
        pending_orgs: pending_orgs.clone(),
        node: mesh.node().clone(),
        channel: joined_channel.map(|(c, _)| c),
        subnet_left: subnet_left.clone(),
    });
    let (stop_tx, mut stop_rx) = mpsc::channel(1);
    let server = tokio::spawn(serve_control(listener, secret, state_ctl.clone(), stop_tx));
    zeroize_slice(&mut secret);

    emit_stream_row(
        fmt,
        &UpEvent {
            event: "ready",
            node: &report,
            state_dir: state.display().to_string(),
        },
    )
    .map_err(|e| generic(format!("write readiness: {e}")))?;

    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = stop_rx.recv() => {}
    }

    // Drain: stop taking control sessions, stop the node, then remove the
    // endpoint and release the lifetime lock. Identity, PSK and state persist.
    state_ctl.draining.store(true, Ordering::SeqCst);
    server.abort();
    if let Some(enrollment) = enrollment {
        enrollment.shutdown().await;
    }
    let stopped = tokio::time::timeout(MESH_SHUTDOWN_TIMEOUT, mesh.shutdown()).await;
    let _ = std::fs::remove_file(&control_path);
    if let Some(task) = joined_link {
        task.abort();
    }
    // Release the join's storage lock before the lifetime lock, even if a
    // control session task still holds the shared state for a moment.
    if let Some(joined) = &joined {
        drop(joined.lock().take());
    }
    drop(joined);
    drop(lifetime_lock);
    drop(storage);
    match stopped {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return Err(generic(format!("mesh shutdown: {e}"))),
        Err(_) => return Err(timeout("mesh shutdown did not complete in time")),
    }
    emit_stream_row(
        fmt,
        &UpEvent {
            event: "stopped",
            node: &report,
            state_dir: state.display().to_string(),
        },
    )
    .map_err(|e| generic(format!("write stop event: {e}")))
}

// ---- node status -------------------------------------------------------------

#[derive(Serialize)]
struct StatusView {
    state: String,
    state_dir: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    node: Option<NodeReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
    /// A joined node's current subnet leaf expiry (moves on renewal).
    #[serde(skip_serializing_if = "Option::is_none")]
    subnet_expires_at: Option<u64>,
    /// A joined node's live link to the node it enrolled with.
    #[serde(skip_serializing_if = "Option::is_none")]
    link: Option<serde_json::Value>,
    /// The org this node is a member of (its installed authority).
    #[serde(skip_serializing_if = "Option::is_none")]
    org: Option<String>,
    /// Standalone org links awaiting approval.
    #[serde(skip_serializing_if = "Option::is_none")]
    org_pending: Option<u64>,
}

async fn observe(state: &Path) -> Result<StatusView, CliError> {
    let dir = state.join(NODE_SUBDIR);
    let view = |s: &str, node: Option<NodeReport>, detail: Option<&str>| StatusView {
        state: s.to_string(),
        state_dir: state.display().to_string(),
        node,
        detail: detail.map(str::to_string),
        subnet_expires_at: None,
        link: None,
        org: None,
        org_pending: None,
    };
    Ok(match probe(&dir)? {
        Liveness::Absent => view(
            "stopped",
            None,
            Some("no node has run with this state directory"),
        ),
        Liveness::Free if dir.join(CONTROL_FILE).exists() => view(
            "stale_metadata",
            None,
            Some("a control file remains but no process holds the lifetime lock"),
        ),
        Liveness::Free => view("stopped", None, None),
        Liveness::Held => match control_call(&dir, serde_json::json!({ "op": "status" })).await {
            Ok((_, reply)) => {
                let node = serde_json::from_value::<NodeReport>(reply["node"].clone()).ok();
                let s = reply["state"].as_str().unwrap_or("unknown").to_string();
                StatusView {
                    subnet_expires_at: reply["subnet_expires_at"].as_u64(),
                    link: Some(reply["link"].clone()).filter(|l| !l.is_null()),
                    org: reply["org"].as_str().map(str::to_string),
                    org_pending: reply["org_pending"].as_u64(),
                    ..view(&s, node, None)
                }
            }
            Err(ControlError::NoControlFile) => view(
                "starting",
                None,
                Some("the lifetime lock is held but the control endpoint is not published yet"),
            ),
            Err(ControlError::Unauthenticated) => view(
                "unknown",
                None,
                Some("the control endpoint did not authenticate; not trusting it"),
            ),
            Err(e) => view("unknown", None, Some(&format!("control endpoint: {e:?}"))),
        },
    })
}

/// Report the selected node's verified state.
pub async fn run_status(
    args: StatusArgs,
    output: Option<OutputFormat>,
    profile_name: &str,
) -> Result<(), CliError> {
    let state = state_dir(args.state_dir, profile_name)?;
    let view = observe(&state).await?;
    emit_value(OutputFormat::resolve_oneshot(output), &view)
        .map_err(|e| generic(format!("write status: {e}")))
}

// ---- down ----------------------------------------------------------------------

#[derive(Serialize)]
struct DownView {
    state: &'static str,
    was_running: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    incarnation: Option<String>,
    stale_metadata: bool,
}

/// Ask the selected node to drain and stop, then verify it released its
/// lifetime lock. Never uses a PID; never revokes or deletes anything.
pub async fn run_down(
    args: DownArgs,
    output: Option<OutputFormat>,
    profile_name: &str,
) -> Result<(), CliError> {
    let state = state_dir(args.state_dir, profile_name)?;
    let dir = state.join(NODE_SUBDIR);
    let fmt = OutputFormat::resolve_oneshot(output);
    let emit = |v: &DownView| emit_value(fmt, v).map_err(|e| generic(format!("write result: {e}")));
    if probe(&dir)? != Liveness::Held {
        return emit(&DownView {
            state: "stopped",
            was_running: false,
            incarnation: None,
            stale_metadata: dir.join(CONTROL_FILE).exists(),
        });
    }
    let (control, reply) = control_call(&dir, serde_json::json!({ "op": "shutdown" })).await.map_err(|e| {
        connection_failure(format!(
            "the node holds its lifetime lock but its control endpoint failed ({e:?}); it was not stopped"
        ))
    })?;
    if reply["accepted"] != serde_json::Value::Bool(true)
        || reply["incarnation"].as_str() != Some(control.incarnation.as_str())
    {
        return Err(generic(
            "the node did not accept the shutdown request for the recorded incarnation",
        ));
    }
    let deadline = tokio::time::Instant::now() + args.wait;
    while tokio::time::Instant::now() < deadline {
        if probe(&dir)? != Liveness::Held {
            return emit(&DownView {
                state: "stopped",
                was_running: true,
                incarnation: Some(control.incarnation),
                stale_metadata: false,
            });
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Err(timeout(format!(
        "node incarnation {} accepted shutdown but still holds its lifetime lock after {:?}; state is running or unknown",
        control.incarnation, args.wait
    )))
}

// ---- leave ---------------------------------------------------------------------

/// Leave the mesh this state directory joined. A running joined node records
/// the departure through its own control endpoint (it owns the join state) and
/// then stops; otherwise the join state is opened and updated directly. The
/// device identity is kept; the delivered PSK and contact are erased.
/// `net-mesh org leave`: record that this device leaves its org and stop its
/// running node (its next `up` runs on the mesh without the org). Offline,
/// the departure is recorded directly.
/// `org members` / `subnet members`: ask the node of `--state-dir` what it
/// issued and what it observes for one org or subnet scope.
pub async fn run_members(
    kind: &str,
    target: String,
    state_dir_arg: Option<PathBuf>,
    remote: Option<RemoteMembers>,
    output: Option<OutputFormat>,
    profile_name: &str,
) -> Result<(), CliError> {
    let dir = state_dir(state_dir_arg, profile_name)?.join(NODE_SUBDIR);
    let (_, mut reply) = control_call(
        &dir,
        serde_json::json!({ "op": "members", "kind": kind, "target": target }),
    )
    .await
    .map_err(|e| {
        connection_failure(format!(
            "no running node answered ({e:?}); inventory is read from a running `net-mesh up`"
        ))
    })?;
    if let Some(e) = reply["error"].as_str() {
        return Err(generic(e.to_string()));
    }
    if let Some(remote) = remote {
        reply["remote"] = ask_remote_members(&dir, kind, &target, &reply, remote).await?;
        reply["completeness"]["remote"] = serde_json::json!(
            "only the named nodes, each at its own observed_at; an unanswered node is unknown, not empty"
        );
    }
    emit_value(OutputFormat::resolve_oneshot(output), &reply)
        .map_err(|e| generic(format!("write result: {e}")))
}

/// Named remote enforcement points to ask, and the root to sign with.
pub(crate) struct RemoteMembers {
    /// The subnet authority root, or the org root (as an entity keypair).
    pub(crate) root: net::adapter::net::identity::EntityKeypair,
    /// The subnet authority (subnet queries only).
    pub(crate) authority: Option<net::adapter::net::identity::EntityId>,
    /// `self` or `ENTITY@HOST:PORT#NOISE_PUBKEY`.
    pub(crate) verifiers: Vec<String>,
    /// Per-node answer bound.
    pub(crate) wait: Duration,
}

/// Sign one request per named node, carry each through this node, verify
/// each observation against its own request, and report per node.
async fn ask_remote_members(
    node_dir: &Path,
    kind: &str,
    target: &str,
    local: &serde_json::Value,
    remote: RemoteMembers,
) -> Result<serde_json::Value, CliError> {
    use net_sdk::members::{MembersObservation, MembersOutcome, MembersRequest, MembersTarget};
    let parse_entity = |hex_id: &str| -> Option<net::adapter::net::identity::EntityId> {
        let bytes: [u8; 32] = hex::decode(hex_id).ok()?.try_into().ok()?;
        Some(net::adapter::net::identity::EntityId::from_bytes(bytes))
    };
    let target_of = || -> Result<MembersTarget, CliError> {
        if kind == "subnet" {
            Ok(MembersTarget::Subnet {
                authority: remote
                    .authority
                    .clone()
                    .ok_or_else(|| invalid_args("--authority is required with --verifier"))?,
                scope: super::subnet::parse_subnet_path(target)?,
            })
        } else {
            let bytes: [u8; 32] = hex::decode(target)
                .ok()
                .and_then(|b| b.try_into().ok())
                .ok_or_else(|| invalid_args("bad org id"))?;
            let mut subjects: Vec<net::adapter::net::identity::EntityId> = local["issued"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|r| r["subject"].as_str().and_then(parse_entity))
                .collect();
            subjects.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
            subjects.dedup();
            Ok(MembersTarget::Org {
                org: net::adapter::net::behavior::org::OrgId(bytes),
                subjects,
            })
        }
    };
    let generation_of = |subject: &str| -> Option<u64> {
        local["issued"]
            .as_array()?
            .iter()
            .find(|r| r["subject"].as_str() == Some(subject))
            .and_then(|r| r["approved_generation"].as_u64())
    };
    let mut rows = Vec::new();
    for v in &remote.verifiers {
        let (entity, contact) = if v == "self" {
            let entity = local["responder"]
                .as_str()
                .and_then(parse_entity)
                .ok_or_else(|| generic("the node did not name itself"))?;
            (entity, None)
        } else {
            let c = super::subnet::parse_verifier(v)?;
            (c.entity.clone(), Some(c))
        };
        let request =
            MembersRequest::sign(target_of()?, entity.clone(), &remote.root).map_err(generic)?;
        let mut call = serde_json::json!({
            "op": "members_forward",
            "request": hex::encode(request.to_bytes()),
            "wait_ms": remote.wait.as_millis() as u64,
        });
        if let Some(c) = &contact {
            call["addr"] = serde_json::json!(c.addr.to_string());
            call["noise_pubkey"] = serde_json::json!(hex::encode(c.noise_pubkey));
        }
        let who = hex::encode(entity.as_bytes());
        let row = match control_call_within(node_dir, call, remote.wait + Duration::from_secs(5))
            .await
        {
            Err(e) => {
                serde_json::json!({ "verifier": who, "state": "no_answer", "detail": format!("{e:?}") })
            }
            Ok((_, reply)) => match reply["observation"].as_str() {
                None => serde_json::json!({
                    "verifier": who,
                    "state": if reply["refused"].is_string() { "refused" } else { "no_answer" },
                    "detail": reply["refused"].as_str().or_else(|| reply["no_answer"].as_str()),
                }),
                Some(hex_o) => match hex::decode(hex_o)
                    .map_err(|e| e.to_string())
                    .and_then(|b| MembersObservation::from_bytes(&b))
                    .and_then(|o| o.verify_for(&request).map(|()| o))
                {
                    Err(e) => {
                        serde_json::json!({ "verifier": who, "state": "bad_observation", "detail": e })
                    }
                    Ok(o) => {
                        let admitted: Vec<serde_json::Value> = o
                            .admitted
                            .iter()
                            .map(|p| {
                                serde_json::json!({
                                    "subject": hex::encode(p.subject.as_bytes()),
                                    "attachment": super::subnet::format_subnet(p.attachment),
                                    "rights": super::subnet::format_subnet_rights(p.rights),
                                    "generation": p.generation,
                                    "expires_at": p.expires_at,
                                })
                            })
                            .collect();
                        let standing: Vec<serde_json::Value> = o
                            .floors
                            .iter()
                            .map(|(s, floor)| {
                                let subject = hex::encode(s.as_bytes());
                                let standing = match generation_of(&subject) {
                                    Some(g) if u64::from(*floor) > g => "revoked_there",
                                    Some(_) => "admissible_there",
                                    None => "unknown_generation",
                                };
                                serde_json::json!({ "subject": subject, "floor": floor, "standing": standing })
                            })
                            .collect();
                        serde_json::json!({
                            "verifier": who,
                            "state": o.outcome.as_str(),
                            "observed_at": o.observed_at,
                            "admitted": if o.outcome == MembersOutcome::Observed && kind == "subnet" { Some(admitted) } else { None },
                            "standing": if o.outcome == MembersOutcome::Observed && kind == "org" { Some(standing) } else { None },
                        })
                    }
                },
            },
        };
        rows.push(row);
    }
    Ok(serde_json::Value::Array(rows))
}

pub async fn run_org_leave(
    state_dir_arg: Option<PathBuf>,
    wait: Duration,
    output: Option<OutputFormat>,
    profile_name: &str,
) -> Result<(), CliError> {
    let state = state_dir(state_dir_arg, profile_name)?;
    let authority = state.join(AUTHORITY_SUBDIR);
    let dir = state.join(NODE_SUBDIR);
    let (org, newly, left_at, runtime, incarnation) = if probe(&dir)? == Liveness::Held {
        let (control, reply) = control_call(&dir, serde_json::json!({ "op": "org_leave" }))
            .await
            .map_err(|e| {
                connection_failure(format!(
                    "the node holds its lifetime lock but its control endpoint failed ({e:?}); nothing was changed"
                ))
            })?;
        if let Some(err) = reply["error"].as_str() {
            return Err(generic(err.to_string()));
        }
        if reply["left"] != serde_json::Value::Bool(true)
            || reply["incarnation"].as_str() != Some(control.incarnation.as_str())
        {
            return Err(generic(
                "the node did not confirm the departure for the recorded incarnation",
            ));
        }
        let runtime = if reply["stopping"] == serde_json::Value::Bool(true) {
            let deadline = tokio::time::Instant::now() + wait;
            loop {
                if probe(&dir)? != Liveness::Held {
                    break;
                }
                if tokio::time::Instant::now() >= deadline {
                    return Err(timeout(format!(
                        "the departure is recorded, but node incarnation {} still holds its lifetime lock after {wait:?}; runtime stop unconfirmed",
                        control.incarnation
                    )));
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            "stopped"
        } else {
            "unchanged (already running without the org)"
        };
        (
            reply["org"].as_str().unwrap_or_default().to_string(),
            reply["newly_left"].as_bool().unwrap_or(false),
            reply["left_at"].as_u64(),
            runtime,
            Some(control.incarnation),
        )
    } else {
        let a = authority.clone();
        let recorded = tokio::task::spawn_blocking(move || {
            if let Some(left) = read_org_left(&a) {
                return Ok((
                    left["org"].as_str().unwrap_or_default().to_string(),
                    false,
                    left["left_at"].as_u64(),
                ));
            }
            let org = owner_org_on_disk(&a)
                .ok_or_else(|| "this device is not an org member; nothing to leave".to_string())?;
            let now = now_unix();
            record_org_left(&a, &org, now)
                .map_err(|e| format!("the departure was not recorded: {e}"))?;
            Ok::<_, String>((org, true, Some(now)))
        })
        .await
        .map_err(|e| generic(format!("org leave task failed: {e}")))?
        .map_err(generic)?;
        (recorded.0, recorded.1, recorded.2, "not running", None)
    };
    emit_value(
        OutputFormat::resolve_oneshot(output),
        &serde_json::json!({
            "state": "left",
            "org": org,
            "newly_left": newly,
            "left_at": left_at,
            "runtime": runtime,
            "incarnation": incarnation,
            // What this command does not do, stated rather than implied.
            "mesh": "unaffected: `net-mesh up` runs on the mesh without the org",
            "authority": "not notified: the org still accepts this device's certificate until the operator runs `org remove`",
            "rejoin": "a new link approved with the org root (`org join`)",
        }),
    )
    .map_err(|e| generic(format!("write result: {e}")))
}

pub async fn run_leave(
    args: LeaveArgs,
    output: Option<OutputFormat>,
    profile_name: &str,
) -> Result<(), CliError> {
    use net_sdk::enrollment::device::{DeviceJoin, DeviceJoinError};

    let state = state_dir(args.state_dir, profile_name)?;
    let join_dir = state.join(super::enrollment::JOIN_SUBDIR);
    if !join_dir.exists() {
        return Err(invalid_args(format!(
            "{} has not joined a mesh (no join state); nothing to leave",
            state.display()
        )));
    }
    let dir = state.join(NODE_SUBDIR);
    // A running joined node withdraws its channel subscription (acknowledged
    // by the publisher, or not) before it stops.
    let mut channel_unsubscribed = serde_json::Value::Null;
    let (newly, left_at, runtime, incarnation) = if probe(&dir)? == Liveness::Held {
        let (control, reply) = control_call(&dir, serde_json::json!({ "op": "leave" }))
            .await
            .map_err(|e| {
                connection_failure(format!(
                    "the node holds its lifetime lock but its control endpoint failed ({e:?}); nothing was changed"
                ))
            })?;
        if let Some(err) = reply["error"].as_str() {
            return Err(generic(err.to_string()));
        }
        if reply["left"] != serde_json::Value::Bool(true)
            || reply["incarnation"].as_str() != Some(control.incarnation.as_str())
        {
            return Err(generic(
                "the node did not confirm the departure for the recorded incarnation",
            ));
        }
        channel_unsubscribed = reply["channel_unsubscribed"].clone();
        let deadline = tokio::time::Instant::now() + args.wait;
        loop {
            if probe(&dir)? != Liveness::Held {
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(timeout(format!(
                    "the departure is recorded, but node incarnation {} still holds its lifetime lock after {:?}; runtime stop unconfirmed",
                    control.incarnation, args.wait
                )));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        (
            reply["newly_left"].as_bool().unwrap_or(false),
            reply["left_at"].as_u64(),
            "stopped",
            Some(control.incarnation),
        )
    } else {
        let jd = join_dir.clone();
        let recorded = tokio::task::spawn_blocking(move || {
            let mut join = DeviceJoin::open(&jd)?;
            let newly = join.leave(now_unix())?;
            Ok::<_, DeviceJoinError>((newly, join.left_at()))
        })
        .await
        .map_err(|e| generic(format!("leave task failed: {e}")))?;
        let (newly, left_at) = recorded.map_err(|e| match e {
            DeviceJoinError::Storage(StorageError::Busy) => generic(format!(
                "join state {} is in use by another process (a `join` in progress?); nothing was changed, retry",
                join_dir.display()
            )),
            other => generic(format!("join state {}: {other}", join_dir.display())),
        })?;
        (newly, left_at, "not_running", None)
    };
    emit_value(
        OutputFormat::resolve_oneshot(output),
        &serde_json::json!({
            "state": "left",
            "newly_left": newly,
            "left_at": left_at,
            "credentials": "erased",
            "runtime": runtime,
            "incarnation": incarnation,
            "channel_unsubscribed": channel_unsubscribed,
            // What this command cannot know or do, stated rather than implied.
            "unmanaged_consumers": "unknown: copies of the PSK held by other processes are not tracked",
            "authority": "not notified: leaving is local; the issuer revokes separately",
            "rejoin": "net-mesh join <token> --rejoin (the issuer must still authorize it)",
        }),
    )
    .map_err(|e| generic(format!("write result: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_state_v3_round_trips_and_v1_v2_state_still_reads() {
        let v3 = NodeSecrets {
            seed: [1; 32],
            psk: Some([2; 32]),
            issuer: Some([3; 32]),
            enroll_port: Some(7443),
            noise: Some([6; 32]),
        };
        let back = NodeSecrets::decode(v3.encode().as_slice()).unwrap();
        assert_eq!(
            (
                back.seed,
                back.psk,
                back.issuer,
                back.enroll_port,
                back.noise
            ),
            (
                [1; 32],
                Some([2; 32]),
                Some([3; 32]),
                Some(7443),
                Some([6; 32])
            )
        );

        // A v2 snapshot (before the Noise key was kept) still reads, keyless.
        let mut v2 = SECRETS_MAGIC.to_vec();
        v2.extend_from_slice(&2u16.to_le_bytes());
        v2.extend_from_slice(&[7; 32]);
        v2.push(0);
        v2.push(1);
        v2.extend_from_slice(&[8; 32]);
        v2.extend_from_slice(&9000u16.to_le_bytes());
        let sum = blake3::derive_key(SECRETS_CHECKSUM, &v2);
        v2.extend_from_slice(&sum);
        let old = NodeSecrets::decode(&v2).unwrap();
        assert_eq!(
            (old.seed, old.psk, old.issuer, old.enroll_port, old.noise),
            ([7; 32], None, Some([8; 32]), Some(9000), None)
        );

        // A v1 snapshot (before enrollment fields existed) is still accepted.
        let mut v1 = SECRETS_MAGIC.to_vec();
        v1.extend_from_slice(&1u16.to_le_bytes());
        v1.extend_from_slice(&[4; 32]);
        v1.push(1);
        v1.extend_from_slice(&[5; 32]);
        let sum = blake3::derive_key(SECRETS_CHECKSUM, &v1);
        v1.extend_from_slice(&sum);
        let old = NodeSecrets::decode(&v1).unwrap();
        assert_eq!(
            (old.seed, old.psk, old.issuer, old.enroll_port),
            ([4; 32], Some([5; 32]), None, None)
        );

        // Any altered byte is refused rather than repaired.
        let mut bad = v3.encode().as_slice().to_vec();
        bad[40] ^= 1;
        assert!(NodeSecrets::decode(&bad).is_err());
    }

    #[tokio::test]
    async fn control_messages_are_encrypted_and_bound_to_direction_and_sequence() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (mut node, _) = listener.accept().await.unwrap();
        let keys = SessionKeys::derive(&[7; 32], &[1; 32], &[2; 32]);
        let secret = b"netmesh-join_SECRET-TOKEN-MARKER";

        // The bytes on the socket never contain the plaintext.
        write_msg(&mut client, &keys, TO_NODE, 0, secret)
            .await
            .unwrap();
        let mut len = [0u8; 4];
        node.read_exact(&mut len).await.unwrap();
        let mut raw = vec![0u8; u32::from_be_bytes(len) as usize];
        node.read_exact(&mut raw).await.unwrap();
        assert!(!raw.windows(secret.len()).any(|w| w == secret));

        // Round trip with the right keys, direction and sequence.
        write_msg(&mut client, &keys, TO_NODE, 1, secret)
            .await
            .unwrap();
        assert_eq!(
            read_msg(&mut node, &keys, TO_NODE, 1).await.unwrap(),
            secret
        );
        // A replayed or reordered frame fails authentication.
        write_msg(&mut client, &keys, TO_NODE, 2, secret)
            .await
            .unwrap();
        assert!(read_msg(&mut node, &keys, TO_NODE, 3).await.is_err());
    }
}
