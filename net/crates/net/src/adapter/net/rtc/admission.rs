//! The §12 browser admission contract: provisional sessions, the
//! S0e allow-list, and the five enforcement gates.
//!
//! **What this is.** A successful Noise handshake proves the peer
//! holds the mesh PSK. On a browser-facing anchor that is not enough
//! to make it a participant: the PSK is a *transport* secret shipped
//! to every holder of a bootstrap credential, so a handshake alone
//! would hand an unenrolled visitor capability publication, routed
//! forwarding and `0x0D02` — before it has enrolled with anybody.
//! §12 therefore installs such a session as **provisional**: it may
//! exercise exactly the bounded bootstrap exchange S0e enumerated,
//! and everything else is denied *before effects*.
//!
//! **What this is not.** Admission is not authority. A promoted
//! session is eligible for the anchor's ordinary services; every
//! channel, subnet, org and provider check still runs and still
//! refuses. Device delegation ≠ `OrgMembershipCert` ≠ dispatcher
//! grant ≠ provider admission.
//!
//! **Kept distinct from `PeerTransport`** on purpose (§12):
//! transport ownership answers *where this session goes*, admission
//! answers *what it may exercise*. Collapsing them is how a peer
//! becomes a routing participant by virtue of the installer having
//! populated a peer entry.

use std::time::{Duration, Instant};

/// How long a provisional session may live unenrolled (S0e §2
/// whole-session bounds). On expiry it is closed and reclaimed.
pub const PROVISIONAL_TTL: Duration = Duration::from_secs(30);

/// Whole-session inbound frame bound for a provisional session.
pub const MAX_PROVISIONAL_FRAMES: u32 = 256;

/// Whole-session inbound byte bound for a provisional session.
pub const MAX_PROVISIONAL_BYTES: u64 = 256 * 1024;

/// Streams a provisional session may have tracked — the enrollment
/// request stream and the reply stream, and nothing else.
pub const MAX_PROVISIONAL_STREAMS: u32 = 2;

/// Tracked stream bytes for those two streams.
pub const MAX_PROVISIONAL_STREAM_BYTES: u64 = 64 * 1024;

/// Channel memberships a provisional session may hold: exactly its
/// own enrollment reply channel.
pub const MAX_PROVISIONAL_CHANNELS: u32 = 1;

/// In-flight enrollment calls per provisional session.
pub const MAX_INFLIGHT_ENROLLMENTS: u32 = 1;

/// Enrollment REQUEST frames (initial + 3 retries).
pub const MAX_ENROLL_REQUEST_FRAMES: u32 = 4;

/// Enrollment request body bound.
pub const MAX_ENROLL_BODY_BYTES: usize = 16 * 1024;

/// The one nRPC service a provisional session may call (S0e §2 B).
pub const ENROLL_SERVICE: &str = "net.mesh.enroll";

/// Renewal — deliberately **not** on the provisional list (S0e §5).
/// A device whose process restarted re-runs the bounded enrollment
/// exchange; putting renewal here would give every unenrolled peer a
/// second unauthenticated service surface for one legitimate case.
pub const RENEWAL_SERVICE: &str = "net.mesh.renew";

/// The reply channel a provisional session may subscribe to, for a
/// caller whose origin hash is `origin`.
pub fn enroll_reply_channel(origin: u64) -> String {
    format!("net.mesh.enroll.replies.{origin:016x}")
}

/// What a session may exercise (§12), kept beside — never inside —
/// `PeerTransport`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerAdmission {
    /// The handshake completed; enrollment has not. Only the S0e
    /// allow-list is permitted, under the budget carried here.
    Provisional {
        /// When the provisional session was installed.
        since: Instant,
        /// Consumption against the whole-session bounds.
        budget: ProvisionalBudget,
    },
    /// Enrollment promoted this exact session incarnation.
    Admitted {
        /// When promotion happened.
        promoted_at: Instant,
        /// The session id promotion was bound to. A later session
        /// under the same `node_id` is **not** this one.
        session_id: u64,
    },
}

