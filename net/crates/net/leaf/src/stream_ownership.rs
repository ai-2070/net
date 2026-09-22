//! Ownership of the streams a leader holds for proxy callers.
//!
//! A stream id is an application label scoped to a session, and the
//! peer is no part of its derivation, so **one label opened to two
//! peers is one id on two sessions** — a supported composition
//! ([`crate::node::LeafEvent::StreamData`]), not a collision. The wire
//! id therefore does not identify an *open*, and a table keyed by it
//! let the second open overwrite the first's slot: after that the
//! first caller's `send` reached, and its `close` closed, the second
//! peer's stream.
//!
//! This type is the one place that decision lives. It is production
//! code used by the real backend **and** by the test backend the
//! browser witnesses drive, deliberately: an ownership mechanism with
//! a second copy in the test fixture is a mechanism whose witnesses
//! cannot see it change.
//!
//! Handles are allocated monotonically and never reused, so a handle
//! cannot come to name a different open — the same argument one level
//! down from `ProxyStream`'s generation stamp.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

/// The identity a backend resolves from the stream it opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ResolvedIdentity {
    /// The wire stream id, which two peers may share.
    pub wire_id: u64,
    /// The authenticated peer this open is with.
    pub peer: u64,
    /// The incarnation of the session it was opened on.
    pub incarnation: u64,
}

/// A stream that can state its own identity.
///
/// Implemented for the types a backend actually holds, so the identity
/// on the open reply comes from **the stream the node opened** and not
/// from the request's options — which are what the caller *asked for*.
/// A backend cannot echo instead, because [`StreamOwnership::adopt`]
/// is what produces the reply's fields.
pub trait StreamIdentity {
    /// `None` when the identity cannot be read, which is a refusal.
    fn identity(&self) -> Option<ResolvedIdentity>;
}

/// Read an identity out of the spellings the wasm boundary hands over.
///
/// Fails closed on anything unreadable: a stream whose identity cannot
/// be parsed is not handed over with the peer left unknown, because a
/// handle that cannot name its peer cannot filter by it.
pub fn resolve_identity(
    stream_id_hex: &str,
    peer_node_hex: &str,
    incarnation: &str,
) -> Option<ResolvedIdentity> {
    Some(ResolvedIdentity {
        wire_id: u64::from_str_radix(stream_id_hex, 16).ok()?,
        peer: u64::from_str_radix(peer_node_hex, 16).ok()?,
        incarnation: incarnation.parse().ok()?,
    })
}

impl StreamIdentity for crate::node::StreamHandle {
    fn identity(&self) -> Option<ResolvedIdentity> {
        Some(ResolvedIdentity {
            wire_id: self.stream_id,
            peer: self.peer,
            incarnation: self.incarnation,
        })
    }
}

#[cfg(target_arch = "wasm32")]
impl StreamIdentity for crate::wasm::LeafStream {
    fn identity(&self) -> Option<ResolvedIdentity> {
        resolve_identity(
            &self.stream_id_hex(),
            &self.peer_node_hex(),
            &self.incarnation(),
        )
    }
}

/// What a backend can do with the streams it holds.
///
/// Three type-specific actions and nothing else: **which** stream an
/// operation reaches, what the reply says and how a refusal is spelled
/// all belong to [`answer_stream_request`], which is shared. A backend
/// that reconstructed any of those would be a second implementation of
/// the ownership rule, and the fixture that reconstructed its reply is
/// exactly how "the backend cannot answer with the requested peer"
/// came to be a comment rather than a property.
pub trait StreamBackend {
    /// The stream object this backend holds.
    type Stream: StreamIdentity;

    /// Open one, from the request's own fields.
    fn open(
        &self,
        label: &str,
        reliability: crate::stream::Reliability,
        stream_id: Option<u64>,
        channel_hash: Option<u16>,
        peer: Option<u64>,
    ) -> Result<Self::Stream, crate::leader::ProxyFailure>;

    /// Put bytes on one this backend owns.
    fn send(
        &self,
        stream: &Self::Stream,
        payload: &[u8],
    ) -> Result<(), crate::leader::ProxyFailure>;

    /// Give one up.
    fn close(&self, stream: Self::Stream);
}

/// Why [`StreamOwnership::adopt`] refused a stream — with the stream
/// itself handed back in every case, and a **refusal-specific
/// disposition** for it (see [`AdoptRefusal::dispose`]).
///
/// Three refusals, three dispositions on the reply: "your open did
/// not resolve", "this identity is already open" and "the handle
/// space is done" send an operator to different places, so one
/// message for all three would be a lie for two of them. The
/// dispositions differ just as much: one of the three must NOT be
/// closed.
#[derive(Debug)]
pub enum AdoptRefusal<T> {
    /// The stream did not report a readable identity.
    Unreadable(T),
    /// A live open already holds this identity — see
    /// [`StreamOwnership::adopt`].
    Duplicate(ResolvedIdentity, T),
    /// The handle space is exhausted: every value has been issued
    /// exactly once and none will be reissued.
    Exhausted(T),
}

