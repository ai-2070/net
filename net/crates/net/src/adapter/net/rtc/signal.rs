//! `SUBPROTOCOL_RTC_SIGNAL` (`0x0D02`) — the codec, the dialog
//! state and the per-sender budget (plan §5 Layer 3).
//!
//! What rides here is **opaque to every hop that carries it**. The
//! frames travel inside an ordinary session-authenticated Net packet,
//! so origin and target are the session endpoints — they are never
//! wire fields, and there is nothing for a relay to rewrite. An
//! anchor forwarding a routed envelope that happens to contain
//! signalling holds no key for it; it can count `0x0D02` separately
//! because `subprotocol_id` is a cleartext AAD-authenticated header
//! field (§10), and that is the whole of what it can do.
//!
//! No key material rides here. By the time a peer can send an
//! `Offer`, the session that carries it already exists; the SDP only
//! decides which *path* the next session takes.
//!
//! **Budget.** A dialog is cheap for the sender and not free for the
//! receiver: each one holds an ICE agent's worth of driver state. So
//! a sender gets at most [`MAX_DIALOGS_PER_PEER`] concurrent dialogs
//! and [`MAX_FRAMES_PER_WINDOW`] frames per [`BUDGET_WINDOW`]; past
//! that, frames are dropped **with a counter**
//! (`RtcStats::signal_over_budget`) rather than silently, because a
//! silent drop here is indistinguishable from a peer that never
//! signalled.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// Subprotocol id for RTC signalling. Registered in
/// `docs/SUBPROTOCOLS.md`; `0x0D03` is reserved for the
/// port-mapping metadata that `traversal/mod.rs` used to reserve
/// `0x0D02` for in a comment and never allocated.
pub const SUBPROTOCOL_RTC_SIGNAL: u16 = 0x0D02;

/// Concurrent dialogs one peer may have open with us.
pub const MAX_DIALOGS_PER_PEER: usize = 4;

/// Signalling frames one peer may send us per [`BUDGET_WINDOW`].
pub const MAX_FRAMES_PER_WINDOW: u32 = 64;

/// The budget window.
pub const BUDGET_WINDOW: Duration = Duration::from_secs(10);

/// Largest SDP blob or candidate string accepted in one frame.
///
/// A real offer is a few kilobytes; this is a bound on memory a
/// remote peer can make us hold while deciding whether to answer,
/// not a protocol limit.
pub const MAX_SDP_BYTES: usize = 16 * 1024;

/// Why an offerer's dialog was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RtcRejectReason {
    /// The receiver does not have RTC configured.
    Unsupported,
    /// The receiver is at its dialog or frame budget.
    Busy,
    /// The receiver declined this specific peer.
    Declined,
    /// ICE did not complete within `RtcConfig::ice_deadline`.
    IceTimeout,
}

impl std::fmt::Display for RtcRejectReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported => write!(f, "peer does not support rtc"),
            Self::Busy => write!(f, "peer is at its signalling budget"),
            Self::Declined => write!(f, "peer declined the dialog"),
            Self::IceTimeout => write!(f, "ice did not complete before the deadline"),
        }
    }
}

/// One signalling frame (plan §5 Layer 3, verbatim).
///
/// `dialog` is chosen by the offerer and is meaningful only between
/// the two session endpoints — it is not a mesh-wide identifier and
/// nothing routes on it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RtcSignalMsg {
    /// SDP offer opening a dialog.
    Offer {
        /// Offerer-chosen dialog id.
        dialog: u64,
        /// Opaque SDP.
        sdp: String,
    },
    /// SDP answer to an open dialog.
    Answer {
        /// The dialog this answers.
        dialog: u64,
        /// Opaque SDP.
        sdp: String,
    },
    /// One trickled ICE candidate.
    Candidate {
        /// The dialog it belongs to.
        dialog: u64,
        /// Candidate in SDP form.
        candidate: String,
        /// The media id it applies to.
        mid: String,
    },
    /// End the dialog.
    Reject {
        /// The dialog being ended.
        dialog: u64,
        /// Why.
        reason: RtcRejectReason,
    },
}

