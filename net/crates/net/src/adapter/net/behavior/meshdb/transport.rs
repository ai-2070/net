//! Real-wire MeshDB transport — bridges
//! [`MeshDbTransport`] into
//! the mesh's `SUBPROTOCOL_MESHDB` subprotocol dispatch.
//!
//! # Surface
//!
//! - [`MeshDbInboundRouter`] — sync, non-blocking trait the
//!   mesh dispatch loop calls when a `SUBPROTOCOL_MESHDB` frame
//!   arrives. One concrete implementation
//!   ([`MeshDbWireDispatcher`]) handles both ends of the
//!   bidirectional protocol: inbound requests get routed to a
//!   local server, inbound responses get routed to the matching
//!   in-flight caller.
//! - [`MeshDbServer`] — owns a local [`MeshQueryExecutor`] and
//!   produces a stream of responses for every inbound request.
//!   Plug into [`MeshDbWireDispatcher::set_server`] to handle
//!   server-side traffic; omit it for caller-only nodes.
//! - [`MeshDbWireTransport`] — the
//!   [`MeshDbTransport`]
//!   implementation that the
//!   [`FederatedMeshQueryExecutor`](super::federated::FederatedMeshQueryExecutor)
//!   speaks. Encodes outbound `MeshDbRequest`s as
//!   `MeshDbFrame::Request`, ships via `MeshNode::send_subprotocol`,
//!   and returns a `ResponseStream` fed by the dispatcher when
//!   matching `MeshDbFrame::Response`s arrive.
//!
//! # Wire model
//!
//! Both directions ride the same `SUBPROTOCOL_MESHDB` slot. Every
//! frame is a postcard-encoded [`MeshDbFrame`] tagging request vs
//! response. The dispatcher's `try_route(from_node, bytes)` is the
//! single hot-path entry; it decodes once and forks:
//!
//! - [`MeshDbFrame::Request`] → forward to the local
//!   [`MeshDbServer`] (if installed). The server spawns a tokio
//!   task per call that drives the executor and sends a stream of
//!   `MeshDbFrame::Response`s back to `from_node` via
//!   [`MeshDbWireSender`].
//! - [`MeshDbFrame::Response`] → look up the matching in-flight
//!   call by `call_id` in the caller-side table and push the
//!   response on its mpsc; the [`ResponseStream`] returned to the
//!   federated executor drains that mpsc.
//!
//! The dispatcher has no view of the underlying mesh adapter — it
//! takes a [`MeshDbWireSender`] trait object for the outbound
//! send. Production: `&MeshNode`; tests: an in-memory sender that
//! short-circuits to another dispatcher.
//!
//! # Lifecycle
//!
//! The MeshDB-wire dispatcher's `try_route` is sync + non-blocking
//! by the same rule as the replication-inbound router: it runs
//! under the mesh dispatch loop's synchronous critical section,
//! so the actual heavy lifting (executor drive, response stream
//! pump) happens in spawned tokio tasks the dispatcher schedules.
//! Cancellation flows in three layers: (1) caller-side ctx /
//! handle cancel flips the federated executor's per-call handle;
//! (2) the wire emits `MeshDbRequest::Cancel { call_id }` to the
//! server; (3) the server's per-call task notices the cancel and
//! short-circuits its row stream.

use std::collections::{HashMap, HashSet};
use std::pin::Pin;
use std::sync::{Arc, Weak};

use async_trait::async_trait;
use futures::stream::Stream;
use futures::StreamExt;
use parking_lot::RwLock;
use tokio::sync::mpsc;
use tokio::sync::Notify;
use tokio_stream::wrappers::ReceiverStream;

use super::error::MeshError;
use super::executor::MeshQueryExecutor;
use super::federated::{MeshDbTransport, ResponseStream, TransportError};
use super::protocol::{MeshDbFrame, MeshDbRequest, MeshDbResponse, ResultBatch};

/// Maximum number of `MeshDbResponse`s buffered per in-flight
/// caller before the dispatcher's `try_route` reports the inbox
/// full and the response is dropped. Mirrors the
/// `ReplicationInboundRouter` philosophy: bounded backpressure,
/// drop + log under saturation rather than block the dispatch
/// loop.
pub const MESHDB_RESPONSE_INBOX_CAPACITY: usize = 64;

/// Bound for the per-call mpsc that buffers outbound response
/// frames on the server side. Pre-encoded byte vectors;
/// over-capacity bumps a drop counter rather than blocking the
/// executor.
pub const MESHDB_SERVER_OUTBOX_CAPACITY: usize = 64;

/// How many `ResultRow`s a server batches into a single
/// [`MeshDbResponse::Batch`] before flushing. Small enough to
/// keep memory bounded on slow links; large enough to amortise
/// the per-frame postcard encode + send cost. 64 is a starting
/// guess — tunable when profiling shows it matters.
pub const MESHDB_SERVER_BATCH_ROWS: usize = 64;

/// Cap on the server's `pending_cancels` set — `(peer, call_id)`
/// pairs that arrived as `Cancel` before the matching `Execute`.
/// The set exists to cover UDP-reorder windows; a single peer
/// shouldn't have hundreds of cancels outstanding, and a hostile
/// peer that floods cancels can't grow this past the cap. When
/// full the new entry is dropped (degraded: the late `Execute`
/// runs normally rather than being short-circuited).
pub const MESHDB_SERVER_PENDING_CANCELS_CAP: usize = 256;