impl<T> AdoptRefusal<T> {
    /// Dispose of the refused stream the way **this** refusal
    /// requires.
    ///
    /// [`Self::Unreadable`] and [`Self::Exhausted`] name a fresh open
    /// nothing else references: its node registration is its own to
    /// give back, so it goes to `close` (the backend's
    /// [`StreamBackend::close`]).
    ///
    /// [`Self::Duplicate`] must **not** close. The refused stream is
    /// a second wrapper on the SURVIVOR's one node stream — identity
    /// is `(wire id, peer, incarnation)` and the veto fired because a
    /// live open already holds exactly that — while the close chain
    /// is shared: `close` reaches `LeafNode::close_stream`, whose
    /// `rx_closed` insert and `stream_kinds` remove are keyed on the
    /// shared `(incarnation, stream id)`. Closing the refused wrapper
    /// is therefore the exact failure mode the veto exists to
    /// prevent, re-created by its own refusal: the survivor's receive
    /// half dies on a duplicate attempt that returned `Err`, counted
    /// `StreamClosed` while `stream_send` keeps working. The wrapper
    /// is discarded instead — it owns nothing the survivor's own
    /// close will not give back (a refused open is disposed before
    /// any handle is handed out, so its listener list is empty).
    pub fn dispose(self, close: impl FnOnce(T)) {
        match self {
            Self::Unreadable(stream) | Self::Exhausted(stream) => close(stream),
            Self::Duplicate(_, stream) => drop(stream),
        }
    }
}

/// Answer one stream request, for either backend.
///
/// Every decision is here: the handle an operation addresses, the
/// identity the open reply carries, and the refusal a closed handle
/// gets.
///
/// **Returns `None` once it has answered**, and `Some((request,
/// reply))` for a request it does not handle — the request **with its
/// replier**, still live. Handing the operation back without its
/// completion authority is not a hand-back: an unanswered `Replier`
/// answers `SessionLost` when it drops, so a caller that received only
/// the enum would find its consumer already told the call had failed,
/// and would have nothing left to answer with.
pub fn answer_stream_request<B: StreamBackend>(
    owned: &StreamOwnership<B::Stream>,
    backend: &B,
    request: crate::leader::LeaderRequest,
    reply: crate::leader::Replier,
) -> Option<(crate::leader::LeaderRequest, crate::leader::Replier)>
where
    B::Stream: Clone,
{
    use crate::error::LeafError;
    use crate::leader::{LeaderRequest, ProxyFailure};

    match request {
        LeaderRequest::StreamOpen {
            label,
            reliability,
            stream_id,
            channel_hash,
            peer,
        } => {
            match backend.open(&label, reliability, stream_id, channel_hash, peer) {
                Ok(stream) => match owned.adopt(stream) {
                    // The identity comes off the stream the node
                    // opened, not off the request. The requested
                    // `peer` IS still in scope and `Option<u64>` is
                    // `Copy`, so answering with it compiles — what
                    // makes that a caught mistake rather than an
                    // impossible one is that this construction is
                    // shared and both browser witnesses discriminate
                    // it.
                    Ok((handle, id)) => {
                        reply.stream(handle, id.wire_id, id.peer, id.incarnation);
                    }
                    Err(refusal) => {
                        // Fail closed either way: a refused stream is
                        // never handed over half-identified or left
                        // aliasing a live open. Each refusal says
                        // which it was — "your open did not resolve",
                        // "this identity is already open" and "the
                        // handle space is done" are three different
                        // things to act on — and the disposal differs
                        // too, which is why it lives in
                        // `AdoptRefusal::dispose`: a duplicate's
                        // wrapper is an alias of the SURVIVOR's one
                        // node stream and is discarded WITHOUT the
                        // shared close chain, whose `rx_closed`/
                        // `stream_kinds` writes would take the
                        // survivor's receive half with it.
                        let message = match &refusal {
                            AdoptRefusal::Unreadable(_) => {
                                "opened stream did not report a readable identity".to_string()
                            }
                            AdoptRefusal::Duplicate(identity, _) => format!(
                                "a stream with identity (wire {}, peer {}, incarnation {}) is \
                                 already open: one (wire id, peer, incarnation) is one open, \
                                 and a second would alias it",
                                identity.wire_id, identity.peer, identity.incarnation
                            ),
                            AdoptRefusal::Exhausted(_) => {
                                "the stream handle space is exhausted".to_string()
                            }
                        };
                        refusal.dispose(|stream| backend.close(stream));
                        reply.fail(ProxyFailure::Typed(LeafError::Session(message)));
                    }
                },
                Err(failure) => reply.fail(failure),
            }
            None
        }
        LeaderRequest::StreamSend { handle, payload } => {
            match owned.with(handle, |stream| backend.send(stream, &payload)) {
                Some(Ok(())) => reply.bytes(bytes::Bytes::new()),
                Some(Err(failure)) => reply.fail(failure),
                // The handle names one open, so this is "your stream
                // is closed", never "some other peer's stream under
                // the same wire id".
                None => reply.fail(ProxyFailure::Typed(LeafError::Session(format!(
                    "no open stream for handle {handle}"
                )))),
            }
            None
        }
        LeaderRequest::StreamClose { handle } => {
            if let Some(stream) = owned.remove(handle) {
                backend.close(stream);
            }
            reply.bytes(bytes::Bytes::new());
            None
        }
        other => Some((other, reply)),
    }
}