impl RtcSignalMsg {
    /// The dialog this frame belongs to.
    #[inline]
    pub fn dialog(&self) -> u64 {
        match self {
            Self::Offer { dialog, .. }
            | Self::Answer { dialog, .. }
            | Self::Candidate { dialog, .. }
            | Self::Reject { dialog, .. } => *dialog,
        }
    }

    /// Encode for the wire (postcard).
    pub fn to_bytes(&self) -> Result<Vec<u8>, postcard::Error> {
        postcard::to_allocvec(self)
    }

    /// Decode a frame, refusing oversize payloads **before** the
    /// dialog state is touched.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, RtcSignalError> {
        if bytes.len() > MAX_SDP_BYTES {
            return Err(RtcSignalError::TooLarge);
        }
        let msg: Self = postcard::from_bytes(bytes).map_err(|_| RtcSignalError::Malformed)?;
        let oversize = match &msg {
            Self::Offer { sdp, .. } | Self::Answer { sdp, .. } => sdp.len() > MAX_SDP_BYTES,
            Self::Candidate { candidate, mid, .. } => candidate.len() + mid.len() > MAX_SDP_BYTES,
            Self::Reject { .. } => false,
        };
        if oversize {
            return Err(RtcSignalError::TooLarge);
        }
        Ok(msg)
    }
}

/// Why a signalling frame was not acted on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RtcSignalError {
    /// Not decodable as an `RtcSignalMsg`.
    Malformed,
    /// Over [`MAX_SDP_BYTES`].
    TooLarge,
    /// The sender is over its frame or dialog budget.
    OverBudget,
    /// The frame names a dialog this sender does not have open.
    UnknownDialog,
}

impl std::fmt::Display for RtcSignalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed => write!(f, "rtc signal: malformed frame"),
            Self::TooLarge => write!(f, "rtc signal: frame over the size bound"),
            Self::OverBudget => write!(f, "rtc signal: sender is over budget"),
            Self::UnknownDialog => write!(f, "rtc signal: no such dialog"),
        }
    }
}

impl std::error::Error for RtcSignalError {}

/// One sender's signalling state: which dialogs are open and how
/// many frames it has sent in the current window.
#[derive(Debug)]
struct SenderState {
    dialogs: Vec<u64>,
    window_started: Instant,
    frames_in_window: u32,
}

/// Per-peer dialog and frame budget for inbound `0x0D02`.
///
/// Keyed by the **session endpoint's node id**, which is
/// authenticated: the frame arrived inside that peer's session, so a
/// sender cannot spend another peer's budget.
#[derive(Debug, Default)]
pub struct SignalBudget {
    senders: HashMap<u64, SenderState>,
}

/// What the budget decided about one inbound frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalAdmit {
    /// Act on it; it opened a new dialog.
    NewDialog,
    /// Act on it; it belongs to an open dialog.
    OpenDialog,
    /// The dialog ended (a `Reject`), and its slot is released.
    DialogEnded,
    /// Refused; count it.
    Refused(RtcSignalError),
}

impl SignalBudget {
    /// Fresh budget.
    pub fn new() -> Self {
        Self::default()
    }