/// Sync, non-blocking router the mesh's inbound dispatch loop
/// calls when a `SUBPROTOCOL_MESHDB` payload arrives. Mirrors the
/// `ReplicationInboundRouter` shape — must not call into async
/// code, must not hold locks across awaits.
///
/// Returns `Err(bytes)` when the dispatcher couldn't route the
/// payload (decode error, in-flight call unknown, server inbox
/// full). The mesh dispatch loop drops + logs the err return;
/// callers' missing responses surface as a timeout (or via the
/// caller-side ctx cancellation).
pub trait MeshDbInboundRouter: Send + Sync {
    /// Decode `bytes` as a [`MeshDbFrame`] and route to either
    /// the local server or the matching in-flight caller. The
    /// caller (typically the mesh's inbound dispatch loop) drops
    /// and logs on `Err`.
    fn try_route(&self, from_node: u64, bytes: &[u8]) -> Result<(), MeshDbRouteError>;
}

/// Why `MeshDbInboundRouter::try_route` declined to handle an
/// inbound payload. The caller (mesh dispatch loop) routes all
/// variants the same way today — drop + log — but the typed
/// surface lets tests assert on the specific failure mode.
#[derive(Debug)]
pub enum MeshDbRouteError {
    /// The `from_node` sent bytes that don't decode as a
    /// `MeshDbFrame`. Either the peer is on a different protocol
    /// version, or this is a stray frame from a different
    /// subprotocol leak.
    Decode(postcard::Error),
    /// Inbound request, but this dispatcher has no
    /// [`MeshDbServer`] installed. Caller-only nodes return
    /// this; legitimate.
    NoServer,
    /// Inbound response, but no in-flight caller has the
    /// matching `call_id` registered. Late response after the
    /// stream already terminated, or a stray frame from a
    /// peer running a stale generation.
    UnknownCallId(u64),
    /// Inbound response carrying a `call_id` that *is* in-flight,
    /// but `from_node` doesn't match the peer the call was
    /// dispatched to. A mutually-authenticated mesh member sent a
    /// response addressed at someone else's call — either a bug
    /// or a hijack attempt. Carries the in-flight call's expected
    /// peer + the actual sender for diagnostics.
    WrongPeer {
        /// The call_id the response carried.
        call_id: u64,
        /// The peer the caller dispatched the request to.
        expected: u64,
        /// The peer the response actually arrived from.
        actual: u64,
    },
    /// Inbound response, but the matching caller's inbox is
    /// full. Bounded backpressure — drop + log; the caller will
    /// either timeout or see the missing batch as a partial
    /// result.
    InboxFull(u64),
}

impl std::fmt::Display for MeshDbRouteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Decode(e) => write!(f, "meshdb frame decode failed: {e}"),
            Self::NoServer => write!(f, "no MeshDbServer installed on this node"),
            Self::UnknownCallId(id) => write!(f, "no in-flight caller for call_id={id:#x}"),
            Self::WrongPeer {
                call_id,
                expected,
                actual,
            } => write!(
                f,
                "meshdb response for call_id={call_id:#x} arrived from node {actual:#x}; expected {expected:#x}"
            ),
            Self::InboxFull(id) => write!(f, "caller mpsc full for call_id={id:#x}"),
        }
    }
}

impl std::error::Error for MeshDbRouteError {}

/// Wire-side `send` abstraction the dispatcher and server use to
/// emit a single `MeshDbFrame` to a remote node. Decouples the
/// dispatcher from `MeshNode` so the tests can swap in a direct
/// in-memory short-circuit, and the production binding can
/// implement this against `MeshNode::send_subprotocol`.
#[async_trait]
pub trait MeshDbWireSender: Send + Sync {
    /// Encode `frame` and ship to `target_node` via whatever
    /// underlying transport. Errors map to `TransportError` —
    /// `NoRoute` if the target isn't a known peer, `Other` for
    /// everything else.
    async fn send_frame(&self, target_node: u64, frame: MeshDbFrame) -> Result<(), TransportError>;
}

/// State kept per in-flight outbound call. The dispatcher pushes
/// inbound `MeshDbResponse`s onto `tx`; the corresponding
/// `ResponseStream` returned from
/// [`MeshDbWireTransport::send`] drains `rx` until a terminal
/// frame arrives (`End` / `Error` / `Batch { final: true }`) or
/// the caller drops it.
///
/// `target_node` is the peer the request was dispatched to.
/// Responses that arrive from any other peer are rejected with
/// `WrongPeer` rather than silently injected — `call_id`s are
/// allocated from a process-global counter and predictable, so
/// without this gate any mutually-authenticated mesh member
/// could hijack another caller's stream by guessing the id.
struct InflightCaller {
    tx: mpsc::Sender<MeshDbResponse>,
    target_node: u64,
}

/// Concrete [`MeshDbInboundRouter`] implementation paired with a
/// [`MeshDbWireTransport`] that produces the response streams.
/// Construct one via [`MeshDbWireDispatcher::new`] and install
/// the result on `MeshNode` via
/// `MeshNode::set_meshdb_inbound_router`. The companion
/// [`MeshDbWireDispatcher::transport`] returns a value suitable
/// for `FederatedMeshQueryExecutor::new(transport)`.
pub struct MeshDbWireDispatcher {
    sender: Arc<dyn MeshDbWireSender>,
    /// Caller-side: per-call_id mpsc senders for incoming responses.
    inflight: Arc<RwLock<HashMap<u64, InflightCaller>>>,
    /// Optional server side. Cleared by default — pure caller-only
    /// dispatchers don't pay for the server's allocator pool.
    server: Arc<RwLock<Option<Arc<MeshDbServer>>>>,
}

impl MeshDbWireDispatcher {
    /// Construct a caller-only dispatcher. Install a server via
    /// [`Self::set_server`] before remote callers can query this
    /// node.
    pub fn new(sender: Arc<dyn MeshDbWireSender>) -> Self {
        Self {
            sender,
            inflight: Arc::new(RwLock::new(HashMap::new())),
            server: Arc::new(RwLock::new(None)),
        }
    }

    /// Install (or replace) the server-side handler. Once set,
    /// every inbound `MeshDbFrame::Request` is routed to this
    /// server's `handle_request`; outbound responses ride
    /// `sender.send_frame`.
    pub fn set_server(&self, server: Option<Arc<MeshDbServer>>) {
        *self.server.write() = server;
    }

