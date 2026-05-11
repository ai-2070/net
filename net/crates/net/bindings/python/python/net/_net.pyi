"""Type stubs for net native module."""

from typing import Iterator, Optional

class IngestResult:
    """Result of an ingestion operation."""

    @property
    def shard_id(self) -> int:
        """Shard the event was assigned to."""
        ...

    @property
    def timestamp(self) -> int:
        """Insertion timestamp (nanoseconds)."""
        ...

class StoredEvent:
    """A stored event returned from polling."""

    @property
    def id(self) -> str:
        """Backend-specific event ID."""
        ...

    @property
    def raw(self) -> str:
        """Raw JSON payload as string."""
        ...

    @property
    def insertion_ts(self) -> int:
        """Insertion timestamp (nanoseconds)."""
        ...

    @property
    def shard_id(self) -> int:
        """Shard ID."""
        ...

    def parse(self) -> dict:
        """Parse the raw JSON into a Python dict."""
        ...

class PollResponse:
    """Poll response containing events and cursor."""

    @property
    def events(self) -> list[StoredEvent]:
        """List of events."""
        ...

    @property
    def next_id(self) -> Optional[str]:
        """Next ID for pagination (pass to next poll as cursor)."""
        ...

    @property
    def has_more(self) -> bool:
        """Whether there are more events available."""
        ...

    def __len__(self) -> int: ...
    def __iter__(self) -> Iterator[StoredEvent]: ...

class Stats:
    """Ingestion statistics."""

    @property
    def events_ingested(self) -> int:
        """Total events ingested."""
        ...

    @property
    def events_dropped(self) -> int:
        """Events dropped due to backpressure."""
        ...