    /// Admit one inbound frame from `from_node`, at `now`.
    ///
    /// The frame-rate window is checked first and applies to every
    /// frame kind including `Reject`: a peer that has exhausted its
    /// window does not get to keep talking by rejecting.
    pub fn admit(&mut self, from_node: u64, msg: &RtcSignalMsg, now: Instant) -> SignalAdmit {
        let state = self
            .senders
            .entry(from_node)
            .or_insert_with(|| SenderState {
                dialogs: Vec::new(),
                window_started: now,
                frames_in_window: 0,
            });
        if now.duration_since(state.window_started) >= BUDGET_WINDOW {
            state.window_started = now;
            state.frames_in_window = 0;
        }
        if state.frames_in_window >= MAX_FRAMES_PER_WINDOW {
            return SignalAdmit::Refused(RtcSignalError::OverBudget);
        }
        state.frames_in_window += 1;

        let dialog = msg.dialog();
        match msg {
            RtcSignalMsg::Offer { .. } => {
                if state.dialogs.contains(&dialog) {
                    return SignalAdmit::OpenDialog;
                }
                if state.dialogs.len() >= MAX_DIALOGS_PER_PEER {
                    return SignalAdmit::Refused(RtcSignalError::OverBudget);
                }
                state.dialogs.push(dialog);
                SignalAdmit::NewDialog
            }
            RtcSignalMsg::Reject { .. } => {
                let known = state.dialogs.iter().position(|d| *d == dialog);
                match known {
                    Some(i) => {
                        state.dialogs.swap_remove(i);
                        SignalAdmit::DialogEnded
                    }
                    None => SignalAdmit::Refused(RtcSignalError::UnknownDialog),
                }
            }
            RtcSignalMsg::Answer { .. } | RtcSignalMsg::Candidate { .. } => {
                // **R5-A: only an Offer creates a dialog owner.**
                // These frames used to establish one, which made
                // four well-formed Candidates for ids nobody
                // offered into four reservations the engine had no
                // dialog for — so nothing could ever expire them —
                // and let a late Candidate **resurrect** an id that
                // a Reject or an expiry had already retired. That
                // resurrection is the red CI witness: A's own
                // dialog slot was freed by B's Reject and then
                // re-created by B's trailing trickled Candidate,
                // and stayed 1 for ever.
                //
                // A dialog *we* offered is in this list already —
                // `note_outbound_dialog` puts it there when the
                // offer is sent — so the legitimate answer/candidate
                // for our own attempt still finds its owner here.
                if state.dialogs.contains(&dialog) {
                    SignalAdmit::OpenDialog
                } else {
                    SignalAdmit::Refused(RtcSignalError::UnknownDialog)
                }
            }
        }
    }

    /// Record a dialog **we** offered (R5).
    ///
    /// The offerer's dialog lives in our outbound state, so an
    /// immediate `Reject` for it used to be refused as
    /// `UnknownDialog` — leaving our own offer alive until its
    /// timeout. Tracking it here costs the same slot the answer or
    /// candidate would have taken a moment later.
    pub fn note_outbound_dialog(&mut self, to_node: u64, dialog: u64) {
        let state = self.senders.entry(to_node).or_insert_with(|| SenderState {
            dialogs: Vec::new(),
            window_started: Instant::now(),
            frames_in_window: 0,
        });
        if !state.dialogs.contains(&dialog) && state.dialogs.len() < MAX_DIALOGS_PER_PEER {
            state.dialogs.push(dialog);
        }
    }

    /// End a dialog locally — an `ice_deadline` expiry, or our own
    /// `Reject`. Releases the slot so the peer may open another.
    pub fn end_dialog(&mut self, from_node: u64, dialog: u64) {
        if let Some(state) = self.senders.get_mut(&from_node) {
            if let Some(i) = state.dialogs.iter().position(|d| *d == dialog) {
                state.dialogs.swap_remove(i);
            }
        }
    }

    /// Forget a peer entirely (session closed).
    pub fn forget(&mut self, from_node: u64) {
        self.senders.remove(&from_node);
    }

