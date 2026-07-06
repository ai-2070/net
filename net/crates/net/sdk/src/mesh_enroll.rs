//! Live enrollment over the mesh — `mesh.serve_enrollment` (operator) +
//! `mesh.join` (device), the networked half of Hermes V2 Phase 1 (Slice B2b).
//!
//! The wire is **direct-addressed nRPC**, reusing the mesh's own handshake and
//! request/response path rather than a parallel channel:
//!
//! * **Operator** — builds a node as the mesh **root**, `start()`s it, and
//!   registers [`OperatorEnrollment::handle_join_request`] as the
//!   [`ENROLLMENT_SERVICE`] via [`Mesh::serve_enrollment`]. Its running
//!   dispatch loop completes routed handshakes from devices it has never met.
//! * **Device** — [`Mesh::join`] decodes the invite string, dials the operator
//!   with [`Mesh::connect_via`] (the **routed** handshake — the joiner's
//!   `node_id` rides inside msg1, so the operator needs no pre-`accept`), then
//!   [`call`](crate::mesh::Mesh)s the enrollment service with its signed
//!   [`JoinRequest`] and verifies the returned grant
//!   ([`JoinOutcome::into_chain`]).
//!
//! The invite's `root` (ed25519) anchors the *delegation*, while the
//! [`Rendezvous`] encoded into `rendezvous` carries the operator node's
//! *transport* coordinates (address + Noise static key + node id) — a different
//! keypair than the ed25519 identity, so both are needed. The device's own node
//! identity is the key being enrolled, so the returned chain's leaf is exactly
//! `mesh.identity()`.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use crate::delegation::DelegationChain;
use crate::enrollment::{EnrollmentError, InviteToken, JoinError, JoinOutcome, JoinRequest};
use crate::mesh::Mesh;
use crate::mesh_rpc::{CallOptionsTyped, Codec, ServeError, ServeHandle};
use crate::operator::OperatorEnrollment;

/// The nRPC service name the operator serves and the device calls.
pub const ENROLLMENT_SERVICE: &str = "net.mesh.enroll";

/// How a joining device reaches the operator's node — the transport
/// coordinates encoded into an invite's `rendezvous` field.
///
/// This is deliberately separate from the invite's `root` (the ed25519 mesh
/// identity that anchors the *delegation*): the mesh handshake uses a **Noise
/// static key** (X25519), a different keypair than the ed25519 entity id. The
/// locator is only *where to ask* — a tampered rendezvous can point a device at
/// an attacker's node, but that node can't forge a `root → device` grant
/// anchored at the real `root`, so [`JoinOutcome::into_chain`] rejects it. The
/// mesh PSK is an out-of-band build-time property of both nodes, not carried
/// here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rendezvous {
    /// The operator node's reachable socket address.
    pub addr: String,
    /// The operator node's Noise static public key (from `Mesh::public_key`).
    pub noise_pubkey: [u8; 32],
    /// The operator node's routing node id (from `Mesh::node_id`).
    pub node_id: u64,
}

impl Rendezvous {
    /// Encode as an invite `rendezvous` string: `addr|<noise-pubkey-hex>|node_id`.
    pub fn encode(&self) -> String {
        format!(
            "{}|{}|{}",
            self.addr,
            hex32(&self.noise_pubkey),
            self.node_id
        )
    }

    /// Parse a `rendezvous` string produced by [`Self::encode`].
    pub fn decode(s: &str) -> Option<Self> {
        let mut it = s.split('|');
        let addr = it.next()?.to_string();
        let noise_pubkey = unhex32(it.next()?)?;
        let node_id = it.next()?.parse::<u64>().ok()?;
        if it.next().is_some() || addr.is_empty() {
            return None;
        }
        Some(Self {
            addr,
            noise_pubkey,
            node_id,
        })
    }
}

fn hex32(b: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for byte in b {
        s.push(char::from_digit((byte >> 4) as u32, 16).expect("nibble is 0..16"));
        s.push(char::from_digit((byte & 0x0f) as u32, 16).expect("nibble is 0..16"));
    }
    s
}

fn unhex32(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 {
        return None;
    }
    let bytes = s.as_bytes();
    let mut out = [0u8; 32];
    for (i, slot) in out.iter_mut().enumerate() {
        let hi = (bytes[i * 2] as char).to_digit(16)?;
        let lo = (bytes[i * 2 + 1] as char).to_digit(16)?;
        *slot = ((hi << 4) | lo) as u8;
    }
    Some(out)
}