class Net:
    """
    High-performance event bus for Python.

    Example:
        >>> from net import Net
        >>> bus = Net(num_shards=4)
        >>> bus.ingest_raw('{"token": "hello"}')
        IngestResult(shard_id=2, timestamp=1234567890)
        >>> bus.shutdown()

    Can also be used as a context manager:
        >>> with Net() as bus:
        ...     bus.ingest_raw('{"data": "value"}')
    """

    def __init__(
        self,
        num_shards: Optional[int] = None,
        ring_buffer_capacity: Optional[int] = None,
        backpressure_mode: Optional[str] = None,
        redis_url: Optional[str] = None,
        redis_prefix: Optional[str] = None,
        redis_pipeline_size: Optional[int] = None,
        redis_pool_size: Optional[int] = None,
        redis_connect_timeout_ms: Optional[int] = None,
        redis_command_timeout_ms: Optional[int] = None,
        redis_max_stream_len: Optional[int] = None,
        jetstream_url: Optional[str] = None,
        jetstream_prefix: Optional[str] = None,
        jetstream_connect_timeout_ms: Optional[int] = None,
        jetstream_request_timeout_ms: Optional[int] = None,
        jetstream_max_messages: Optional[int] = None,
        jetstream_max_bytes: Optional[int] = None,
        jetstream_max_age_ms: Optional[int] = None,
        jetstream_replicas: Optional[int] = None,
        net_bind_addr: Optional[str] = None,
        net_peer_addr: Optional[str] = None,
        net_psk: Optional[str] = None,
        net_role: Optional[str] = None,
        net_peer_public_key: Optional[str] = None,
        net_secret_key: Optional[str] = None,
        net_public_key: Optional[str] = None,
        net_reliability: Optional[str] = None,
        net_heartbeat_interval_ms: Optional[int] = None,
        net_session_timeout_ms: Optional[int] = None,
        net_batched_io: Optional[bool] = None,
        net_packet_pool_size: Optional[int] = None,
    ) -> None:
        """
        Create a new Net event bus.

        Args:
            num_shards: Number of shards (defaults to CPU core count)
            ring_buffer_capacity: Ring buffer capacity per shard (must be power of 2)
            backpressure_mode: One of "drop_newest", "drop_oldest", "fail_producer"
            redis_url: Redis connection URL (e.g., "redis://localhost:6379")
            redis_prefix: Stream key prefix (default: "net")
            redis_pipeline_size: Maximum commands per pipeline (default: 1000)
            redis_pool_size: Connection pool size (default: num_shards)
            redis_connect_timeout_ms: Connection timeout in milliseconds (default: 5000)
            redis_command_timeout_ms: Command timeout in milliseconds (default: 1000)
            redis_max_stream_len: Maximum stream length, unlimited if not set
            jetstream_url: NATS JetStream URL (e.g., "nats://localhost:4222")
            jetstream_prefix: Stream name prefix (default: "net")
            jetstream_connect_timeout_ms: Connection timeout in milliseconds (default: 5000)
            jetstream_request_timeout_ms: Request timeout in milliseconds (default: 5000)
            jetstream_max_messages: Maximum messages per stream, unlimited if not set
            jetstream_max_bytes: Maximum bytes per stream, unlimited if not set
            jetstream_max_age_ms: Maximum age for messages in milliseconds, unlimited if not set
            jetstream_replicas: Number of stream replicas (default: 1)
            net_bind_addr: Net local bind address (e.g., "127.0.0.1:9000")
            net_peer_addr: Net remote peer address (e.g., "127.0.0.1:9001")
            net_psk: Hex-encoded 32-byte pre-shared key
            net_role: Connection role - "initiator" or "responder"
            net_peer_public_key: Hex-encoded peer's public key (required for initiator)
            net_secret_key: Hex-encoded secret key (required for responder)
            net_public_key: Hex-encoded public key (required for responder)
            net_reliability: Reliability mode - "none", "light", or "full" (default: "none")
            net_heartbeat_interval_ms: Heartbeat interval in milliseconds (default: 5000)
            net_session_timeout_ms: Session timeout in milliseconds (default: 30000)
            net_batched_io: Enable batched I/O for Linux (default: False)
            net_packet_pool_size: Packet pool size (default: 64)
        """
        ...

    def ingest_raw(self, json: str) -> IngestResult:
        """
        Ingest a raw JSON string (fastest path).

        This is the recommended method for high-throughput ingestion.
        The JSON string is stored directly without parsing.

        Args:
            json: JSON string to ingest

        Returns:
            IngestResult with shard_id and timestamp
        """
        ...

    def ingest(self, event: dict) -> IngestResult:
        """
        Ingest a Python dict (convenience method).

        The dict is serialized to JSON before ingestion.
        For maximum performance, use `ingest_raw` with pre-serialized JSON.

        Args:
            event: Dict to ingest (will be JSON serialized)

        Returns:
            IngestResult with shard_id and timestamp
        """
        ...

    def ingest_raw_batch(self, events: list[str]) -> int:
        """
        Ingest multiple raw JSON strings in a batch.

        Args:
            events: List of JSON strings to ingest

        Returns:
            Number of successfully ingested events
        """
        ...

    def poll(
        self,
        limit: int,
        cursor: Optional[str] = None,
        filter: Optional[str] = None,
        ordering: Optional[str] = None,
    ) -> PollResponse:
        """
        Poll events from the bus.

        Args:
            limit: Maximum number of events to return
            cursor: Optional cursor to resume from
            filter: Optional JSON filter expression
            ordering: Event ordering - "none" (default, fastest) or "insertion_ts" (cross-shard ordering by timestamp)

        Returns:
            PollResponse with events and pagination cursor
        """
        ...

    def num_shards(self) -> int:
        """Get the number of active shards."""
        ...

    def stats(self) -> Stats:
        """Get ingestion statistics."""
        ...

    def shutdown(self) -> None:
        """Gracefully shutdown the event bus."""
        ...

    def __enter__(self) -> "Net": ...
    def __exit__(self, exc_type, exc_val, exc_tb) -> bool: ...

# =========================================================================
# CortEX adapter (requires the `cortex` feature at build time)
# =========================================================================

from typing import Iterator, List, Optional

