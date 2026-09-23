//! The **genuinely anchorless** [`ControlPlane`]: an in-memory
//! carrier with no anchor behind it, no network behind it, and no
//! key material of its own (Stage 5 slice 6).
//!
//! The brief's standard is the reason this file is short: *"a mock
//! that quietly relays Net packets is a re-implementation of the
//! anchor and does not count — the report says what the mock
//! carries, message by message."* So:
//!
//! 1. **It carries four things and refuses everything else**: signed
//!    announcements, signed `0x0D02` envelopes (which is how SDP and
//!    ICE candidates travel), and the query reply that hands stored
//!    announcements back. There is no path through this type by
//!    which a Net packet can reach another node — and the attempt is
//!    not merely absent, it is [refused and recorded](Carried).
//! 2. **Every input is the test's.** Identities, Noise statics, the
//!    trust-domain PSK and admission are provisioned by the test
//!    directly into the leaves. This type holds no key at all: it
//!    cannot verify an envelope, cannot read an announcement, and
//!    cannot mint a peer's static key. That is the whole point of
//!    the self-authenticating envelope — the carrier is not trusted,
//!    so it does not need to be trustworthy.
//! 3. **Every message is accounted.** [`MockMesh::accounting_table`]
//!    prints kind, direction and byte length for every leg that
//!    crossed, and [`MockMesh::carried_kinds`] is what a test
//!    asserts against. A leaked Net packet appears in that ledger as
//!    [`CarriedKind::RefusedNetPacket`], so it fails the assertion
//!    even if a future caller swallowed the error.
//!
//! # What the trait's shape looks like from here
//!
//! Two methods are **unimplementable without an anchor**, and both
//! refuse rather than pretend:
//!
//! - [`ControlPlane::offer`] — an offer carried on a leaf's behalf is
//!   authenticated by whatever fronts the mesh (for
//!   `AnchorControlPlane`, the bootstrap credential). Nothing fronts
//!   this mesh, and a carrier holds no key to sign with, so an
//!   unsigned offer would arrive unauthenticated. The peer path is
//!   [`ControlPlane::signal`], whose envelopes the *leaf* signs.
//! - [`ControlPlane::trickle`] — the carrier-authenticated candidate
//!   form, admissible inside the anchor's credentialed bootstrap
//!   dialog. Here a candidate is a signed `Candidate` envelope like
//!   every other signalling message.
//!
//! That asymmetry is the finding, not a defect: `ControlEvent::Candidate`
//! is the carrier-authenticated shape and `ControlEvent::Signal` the
//! self-authenticating one, and a leaf that can drive the second
//! needs no carrier it trusts.
//!
//! # Not a shipped code path
//!
//! Behind the `mock-control-plane` feature, off by default, so a
//! mock cannot become production by accident. It is also **not**
//! `wasm32`-only: it is plain memory, it is tested natively, and the
//! wasm runner uses the same code in a browser.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap, VecDeque};
use std::rc::Rc;

use crate::control_plane::{
    BootstrapAccepted, ControlEvent, ControlPlane, DialogId, IceCandidate, NodeId, Sdp,
    SignalEnvelope, SignalKind, SignedAnnouncement,
};
use crate::error::{LeafError, Result};
use crate::signal;

/// What one carried message was.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CarriedKind {
    /// A signed capability announcement, stored or delivered
    /// verbatim.
    Announcement,
    /// A stored announcement handed back to a `query_capability`.
    QueryReply,
    /// A signed `0x0D02` envelope: an offer, an answer, one
    /// candidate, or a reject.
    Signal(SignalKind),
    /// An organization root's signed revocation-floor bundle, carried
    /// verbatim to one leaf. Signed control material like an
    /// announcement — the carrier cannot forge one and the leaf
    /// verifies it — which is why it is signalling and the tripwire
    /// below still applies to its bytes.
    Revocation,
    /// **A tripwire hit.** Something that parses as a Net packet was
    /// handed to the mock to carry. It was refused, and it is in the
    /// ledger so the refusal cannot be swallowed silently.
    RefusedNetPacket,
}

