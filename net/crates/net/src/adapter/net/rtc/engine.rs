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
    /// Whether the remote answer has been applied (meaningful on an
    /// offerer dialog). Set exactly once, so a duplicate `Answer` is
    /// a no-op rather than a second application — or, worse, the
    /// teardown of a dialog whose channel is opening.
    pub answered: bool,
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

    /// Record a dialog we are driving. Returns the displaced entry
    /// when the id was already live: a silent replacement would leak
    /// the predecessor's ICE session and double-count the attempt,
    /// so displacement is surfaced to the caller. A returned entry
    /// is a **live attempt** — the caller MUST retire it (close its
    /// session, terminal-charge it) and NEVER drop the `Option`.
    ///
    /// Displacement is only reachable for a caller that inserts into
    /// a key it neither checked nor vacated under the same
    /// table-lock hold. Every in-tree call site holds the lock
    /// across its check (`handle_signal`'s `Offer` arm and
    /// `start_dialog` short-circuit on [`DialogTable::peer_for`]) or
    /// across its remove-and-restore (`spawn_dialog_completion`'s
    /// lost-claim path re-inserts into the key it just removed, in
    /// one hold), so a displaced entry is unreachable in-tree — and
    /// retired as a live attempt regardless.
    pub fn insert(&mut self, peer_node: u64, dialog: u64, entry: Dialog) -> Option<Dialog> {
        self.dialogs.insert((peer_node, dialog), entry)
    }

    /// The RTC session a dialog is bound to.
    pub fn peer_for(&self, peer_node: u64, dialog: u64) -> Option<RtcPeerId> {
        self.dialogs.get(&(peer_node, dialog)).map(|d| d.peer)
    }

    /// The session a remote `Answer` may still be applied to: the
    /// dialog must be one WE offered, with no answer applied yet.
    /// `None` covers a duplicate `Answer` and an `Answer` for a
    /// dialog we answered — neither is terminal, there is simply
    /// nothing to do.
    fn open_answer(&self, peer_node: u64, dialog: u64) -> Option<RtcPeerId> {
        self.dialogs
            .get(&(peer_node, dialog))
            .filter(|d| d.offerer && !d.answered)
            .map(|d| d.peer)
    }

    /// Record that the remote answer applied to this dialog.
    fn mark_answered(&mut self, peer_node: u64, dialog: u64) {
        if let Some(d) = self.dialogs.get_mut(&(peer_node, dialog)) {
            d.answered = true;
        }
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

    /// How many dialogs this peer holds (R2): the dialog table's own
    /// view, which stays keyed by the peer the dialog names even
    /// when the signalling BUDGET is charged to an attempt identity
    /// the caller could not choose.
    pub fn open_for(&self, peer_node: u64) -> usize {
        self.dialogs
            .keys()
            .filter(|(node, _)| *node == peer_node)
            .count()
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
        /// The local endpoint this dialog runs on (R4): the caller
        /// trickles its candidate and owns the completion.
        peer: RtcPeerId,
    },
    /// The answer was applied; ICE is now running on `peer` (R4).
    AnswerApplied {
        /// The dialog this answer belongs to.
        dialog: u64,
        /// The local endpoint ICE is running on.
        peer: RtcPeerId,
    },
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
            // A duplicate Offer for a live dialog is IDEMPOTENT: the
            // row and the ICE session it names stay exactly as the
            // first Offer left them. Accepting it again would mint a
            // SECOND session, silently replace the live row (leaking
            // the predecessor's session) and move `ice_attempted`
            // twice for one attempt.
            if dialogs.peer_for(from_node, dialog).is_some() {
                return SignalOutcome::Ignored;
            }
            // A peer asking for a DataChannel. Accept the offer on a
            // fresh local session and answer over the same routed
            // path the offer arrived on.
            match driver.accept_offer(sdp).await {
                Ok((peer, answer)) => {
                    let displaced = dialogs.insert(
                        from_node,
                        dialog,
                        Dialog {
                            peer,
                            offerer: false,
                            deadline: Instant::now() + ice_deadline,
                            answered: false,
                        },
                    );
                    // Unreachable while callers hold the table lock
                    // across the `peer_for` short-circuit above — but
                    // a displaced row is a live attempt, never a
                    // dropped `Option` (#2).
                    retire_displaced_attempt(driver, displaced).await;
                    driver.stats().note_ice_attempted();
                    SignalOutcome::Answer {
                        dialog,
                        sdp: answer,
                        peer,
                    }
                }
                Err(_) => SignalOutcome::Reject {
                    dialog,
                    reason: RtcRejectReason::Busy,
                },
            }
        }
        RtcSignalMsg::Answer { dialog, sdp } => {
            let Some(peer) = dialogs.open_answer(from_node, dialog) else {
                // Nothing to apply: an answer for a dialog we never
                // offered (a late frame for one that closed), one for
                // a dialog WE answered, or a duplicate for an answer
                // already applied. A duplicate `Answer` is IDEMPOTENT
                // — the live dialog and the ICE session that may be
                // mid-open survive it untouched. This used to fall
                // into the error arm below and tear the dialog down,
                // counting `ice_failed` for an attempt whose channel
                // was opening.
                return SignalOutcome::Ignored;
            };
            match driver.accept_answer(peer, sdp).await {
                Ok(()) => {
                    dialogs.mark_answered(from_node, dialog);
                    SignalOutcome::AnswerApplied { dialog, peer }
                }
                Err(_) => {
                    // Terminal: the dialog is gone from the table,
                    // so the expiry sweep will never see it. An
                    // answer this driver cannot apply is `ice_failed`
                    // — the attempt cannot work, as distinct from
                    // running out of time.
                    dialogs.remove(from_node, dialog);
                    driver.stats().note_ice_failed();
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
                // Terminal, and counted here because the removal is
                // what takes the attempt away from the expiry
                // sweep. The peer refused: `ice_failed`, not
                // `ice_relayed`. `Ignored` below counts nothing —
                // a Reject for a dialog we do not hold is a late
                // frame for an attempt that already terminated, and
                // charging it would double-count that attempt.
                driver.stats().note_ice_failed();
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

/// Retire a row [`DialogTable::insert`] displaced (#2): it is a LIVE
/// attempt — its ICE session is closed and its attempt is
/// terminal-charged here, so no attempt can end with no terminal
/// owner. `None` — every in-tree call site — is a no-op.
async fn retire_displaced_attempt(driver: &RtcDriverHandle, displaced: Option<Dialog>) {
    if let Some(entry) = displaced {
        driver.stats().note_ice_failed();
        let _ = driver.close(entry.peer).await;
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
    // A live dialog with this id must not be silently replaced: the
    // displaced session would leak and the attempt's accounting
    // would move twice. The id is minted fresh, so this is refused
    // rather than absorbed.
    if dialogs.peer_for(to_node, dialog).is_some() {
        return Err("dialog id already live".into());
    }
    let (peer, sdp) = driver.create_offer().await?;
    let displaced = dialogs.insert(
        to_node,
        dialog,
        Dialog {
            peer,
            offerer: true,
            deadline: Instant::now() + ice_deadline,
            answered: false,
        },
    );
    retire_displaced_attempt(driver, displaced).await;
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
            answered: false,
        }
    }

    /// A real driver: the duplicate-frame witnesses below must
    /// observe real session allocation, not a mock's echo.
    async fn driver() -> RtcDriverHandle {
        use super::super::config::RtcConfig;
        use super::super::driver::RtcDriver;
        use super::super::stats::RtcStats;
        let (ingress_tx, _ingress_rx) = tokio::sync::mpsc::channel(8);
        let (closed_tx, _closed_rx) = tokio::sync::mpsc::channel(8);
        RtcDriver::spawn(
            RtcConfig::new(),
            "127.0.0.1:0".parse().expect("addr"),
            Arc::new(RtcStats::default()),
            ingress_tx,
            closed_tx,
        )
        .await
        .expect("driver")
    }

    /// A duplicate `Offer` for a live dialog is idempotent: the row
    /// and its ICE session survive untouched, no second session is
    /// allocated, and `ice_attempted` — the §10 partition's
    /// denominator — moves once per attempt, not once per frame.
    /// Inverse: pre-fix the duplicate was accepted, replacing the row
    /// (leaking the predecessor session) and re-counting the attempt.
    #[tokio::test]
    async fn a_duplicate_offer_is_idempotent() {
        let producer = driver().await;
        let responder = driver().await;
        let mut dialogs = DialogTable::new();
        let (_, sdp) = producer.create_offer().await.expect("offer");
        let offer = RtcSignalMsg::Offer { dialog: 3, sdp };
        let first = handle_signal(
            &responder,
            &mut dialogs,
            7,
            offer.clone(),
            Duration::from_secs(10),
        )
        .await;
        let SignalOutcome::Answer { peer, .. } = first else {
            panic!("the first offer is answered, got {first:?}");
        };

        let attempted = responder.stats().ice_attempted();
        let second =
            handle_signal(&responder, &mut dialogs, 7, offer, Duration::from_secs(10)).await;
        assert_eq!(
            second,
            SignalOutcome::Ignored,
            "a duplicate offer has nothing left to do"
        );
        assert_eq!(
            dialogs.peer_for(7, 3),
            Some(peer),
            "the live row — and the ICE session it names — are kept"
        );
        assert_eq!(
            responder.stats().ice_attempted(),
            attempted,
            "a duplicate offer must not move the ice_attempted denominator"
        );
        assert_eq!(
            responder.transport().retained_slots(),
            1,
            "the duplicate must not allocate a second ICE session"
        );
    }

    /// A duplicate `Answer` must not be terminal: the dialog whose
    /// channel is opening survives it, and `ice_failed` does not move.
    /// Inverse: pre-fix the second application failed and tore the
    /// dialog down as `ice_failed`.
    #[tokio::test]
    async fn a_duplicate_answer_is_not_terminal() {
        let offerer = driver().await;
        let answerer = driver().await;
        let mut dialogs = DialogTable::new();
        let started = start_dialog(&offerer, &mut dialogs, 9, 4, Duration::from_secs(10))
            .await
            .expect("dialog");
        let RtcSignalMsg::Offer { sdp, .. } = started else {
            panic!("start_dialog produces an offer");
        };
        let (_, answer) = answerer.accept_offer(sdp).await.expect("answer");
        let applied = handle_signal(
            &offerer,
            &mut dialogs,
            9,
            RtcSignalMsg::Answer {
                dialog: 4,
                sdp: answer.clone(),
            },
            Duration::from_secs(10),
        )
        .await;
        assert!(matches!(applied, SignalOutcome::AnswerApplied { .. }));

        let failed = offerer.stats().ice_failed();
        let duplicate = handle_signal(
            &offerer,
            &mut dialogs,
            9,
            RtcSignalMsg::Answer {
                dialog: 4,
                sdp: answer,
            },
            Duration::from_secs(10),
        )
        .await;
        assert_eq!(
            duplicate,
            SignalOutcome::Ignored,
            "a duplicate answer has nothing left to apply"
        );
        assert!(
            dialogs.peer_for(9, 4).is_some(),
            "the dialog survives its duplicate answer"
        );
        assert_eq!(
            offerer.stats().ice_failed(),
            failed,
            "a duplicate answer is not a terminal failure"
        );
    }

    /// The mirror case: an `Answer` aimed at a dialog WE answered —
    /// its channel is opening — must neither be applied over the
    /// opening session nor tear the dialog down.
    #[tokio::test]
    async fn an_answer_for_a_dialog_we_answered_is_ignored_not_terminal() {
        let producer = driver().await;
        let responder = driver().await;
        let mut dialogs = DialogTable::new();
        let (_, sdp) = producer.create_offer().await.expect("offer");
        let answered = handle_signal(
            &responder,
            &mut dialogs,
            7,
            RtcSignalMsg::Offer {
                dialog: 8,
                sdp: sdp.clone(),
            },
            Duration::from_secs(10),
        )
        .await;
        let SignalOutcome::Answer { peer, .. } = answered else {
            panic!("the offer is answered, got {answered:?}");
        };

        let failed = responder.stats().ice_failed();
        let mirror = handle_signal(
            &responder,
            &mut dialogs,
            7,
            RtcSignalMsg::Answer { dialog: 8, sdp },
            Duration::from_secs(10),
        )
        .await;
        assert_eq!(mirror, SignalOutcome::Ignored);
        assert_eq!(
            dialogs.peer_for(7, 8),
            Some(peer),
            "the opening dialog is untouched"
        );
        assert_eq!(
            responder.stats().ice_failed(),
            failed,
            "an answer we did not wait for is not a terminal failure"
        );
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