class Redex:
    """Local RedEX manager. One handle per node; shared by all adapters.

    persistent_dir: when provided, adapters opened with persistent=True
    write their channel's idx/dat files under this directory and
    replay them on reopen.
    """
    def __init__(self, persistent_dir: Optional[str] = None) -> None: ...
    def open_file(
        self,
        name: str,
        *,
        persistent: bool = False,
        fsync_every_n: Optional[int] = None,
        fsync_interval_ms: Optional[int] = None,
        retention_max_events: Optional[int] = None,
        retention_max_bytes: Optional[int] = None,
        retention_max_age_ms: Optional[int] = None,
    ) -> "RedexFile":
        """Open (or get) a raw RedEX file for domain-agnostic persistent
        logging. Bypasses the CortEX fold layer — use when you want an
        append-only log and will handle your own event model.

        `fsync_every_n` and `fsync_interval_ms` are mutually exclusive.
        With neither set, close / explicit sync are the only disk
        barriers (`FsyncPolicy::Never`).

        `persistent=True` requires this Redex to have been constructed
        with a `persistent_dir`; otherwise raises `RedexError`.
        """
        ...

class RedexEvent:
    """A materialized RedEX event: `seq` + `payload` + checksum /
    inline flag. Yielded by `RedexFile.read_range` / `RedexTailIter`."""

    seq: int
    payload: bytes
    checksum: int
    is_inline: bool

class RedexFile:
    """Raw RedEX file handle.

    Append / tail / read without the CortEX adapter layer. Safe to
    share; all methods take `&self`. Dropping the last reference does
    NOT close the file — call `close()` explicitly.
    """
    def append(self, payload: bytes) -> int:
        """Append one payload; returns the assigned sequence number."""
        ...
    def append_batch(self, payloads: list[bytes]) -> int | None:
        """Append a batch atomically; returns the seq of the FIRST event,
        or `None` if `payloads` was empty (no events appended).
        Subsequent events are `first + 0, first + 1, ...`.
        """
        ...
    def read_range(self, start: int, end: int) -> list[RedexEvent]:
        """Read the half-open range `[start, end)` from the in-memory
        index. Only retained entries are returned — evicted seqs are
        silently skipped."""
        ...
    def __len__(self) -> int: ...
    def tail(self, from_seq: int = 0) -> "RedexTailIter":
        """Live tail iterator. Backfills `seq >= from_seq` atomically
        and then streams subsequent appends. Stop early with
        `iter.close()`; `StopIteration` fires when the file closes."""
        ...
    def sync(self) -> None:
        """Explicit fsync. Always fsyncs regardless of policy; no-op
        on heap-only files."""
        ...
    def close(self) -> None:
        """Close the file. Outstanding tail iterators stop on their
        next `__next__` call."""
        ...

class RedexTailIter(Iterator[RedexEvent]):
    def __iter__(self) -> "RedexTailIter": ...
    def __next__(self) -> RedexEvent: ...
    def close(self) -> None: ...

class Task:
    """A materialized task record."""

    id: int
    title: str
    status: str  # "pending" | "completed"
    created_ns: int
    updated_ns: int