    /// Build the [`MeshDbTransport`] that the federated executor
    /// speaks. Clones the dispatcher's outbound sender + inflight
    /// table; the transport's `send` registers a caller and the
    /// dispatcher's `try_route` resolves matching responses.
    pub fn transport(&self) -> Arc<MeshDbWireTransport> {
        Arc::new(MeshDbWireTransport {
            sender: self.sender.clone(),
            inflight: self.inflight.clone(),
        })
    }
}

impl MeshDbInboundRouter for MeshDbWireDispatcher {
    fn try_route(&self, from_node: u64, bytes: &[u8]) -> Result<(), MeshDbRouteError> {
        let frame = MeshDbFrame::decode(bytes).map_err(MeshDbRouteError::Decode)?;
        match frame {
            MeshDbFrame::Request(req) => {
                let server = self.server.read().clone();
                match server {
                    Some(srv) => {
                        // Spawn the per-call task off the dispatch
                        // critical section. The server's
                        // `dispatch_request` owns the rest of the
                        // call's lifecycle (executor drive + outbound
                        // response stream + cancellation).
                        srv.dispatch_request(from_node, req, self.sender.clone());
                        Ok(())
                    }
                    None => Err(MeshDbRouteError::NoServer),
                }
            }
            MeshDbFrame::Response(resp) => {
                let call_id = response_call_id(&resp);
                let guard = self.inflight.read();
                let entry = guard
                    .get(&call_id)
                    .ok_or(MeshDbRouteError::UnknownCallId(call_id))?;
                // Gate by sender: only the peer the request was
                // dispatched to may inject responses into this
                // call's stream. Mutually-authenticated mesh
                // members are still bounded by their own node_id.
                if entry.target_node != from_node {
                    return Err(MeshDbRouteError::WrongPeer {
                        call_id,
                        expected: entry.target_node,
                        actual: from_node,
                    });
                }
                entry.tx.try_send(resp).map_err(|e| match e {
                    mpsc::error::TrySendError::Full(_) => MeshDbRouteError::InboxFull(call_id),
                    // Closed inbox = caller stream dropped =
                    // pretend we never saw the call_id so the
                    // dispatch logs the standard "no caller"
                    // path. Functionally equivalent.
                    mpsc::error::TrySendError::Closed(_) => {
                        MeshDbRouteError::UnknownCallId(call_id)
                    }
                })?;
                Ok(())
            }
        }
    }
}

fn response_call_id(r: &MeshDbResponse) -> u64 {
    match r {
        MeshDbResponse::Batch { call_id, .. } => *call_id,
        MeshDbResponse::End { call_id } => *call_id,
        MeshDbResponse::Error { call_id, .. } => *call_id,
    }
}

/// The [`MeshDbTransport`] face presented to the federated
/// executor. Each `send` registers a caller in the paired
/// dispatcher, emits the request to `target_node`, and returns a
/// `ResponseStream` that drains inbound responses until a
/// terminal frame arrives.
///
/// On `Drop` of the returned stream the caller is unregistered
/// from the dispatcher's table.
pub struct MeshDbWireTransport {
    sender: Arc<dyn MeshDbWireSender>,
    inflight: Arc<RwLock<HashMap<u64, InflightCaller>>>,
}

#[async_trait]
impl MeshDbTransport for MeshDbWireTransport {
    async fn send(
        &self,
        node: u64,
        request: MeshDbRequest,
    ) -> Result<ResponseStream, TransportError> {
        let call_id = request_call_id(&request);
        let (tx, rx) = mpsc::channel(MESHDB_RESPONSE_INBOX_CAPACITY);
        // Register BEFORE sending — otherwise a fast responder
        // could ship msg back before our caller-table entry is
        // visible and the response would be dropped as
        // UnknownCallId.
        let prev = self.inflight.write().insert(
            call_id,
            InflightCaller {
                tx,
                target_node: node,
            },
        );
        // `call_id`s come from the process-global
        // `FEDERATED_CALL_ID_COUNTER`, so a collision means either
        // (a) someone hand-rolled a request bypassing the counter,
        // or (b) the same call_id reached `send` twice (e.g. a
        // retry that recycled the id). Both are bugs in the layer
        // above us; debug_assert so the test suite catches them
        // without paying a release-build cost. Release builds keep
        // the latest-wins behaviour rather than rejecting — the
        // earlier caller would otherwise hang forever on a stale
        // tx that no one drains.
        debug_assert!(
            prev.is_none(),
            "duplicate inflight call_id={call_id:#x}; previous caller silently overwritten",
        );
        let send_result = self
            .sender
            .send_frame(node, MeshDbFrame::Request(request))
            .await;
        if let Err(e) = send_result {
            self.inflight.write().remove(&call_id);
            return Err(e);
        }
        // Wrap the receiver in a stream that translates
        // `MeshDbResponse` → `Result<ResultRow, MeshError>` and
        // un-registers the caller on terminal frame or drop.
        let inflight = self.inflight.clone();
        let stream = ResponseStreamGuard {
            inner: ReceiverStream::new(rx),
            call_id,
            inflight,
            terminated: false,
        };
        Ok(Box::pin(stream))
    }
}

fn request_call_id(r: &MeshDbRequest) -> u64 {
    match r {
        MeshDbRequest::Execute { call_id, .. } => *call_id,
        MeshDbRequest::Resume { call_id, .. } => *call_id,
        MeshDbRequest::Cancel { call_id } => *call_id,
    }
}

/// Stream wrapper that yields `MeshDbResponse`s as the federated
/// executor expects them. Tracks whether a terminal frame has
/// been delivered so subsequent inbound responses are silently
/// dropped (late-arriving frames after `End` / `Error` /
/// `Batch::last`). On `Drop`, removes the in-flight entry from
/// the dispatcher's table.
struct ResponseStreamGuard {
    inner: ReceiverStream<MeshDbResponse>,
    call_id: u64,
    inflight: Arc<RwLock<HashMap<u64, InflightCaller>>>,
    terminated: bool,
}