impl CarriedKind {
    /// The name the accounting table prints.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Announcement => "announcement",
            Self::QueryReply => "query-reply",
            Self::Signal(SignalKind::Offer) => "signal:offer",
            Self::Signal(SignalKind::Answer) => "signal:answer",
            Self::Signal(SignalKind::Candidate) => "signal:candidate",
            Self::Signal(SignalKind::Reject) => "signal:reject",
            Self::Revocation => "revocation",
            Self::RefusedNetPacket => "REFUSED-NET-PACKET",
        }
    }

    /// Whether this kind is signalling — i.e. one of the things a
    /// control plane is allowed to carry: the four `0x0D02` kinds,
    /// announcements and their query replies, and the org-root-signed
    /// revocation facts feed.
    ///
    /// The assertion a test makes over
    /// [`MockMesh::carried_kinds`]. [`Self::RefusedNetPacket`] is
    /// deliberately not signalling.
    pub const fn is_signalling(self) -> bool {
        matches!(
            self,
            Self::Announcement | Self::QueryReply | Self::Signal(_) | Self::Revocation
        )
    }
}

/// One leg that crossed the mock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Carried {
    /// 1-based position in the ledger.
    pub seq: usize,
    /// What it was.
    pub kind: CarriedKind,
    /// Who handed it over. `None` is the mock's own store.
    pub from: Option<NodeId>,
    /// Who received it. `None` is the mock's own store.
    pub to: Option<NodeId>,
    /// Length in bytes of exactly what crossed: the announcement's
    /// bytes, or the envelope's encoded form.
    pub bytes: usize,
}

/// The shared switchboard.
struct MeshState {
    /// Admitted nodes and the short label the test gave each.
    /// Admission is provisioned, never inferred: an unadmitted node
    /// is refused, which is what makes "every input is the test's"
    /// checkable.
    admitted: RefCell<HashMap<NodeId, String>>,
    /// Per-node event queues, drained by each leaf's own loop.
    inboxes: RefCell<HashMap<NodeId, VecDeque<ControlEvent>>>,
    /// The announcement store: one per publisher, newest write wins.
    /// Keyed by node id — the only field of an announcement the mock
    /// is told, because the publisher hands it over separately. The
    /// bytes themselves are never parsed here.
    store: RefCell<Vec<(NodeId, SignedAnnouncement)>>,
    /// Per-node revocation-facts queues, drained by
    /// `take_revocation_bundles`. Verbatim bytes: the carrier never
    /// reads inside a bundle, exactly as it never reads inside an
    /// announcement.
    revocations: RefCell<HashMap<NodeId, VecDeque<Vec<u8>>>>,
    /// The ledger.
    log: RefCell<Vec<Carried>>,
}

/// An in-memory mesh of leaves that share nothing but this carrier.
#[derive(Clone)]
pub struct MockMesh {
    state: Rc<MeshState>,
}

impl Default for MockMesh {
    fn default() -> Self {
        Self::new()
    }
}

impl MockMesh {
    /// An empty mesh: no members, no announcements, no keys.
    pub fn new() -> Self {
        Self {
            state: Rc::new(MeshState {
                admitted: RefCell::new(HashMap::new()),
                inboxes: RefCell::new(HashMap::new()),
                store: RefCell::new(Vec::new()),
                revocations: RefCell::new(HashMap::new()),
                log: RefCell::new(Vec::new()),
            }),
        }
    }

    /// Admit `node` under `label`, and hand back its control plane.
    ///
    /// `label` is what the accounting table prints instead of a
    /// 16-digit hex id. Admission carries no key: what a leaf is
    /// allowed to do is decided here, what a leaf *is* is decided by
    /// its own identity and proven by the signatures on what it
    /// sends.
    pub fn admit(&self, node: NodeId, label: &str) -> MockControlPlane {
        self.state
            .admitted
            .borrow_mut()
            .insert(node, label.to_string());
        self.state.inboxes.borrow_mut().entry(node).or_default();
        MockControlPlane {
            node,
            state: Rc::clone(&self.state),
        }
    }

    /// The ledger, in order.
    pub fn carried(&self) -> Vec<Carried> {
        self.state.log.borrow().clone()
    }

