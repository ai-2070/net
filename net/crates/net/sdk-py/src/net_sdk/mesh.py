"""
MeshNode — the multi-peer encrypted mesh handle.

Wraps the PyO3 ``_net.NetMesh`` binding with typed Python APIs, plus
re-exports the ``BackpressureError`` / ``NotConnectedError`` exception
classes from the binding so daemon code can ``except`` on them
directly.

Example:
    >>> from net_sdk import MeshNode, BackpressureError
    >>>
    >>> node = MeshNode(bind_addr="127.0.0.1:9000", psk="00" * 32)
    >>> node.connect("127.0.0.1:9001", peer_pubkey, 0x2222)
    >>> node.start()
    >>>
    >>> stream = node.open_stream(
    ...     peer_node_id=0x2222,
    ...     stream_id=7,
    ...     reliability="reliable",
    ...     window_bytes=256,
    ... )
    >>>
    >>> try:
    ...     node.send_on_stream(stream, [b"hello"])
    ... except BackpressureError:
    ...     # daemon decides: drop, buffer, or retry
    ...     pass
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import List, Literal, Optional

# The PyO3 module is `_net`; binding classes and exceptions come from it.
# `BackpressureError` and `NotConnectedError` are `PyException` subclasses
# defined via `pyo3::create_exception!` — re-export them here so users
# import from `net_sdk`, not the private binding module.
from net import (  # type: ignore[attr-defined]
    NetMesh as _NetMesh,
    BackpressureError,
    NotConnectedError,
)


Reliability = Literal["fire_and_forget", "reliable"]


@dataclass(frozen=True)
class StreamStats:
    """Per-stream statistics snapshot. Cheap to read (atomic loads)."""

    tx_seq: int
    rx_seq: int
    inbound_pending: int
    last_activity_ns: int
    active: bool
    backpressure_events: int
    """Cumulative ``BackpressureError`` rejections since the stream opened."""
    tx_credit_remaining: int
    """Bytes of send credit still available. ``0`` = next send rejected."""
    tx_window: int
    """Configured initial credit window in bytes. ``0`` = unbounded."""
    credit_grants_received: int
    """Cumulative ``StreamWindow`` grants received from the peer."""
    credit_grants_sent: int
    """Cumulative ``StreamWindow`` grants emitted to the peer."""


class MeshStream:
    """Opaque handle to an open stream.

    Pass back to :meth:`MeshNode.send_on_stream`,
    :meth:`MeshNode.send_with_retry`, :meth:`MeshNode.send_blocking`,
    or :meth:`MeshNode.close_stream`. The ``peer_node_id`` and
    ``stream_id`` fields are exposed for diagnostics.
    """

    __slots__ = ("peer_node_id", "stream_id", "_native")

    def __init__(self, peer_node_id: int, stream_id: int, native: object) -> None:
        self.peer_node_id = peer_node_id
        self.stream_id = stream_id
        self._native = native

    def __repr__(self) -> str:
        return (
            f"MeshStream(peer_node_id={self.peer_node_id:#x}, "
            f"stream_id={self.stream_id:#x})"
        )


class MeshNode:
    """A node on the Net mesh with stream multiplexing + backpressure."""

    def __init__(
        self,
        bind_addr: str,
        psk: str,
        *,
        heartbeat_interval_ms: Optional[int] = None,
        session_timeout_ms: Optional[int] = None,
        num_shards: Optional[int] = None,
    ) -> None:
        self._native = _NetMesh(
            bind_addr,
            psk,
            heartbeat_interval_ms=heartbeat_interval_ms,
            session_timeout_ms=session_timeout_ms,
            num_shards=num_shards,
        )

    @property
    def public_key(self) -> str:
        """Hex-encoded Noise static public key."""
        return self._native.public_key

    @property
    def node_id(self) -> int:
        """This node's id."""
        return self._native.node_id

    def connect(self, peer_addr: str, peer_public_key: str, peer_node_id: int) -> None:
        """Connect to a peer as initiator."""
        self._native.connect(peer_addr, peer_public_key, peer_node_id)

    def accept(self, peer_node_id: int) -> str:
        """Accept an incoming connection as responder. Returns the peer's wire address."""
        return self._native.accept(peer_node_id)

    def start(self) -> None:
        """Start the receive loop / heartbeats / router."""
        self._native.start()

    def peer_count(self) -> int:
        """Number of connected peers."""
        return self._native.peer_count()

    # ── Gang-claim resource-island scheduler ─────────────────────────

    def publish_island_topology(
        self,
        island_id: int,
        units: List[int],
        capabilities: List[str],
        load: float,
        p50_latency_us: int,
    ) -> int:
        """Publish this node's island-topology record (its host is forced
        to this node). Self-indexed locally so this node's own scheduler
        sees it, then broadcast to peers; returns the peer fan-out count.
        `capabilities` are resident tags (e.g. ``"model:<hex>"``)."""
        return self._native.publish_island_topology(
            island_id, units, capabilities, load, p50_latency_us
        )

    def match_islands(
        self,
        tags_all: List[str],
        *,
        min_units: Optional[int] = None,
        max_load: Optional[float] = None,
        max_p50_latency_us: Optional[int] = None,
        require_capabilities: Optional[List[str]] = None,
        selection: Optional[str] = None,
        load_band_target: Optional[float] = None,
        prefer_capability: Optional[str] = None,
    ) -> List[int]:
        """Match islands against the criteria over this node's folds
        (read-only; no claim). Best island first. `selection` is one of
        ``least_loaded`` (default) / ``pack`` / ``load_band`` / ``lowest_id``."""
        return self._native.match_islands(
            tags_all,
            min_units=min_units,
            max_load=max_load,
            max_p50_latency_us=max_p50_latency_us,
            require_capabilities=require_capabilities or [],
            selection=selection,
            load_band_target=load_band_target,
            prefer_capability=prefer_capability,
        )

    def reserve_island(self, island_id: int, until_unix_us: int) -> str:
        """Reserve `island_id` until `until_unix_us` (wall-clock micros).
        Returns ``"won"`` if this node now holds it, ``"lost"`` otherwise."""
        return self._native.reserve_island(island_id, until_unix_us)

    def release_island(self, island_id: int) -> str:
        """Release `island_id` this node holds. Returns ``"lost"`` if this
        node wasn't the holder."""
        return self._native.release_island(island_id)

    def claim_island(
        self,
        tags_all: List[str],
        until_unix_us: int,
        *,
        min_units: Optional[int] = None,
        max_load: Optional[float] = None,
        max_p50_latency_us: Optional[int] = None,
        require_capabilities: Optional[List[str]] = None,
        selection: Optional[str] = None,
        load_band_target: Optional[float] = None,
        prefer_capability: Optional[str] = None,
    ) -> Optional[int]:
        """Match + reserve the first available island in one call. Returns
        its id, or ``None`` when nothing matched / all contended."""
        return self._native.claim_island(
            tags_all,
            until_unix_us,
            min_units=min_units,
            max_load=max_load,
            max_p50_latency_us=max_p50_latency_us,
            require_capabilities=require_capabilities or [],
            selection=selection,
            load_band_target=load_band_target,
            prefer_capability=prefer_capability,
        )

    # ── Stream API ───────────────────────────────────────────────────

    def open_stream(
        self,
        peer_node_id: int,
        stream_id: int,
        *,
        reliability: Reliability = "fire_and_forget",
        window_bytes: Optional[int] = None,
        fairness_weight: int = 1,
    ) -> MeshStream:
        """Open (or look up) a logical stream to a connected peer.

        ``window_bytes`` defaults to the core's
        ``DEFAULT_STREAM_WINDOW_BYTES`` (64 KB) when ``None`` so v2
        backpressure is ON out of the box. Pass ``0`` to restore the
        v1 unbounded-queue behavior on this stream.

        Repeated calls for the same ``(peer_node_id, stream_id)`` are
        idempotent — the first open wins and later differing configs
        are logged and ignored.
        """
        kwargs = {
            "reliability": reliability,
            "fairness_weight": fairness_weight,
        }
        if window_bytes is not None:
            kwargs["window_bytes"] = window_bytes
        native = self._native.open_stream(peer_node_id, stream_id, **kwargs)
        return MeshStream(peer_node_id, stream_id, native)

    def close_stream(self, peer_node_id: int, stream_id: int) -> None:
        """Close a stream. Idempotent."""
        self._native.close_stream(peer_node_id, stream_id)

    def send_on_stream(self, stream: MeshStream, events: List[bytes]) -> None:
        """Send a batch of events on an explicit stream.

        Raises:
            BackpressureError: stream's in-flight window is full — the
                caller decides whether to drop, retry, or buffer.
            NotConnectedError: stream's peer session is gone.
            RuntimeError: underlying transport failure.
        """
        self._native.send_on_stream(stream._native, events)

    def send_with_retry(
        self,
        stream: MeshStream,
        events: List[bytes],
        max_retries: int = 8,
    ) -> None:
        """Send, retrying on :class:`BackpressureError` with 5 ms → 200 ms
        exponential backoff up to ``max_retries`` times. Transport
        errors and :class:`NotConnectedError` are raised immediately.
        """
        self._native.send_with_retry(stream._native, events, max_retries)

    def send_blocking(self, stream: MeshStream, events: List[bytes]) -> None:
        """Block the calling thread until the send succeeds or a
        transport error occurs.

        Retries :class:`BackpressureError` with 5 ms → 200 ms
        exponential backoff up to 4096 times (~13 min worst case) —
        effectively "block until the network lets up" for practical
        workloads, but with a hard upper bound so runaway pressure
        can't hang the caller forever. Use :meth:`send_with_retry`
        for a tighter bound.
        """
        self._native.send_blocking(stream._native, events)

    def stream_stats(self, peer_node_id: int, stream_id: int) -> Optional[StreamStats]:
        """Snapshot of per-stream stats. ``None`` if the peer or stream
        isn't registered."""
        raw = self._native.stream_stats(peer_node_id, stream_id)
        if raw is None:
            return None
        return StreamStats(
            tx_seq=raw.tx_seq,
            rx_seq=raw.rx_seq,
            inbound_pending=raw.inbound_pending,
            last_activity_ns=raw.last_activity_ns,
            active=raw.active,
            backpressure_events=raw.backpressure_events,
            tx_credit_remaining=raw.tx_credit_remaining,
            tx_window=raw.tx_window,
            credit_grants_received=raw.credit_grants_received,
            credit_grants_sent=raw.credit_grants_sent,
        )

    def shutdown(self) -> None:
        """Shutdown the mesh node."""
        self._native.shutdown()

    def __enter__(self) -> "MeshNode":
        return self

    def __exit__(self, *_: object) -> None:
        self.shutdown()


__all__ = [
    "MeshNode",
    "MeshStream",
    "StreamStats",
    "Reliability",
    "BackpressureError",
    "NotConnectedError",
]