impl Drop for ResponseStreamGuard {
    fn drop(&mut self) {
        self.inflight.write().remove(&self.call_id);
    }
}

impl Stream for ResponseStreamGuard {
    type Item = MeshDbResponse;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        use std::task::Poll;
        if self.terminated {
            return Poll::Ready(None);
        }
        match self.inner.poll_next_unpin(cx) {
            Poll::Ready(Some(resp)) => {
                if matches!(
                    &resp,
                    MeshDbResponse::End { .. }
                        | MeshDbResponse::Error { .. }
                        | MeshDbResponse::Batch {
                            batch: ResultBatch { r#final: true, .. },
                            ..
                        }
                ) {
                    self.terminated = true;
                }
                Poll::Ready(Some(resp))
            }
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

/// Server-side handler. Owns a `MeshQueryExecutor` (typically a
/// `LocalMeshQueryExecutor` backed by RedEX) and produces a
/// stream of [`MeshDbResponse`]s for every inbound
/// [`MeshDbRequest::Execute`]. `Resume` and `Cancel` are
/// supported at the protocol level; `Resume` returns a typed
/// `MeshError::ExecutorError` until a consumer drives the
/// continuation surface.
pub struct MeshDbServer {
    executor: Arc<dyn MeshQueryExecutor>,
    /// In-flight server-side calls, keyed by (peer, call_id). A
    /// `Cancel { call_id }` from a peer flips the matching
    /// handle's cancel flag.
    inflight: Arc<RwLock<HashMap<(u64, u64), ServerCallHandle>>>,
    /// `(peer, call_id)` pairs whose `Cancel` arrived before the
    /// matching `Execute` — covers the (rare) UDP-reorder window
    /// where the wire delivers a follow-up cancel ahead of the
    /// initial request. On `Execute` we check + drain the entry;
    /// if present, the call short-circuits to `QueryCancelled`
    /// without driving the executor. Capped at
    /// `MESHDB_SERVER_PENDING_CANCELS_CAP`.
    pending_cancels: Arc<RwLock<HashSet<(u64, u64)>>>,
}

struct ServerCallHandle {
    /// Notified when the peer sends a `Cancel { call_id }`. The
    /// per-call task observes this between row sends and exits
    /// early via the standard `QueryCancelled` path.
    cancel: Arc<Notify>,
}

impl MeshDbServer {
    /// Construct a server that drives `executor` for inbound
    /// `Execute` requests.
    pub fn new(executor: Arc<dyn MeshQueryExecutor>) -> Arc<Self> {
        Arc::new(Self {
            executor,
            inflight: Arc::new(RwLock::new(HashMap::new())),
            pending_cancels: Arc::new(RwLock::new(HashSet::new())),
        })
    }

    /// Number of in-flight server-side calls. Useful for tests
    /// and operator dashboards.
    pub fn inflight_calls(&self) -> usize {
        self.inflight.read().len()
    }

    /// Number of `(peer, call_id)` cancels that arrived without a
    /// matching `Execute` and are still parked. Test surface.
    pub fn pending_cancels(&self) -> usize {
        self.pending_cancels.read().len()
    }

    /// Spawn the per-call task that drives the executor and ships
    /// responses back to `peer` via `sender`. The dispatcher
    /// calls this on every inbound `MeshDbFrame::Request`; the
    /// task owns the call's lifecycle from here.
    fn dispatch_request(
        self: &Arc<Self>,
        peer: u64,
        request: MeshDbRequest,
        sender: Arc<dyn MeshDbWireSender>,
    ) {
        match request {
            MeshDbRequest::Execute { call_id, plan } => {
                // Cover the UDP-reorder window: if a `Cancel` for
                // this (peer, call_id) was parked before the
                // `Execute` landed, drain it and short-circuit
                // straight to `QueryCancelled` rather than driving
                // the executor and racing the cancel into the
                // tokio::select! arm in `run_server_call`.
                let cancelled_early =
                    self.pending_cancels.write().remove(&(peer, call_id));
                if cancelled_early {
                    let sender_clone = sender.clone();
                    tokio::spawn(async move {
                        let _ = sender_clone
                            .send_frame(
                                peer,
                                MeshDbFrame::Response(MeshDbResponse::Error {
                                    call_id,
                                    error: MeshError::QueryCancelled,
                                }),
                            )
                            .await;
                    });
                    return;
                }
                let cancel = Arc::new(Notify::new());
                self.inflight.write().insert(
                    (peer, call_id),
                    ServerCallHandle {
                        cancel: cancel.clone(),
                    },
                );
                let executor = self.executor.clone();
                let inflight = self.inflight.clone();
                tokio::spawn(async move {
                    run_server_call(peer, call_id, plan, executor, sender, cancel, inflight).await;
                });
            }
            MeshDbRequest::Cancel { call_id } => {
                // Flip the cancel flag for the matching call (if
                // any). The per-call task observes and exits at
                // the next row boundary, sending a final
                // `Error { QueryCancelled }`. If no matching call
                // is in-flight, park the cancel — Execute may
                // still be in the receive queue (UDP reorder).
                let guard = self.inflight.read();
                if let Some(handle) = guard.get(&(peer, call_id)) {
                    handle.cancel.notify_one();
                } else {
                    drop(guard);
                    let mut pending = self.pending_cancels.write();
                    if pending.len() < MESHDB_SERVER_PENDING_CANCELS_CAP {
                        pending.insert((peer, call_id));
                    }
                    // Cap reached: drop. The late `Execute` runs
                    // normally; degraded but bounded.
                }
            }
            MeshDbRequest::Resume { call_id, .. } => {
                // Continuation tokens aren't surfaced from the
                // executor today; respond with a typed error so
                // the caller observes the missing capability
                // rather than hanging.
                let sender_clone = sender.clone();
                tokio::spawn(async move {
                    let _ = sender_clone
                        .send_frame(
                            peer,
                            MeshDbFrame::Response(MeshDbResponse::Error {
                                call_id,
                                error: MeshError::ExecutorError {
                                    node: 0,
                                    detail: "Resume is not yet supported by the server side"
                                        .to_string(),
                                },
                            }),
                        )
                        .await;
                });
            }
        }
    }
}

/// Drive one server-side query call to completion. Drains the
/// executor's row stream into `MESHDB_SERVER_BATCH_ROWS`-sized
/// batches and ships them as `MeshDbResponse::Batch` frames;
/// terminates with `End` on clean drain, `Error` on executor
/// failure or `Cancel` notification.
async fn run_server_call(
    peer: u64,
    call_id: u64,
    plan: super::planner::ExecutionPlan,
    executor: Arc<dyn MeshQueryExecutor>,
    sender: Arc<dyn MeshDbWireSender>,
    cancel: Arc<Notify>,
    inflight: Arc<RwLock<HashMap<(u64, u64), ServerCallHandle>>>,
) {
    // Always unregister on exit — Drop guard avoids leaking the
    // entry on any panic / early return.
    struct InflightGuard {
        peer: u64,
        call_id: u64,
        inflight: Arc<RwLock<HashMap<(u64, u64), ServerCallHandle>>>,
    }
    impl Drop for InflightGuard {
        fn drop(&mut self) {
            self.inflight.write().remove(&(self.peer, self.call_id));
        }
    }
    let _guard = InflightGuard {
        peer,
        call_id,
        inflight,
    };

    let running = match executor.execute(plan).await {
        Ok(r) => r,
        Err(err) => {
            let _ = sender
                .send_frame(
                    peer,
                    MeshDbFrame::Response(MeshDbResponse::Error {
                        call_id,
                        error: err,
                    }),
                )
                .await;
            return;
        }
    };
    let handle = running.handle.clone();
    let mut stream = running.rows;
    let mut batch: Vec<super::query::ResultRow> = Vec::with_capacity(MESHDB_SERVER_BATCH_ROWS);

    // Race the cancel notify against the next row.
    loop {
        tokio::select! {
            biased;
            _ = cancel.notified() => {
                handle.cancel();
                let _ = sender
                    .send_frame(
                        peer,
                        MeshDbFrame::Response(MeshDbResponse::Error {
                            call_id,
                            error: MeshError::QueryCancelled,
                        }),
                    )
                    .await;
                return;
            }
            item = stream.next() => {
                match item {
                    Some(Ok(row)) => {
                        batch.push(row);
                        if batch.len() >= MESHDB_SERVER_BATCH_ROWS {
                            let chunk = std::mem::take(&mut batch);
                            if sender
                                .send_frame(
                                    peer,
                                    MeshDbFrame::Response(MeshDbResponse::Batch {
                                        call_id,
                                        batch: ResultBatch::chunk(chunk),
                                    }),
                                )
                                .await
                                .is_err()
                            {
                                return;
                            }
                        }
                    }
                    Some(Err(err)) => {
                        let _ = sender
                            .send_frame(
                                peer,
                                MeshDbFrame::Response(MeshDbResponse::Error { call_id, error: err }),
                            )
                            .await;
                        return;
                    }
                    None => break,
                }
            }
        }
    }

    // Flush any residual rows then emit the terminal End.
    // The `last` flag on the batch is enough — no need for a
    // separate End. Stay strict and emit End anyway for
    // wire-uniformity; the caller-side stream guard treats
    // either as terminal.
    if !batch.is_empty()
        && sender
            .send_frame(
                peer,
                MeshDbFrame::Response(MeshDbResponse::Batch {
                    call_id,
                    batch: ResultBatch::last(batch),
                }),
            )
            .await
            .is_err()
    {
        return;
    }
    let _ = sender
        .send_frame(peer, MeshDbFrame::Response(MeshDbResponse::End { call_id }))
        .await;
}

// =============================================================================
// MeshNode integration
// =============================================================================

/// [`MeshDbWireSender`] backed by a live `MeshNode`. Resolves
/// `target_node → SocketAddr` via the mesh's peer table and ships
/// the encoded frame as a single `SUBPROTOCOL_MESHDB` event via
/// `MeshNode::send_subprotocol`.
///
/// Holds the mesh as `Weak` to avoid a reference cycle —
/// `MeshNode` owns the dispatcher (via the inbound-router slot)
/// which owns the dispatcher's sender; making the back-link strong
/// would leak both forever.
pub struct MeshNodeMeshDbSender {
    mesh: Weak<crate::adapter::net::MeshNode>,
}

impl MeshNodeMeshDbSender {
    /// Construct from a strong reference; the sender holds Weak.
    pub fn new(mesh: &Arc<crate::adapter::net::MeshNode>) -> Self {
        Self {
            mesh: Arc::downgrade(mesh),
        }
    }
}

#[async_trait]
impl MeshDbWireSender for MeshNodeMeshDbSender {
    async fn send_frame(&self, target_node: u64, frame: MeshDbFrame) -> Result<(), TransportError> {
        let mesh = self
            .mesh
            .upgrade()
            .ok_or_else(|| TransportError::Other("mesh node dropped".into()))?;
        let peer_addr = mesh
            .peer_addr(target_node)
            .ok_or(TransportError::NoRoute(target_node))?;
        let bytes = frame
            .encode()
            .map_err(|e| TransportError::Other(format!("frame encode: {e}")))?;
        mesh.send_subprotocol(peer_addr, super::protocol::SUBPROTOCOL_MESHDB, &bytes)
            .await
            .map_err(|e| TransportError::Other(e.to_string()))
    }
}

/// Install MeshDB wire-protocol handling on `mesh` and return the
/// pair of (dispatcher, transport). The transport is what the
/// federated executor speaks; the dispatcher is what the mesh's
/// inbound dispatch loop calls.
///
/// Caller-only nodes leave `server` as `None`. Nodes that
/// answer remote queries pass `Some(MeshDbServer::new(executor))`.
///
/// Idempotent — calling twice replaces the previously-installed
/// router on `mesh`.
pub fn enable_meshdb_on_mesh(
    mesh: &Arc<crate::adapter::net::MeshNode>,
    server: Option<Arc<MeshDbServer>>,
) -> (Arc<MeshDbWireDispatcher>, Arc<MeshDbWireTransport>) {
    let sender = Arc::new(MeshNodeMeshDbSender::new(mesh));
    let dispatcher = Arc::new(MeshDbWireDispatcher::new(sender));
    dispatcher.set_server(server);
    let transport = dispatcher.transport();
    mesh.set_meshdb_inbound_router(Some(dispatcher.clone() as Arc<dyn MeshDbInboundRouter>));
    (dispatcher, transport)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::behavior::meshdb::executor::{
        ChainReader, LocalMeshQueryExecutor, MeshQueryExecutor,
    };
    use crate::adapter::net::behavior::meshdb::planner::{
        CostEstimate, ExecutionPlan, OperatorNode, OperatorPlan,
    };
    use crate::adapter::net::behavior::meshdb::query::SeqNum;
    use std::collections::BTreeMap;

    /// In-memory two-party sender that short-circuits the wire:
    /// `send_frame(target, frame)` delivers to the dispatcher
    /// keyed at `target` in the registry. Both sides install the
    /// SAME `InMemoryWire` and the dispatchers it routes to.
    #[derive(Default)]
    struct InMemoryWire {
        dispatchers: parking_lot::Mutex<HashMap<u64, Arc<MeshDbWireDispatcher>>>,
        /// Local-side node_id when we receive — used as
        /// `from_node` for `try_route`.
        from_node_of: parking_lot::Mutex<HashMap<u64, u64>>,
    }

    impl InMemoryWire {
        fn register(&self, local_node: u64, dispatcher: Arc<MeshDbWireDispatcher>) {
            self.dispatchers.lock().insert(local_node, dispatcher);
        }
        fn set_local(&self, target_node: u64, local_node: u64) {
            // When we deliver to `target_node`, the receiving
            // dispatcher's `try_route` needs `from_node` =
            // `local_node` (i.e. our id from the target's
            // perspective).
            self.from_node_of.lock().insert(target_node, local_node);
        }
    }

    struct SenderTo {
        wire: Arc<InMemoryWire>,
        /// The local node_id this sender speaks AS.
        local_node: u64,
    }

    #[async_trait]
    impl MeshDbWireSender for SenderTo {
        async fn send_frame(
            &self,
            target_node: u64,
            frame: MeshDbFrame,
        ) -> Result<(), TransportError> {
            let bytes = frame
                .encode()
                .map_err(|e| TransportError::Other(e.to_string()))?;
            let dispatcher = self
                .wire
                .dispatchers
                .lock()
                .get(&target_node)
                .cloned()
                .ok_or(TransportError::NoRoute(target_node))?;
            self.wire.set_local(target_node, self.local_node);
            dispatcher
                .try_route(self.local_node, &bytes)
                .map_err(|e| TransportError::Other(e.to_string()))?;
            Ok(())
        }
    }

    /// Test-only ChainReader backed by a BTreeMap.
    #[derive(Default)]
    struct InMemoryChainReader {
        chains: std::sync::Mutex<BTreeMap<u64, BTreeMap<SeqNum, Vec<u8>>>>,
    }

    impl InMemoryChainReader {
        fn append(&self, origin: u64, seq: SeqNum, payload: Vec<u8>) {
            self.chains
                .lock()
                .unwrap()
                .entry(origin)
                .or_default()
                .insert(seq, payload);
        }
    }

    impl ChainReader for InMemoryChainReader {
        fn read_one(&self, origin: u64, seq: SeqNum) -> Option<Vec<u8>> {
            self.chains.lock().unwrap().get(&origin)?.get(&seq).cloned()
        }
        fn read_range(&self, origin: u64, start: SeqNum, end: SeqNum) -> Vec<(SeqNum, Vec<u8>)> {
            self.chains
                .lock()
                .unwrap()
                .get(&origin)
                .map(|c| c.range(start..end).map(|(s, p)| (*s, p.clone())).collect())
                .unwrap_or_default()
        }
        fn latest_seq(&self, origin: u64) -> Option<SeqNum> {
            self.chains
                .lock()
                .unwrap()
                .get(&origin)?
                .keys()
                .next_back()
                .copied()
        }
    }

    fn atomic_plan(op: OperatorPlan) -> ExecutionPlan {
        ExecutionPlan {
            root: OperatorNode {
                operator: op,
                target_nodes: vec![0xB],
                cost: CostEstimate::default(),
            },
            total_cost: CostEstimate::default(),
        }
    }

    #[tokio::test]
    async fn wire_dispatcher_round_trips_a_latest_query_across_two_nodes() {
        // Two-node setup: node_a is the caller; node_b runs the
        // server. The in-memory wire short-circuits encoded
        // frames between their dispatchers. We assert the
        // returned row matches what the server's executor stored.

        let wire = Arc::new(InMemoryWire::default());
        let node_a: u64 = 0xA;
        let node_b: u64 = 0xB;

        // Caller side.
        let sender_a = Arc::new(SenderTo {
            wire: wire.clone(),
            local_node: node_a,
        });
        let dispatcher_a = Arc::new(MeshDbWireDispatcher::new(sender_a));
        wire.register(node_a, dispatcher_a.clone());

        // Server side — runs `LocalMeshQueryExecutor` over an
        // in-memory ChainReader.
        let reader_b = Arc::new(InMemoryChainReader::default());
        reader_b.append(0xCAFE, SeqNum(7), b"hello-wire".to_vec());
        let executor_b: Arc<dyn MeshQueryExecutor> =
            Arc::new(LocalMeshQueryExecutor::new(reader_b));
        let server_b = MeshDbServer::new(executor_b);
        let sender_b = Arc::new(SenderTo {
            wire: wire.clone(),
            local_node: node_b,
        });
        let dispatcher_b = Arc::new(MeshDbWireDispatcher::new(sender_b));
        dispatcher_b.set_server(Some(server_b.clone()));
        wire.register(node_b, dispatcher_b);

        // Build a federated executor on A whose transport is
        // dispatcher_a's; query the server on B for `Latest(0xCAFE)`.
        let transport_a = dispatcher_a.transport();
        let fed_a =
            crate::adapter::net::behavior::meshdb::federated::FederatedMeshQueryExecutor::new(
                transport_a,
            );
        let plan = atomic_plan(OperatorPlan::LatestRead { origin: 0xCAFE });
        let running = fed_a
            .execute(plan)
            .await
            .expect("federated execute over the wire");
        use futures::StreamExt;
        let mut rows = Vec::new();
        let mut stream = running.rows;
        while let Some(item) = stream.next().await {
            rows.push(item.expect("row"));
        }
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].origin, 0xCAFE);
        assert_eq!(rows[0].seq, SeqNum(7));
        assert_eq!(rows[0].payload, b"hello-wire");

        // Server-side bookkeeping cleared after the call drained.
        assert_eq!(server_b.inflight_calls(), 0);
    }

    #[tokio::test]
    async fn wire_dispatcher_no_server_returns_route_error() {
        // Caller-only dispatcher (no server installed). An
        // inbound request frame routes to NoServer.
        let wire = Arc::new(InMemoryWire::default());
        let sender = Arc::new(SenderTo {
            wire: wire.clone(),
            local_node: 0xA,
        });
        let dispatcher = Arc::new(MeshDbWireDispatcher::new(sender));
        let frame = MeshDbFrame::Request(MeshDbRequest::Execute {
            call_id: 42,
            plan: atomic_plan(OperatorPlan::LatestRead { origin: 1 }),
        });
        let err = dispatcher
            .try_route(0xB, &frame.encode().unwrap())
            .expect_err("no server -> error");
        assert!(matches!(err, MeshDbRouteError::NoServer));
    }

    #[tokio::test]
    async fn wire_dispatcher_decode_error_surfaces_route_error() {
        let wire = Arc::new(InMemoryWire::default());
        let sender = Arc::new(SenderTo {
            wire,
            local_node: 0xA,
        });
        let dispatcher = Arc::new(MeshDbWireDispatcher::new(sender));
        let err = dispatcher
            .try_route(0xB, &[0xFFu8; 8])
            .expect_err("garbage bytes -> decode error");
        assert!(matches!(err, MeshDbRouteError::Decode(_)));
    }

    #[tokio::test]
    async fn wire_response_to_unknown_call_id_drops() {
        // A response for a call_id we never registered must
        // surface UnknownCallId rather than panic.
        let wire = Arc::new(InMemoryWire::default());
        let sender = Arc::new(SenderTo {
            wire,
            local_node: 0xA,
        });
        let dispatcher = Arc::new(MeshDbWireDispatcher::new(sender));
        let frame = MeshDbFrame::Response(MeshDbResponse::End { call_id: 999 });
        let err = dispatcher
            .try_route(0xB, &frame.encode().unwrap())
            .expect_err("no caller -> error");
        assert!(matches!(err, MeshDbRouteError::UnknownCallId(999)));
    }

    #[tokio::test]
    async fn server_cancel_before_execute_short_circuits_to_query_cancelled() {
        // Pin: UDP reorder could in principle deliver `Cancel`
        // ahead of `Execute` on the same subprotocol stream. The
        // pre-fix behaviour: the cancel finds no matching
        // inflight handle, gets silently dropped, and the
        // following `Execute` then runs the executor to
        // completion as if cancel was never requested. The fix
        // parks early cancels in `pending_cancels`; when the
        // matching `Execute` lands, the server short-circuits to
        // `MeshError::QueryCancelled` without driving the
        // executor.
        //
        // Drive the dispatch path directly via `dispatch_request`
        // so we control the order without faking wire packets.
        use std::sync::atomic::{AtomicUsize, Ordering};

        let wire = Arc::new(InMemoryWire::default());
        let sender = Arc::new(SenderTo {
            wire: wire.clone(),
            local_node: 0xB,
        });

        // Executor that counts `execute` calls so we can prove
        // the early-cancel path skipped the executor entirely.
        use crate::adapter::net::behavior::meshdb::executor::{ExecuteOptions, RunningQuery};
        struct CountingExecutor {
            executor: Arc<dyn MeshQueryExecutor>,
            calls: Arc<AtomicUsize>,
        }
        #[async_trait]
        impl MeshQueryExecutor for CountingExecutor {
            async fn execute(&self, plan: ExecutionPlan) -> Result<RunningQuery, MeshError> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                self.executor.execute(plan).await
            }
            async fn execute_with(
                &self,
                plan: ExecutionPlan,
                options: ExecuteOptions,
            ) -> Result<RunningQuery, MeshError> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                self.executor.execute_with(plan, options).await
            }
        }

