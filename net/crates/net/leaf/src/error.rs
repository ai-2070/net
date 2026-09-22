//! What can go wrong in a leaf, typed.
//!
//! **The correction Stage 5 was given** (plan, Stage 5 "Failure
//! typing, corrected"): an ICE timeout is not evidence of UDP
//! blocking. An unreachable anchor, a misconfigured one and an
//! overloaded one all produce the same symptom, so the surfaced
//! result stays [`RtcError::IceTimeout`] unless the narrower cause
//! has actually been established. [`RtcError::UdpBlocked`] carries
//! the evidence that distinguishes it and cannot be constructed
//! without it.

use core::fmt;

/// The one error type the leaf's public surface returns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeafError {
    /// The wire layer refused a packet: framing, AEAD, replay window.
    Wire(String),
    /// The anti-replay window refused the packet's AEAD counter — a
    /// duplicate or too-late arrival the wire's replay window would
    /// not admit.
    ///
    /// Typed rather than prose inside [`Self::Wire`] so the receive
    /// path can count a replay-attack signal without matching on
    /// error text: a wording refactor upstream must not silently
    /// erase it.
    Replay,
    /// A session-level failure: no session for a peer, a handshake
    /// that did not complete, a session that was replaced.
    Session(String),
    /// The control plane could not carry what the leaf asked it to.
    ControlPlane(String),
    /// The RTC transport itself.
    Rtc(RtcError),
    /// An nRPC call failed or was disposed of.
    Rpc(RpcError),
    /// Identity storage, generation or custody.
    Identity(String),
    /// This tab is not the leader for the origin, and the operation
    /// requires the node. Carries the fenced generation the caller
    /// presented, so a stale follower's failure is legible.
    NotLeader {
        /// The generation the caller believed it held.
        presented: u64,
        /// The generation the leader currently holds, when known.
        current: Option<u64>,
    },
    /// A send was refused because the stream's send window is
    /// exhausted until the peer returns credit.
    ///
    /// Synchronous and bounded on purpose: the leaf runs on the
    /// browser's main thread and has no runtime to park a send on,
    /// so a full window is a typed refusal the caller sees now,
    /// never a queue that grows behind its back.
    Backpressure {
        /// The stream whose window is full.
        stream_id: u64,
        /// On-wire bytes the refused message needed.
        needed: u32,
        /// Credit the stream had.
        remaining: u32,
    },
    /// A reliable send was refused because the stream's retransmit
    /// window has no room to **own** the packets it would produce.
    ///
    /// Distinct from [`Self::Backpressure`] because the bounds are
    /// distinct: credit is bytes, and the retransmit window is a
    /// count of descriptors. Tiny reliable messages exhaust the
    /// second long before the first — 129 one-byte sends fit
    /// comfortably inside a 64 KiB window and overrun a
    /// 128-descriptor bound — and past it the wire evicts the
    /// oldest still-unacknowledged descriptor, so the packet stays
    /// sent and becomes unrecoverable by NACK or RTO. A refusal the
    /// caller sees is the only disposition that keeps every
    /// admitted packet owned.
    ReliableWindowFull {
        /// The stream whose retransmit window is full.
        stream_id: u64,
        /// Packets the refused message needed to own.
        needed: usize,
        /// Descriptor slots the stream had.
        remaining: usize,
    },
    /// A caller-supplied `iceServers` entry points a STUN URL at
    /// **this connection's own peer**.
    ///
    /// Stage 6: an anchor's RTC endpoint is an ICE agent, not a STUN
    /// server for the connection it is a party to, and configuring
    /// it as one produces a connection that gathers no
    /// server-reflexive candidate and then simply times out. The
    /// refusal is returned **before any ICE work** — before the
    /// offer exists — because "promptly and descriptively" is the
    /// only useful disposition for a configuration that cannot
    /// work.
    ///
    /// It is a refusal and **not** a silent strip: stripping would
    /// turn an explicit NAT-traversal configuration into a
    /// host-candidate-only attempt while appearing to have accepted
    /// the caller's settings.
    ///
    /// **Detection is endpoint equality only**, after default-port
    /// normalisation. A STUN URL naming a DNS alias that happens to
    /// resolve to the peer's address is **not** detected: the leaf
    /// does not resolve names, and promising exhaustive detection it
    /// cannot provide would be worse than naming the boundary. The
    /// announced STUN endpoint exists so the working configuration
    /// needs no detection at all.
    IceServerConflictsWithPeer {
        /// The `iceServers` URL the caller supplied, verbatim.
        entry: String,
        /// This connection's peer RTC endpoint, as announced.
        peer_rtc_addr: String,
    },
}