class TasksAdapter:
    @staticmethod
    def open(
        redex: Redex, origin_hash: int, persistent: bool = False
    ) -> "TasksAdapter": ...
    @staticmethod
    def open_from_snapshot(
        redex: Redex,
        origin_hash: int,
        state_bytes: bytes,
        last_seq: Optional[int] = None,
        persistent: bool = False,
    ) -> "TasksAdapter": ...
    def snapshot(self) -> tuple[bytes, Optional[int]]: ...
    def create(self, id: int, title: str, now_ns: int) -> int: ...
    def rename(self, id: int, new_title: str, now_ns: int) -> int: ...
    def complete(self, id: int, now_ns: int) -> int: ...
    def delete(self, id: int) -> int: ...
    def wait_for_seq(self, seq: int) -> None: ...
    def close(self) -> None: ...
    def is_running(self) -> bool: ...
    def count(self) -> int: ...
    def list_tasks(
        self,
        *,
        status: Optional[str] = None,
        title_contains: Optional[str] = None,
        created_after_ns: Optional[int] = None,
        created_before_ns: Optional[int] = None,
        updated_after_ns: Optional[int] = None,
        updated_before_ns: Optional[int] = None,
        order_by: Optional[str] = None,
        limit: Optional[int] = None,
    ) -> List[Task]: ...
    def watch_tasks(
        self,
        *,
        status: Optional[str] = None,
        title_contains: Optional[str] = None,
        created_after_ns: Optional[int] = None,
        created_before_ns: Optional[int] = None,
        updated_after_ns: Optional[int] = None,
        updated_before_ns: Optional[int] = None,
        order_by: Optional[str] = None,
        limit: Optional[int] = None,
    ) -> "TaskWatchIter": ...
    def snapshot_and_watch_tasks(
        self,
        *,
        status: Optional[str] = None,
        title_contains: Optional[str] = None,
        created_after_ns: Optional[int] = None,
        created_before_ns: Optional[int] = None,
        updated_after_ns: Optional[int] = None,
        updated_before_ns: Optional[int] = None,
        order_by: Optional[str] = None,
        limit: Optional[int] = None,
    ) -> tuple[List[Task], "TaskWatchIter"]:
        """
        Atomic "paint + react" primitive. Returns `(snapshot, iter)` in
        one call; the iterator drops only leading emissions equal to
        `snapshot`, so a mutation racing construction is forwarded
        through instead of being silently dropped.

        Prefer this to calling `list_tasks` + `watch_tasks` separately
        — those race each other and a mutation landing between them
        would be lost.
        """
        ...

class TaskWatchIter(Iterator[List[Task]]):
    def __iter__(self) -> "TaskWatchIter": ...
    def __next__(self) -> List[Task]: ...
    def close(self) -> None: ...

class Memory:
    """A materialized memory record."""

    id: int
    content: str
    tags: List[str]
    source: str
    created_ns: int
    updated_ns: int
    pinned: bool

class MemoriesAdapter:
    @staticmethod
    def open(
        redex: Redex, origin_hash: int, persistent: bool = False
    ) -> "MemoriesAdapter": ...
    @staticmethod
    def open_from_snapshot(
        redex: Redex,
        origin_hash: int,
        state_bytes: bytes,
        last_seq: Optional[int] = None,
        persistent: bool = False,
    ) -> "MemoriesAdapter": ...
    def snapshot(self) -> tuple[bytes, Optional[int]]: ...
    def store(
        self,
        id: int,
        content: str,
        tags: List[str],
        source: str,
        now_ns: int,
    ) -> int: ...
    def retag(self, id: int, tags: List[str], now_ns: int) -> int: ...
    def pin(self, id: int, now_ns: int) -> int: ...
    def unpin(self, id: int, now_ns: int) -> int: ...
    def delete(self, id: int) -> int: ...
    def wait_for_seq(self, seq: int) -> None: ...
    def close(self) -> None: ...
    def is_running(self) -> bool: ...
    def count(self) -> int: ...
    def list_memories(
        self,
        *,
        source: Optional[str] = None,
        content_contains: Optional[str] = None,
        tag: Optional[str] = None,
        any_tag: Optional[List[str]] = None,
        all_tags: Optional[List[str]] = None,
        pinned: Optional[bool] = None,
        created_after_ns: Optional[int] = None,
        created_before_ns: Optional[int] = None,
        updated_after_ns: Optional[int] = None,
        updated_before_ns: Optional[int] = None,
        order_by: Optional[str] = None,
        limit: Optional[int] = None,
    ) -> List[Memory]: ...
    def watch_memories(
        self,
        *,
        source: Optional[str] = None,
        content_contains: Optional[str] = None,
        tag: Optional[str] = None,
        any_tag: Optional[List[str]] = None,
        all_tags: Optional[List[str]] = None,
        pinned: Optional[bool] = None,
        created_after_ns: Optional[int] = None,
        created_before_ns: Optional[int] = None,
        updated_after_ns: Optional[int] = None,
        updated_before_ns: Optional[int] = None,
        order_by: Optional[str] = None,
        limit: Optional[int] = None,
    ) -> "MemoryWatchIter": ...
    def snapshot_and_watch_memories(
        self,
        *,
        source: Optional[str] = None,
        content_contains: Optional[str] = None,
        tag: Optional[str] = None,
        any_tag: Optional[List[str]] = None,
        all_tags: Optional[List[str]] = None,
        pinned: Optional[bool] = None,
        created_after_ns: Optional[int] = None,
        created_before_ns: Optional[int] = None,
        updated_after_ns: Optional[int] = None,
        updated_before_ns: Optional[int] = None,
        order_by: Optional[str] = None,
        limit: Optional[int] = None,
    ) -> tuple[List[Memory], "MemoryWatchIter"]:
        """Atomic snapshot + watch. See `TasksAdapter.snapshot_and_watch_tasks`."""
        ...