        let reader = Arc::new(InMemoryChainReader::default());
        reader.append(0xCAFE, SeqNum(7), b"never-read".to_vec());
        let inner: Arc<dyn MeshQueryExecutor> = Arc::new(LocalMeshQueryExecutor::new(reader));
        let calls = Arc::new(AtomicUsize::new(0));
        let executor: Arc<dyn MeshQueryExecutor> = Arc::new(CountingExecutor {
            executor: inner,
            calls: calls.clone(),
        });
        let server = MeshDbServer::new(executor);

        // Register a caller-side dispatcher on the OTHER end so
        // the server's outbound `MeshDbResponse::Error` has a
        // place to land for verification. Caller node = 0xA.
        let node_a: u64 = 0xA;
        let sender_a = Arc::new(SenderTo {
            wire: wire.clone(),
            local_node: node_a,
        });
        let dispatcher_a = Arc::new(MeshDbWireDispatcher::new(sender_a));
        wire.register(node_a, dispatcher_a.clone());

        // Register a pretend inflight caller on A keyed by
        // call_id so the server's outbound `Error` frame routes.
        // Skip the usual `transport.send()` because we want to
        // *manually* sequence Cancel-before-Execute on the
        // server side.
        let call_id = 0xC0FFEE_u64;
        let (tx, mut rx) = mpsc::channel(8);
        dispatcher_a.inflight.write().insert(
            call_id,
            InflightCaller {
                tx,
                target_node: 0xB,
            },
        );