/// The streams one backend owns, addressed by handle.
pub struct StreamOwnership<T> {
    streams: RefCell<HashMap<u64, T>>,
    /// The identities the opens above resolve to, so one identity is
    /// one open. See [`StreamOwnership::adopt`].
    identities: RefCell<HashMap<ResolvedIdentity, u64>>,
    next: Cell<u64>,
}

impl<T> Default for StreamOwnership<T> {
    fn default() -> Self {
        Self {
            streams: RefCell::new(HashMap::new()),
            identities: RefCell::new(HashMap::new()),
            next: Cell::new(0),
        }
    }
}

impl<T> StreamOwnership<T> {
    /// Take ownership of one open stream and return its handle.
    ///
    /// The handle is fresh even when another open already has this
    /// stream's wire id, which is the entire point.
    ///
    /// **A checked increment, refusing at the counter's end.** A
    /// saturating one reissued `u64::MAX` forever and the `insert`
    /// below overwrote the live entry under it — the one way to break
    /// "a handle is never reused", which is what stops a retained
    /// caller addressing a later open. The refused stream is handed
    /// back for the caller to close.
    pub fn insert(&self, stream: T) -> Result<u64, T> {
        let Some(handle) = self.next.get().checked_add(1) else {
            return Err(stream);
        };
        self.next.set(handle);
        self.streams.borrow_mut().insert(handle, stream);
        Ok(handle)
    }

    /// Take ownership of an opened stream **and its own identity**.
    ///
    /// This is the whole of the open-time decision, in one place a
    /// backend cannot route around: the handle and the identity on the
    /// reply both come from here, so no backend can answer with the
    /// peer the caller requested instead of the one it got. An
    /// unreadable identity is refused and the stream handed back, for
    /// the caller to close — and so is a **duplicate** one.
    ///
    /// **One identity is one open.** Two opens sharing `(wire_id,
    /// peer, incarnation)` are two handles over ONE node stream: both
    /// consumers receive every payload of the one wire stream, and
    /// closing either one kills the survivor's receive half — the
    /// node's `stream_kinds` and `rx_closed` are keyed on the shared
    /// `(peer, stream_id)`. The second open is refused and its
    /// wrapper disposed **without** the shared close chain
    /// ([`AdoptRefusal::dispose`]); the survivor is untouched,
    /// receive state included. The identity is released when the
    /// handle is, so an open-close-open sequence of the same stream
    /// is two opens, as it should be.
    pub fn adopt(&self, stream: T) -> Result<(u64, ResolvedIdentity), AdoptRefusal<T>>
    where
        T: StreamIdentity,
    {
        let Some(identity) = stream.identity() else {
            return Err(AdoptRefusal::Unreadable(stream));
        };
        if self.identities.borrow().contains_key(&identity) {
            return Err(AdoptRefusal::Duplicate(identity, stream));
        }
        let handle = self.insert(stream).map_err(AdoptRefusal::Exhausted)?;
        self.identities.borrow_mut().insert(identity, handle);
        Ok((handle, identity))
    }

    /// Do something with the stream this handle owns.
    ///
    /// `None` when the handle owns nothing — which is "your stream is
    /// closed", never "some other peer's stream under the same wire
    /// id".
    ///
    /// **The borrow is released before the action runs.** The action
    /// is application code — `answer_stream_request`'s send arm runs
    /// `dispatch_events`, whose JS listeners are documented to call
    /// straight back in — so holding the map borrow across it trapped
    /// "already borrowed" on any re-entry (an `open` or a `close`
    /// arriving inside a `send`) and killed the request mid-flight.
    /// `T` is handle-like, so acting on a clone addresses the same
    /// open and the map is free for the whole of the action.
    pub fn with<R>(&self, handle: u64, action: impl FnOnce(&T) -> R) -> Option<R>
    where
        T: Clone,
    {
        let stream = self.streams.borrow().get(&handle).cloned();
        stream.as_ref().map(action)
    }

    /// Give up the stream this handle owns, if it still owns one.
    pub fn remove(&self, handle: u64) -> Option<T> {
        let stream = self.streams.borrow_mut().remove(&handle)?;
        // By handle value rather than through the stream's identity:
        // the identity map is a veto on ADOPTION, so a value inserted
        // without one (`insert`) has nothing to clear and must not
        // need a bound this method does not otherwise want.
        self.identities
            .borrow_mut()
            .retain(|_, owner| *owner != handle);
        Some(stream)
    }

    /// Take every stream, for a backend that is shutting down.
    pub fn drain(&self) -> Vec<T> {
        self.identities.borrow_mut().clear();
        self.streams.borrow_mut().drain().map(|(_, s)| s).collect()
    }

