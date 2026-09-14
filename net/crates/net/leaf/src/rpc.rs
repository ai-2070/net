//! The nRPC client: the call table and its dispositions.
//!
//! One rule dominates this module, and it comes from §8's
//! leader-lifecycle list: **a call never retries itself.** Every way
//! a call can end other than a reply is a typed failure handed to
//! the caller, who decides. A silent retry would re-execute a
//! non-idempotent handler the caller has no way to know already ran.
//!
//! | how it ends | what the caller gets |
//! |---|---|
//! | RESPONSE, status `Ok` | the body |
//! | RESPONSE, any other status | [`RpcError::Refused`] with the status and diagnostic |
//! | `DEADLINE_EXCEEDED` from the server | [`RpcError::Timeout`] |
//! | the local deadline elapses | [`RpcError::Timeout`] |
//! | the session carrying it goes away | [`RpcError::SessionLost`] |
//! | this tab stops being the leader | [`RpcError::LeaderLost`] |
//! | the reply does not decode | [`RpcError::Malformed`] |
//!
//! # Why the deadline is swept, not slept
//!
//! There is no timer in this crate. A wasm build has no runtime, and
//! a `tokio::time::sleep` would be a native-only dependency in the
//! one place that must work in a browser. So the table holds
//! deadlines and [`CallTable::expire`] sweeps them: the node calls it
//! on every inbound packet, and the wasm surface additionally arms a
//! `setTimeout` per call so a call still fails on time on an
//! otherwise silent connection. One mechanism, two triggers, and the
//! sweep is what the native test exercises.

use std::collections::HashMap;

use bytes::Bytes;
use futures_channel::oneshot;
use net_wire::clock::Instant;

use crate::clock::Deadline;
use crate::control_plane::NodeId;
use crate::counters::{DropReason, LeafCounters};
use crate::error::RpcError;
use crate::rpc_wire::{RpcFrame, RpcStatus};

/// Default call deadline when the caller gives none.
///
/// Thirty seconds is the same order as the SDK's transfer RPC
/// deadline and long enough to cross a routed anchor hop; a caller
/// that wants less passes `timeout_ms`.
pub const DEFAULT_CALL_TIMEOUT_MS: u64 = 30_000;

/// In-flight calls a leaf will hold.
///
/// A browser tab that leaked call slots would leak the oneshot
/// senders with them. 256 concurrent calls is far above any
/// interactive workload and bounds the table; past it, `register`
/// refuses with `RpcError::Backpressure`-shaped honesty rather
/// than growing.
pub const MAX_IN_FLIGHT_CALLS: usize = 256;

/// The receiver a caller awaits.
pub type CallResult = oneshot::Receiver<Result<Bytes, RpcError>>;

/// Who a reply must come from to be this call's reply.
///
/// A `call_id` is a correlation handle, not an authorization: it is
/// minted locally, it increments, and the peer a call was sent to
/// observes it. Three facts together say the reply is *this* call's
/// — the authenticated transport peer, the incarnation of the
/// session it arrived on, and the canonical reply channel the frame
/// declares. A pending entry records all three and is matched on all
/// three **before** it is consumed, so a wrong-owner frame neither
/// completes the call nor removes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallOwner {
    /// The authenticated peer the call was issued to.
    pub peer: NodeId,
    /// The incarnation of the session it rides.
    pub incarnation: u64,
    /// Canonical `u64` hash of the reply channel the response must
    /// declare (`RpcRouteV1`).
    pub reply_route: u64,
}

/// A call the sweep ended, and where its CANCEL must go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExpiredCall {
    /// The call that ended.
    pub call_id: u64,
    /// The peer it was issued to.
    pub peer: NodeId,
    /// The canonical request route it rode, so the CANCEL lands on
    /// the channel the server is serving.
    pub route: u64,
}

/// One registered call.
struct Pending {
    owner: CallOwner,
    /// Canonical request route, for the CANCEL.
    route: u64,
    deadline: Deadline,
    reply: oneshot::Sender<Result<Bytes, RpcError>>,
}

/// The call table: `call_id` → the pending call.
#[derive(Default)]
pub struct CallTable {
    pending: HashMap<u64, Pending>,
    next_call_id: u64,
}

impl core::fmt::Debug for CallTable {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("CallTable")
            .field("in_flight", &self.pending.len())
            .finish()
    }
}