class MemoryWatchIter(Iterator[List[Memory]]):
    def __iter__(self) -> "MemoryWatchIter": ...
    def __next__(self) -> List[Memory]: ...
    def close(self) -> None: ...

class NetDb:
    """Unified NetDB handle bundling TasksAdapter + MemoriesAdapter.

    Access per-model adapters via the `.tasks` / `.memories`
    properties. For raw event / stream access, drop down to the
    underlying adapters.
    """

    @staticmethod
    def open(
        *,
        origin_hash: int,
        persistent_dir: Optional[str] = None,
        persistent: bool = False,
        with_tasks: bool = False,
        with_memories: bool = False,
    ) -> "NetDb": ...
    @staticmethod
    def open_from_snapshot(
        bundle: bytes,
        *,
        origin_hash: int,
        persistent_dir: Optional[str] = None,
        persistent: bool = False,
        with_tasks: bool = False,
        with_memories: bool = False,
    ) -> "NetDb": ...
    @property
    def tasks(self) -> Optional[TasksAdapter]: ...
    @property
    def memories(self) -> Optional[MemoriesAdapter]: ...
    def snapshot(self) -> bytes: ...
    def close(self) -> None: ...

class CortexError(Exception):
    """Raised by CortEX adapter operations (tasks, memories) on
    adapter-level failures: `adapter closed`, `fold stopped at seq N`,
    and underlying RedEX storage errors."""

class NetDbError(Exception):
    """Raised by NetDB handle-level operations: snapshot encode /
    decode, missing-model accesses. Per-adapter failures inside a
    NetDB still surface as `CortexError`."""

class RedexError(Exception):
    """Raised by raw RedEX file operations: append / tail / read /
    sync / close, invalid channel names, mutually-exclusive config
    options, or `persistent=True` without a `persistent_dir`."""

# =========================================================================
# Mesh transport + per-peer streams (`net` feature)
# =========================================================================

class NetKeypair:
    """Hex-encoded ed25519 keypair for encrypted UDP transport.

    Treat `secret_key` as secret material — persist via your own
    envelope encryption / secret manager.
    """

    public_key: str
    secret_key: str

def generate_net_keypair() -> NetKeypair:
    """Generate a fresh ed25519 keypair for encrypted UDP transport."""
    ...

class NetStream:
    """Opaque handle to an open mesh stream between this node and a peer."""

    @property
    def peer_node_id(self) -> int: ...
    @property
    def stream_id(self) -> int: ...

class NetStreamStats:
    """Snapshot of per-stream stats.

    `backpressure_events` is the cumulative count of rejections since
    the stream opened; `tx_credit_remaining` dipping to 0 means the
    next send will raise `BackpressureError`.
    """

    tx_seq: int
    rx_seq: int
    inbound_pending: int
    last_activity_ns: int
    active: bool
    backpressure_events: int
    tx_credit_remaining: int
    tx_window: int
    credit_grants_received: int
    credit_grants_sent: int