/// Why an RTC attempt did not produce a DataChannel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RtcError {
    /// ICE did not connect inside the deadline. **This is the
    /// default classification**, and it is deliberately weaker than
    /// "UDP is blocked": the same symptom is produced by an anchor
    /// that is down, wrong, or saturated.
    IceTimeout,
    /// UDP to the anchor is blocked. Constructible only through
    /// [`RtcError::udp_blocked`], which requires the distinguishing
    /// evidence.
    UdpBlocked(UdpBlockedEvidence),
    /// The DataChannel opened and then closed under us.
    ChannelClosed(String),
    /// The browser refused the attempt outright (no
    /// `RTCPeerConnection`, a permissions failure, an SDP the engine
    /// rejected).
    Unsupported(String),
}

impl RtcError {
    /// Classify an attempt as UDP-blocked.
    ///
    /// The only evidence that distinguishes UDP blocking from an
    /// unreachable anchor: the HTTPS bootstrap to that same anchor
    /// **succeeded** (so the anchor is up and addressable) while a
    /// STUN binding to the `rtc_addr` it published got no answer. Both
    /// halves are required; a caller that has only the ICE timeout
    /// must return [`RtcError::IceTimeout`].
    pub fn udp_blocked(evidence: UdpBlockedEvidence) -> Self {
        Self::UdpBlocked(evidence)
    }
}

/// The two observations that, together, distinguish blocked UDP from
/// an anchor that simply is not there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UdpBlockedEvidence {
    /// The anchor answered its HTTPS bootstrap — it is up, reachable
    /// and correctly configured enough to accept an offer.
    pub bootstrap_ok: bool,
    /// A STUN binding request to the `rtc_addr` that same anchor
    /// published was not answered within the probe deadline.
    pub stun_probe_failed: bool,
    /// The address the probe was aimed at, so the claim names its
    /// own subject.
    pub probed: String,
}

impl UdpBlockedEvidence {
    /// `Some(evidence)` only when both observations hold; `None`
    /// otherwise, which is the caller's instruction to stay with
    /// [`RtcError::IceTimeout`].
    pub fn new(
        bootstrap_ok: bool,
        stun_probe_failed: bool,
        probed: impl Into<String>,
    ) -> Option<Self> {
        (bootstrap_ok && stun_probe_failed).then(|| Self {
            bootstrap_ok,
            stun_probe_failed,
            probed: probed.into(),
        })
    }
}

/// How an nRPC call ended when it did not return a reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RpcError {
    /// The service answered with a typed refusal.
    Refused {
        /// The wire status code.
        status: u16,
        /// The human half, when the service sent one.
        message: String,
    },
    /// The call's deadline elapsed.
    Timeout,
    /// The session carrying the call went away. **Never retried
    /// silently** (§8's leader-lifecycle rule): the caller is told,
    /// and decides.
    SessionLost,
    /// The leader that owned this call was replaced while it was in
    /// flight. Same rule: typed, never silently re-issued.
    LeaderLost {
        /// The generation that owned the call.
        generation: u64,
    },
    /// The caller's **local** deadline elapsed before the tab running
    /// the node answered.
    ///
    /// Not [`RpcError::Timeout`], and the difference is the whole
    /// point of the variant: a `Timeout` is the node's own deadline
    /// expiring on the call it owns, so nothing happened. This one is
    /// a follower's deadline expiring on a call the *leader* owns —
    /// the leader may have executed it, may be executing it, and a
    /// deadline on this tab cannot cancel work already admitted on
    /// another. So the outcome is stated as what it is: indeterminate,
    /// and never retried, because a retry would be a second execution
    /// of something that may have executed once already.
    Indeterminate {
        /// The local deadline that elapsed, in milliseconds.
        deadline_ms: u32,
    },
    /// The reply did not decode.
    Malformed(String),
}

