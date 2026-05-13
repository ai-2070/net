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

use std::collections::HashMap;
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
struct InflightCaller {
    tx: mpsc::Sender<MeshDbResponse>,
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
                let _ = from_node; // unused — responses are addressed by call_id only.
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
        self.inflight.write().insert(call_id, InflightCaller { tx });
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
        })
    }

    /// Number of in-flight server-side calls. Useful for tests
    /// and operator dashboards.
    pub fn inflight_calls(&self) -> usize {
        self.inflight.read().len()
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
                // `Error { QueryCancelled }`.
                let guard = self.inflight.read();
                if let Some(handle) = guard.get(&(peer, call_id)) {
                    handle.cancel.notify_one();
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
}
