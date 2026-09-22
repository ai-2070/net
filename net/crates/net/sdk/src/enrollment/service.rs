// SPDX-License-Identifier: MIT OR Apache-2.0
//! Enrollment redemption service: the TCP listener side of [`super::redeem`].
//!
//! One service owns one issuer's shared [`EnrollmentLedger`]. For each session
//! it completes the PSK-free Noise handshake as the invite-pinned responder,
//! reads one request, and verifies — before any ledger mutation — the invite
//! signature and issuer, that the invite is byte-identical to the recorded one,
//! the intent against that invite, and the subject's signature over this
//! session's transcript. Only then does it claim, issue (through the caller's
//! [`BundleIssuer`]) or recover, and answer on the same session.
//!
//! Bounded: concurrent sessions, per-session deadline, frame and request sizes.
//! Excess sessions are closed without a response. The service exposes no
//! management operation; the owner uses [`EnrollmentService::ledger`] locally.
//! Stopping the service stops redemption only.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Notify, Semaphore};
use tokio::task::JoinHandle;

use super::invite::{EnrollmentKey, MembershipInvite, RedemptionIntent};
use super::policy::PolicyError;
use super::redeem::{
    decode_request, encode_response, read_frame, send_sealed, session_hash, transcript,
    write_frame, RedeemOutcome, Refusal, ResponderKey, MAX_BUNDLE_BYTES, MAX_HANDSHAKE_FRAME,
    MAX_REQUEST_BYTES,
};
use super::store::{ClaimOutcome, EnrollmentLedger, LedgerError};
use crate::identity::Identity;

/// The ledger shared by the service and its local owner (approve/revoke/offer).
pub type SharedLedger = Arc<Mutex<EnrollmentLedger>>;

/// Produces the secret bundle for a committed claim. Called with the ledger
/// lock held, so it must be local and bounded (sign, read local state) — never
/// a network or human wait.
pub trait BundleIssuer: Send + Sync + 'static {
    /// Build the bundle for this ready claim. Nothing is delivered or recorded
    /// unless the ledger then commits it; an error leaves the claim resumable.
    fn issue(
        &self,
        invite: &MembershipInvite,
        intent: &RedemptionIntent,
    ) -> Result<Vec<u8>, Refusal>;

    /// Whether an already committed bundle may still be delivered (e.g. the
    /// device's membership or the transport secret has not been revoked since).
    fn may_recover(
        &self,
        invite: &MembershipInvite,
        intent: &RedemptionIntent,
    ) -> Result<(), Refusal>;
}

/// Service bounds.
#[derive(Clone, Copy, Debug)]
pub struct ServiceConfig {
    /// Maximum concurrent sessions; excess connections are closed immediately.
    pub max_sessions: usize,
    /// Deadline for one whole session, handshake to response.
    pub session_timeout: Duration,
}

impl Default for ServiceConfig {
    fn default() -> Self {
        Self {
            max_sessions: 64,
            session_timeout: Duration::from_secs(10),
        }
    }
}

/// Failure to start the service.
#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    /// The ledger belongs to a different issuer than the serving identity.
    #[error("ledger issuer does not match the serving identity")]
    IssuerMismatch,
    /// Invalid configuration (zero sessions or zero timeout).
    #[error("invalid enrollment service configuration")]
    InvalidConfig,
    /// Binding the listener failed.
    #[error("enrollment listener: {0}")]
    Io(#[from] std::io::Error),
}

/// A running enrollment listener. Dropping it without [`Self::shutdown`] also
/// stops accepting (the accept task is aborted).
pub struct EnrollmentService {
    local_addr: SocketAddr,
    key: EnrollmentKey,
    ledger: SharedLedger,
    stop: Arc<Notify>,
    task: Option<JoinHandle<()>>,
}

impl core::fmt::Debug for EnrollmentService {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("EnrollmentService")
            .field("local_addr", &self.local_addr)
            .field("key", &self.key)
            .finish()
    }
}

impl EnrollmentService {
    /// Bind `addr` and serve redemptions for `issuer` from `ledger`.
    /// Ready (accepting) when this returns.
    pub async fn bind(
        addr: SocketAddr,
        issuer: &Identity,
        ledger: SharedLedger,
        bundles: Arc<dyn BundleIssuer>,
        config: ServiceConfig,
    ) -> Result<Self, ServiceError> {
        if config.max_sessions == 0 || config.session_timeout.is_zero() {
            return Err(ServiceError::InvalidConfig);
        }
        if ledger.lock().issuer() != issuer.entity_id() {
            return Err(ServiceError::IssuerMismatch);
        }
        let key = ResponderKey::derive(issuer);
        let listener = TcpListener::bind(addr).await?;
        let local_addr = listener.local_addr()?;
        let stop = Arc::new(Notify::new());
        let ctx = Arc::new(Ctx {
            key: key.clone(),
            ledger: ledger.clone(),
            bundles,
            timeout: config.session_timeout,
        });
        let task = tokio::spawn(accept_loop(
            listener,
            ctx,
            Arc::new(Semaphore::new(config.max_sessions)),
            stop.clone(),
        ));
        Ok(Self {
            local_addr,
            key: key.public(),
            ledger,
            stop,
            task: Some(task),
        })
    }

    /// Bound address (use for the invite endpoint when binding port 0).
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Public key this service proves; sign it into invitations.
    pub fn enrollment_key(&self) -> EnrollmentKey {
        self.key
    }

    /// The shared ledger, for the local owner's offer/approve/deny/revoke.
    pub fn ledger(&self) -> &SharedLedger {
        &self.ledger
    }