impl Default for PeerAdmission {
    /// Native sessions — UDP, and RTC on a node that serves no
    /// bootstrap — are admitted. The gate exists for browser-facing
    /// anchors; everywhere else it must be invisible, which is what
    /// keeps native ↔ native behaviour unchanged.
    fn default() -> Self {
        Self::Admitted {
            promoted_at: Instant::now(),
            session_id: 0,
        }
    }
}

impl PeerAdmission {
    /// A freshly installed provisional session.
    pub fn provisional(now: Instant) -> Self {
        Self::Provisional {
            since: now,
            budget: ProvisionalBudget::default(),
        }
    }

    /// Is this session still provisional?
    #[inline]
    pub fn is_provisional(&self) -> bool {
        matches!(self, Self::Provisional { .. })
    }

    /// Has a provisional session outlived [`PROVISIONAL_TTL`]?
    /// Always false once admitted.
    pub fn is_expired(&self, now: Instant) -> bool {
        match self {
            Self::Provisional { since, .. } => now.duration_since(*since) >= PROVISIONAL_TTL,
            Self::Admitted { .. } => false,
        }
    }

    /// The session id an admitted peer was promoted for.
    pub fn admitted_session(&self) -> Option<u64> {
        match self {
            Self::Admitted { session_id, .. } => Some(*session_id),
            Self::Provisional { .. } => None,
        }
    }
}

/// Whole-session consumption for a provisional peer (S0e §2).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProvisionalBudget {
    /// Inbound frames seen.
    pub frames: u32,
    /// Inbound bytes seen.
    pub bytes: u64,
    /// Distinct streams tracked.
    pub streams: u32,
    /// Channel memberships held.
    pub channels: u32,
    /// Enrollment REQUEST frames seen.
    pub enroll_requests: u32,
    /// Enrollment calls currently in flight.
    pub inflight_enrollments: u32,
}

impl ProvisionalBudget {
    /// Charge one inbound frame of `bytes`. `Err` means the session
    /// breached a whole-session bound and must be closed and
    /// reclaimed (§12 step 5).
    pub fn charge_frame(&mut self, bytes: u64) -> Result<(), AdmissionRefusal> {
        self.frames = self.frames.saturating_add(1);
        self.bytes = self.bytes.saturating_add(bytes);
        if self.frames > MAX_PROVISIONAL_FRAMES || self.bytes > MAX_PROVISIONAL_BYTES {
            return Err(AdmissionRefusal::BudgetExhausted);
        }
        Ok(())
    }

    /// Charge one channel membership.
    pub fn charge_channel(&mut self) -> Result<(), AdmissionRefusal> {
        self.channels = self.channels.saturating_add(1);
        if self.channels > MAX_PROVISIONAL_CHANNELS {
            return Err(AdmissionRefusal::BudgetExhausted);
        }
        Ok(())
    }

    /// Charge one enrollment REQUEST frame.
    pub fn charge_enroll_request(&mut self) -> Result<(), AdmissionRefusal> {
        self.enroll_requests = self.enroll_requests.saturating_add(1);
        if self.enroll_requests > MAX_ENROLL_REQUEST_FRAMES {
            return Err(AdmissionRefusal::BudgetExhausted);
        }
        Ok(())
    }
}

/// Why an action was refused, by gate. Each variant has its own
/// counter on [`super::RtcStats`] — a refusal that is not counted is
/// a refusal nobody can operate on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmissionRefusal {
    /// The adjacent session may not have this anchor forward for it
    /// (§12: no third-party relay forwarding before enrollment).
    Forward,
    /// A provisional session does not install routes or become a
    /// discovery participant.
    RouteInstall,
    /// Not the one permitted reply-channel subscription.
    Subscribe,
    /// A provisional peer's announcements are neither ingested nor
    /// flooded.
    Announce,
    /// Not a permitted bootstrap action at application delivery.
    Deliver,
    /// A whole-session bound was breached: close and reclaim.
    BudgetExhausted,
}