    /// How many opens are held.
    pub fn len(&self) -> usize {
        self.streams.borrow().len()
    }

    /// Whether any open is held.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::rc::Rc;

    /// The identity every spy stream resolves to — one peer, one
    /// incarnation, wire ids minted per open. Fixed so two opens can
    /// share an identity on purpose (by rewinding `next_wire`).
    const SPY_PEER: u64 = 0xaa;
    const SPY_INCARNATION: u64 = 1;

    /// Two opens sharing a wire id are two opens.
    ///
    /// The values here stand in for streams; what is under test is the
    /// addressing, and the defect was that the addressing key was the
    /// value's wire id. Native, so this executes everywhere — the
    /// browser witnesses in `tests/wasm_leader.rs` drive the same type
    /// through the real `ProxyStream`.
    #[test]
    fn two_streams_with_one_wire_id_get_distinct_handles() {
        let owned: StreamOwnership<(&str, u64)> = StreamOwnership::default();
        // Same wire id (9), different peers.
        let a = owned.insert(("peer-a", 9)).expect("handles remain");
        let b = owned.insert(("peer-b", 9)).expect("handles remain");

        assert_ne!(a, b, "two opens must not share a handle");
        assert_eq!(
            owned.len(),
            2,
            "the second open must not overwrite the first"
        );
        assert_eq!(owned.with(a, |s| s.0), Some("peer-a"));
        assert_eq!(owned.with(b, |s| s.0), Some("peer-b"));
    }

    #[test]
    fn removing_one_leaves_the_other() {
        let owned: StreamOwnership<(&str, u64)> = StreamOwnership::default();
        let a = owned.insert(("peer-a", 9)).expect("handles remain");
        let b = owned.insert(("peer-b", 9)).expect("handles remain");

        assert_eq!(owned.remove(a).map(|s| s.0), Some("peer-a"));

        // A is gone and unusable …
        assert_eq!(owned.with(a, |s| s.0), None);
        assert!(owned.remove(a).is_none(), "a handle is not closable twice");
        // … and B is untouched, under the same wire id.
        assert_eq!(owned.with(b, |s| s.0), Some("peer-b"));
        assert_eq!(owned.len(), 1);
    }

    #[test]
    fn a_handle_is_never_reused() {
        let owned: StreamOwnership<u64> = StreamOwnership::default();
        let first = owned.insert(1).expect("handles remain");
        owned.remove(first);
        let second = owned.insert(2).expect("handles remain");

        // A reused handle would let a retained caller address a later
        // open — the defect this type exists to prevent, one level
        // down from `ProxyStream`'s generation stamp.
        assert_ne!(first, second);
        assert_eq!(owned.with(first, |s| *s), None);
    }

    #[test]
    fn an_unknown_handle_owns_nothing() {
        let owned: StreamOwnership<u64> = StreamOwnership::default();
        let handle = owned.insert(7).expect("handles remain");

        assert_eq!(owned.with(handle.wrapping_add(1), |s| *s), None);
        assert_eq!(owned.with(0, |s| *s), None);
    }

    /// A stream whose identity cannot be read is refused, and handed
    /// back so the caller can close it.
    ///
    /// Unreachable through the sans-IO `StreamHandle`, whose identity
    /// is three `u64` fields and cannot fail — so it is asserted here,
    /// over a type that fails on purpose. The production
    /// `wasm::LeafStream` impl parses the wasm boundary's spellings,
    /// which is where an unreadable identity actually comes from.
    struct Unreadable;

    impl StreamIdentity for Unreadable {
        fn identity(&self) -> Option<ResolvedIdentity> {
            None
        }
    }

    #[test]
    fn adopt_refuses_a_stream_that_cannot_state_its_identity() {
        let owned: StreamOwnership<Unreadable> = StreamOwnership::default();

        assert!(
            owned.adopt(Unreadable).is_err(),
            "an unreadable identity is a refusal"
        );
        // Nothing was taken: the caller still owns the stream and will
        // close it, rather than the backend holding one whose peer it
        // cannot name.
        assert!(owned.is_empty());
    }

    #[test]
    fn adopt_reports_the_identity_the_stream_states() {
        let owned: StreamOwnership<crate::node::StreamHandle> = StreamOwnership::default();
        let handle = crate::node::StreamHandle {
            peer: 0xaa,
            incarnation: 3,
            stream_id: 9,
            channel_hash: 0,
            reliability: crate::stream::Reliability::Reliable,
        };

        let (owner, id) = owned.adopt(handle).expect("a readable identity is adopted");

        assert_eq!(owner, 1);
        assert_eq!(id.peer, 0xaa);
        assert_eq!(id.wire_id, 9);
        assert_eq!(id.incarnation, 3);
    }