impl CallTable {
    /// A table whose `call_id` sequence starts at `seed`.
    ///
    /// The seed is drawn from the CSPRNG by the node rather than
    /// starting at zero: a new leader for the same identity must not
    /// mint `call_id`s a replaced leader already had in flight, or a
    /// late reply to the old call would satisfy the new one. §8's
    /// stale-leader fencing, at the call-table level.
    pub fn with_seed(seed: u64) -> Self {
        Self {
            pending: HashMap::new(),
            next_call_id: seed,
        }
    }

    /// How many calls are in flight.
    pub fn in_flight(&self) -> usize {
        self.pending.len()
    }

    /// Whether nothing is in flight.
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// Register a call and get back its id and the caller's receiver.
    ///
    /// `owner` is the triple a reply must present; `request_route`
    /// is where a CANCEL for it goes.
    ///
    /// Refuses past [`MAX_IN_FLIGHT_CALLS`] — the table is bounded,
    /// and a refusal is a typed error the caller sees rather than a
    /// tab that grows until it dies.
    pub fn register(
        &mut self,
        owner: CallOwner,
        request_route: u64,
        timeout_ms: u64,
    ) -> Result<(u64, CallResult), RpcError> {
        if self.pending.len() >= MAX_IN_FLIGHT_CALLS {
            return Err(RpcError::Refused {
                status: RpcStatus::Backpressure.to_wire(),
                message: format!("this leaf already holds {MAX_IN_FLIGHT_CALLS} calls in flight"),
            });
        }
        let call_id = self.next_call_id;
        self.next_call_id = self.next_call_id.wrapping_add(1);
        let (reply, receiver) = oneshot::channel();
        self.pending.insert(
            call_id,
            Pending {
                owner,
                route: request_route,
                deadline: Deadline::in_ms(timeout_ms),
                reply,
            },
        );
        Ok((call_id, receiver))
    }

    /// Cancel a registered call locally without delivering anything.
    ///
    /// Returns the peer it was addressed to, so the caller can emit
    /// the matching CANCEL frame. Dropping the sender makes the
    /// caller's await resolve as cancelled.
    pub fn take(&mut self, call_id: u64) -> Option<NodeId> {
        self.pending.remove(&call_id).map(|p| p.owner.peer)
    }

    /// Deliver an inbound reply frame presented by `presented`.
    ///
    /// The entry is inspected before it is removed: an id that
    /// matches but whose peer, session incarnation or reply route
    /// does not is **not** this call's reply, so it neither
    /// completes the call nor takes its slot, and the correct reply
    /// can still arrive afterwards.
    ///
    /// `false` means nothing matched — a late reply to a call that
    /// already ended, or a wrong-owner frame — and the counter
    /// moves.
    pub fn deliver(
        &mut self,
        frame: RpcFrame,
        presented: CallOwner,
        counters: &LeafCounters,
    ) -> bool {
        let call_id = match &frame {
            RpcFrame::Response { call_id, .. } | RpcFrame::DeadlineExceeded { call_id } => *call_id,
        };
        match self.pending.get(&call_id) {
            Some(pending) if pending.owner == presented => {}
            _ => {
                counters.drop_for(DropReason::UnknownCall);
                return false;
            }
        }
        let Some(pending) = self.pending.remove(&call_id) else {
            counters.drop_for(DropReason::UnknownCall);
            return false;
        };
        let outcome = match frame {
            RpcFrame::DeadlineExceeded { .. } => Err(RpcError::Timeout),
            RpcFrame::Response { payload, .. } => {
                if payload.status.is_ok() {
                    Ok(payload.body)
                } else {
                    Err(RpcError::Refused {
                        status: payload.status.to_wire(),
                        // A non-`Ok` response carries a UTF-8
                        // diagnostic in the body by convention;
                        // lossy is right here — a malformed
                        // diagnostic must not mask the status.
                        message: String::from_utf8_lossy(&payload.body).into_owned(),
                    })
                }
            }
        };
        // A closed receiver means the caller stopped awaiting. Not
        // an error: the call still ended exactly once.
        let _ = pending.reply.send(outcome);
        true
    }

    /// Fail every call whose deadline has passed as of `now`.
    ///
    /// Returns each call with the peer and route it rode, so the
    /// caller can emit a CANCEL for it — the server is told, rather
    /// than left running work nobody awaits.
    pub fn expire(&mut self, now: Instant) -> Vec<ExpiredCall> {
        let expired: Vec<ExpiredCall> = self
            .pending
            .iter()
            .filter(|(_, p)| p.deadline.expired_at(now))
            .map(|(id, p)| ExpiredCall {
                call_id: *id,
                peer: p.owner.peer,
                route: p.route,
            })
            .collect();
        for call in &expired {
            if let Some(pending) = self.pending.remove(&call.call_id) {
                let _ = pending.reply.send(Err(RpcError::Timeout));
            }
        }
        expired
    }