/// Errors from the device-side [`Mesh::join`] flow.
#[derive(Debug, thiserror::Error)]
pub enum JoinFlowError {
    /// The invite string didn't decode.
    #[error("invalid invite string: {0}")]
    Decode(EnrollmentError),
    /// This mesh was built without an identity, so there's no device key to
    /// enroll.
    #[error("this mesh has no device identity to enroll (build it with an identity)")]
    NoIdentity,
    /// Dialing the operator or calling the enrollment service failed.
    #[error("enrollment transport failed: {0}")]
    Transport(String),
    /// The operator rejected the request, or the returned grant didn't verify.
    #[error(transparent)]
    Outcome(JoinError),
}

impl Mesh {
    /// The [`Rendezvous`] locator for *this* node, encoded as an invite
    /// `rendezvous` string — hand it to `OperatorEnrollment::invite` so joining
    /// devices can dial back. Combines this node's address, Noise static key,
    /// and node id.
    pub fn rendezvous_string(&self) -> String {
        Rendezvous {
            addr: self.local_addr().to_string(),
            noise_pubkey: *self.public_key(),
            node_id: self.node_id(),
        }
        .encode()
    }

    /// **Operator side.** Register the enrollment service, gating every valid
    /// request through `approver` before it's admitted — the V2 threat model
    /// ("a leaked invite lets someone *ask*, never admits them").
    ///
    /// `approver` is handed the (already-validated) [`JoinRequest`] so it can
    /// surface the asker's device id / name / tags to the operator (a
    /// Telegram/desktop prompt, a policy check, …) and returns `true` to admit.
    /// A denial answers the device a coded `Rejected` and **doesn't** burn the
    /// single-use invite, so the real device can still use it. `operator`
    /// mints/signs against the mesh root; `grant_ttl` / `max_depth` shape each
    /// `root → device` grant.
    ///
    /// Hold the returned [`ServeHandle`] for as long as enrollment should stay
    /// open — dropping it unregisters the service. The node must be `start()`ed
    /// for its dispatch loop to answer. For headless auto-admission (invite is
    /// the authorization), use [`Self::serve_enrollment_auto`].
    pub fn serve_enrollment<F, Fut>(
        &self,
        operator: Arc<OperatorEnrollment>,
        grant_ttl: Duration,
        max_depth: u8,
        approver: F,
    ) -> Result<ServeHandle, ServeError>
    where
        F: Fn(JoinRequest) -> Fut + Clone + Send + Sync + 'static,
        Fut: Future<Output = bool> + Send + 'static,
    {
        self.serve_rpc_typed(ENROLLMENT_SERVICE, Codec::Json, move |req: Vec<u8>| {
            let operator = operator.clone();
            let approver = approver.clone();
            async move {
                // Never fails out of band: a malformed request, a denial, or a
                // rejected check is a coded `JoinOutcome::Rejected` the device
                // reads, so always `Ok(bytes)`.
                Ok::<Vec<u8>, String>(
                    operator
                        .handle_join_request_approved(&req, grant_ttl, max_depth, approver)
                        .await,
                )
            }
        })
    }

    /// **Operator side, headless.** Register the enrollment service that
    /// **auto-admits any valid single-use invite** — the invite *is* the
    /// authorization. Weaker than [`Self::serve_enrollment`]: a leaked, still
    /// valid invite gets its holder *admitted*, not just able to ask. Use only
    /// where no operator surface exists (a scripted fleet enroll) and the
    /// invite's short TTL + single-use are the whole gate.
    pub fn serve_enrollment_auto(
        &self,
        operator: Arc<OperatorEnrollment>,
        grant_ttl: Duration,
        max_depth: u8,
    ) -> Result<ServeHandle, ServeError> {
        self.serve_rpc_typed(ENROLLMENT_SERVICE, Codec::Json, move |req: Vec<u8>| {
            let operator = operator.clone();
            async move {
                Ok::<Vec<u8>, String>(operator.handle_join_request(&req, grant_ttl, max_depth))
            }
        })
    }