impl fmt::Display for LeafError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Wire(e) => write!(f, "wire: {e}"),
            Self::Replay => {
                write!(f, "replay: the anti-replay window refused the AEAD counter")
            }
            Self::Session(e) => write!(f, "session: {e}"),
            Self::ControlPlane(e) => write!(f, "control plane: {e}"),
            Self::Rtc(e) => write!(f, "rtc: {e}"),
            Self::Rpc(e) => write!(f, "rpc: {e}"),
            Self::Identity(e) => write!(f, "identity: {e}"),
            Self::NotLeader { presented, current } => match current {
                Some(current) => write!(
                    f,
                    "not the leader: this tab holds generation {presented}, the leader holds {current}"
                ),
                None => write!(f, "not the leader: this tab holds generation {presented}"),
            },
            Self::Backpressure {
                stream_id,
                needed,
                remaining,
            } => write!(
                f,
                "backpressure: stream {stream_id:#x} needs {needed} bytes of send \
                 credit and has {remaining}; the peer has not granted more yet"
            ),
            Self::ReliableWindowFull {
                stream_id,
                needed,
                remaining,
            } => write!(
                f,
                "reliable window full: stream {stream_id:#x} needs {needed} retransmit \
                 descriptor(s) and has room for {remaining}; the peer has not \
                 acknowledged enough packets yet"
            ),
            Self::IceServerConflictsWithPeer {
                entry,
                peer_rtc_addr,
            } => write!(
                f,
                "ice configuration: the iceServers entry {entry} names this connection's \
                 peer RTC endpoint {peer_rtc_addr}; a peer cannot be its own STUN server. \
                 Omit iceServers to use the STUN endpoint the anchor announces (the \
                 stun_addr field of GET /rtc/anchor), or name a STUN server that is not \
                 this peer"
            ),
        }
    }
}

impl fmt::Display for RtcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::IceTimeout => write!(
                f,
                "ICE did not connect inside the deadline (this does not establish that UDP is blocked)"
            ),
            Self::UdpBlocked(e) => write!(
                f,
                "UDP appears blocked: the anchor's HTTPS bootstrap succeeded but a STUN binding to {} was unanswered",
                e.probed
            ),
            Self::ChannelClosed(e) => write!(f, "the DataChannel closed: {e}"),
            Self::Unsupported(e) => write!(f, "this browser refused the attempt: {e}"),
        }
    }
}

impl fmt::Display for RpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused { status, message } => write!(f, "refused ({status}): {message}"),
            Self::Timeout => write!(f, "the call's deadline elapsed"),
            Self::SessionLost => write!(f, "the session carrying the call went away"),
            Self::LeaderLost { generation } => {
                write!(f, "the leader holding generation {generation} was replaced")
            }
            Self::Indeterminate { deadline_ms } => write!(
                f,
                "the local deadline of {deadline_ms}ms elapsed before the tab running \
                 the node answered; the remote operation may still have executed \
                 (it was not retried)"
            ),
            Self::Malformed(e) => write!(f, "the reply did not decode: {e}"),
        }
    }
}

impl core::error::Error for LeafError {}
impl core::error::Error for RtcError {}
impl core::error::Error for RpcError {}

/// The leaf's result alias. The error parameter is defaulted so the
/// common site stays short while a precise error stays expressible.
pub type Result<T, E = LeafError> = core::result::Result<T, E>;

#[cfg(test)]
mod tests {
    use super::*;

    /// The corrected typing, as a property: nothing short of both
    /// observations can produce `UdpBlocked`.
    #[test]
    fn udp_blocked_requires_both_observations() {
        assert!(UdpBlockedEvidence::new(true, true, "203.0.113.7:7101").is_some());
        // An ICE timeout with a bootstrap that also failed is an
        // unreachable anchor, not a blocked transport.
        assert!(UdpBlockedEvidence::new(false, true, "203.0.113.7:7101").is_none());
        // A STUN probe that succeeded says UDP works.
        assert!(UdpBlockedEvidence::new(true, false, "203.0.113.7:7101").is_none());
        assert!(UdpBlockedEvidence::new(false, false, "203.0.113.7:7101").is_none());
    }

    #[test]
    fn the_ice_timeout_message_refuses_the_stronger_claim() {
        let rendered = RtcError::IceTimeout.to_string();
        assert!(
            rendered.contains("does not establish that UDP is blocked"),
            "the weaker classification must say what it is not: {rendered}",
        );
    }
}