impl std::fmt::Display for AdmissionRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let what = match self {
            Self::Forward => "forwarding for a provisional session",
            Self::RouteInstall => "route installation for a provisional session",
            Self::Subscribe => "channel subscription outside the bootstrap allow-list",
            Self::Announce => "announcement ingest from a provisional session",
            Self::Deliver => "application delivery outside the bootstrap allow-list",
            Self::BudgetExhausted => "provisional session budget exhausted",
        };
        write!(f, "admission refused: {what}")
    }
}

impl std::error::Error for AdmissionRefusal {}

/// The application action a delivery gate is being asked about.
///
/// This is the shape §12 step 3 demands: the enrollment REQUEST's
/// service name lives *inside* the nRPC envelope, so the decision
/// cannot be a header test. The caller decodes under bounds first
/// and hands the decoded facts here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BootstrapAction<'a> {
    /// An nRPC REQUEST for `service`, targeted at `target_node`,
    /// with a reply channel and a body length.
    NrpcRequest {
        /// Service name from the decoded envelope.
        service: &'a str,
        /// Who the call is addressed to.
        target_node: u64,
        /// Reply channel named in the envelope.
        reply_channel: &'a str,
        /// Decoded body length.
        body_len: usize,
    },
    /// An nRPC CANCEL for an in-flight call.
    NrpcCancel,
    /// A channel membership Subscribe.
    Subscribe {
        /// Channel being subscribed to.
        channel: &'a str,
        /// Whether the frame carried a token.
        has_token: bool,
        /// Whether the frame named a queue group.
        has_queue_group: bool,
    },
    /// A channel membership Unsubscribe.
    Unsubscribe {
        /// Channel being left.
        channel: &'a str,
    },
    /// Stream-window / reliability control for a tracked stream.
    StreamControl,
    /// Session maintenance — heartbeat.
    Heartbeat,
    /// A pingwave, in either direction.
    Pingwave,
    /// Anything else: capability announcements, folds, sensing,
    /// migration, `0x0D02`, ordinary publishes and every other nRPC
    /// service.
    Other,
}

/// Decide one bootstrap action against the S0e allow-list.
///
/// `local_node_id` is this anchor, `caller_origin` is the
/// authenticated origin hash of the calling session — both come from
/// the session, never from the frame, which is what makes "its own
/// reply channel" a checkable claim rather than a self-declaration.
pub fn allow_provisional_action(
    action: &BootstrapAction<'_>,
    local_node_id: u64,
    caller_origin: u64,
) -> Result<(), AdmissionRefusal> {
    match action {
        // B1: exactly one service, addressed to this anchor, with
        // the caller's own reply channel and a bounded body.
        BootstrapAction::NrpcRequest {
            service,
            target_node,
            reply_channel,
            body_len,
        } => {
            if *service != ENROLL_SERVICE
                || *target_node != local_node_id
                || *reply_channel != enroll_reply_channel(caller_origin)
                || *body_len > MAX_ENROLL_BODY_BYTES
            {
                return Err(AdmissionRefusal::Deliver);
            }
            Ok(())
        }
        // B3: cancelling one's own in-flight enrollment call.
        BootstrapAction::NrpcCancel => Ok(()),
        // C1: the one reply channel, with no token and no queue
        // group. Wildcards and caller-chosen destinations are
        // refused, not ignored.
        BootstrapAction::Subscribe {
            channel,
            has_token,
            has_queue_group,
        } => {
            if *channel != enroll_reply_channel(caller_origin)
                || *has_token
                || *has_queue_group
                || channel.contains('*')
            {
                return Err(AdmissionRefusal::Subscribe);
            }
            Ok(())
        }
        // C2: teardown of that same channel.
        BootstrapAction::Unsubscribe { channel } => {
            if *channel != enroll_reply_channel(caller_origin) {
                return Err(AdmissionRefusal::Subscribe);
            }
            Ok(())
        }
        // D: flow control for the bootstrap streams.
        BootstrapAction::StreamControl => Ok(()),
        // E1: maintenance is permitted; pingwave is not, in either
        // direction (S0e §3 row 11).
        BootstrapAction::Heartbeat => Ok(()),
        BootstrapAction::Pingwave => Err(AdmissionRefusal::Deliver),
        BootstrapAction::Other => Err(AdmissionRefusal::Deliver),
    }
}