    /// Carry one org-root-signed revocation bundle to `to`, verbatim,
    /// exactly as the anchor's control socket would.
    ///
    /// The carrier is the org operator's feed endpoint here: the
    /// bundle arrives already signed by the organization root and the
    /// leaf verifies it — the mock carries and accounts, and can
    /// therefore no more forge a floor than it can forge an
    /// announcement. A Net-packet-shaped bundle hits the same tripwire
    /// every other carry does: the feed's payload is control material
    /// or it is refused, and the refusal is on the ledger.
    pub fn deliver_revocation(&self, to: NodeId, bundle: Vec<u8>) -> Result<()> {
        if !self.state.admitted.borrow().contains_key(&to) {
            return Err(refused(&format!(
                "{to:#018x} was never admitted to this mesh — every member is \
                 provisioned explicitly by the test"
            )));
        }
        if looks_like_a_net_packet(&bundle) {
            let mut log = self.state.log.borrow_mut();
            let seq = log.len() + 1;
            log.push(Carried {
                seq,
                kind: CarriedKind::RefusedNetPacket,
                from: None,
                to: Some(to),
                bytes: bundle.len(),
            });
            return Err(refused(
                "that is a Net packet. A control plane carries signalling — SDP, \
                 candidates, signed announcements, signed envelopes, signed \
                 revocation bundles — and a control plane that forwarded packets \
                 would be a relay wearing a trait",
            ));
        }
        {
            let mut log = self.state.log.borrow_mut();
            let seq = log.len() + 1;
            log.push(Carried {
                seq,
                kind: CarriedKind::Revocation,
                from: None,
                to: Some(to),
                bytes: bundle.len(),
            });
        }
        self.state
            .revocations
            .borrow_mut()
            .entry(to)
            .or_default()
            .push_back(bundle);
        Ok(())
    }

    /// The distinct kinds that crossed.
    ///
    /// The set a test asserts on: it must contain signalling kinds
    /// and nothing else.
    pub fn carried_kinds(&self) -> BTreeSet<CarriedKind> {
        self.state.log.borrow().iter().map(|c| c.kind).collect()
    }

    /// Total bytes carried, across every leg.
    pub fn carried_bytes(&self) -> usize {
        self.state.log.borrow().iter().map(|c| c.bytes).sum()
    }

    /// The message-by-message accounting table the brief asks for.
    pub fn accounting_table(&self) -> String {
        let log = self.state.log.borrow();
        let mut out = String::new();
        out.push_str("  #  kind                direction      bytes\n");
        out.push_str("  -- ------------------- -------------- -----\n");
        for carried in log.iter() {
            out.push_str(&format!(
                "  {:>2}  {:<19} {:<14} {:>5}\n",
                carried.seq,
                carried.kind.label(),
                format!(
                    "{} -> {}",
                    self.label_of(carried.from),
                    self.label_of(carried.to)
                ),
                carried.bytes,
            ));
        }
        out.push_str(&format!(
            "  {} message(s), {} byte(s) total\n",
            log.len(),
            log.iter().map(|c| c.bytes).sum::<usize>()
        ));
        out
    }

    fn label_of(&self, node: Option<NodeId>) -> String {
        match node {
            None => "mock".to_string(),
            Some(node) => self
                .state
                .admitted
                .borrow()
                .get(&node)
                .cloned()
                .unwrap_or_else(|| format!("{node:#018x}")),
        }
    }
}

/// One leaf's view of the mesh.
pub struct MockControlPlane {
    node: NodeId,
    state: Rc<MeshState>,
}

impl MockControlPlane {
    /// The node this control plane belongs to.
    #[inline]
    pub fn node(&self) -> NodeId {
        self.node
    }

    fn record(&self, kind: CarriedKind, from: Option<NodeId>, to: Option<NodeId>, bytes: usize) {
        let mut log = self.state.log.borrow_mut();
        let seq = log.len() + 1;
        log.push(Carried {
            seq,
            kind,
            from,
            to,
            bytes,
        });
    }

    fn require_admitted(&self, node: NodeId) -> Result<()> {
        if self.state.admitted.borrow().contains_key(&node) {
            return Ok(());
        }
        Err(refused(&format!(
            "{node:#018x} was never admitted to this mesh — every member is \
             provisioned explicitly by the test"
        )))
    }