class NetMesh:
    """Multi-peer encrypted mesh handle.

    Manages connections to multiple peers over one UDP socket with
    automatic failure detection and rerouting. Open per-peer streams
    via `open_stream(...)`; send with `send_on_stream`, react to
    `BackpressureError` / `NotConnectedError` at the app layer.

    Canonical three send policies:

    * Drop on pressure — catch `BackpressureError`, record the drop.
    * Retry with backoff — `send_with_retry(stream, payloads, retries=8)`.
    * Block until clear — `send_blocking(stream, payloads)`.
    """

    def __init__(
        self,
        bind_addr: str,
        psk: str,
        heartbeat_interval_ms: Optional[int] = None,
        session_timeout_ms: Optional[int] = None,
        num_shards: Optional[int] = None,
        identity_seed: Optional[bytes] = None,
        capability_gc_interval_ms: Optional[int] = None,
        require_signed_capabilities: Optional[bool] = None,
        subnet: Optional[List[int]] = None,
        subnet_policy: Optional[dict] = None,
    ) -> None:
        """Construct a new mesh node.

        ``subnet`` is 1–4 ints each in ``[0, 255]``. Defaults to
        ``SubnetId::GLOBAL`` (no restriction). ``subnet_policy`` is
        a dict of shape ``{"rules": [{"tag_prefix": str, "level":
        int, "values": {str: int}}]}`` that derives a subnet from
        the node's capability tags — see
        ``docs/SDK_SECURITY_SURFACE_PLAN.md``.
        """
        ...

    @property
    def public_key(self) -> str:
        """Hex-encoded 32-byte Noise static public key."""
        ...
    @property
    def entity_id(self) -> bytes:
        """32-byte ed25519 entity id. Matches ``Identity.from_seed(seed).entity_id``
        when the mesh was constructed with ``identity_seed=seed``."""
        ...
    @property
    def node_id(self) -> int:
        """u64 node identifier derived from the keypair."""
        ...

    def connect(
        self,
        peer_addr: str,
        peer_public_key: str,
        peer_node_id: int,
    ) -> None:
        """Connect to a peer (initiator). Blocks until handshake
        completes or the timeout elapses."""
        ...
    def accept(self, peer_node_id: int) -> str:
        """Accept an incoming connection (responder). Returns the
        peer's wire address."""
        ...
    def start(self) -> None:
        """Start the receive loop, heartbeats, and router."""
        ...

    def push_to(self, peer_addr: str, json: str) -> bool:
        """Send a raw JSON payload to a direct peer address."""
        ...
    def poll(self, limit: int) -> List[StoredEvent]:
        """Drain up to `limit` events from shard 0."""
        ...

    def add_route(self, dest_node_id: int, next_hop_addr: str) -> None:
        """Add a routing table entry."""
        ...
    def peer_count(self) -> int: ...
    def discovered_nodes(self) -> int: ...
    def open_stream(
        self,
        peer_node_id: int,
        stream_id: int,
        reliability: Optional[str] = None,
        window_bytes: int = 65536,
        fairness_weight: int = 1,
    ) -> "NetStream":
        """Open (or look up) a stream to a connected peer. Repeated
        calls for the same `(peer_node_id, stream_id)` are idempotent;
        first-open wins."""
        ...
    def close_stream(self, peer_node_id: int, stream_id: int) -> None:
        """Close a stream. Idempotent."""
        ...
    def send_on_stream(self, stream: "NetStream", events: List[bytes]) -> None:
        """Send a batch of events on a stream. Raises
        `BackpressureError` if the window is full, `NotConnectedError`
        if the peer session is gone."""
        ...
    def send_with_retry(
        self,
        stream: "NetStream",
        events: List[bytes],
        max_retries: int = 8,
    ) -> None:
        """Retry `BackpressureError` with 5–200 ms exponential backoff
        up to `max_retries` times. Transport errors propagate."""
        ...
    def send_blocking(self, stream: "NetStream", events: List[bytes]) -> None:
        """Retry until the send succeeds or the ~13-minute upper bound
        is hit. Releases the GIL while waiting."""
        ...
    def stream_stats(
        self, peer_node_id: int, stream_id: int
    ) -> Optional["NetStreamStats"]:
        """Per-stream stats snapshot; `None` if the stream isn't open."""
        ...

    def shutdown(self) -> None:
        """Graceful shutdown. Idempotent."""
        ...

    def __enter__(self) -> "NetMesh": ...
    def __exit__(self, exc_type: object, exc_val: object, exc_tb: object) -> bool: ...
    def __repr__(self) -> str: ...
    def register_channel(
        self,
        name: str,
        *,
        visibility: Optional[str] = None,
        reliable: Optional[bool] = None,
        require_token: Optional[bool] = None,
        priority: Optional[int] = None,
        max_rate_pps: Optional[int] = None,
        publish_caps: Optional[dict] = None,
        subscribe_caps: Optional[dict] = None,
    ) -> None:
        """Register a channel on this node. Subscribers are validated
        against this config before being added to the roster.

        ``publish_caps`` / ``subscribe_caps`` are ``CapabilityFilter``
        dicts (same shape as the ``filter`` argument to
        :meth:`find_nodes`) that restrict who may publish or subscribe
        based on the other node's announced capabilities.

        Raises ``ChannelError`` for invalid names / visibility."""
        ...
    def subscribe_channel(
        self,
        publisher_node_id: int,
        channel: str,
        token: Optional[bytes] = None,
    ) -> None:
        """Subscribe to `channel` on `publisher_node_id`.

        Optional ``token`` is the serialized ``PermissionToken`` bytes
        (159 bytes) — attach it when the publisher set
        ``require_token=True`` on the channel, or when the caller's
        caps don't satisfy ``subscribe_caps`` on their own.

        Raises ``ChannelAuthError`` for unauthorized rejections,
        ``ChannelError`` for other rejection / transport failures, and
        ``TokenError`` if ``token`` is malformed or has a bad
        signature."""
        ...
    def unsubscribe_channel(self, publisher_node_id: int, channel: str) -> None:
        """Idempotent counterpart of `subscribe_channel`."""
        ...
    def publish(
        self,
        channel: str,
        payload: bytes,
        *,
        reliability: Optional[str] = None,
        on_failure: Optional[str] = None,
        max_inflight: Optional[int] = None,
    ) -> dict:
        """Fan one payload to every subscriber. Returns a
        `PublishReport` dict: `{attempted, delivered, errors}` where
        `errors` is a list of `{node_id, message}`."""
        ...

    def announce_capabilities(self, caps: dict) -> None:
        """Broadcast `caps` to every directly-connected peer and
        self-index so :meth:`find_nodes` matches. Multi-hop
        propagation is deferred — peers more than one hop away do
        not see the announcement. See ``SDK_SECURITY_SURFACE_PLAN.md``
        for the capability dict shape (hardware, software, models,
        tools, tags, limits)."""
        ...

    def find_nodes(self, filter: dict) -> list[int]:
        """Query the local capability index. Returns node ids
        (including own when the filter matches this node's own
        announcement) whose latest announcement matches `filter`.
        Filter shape mirrors `CapabilityFilter`: ``require_tags``,
        ``require_models``, ``require_tools``, ``min_memory_gb``,
        ``require_gpu``, ``gpu_vendor``, ``min_vram_gb``,
        ``min_context_length``, ``require_modalities``."""
        ...

