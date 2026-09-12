//! The signalling engine: `0x0D02` frames in, an installed direct
//! session out (plan §9 steps 3–6).
//!
//! This is the piece that makes §5's three layers into one flow.
//! Layer 1 gave us the peer's Noise key from its signed
//! announcement; Layer 2 gave us a routed session through an anchor;
//! Layer 3 is the frames this engine exchanges over that session.
//! When ICE connects, the direct DataChannel gets its own Noise
//! handshake and **replaces** the routed session through the
//! ordinary installer — CAS, quiescence gate and live-handle fence
//! as Stage 3's repair left them.
//!
//! Three rules, each one a thing that went wrong somewhere before:
//!
//! 1. **A failed ICE attempt changes nothing.** If the deadline
//!    passes, the routed session is simply never replaced,
//!    `ice_relayed` increments, and the peer stays reachable. The
//!    plan is explicit that ICE reaching `connected` is not a health
//!    gate.
//! 2. **Retry is never per packet** (§9 step 6). The engine answers
//!    what it is sent and offers when asked to; it does not watch
//!    traffic and re-offer.
//! 3. **The anchor in the middle reads nothing.** It forwards the
//!    packets carrying these frames as ordinary routed traffic and
//!    holds no key for them.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::driver::RtcDriverHandle;
use super::signal::{RtcRejectReason, RtcSignalMsg};
use super::RtcPeerId;

/// One dialog this node is driving.
#[derive(Debug)]
pub struct Dialog {
    /// The local RTC session backing it.
    pub peer: RtcPeerId,
    /// Whether we sent the offer.
    pub offerer: bool,
    /// When the attempt must be abandoned.
    pub deadline: Instant,
}

/// Dialogs keyed by `(peer_node_id, dialog)`.
#[derive(Debug, Default)]
pub struct DialogTable {
    dialogs: HashMap<(u64, u64), Dialog>,
}

impl DialogTable {
    /// Empty table.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a dialog we are driving.
    pub fn insert(&mut self, peer_node: u64, dialog: u64, entry: Dialog) {
        self.dialogs.insert((peer_node, dialog), entry);
    }

    /// The RTC session a dialog is bound to.
    pub fn peer_for(&self, peer_node: u64, dialog: u64) -> Option<RtcPeerId> {
        self.dialogs.get(&(peer_node, dialog)).map(|d| d.peer)
    }

    /// Forget a dialog (answered, rejected, or timed out).
    pub fn remove(&mut self, peer_node: u64, dialog: u64) -> Option<Dialog> {
        self.dialogs.remove(&(peer_node, dialog))
    }

    /// Dialogs whose deadline has passed, drained.
    pub fn take_expired(&mut self, now: Instant) -> Vec<((u64, u64), Dialog)> {
        let expired: Vec<(u64, u64)> = self
            .dialogs
            .iter()
            .filter(|(_, d)| d.deadline <= now)
            .map(|(k, _)| *k)
            .collect();
        expired
            .into_iter()
            .filter_map(|k| self.dialogs.remove(&k).map(|d| (k, d)))
            .collect()
    }

    /// How many dialogs are open.
    pub fn len(&self) -> usize {
        self.dialogs.len()
    }

    /// Whether any dialog is open.
    pub fn is_empty(&self) -> bool {
        self.dialogs.is_empty()
    }
}

/// What the engine decided to do with one inbound frame. The caller
/// performs the effects, so this stays testable without a mesh.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignalOutcome {
    /// Answer this offer with `sdp` on `dialog`.
    Answer {
        /// Dialog being answered.
        dialog: u64,
        /// The answer SDP produced by the local driver.
        sdp: String,
    },
    /// The answer was applied; ICE is now running.
    AnswerApplied,
    /// The candidate was applied.
    CandidateApplied,
    /// The dialog ended.
    Ended(RtcRejectReason),
    /// Nothing to do — a frame for a dialog we do not have.
    Ignored,
    /// The local driver refused; tell the peer.
    Reject {
        /// Dialog to reject.
        dialog: u64,
        /// Why.
        reason: RtcRejectReason,
    },
}