    /// Fail every call owned by one session incarnation.
    ///
    /// The replacement rule: a re-handshake with the same peer
    /// retires the predecessor's calls **exactly once**, typed, and
    /// leaves the successor's — which carry the new incarnation —
    /// untouched. Keying on the peer alone cannot express that,
    /// because both incarnations have the same peer.
    pub fn fail_incarnation(&mut self, incarnation: u64) -> usize {
        let lost: Vec<u64> = self
            .pending
            .iter()
            .filter(|(_, p)| p.owner.incarnation == incarnation)
            .map(|(id, _)| *id)
            .collect();
        for id in &lost {
            if let Some(pending) = self.pending.remove(id) {
                let _ = pending.reply.send(Err(RpcError::SessionLost));
            }
        }
        lost.len()
    }

    /// Fail every call riding a lost session, and say which.
    ///
    /// The §8 rule: pending calls on leader loss or session loss
    /// fail **typed**, never silently re-issued.
    pub fn fail_peer(&mut self, peer: NodeId) -> usize {
        let lost: Vec<u64> = self
            .pending
            .iter()
            .filter(|(_, p)| p.owner.peer == peer)
            .map(|(id, _)| *id)
            .collect();
        for id in &lost {
            if let Some(pending) = self.pending.remove(id) {
                let _ = pending.reply.send(Err(RpcError::SessionLost));
            }
        }
        lost.len()
    }

