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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

/// Answer one stream request, for either backend.
///
/// Every decision is here: the handle an operation addresses, the
/// identity the open reply carries, and the refusal a closed handle
/// gets. `None` is returned for a request this does not handle, so a
/// caller's `match` keeps its other arms.
pub fn answer_stream_request<B: StreamBackend>(
    owned: &StreamOwnership<B::Stream>,
    backend: &B,
    request: crate::leader::LeaderRequest,
    reply: crate::leader::Replier,
) -> Option<crate::leader::LeaderRequest> {
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
                    // opened. The requested `peer` is not in scope
                    // here, so it cannot be answered with.
                    Ok((handle, id)) => {
                        reply.stream(handle, id.wire_id, id.peer, id.incarnation);
                    }
                    Err(stream) => {
                        // Fail closed: a stream whose identity cannot
                        // be read is given back to be closed, not
                        // handed over with an unknown peer.
                        backend.close(stream);
                        reply.fail(ProxyFailure::Typed(LeafError::Session(
                            "opened stream did not report a readable identity".into(),
                        )));
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
        other => Some(other),
    }
}

/// The streams one backend owns, addressed by handle.
pub struct StreamOwnership<T> {
    streams: RefCell<HashMap<u64, T>>,
    next: Cell<u64>,
}

impl<T> Default for StreamOwnership<T> {
    fn default() -> Self {
        Self {
            streams: RefCell::new(HashMap::new()),
            next: Cell::new(0),
        }
    }
}

impl<T> StreamOwnership<T> {
    /// Take ownership of one open stream and return its handle.
    ///
    /// The handle is fresh even when another open already has this
    /// stream's wire id, which is the entire point.
    pub fn insert(&self, stream: T) -> u64 {
        let handle = self.next.get().saturating_add(1);
        self.next.set(handle);
        self.streams.borrow_mut().insert(handle, stream);
        handle
    }

    /// Take ownership of an opened stream **and its own identity**.
    ///
    /// This is the whole of the open-time decision, in one place a
    /// backend cannot route around: the handle and the identity on the
    /// reply both come from here, so no backend can answer with the
    /// peer the caller requested instead of the one it got. An
    /// unreadable identity is refused and the stream handed back, for
    /// the caller to close.
    pub fn adopt(&self, stream: T) -> Result<(u64, ResolvedIdentity), T>
    where
        T: StreamIdentity,
    {
        match stream.identity() {
            Some(identity) => Ok((self.insert(stream), identity)),
            None => Err(stream),
        }
    }

    /// Do something with the stream this handle owns.
    ///
    /// `None` when the handle owns nothing — which is "your stream is
    /// closed", never "some other peer's stream under the same wire
    /// id".
    pub fn with<R>(&self, handle: u64, action: impl FnOnce(&T) -> R) -> Option<R> {
        let streams = self.streams.borrow();
        streams.get(&handle).map(action)
    }

    /// Give up the stream this handle owns, if it still owns one.
    pub fn remove(&self, handle: u64) -> Option<T> {
        self.streams.borrow_mut().remove(&handle)
    }

    /// Take every stream, for a backend that is shutting down.
    pub fn drain(&self) -> Vec<T> {
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
        let a = owned.insert(("peer-a", 9));
        let b = owned.insert(("peer-b", 9));

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
        let a = owned.insert(("peer-a", 9));
        let b = owned.insert(("peer-b", 9));

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
        let first = owned.insert(1);
        owned.remove(first);
        let second = owned.insert(2);

        // A reused handle would let a retained caller address a later
        // open — the defect this type exists to prevent, one level
        // down from `ProxyStream`'s generation stamp.
        assert_ne!(first, second);
        assert_eq!(owned.with(first, |s| *s), None);
    }

    #[test]
    fn an_unknown_handle_owns_nothing() {
        let owned: StreamOwnership<u64> = StreamOwnership::default();
        let handle = owned.insert(7);

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
    }

    /// A stream that can be made unresolvable.
    struct SpyStream {
        resolvable: bool,
    }

    impl StreamIdentity for SpyStream {
        fn identity(&self) -> Option<ResolvedIdentity> {
            self.resolvable.then_some(ResolvedIdentity {
                wire_id: 9,
                peer: 0xaa,
                incarnation: 1,
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
            Ok(SpyStream {
                resolvable: self.resolvable,
            })
        }

        fn send(
            &self,
            _stream: &Self::Stream,
            payload: &[u8],
        ) -> Result<(), crate::leader::ProxyFailure> {
            self.sent.borrow_mut().push(payload.to_vec());
            Ok(())
        }

        fn close(&self, _stream: Self::Stream) {
            self.closed.set(self.closed.get() + 1);
        }
    }

    fn spy(resolvable: bool) -> SpyBackend {
        SpyBackend {
            resolvable,
            closed: Cell::new(0),
            sent: RefCell::new(Vec::new()),
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
        // `None`.
        for requested in [None, Some(0xbbbb)] {
            let outcome = answer(&owned, &backend, open_request(requested));
            match outcome.expect("opened") {
                crate::leader::ProxyValue::Stream {
                    peer,
                    stream_id,
                    incarnation,
                    ..
                } => {
                    assert_eq!(peer, 0xaa, "the reply must carry the resolved peer");
                    assert_eq!(stream_id, 9);
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
    fn a_request_this_dispatch_does_not_handle_is_returned() {
        let owned: StreamOwnership<SpyStream> = StreamOwnership::default();
        let backend = spy(true);
        let (reply, _rx) = crate::leader::Replier::local_for_test(1);

        let returned = answer_stream_request(
            &owned,
            &backend,
            crate::leader::LeaderRequest::Counters,
            reply,
        );

        assert!(
            returned.is_some(),
            "a caller's other arms must still see it"
        );
    }

    #[test]
    fn draining_gives_up_everything() {
        let owned: StreamOwnership<u64> = StreamOwnership::default();
        owned.insert(1);
        owned.insert(2);

        let mut taken = owned.drain();
        taken.sort_unstable();

        assert_eq!(taken, vec![1, 2]);
        assert!(owned.is_empty(), "a shut-down backend holds nothing");
    }
}
