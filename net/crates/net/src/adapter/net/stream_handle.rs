//! The application-facing `Stream` handle.
//!
//! Core-owned on purpose. Stage 2 moved this type into
//! `net-mesh-wire` with its fields public, because a crate boundary
//! cannot express "private outside `adapter::net`" — and that turned
//! a compile error into a live wire bug:
//!
//! ```text
//! let handle = node.open_stream(peer, id, StreamConfig::default())?;  // FireAndForget
//! handle.config.reliability = Reliability::Reliable;                  // was E0616
//! node.send_on_stream(&handle, &events).await?;
//! ```
//!
//! `send_on_stream` takes the wire flags off the **handle**
//! (`mesh.rs`, `let reliable = stream.config().reliability...`) while
//! every piece of retransmit bookkeeping reads the **live**
//! `StreamState`. The packet therefore leaves with `RELIABLE` set and
//! nothing retained to retransmit it: a stream the peer will NACK and
//! the sender can never answer.
//!
//! So the fields are private again and the only constructor is
//! `pub(crate)`, reachable through [`MeshNode::open_stream`]
//! (`crate::adapter::net::MeshNode::open_stream`). An application can
//! read the config and cannot write it; there is no way to mint a
//! handle for an existing stream id with a config the session never
//! agreed to.

use super::stream::StreamConfig;

/// A typed handle to a logical stream within a peer session.
///
/// Created by [`MeshNode::open_stream`](super::MeshNode::open_stream);
/// dropped at any point without affecting the underlying
/// `StreamState` — the stream is removed only when
/// [`MeshNode::close_stream`](super::MeshNode::close_stream) is
/// explicitly called, when it's idle-evicted, or when its parent
/// session tears down.
///
/// The fields are private and there is no public constructor: the
/// handle's `config` is what `send_on_stream` puts on the wire, while
/// the retransmit window lives on the session's `StreamState`. If an
/// application could write either, the two would disagree and the
/// packet would claim a reliability the sender is not tracking.
///
/// Reading is fine:
///
/// ```
/// # use net::adapter::net::{Reliability, Stream, StreamConfig};
/// fn inspect(handle: &Stream) -> (u64, u64, u64, Reliability) {
///     (
///         handle.peer_node_id(),
///         handle.stream_id(),
///         handle.epoch(),
///         handle.config().reliability,
///     )
/// }
/// ```
///
/// Writing the config is rejected — the regression this type exists
/// for:
///
/// ```compile_fail
/// # use net::adapter::net::{Reliability, Stream};
/// fn upgrade(handle: &mut Stream) {
///     handle.config.reliability = Reliability::Reliable;
/// }
/// ```
///
/// So is writing the epoch, which would let a stale handle address a
/// reopened stream's state:
///
/// ```compile_fail
/// # use net::adapter::net::Stream;
/// fn retarget(handle: &mut Stream) {
///     handle.epoch = 7;
/// }
/// ```
///
/// And a handle cannot be minted for an existing stream id at all:
///
/// ```compile_fail
/// # use net::adapter::net::{Stream, StreamConfig};
/// fn forge() -> Stream {
///     Stream {
///         peer_node_id: 1,
///         stream_id: 2,
///         epoch: 0,
///         config: StreamConfig::default(),
///     }
/// }
/// ```
#[derive(Debug, Clone)]
pub struct Stream {
    peer_node_id: u64,
    stream_id: u64,
    epoch: u64,
    config: StreamConfig,
}

impl Stream {
    /// Build a handle for a stream the session has just opened.
    ///
    /// `pub(crate)` by design: `epoch` is only meaningful when it came
    /// from `NetSession::open_stream_full`, and `config` is only
    /// truthful when it is the config that open actually installed.
    #[inline]
    pub(crate) fn new(peer_node_id: u64, stream_id: u64, epoch: u64, config: StreamConfig) -> Self {
        Self {
            peer_node_id,
            stream_id,
            epoch,
            config,
        }
    }

    /// The peer this stream terminates at.
    #[inline]
    pub fn peer_node_id(&self) -> u64 {
        self.peer_node_id
    }

    /// The stream id. Caller-chosen, opaque `u64`.
    #[inline]
    pub fn stream_id(&self) -> u64 {
        self.stream_id
    }

    /// Epoch of the `StreamState` this handle was opened against.
    ///
    /// If the stream is closed and reopened under the same id, the new
    /// state carries a different epoch and this handle's sends fail
    /// with `NotConnected` — which is what stops a stale handle from
    /// silently operating on a different lifetime of the same id.
    #[inline]
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// The config this stream was opened with.
    ///
    /// A shared reference on purpose: this value is read on every
    /// `send_on_stream` to choose the wire flags, and the session's
    /// reliability bookkeeping was installed from it at open time.
    #[inline]
    pub fn config(&self) -> &StreamConfig {
        &self.config
    }
}