    /// Fail every call with `error` — leader replacement, or close.
    pub fn fail_all(&mut self, error: RpcError) -> usize {
        let count = self.pending.len();
        for (_, pending) in self.pending.drain() {
            let _ = pending.reply.send(Err(error.clone()));
        }
        count
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::now;
    use crate::rpc_wire::RpcResponsePayload;

    const PEER: NodeId = 0xAAAA;
    const OTHER: NodeId = 0xBBBB;
    const INCARNATION: u64 = 42;
    const REQUEST_ROUTE: u64 = 0x1111_2222_3333_4444;
    const REPLY_ROUTE: u64 = 0x5555_6666_7777_8888;

    fn owner(peer: NodeId) -> CallOwner {
        CallOwner {
            peer,
            incarnation: INCARNATION,
            reply_route: REPLY_ROUTE,
        }
    }

    fn response(call_id: u64, status: RpcStatus, body: &'static [u8]) -> RpcFrame {
        RpcFrame::Response {
            call_id,
            payload: RpcResponsePayload {
                status,
                headers: Vec::new(),
                body: Bytes::from_static(body),
            },
        }
    }

    fn taken(rx: &mut CallResult) -> Result<Bytes, RpcError> {
        rx.try_recv()
            .expect("the sender must not have been dropped")
            .expect("the call must have been resolved")
    }

    #[test]
    fn a_successful_reply_reaches_the_caller_and_frees_the_slot() {
        let c = LeafCounters::new();
        let mut table = CallTable::with_seed(100);
        let (call_id, mut rx) = table
            .register(owner(PEER), REQUEST_ROUTE, 1_000)
            .expect("register");
        assert_eq!(call_id, 100);
        assert_eq!(table.in_flight(), 1);

        assert!(table.deliver(response(call_id, RpcStatus::Ok, b"NMO1"), owner(PEER), &c));
        assert_eq!(taken(&mut rx).expect("Ok"), Bytes::from_static(b"NMO1"));
        assert!(table.is_empty(), "a resolved call must free its slot");
        assert_eq!(c.total_drops(), 0);
    }

    /// A typed refusal is a *result*, not an error to be retried.
    #[test]
    fn a_non_ok_status_becomes_a_typed_refusal_carrying_the_diagnostic() {
        let c = LeafCounters::new();
        let mut table = CallTable::with_seed(0);
        let (call_id, mut rx) = table
            .register(owner(PEER), REQUEST_ROUTE, 1_000)
            .expect("register");
        table.deliver(
            response(call_id, RpcStatus::AdmissionDenied, b"not admitted"),
            owner(PEER),
            &c,
        );
        match taken(&mut rx) {
            Err(RpcError::Refused { status, message }) => {
                assert_eq!(status, RpcStatus::AdmissionDenied.to_wire());
                assert_eq!(message, "not admitted");
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn a_server_deadline_frame_is_a_timeout_not_a_refusal() {
        let c = LeafCounters::new();
        let mut table = CallTable::with_seed(0);
        let (call_id, mut rx) = table
            .register(owner(PEER), REQUEST_ROUTE, 1_000)
            .expect("register");
        table.deliver(RpcFrame::DeadlineExceeded { call_id }, owner(PEER), &c);
        assert_eq!(taken(&mut rx), Err(RpcError::Timeout));
    }

    /// The local deadline: swept, and the ids come back so a CANCEL
    /// can follow.
    #[test]
    fn the_local_deadline_produces_a_timeout_and_names_the_call() {
        let mut table = CallTable::with_seed(0);
        let (soon, mut soon_rx) = table
            .register(owner(PEER), REQUEST_ROUTE, 10)
            .expect("register");
        let (later, mut later_rx) = table
            .register(owner(PEER), REQUEST_ROUTE, 60_000)
            .expect("register");

        let t0 = now();
        assert!(table.expire(t0).is_empty(), "nothing is due yet");

        let after = t0 + core::time::Duration::from_millis(20);
        assert_eq!(
            table.expire(after),
            vec![ExpiredCall {
                call_id: soon,
                peer: PEER,
                route: REQUEST_ROUTE
            }],
            "exactly the due call, with the peer and route a CANCEL needs"
        );
        assert_eq!(taken(&mut soon_rx), Err(RpcError::Timeout));
        assert_eq!(table.in_flight(), 1);
        assert!(
            later_rx.try_recv().expect("still open").is_none(),
            "an undue call must not be touched"
        );
        assert_eq!(later, 1);
    }

    /// The §8 rule, as a test: a lost session fails its calls
    /// typed, and leaves everyone else's alone.
    #[test]
    fn a_lost_session_fails_only_its_own_calls_and_never_retries() {
        let mut table = CallTable::with_seed(0);
        let (_a, mut a_rx) = table
            .register(owner(PEER), REQUEST_ROUTE, 60_000)
            .expect("register");
        let (_b, mut b_rx) = table
            .register(owner(PEER), REQUEST_ROUTE, 60_000)
            .expect("register");
        let (_c, mut c_rx) = table
            .register(owner(OTHER), REQUEST_ROUTE, 60_000)
            .expect("register");

        assert_eq!(table.fail_peer(PEER), 2);
        assert_eq!(taken(&mut a_rx), Err(RpcError::SessionLost));
        assert_eq!(taken(&mut b_rx), Err(RpcError::SessionLost));
        assert!(
            c_rx.try_recv().expect("still open").is_none(),
            "another peer's calls must survive"
        );
        assert_eq!(table.in_flight(), 1);
    }

    #[test]
    fn leader_loss_fails_everything_with_the_generation_that_owned_it() {
        let mut table = CallTable::with_seed(0);
        let (_a, mut a_rx) = table
            .register(owner(PEER), REQUEST_ROUTE, 60_000)
            .expect("register");
        let (_b, mut b_rx) = table
            .register(owner(OTHER), REQUEST_ROUTE, 60_000)
            .expect("register");

        assert_eq!(table.fail_all(RpcError::LeaderLost { generation: 7 }), 2);
        assert_eq!(
            taken(&mut a_rx),
            Err(RpcError::LeaderLost { generation: 7 })
        );
        assert_eq!(
            taken(&mut b_rx),
            Err(RpcError::LeaderLost { generation: 7 })
        );
        assert!(table.is_empty());
    }

    #[test]
    fn a_late_reply_to_a_finished_call_is_dropped_and_counted() {
        let c = LeafCounters::new();
        let mut table = CallTable::with_seed(0);
        let (call_id, _rx) = table
            .register(owner(PEER), REQUEST_ROUTE, 1_000)
            .expect("register");
        assert!(table.deliver(response(call_id, RpcStatus::Ok, b"first"), owner(PEER), &c));
        assert!(
            !table.deliver(response(call_id, RpcStatus::Ok, b"second"), owner(PEER), &c),
            "a second reply for the same call must not be delivered"
        );
        assert_eq!(c.drops(DropReason::UnknownCall), 1);
    }

    #[test]
    fn a_dropped_caller_does_not_break_delivery_accounting() {
        let c = LeafCounters::new();
        let mut table = CallTable::with_seed(0);
        let (call_id, rx) = table
            .register(owner(PEER), REQUEST_ROUTE, 1_000)
            .expect("register");
        drop(rx); // the caller stopped awaiting
        assert!(
            table.deliver(response(call_id, RpcStatus::Ok, b"x"), owner(PEER), &c),
            "the call still ended exactly once"
        );
        assert!(table.is_empty());
        assert_eq!(c.total_drops(), 0);
    }

    /// The ownership triple, one field at a time: each mismatch
    /// must leave the entry where it is, and the right reply must
    /// still succeed afterwards.
    #[test]
    fn a_reply_is_refused_unless_peer_incarnation_and_route_all_match() {
        for wrong in [
            CallOwner {
                peer: OTHER,
                incarnation: INCARNATION,
                reply_route: REPLY_ROUTE,
            },
            CallOwner {
                peer: PEER,
                incarnation: INCARNATION + 1,
                reply_route: REPLY_ROUTE,
            },
            CallOwner {
                peer: PEER,
                incarnation: INCARNATION,
                reply_route: REPLY_ROUTE ^ 1,
            },
        ] {
            let c = LeafCounters::new();
            let mut table = CallTable::with_seed(0);
            let (call_id, mut rx) = table
                .register(owner(PEER), REQUEST_ROUTE, 60_000)
                .expect("register");
            assert!(
                !table.deliver(response(call_id, RpcStatus::Ok, b"stolen"), wrong, &c),
                "{wrong:?} must not complete the call"
            );
            assert_eq!(table.in_flight(), 1, "{wrong:?} must not consume the entry");
            assert!(rx.try_recv().expect("still open").is_none());
            assert_eq!(c.drops(DropReason::UnknownCall), 1);

            assert!(table.deliver(response(call_id, RpcStatus::Ok, b"real"), owner(PEER), &c));
            assert_eq!(taken(&mut rx).expect("Ok"), Bytes::from_static(b"real"));
        }
    }

    /// Replacement retires the predecessor's calls and nothing else.
    #[test]
    fn failing_one_incarnation_leaves_the_successors_calls_alone() {
        let mut table = CallTable::with_seed(0);
        let (_old, mut old_rx) = table
            .register(owner(PEER), REQUEST_ROUTE, 60_000)
            .expect("register");
        let successor = CallOwner {
            peer: PEER,
            incarnation: INCARNATION + 1,
            reply_route: REPLY_ROUTE,
        };
        let (_new, mut new_rx) = table
            .register(successor, REQUEST_ROUTE, 60_000)
            .expect("register");

        assert_eq!(table.fail_incarnation(INCARNATION), 1);
        assert_eq!(taken(&mut old_rx), Err(RpcError::SessionLost));
        assert!(
            new_rx.try_recv().expect("still open").is_none(),
            "the successor's call must survive its predecessor's retirement"
        );
        assert_eq!(
            table.fail_incarnation(INCARNATION),
            0,
            "an incarnation is retired exactly once"
        );
    }

    #[test]
    fn the_table_is_bounded_and_refuses_rather_than_growing() {
        let mut table = CallTable::with_seed(0);
        let mut held = Vec::new();
        for _ in 0..MAX_IN_FLIGHT_CALLS {
            held.push(
                table
                    .register(owner(PEER), REQUEST_ROUTE, 60_000)
                    .expect("under the bound"),
            );
        }
        match table.register(owner(PEER), REQUEST_ROUTE, 60_000) {
            Err(RpcError::Refused { status, .. }) => {
                assert_eq!(status, RpcStatus::Backpressure.to_wire())
            }
            other => panic!("expected a bounded refusal, got {other:?}"),
        }
        assert_eq!(table.in_flight(), MAX_IN_FLIGHT_CALLS);
    }

    #[test]
    fn cancelling_a_call_returns_its_peer_and_frees_the_slot() {
        let mut table = CallTable::with_seed(0);
        let (call_id, _rx) = table
            .register(owner(PEER), REQUEST_ROUTE, 60_000)
            .expect("register");
        assert_eq!(table.take(call_id), Some(PEER));
        assert!(table.is_empty());
        assert_eq!(
            table.take(call_id),
            None,
            "cancelling twice is not an error"
        );
    }

    /// Two leaders for one identity must not mint colliding ids.
    #[test]
    fn seeded_call_ids_do_not_restart_at_zero() {
        let mut first = CallTable::with_seed(0x1000);
        let mut second = CallTable::with_seed(0x9000);
        let (a, _ra) = first
            .register(owner(PEER), REQUEST_ROUTE, 1)
            .expect("register");
        let (b, _rb) = second
            .register(owner(PEER), REQUEST_ROUTE, 1)
            .expect("register");
        assert_ne!(a, b);
        assert_eq!(a, 0x1000);
        assert_eq!(b, 0x9000);
    }
}