    /// **Device side.** Enroll this node into the mesh named by `invite`,
    /// under `name` + `tags`, returning the verified `root → device`
    /// [`DelegationChain`].
    ///
    /// The node must already be `start()`ed (the routed handshake needs the
    /// local dispatch loop). The enrolled key is this mesh's own identity, so
    /// the returned chain's leaf is `self.identity()`.
    pub async fn join(
        &self,
        invite: &str,
        name: impl Into<String>,
        tags: Vec<String>,
    ) -> Result<DelegationChain, JoinFlowError> {
        let invite = InviteToken::decode(invite).map_err(JoinFlowError::Decode)?;
        let device = self.identity().cloned().ok_or(JoinFlowError::NoIdentity)?;
        let rv = Rendezvous::decode(&invite.rendezvous)
            .ok_or_else(|| JoinFlowError::Transport("malformed rendezvous locator".into()))?;

        // Attach to the operator's running node. `connect_via` is the routed
        // handshake — no pre-`accept` on the operator's side. The handshake uses
        // the node's Noise static key (not the ed25519 mesh root).
        self.connect_via(&rv.addr, &rv.noise_pubkey, rv.node_id)
            .await
            .map_err(|e| JoinFlowError::Transport(format!("connect: {e}")))?;

        let request = JoinRequest::create(&device, name, tags, &invite);
        let response: Vec<u8> = self
            .call_typed(
                rv.node_id,
                ENROLLMENT_SERVICE,
                &request.to_bytes(),
                CallOptionsTyped::default(),
            )
            .await
            .map_err(|e| JoinFlowError::Transport(format!("call: {e}")))?;

        JoinOutcome::from_bytes(&response)
            .and_then(|outcome| outcome.into_chain(device.entity_id(), &invite.root))
            .map_err(JoinFlowError::Outcome)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::delegation::DEFAULT_DELEGATION_DEPTH;
    use crate::mesh::MeshBuilder;
    use crate::Identity;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_device_enrolls_over_the_mesh_and_the_operator_records_it() {
        let psk = [0x42u8; 32];
        let root = Identity::generate();

        // Operator node runs as the mesh root, serving enrollment.
        let operator_mesh = MeshBuilder::new("127.0.0.1:0", &psk)
            .unwrap()
            .identity(root.clone())
            .build()
            .await
            .unwrap();
        operator_mesh.start();

        let dir = tempfile::tempdir().unwrap();
        let op = Arc::new(OperatorEnrollment::new(
            root.clone(),
            dir.path().join("devices.json"),
            dir.path().join("revocations.json"),
        ));
        let _handle = operator_mesh
            .serve_enrollment(
                op.clone(),
                Duration::from_secs(3600),
                DEFAULT_DELEGATION_DEPTH,
                // Approve every request in this happy-path test.
                |_req| async { true },
            )
            .expect("serve enrollment");

        // The invite's rendezvous is the operator node's transport locator
        // (addr + Noise pubkey + node id).
        let invite = op.invite(operator_mesh.rendezvous_string(), Duration::from_secs(300));
        let invite_str = invite.encode();

        // A fresh device generates its own key, joins over the wire.
        let device_id = Identity::generate();
        let device_mesh = MeshBuilder::new("127.0.0.1:0", &psk)
            .unwrap()
            .identity(device_id.clone())
            .build()
            .await
            .unwrap();
        device_mesh.start();

        let chain = device_mesh
            .join(&invite_str, "pc", vec!["region:office".into()])
            .await
            .expect("device enrolls over the mesh");

        // The returned grant binds to this device and anchors at the root.
        assert_eq!(&chain.leaf(), device_id.entity_id());
        assert_eq!(&chain.root(), root.entity_id());

        // The operator recorded the device in its inventory.
        let devices = op.devices().unwrap();
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].name, "pc");
        assert_eq!(&devices[0].device, device_id.entity_id());
        assert!(!devices[0].is_revoked());

        // The invite was single-use — a replay of the same string now fails.
        let replay = device_mesh.join(&invite_str, "pc", vec![]).await;
        assert!(replay.is_err(), "single-use invite must not enroll twice");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_denied_join_is_rejected_and_leaves_the_invite_usable() {
        use crate::enrollment::{reject, JoinError};

        let psk = [0x37u8; 32];
        let root = Identity::generate();
        let operator_mesh = MeshBuilder::new("127.0.0.1:0", &psk)
            .unwrap()
            .identity(root.clone())
            .build()
            .await
            .unwrap();
        operator_mesh.start();

        let dir = tempfile::tempdir().unwrap();
        let op = Arc::new(OperatorEnrollment::new(
            root.clone(),
            dir.path().join("devices.json"),
            dir.path().join("revocations.json"),
        ));
        // The operator denies this request.
        let _handle = operator_mesh
            .serve_enrollment(
                op.clone(),
                Duration::from_secs(3600),
                DEFAULT_DELEGATION_DEPTH,
                |_req| async { false },
            )
            .expect("serve enrollment");

        let invite = op.invite(operator_mesh.rendezvous_string(), Duration::from_secs(300));
        let invite_str = invite.encode();

        let device_mesh = MeshBuilder::new("127.0.0.1:0", &psk)
            .unwrap()
            .identity(Identity::generate())
            .build()
            .await
            .unwrap();
        device_mesh.start();

        let err = device_mesh
            .join(&invite_str, "pc", vec![])
            .await
            .expect_err("a denied join must fail");
        assert!(matches!(
            err,
            JoinFlowError::Outcome(JoinError::Rejected { code, .. }) if code == reject::DENIED
        ));

        // Denial admitted nobody and did NOT burn the invite — it's still
        // pending for the real device.
        assert!(op.devices().unwrap().is_empty());
        assert_eq!(op.pending_invites(0).len(), 1);
    }
}