    #[test]
    fn resolve_identity_fails_closed_on_anything_unreadable() {
        // The control first: the spellings the boundary really hands
        // over resolve.
        assert_eq!(
            resolve_identity("0000000000000009", "00000000000000aa", "3"),
            Some(ResolvedIdentity {
                wire_id: 9,
                peer: 0xaa,
                incarnation: 3
            }),
        );

        // And every unreadable one is a refusal, not a zero.
        for (id, peer, inc, why) in [
            ("", "aa", "1", "empty id"),
            ("zz", "aa", "1", "non-hex id"),
            ("9", "", "1", "empty peer"),
            ("9", "0xaa", "1", "prefixed peer"),
            ("9", "aa", "", "empty incarnation"),
            ("9", "aa", "three", "non-numeric incarnation"),
            ("9", "aa", "-1", "signed incarnation"),
        ] {
            assert_eq!(resolve_identity(id, peer, inc), None, "{why}");
        }
    }

    /// The shared dispatch's own contract, natively.
    ///
    /// The browser witnesses drive this function through two real
    /// backends, but a `StreamHandle`'s identity cannot fail, so the
    /// refusal paths are unreachable from there. They are reachable
    /// here, over a backend that fails on purpose — which is what
    /// makes "an unresolvable identity is refused **and the stream
    /// given back to be closed**" a witnessed rule rather than a line
    /// of prose.
    struct SpyBackend {
        resolvable: bool,
        closed: Cell<usize>,
        sent: RefCell<Vec<Vec<u8>>>,
        /// The wire id the next open resolves to: identity is
        /// `(wire_id, peer, incarnation)`, so distinct opens need
        /// distinct values here, and rewinding it mints a duplicate
        /// identity on purpose.
        next_wire: Cell<u64>,
        /// A call-back `send` performs, standing in for the
        /// `dispatch_events` listeners that are documented to call
        /// straight back in: `(the table to re-enter, the handle to
        /// close)`.
        reenter: RefCell<Option<(Rc<StreamOwnership<SpyStream>>, u64)>>,
        /// The node-level receive state the production open/close
        /// chain reaches, modeled with the node's own keys:
        /// `LeafNode::rx_closed` is `(incarnation, stream id)` and
        /// `LeafNode::stream_kinds` is `(peer, stream id)` — both
        /// **shared** by every wrapper of one identity, because one
        /// `(wire id, peer, incarnation)` is ONE node stream. `open`
        /// and `close` below perform exactly the writes
        /// `LeafNode::open_stream` / `LeafNode::close_stream` perform,
        /// so an oracle reading these after a refused open sees what
        /// the refusal's disposition really does to the SURVIVOR's
        /// receive half — a spy whose `close` only bumps an
        /// identity-private counter cannot tell "disposed the refused
        /// wrapper" from "closed the shared stream".
        rx_closed: RefCell<HashSet<(u64, u64)>>,
        stream_kinds: RefCell<HashSet<(u64, u64)>>,
    }

    /// A stream that can be made unresolvable, over an identity the
    /// backend mints per open.
    #[derive(Clone)]
    struct SpyStream {
        resolvable: bool,
        wire_id: u64,
    }

    impl StreamIdentity for SpyStream {
        fn identity(&self) -> Option<ResolvedIdentity> {
            self.resolvable.then_some(ResolvedIdentity {
                wire_id: self.wire_id,
                peer: SPY_PEER,
                incarnation: SPY_INCARNATION,
            })
        }
    }

    impl StreamBackend for SpyBackend {
        type Stream = SpyStream;

        fn open(
            &self,
            _label: &str,
            _reliability: crate::stream::Reliability,
            _stream_id: Option<u64>,
            _channel_hash: Option<u16>,
            _peer: Option<u64>,
        ) -> Result<Self::Stream, crate::leader::ProxyFailure> {
            let wire_id = self.next_wire.get();
            self.next_wire.set(wire_id.wrapping_add(1));
            // `LeafNode::open_stream`'s two shared writes: the id is
            // registered as a stream, and reopening un-closes the
            // receive half. Idempotent for a duplicate identity —
            // which is why the OPEN of a duplicate attempt is
            // harmless and only the refusal's disposition can hurt
            // the survivor.
            self.stream_kinds.borrow_mut().insert((SPY_PEER, wire_id));
            self.rx_closed
                .borrow_mut()
                .remove(&(SPY_INCARNATION, wire_id));
            Ok(SpyStream {
                resolvable: self.resolvable,
                wire_id,
            })
        }

        fn send(
            &self,
            _stream: &Self::Stream,
            payload: &[u8],
        ) -> Result<(), crate::leader::ProxyFailure> {
            self.sent.borrow_mut().push(payload.to_vec());
            // The documented re-entry: a `send` runs `dispatch_events`
            // into JS listeners that may call straight back in.
            if let Some((owned, handle)) = self.reenter.borrow_mut().take() {
                let (reply, _answers) = crate::leader::Replier::local_for_test(1);
                let unhandled = answer_stream_request(
                    &owned,
                    self,
                    crate::leader::LeaderRequest::StreamClose { handle },
                    reply,
                );
                assert!(unhandled.is_none(), "the stream arms prefilter");
            }
            Ok(())
        }

