//! Unified SDK error type.

use thiserror::Error;

/// Unified SDK error.
///
/// Marked `#[non_exhaustive]` so adding a new variant is a minor-version
/// change — external `match` statements must include a wildcard arm.
/// The most recent variant additions (`Sampled`, `Unrouted`) tightened
/// `From<IngestionError>` so structured backpressure / sampling /
/// no-route signals stop being funnelled into a stringly-typed
/// `Ingestion(String)`. Callers that previously matched on the
/// string content of `Ingestion` need to be updated to match the
/// new variants.
#[derive(Error, Debug)]
#[non_exhaustive]
pub enum SdkError {
    #[error("node has been shut down")]
    Shutdown,

    /// Generic ingestion failure that doesn't map to a more
    /// specific variant.
    ///
    /// Pre-fix, every `IngestionError` was funnelled here
    /// — `Backpressure`, `Sampled`, `Unrouted`, and shutdown all
    /// became `Ingestion("…")` and callers had to string-match to
    /// pick a remediation. Today's `From<IngestionError>` impl
    /// routes the structured variants below; this `String` variant
    /// stays as a fallback for any future `IngestionError`
    /// addition and for callers that already pattern-match on it.
    #[error("ingestion failed: {0}")]
    Ingestion(String),

    /// Event was deliberately dropped by a sampling / decimation
    /// policy. Retrying is pointless — the producer should accept
    /// the drop or change the sampling rate.
    #[error("event dropped due to sampling")]
    Sampled,

    /// No routable shard for the event. Typically a topology-
    /// transient state (a concurrent scale-down removed the
    /// hashed shard id, or the shard is still provisioning).
    /// Retry once topology stabilizes; back-off-and-retry on
    /// `Backpressure` semantics is the wrong remediation.
    #[error("event has no routable shard")]
    Unrouted,

    #[error("poll failed: {0}")]
    Poll(String),

    #[error("adapter error: {0}")]
    Adapter(String),

    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("invalid configuration: {0}")]
    Config(String),

    #[error("mesh transport not available")]
    NoMesh,

    /// Stream's per-stream in-flight window is full. The caller's events
    /// were NOT sent — daemons decide whether to drop, retry, or buffer
    /// at the app layer. See `Mesh::send_with_retry` / `send_blocking`
    /// for the two built-in policies.
    #[error("stream backpressure: queue full")]
    Backpressure,

    /// Stream's peer session is gone (peer disconnected, never
    /// connected, or the stream was closed).
    #[error("stream not connected")]
    NotConnected,

    /// The stream handle's session has been replaced by a successor
    /// incarnation of the same peer. Distinct from
    /// [`Self::NotConnected`]: the peer is connected, the stream id
    /// may even be open — but on a different session than the one
    /// this handle was minted against, with its own credit, sequence
    /// space and reliability config. Re-open against the current
    /// session; retrying the handle can never succeed.
    #[error("stream's session has been replaced by a successor")]
    SessionSuperseded,

    /// A publisher's `Ack` rejected a Subscribe / Unsubscribe
    /// request. `None` means the rejection arrived without a
    /// structured reason. Gated behind `net` because
    /// `AckReason` lives in the network-transport surface;
    /// non-`net` SDK builds (e.g. redis-only consumer helpers)
    /// don't compile this variant.
    #[cfg(feature = "net")]
    #[error("channel membership rejected: {0:?}")]
    ChannelRejected(Option<net::adapter::net::AckReason>),

    /// NAT-traversal failure — a reflex probe, hole-punch, or
    /// port-mapping path couldn't complete. Always represents a
    /// missed *optimization*, never a connectivity failure:
    /// every NATed peer remains reachable through the routed-
    /// handshake path. `kind` is a stable discriminator
    /// matching the `nat-traversal` crate's error vocabulary
    /// (`reflex-timeout` / `peer-not-reachable` / `transport` /
    /// `rendezvous-no-relay` / `rendezvous-rejected` /
    /// `punch-failed` / `port-map-unavailable` / `unsupported`).
    #[cfg(feature = "nat-traversal")]
    #[error("traversal {kind}: {message}")]
    Traversal {
        /// Stable machine-readable discriminator — same value as
        /// `TraversalError::kind()`. Never localized; never
        /// changed once a variant has shipped.
        kind: &'static str,
        /// Human-readable detail (e.g. the underlying socket
        /// error on a `transport` failure). Empty for kinds
        /// that have no variable detail.
        message: String,
    },
}