class BackpressureError(Exception):
    """Raised when a stream's in-flight window is full. The event was
    NOT sent — the caller decides drop / retry / buffer."""

class NotConnectedError(Exception):
    """Raised when a stream's peer session is gone (disconnected,
    never connected, or the stream was closed)."""

class ChannelError(Exception):
    """Raised when a channel operation fails for a non-auth reason:
    invalid name / visibility, unknown channel, rate limit,
    transport failure. Auth rejections raise `ChannelAuthError`
    (subclass of this class)."""

class ChannelAuthError(ChannelError):
    """Subclass of `ChannelError`. Raised when a Subscribe /
    Unsubscribe request is rejected because the publisher's ACL
    denied the subscriber."""

class IdentityError(Exception):
    """Raised for malformed inputs at the identity layer (wrong
    seed length, invalid entity id, unknown scope, etc.). Token-
    validity failures raise `TokenError` instead."""

class TokenError(Exception):
    """Raised when a `PermissionToken` fails validation. The
    exception message has the form ``token: <kind>`` where
    ``<kind>`` is one of ``invalid_signature`` |
    ``not_yet_valid`` | ``expired`` | ``delegation_exhausted`` |
    ``delegation_not_allowed`` | ``not_authorized`` |
    ``invalid_format``. Programmatic callers parse it via
    ``str(e).removeprefix("token: ")``."""