        fn close(&self, stream: Self::Stream) {
            self.closed.set(self.closed.get() + 1);
            // `LeafNode::close_stream`'s two shared writes: the
            // receive half is marked closed and the registration
            // given up — keyed on the SHARED identity, so this hits
            // every wrapper of the one node stream, exactly as the
            // production chain does.
            self.rx_closed
                .borrow_mut()
                .insert((SPY_INCARNATION, stream.wire_id));
            self.stream_kinds
                .borrow_mut()
                .remove(&(SPY_PEER, stream.wire_id));
        }
    }

    fn spy(resolvable: bool) -> SpyBackend {
        SpyBackend {
            resolvable,
            closed: Cell::new(0),
            sent: RefCell::new(Vec::new()),
            next_wire: Cell::new(9),
            reenter: RefCell::new(None),
            rx_closed: RefCell::new(HashSet::new()),
            stream_kinds: RefCell::new(HashSet::new()),
        }
    }

    fn answer(
        owned: &StreamOwnership<SpyStream>,
        backend: &SpyBackend,
        request: crate::leader::LeaderRequest,
    ) -> crate::leader::ProxyOutcome {
        let (reply, mut rx) = crate::leader::Replier::local_for_test(1);
        assert!(answer_stream_request(owned, backend, request, reply).is_none());
        rx.try_recv()
            .expect("the replier answered")
            .expect("exactly one answer")
    }

    fn open_request(peer: Option<u64>) -> crate::leader::LeaderRequest {
        crate::leader::LeaderRequest::StreamOpen {
            label: "l".into(),
            reliability: crate::stream::Reliability::Reliable,
            stream_id: Some(9),
            channel_hash: None,
            peer,
        }
    }

    #[test]
    fn the_open_reply_carries_the_resolved_identity_not_the_requested_peer() {
        let owned = StreamOwnership::default();
        let backend = spy(true);

        // The request names a DIFFERENT peer from the one the stream
        // resolves to, which is the case that separates "resolved"
        // from "echoed". An unnamed-peer open is the same case with
        // `None`. Each open mints its own wire id — one identity is
        // one open now — so the reply's id tracks the mint while the
        // RESOLVED peer and incarnation stay the stream's own.
        for (opened, requested) in [None, Some(0xbbbb)].into_iter().enumerate() {
            let outcome = answer(&owned, &backend, open_request(requested));
            match outcome.expect("opened") {
                crate::leader::ProxyValue::Stream {
                    peer,
                    stream_id,
                    incarnation,
                    ..
                } => {
                    assert_eq!(peer, 0xaa, "the reply must carry the resolved peer");
                    assert_eq!(stream_id, 9 + opened as u64, "and the id it resolved to");
                    assert_eq!(incarnation, 1);
                }
                other => panic!("expected a stream answer, got {other:?}"),
            }
        }
    }

    #[test]
    fn an_unresolvable_open_is_refused_and_the_stream_is_closed() {
        let owned = StreamOwnership::default();
        let backend = spy(false);

        let outcome = answer(&owned, &backend, open_request(Some(0xaa)));

        assert!(outcome.is_err(), "an unresolvable identity must be refused");
        assert_eq!(
            backend.closed.get(),
            1,
            "the stream must be given back and closed, not leaked open"
        );
        assert!(owned.is_empty(), "nothing is owned after a refusal");
    }

    #[test]
    fn a_send_reaches_the_stream_its_handle_owns() {
        let owned = StreamOwnership::default();
        let backend = spy(true);
        let first = match answer(&owned, &backend, open_request(None)).expect("open") {
            crate::leader::ProxyValue::Stream { handle, .. } => handle,
            other => panic!("{other:?}"),
        };
        let second = match answer(&owned, &backend, open_request(None)).expect("open") {
            crate::leader::ProxyValue::Stream { handle, .. } => handle,
            other => panic!("{other:?}"),
        };
        assert_ne!(first, second);

        assert!(answer(
            &owned,
            &backend,
            crate::leader::LeaderRequest::StreamSend {
                handle: second,
                payload: bytes::Bytes::from_static(b"two"),
            }
        )
        .is_ok());
        assert_eq!(backend.sent.borrow().len(), 1);

        // Closing one leaves the other, and the closed handle is then
        // refused rather than reaching the survivor.
        assert!(answer(
            &owned,
            &backend,
            crate::leader::LeaderRequest::StreamClose { handle: first }
        )
        .is_ok());
        assert_eq!(backend.closed.get(), 1);
        let refused = answer(
            &owned,
            &backend,
            crate::leader::LeaderRequest::StreamSend {
                handle: first,
                payload: bytes::Bytes::from_static(b"after"),
            },
        );
        assert!(refused.is_err(), "a closed handle must be refused");
        assert_eq!(
            backend.sent.borrow().len(),
            1,
            "a closed handle's send must not reach the surviving stream"
        );
    }