/// Drive one inbound frame against the driver and the dialog table.
///
/// Separated from the mesh so the decision can be witnessed without
/// standing up two nodes: everything mesh-shaped (sending the
/// answer, installing the session) is the caller's, and everything
/// str0m-shaped is the driver's.
pub async fn handle_signal(
    driver: &RtcDriverHandle,
    dialogs: &mut DialogTable,
    from_node: u64,
    msg: RtcSignalMsg,
    ice_deadline: Duration,
) -> SignalOutcome {
    match msg {
        RtcSignalMsg::Offer { dialog, sdp } => {
            // A peer asking for a DataChannel. Accept the offer on a
            // fresh local session and answer over the same routed
            // path the offer arrived on.
            match driver.accept_offer(sdp).await {
                Ok((peer, answer)) => {
                    dialogs.insert(
                        from_node,
                        dialog,
                        Dialog {
                            peer,
                            offerer: false,
                            deadline: Instant::now() + ice_deadline,
                        },
                    );
                    driver.stats().note_ice_attempted();
                    SignalOutcome::Answer {
                        dialog,
                        sdp: answer,
                    }
                }
                Err(_) => SignalOutcome::Reject {
                    dialog,
                    reason: RtcRejectReason::Busy,
                },
            }
        }
        RtcSignalMsg::Answer { dialog, sdp } => {
            let Some(peer) = dialogs.peer_for(from_node, dialog) else {
                // An answer for a dialog we never offered. Not an
                // error worth ending anything over — it is a late
                // frame for a dialog that already closed.
                return SignalOutcome::Ignored;
            };
            match driver.accept_answer(peer, sdp).await {
                Ok(()) => SignalOutcome::AnswerApplied,
                Err(_) => {
                    dialogs.remove(from_node, dialog);
                    SignalOutcome::Reject {
                        dialog,
                        reason: RtcRejectReason::Declined,
                    }
                }
            }
        }
        RtcSignalMsg::Candidate {
            dialog, candidate, ..
        } => {
            let Some(peer) = dialogs.peer_for(from_node, dialog) else {
                return SignalOutcome::Ignored;
            };
            match driver.remote_candidate(peer, candidate).await {
                Ok(()) => SignalOutcome::CandidateApplied,
                // A candidate we cannot parse is one path lost, not
                // the dialog: ICE has others.
                Err(_) => SignalOutcome::Ignored,
            }
        }
        RtcSignalMsg::Reject { dialog, reason } => {
            if let Some(entry) = dialogs.remove(from_node, dialog) {
                let driver = driver.clone();
                let peer = entry.peer;
                tokio::spawn(async move {
                    let _ = driver.close(peer).await;
                });
                SignalOutcome::Ended(reason)
            } else {
                SignalOutcome::Ignored
            }
        }
    }
}

/// Start a dialog: create a local session, produce the offer, and
/// record the attempt. The caller sends the returned frame.
pub async fn start_dialog(
    driver: &RtcDriverHandle,
    dialogs: &mut DialogTable,
    to_node: u64,
    dialog: u64,
    ice_deadline: Duration,
) -> Result<RtcSignalMsg, String> {
    let (peer, sdp) = driver.create_offer().await?;
    dialogs.insert(
        to_node,
        dialog,
        Dialog {
            peer,
            offerer: true,
            deadline: Instant::now() + ice_deadline,
        },
    );
    driver.stats().note_ice_attempted();
    Ok(RtcSignalMsg::Offer { dialog, sdp })
}

/// Abandon dialogs past their deadline (§9 step 6).
///
/// The routed session is *not* touched: it was never replaced, so
/// there is nothing to restore. `ice_relayed` records that the pair
/// stayed on the anchor, which is the deployment telemetry §10 asks
/// for — reported, never gated.
pub async fn expire_dialogs(
    driver: &RtcDriverHandle,
    dialogs: &mut DialogTable,
    now: Instant,
) -> Vec<(u64, u64)> {
    let expired = dialogs.take_expired(now);
    let mut ended = Vec::new();
    for ((node, dialog), entry) in expired {
        driver.stats().note_ice_relayed();
        let _ = driver.close(entry.peer).await;
        ended.push((node, dialog));
    }
    ended
}

/// Shared dialog table handle for the mesh.
pub type SharedDialogs = Arc<tokio::sync::Mutex<DialogTable>>;

#[cfg(test)]
mod tests {
    use super::*;

    fn dialog(deadline: Instant) -> Dialog {
        Dialog {
            peer: RtcPeerId {
                slot: 1,
                generation: 0,
            },
            offerer: true,
            deadline,
        }
    }

    #[test]
    fn a_dialog_is_keyed_by_peer_and_id_not_by_id_alone() {
        let mut table = DialogTable::new();
        let now = Instant::now();
        table.insert(1, 7, dialog(now + Duration::from_secs(10)));
        assert_eq!(
            table.peer_for(2, 7),
            None,
            "another peer's dialog 7 is not this one — the id is chosen by the \
             offerer and is meaningful only between the two endpoints"
        );
        assert!(table.peer_for(1, 7).is_some());
    }

    #[test]
    fn expiry_drains_only_what_is_past_its_deadline() {
        let mut table = DialogTable::new();
        let now = Instant::now();
        table.insert(1, 1, dialog(now - Duration::from_secs(1)));
        table.insert(1, 2, dialog(now + Duration::from_secs(30)));
        let expired = table.take_expired(now);
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].0, (1, 1));
        assert_eq!(
            table.len(),
            1,
            "the live dialog survives its sibling's expiry"
        );
    }

    #[test]
    fn removing_a_dialog_twice_is_not_an_error() {
        let mut table = DialogTable::new();
        table.insert(3, 9, dialog(Instant::now() + Duration::from_secs(5)));
        assert!(table.remove(3, 9).is_some());
        assert!(
            table.remove(3, 9).is_none(),
            "a Reject racing an expiry must not panic or resurrect anything"
        );
        assert!(table.is_empty());
    }
}