/// The set of endpoints whose sessions are provisional, shared with
/// components that cannot read `PeerInfo` — the router's own send
/// loop (F3), the traversal introducer (F6) and the proxy (F7).
///
/// A projection, not a second source of truth: `PeerInfo::admission`
/// decides, and this mirrors it on install, promotion and removal.
/// It exists because those three sites forward for an adjacent
/// session without ever touching the peer map, and §12's rule is
/// about the adjacent session, not about who wrote the packet.
pub type ProvisionalEndpoints = std::sync::Arc<dashmap::DashSet<super::super::PeerAddr>>;

#[cfg(test)]
mod tests {
    use super::*;

    const ANCHOR: u64 = 0xA0;
    const CALLER: u64 = 0xC0;

    fn reply() -> String {
        enroll_reply_channel(CALLER)
    }

    #[test]
    fn the_permitted_enrollment_call_is_allowed_and_everything_near_it_is_not() {
        let ok = BootstrapAction::NrpcRequest {
            service: ENROLL_SERVICE,
            target_node: ANCHOR,
            reply_channel: &reply(),
            body_len: 512,
        };
        assert_eq!(allow_provisional_action(&ok, ANCHOR, CALLER), Ok(()));

        // A different service on the same carrier — the reason §12
        // calls this an action allow-list and not "allow nRPC".
        let other_service = BootstrapAction::NrpcRequest {
            service: "app.orders.place",
            target_node: ANCHOR,
            reply_channel: &reply(),
            body_len: 512,
        };
        assert_eq!(
            allow_provisional_action(&other_service, ANCHOR, CALLER),
            Err(AdmissionRefusal::Deliver)
        );

        // Renewal is deliberately absent from the list (S0e §5).
        let renewal = BootstrapAction::NrpcRequest {
            service: RENEWAL_SERVICE,
            target_node: ANCHOR,
            reply_channel: &reply(),
            body_len: 512,
        };
        assert_eq!(
            allow_provisional_action(&renewal, ANCHOR, CALLER),
            Err(AdmissionRefusal::Deliver)
        );

        // Addressed past the anchor.
        let third_party = BootstrapAction::NrpcRequest {
            service: ENROLL_SERVICE,
            target_node: 0xBEEF,
            reply_channel: &reply(),
            body_len: 512,
        };
        assert_eq!(
            allow_provisional_action(&third_party, ANCHOR, CALLER),
            Err(AdmissionRefusal::Deliver)
        );

        // Someone else's reply channel.
        let other_reply = enroll_reply_channel(0xDEAD);
        let stolen = BootstrapAction::NrpcRequest {
            service: ENROLL_SERVICE,
            target_node: ANCHOR,
            reply_channel: &other_reply,
            body_len: 512,
        };
        assert_eq!(
            allow_provisional_action(&stolen, ANCHOR, CALLER),
            Err(AdmissionRefusal::Deliver)
        );

        // Oversize body.
        let fat = BootstrapAction::NrpcRequest {
            service: ENROLL_SERVICE,
            target_node: ANCHOR,
            reply_channel: &reply(),
            body_len: MAX_ENROLL_BODY_BYTES + 1,
        };
        assert_eq!(
            allow_provisional_action(&fat, ANCHOR, CALLER),
            Err(AdmissionRefusal::Deliver)
        );
    }