    #[test]
    fn an_unhandled_request_is_returned_with_a_live_replier() {
        let owned: StreamOwnership<SpyStream> = StreamOwnership::default();
        let backend = spy(true);
        let (reply, mut rx) = crate::leader::Replier::local_for_test(1);

        let returned = answer_stream_request(
            &owned,
            &backend,
            crate::leader::LeaderRequest::Counters,
            reply,
        );

        let Some((request, reply)) = returned else {
            panic!("a caller's other arms must still see the request");
        };
        assert!(matches!(request, crate::leader::LeaderRequest::Counters));
        // Nothing has answered yet. Returning the enum while dropping
        // the replier would have already sent `SessionLost`, so the
        // caller's own answer would be the second one and its consumer
        // would have seen a failure it never suffered.
        assert!(
            rx.try_recv().expect("the channel is open").is_none(),
            "an unhandled request must not already be answered"
        );

        // The handed-back token is what the caller completes with, and
        // its answer is the only one.
        reply.text("[]".into());
        let outcome = rx
            .try_recv()
            .expect("the channel is open")
            .expect("the caller's answer arrived");
        assert!(matches!(outcome, Ok(crate::leader::ProxyValue::Text(text)) if text == "[]"));
        assert!(
            rx.try_recv().is_err() || rx.try_recv().map(|o| o.is_none()).unwrap_or(true),
            "exactly one answer"
        );
    }

    #[test]
    fn draining_gives_up_everything() {
        let owned: StreamOwnership<u64> = StreamOwnership::default();
        owned.insert(1).expect("handles remain");
        owned.insert(2).expect("handles remain");

        let mut taken = owned.drain();
        taken.sort_unstable();

        assert_eq!(taken, vec![1, 2]);
        assert!(owned.is_empty(), "a shut-down backend holds nothing");
    }

    /// One identity is one open: a second open of a live `(wire_id,
    /// peer, incarnation)` is refused — and the survivor's RECEIVE
    /// state is exactly as before the refused open.
    ///
    /// Two opens sharing all three were two handles over ONE node
    /// stream — both consumers received every payload of the one wire
    /// stream, and closing either one removed the shared
    /// `(peer, stream_id)` state out from under the survivor's
    /// receive half. The veto prevents the alias; what this oracle
    /// additionally watches is what the REFUSAL's disposition does to
    /// the survivor — the defect the repair-pass review found in the
    /// repair itself: the refused wrapper used to go to
    /// `backend.close`, whose chain (`LeafStream::close` →
    /// `LeafNode::close_stream`) inserts `rx_closed` and removes
    /// `stream_kinds` under the SHARED identity, destroying the
    /// survivor's node-level receive half on a mere duplicate attempt
    /// that returned `Err`.
    ///
    /// So `SpyBackend`'s `open`/`close` perform exactly the shared
    /// writes the production pair performs, and the assertions below
    /// read that shared state directly. The shape this test had —
    /// a `SpyStream::close` counter and an ownership-map check —
    /// cannot distinguish "disposed the refused wrapper" from "closed
    /// the shared stream": either way the counter moves and the map
    /// is untouched.
    ///
    /// One disposition inversion, forced by the repair the reviewer
    /// specified. The repair-pass review requires "a refused open
    /// leaves the survivor's node-level receive state exactly as it
    /// was — dispose of the refused wrapper without the shared close
    /// path", so this test's `closed` count changed from `1` ("the
    /// refused stream is given back and closed, not leaked open") to
    /// `0`: the old assertion pinned the very disposition the repair
    /// removes. The refusal assertions and the survivor assertions
    /// are unchanged, the receive-state oracle is what the review
    /// asked for, and the survivor's own close at the end moves the
    /// shared state — so the oracle discriminates exactly what its
    /// name and its failure messages name.
    #[test]
    fn a_second_open_of_one_identity_is_refused_and_the_survivor_is_untouched() {
        let owned = StreamOwnership::default();
        let backend = spy(true);
        let first = match answer(&owned, &backend, open_request(None)).expect("the first open") {
            crate::leader::ProxyValue::Stream { handle, .. } => handle,
            other => panic!("{other:?}"),
        };

        // The survivor's node-level receive state BEFORE the refused
        // open: registered and not closed.
        assert!(backend.stream_kinds.borrow().contains(&(SPY_PEER, 9)));
        assert!(!backend.rx_closed.borrow().contains(&(SPY_INCARNATION, 9)));

        // The same identity again: the same wire stream, opened twice.
        backend.next_wire.set(9);
        answer(&owned, &backend, open_request(Some(0xbbbb)))
            .expect_err("a duplicate identity must be refused");
        assert!(
            !backend.rx_closed.borrow().contains(&(SPY_INCARNATION, 9)),
            "the survivor's receive half is exactly as before the refused open: not closed"
        );
        assert!(
            backend.stream_kinds.borrow().contains(&(SPY_PEER, 9)),
            "and its stream registration is still claimed"
        );
        assert_eq!(
            backend.closed.get(),
            0,
            "the refused wrapper is disposed WITHOUT the shared close: `close` runs the \
             production close chain (`LeafStream::close` → `LeafNode::close_stream`), whose \
             `rx_closed`/`stream_kinds` writes are keyed on the SHARED identity and would take \
             the SURVIVOR's receive half with it"
        );
        assert_eq!(
            owned.with(first, |s| s.wire_id),
            Some(9),
            "the survivor is untouched"
        );
        assert_eq!(owned.len(), 1, "the duplicate took no slot");

        // The identity is released with its handle: after the survivor        // closes, the same identity is openable again.
        assert!(answer(
            &owned,
            &backend,
            crate::leader::LeaderRequest::StreamClose { handle: first }
        )
        .is_ok());
        // The positive control the oracle needs: the survivor's own
        // close DOES run the shared close chain, so the silence above
        // is the refusal's disposition and not a dead model.
        assert_eq!(backend.closed.get(), 1, "the survivor's close ran");
        assert!(
            backend.rx_closed.borrow().contains(&(SPY_INCARNATION, 9)),
            "the survivor's own close gives the shared receive state back"
        );
        assert!(!backend.stream_kinds.borrow().contains(&(SPY_PEER, 9)));
        backend.next_wire.set(9);
        answer(&owned, &backend, open_request(None))
            .expect("a closed identity may be opened again");
    }