    /// The tripwire.
    ///
    /// Anything that parses as a Net packet header is refused AND
    /// recorded, so a caller that ignored the error still reddens
    /// the ledger assertion.
    fn refuse_net_packets(&self, to: Option<NodeId>, bytes: &[u8]) -> Result<()> {
        if !looks_like_a_net_packet(bytes) {
            return Ok(());
        }
        self.record(
            CarriedKind::RefusedNetPacket,
            Some(self.node),
            to,
            bytes.len(),
        );
        Err(refused(
            "that is a Net packet. A control plane carries signalling — SDP, \
             candidates, signed announcements, signed envelopes — and a control \
             plane that forwarded packets would be a relay wearing a trait",
        ))
    }

    fn deliver(&self, to: NodeId, event: ControlEvent) {
        self.state
            .inboxes
            .borrow_mut()
            .entry(to)
            .or_default()
            .push_back(event);
    }
}

impl ControlPlane for MockControlPlane {
    async fn offer(&self, _offer: Sdp) -> Result<BootstrapAccepted> {
        // Not a gap: a refusal that names a real property. Nothing
        // fronts this mesh, so there is nobody to offer *to*; and a
        // carrier cannot mint an attempt on a leaf's behalf because
        // it holds no key to sign one with. A peer attempt is a
        // signed envelope through `signal`, and the peer's Noise
        // static comes from the announcement the leaf verified
        // itself (§5 Layer 1), never from the carrier.
        Err(refused(
            "this control plane fronts no node: a peer attempt travels as a signed \
             SignalEnvelope through `signal`, because nothing here could \
             authenticate an offer it did not sign",
        ))
    }

    async fn trickle(&self, _dialog: DialogId, _candidate: IceCandidate) -> Result<()> {
        Err(refused(
            "an unsigned candidate is only admissible inside a bootstrap dialog the \
             carrier authenticated; here a candidate is a signed Candidate envelope \
             through `signal`",
        ))
    }

    async fn end_attempt(&self, _dialog: DialogId) -> Result<()> {
        // Idempotent and local. The mock holds no dialog table:
        // there is nothing to forget and nothing to tell anybody.
        // A leaf that wants the peer to know sends a signed Reject.
        Ok(())
    }

    async fn publish_announcement(&self, announcement: SignedAnnouncement) -> Result<()> {
        self.require_admitted(self.node)?;
        self.refuse_net_packets(None, &announcement.0)?;
        let bytes = announcement.0.len();
        self.record(CarriedKind::Announcement, Some(self.node), None, bytes);

        {
            let mut store = self.state.store.borrow_mut();
            store.retain(|(node, _)| *node != self.node);
            store.push((self.node, announcement.clone()));
        }

        // Fan out to the members that are already here. Verbatim,
        // unread, and verified by each receiver — which is exactly
        // what makes a dumb store sufficient in the serverless
        // follow-on.
        let peers: Vec<NodeId> = self
            .state
            .admitted
            .borrow()
            .keys()
            .copied()
            .filter(|peer| *peer != self.node)
            .collect();
        for peer in peers {
            self.record(CarriedKind::Announcement, None, Some(peer), bytes);
            self.deliver(peer, ControlEvent::Announcement(announcement.clone()));
        }
        Ok(())
    }

    async fn query_capability(&self, _capability: &str) -> Result<Vec<SignedAnnouncement>> {
        self.require_admitted(self.node)?;
        // **Index-free on purpose.** Filtering by capability would
        // mean parsing the document this type is supposed to carry
        // blind; the leaf verifies each announcement and answers
        // `query` from what it verified. So the reply is everything
        // held except the querier's own.
        let held: Vec<SignedAnnouncement> = self
            .state
            .store
            .borrow()
            .iter()
            .filter(|(node, _)| *node != self.node)
            .map(|(_, announcement)| announcement.clone())
            .collect();
        for announcement in &held {
            self.record(
                CarriedKind::QueryReply,
                None,
                Some(self.node),
                announcement.0.len(),
            );
        }
        Ok(held)
    }