        // 1) Cancel arrives first — no Execute yet, so it parks.
        let plan = atomic_plan(OperatorPlan::LatestRead { origin: 0xCAFE });
        let server_sender: Arc<dyn MeshDbWireSender> = sender.clone();
        server.dispatch_request(
            node_a,
            MeshDbRequest::Cancel { call_id },
            server_sender.clone(),
        );
        assert_eq!(
            server.pending_cancels(),
            1,
            "cancel without a matching inflight handle must be parked",
        );

        // 2) Execute lands — must short-circuit; executor is
        // never driven; a single `QueryCancelled` reaches A.
        server.dispatch_request(
            node_a,
            MeshDbRequest::Execute { call_id, plan },
            server_sender,
        );

        // Drain the response and verify.
        let resp = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
            .await
            .expect("server response timed out")
            .expect("server response channel closed");
        match resp {
            MeshDbResponse::Error {
                call_id: got_id,
                error: MeshError::QueryCancelled,
            } => assert_eq!(got_id, call_id),
            other => panic!("expected QueryCancelled; got {other:?}"),
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "executor must not be driven when cancel arrives first",
        );
        // Park slot drained.
        assert_eq!(server.pending_cancels(), 0);
    }

    #[tokio::test]
    async fn wire_response_from_wrong_peer_rejected_without_injection() {
        // Pin: `call_id`s come from a process-global counter and
        // are therefore predictable. Without per-peer scoping any
        // authenticated mesh member could craft a Response with
        // someone else's call_id and inject arbitrary rows /
        // errors into that caller's stream. This test stands up
        // node A as the caller talking to node B (legitimate),
        // then has node C send an unsolicited Response carrying
        // A's call_id. The dispatcher must reject it with
        // `WrongPeer` and the caller's stream must stay clean.
        let wire = Arc::new(InMemoryWire::default());
        let node_a: u64 = 0xA;
        let node_b: u64 = 0xB;
        let node_c: u64 = 0xC; // hostile

        // Caller (A) — no server, just a transport pointed at B.
        let sender_a = Arc::new(SenderTo {
            wire: wire.clone(),
            local_node: node_a,
        });
        let dispatcher_a = Arc::new(MeshDbWireDispatcher::new(sender_a));
        wire.register(node_a, dispatcher_a.clone());

        // Legitimate server (B) with a chain that holds one row,
        // but we'll race C's spoofed response in first.
        let reader_b = Arc::new(InMemoryChainReader::default());
        reader_b.append(0xCAFE, SeqNum(7), b"real".to_vec());
        let executor_b: Arc<dyn MeshQueryExecutor> =
            Arc::new(LocalMeshQueryExecutor::new(reader_b));
        let server_b = MeshDbServer::new(executor_b);
        let sender_b = Arc::new(SenderTo {
            wire: wire.clone(),
            local_node: node_b,
        });
        let dispatcher_b = Arc::new(MeshDbWireDispatcher::new(sender_b));
        dispatcher_b.set_server(Some(server_b));
        wire.register(node_b, dispatcher_b);

        // A issues a query at B and registers the inflight entry.
        let transport_a = dispatcher_a.transport();
        let plan_call_id = 0xDEAD_BEEF;
        let mut stream = transport_a
            .send(
                node_b,
                MeshDbRequest::Execute {
                    call_id: plan_call_id,
                    plan: atomic_plan(OperatorPlan::LatestRead { origin: 0xCAFE }),
                },
            )
            .await
            .expect("send to B");

        // C tries to inject a Response carrying B's call_id.
        let spoof = MeshDbFrame::Response(MeshDbResponse::Error {
            call_id: plan_call_id,
            error: MeshError::ExecutorError {
                node: node_c,
                detail: "spoofed".to_string(),
            },
        });
        let err = dispatcher_a
            .try_route(node_c, &spoof.encode().unwrap())
            .expect_err("spoofed response must be rejected");
        match err {
            MeshDbRouteError::WrongPeer {
                call_id,
                expected,
                actual,
            } => {
                assert_eq!(call_id, plan_call_id);
                assert_eq!(expected, node_b);
                assert_eq!(actual, node_c);
            }
            other => panic!("expected WrongPeer; got {other:?}"),
        }

        // B's legitimate response still arrives and the stream
        // delivers exactly what the server produced.
        let mut got = Vec::new();
        while let Some(resp) = stream.next().await {
            got.push(resp);
        }
        // Expect a terminal frame and no spoofed error to have
        // leaked through.
        assert!(
            !got.iter().any(|r| matches!(
                r,
                MeshDbResponse::Error { error: MeshError::ExecutorError { detail, .. }, .. }
                    if detail == "spoofed"
            )),
            "spoofed response must not be visible to the caller; got {got:?}",
        );
    }
}