    /// Stop accepting new sessions and wait for the accept loop to exit.
    /// Sessions already running finish or hit their own deadline.
    pub async fn shutdown(mut self) {
        self.stop.notify_one();
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

impl Drop for EnrollmentService {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

struct Ctx {
    key: ResponderKey,
    ledger: SharedLedger,
    bundles: Arc<dyn BundleIssuer>,
    timeout: Duration,
}

async fn accept_loop(
    listener: TcpListener,
    ctx: Arc<Ctx>,
    permits: Arc<Semaphore>,
    stop: Arc<Notify>,
) {
    loop {
        let stream = tokio::select! {
            _ = stop.notified() => return,
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => stream,
                // Transient accept errors (e.g. fd exhaustion) must not kill
                // the listener; back off briefly instead of spinning.
                Err(_) => {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
            },
        };
        // Capacity is refused by closing, never by queueing unbounded work.
        let Ok(permit) = permits.clone().try_acquire_owned() else {
            drop(stream);
            continue;
        };
        let ctx = ctx.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let _ = tokio::time::timeout(ctx.timeout, session(stream, &ctx)).await;
        });
    }
}

/// One session. Any protocol or I/O failure closes it without a response.
async fn session(mut stream: TcpStream, ctx: &Arc<Ctx>) -> std::io::Result<()> {
    let bad = || std::io::Error::new(std::io::ErrorKind::InvalidData, "enrollment handshake");
    let mut hs = ctx.key.responder().map_err(|_| bad())?;
    let Some(msg1) = read_frame(&mut stream, MAX_HANDSHAKE_FRAME).await? else {
        return Ok(());
    };
    let mut buf = vec![0u8; MAX_HANDSHAKE_FRAME];
    let n = hs.read_message(&msg1, &mut buf).map_err(|_| bad())?;
    if n != 0 {
        return Err(bad());
    }
    let n = hs.write_message(&[], &mut buf).map_err(|_| bad())?;
    write_frame(&mut stream, &buf[..n]).await?;
    let hh = session_hash(&hs).ok_or_else(bad)?;
    let mut transport = hs.into_transport_mode().map_err(|_| bad())?;

    let Some(sealed) = read_frame(&mut stream, MAX_REQUEST_BYTES + 16).await? else {
        return Ok(());
    };
    let mut plain = vec![0u8; sealed.len()];
    let n = transport
        .read_message(&sealed, &mut plain)
        .map_err(|_| bad())?;
    plain.truncate(n);

    let ctx2 = ctx.clone();
    let result = tokio::task::spawn_blocking(move || decide(&ctx2, &hh, &plain))
        .await
        .unwrap_or(Err(Refusal::Unavailable));
    send_sealed(&mut stream, &mut transport, &encode_response(&result)).await
}

/// Verify everything, then apply the ledger transition. Blocking (ledger I/O).
fn decide(ctx: &Ctx, hh: &[u8; 32], plain: &[u8]) -> Result<RedeemOutcome, Refusal> {
    let request = decode_request(plain).ok_or(Refusal::Invalid)?;
    let invite = MembershipInvite::from_bytes(&request.invite).map_err(|_| Refusal::Invalid)?;
    let intent = RedemptionIntent::from_bytes(&request.intent).map_err(|_| Refusal::Invalid)?;
    intent
        .check_against(&invite)
        .map_err(|_| Refusal::Invalid)?;
    let message = transcript(hh, &invite.digest(), intent.subject(), &intent.digest());
    intent
        .subject()
        .verify_bytes(&message, &request.signature)
        .map_err(|_| Refusal::Invalid)?;

    let id = invite.invitation_id();
    let claimant = intent.claimant();
    let mut ledger = ctx.ledger.lock();
    if ledger.issuer() != invite.issuer() {
        return Err(Refusal::Invalid);
    }
    // Only the exact recorded invite may redeem; a different signed invite
    // reusing the identifier is treated as unknown.
    if ledger.invite_digest(&id).map_err(refusal)? != invite.digest() {
        return Err(Refusal::Invalid);
    }
    let now = super::now_unix();
    match ledger.claim(&id, &claimant, now).map_err(refusal)? {
        ClaimOutcome::PendingApproval => Ok(RedeemOutcome::PendingApproval),
        ClaimOutcome::Ready => {
            let bundle = ctx.bundles.issue(&invite, &intent)?;
            if bundle.is_empty() || bundle.len() > MAX_BUNDLE_BYTES {
                return Err(Refusal::Unavailable);
            }
            let receipt_id = ledger
                .issue(&id, &claimant, &bundle, now)
                .map_err(refusal)?;
            Ok(RedeemOutcome::Issued {
                receipt_id,
                recovered: false,
                bundle,
            })
        }
        ClaimOutcome::AlreadyIssued(_) => {
            ctx.bundles.may_recover(&invite, &intent)?;
            let recovered = ledger.recover(&id, &claimant, now).map_err(refusal)?;
            Ok(RedeemOutcome::Issued {
                receipt_id: recovered.receipt_id,
                recovered: true,
                bundle: recovered.payload,
            })
        }
    }
}

fn refusal(e: LedgerError) -> Refusal {
    match e {
        LedgerError::UnknownInvitation => Refusal::Invalid,
        LedgerError::Policy(PolicyError::Expired | PolicyError::NotYetValid) => Refusal::Expired,
        LedgerError::WrongSubject | LedgerError::ClaimConflict => Refusal::Conflict,
        LedgerError::Revoked => Refusal::Revoked,
        LedgerError::Denied => Refusal::Denied,
        LedgerError::RecoveryClosed => Refusal::RecoveryClosed,
        _ => Refusal::Unavailable,
    }
}