    async fn signal(&self, envelope: SignalEnvelope) -> Result<()> {
        self.require_admitted(self.node)?;
        if envelope.from != self.node {
            // A link-level check, not a content one: this
            // connection belongs to one node. The signature is what
            // actually binds `from`, and the receiver checks it.
            return Err(refused(&format!(
                "this control plane belongs to {:#018x} and cannot send an \
                 envelope claiming to be from {:#018x}",
                self.node, envelope.from
            )));
        }
        self.require_admitted(envelope.to)?;
        self.refuse_net_packets(Some(envelope.to), &envelope.payload)?;

        // The encoded length is what crossed; the mock never reads
        // inside it.
        let bytes = signal::encode(&envelope)?.len();
        self.record(
            CarriedKind::Signal(envelope.kind),
            Some(envelope.from),
            Some(envelope.to),
            bytes,
        );
        let to = envelope.to;
        self.deliver(to, ControlEvent::Signal(envelope));
        Ok(())
    }

    fn drain_events(&self) -> Vec<ControlEvent> {
        self.state
            .inboxes
            .borrow_mut()
            .get_mut(&self.node)
            .map(|queue| queue.drain(..).collect())
            .unwrap_or_default()
    }

    fn take_revocation_bundles(&mut self) -> Vec<Vec<u8>> {
        self.state
            .revocations
            .borrow_mut()
            .get_mut(&self.node)
            .map(|queue| queue.drain(..).collect())
            .unwrap_or_default()
    }
}

/// Does this blob start with a Net packet header?
///
/// The magic and the version, which is what
/// `ParsedPacket::parse` requires and what a relay would be
/// forwarding. Deliberately cheap and deliberately not exhaustive:
/// its job is to catch the plausible mistake — somebody reaching for
/// the control plane to move a packet — not to be a parser.
fn looks_like_a_net_packet(bytes: &[u8]) -> bool {
    bytes.len() >= net_wire::protocol::HEADER_SIZE
        && u16::from_le_bytes([bytes[0], bytes[1]]) == net_wire::protocol::MAGIC
        && bytes[2] == net_wire::protocol::VERSION
}