class Identity:
    """ed25519 keypair + local token cache. Cheap to use from
    multiple threads — both inner members are `Arc`-backed on
    the Rust side.

    Persist via :meth:`to_bytes` (the 32-byte ed25519 seed) and
    reload with :meth:`from_seed` / :meth:`from_bytes` on
    subsequent runs. Treat the seed as secret material.
    """

    @staticmethod
    def generate() -> "Identity":
        """Generate a fresh ed25519 identity."""
        ...
    @staticmethod
    def from_seed(seed: bytes) -> "Identity":
        """Load from a caller-owned 32-byte ed25519 seed."""
        ...
    @staticmethod
    def from_bytes(data: bytes) -> "Identity":
        """Alias for :meth:`from_seed`."""
        ...
    def to_bytes(self) -> bytes:
        """Serialize as the 32-byte seed. Treat as secret."""
        ...
    @property
    def entity_id(self) -> bytes:
        """Ed25519 public key (32 bytes)."""
        ...
    @property
    def origin_hash(self) -> int:
        """Derived 64-bit origin hash used in packet headers."""
        ...
    @property
    def node_id(self) -> int:
        """Derived 64-bit node id used for routing / addressing."""
        ...
    @property
    def token_cache_len(self) -> int:
        """Number of cached tokens (testing aid)."""
        ...
    def sign(self, message: bytes) -> bytes:
        """Sign arbitrary bytes. Returns 64-byte ed25519 signature."""
        ...
    def issue_token(
        self,
        subject: bytes,
        scope: List[str],
        channel: str,
        ttl_seconds: int,
        delegation_depth: int = 0,
    ) -> bytes:
        """Issue a scoped token to ``subject`` (32-byte entity id).
        Scope is a subset of ``['publish', 'subscribe', 'admin',
        'delegate']``. Returns the 159-byte serialized
        ``PermissionToken``."""
        ...
    def install_token(self, token: bytes) -> None:
        """Install a received token. Signature is verified on
        insert; raises :class:`TokenError` on bad signature or
        malformed bytes."""
        ...
    def lookup_token(self, subject: bytes, channel: str) -> Optional[bytes]:
        """Look up a cached token by ``(subject, channel)``.
        Returns ``None`` if no exact-channel token is cached."""
        ...

def parse_token(token: bytes) -> dict:
    """Parse a serialized token into a dict. Raises
    :class:`TokenError` on bad length / structure. Does NOT
    verify the signature — use :func:`verify_token` for that."""
    ...

def verify_token(token: bytes) -> bool:
    """Verify the ed25519 signature. ``True`` = valid. Does NOT
    check time-bound validity — see :func:`token_is_expired`."""
    ...

def token_is_expired(token: bytes) -> bool:
    """``True`` if the token's ``not_after`` has passed (host
    wall-clock)."""
    ...

def delegate_token(
    signer: Identity,
    parent: bytes,
    new_subject: bytes,
    restricted_scope: List[str],
) -> bytes:
    """Delegate a token to a new subject. The ``parent`` token
    must include ``'delegate'`` scope and have
    ``delegation_depth > 0``; the ``signer`` must be the subject
    of the parent token."""
    ...

def channel_hash(channel: str) -> int:
    """Hash a channel name to the 16-bit wire-format value."""
    ...

def normalize_gpu_vendor(vendor: str) -> str:
    """Normalize a GPU vendor string to canonical lowercase:
    ``nvidia | amd | intel | apple | qualcomm | unknown``. Unknown
    inputs collapse to ``"unknown"``. Matches the NAPI helper so TS
    and Python callers produce identical announcement payloads."""
    ...