    #[test]
    fn only_the_callers_own_reply_channel_subscribes_and_only_bare() {
        let own = reply();
        let ok = BootstrapAction::Subscribe {
            channel: &own,
            has_token: false,
            has_queue_group: false,
        };
        assert_eq!(allow_provisional_action(&ok, ANCHOR, CALLER), Ok(()));

        for bad in [
            BootstrapAction::Subscribe {
                channel: "app.events",
                has_token: false,
                has_queue_group: false,
            },
            BootstrapAction::Subscribe {
                channel: "net.mesh.enroll.replies.*",
                has_token: false,
                has_queue_group: false,
            },
            BootstrapAction::Subscribe {
                channel: &own,
                has_token: true,
                has_queue_group: false,
            },
            BootstrapAction::Subscribe {
                channel: &own,
                has_token: false,
                has_queue_group: true,
            },
        ] {
            assert_eq!(
                allow_provisional_action(&bad, ANCHOR, CALLER),
                Err(AdmissionRefusal::Subscribe),
                "wildcards, tokens, queue groups and unrelated channels are \
                 REFUSED, not ignored: {bad:?}"
            );
        }
    }

    #[test]
    fn heartbeat_is_maintenance_and_pingwave_is_not() {
        assert_eq!(
            allow_provisional_action(&BootstrapAction::Heartbeat, ANCHOR, CALLER),
            Ok(())
        );
        assert_eq!(
            allow_provisional_action(&BootstrapAction::Pingwave, ANCHOR, CALLER),
            Err(AdmissionRefusal::Deliver),
            "they are emitted two lines apart in one loop body; the allow-list \
             splits the statements, not the loop"
        );
    }

    #[test]
    fn the_whole_session_bounds_breach_rather_than_clamp() {
        let mut budget = ProvisionalBudget::default();
        for _ in 0..MAX_PROVISIONAL_FRAMES {
            budget.charge_frame(16).expect("within the frame bound");
        }
        assert_eq!(
            budget.charge_frame(16),
            Err(AdmissionRefusal::BudgetExhausted),
            "a breach closes and reclaims the session (§12 step 5) — it does \
             not silently stop counting"
        );

        let mut bytes = ProvisionalBudget::default();
        assert_eq!(
            bytes.charge_frame(MAX_PROVISIONAL_BYTES + 1),
            Err(AdmissionRefusal::BudgetExhausted)
        );

        let mut channels = ProvisionalBudget::default();
        channels.charge_channel().expect("the one reply channel");
        assert_eq!(
            channels.charge_channel(),
            Err(AdmissionRefusal::BudgetExhausted)
        );

        let mut requests = ProvisionalBudget::default();
        for _ in 0..MAX_ENROLL_REQUEST_FRAMES {
            requests
                .charge_enroll_request()
                .expect("initial + 3 retries");
        }
        assert_eq!(
            requests.charge_enroll_request(),
            Err(AdmissionRefusal::BudgetExhausted)
        );
    }

    #[test]
    fn provisional_state_expires_and_admitted_state_does_not() {
        let start = Instant::now();
        let prov = PeerAdmission::provisional(start);
        assert!(prov.is_provisional());
        assert!(!prov.is_expired(start));
        assert!(prov.is_expired(start + PROVISIONAL_TTL));

        let admitted = PeerAdmission::Admitted {
            promoted_at: start,
            session_id: 7,
        };
        assert!(!admitted.is_provisional());
        assert!(!admitted.is_expired(start + PROVISIONAL_TTL * 10));
        assert_eq!(admitted.admitted_session(), Some(7));
    }

    #[test]
    fn the_default_is_admitted_so_native_sessions_are_unchanged() {
        assert!(!PeerAdmission::default().is_provisional());
    }
}