    /// The refused wrapper's disposal is refusal-specific, and the
    /// alias case is the one that must not close.
    ///
    /// The rule under test is [`AdoptRefusal::dispose`]'s own: a
    /// fresh open nothing else references is given back to `close`,
    /// while a duplicate's wrapper — an alias of the survivor's one
    /// node stream — is DISCARDED, because the shared close chain
    /// would take the survivor's receive half with it. The two
    /// fresh-open arms are the positive control: the rule is "never
    /// close the alias", not "never close a refusal".
    #[test]
    fn a_duplicate_refusal_disposes_the_wrapper_without_closing_the_shared_stream() {
        let identity = ResolvedIdentity {
            wire_id: 9,
            peer: SPY_PEER,
            incarnation: SPY_INCARNATION,
        };
        let mut closed = 0u64;

        AdoptRefusal::Duplicate(identity, 7).dispose(|_| closed += 1);
        assert_eq!(
            closed, 0,
            "a duplicate wrapper must be discarded, never closed: close runs the shared \
             close chain over the SURVIVOR's identity"
        );

        AdoptRefusal::Unreadable(7).dispose(|_| closed += 1);
        AdoptRefusal::Exhausted(7).dispose(|_| closed += 1);
        assert_eq!(
            closed, 2,
            "a fresh open nothing else references is still given back to close"
        );
    }

    /// A send whose dispatch calls straight back in may close another
    /// open mid-flight.
    ///
    /// The re-entry `LeafStream::on_message` documents ("the callback
    /// runs with no borrow of the node held, so it may call straight
    /// back in") reaches this table too, and `with` used to hold the
    /// map borrow across the action: a listener closing another open
    /// trapped `already borrowed` and killed the request mid-flight.
    #[test]
    fn a_send_that_is_called_back_into_can_close_another_open() {
        let owned: Rc<StreamOwnership<SpyStream>> = Rc::new(StreamOwnership::default());
        let backend = spy(true);
        let first = match answer(&owned, &backend, open_request(None)).expect("open") {
            crate::leader::ProxyValue::Stream { handle, .. } => handle,
            other => panic!("{other:?}"),
        };
        let second = match answer(&owned, &backend, open_request(None)).expect("open") {
            crate::leader::ProxyValue::Stream { handle, .. } => handle,
            other => panic!("{other:?}"),
        };
        backend
            .reenter
            .borrow_mut()
            .replace((Rc::clone(&owned), first));

        assert!(
            answer(
                &owned,
                &backend,
                crate::leader::LeaderRequest::StreamSend {
                    handle: second,
                    payload: bytes::Bytes::from_static(b"two"),
                }
            )
            .is_ok(),
            "the re-entrant close must not kill the send that called it"
        );
        assert_eq!(backend.closed.get(), 1, "the callback's close ran");
        assert_eq!(
            owned.with(first, |s| s.wire_id),
            None,
            "and the closed open is gone"
        );
        assert!(
            owned.with(second, |s| s.wire_id).is_some(),
            "the sending open is untouched"
        );
        assert_eq!(backend.sent.borrow().len(), 1);
    }

    /// The counter's end refuses rather than reissuing.
    ///
    /// A `saturating_add(1)` reissued `u64::MAX` forever and the map
    /// insert overwrote the live entry under it — the one way to break
    /// "a handle is never reused", which is what stops a retained
    /// caller addressing a later open.
    #[test]
    fn the_terminal_handle_is_refused_rather_than_reissued() {
        let owned: StreamOwnership<u64> = StreamOwnership::default();
        owned.next.set(u64::MAX);

        assert!(
            owned.insert(1).is_err(),
            "the counter's end must refuse, not wrap"
        );
        assert!(
            owned.is_empty(),
            "a refusal must not overwrite a live entry"
        );
    }
}