#[cfg(feature = "net")]
impl From<net::adapter::net::StreamError> for SdkError {
    fn from(e: net::adapter::net::StreamError) -> Self {
        use net::adapter::net::StreamError;
        match e {
            StreamError::Backpressure => SdkError::Backpressure,
            StreamError::NotConnected => SdkError::NotConnected,
            StreamError::SessionSuperseded => SdkError::SessionSuperseded,
            StreamError::Transport(msg) => SdkError::Adapter(msg),
        }
    }
}

impl From<net::error::IngestionError> for SdkError {
    fn from(e: net::error::IngestionError) -> Self {
        use net::error::IngestionError;
        // Pre-fix this stringified every variant into
        // `SdkError::Ingestion(String)`, forcing callers to match
        // on the message text to distinguish "ring buffer full,
        // retry with backoff" (Backpressure) from "event dropped
        // by sampling, retry futile" (Sampled) from "no routable
        // shard, retry once topology stabilizes" (Unrouted).
        // Each maps to a structured SdkError variant so the
        // remediation choice is encoded in the type system.
        match e {
            IngestionError::Backpressure => SdkError::Backpressure,
            IngestionError::Sampled => SdkError::Sampled,
            IngestionError::Unrouted => SdkError::Unrouted,
            IngestionError::ShuttingDown => SdkError::Shutdown,
            IngestionError::Serialization(err) => SdkError::Serialization(err),
        }
    }
}

impl From<net::error::ConsumerError> for SdkError {
    fn from(e: net::error::ConsumerError) -> Self {
        SdkError::Poll(e.to_string())
    }
}

impl From<net::error::AdapterError> for SdkError {
    fn from(e: net::error::AdapterError) -> Self {
        SdkError::Adapter(e.to_string())
    }
}

#[cfg(feature = "nat-traversal")]
impl From<net::adapter::net::traversal::TraversalError> for SdkError {
    fn from(e: net::adapter::net::traversal::TraversalError) -> Self {
        SdkError::Traversal {
            kind: e.kind(),
            message: e.to_string(),
        }
    }
}

pub type Result<T> = std::result::Result<T, SdkError>;

#[cfg(test)]
mod tests {
    use super::*;
    use net::error::IngestionError;

    /// Each `IngestionError` variant must map to a
    /// structured `SdkError` so callers don't have to string-
    /// match the message text to pick a remediation.
    #[test]
    fn ingestion_backpressure_maps_to_structured_backpressure() {
        let sdk: SdkError = IngestionError::Backpressure.into();
        assert!(
            matches!(sdk, SdkError::Backpressure),
            "Backpressure must map to SdkError::Backpressure, got {:?}",
            sdk
        );
    }

    #[test]
    fn ingestion_sampled_maps_to_structured_sampled() {
        let sdk: SdkError = IngestionError::Sampled.into();
        assert!(
            matches!(sdk, SdkError::Sampled),
            "Sampled must map to SdkError::Sampled so callers \
             know retry is pointless; got {:?}",
            sdk
        );
    }

    #[test]
    fn ingestion_unrouted_maps_to_structured_unrouted() {
        let sdk: SdkError = IngestionError::Unrouted.into();
        assert!(
            matches!(sdk, SdkError::Unrouted),
            "Unrouted must map to SdkError::Unrouted so callers \
             know to wait for topology to stabilize; got {:?}",
            sdk
        );
    }

    #[test]
    fn ingestion_shutdown_maps_to_structured_shutdown() {
        let sdk: SdkError = IngestionError::ShuttingDown.into();
        assert!(
            matches!(sdk, SdkError::Shutdown),
            "ShuttingDown must reuse SdkError::Shutdown, got {:?}",
            sdk
        );
    }

    #[test]
    fn ingestion_serialization_preserves_structured_serialization() {
        // Construct an IngestionError::Serialization carrying a
        // real serde_json error so the From conversion has
        // something to unwrap.
        let parse_err: serde_json::Error =
            serde_json::from_str::<serde_json::Value>("{ this is not json").unwrap_err();
        let sdk: SdkError = IngestionError::Serialization(parse_err).into();
        assert!(
            matches!(sdk, SdkError::Serialization(_)),
            "Serialization must keep its #[from] serde_json::Error, got {:?}",
            sdk
        );
    }
}