    /// How many dialogs this peer currently holds.
    pub fn open_dialogs(&self, from_node: u64) -> usize {
        self.senders
            .get(&from_node)
            .map(|s| s.dialogs.len())
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offer(dialog: u64) -> RtcSignalMsg {
        RtcSignalMsg::Offer {
            dialog,
            sdp: "v=0".to_string(),
        }
    }

    #[test]
    fn every_frame_kind_round_trips_through_postcard() {
        let frames = [
            offer(1),
            RtcSignalMsg::Answer {
                dialog: 1,
                sdp: "v=0\r\na=answer".to_string(),
            },
            RtcSignalMsg::Candidate {
                dialog: 1,
                candidate: "candidate:1 1 udp 2130706431 127.0.0.1 4444 typ host".to_string(),
                mid: "0".to_string(),
            },
            RtcSignalMsg::Reject {
                dialog: 1,
                reason: RtcRejectReason::Busy,
            },
        ];
        for frame in frames {
            let bytes = frame.to_bytes().expect("encodes");
            assert_eq!(
                RtcSignalMsg::from_bytes(&bytes).expect("decodes"),
                frame,
                "frame did not survive the wire form"
            );
        }
    }

    #[test]
    fn an_oversize_sdp_is_refused_before_anything_else_happens() {
        let huge = RtcSignalMsg::Offer {
            dialog: 7,
            sdp: "x".repeat(MAX_SDP_BYTES + 1),
        };
        let bytes = huge.to_bytes().expect("encodes");
        assert_eq!(
            RtcSignalMsg::from_bytes(&bytes),
            Err(RtcSignalError::TooLarge),
            "a remote peer must not be able to make us hold arbitrary SDP"
        );
    }

    #[test]
    fn garbage_is_malformed_not_a_panic() {
        assert_eq!(
            RtcSignalMsg::from_bytes(&[0xFF, 0xFF, 0xFF]),
            Err(RtcSignalError::Malformed)
        );
    }

    #[test]
    fn the_fifth_concurrent_dialog_is_refused_and_a_reject_frees_a_slot() {
        let mut budget = SignalBudget::new();
        let now = Instant::now();
        for d in 0..MAX_DIALOGS_PER_PEER as u64 {
            assert_eq!(budget.admit(9, &offer(d), now), SignalAdmit::NewDialog);
        }
        assert_eq!(
            budget.admit(9, &offer(99), now),
            SignalAdmit::Refused(RtcSignalError::OverBudget),
            "concurrency is what costs the receiver, so that is what is bounded"
        );

        let ended = RtcSignalMsg::Reject {
            dialog: 0,
            reason: RtcRejectReason::Declined,
        };
        assert_eq!(budget.admit(9, &ended, now), SignalAdmit::DialogEnded);
        assert_eq!(budget.open_dialogs(9), MAX_DIALOGS_PER_PEER - 1);
        assert_eq!(budget.admit(9, &offer(99), now), SignalAdmit::NewDialog);
    }

    #[test]
    fn the_frame_window_refuses_and_then_refills() {
        let mut budget = SignalBudget::new();
        let start = Instant::now();
        // Reuse one dialog so the dialog bound is not what refuses.
        for _ in 0..MAX_FRAMES_PER_WINDOW {
            let admitted = budget.admit(3, &offer(1), start);
            assert!(!matches!(admitted, SignalAdmit::Refused(_)));
        }
        assert_eq!(
            budget.admit(3, &offer(1), start),
            SignalAdmit::Refused(RtcSignalError::OverBudget)
        );
        // A full window later the sender is welcome again.
        let later = start + BUDGET_WINDOW;
        assert!(!matches!(
            budget.admit(3, &offer(1), later),
            SignalAdmit::Refused(_)
        ));
    }

    #[test]
    fn budgets_are_per_sender_and_forgotten_with_the_session() {
        let mut budget = SignalBudget::new();
        let now = Instant::now();
        for d in 0..MAX_DIALOGS_PER_PEER as u64 {
            budget.admit(1, &offer(d), now);
        }
        assert_eq!(
            budget.admit(2, &offer(0), now),
            SignalAdmit::NewDialog,
            "one peer's exhaustion must not refuse another's first offer"
        );
        budget.forget(1);
        assert_eq!(budget.open_dialogs(1), 0);
    }

    #[test]
    fn rejecting_an_unknown_dialog_is_refused_not_acted_on() {
        let mut budget = SignalBudget::new();
        let now = Instant::now();
        assert_eq!(
            budget.admit(
                5,
                &RtcSignalMsg::Reject {
                    dialog: 1234,
                    reason: RtcRejectReason::Declined,
                },
                now
            ),
            SignalAdmit::Refused(RtcSignalError::UnknownDialog),
            "a Reject for a dialog that was never open must not end someone else's"
        );
    }
}