fn refused(what: &str) -> LeafError {
    LeafError::ControlPlane(what.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::LeafIdentity;

    const A: NodeId = 0x1111_1111_1111_1111;
    const B: NodeId = 0x2222_2222_2222_2222;

    fn envelope(from: NodeId, to: NodeId, kind: SignalKind, payload: &[u8]) -> SignalEnvelope {
        SignalEnvelope {
            from,
            to,
            dialog: 7,
            kind,
            payload: payload.to_vec(),
            not_after: 1_000,
            signature: vec![0u8; 64],
        }
    }

    /// The whole reason the mock is not an anchor: it cannot accept
    /// an offer, because it could not authenticate one.
    #[test]
    fn an_offer_is_refused_because_no_carrier_could_authenticate_it() {
        let mesh = MockMesh::new();
        let a = mesh.admit(A, "A");
        let error =
            ready(a.offer(Sdp("v=0".into()))).expect_err("the mock must not accept an offer");
        assert!(
            matches!(&error, LeafError::ControlPlane(m) if m.contains("SignalEnvelope")),
            "the refusal must name the path that replaces it, got {error}"
        );
        assert!(
            mesh.carried().is_empty(),
            "a refused offer must not appear in the ledger as carried"
        );
    }

    /// An envelope reaches its addressee, verbatim, and nobody else.
    #[test]
    fn an_envelope_reaches_only_its_addressee() {
        let mesh = MockMesh::new();
        let a = mesh.admit(A, "A");
        let b = mesh.admit(B, "B");
        let sent = envelope(A, B, SignalKind::Offer, b"v=0\r\n");
        ready(a.signal(sent.clone())).expect("carried");

        assert!(a.drain_events().is_empty(), "the sender is not a receiver");
        let received = b.drain_events();
        assert_eq!(received.len(), 1);
        match &received[0] {
            ControlEvent::Signal(got) => assert_eq!(got, &sent, "carried verbatim"),
            other => panic!("expected a Signal, got {other:?}"),
        }
        assert_eq!(
            mesh.carried_kinds(),
            BTreeSet::from([CarriedKind::Signal(SignalKind::Offer)])
        );
    }

    /// The carrier is not trusted, but it is still one connection
    /// per node: it will not relay a `from` this connection does not
    /// own.
    #[test]
    fn a_member_cannot_send_an_envelope_claiming_another_sender() {
        let mesh = MockMesh::new();
        let a = mesh.admit(A, "A");
        mesh.admit(B, "B");
        let error = ready(a.signal(envelope(B, A, SignalKind::Answer, b"x")))
            .expect_err("a spoofed `from` must be refused");
        assert!(matches!(error, LeafError::ControlPlane(_)));
        assert!(mesh.carried().is_empty());
    }

    /// Admission is provisioned, not inferred.
    #[test]
    fn an_unadmitted_addressee_is_refused() {
        let mesh = MockMesh::new();
        let a = mesh.admit(A, "A");
        let error = ready(a.signal(envelope(A, B, SignalKind::Offer, b"v=0")))
            .expect_err("B was never admitted");
        assert!(matches!(&error, LeafError::ControlPlane(m) if m.contains("never admitted")));
    }

    /// **The tripwire, fired.** A Net packet handed to the mock is
    /// refused and lands in the ledger as a non-signalling kind, so
    /// the test's kind assertion fails even if the error were
    /// ignored.
    #[test]
    fn a_net_packet_is_refused_and_poisons_the_ledger() {
        let mesh = MockMesh::new();
        let a = mesh.admit(A, "A");
        let b = mesh.admit(B, "B");

        let mut packet = vec![0u8; net_wire::protocol::HEADER_SIZE + 16];
        packet[0..2].copy_from_slice(&net_wire::protocol::MAGIC.to_le_bytes());
        packet[2] = net_wire::protocol::VERSION;

        let error = ready(a.signal(envelope(A, B, SignalKind::Offer, &packet)))
            .expect_err("a Net packet must not cross a control plane");
        assert!(matches!(&error, LeafError::ControlPlane(m) if m.contains("Net packet")));
        assert!(
            b.drain_events().is_empty(),
            "nothing may be delivered when the tripwire fires"
        );
        assert_eq!(
            mesh.carried_kinds(),
            BTreeSet::from([CarriedKind::RefusedNetPacket]),
            "the refusal must be in the ledger, or a swallowed error would hide it"
        );
        assert!(
            !mesh.carried_kinds().iter().all(|kind| kind.is_signalling()),
            "the ledger assertion a test makes must fail after a leak attempt"
        );
    }

    /// The same tripwire on the announcement path: a store is not a
    /// packet queue.
    #[test]
    fn a_net_packet_cannot_be_published_as_an_announcement() {
        let mesh = MockMesh::new();
        let a = mesh.admit(A, "A");
        let mut packet = vec![0u8; net_wire::protocol::HEADER_SIZE];
        packet[0..2].copy_from_slice(&net_wire::protocol::MAGIC.to_le_bytes());
        packet[2] = net_wire::protocol::VERSION;
        ready(a.publish_announcement(SignedAnnouncement(packet)))
            .expect_err("the store refuses packets too");
        assert_eq!(
            mesh.carried_kinds(),
            BTreeSet::from([CarriedKind::RefusedNetPacket])
        );
    }

    /// Publishing pushes to the members present and stores for the
    /// ones that are not: a late joiner discovers by `query` what it
    /// was not there to receive.
    #[test]
    fn a_late_joiner_discovers_by_query_what_it_never_received() {
        let mesh = MockMesh::new();
        let identity = LeafIdentity::from_secrets(
            crate::identity::EntityKeypair::from_secret([3u8; 32]),
            [4u8; 32],
        );
        let announcement = crate::announce::build_announcement(
            &identity,
            &["chat".to_string()],
            1,
            1_700_000_000_000_000_000,
            300,
        )
        .expect("announcement");

        let a = mesh.admit(identity.node_id(), "A");
        ready(a.publish_announcement(SignedAnnouncement(announcement.clone()))).expect("published");

        let b = mesh.admit(B, "B");
        assert!(
            b.drain_events().is_empty(),
            "B joined after the publish, so no push could have reached it"
        );
        let held = ready(b.query_capability("chat")).expect("query");
        assert_eq!(held, vec![SignedAnnouncement(announcement)]);
        // And what it got back verifies on its own — the carrier
        // added nothing and is not trusted for anything.
        crate::announce::verify_announcement(&held[0].0).expect("verifies unaided");
    }

    /// The ledger is what the report prints: every leg, with the
    /// direction and the length of exactly what crossed.
    #[test]
    fn the_ledger_records_each_leg_with_its_direction_and_length() {
        let mesh = MockMesh::new();
        let a = mesh.admit(A, "A");
        mesh.admit(B, "B");
        let sent = envelope(A, B, SignalKind::Candidate, b"candidate:1 1 udp");
        let encoded = signal::encode(&sent).expect("encodes");
        ready(a.signal(sent)).expect("carried");

        let ledger = mesh.carried();
        assert_eq!(ledger.len(), 1);
        assert_eq!(
            ledger[0],
            Carried {
                seq: 1,
                kind: CarriedKind::Signal(SignalKind::Candidate),
                from: Some(A),
                to: Some(B),
                bytes: encoded.len(),
            }
        );
        let table = mesh.accounting_table();
        assert!(table.contains("signal:candidate"), "{table}");
        assert!(table.contains("A -> B"), "{table}");
        assert_eq!(mesh.carried_bytes(), encoded.len());
    }

    #[test]
    fn a_revocation_bundle_is_carried_verbatim_accounted_and_takeable_once() {
        let mesh = MockMesh::new();
        let mut a = mesh.admit(A, "A");
        mesh.admit(B, "B");
        let bundle = vec![0x51, 0x52, 0x53, 0x99];
        mesh.deliver_revocation(A, bundle.clone()).expect("carry");
        assert_eq!(
            a.take_revocation_bundles(),
            vec![bundle],
            "exact bytes, in arrival order"
        );
        assert_eq!(
            a.take_revocation_bundles(),
            Vec::<Vec<u8>>::new(),
            "taken once — the feed is a take, not a peek"
        );
        let carried = mesh.carried();
        assert_eq!(carried.len(), 1, "one leg crossed");
        assert_eq!(carried[0].kind, CarriedKind::Revocation);
        assert_eq!(carried[0].to, Some(A));
        assert_eq!(carried[0].bytes, 4, "the ledger counts exactly what crossed");
        assert!(
            carried[0].kind.is_signalling(),
            "a signed facts feed is control material, not a packet"
        );
    }

    #[test]
    fn a_net_packet_cannot_be_fed_as_a_revocation_bundle() {
        let mesh = MockMesh::new();
        let mut a = mesh.admit(A, "A");
        let mut packet = vec![0u8; net_wire::protocol::HEADER_SIZE];
        packet[0..2].copy_from_slice(&net_wire::protocol::MAGIC.to_le_bytes());
        packet[2] = net_wire::protocol::VERSION;
        let error = mesh
            .deliver_revocation(A, packet)
            .expect_err("the tripwire fires on the feed too");
        assert!(
            matches!(error, LeafError::ControlPlane(_)),
            "typed refusal: {error}"
        );
        assert_eq!(
            a.take_revocation_bundles(),
            Vec::<Vec<u8>>::new(),
            "nothing was queued for a refused carry"
        );
        assert_eq!(
            mesh.carried_kinds(),
            BTreeSet::from([CarriedKind::RefusedNetPacket]),
            "the refusal is on the ledger and nothing else crossed"
        );
    }

    /// Every future this type returns is already complete, so a test
    /// needs no executor. Polling once and requiring `Ready` is
    /// itself an assertion that stays true: a mock that started
    /// awaiting something would be waiting on a network it does not
    /// have.
    fn ready<T>(future: impl core::future::Future<Output = T>) -> T {
        use core::task::{Context, Poll, Waker};
        let mut context = Context::from_waker(Waker::noop());
        match core::pin::pin!(future).poll(&mut context) {
            Poll::Ready(value) => value,
            Poll::Pending => panic!("an in-memory control plane must never pend"),
        }
    }
}
