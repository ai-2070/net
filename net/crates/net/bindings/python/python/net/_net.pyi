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

from typing import Any, Dict, Iterator, List, Optional, Tuple

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
        reflex_override: Optional[str] = None,
        try_port_mapping: Optional[bool] = None,
        auto_direct_upgrade: Optional[bool] = None,
        permissive_channels: Optional[bool] = None,
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
    @property
    def local_addr(self) -> str:
        """This node's bound local UDP address (e.g. ``"127.0.0.1:54321"``);
        resolves the OS-assigned port for a ``:0`` bind."""
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

    def rendezvous_string(self) -> str:
        """The invite ``rendezvous`` locator for this node (address + Noise
        static key + node id), to pass to ``OperatorEnrollment.invite``.
        (Requires the ``delegation`` feature.)"""
        ...
    def join(
        self, device: Identity, invite: str, name: str, tags: List[str]
    ) -> DelegationChain:
        """Device-side enrollment: enroll ``device``'s key into the mesh named
        by the ``invite`` string, returning the verified ``root -> device``
        chain. This node must be ``start()``ed. (Requires ``delegation``.)"""
        ...
    def serve_enrollment_auto(
        self,
        operator: "OperatorEnrollment",
        grant_ttl_seconds: int,
        max_depth: Optional[int] = ...,
    ) -> "EnrollmentServeHandle":
        """Operator-side: serve the full device lifecycle on this node (auto —
        the invite is the authorization): enroll (join) + renew. Hold the
        returned handle to keep the services open. This node must be
        ``start()``ed. (Requires ``delegation``.)"""
        ...
    def renew(self, enrollment: "DeviceEnrollment") -> DelegationChain:
        """Device-side renewal: refresh the grant carried by ``enrollment`` over
        the mesh, returning the verified fresh ``root -> device`` chain. This
        node must be ``start()``ed + ``permissive_channels=True``. (Requires
        ``delegation``.)"""
        ...
    def publish_tools(
        self,
        tools: List[Tuple[str, Optional[str], str]],
        callback: Any,
        version: str = ...,
        owner_origin: Optional[int] = ...,
        allow_any_caller: bool = ...,
    ) -> "LocalPublicationHandle":
        """Publish this node's OWN local tools as mesh capabilities (V2 Phase 2)
        — the inverse of ``net wrap``. ``tools`` is a list of
        ``(name, description|None, input_schema_json)`` (the input schema as a
        JSON string). ``callback`` is an **async** callable
        ``async (tool_name: str, args_json: str) -> str | tuple[str, bool]``
        invoked on a remote call; its return is the tool's text output (a
        ``(text, is_error)`` tuple flags a tool-level error). A consumer
        discovers + invokes these through the ordinary
        :class:`AsyncCapabilityGateway`. ``owner_origin`` scopes admission (an
        ``origin_hash`` admits only that caller; ``None`` admits only **this
        node itself** — the fail-closed default). Pass
        ``allow_any_caller=True`` to explicitly admit every mesh peer
        (overrides ``owner_origin``; gate invocations yourself, e.g. with an
        approval callback). Hold the returned handle to keep the tools
        published. This node must be ``start()``ed + ``permissive_channels=True``.
        (Requires the ``publish`` feature.)"""
        ...
    def serve_a2a(self, callback: Any) -> "A2aServeHandle":
        """Serve the agent-to-agent (A2A) task lifecycle (V2 Phase 3), backed by
        a Python **async** task executor ``callback``
        ``async (task_id: str, prompt: str, context_refs: list[str],
        tags: list[str]) -> str`` returning the result's artifact ref. Hold the
        returned handle to keep accepting tasks. This node must be ``start()``ed.
        (Requires the ``a2a`` feature.)"""
        ...
    def submit_task(
        self,
        target_node_id: int,
        prompt: str,
        context_refs: List[str] = ...,
        tags: List[str] = ...,
    ) -> str:
        """Hand off a task to the executor at ``target_node_id``: ``prompt`` plus
        optional Datafort ``context_refs`` (the executor doesn't share your
        memory) and routing ``tags``. Returns the accepted task id; raises if the
        executor rejected it. The node must already be connected to
        ``target_node_id``. (Requires the ``a2a`` feature.)"""
        ...
    def task_status(self, target_node_id: int, task_id: str) -> Optional[str]:
        """The executor's status for ``task_id`` as a JSON string
        (``{brief, state, updated_at}``), or ``None`` if unknown. (Requires the
        ``a2a`` feature.)"""
        ...
    def cancel_task(self, target_node_id: int, task_id: str) -> bool:
        """Cancel ``task_id`` on the executor; returns whether it was in flight.
        The executor's coroutine is cancelled — the remote work stops. (Requires
        the ``a2a`` feature.)"""
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
        token_roots: Optional[list[bytes]] = None,
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

        ``require_token`` gates publish/subscribe on a valid token
        chain; on its own (no ``token_roots``) it fails closed.
        ``token_roots`` is a list of 32-byte entity ids whose signature
        may root a presented chain — set it to anchor the channel's
        root of trust.

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
        (161 bytes) — attach it when the publisher set
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

    # -- NAT traversal (requires the `nat-traversal` build) ---------
    def traversal_stats(self) -> dict:
        """Cumulative NAT-traversal counters — the full stage-5
        snapshot. Keys: ``punches_attempted``,
        ``punches_succeeded``, ``punches_failed`` (derived),
        ``relay_fallbacks``, ``punch_timeouts``,
        ``punch_rejections``, ``rendezvous_no_relay``,
        ``upgrades_attempted``, ``upgrades_succeeded``,
        ``upgrades_deferred_busy``, ``port_mapping_active``
        (bool), ``port_mapping_external`` (str | None),
        ``port_mapping_renewals``. Monotonic; never reset."""
        ...

    def connect_direct(
        self, peer_node_id: int, peer_public_key: str, coordinator: int
    ) -> None:
        """Establish a session via the rendezvous path with
        ``coordinator`` mediating. Optimization, not correctness —
        always resolves (punch-failed falls back to the routed
        handshake); inspect :meth:`traversal_stats` to distinguish
        outcomes."""
        ...

    def connect_direct_auto(self, peer_node_id: int, peer_public_key: str) -> None:
        """Like :meth:`connect_direct`, but auto-selects the
        rendezvous coordinator. Raises ``RuntimeError`` with
        ``traversal: rendezvous-no-relay`` when a punch-needing
        pair has no coordinator candidate — the caller stays on
        the routed path."""
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
        'delegate']``. Returns the 161-byte serialized
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
    """Hash a channel name to its canonical 64-bit substrate identifier.

    Used as the ACL/storage/config key (matches the ``channel_hash``
    field on ``PermissionToken``). The wire ``NetHeader`` fast-path
    hint is the low 16 bits of this value.
    """
    ...

def normalize_gpu_vendor(vendor: str) -> str:
    """Normalize a GPU vendor string to canonical lowercase:
    ``nvidia | amd | intel | apple | qualcomm | unknown``. Unknown
    inputs collapse to ``"unknown"``. Matches the NAPI helper so TS
    and Python callers produce identical announcement payloads."""
    ...

# =============================================================================
# Delegated agent identity (`HERMES_INTEGRATION_PLAN.md` Phase 3). Present iff
# the module was built with the `delegation` feature (default in the wheel).
# H8: takes/returns opaque `Identity` handles + public entity-ids; private
# seeds never cross into Python.
# =============================================================================

GATEWAY_DELEGATION_CHANNEL: str
"""The well-known channel every gateway delegation binds to (never actually
published to). The deriver and the verifier both agree on it."""

def derive_child_identity(parent: Identity, label: str) -> Identity:
    """Derive a stable child ``Identity`` handle from ``parent`` under
    ``label`` (deterministic blake3 KDF over the parent seed), so a
    machine / gateway identity is reproducible across restarts from the
    root alone. The returned handle owns its keypair; the private seed is
    never exposed. ``label`` namespaces siblings, e.g. ``"machine:hostA"``
    vs ``"gateway:hostA:hermes"``."""
    ...

def default_revocation_store_path() -> Optional[str]:
    """The per-user default revocation-store path — the same file a
    ``net wrap --owner-root`` provider honors and ``net identity revoke``
    writes — or ``None`` if neither a data-local nor a home directory resolves.
    Pass it (or the ``NET_MESH_REVOCATION_STORE`` override) to
    :meth:`RevocationRegistry.load_from_store`."""
    ...

class RevocationRegistry:
    """Shared per-issuer revocation floor. Bumping an issuer's floor
    invalidates every outstanding delegation from that issuer — including
    delegated children — the moment :meth:`DelegationChain.verify` next
    runs. One registry is shared by a gateway and all its subagents."""

    def __init__(self) -> None: ...
    def revoke_below(self, issuer: bytes, generation: int) -> None:
        """Set ``issuer``'s floor to ``generation``; tokens with a lower
        ``issuer_generation`` are rejected next verify. Monotonic."""
        ...
    def revoke(self, issuer: bytes) -> None:
        """Revoke every generation-0 delegation from ``issuer`` (floor ->
        1). Revoking a *machine* identity kills its gateway chain and that
        gateway's subagents, while another machine's chain is untouched."""
        ...
    def floor(self, issuer: bytes) -> int:
        """Current revocation floor for ``issuer`` (0 if never revoked)."""
        ...
    def load_from_store(self, path: str) -> None:
        """Reload the machine-shared revocation floors at ``path`` into this
        registry (monotonic — floors only rise, so re-loading is idempotent). A
        missing store file is a no-op; an unreadable/corrupt store raises. Lets a
        caller's self-check observe an operator's ``net identity revoke`` (the
        same file a ``net wrap --owner-root`` provider honors), so a revoked chain
        fails :meth:`DelegationChain.verify` on the caller side too. Use
        :func:`default_revocation_store_path` for ``path``."""
        ...

class DelegationChain:
    """A ``root -> ... -> leaf`` delegation chain that attributes a
    capability invocation to the terminal agent identity. Build with
    :meth:`derive_gateway`, extend per-task with :meth:`extend_to_subagent`,
    check with :meth:`verify`."""

    @staticmethod
    def derive_gateway(
        root: Identity,
        machine: Identity,
        gateway: Identity,
        ttl_seconds: int,
        max_depth: Optional[int] = ...,
    ) -> "DelegationChain":
        """Build a ``root -> machine -> gateway`` chain. ``root`` and
        ``machine`` sign their delegations; only ``gateway``'s public
        entity-id is used. ``ttl_seconds`` is the grant lifetime (the whole
        chain expires together); ``max_depth`` (default 4) leaves room for
        subagent hops."""
        ...
    def extend_to_subagent(
        self, leaf_signer: Identity, subagent: bytes
    ) -> "DelegationChain":
        """Extend with a ``... -> subagent`` link signed by the current
        leaf's owner (``leaf_signer``, whose entity-id must equal the
        chain's current leaf subject). Returns a new chain; the original is
        unchanged."""
        ...
    def verify(
        self,
        presenter: bytes,
        root: bytes,
        registry: RevocationRegistry,
        skew_seconds: int = 0,
    ) -> bool:
        """``True`` if the chain still authorizes an invocation by
        ``presenter``, anchored at ``root``, honoring ``registry``. Returns
        ``False`` (never raises) when expired, revoked, rooted elsewhere, or
        presented by the wrong identity."""
        ...
    @staticmethod
    def from_bytes(data: bytes) -> "DelegationChain":
        """Parse a serialized chain. Raises ``TokenError`` on an empty
        chain, too many links, or trailing garbage."""
        ...
    def to_bytes(self) -> bytes:
        """Serialize to wire bytes (a ``TokenChain`` blob)."""
        ...
    def subjects(self) -> List[bytes]:
        """The subject entity-id of each link, root-to-leaf."""
        ...
    @property
    def leaf(self) -> bytes:
        """The terminal (leaf) subject entity-id — the agent this chain
        attributes to."""
        ...
    @property
    def root(self) -> bytes:
        """The root issuer entity-id the chain anchors at."""
        ...
    def __len__(self) -> int: ...

# -----------------------------------------------------------------------------
# Device enrollment (HERMES_INTEGRATION_PLAN_V2.md Phase 1).
# -----------------------------------------------------------------------------

def fingerprint(entity: bytes) -> str:
    """A short, human-comparable fingerprint of a 32-byte entity-id, shown
    on both sides of a join (``A1B2-C3D4-E5F6-0789``)."""
    ...

class InviteToken:
    """A pre-authorization to *ask* to join a mesh — not a key. Carries the
    mesh ``root``, a ``rendezvous`` locator, a single-use nonce, and a short
    TTL. The copy-paste / QR form is :meth:`encode`."""

    @staticmethod
    def decode(s: str) -> "InviteToken":
        """Parse a ``net-invite:<base64url>`` string. Raises on a missing
        prefix, bad base64, or malformed bytes."""
        ...
    def encode(self) -> str:
        """The copy-paste / QR invite string."""
        ...
    def root_fingerprint(self) -> str:
        """The displayed fingerprint of the mesh root — show it to the joiner."""
        ...
    def is_expired(self, now: int) -> bool:
        """Whether the invite has expired at ``now`` (unix secs)."""
        ...
    def to_bytes(self) -> bytes: ...
    @staticmethod
    def from_bytes(data: bytes) -> "InviteToken": ...
    @property
    def root(self) -> bytes:
        """The mesh root entity-id this invite admits into."""
        ...
    @property
    def rendezvous(self) -> str:
        """The transport locator the device dials."""
        ...
    @property
    def expires_at(self) -> int: ...

class JoinRequest:
    """A device's request to join, signed by the device's own key."""

    @staticmethod
    def create(
        device: Identity, name: str, tags: List[str], invite: InviteToken
    ) -> "JoinRequest":
        """Build + sign a request against ``invite``. ``device`` is the opaque
        ``Identity`` handle whose key is enrolled (H8: seed stays in Rust)."""
        ...
    def verify_self_signature(self) -> bool:
        """``True`` if the device's self-signature verifies."""
        ...
    def to_bytes(self) -> bytes: ...
    @staticmethod
    def from_bytes(data: bytes) -> "JoinRequest":
        """Parse wire bytes (does not verify the signature)."""
        ...
    @property
    def device(self) -> bytes: ...
    @property
    def name(self) -> str: ...
    @property
    def tags(self) -> List[str]: ...

class JoinOutcome:
    """The operator's response to a join request — admitted (carrying the
    granted chain) or rejected (with a stable code + message)."""

    @staticmethod
    def from_bytes(data: bytes) -> "JoinOutcome": ...
    def to_bytes(self) -> bytes: ...
    def into_chain(self, device: bytes, invite_root: bytes) -> DelegationChain:
        """Device-side: verify the admitted grant anchors at ``invite_root``
        and binds to ``device``, returning the ``DelegationChain``. Raises on a
        rejection or an untrusted grant (wrong root / device)."""
        ...
    @property
    def is_admitted(self) -> bool: ...
    @property
    def reject_code(self) -> Optional[int]:
        """The stable reject code (1..=7: malformed, unknown-invite, expired,
        bad-request, replay, internal, denied) if rejected, else ``None``."""
        ...
    @property
    def reject_message(self) -> Optional[str]: ...

class DeviceRecord:
    """One enrolled device in the operator's inventory."""

    @property
    def device(self) -> bytes: ...
    @property
    def name(self) -> str: ...
    @property
    def tags(self) -> List[str]: ...
    @property
    def enrolled_at(self) -> int: ...
    @property
    def revoked_at(self) -> Optional[int]:
        """Unix-seconds the device was revoked, or ``None`` while active."""
        ...
    @property
    def is_revoked(self) -> bool: ...

class DeviceEnrollment:
    """A device's persisted enrollment — its own key + the ``root -> device``
    grant it received — so it survives restarts without re-pairing. The device
    seed stays in Rust (H8); :attr:`device` hands back an opaque ``Identity``."""

    def __init__(
        self,
        device: Identity,
        chain: DelegationChain,
        rendezvous: str,
        enrolled_at: int,
    ) -> None: ...
    @staticmethod
    def load(path: str) -> Optional["DeviceEnrollment"]:
        """Load a persisted enrollment. ``None`` if none is saved yet; raises on
        a corrupt file."""
        ...
    def save(self, path: str) -> None:
        """Persist to ``path`` (0600, atomic). Overwrites — e.g. after renewal."""
        ...
    def is_valid(self, revocation: RevocationRegistry, skew_seconds: int = 0) -> bool:
        """Whether the grant still verifies + is unexpired. An empty registry is
        fine device-side (the provider enforces revocation on invoke)."""
        ...
    def needs_renewal(self, window_seconds: int, now: int) -> bool:
        """Whether the grant is within ``window_seconds`` of expiry at ``now``."""
        ...
    @property
    def device(self) -> Identity:
        """The device's opaque ``Identity`` handle — extend the grant to a
        gateway with it."""
        ...
    @property
    def chain(self) -> DelegationChain: ...
    @property
    def rendezvous(self) -> str:
        """The operator's rendezvous locator — where the device dials to renew."""
        ...
    @property
    def root(self) -> bytes: ...
    @property
    def enrolled_at(self) -> int: ...
    @property
    def expires_at(self) -> int: ...

class OperatorEnrollment:
    """The operator side: mint invites, approve join requests into
    ``root -> device`` delegations, and manage the device inventory."""

    def __init__(
        self, root: Identity, registry_path: str, revocation_path: str
    ) -> None: ...
    @staticmethod
    def with_default_paths(root: Identity) -> "OperatorEnrollment":
        """Build using the per-user default store paths. Raises if neither
        resolves."""
        ...
    def invite(self, rendezvous: str, ttl_seconds: int) -> InviteToken:
        """Mint an invite valid for ``ttl_seconds``, tracking it so a later
        :meth:`approve` can match a request. ``rendezvous`` is the transport
        locator devices dial."""
        ...
    def approve(
        self, request: JoinRequest, grant_ttl_seconds: int, max_depth: Optional[int] = ...
    ) -> DelegationChain:
        """Approve a request (auto — invite-as-authorization): run the checks,
        record the device, retire the single-use invite, return the
        ``root -> device`` chain. Raises on any rejection."""
        ...
    def handle_join_request(
        self, request_bytes: bytes, grant_ttl_seconds: int, max_depth: Optional[int] = ...
    ) -> bytes:
        """Server-side: turn serialized ``JoinRequest`` bytes into serialized
        ``JoinOutcome`` bytes (auto — invite-as-authorization). Never raises; a
        rejection is a coded ``JoinOutcome``."""
        ...
    def revoke(self, device: bytes) -> None:
        """Revoke a device: raise its revocation floor and stamp the inventory."""
        ...
    def devices(self) -> List[DeviceRecord]:
        """The enrolled devices in the inventory."""
        ...
    def forget(self, device: bytes) -> bool:
        """Prune a device from the inventory (orthogonal to revoking its floor).
        Returns whether a record existed."""
        ...
    def pending_invites(self, now: int) -> List[InviteToken]:
        """Outstanding (minted, unredeemed, unexpired at ``now``) invites."""
        ...
    @property
    def root_id(self) -> bytes: ...
    def root_fingerprint(self) -> str: ...

class EnrollmentServeHandle:
    """Keeps a served enrollment service alive (returned by
    ``NetMesh.serve_enrollment_auto``). Dropping it or calling :meth:`stop`
    unregisters the service."""

    def stop(self) -> None:
        """Stop serving enrollment (unregister the service)."""
        ...
    @property
    def serving(self) -> bool: ...

class LocalPublicationHandle:
    """A live publication of a node's OWN local tools (returned by
    ``NetMesh.publish_tools``). Hold it to keep the tools announced + served."""

    @property
    def tools(self) -> List[str]:
        """The served tool ids (channel-safe)."""
        ...
    @property
    def skipped_tools(self) -> List[str]:
        """Tool names skipped because they had no usable id (an empty name)."""
        ...
    @property
    def serving(self) -> bool:
        """Whether the publication is still live."""
        ...
    def withdraw(self) -> None:
        """Withdraw immediately: re-announce the remaining set so peers stop
        advertising these tools, then stop the services. Idempotent."""
        ...
    def stop(self) -> None:
        """Stop serving (unregister on drop; does not re-announce). Idempotent."""
        ...

class A2aServeHandle:
    """Keeps the served agent-to-agent task services alive (returned by
    ``NetMesh.serve_a2a``). Dropping it or calling :meth:`stop` unregisters
    them."""

    def stop(self) -> None:
        """Stop accepting A2A tasks (unregister the services)."""
        ...
    @property
    def serving(self) -> bool: ...

# =============================================================================
# Stubs for symbols exported by `net._net` at runtime that aren't yet typed
# in detail here. Each class is declared without full method signatures so
# `from net import X` resolves cleanly under mypy / pyright; method-level
# typing happens at runtime via inspect / help(X). For the authoritative
# method surface, see the matching PyO3 source under
# `net/crates/net/bindings/python/src/<file>.rs`.
# =============================================================================

# ---- MeshOS daemon-author SDK (`meshos.rs`) ---------------------------------

class MeshOsSdkError(Exception):
    """MeshOS SDK-level error. Carries a ``.kind`` attribute matching
    the ``<<meshos-sdk-kind:KIND>>`` envelope; see
    ``net_sdk.meshos.meshos_sdk_error_kind``."""

    kind: Optional[str]

class MeshOsDaemonSdk:
    """Entry point for daemon-author code. Construct via
    :py:meth:`start`; use ``register_daemon`` to register a
    Python daemon instance. See
    `net/crates/net/bindings/python/src/meshos.rs` for the
    full method surface."""

    @staticmethod
    def start(
        config: Optional[dict] = None,
        control_capacity: Optional[int] = None,
    ) -> "MeshOsDaemonSdk":
        """Start the SDK with optional config + the substrate's
        ``LoggingDispatcher`` as the action consumer."""
        ...
    def register_daemon(
        self, daemon: Any, identity: "Identity"
    ) -> "MeshOsDaemonHandle":
        """Register a Python daemon under the supplied identity."""
        ...
    def dropped_control_events(self) -> int:
        """Diagnostic counter — total control events the router
        dropped because a daemon's channel was full."""
        ...
    def shutdown(self) -> None:
        """Tear down the wrapped runtime. Consumes the SDK by
        value — subsequent calls raise ``already_shutdown``."""
        ...
    def __enter__(self) -> "MeshOsDaemonSdk": ...
    def __exit__(
        self,
        exc_type: Optional[Any] = None,
        exc_value: Optional[Any] = None,
        exc_traceback: Optional[Any] = None,
    ) -> bool: ...
    def __repr__(self) -> str: ...

class MeshOsDaemonHandle:
    """Operator-side handle to a registered daemon. See `meshos.rs`."""

    @property
    def daemon_id(self) -> int:
        """Substrate identifier (the keypair's origin hash).
        Stable across the handle's lifetime, including after
        shutdown."""
        ...
    @property
    def daemon_name(self) -> str:
        """Daemon's ``name`` at registration. Stable across the
        handle's lifetime, including after shutdown."""
        ...
    def metadata(self) -> dict:
        """Return the cached metadata view as a dict."""
        ...
    def refresh_metadata(self) -> dict:
        """Rebuild the metadata view from the runtime's latest
        snapshot."""
        ...
    def next_control(self, timeout_ms: Optional[int] = None) -> Optional[dict]:
        """Block until the next control event arrives, or
        ``timeout_ms`` elapses, or the runtime shuts down."""
        ...
    def try_next_control(self) -> Optional[dict]:
        """Non-blocking control-event receive. Returns ``None``
        if the channel is empty / closed."""
        ...
    def publish_log(self, level: str, message: str) -> None:
        """Publish a log line tagged with this daemon's id.
        ``level`` is one of ``trace|debug|info|warn|error``."""
        ...
    def publish_capabilities(self, caps: Optional[dict] = None) -> None:
        """Publish (or update) the daemon's capability set."""
        ...
    def graceful_shutdown(self, grace_ms: Optional[int] = None) -> None:
        """Drive a graceful shutdown. Sends ``Shutdown
        { grace_period_ms }`` on the daemon's control channel,
        parks for ``grace_ms``, then unregisters. Consumes the
        handle."""
        ...
    def __enter__(self) -> "MeshOsDaemonHandle": ...
    def __exit__(
        self,
        exc_type: Optional[Any] = None,
        exc_value: Optional[Any] = None,
        exc_traceback: Optional[Any] = None,
    ) -> bool: ...
    def __repr__(self) -> str: ...

# ---- MeshDB query layer (`meshdb.rs`) ---------------------------------------

class MeshDbError(Exception):
    """MeshDB query-layer error. Carries a ``<<meshdb-kind:...>>``
    envelope; see ``net_sdk.meshdb``."""

    kind: Optional[str]

class ResultRow:
    """One row from a query result. ``origin`` is the chain's
    16-hex u64 identifier; ``seq`` is the sequence number;
    ``payload`` is opaque bytes."""

    @property
    def origin(self) -> int: ...
    @property
    def seq(self) -> int: ...
    @property
    def payload(self) -> bytes: ...
    def decode_aggregate(self) -> Optional["AggregateResult"]:
        """Try to decode this row's payload as an aggregate
        payload. Returns ``None`` for non-aggregate rows."""
        ...
    def decode_joined(self) -> Optional["JoinedRow"]:
        """Try to decode this row's payload as a joined-row
        payload. Returns ``None`` when the bytes don't
        deserialize as a JoinedRow."""
        ...
    def decode_window(self) -> Optional["WindowBoundary"]:
        """Try to decode this row's payload as a window bucket.
        Returns ``None`` when the bytes don't deserialize as a
        WindowBoundary."""
        ...
    def __repr__(self) -> str: ...

class AggregateResult:
    """Decoded aggregate-row payload. ``kind`` names which
    aggregate function ran; ``value`` is the numeric output;
    ``count`` mirrors ``value`` as an integer for count-flavored
    kinds."""

    @property
    def group(self) -> Optional["GroupKey"]: ...
    @property
    def kind(self) -> str: ...
    @property
    def value(self) -> Optional[float]: ...
    @property
    def count(self) -> Optional[int]: ...
    def __repr__(self) -> str: ...

class GroupKey:
    """Decoded group-key identifier carried inside an
    ``AggregateResult``. ``kind`` is ``"origin"`` / ``"seq"`` /
    ``"origin_seq"``."""

    @property
    def kind(self) -> str: ...
    @property
    def origin(self) -> Optional[int]: ...
    @property
    def seq(self) -> Optional[int]: ...
    def __repr__(self) -> str: ...

class JoinedRow:
    """Decoded join-row payload. ``left`` / ``right`` are the
    source rows from each side of the join; either side is
    ``None`` for outer-join unmatched rows."""

    @property
    def left(self) -> Optional["ResultRow"]: ...
    @property
    def right(self) -> Optional["ResultRow"]: ...
    def __repr__(self) -> str: ...

class WindowBoundary:
    """Decoded window-bucket payload. ``start`` and ``end`` are
    the bucket's seq bounds (half-open); ``rows`` is the list of
    rows that landed in the bucket."""

    @property
    def start(self) -> int: ...
    @property
    def end(self) -> int: ...
    @property
    def rows(self) -> List["ResultRow"]: ...
    def __repr__(self) -> str: ...

class CachePolicy:
    """Cache config envelope for ``ExecuteOptions``."""

    @staticmethod
    def permanent() -> "CachePolicy":
        """Cache until LRU eviction. Use only for queries whose
        result is immutable under substrate semantics."""
        ...
    @staticmethod
    def time_bound(seconds: float = 5.0) -> "CachePolicy":
        """TTL expiry. Defaults to 5 s."""
        ...
    def __repr__(self) -> str: ...

class ExecuteOptions:
    """Per-execute options. Defaults: ``bypass_cache=False``,
    ``cache_policy=TimeBound(5s)``."""

    def __init__(
        self,
        bypass_cache: bool = False,
        cache_policy: Optional["CachePolicy"] = None,
    ) -> None: ...
    @property
    def bypass_cache(self) -> bool: ...
    def __repr__(self) -> str: ...

class Predicate:
    """MeshDB filter predicate IR (note: distinct from the
    capability-system ``Predicate``). Construct via the static
    factory methods."""

    @staticmethod
    def exists(field: str) -> "Predicate":
        """``field`` is present (any value)."""
        ...
    @staticmethod
    def equals(field: str, value: str) -> "Predicate":
        """``field == value`` (string equality)."""
        ...
    @staticmethod
    def numeric_at_least(field: str, threshold: float) -> "Predicate":
        """``field >= threshold`` (numeric)."""
        ...
    @staticmethod
    def numeric_at_most(field: str, threshold: float) -> "Predicate":
        """``field <= threshold`` (numeric)."""
        ...
    @staticmethod
    def numeric_in_range(field: str, min: float, max: float) -> "Predicate":
        """``min <= field <= max`` (numeric, both bounds inclusive)."""
        ...
    @staticmethod
    def string_prefix(field: str, prefix: str) -> "Predicate":
        """``field.startswith(prefix)``."""
        ...
    @staticmethod
    def string_matches(field: str, pattern: str) -> "Predicate":
        """``pattern in field`` (substring)."""
        ...
    @staticmethod
    def semver_at_least(field: str, version: str) -> "Predicate":
        """``field >= version`` (semver)."""
        ...
    @staticmethod
    def and_(predicates: List["Predicate"]) -> "Predicate":
        """Conjunction. Empty list evaluates to ``True``."""
        ...
    @staticmethod
    def or_(predicates: List["Predicate"]) -> "Predicate":
        """Disjunction. Empty list evaluates to ``False``."""
        ...
    @staticmethod
    def not_(predicate: "Predicate") -> "Predicate":
        """Negation."""
        ...
    def __repr__(self) -> str: ...

class MeshQuery:
    """A planned query AST. Reusable across runners. Construct
    via static factory methods or via ``MeshQuery.builder()``."""

    @staticmethod
    def at(origin: int, seq: int) -> "MeshQuery":
        """Read the event at ``seq`` from chain ``origin``."""
        ...
    @staticmethod
    def between(origin: int, start: int, end: int) -> "MeshQuery":
        """Read events in the half-open seq range
        ``[start, end)`` from chain ``origin``."""
        ...
    @staticmethod
    def latest(origin: int) -> "MeshQuery":
        """Read the tip event from chain ``origin``."""
        ...
    @staticmethod
    def builder() -> "QueryBuilder":
        """Start a fluent builder."""
        ...
    @staticmethod
    def filter(inner: "MeshQuery", predicate: "Predicate") -> "MeshQuery":
        """Filter ``inner``'s rows by ``predicate``."""
        ...
    @staticmethod
    def window(inner: "MeshQuery", size: int) -> "MeshQuery":
        """Tumbling window on ``seq`` with the given bucket
        ``size``."""
        ...
    @staticmethod
    def count(
        inner: "MeshQuery", group_by: Optional[List[str]] = None
    ) -> "MeshQuery":
        """Count rows."""
        ...
    @staticmethod
    def sum(
        inner: "MeshQuery",
        field: str,
        group_by: Optional[List[str]] = None,
    ) -> "MeshQuery":
        """Sum of a numeric field across rows."""
        ...
    @staticmethod
    def avg(
        inner: "MeshQuery",
        field: str,
        group_by: Optional[List[str]] = None,
    ) -> "MeshQuery":
        """Arithmetic mean across rows whose field resolves to
        a number."""
        ...
    @staticmethod
    def min(
        inner: "MeshQuery",
        field: str,
        group_by: Optional[List[str]] = None,
    ) -> "MeshQuery": ...
    @staticmethod
    def max(
        inner: "MeshQuery",
        field: str,
        group_by: Optional[List[str]] = None,
    ) -> "MeshQuery": ...
    @staticmethod
    def percentile(
        inner: "MeshQuery",
        field: str,
        p: float,
        group_by: Optional[List[str]] = None,
    ) -> "MeshQuery":
        """Nearest-rank exact percentile at ``p`` in
        ``[0.0, 1.0]``."""
        ...
    @staticmethod
    def distinct_count(
        inner: "MeshQuery",
        field: str,
        group_by: Optional[List[str]] = None,
    ) -> "MeshQuery":
        """Exact distinct count over the canonical string
        projection of a field."""
        ...
    @staticmethod
    def lineage_emit(
        origin: int, entries: List["LineageEntry"], direction: str
    ) -> "MeshQuery":
        """Emit a pre-walked lineage as one ``ResultRow`` per
        entry. ``direction`` is ``"back"`` or ``"forward"``."""
        ...
    @staticmethod
    def join(
        left: "MeshQuery",
        right: "MeshQuery",
        kind: str,
        key: str,
        strategy: Optional[str] = None,
        watermark_secs: float = 5.0,
    ) -> "MeshQuery":
        """Inner / outer hash-join over row-intrinsic or JSON
        payload keys."""
        ...
    def __repr__(self) -> str: ...

class QueryBuilder:
    """Fluent builder for the common-ops query shape. Chain
    ``.at`` / ``.between`` / ``.latest`` to seed; ``.filter`` /
    ``.count`` / aggregates / ``.window`` / ``.join`` to
    compose; ``.build()`` to consume into a ``MeshQuery``."""

    def at(self, origin: int, seq: int) -> "QueryBuilder":
        """Source: read a single event at ``seq``."""
        ...
    def between(
        self, origin: int, start: int, end: int
    ) -> "QueryBuilder":
        """Source: read events in the half-open seq range."""
        ...
    def latest(self, origin: int) -> "QueryBuilder":
        """Source: read the tip event."""
        ...
    def filter(self, predicate: "Predicate") -> "QueryBuilder":
        """Filter the current pipeline's rows by ``predicate``."""
        ...
    def count(
        self, group_by: Optional[List[str]] = None
    ) -> "QueryBuilder": ...
    def sum(
        self, field: str, group_by: Optional[List[str]] = None
    ) -> "QueryBuilder": ...
    def avg(
        self, field: str, group_by: Optional[List[str]] = None
    ) -> "QueryBuilder": ...
    def min(
        self, field: str, group_by: Optional[List[str]] = None
    ) -> "QueryBuilder": ...
    def max(
        self, field: str, group_by: Optional[List[str]] = None
    ) -> "QueryBuilder": ...
    def percentile(
        self,
        field: str,
        p: float,
        group_by: Optional[List[str]] = None,
    ) -> "QueryBuilder": ...
    def distinct_count(
        self, field: str, group_by: Optional[List[str]] = None
    ) -> "QueryBuilder": ...
    def window(self, size: int) -> "QueryBuilder":
        """Tumbling window on ``seq`` over the current pipeline."""
        ...
    def join(
        self,
        right: "MeshQuery",
        kind: str,
        key: str,
        strategy: Optional[str] = None,
        watermark_secs: float = 5.0,
    ) -> "QueryBuilder": ...
    def build(self) -> "MeshQuery":
        """Terminal: consume the builder into a ``MeshQuery``."""
        ...
    def __repr__(self) -> str: ...

class LineageEntry:
    """One chain reached during a lineage walk. Hand to
    ``MeshQuery.lineage_emit(...)``."""

    def __init__(
        self, origin: int, depth: int, tip_seq: Optional[int] = None
    ) -> None: ...
    @property
    def origin(self) -> int: ...
    @property
    def depth(self) -> int: ...
    @property
    def tip_seq(self) -> Optional[int]: ...
    def __repr__(self) -> str: ...

class InMemoryChainReader:
    """In-process ``ChainReader`` Python wrapper. Slice 1 ships
    a simple in-memory variant; ``.append(origin, seq, payload)``
    populates it. Pass to ``MeshQueryRunner``."""

    def __init__(self) -> None: ...
    def append(self, origin: int, seq: int, payload: bytes) -> None:
        """Append a single event to the in-memory store."""
        ...
    def latest_seq(self, origin: int) -> Optional[int]:
        """Tip of chain ``origin``, or ``None`` if unknown."""
        ...
    def __repr__(self) -> str: ...

class MeshQueryRunner:
    """Drives query execution against an ``InMemoryChainReader``.
    Sync-drain by design — Python is sync-first."""

    def __init__(
        self, reader: "InMemoryChainReader", enable_cache: bool = False
    ) -> None: ...
    def execute(
        self,
        query: "MeshQuery",
        options: Optional["ExecuteOptions"] = None,
    ) -> List["ResultRow"]:
        """Execute ``query`` synchronously. Returns the full row
        list."""
        ...

# ---- Dataforts blob surface (`blob.rs`) -------------------------------------

class BlobError(Exception):
    """Dataforts blob-layer error."""

class BlobRef:
    """Content-addressed pointer to a blob payload (substrate-resolved)."""

class MeshBlobAdapter:
    """Substrate-owned blob adapter; publish / resolve through the mesh."""

def register_blob_adapter(adapter_id: str, adapter: object) -> None: ...
def register_filesystem_blob_adapter(adapter_id: str, root: str) -> None: ...
def unregister_blob_adapter(adapter_id: str) -> None: ...
def blob_adapter_ids() -> List[str]: ...
def blob_adapter_registered(adapter_id: str) -> bool: ...
def blob_publish(adapter_id: str, payload: bytes) -> bytes: ...
def blob_resolve(blob_ref: bytes) -> bytes: ...

# ---- Compute / daemon runtime (`compute.rs`) --------------------------------

class DaemonError(Exception):
    """Daemon-runtime error."""

class MigrationError(Exception):
    """Live-migration error. Carries a ``<<daemon: migration: KIND>>``
    envelope; see ``net_sdk.compute.migration_error_kind``."""

class CausalEvent:
    """A causal event delivered to a daemon's ``process`` method."""

    origin_hash: int
    sequence: int
    payload: bytes

class DaemonRuntime:
    """Substrate-owned daemon supervisor. ``spawn`` / ``spawn_from_snapshot``
    / ``snapshot`` / ``shutdown``."""

class DaemonHandle:
    """Handle to a spawned daemon."""

class MigrationHandle:
    """Handle to an in-flight live-migration."""

# ---- Groups (`groups.rs`) ---------------------------------------------------

class GroupError(Exception):
    """Group orchestration error."""

class ReplicaGroup:
    """N-replica HA group. Each member sees every event."""

class ForkGroup:
    """Fork-style group with deterministic event partitioning."""

class StandbyGroup:
    """Active/standby group with leader election."""

# ---- Deck operator SDK (`deck.rs`) ------------------------------------------

class DeckSdkError(Exception):
    """Deck operator SDK error. Carries a ``<<deck-sdk-kind:KIND>>``
    envelope; see ``net_sdk.deck.deck_sdk_error_kind``."""

    kind: Optional[str]

class OperatorIdentity:
    """Operator credential bundle. Construct via ``generate()``
    (tests) or ``from_seed(bytes)`` (production loads)."""

    @staticmethod
    def generate() -> "OperatorIdentity":
        """Generate a fresh operator identity."""
        ...
    @staticmethod
    def from_seed(seed: bytes) -> "OperatorIdentity":
        """Load from a 32-byte ed25519 seed."""
        ...
    @staticmethod
    def from_identity(identity: "Identity") -> "OperatorIdentity":
        """Build from an existing ``net.Identity``."""
        ...
    @property
    def operator_id(self) -> int:
        """64-bit operator identifier (the keypair's origin hash)."""
        ...
    def public_key(self) -> bytes:
        """32-byte ed25519 public key."""
        ...
    def sign_proposal(
        self, simulated: "SimulatedIceProposal"
    ) -> dict:
        """Sign a simulated ICE proposal. Returns
        ``{"operator_id": int, "signature": bytes}``."""
        ...
    def sign_payload(self, payload: bytes) -> dict:
        """Sign raw payload bytes with this operator's ed25519
        key."""
        ...
    def __repr__(self) -> str: ...

class DeckClient:
    """Operator-side client for the cluster's admin / snapshot /
    log / failure / ICE surfaces."""

    def __init__(
        self,
        operator_seed: bytes,
        meshos_config: Optional[dict] = None,
        deck_config: Optional[dict] = None,
    ) -> None:
        """Construct a deck client owning a private supervisor
        runtime. ``operator_seed`` must be exactly 32 bytes."""
        ...
    @staticmethod
    def from_meshos(
        sdk: "MeshOsDaemonSdk",
        identity: "OperatorIdentity",
        config: Optional[dict] = None,
    ) -> "DeckClient":
        """Construct against a running ``MeshOsDaemonSdk``."""
        ...
    def identity(self) -> "OperatorIdentity":
        """Operator identity bound to this client."""
        ...
    def close(self) -> None:
        """Tear down the private supervisor runtime if this
        client owns one. Idempotent."""
        ...
    @property
    def admin(self) -> "AdminCommands":
        """Typed admin-event surface."""
        ...
    def status(self) -> str:
        """One-shot read of the latest ``MeshOsSnapshot`` as a
        JSON string."""
        ...
    def status_summary(self) -> dict:
        """One-shot read of the rolled-up ``StatusSummary``."""
        ...
    def snapshots(self) -> "SnapshotStream":
        """Live snapshot stream — sync iterator over
        JSON-encoded ``MeshOsSnapshot`` strings."""
        ...
    def status_summary_stream(self) -> "StatusSummaryStream":
        """Live ``StatusSummary`` stream — sync iterator over
        typed dicts."""
        ...
    @property
    def ice(self) -> "IceCommands":
        """Break-glass surface."""
        ...
    def audit(self) -> Any:
        """Audit query builder over the in-memory admin-audit
        ring."""
        ...
    def subscribe_logs(self, filter: Optional[dict] = None) -> Any:
        """Subscribe to per-daemon / per-node log lines."""
        ...
    def subscribe_failures(self, since_seq: int = 0) -> Any:
        """Subscribe to executor failure records starting from
        ``since_seq + 1``."""
        ...
    def __repr__(self) -> str: ...

class AdminCommands:
    """Admin command dispatcher exposed via ``DeckClient.admin``.
    Each method commits an ``AdminEvent`` variant + returns a
    ``ChainCommit`` dict."""

    def drain(self, node: int, drain_for_ms: int) -> dict:
        """Drain a node — start draining workloads."""
        ...
    def enter_maintenance(
        self, node: int, drain_for_ms: Optional[int] = None
    ) -> dict:
        """Enter maintenance mode on a node."""
        ...
    def exit_maintenance(self, node: int) -> dict: ...
    def cordon(self, node: int) -> dict: ...
    def uncordon(self, node: int) -> dict: ...
    def drop_replicas(self, node: int, chains: List[int]) -> dict: ...
    def invalidate_placement(self, node: int) -> dict: ...
    def restart_all_daemons(self, node: int) -> dict: ...
    def clear_avoid_list(self, node: int) -> dict: ...

class SnapshotStream:
    """Live ``MeshOsSnapshot`` stream as a Python sync iterator.
    Each ``__next__`` blocks until the next snapshot publishes.
    Slice 1 returns JSON strings."""

    def __iter__(self) -> "SnapshotStream": ...
    def __next__(self) -> str: ...
    def close(self) -> None:
        """Close the stream explicitly. Subsequent ``__next__``
        calls raise ``StopIteration``. Idempotent."""
        ...

class StatusSummaryStream:
    """Live ``StatusSummary`` stream — sync iterator over typed
    dicts."""

    def __iter__(self) -> "StatusSummaryStream": ...
    def __next__(self) -> dict: ...
    def close(self) -> None: ...

class IceCommands:
    """ICE break-glass command dispatcher. Each factory returns
    an ``IceProposal`` that must be ``simulate()``-d before
    commit per the typestate contract."""

    def freeze_cluster(self, ttl_ms: int) -> "IceProposal": ...
    def flush_avoid_lists(self, scope: dict) -> "IceProposal": ...
    def force_evict_replica(
        self, chain: int, victim: int
    ) -> "IceProposal": ...
    def force_restart_daemon(
        self, id: int, name: str
    ) -> "IceProposal":
        """Propose force-restarting a daemon. ``id`` is the
        registry-local daemon id; ``name`` is
        ``MeshDaemon::name()``."""
        ...
    def force_cutover(
        self, chain: int, target: int
    ) -> "IceProposal": ...
    def kill_migration(self, migration: int) -> "IceProposal": ...
    def thaw_cluster(self) -> "IceProposal": ...

class IceProposal:
    """ICE break-glass proposal envelope. Pre-simulation
    typestate. Has no ``commit`` method — must be ``simulate()``-d
    first."""

    @property
    def issued_at_ms(self) -> int: ...
    def simulate(self) -> "SimulatedIceProposal":
        """Pre-execution preview. Consumes the proposal —
        subsequent calls raise
        ``DeckSdkError(kind="already_simulated")``."""
        ...
    def __repr__(self) -> str: ...

class SimulatedIceProposal:
    """Dry-run ICE proposal. Only class exposing ``commit``."""

    def blast_radius(self) -> str:
        """Pre-execution blast-radius preview as a JSON string."""
        ...
    @property
    def issued_at_ms(self) -> int: ...
    def blast_hash(self) -> bytes:
        """Blake3 digest of the blast radius. Signers must
        cover this exact hash."""
        ...
    def signing_payload(self) -> bytes:
        """Deterministic signing payload bytes the verifier will
        reconstruct."""
        ...
    def commit(self, signatures: List[dict]) -> dict:
        """Commit the proposal with the supplied operator
        signatures. Each signature is a dict
        ``{"operator_id": int, "signature": bytes}``."""
        ...
    def __repr__(self) -> str: ...

class OperatorRegistry:
    """Operator policy registry. Holds known operator public
    keys keyed by 64-bit operator id."""

    def __init__(self) -> None: ...
    def insert(self, operator_id: int, public_key: bytes) -> None:
        """Insert an operator's 32-byte ed25519 public key under
        ``operator_id``."""
        ...
    def register(self, identity: "OperatorIdentity") -> None:
        """Register ``identity``'s public key under its derived
        operator id."""
        ...
    def contains(self, operator_id: int) -> bool:
        """``True`` iff ``operator_id`` is registered."""
        ...
    def is_empty(self) -> bool: ...
    def verify(self, signature: dict, payload: bytes) -> None:
        """Verify a single ``OperatorSignature`` dict over
        ``payload``."""
        ...
    def verify_bundle(
        self,
        signatures: List[dict],
        payload: bytes,
        threshold: int,
    ) -> None:
        """Verify every signature in the bundle over ``payload``
        and confirm at least ``threshold`` distinct operator
        ids signed it."""
        ...
    def __contains__(self, operator_id: int) -> bool: ...
    def __len__(self) -> int: ...
    def __repr__(self) -> str: ...

class AdminVerifier:
    """Substrate-side admin commit verifier. Bundles an
    ``OperatorRegistry`` snapshot with the cluster's signature
    threshold + freshness / skew / ICE-cooldown windows."""

    def __init__(
        self, registry: "OperatorRegistry", threshold: int
    ) -> None: ...
    @staticmethod
    def with_freshness(
        registry: "OperatorRegistry",
        threshold: int,
        freshness_window_ms: int,
        future_skew_ms: int,
    ) -> "AdminVerifier":
        """Build with explicit freshness + future-skew windows
        and the default ICE cooldown."""
        ...
    @staticmethod
    def with_full_policy(
        registry: "OperatorRegistry",
        threshold: int,
        freshness_window_ms: int,
        future_skew_ms: int,
        ice_cooldown_ms: int,
    ) -> "AdminVerifier":
        """Build with every policy knob explicit."""
        ...
    @property
    def threshold(self) -> int: ...
    @property
    def freshness_window_ms(self) -> int: ...
    @property
    def future_skew_ms(self) -> int: ...
    @property
    def ice_cooldown_ms(self) -> int: ...
    def __repr__(self) -> str: ...

# ---- Redis Streams dedup (`redis_dedup.rs`) ---------------------------------

class RedisStreamDedup:
    """LRU-bounded dedup helper for Redis Streams consumers."""

# ---- Aggregator-registry RPC clients (`aggregator.rs`) ----------------------

class RegistryClientError(Exception):
    """Aggregator registry RPC failure.

    ``kind`` is one of ``"transport"`` | ``"codec"`` |
    ``"unknown-template"`` | ``"duplicate-group-name"`` |
    ``"spawn-rejected"`` | ``"spawn-not-supported"``.
    ``server_detail`` carries the long-form text.
    """
    kind: str
    server_detail: str | None

class UnknownTemplate(RegistryClientError): ...
class DuplicateGroupName(RegistryClientError): ...
class SpawnRejected(RegistryClientError): ...
class SpawnNotSupported(RegistryClientError): ...

class FoldQueryClientError(Exception):
    """Fold-query RPC failure.

    ``kind`` is one of ``"transport"`` | ``"codec"`` |
    ``"unknown-kind"``. ``server_detail`` carries the long-form
    text.
    """
    kind: str
    server_detail: str | None

class UnknownFoldKind(FoldQueryClientError): ...

class RegistryClient:
    """Client for the ``aggregator.registry`` RPC service."""

    def __init__(self, mesh: NetMesh) -> None: ...
    def with_deadline(self, millis: int) -> RegistryClient: ...
    def list(self, target_node_id: int) -> List[Dict[str, Any]]: ...
    def spawn(
        self,
        target_node_id: int,
        template_name: str,
        group_name: str,
        replica_count: int,
    ) -> Dict[str, Any]: ...
    def unregister(self, target_node_id: int, group_name: str) -> bool: ...
    def __repr__(self) -> str: ...

class FoldQueryClient:
    """Client for the ``fold.query`` RPC service with a TTL cache."""

    def __init__(self, mesh: NetMesh) -> None: ...
    def with_ttl(self, millis: int) -> FoldQueryClient: ...
    def with_deadline(self, millis: int) -> FoldQueryClient: ...
    def query_latest(
        self, target_node_id: int, kind: int
    ) -> List[Dict[str, Any]]: ...
    def query_summarize_now(
        self, target_node_id: int, kind: int
    ) -> List[Dict[str, Any]]: ...
    def invalidate_cache(self) -> None: ...
    def invalidate_target(self, target_node_id: int) -> None: ...
    def __repr__(self) -> str: ...

# =============================================================================
# Async siblings — D-3.
#
# Each Async* class wraps the same Arc<...> as its sync counterpart.
# Methods marked ``async`` here return Python awaitables (the runtime
# uses ``pyo3-async-runtimes`` under the hood, so the same coroutine
# protocol applies). Stubs intentionally lean on the bare ``async def``
# form for IDE completion; the actual return values match the sync
# siblings' shapes.
# =============================================================================

from typing import AsyncIterator, Awaitable, Callable, Tuple, Union, overload

# ----- T1: net mesh + streams -----

class AsyncNetStream:
    """Async sibling of :class:`NetStream`. Same handle; awaitable sends."""

    @property
    def peer_node_id(self) -> int: ...
    @property
    def stream_id(self) -> int: ...
    def __repr__(self) -> str: ...
    async def send(self, events: List[bytes]) -> None: ...
    async def send_with_retry(
        self, events: List[bytes], max_retries: int = 8
    ) -> None: ...
    async def send_blocking(self, events: List[bytes]) -> None: ...

class AsyncNetMesh:
    """Async sibling of :class:`NetMesh`. Shares the same MeshNode."""

    def __init__(self, mesh: NetMesh) -> None: ...
    @property
    def public_key(self) -> str: ...
    @property
    def entity_id(self) -> bytes: ...
    @property
    def node_id(self) -> int: ...
    def peer_count(self) -> int: ...
    def discovered_nodes(self) -> int: ...
    def start(self) -> None: ...
    async def connect(
        self, peer_addr: str, peer_public_key: str, peer_node_id: int
    ) -> None: ...
    async def accept(self, peer_node_id: int) -> str: ...
    async def push_to(self, peer_addr: str, json: str) -> bool: ...
    async def poll(self, limit: int) -> List[StoredEvent]: ...
    async def subscribe_channel(
        self,
        publisher_node_id: int,
        channel: str,
        token: Optional[bytes] = None,
    ) -> None: ...
    async def unsubscribe_channel(
        self, publisher_node_id: int, channel: str
    ) -> None: ...
    async def publish(
        self,
        channel: str,
        payload: bytes,
        *,
        reliability: Optional[str] = None,
        on_failure: Optional[str] = None,
        max_inflight: Optional[int] = None,
    ) -> Dict[str, Any]: ...
    async def announce_capabilities(self, caps: Dict[str, Any]) -> None: ...
    def open_stream(
        self,
        peer_node_id: int,
        stream_id: int,
        reliability: Optional[str] = None,
        window_bytes: int = ...,
        fairness_weight: int = 1,
    ) -> AsyncNetStream: ...
    def close_stream(self, peer_node_id: int, stream_id: int) -> None: ...
    def stream_stats(
        self, peer_node_id: int, stream_id: int
    ) -> Optional[NetStreamStats]: ...
    def find_nodes(self, filter: Dict[str, Any]) -> List[int]: ...
    async def shutdown(self) -> None: ...
    def __repr__(self) -> str: ...

# ----- T1: mesh_rpc -----

class AsyncRpcStream:
    """PEP 525 async iterator over an nRPC server-streaming call."""

    def __aiter__(self) -> AsyncRpcStream: ...
    async def __anext__(self) -> bytes: ...
    def grant(self, n: int) -> None: ...
    def flow_controlled(self) -> bool: ...
    def close(self) -> None: ...
    async def aclose(self) -> None: ...

class AsyncClientStreamCall:
    """Async client-streaming call handle."""

    async def send(self, body: bytes) -> None: ...
    async def finish(self) -> bytes: ...
    def call_id(self) -> int: ...
    def flow_controlled(self) -> bool: ...
    def close(self) -> None: ...
    async def aclose(self) -> None: ...

class AsyncDuplexSink:
    """Send half of an async duplex call."""

    async def send(self, body: bytes) -> None: ...
    async def finish_sending(self) -> None: ...

class AsyncDuplexStream:
    """Receive half of an async duplex call. PEP 525 async iterator."""

    def __aiter__(self) -> AsyncDuplexStream: ...
    async def __anext__(self) -> bytes: ...

class AsyncDuplexCall:
    """Combined async duplex call. ``into_split()`` peels sink + stream halves."""

    def into_split(self) -> Tuple[AsyncDuplexSink, AsyncDuplexStream]: ...
    def call_id(self) -> int: ...
    def flow_controlled(self) -> bool: ...

class AsyncMeshRpc:
    """Async sibling of :class:`MeshRpc`."""

    def __init__(self, mesh: NetMesh) -> None: ...
    def serve(
        self,
        service: str,
        handler: Callable[[bytes], Any],
        handler_timeout_ms: Optional[int] = None,
    ) -> ServeHandle: ...
    async def call(
        self,
        target_node_id: int,
        service: str,
        request: bytes,
        opts: Optional[Dict[str, Any]] = None,
    ) -> bytes: ...
    async def call_service(
        self,
        service: str,
        request: bytes,
        opts: Optional[Dict[str, Any]] = None,
    ) -> bytes: ...
    def find_service_nodes(self, service: str) -> List[int]: ...
    async def call_streaming(
        self,
        target_node_id: int,
        service: str,
        request: bytes,
        opts: Optional[Dict[str, Any]] = None,
    ) -> AsyncRpcStream: ...
    async def call_client_stream(
        self,
        target_node_id: int,
        service: str,
        opts: Optional[Dict[str, Any]] = None,
    ) -> AsyncClientStreamCall: ...
    async def call_duplex(
        self,
        target_node_id: int,
        service: str,
        opts: Optional[Dict[str, Any]] = None,
    ) -> AsyncDuplexCall: ...
    def serve_client_stream(
        self,
        service: str,
        handler: Callable[..., Any],
        handler_timeout_ms: Optional[int] = None,
    ) -> ServeHandle: ...
    def serve_duplex(
        self,
        service: str,
        handler: Callable[..., Any],
        handler_timeout_ms: Optional[int] = None,
    ) -> ServeHandle: ...

# ----- T2: cortex -----

class AsyncMemoryWatchIter:
    """PEP 525 async iterator over a memories watch."""

    def __aiter__(self) -> AsyncMemoryWatchIter: ...
    async def __anext__(self) -> List[Memory]: ...
    def close(self) -> None: ...
    async def aclose(self) -> None: ...

class AsyncMemoriesAdapter:
    """Async sibling of :class:`MemoriesAdapter`."""

    def __init__(self, memories: MemoriesAdapter) -> None: ...
    def snapshot(self) -> Tuple[bytes, Optional[int]]: ...
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
    async def wait_for_seq(self, seq: int) -> None: ...
    async def wait_for_token(
        self, token: WriteToken, deadline_ms: int = 1000
    ) -> None: ...
    def close(self) -> None: ...
    def is_running(self) -> bool: ...
    def count(self) -> int: ...
    def list_memories(self, **kwargs: Any) -> List[Memory]: ...
    async def watch_memories(self, **kwargs: Any) -> AsyncMemoryWatchIter: ...
    async def snapshot_and_watch_memories(
        self, **kwargs: Any
    ) -> Tuple[List[Memory], AsyncMemoryWatchIter]: ...

class AsyncTaskWatchIter:
    """PEP 525 async iterator over a tasks watch."""

    def __aiter__(self) -> AsyncTaskWatchIter: ...
    async def __anext__(self) -> List[Task]: ...
    def close(self) -> None: ...
    async def aclose(self) -> None: ...

class AsyncTasksAdapter:
    """Async sibling of :class:`TasksAdapter`."""

    def __init__(self, tasks: TasksAdapter) -> None: ...
    def snapshot(self) -> Tuple[bytes, Optional[int]]: ...
    def create(self, id: int, title: str, now_ns: int) -> int: ...
    def rename(self, id: int, new_title: str, now_ns: int) -> int: ...
    def complete(self, id: int, now_ns: int) -> int: ...
    def delete(self, id: int) -> int: ...
    async def wait_for_seq(self, seq: int) -> None: ...
    async def wait_for_token(
        self, token: WriteToken, deadline_ms: int = 1000
    ) -> None: ...
    def close(self) -> None: ...
    def is_running(self) -> bool: ...
    def count(self) -> int: ...
    def list_tasks(self, **kwargs: Any) -> List[Task]: ...
    async def watch_tasks(self, **kwargs: Any) -> AsyncTaskWatchIter: ...
    async def snapshot_and_watch_tasks(
        self, **kwargs: Any
    ) -> Tuple[List[Task], AsyncTaskWatchIter]: ...

class AsyncRedexTailIter:
    """PEP 525 async iterator over a Redex file tail."""

    def __aiter__(self) -> AsyncRedexTailIter: ...
    async def __anext__(self) -> RedexEvent: ...
    def close(self) -> None: ...
    async def aclose(self) -> None: ...

class AsyncRedexFile:
    """Async sibling of :class:`RedexFile`."""

    def __init__(self, file: RedexFile) -> None: ...
    def append(self, payload: bytes) -> int: ...
    def append_batch(self, payloads: List[bytes]) -> Optional[int]: ...
    def read_range(self, start: int, end: int) -> List[RedexEvent]: ...
    def __len__(self) -> int: ...
    def sync(self) -> None: ...
    def close(self) -> None: ...
    async def tail(self, from_seq: int = 0) -> AsyncRedexTailIter: ...

# ----- T2: compute -----

class AsyncMigrationHandle:
    """Async sibling of :class:`MigrationHandle`."""

    @property
    def origin_hash(self) -> int: ...
    @property
    def source_node(self) -> int: ...
    @property
    def target_node(self) -> int: ...
    def phase(self) -> Optional[str]: ...
    async def wait(self) -> None: ...
    async def wait_with_timeout(self, timeout_ms: int) -> None: ...
    async def cancel(self) -> None: ...
    def phases(self) -> MigrationPhasesIter: ...
    def __repr__(self) -> str: ...

class AsyncDaemonRuntime:
    """Async sibling of :class:`DaemonRuntime`."""

    def __init__(self, rt: DaemonRuntime) -> None: ...
    def is_ready(self) -> bool: ...
    def daemon_count(self) -> int: ...
    def register_factory(
        self, kind: str, factory: Callable[[], Any]
    ) -> None: ...
    async def start(self) -> None: ...
    async def shutdown(self) -> None: ...
    async def spawn(
        self,
        kind: str,
        identity: Identity,
        config: Optional[Dict[str, Any]] = None,
    ) -> DaemonHandle: ...
    async def spawn_from_snapshot(
        self,
        kind: str,
        identity: Identity,
        snapshot_bytes: bytes,
        config: Optional[Dict[str, Any]] = None,
    ) -> DaemonHandle: ...
    async def stop(self, origin_hash: int) -> None: ...
    async def snapshot(self, origin_hash: int) -> Optional[bytes]: ...
    async def deliver(
        self, origin_hash: int, event: CausalEvent
    ) -> List[bytes]: ...
    async def start_migration(
        self, origin_hash: int, source_node: int, target_node: int
    ) -> AsyncMigrationHandle: ...
    async def start_migration_with(
        self,
        origin_hash: int,
        source_node: int,
        target_node: int,
        opts: Dict[str, Any],
    ) -> AsyncMigrationHandle: ...
    def expect_migration(
        self,
        kind: str,
        origin_hash: int,
        config: Optional[Dict[str, Any]] = None,
    ) -> None: ...
    def register_migration_target_identity(
        self,
        kind: str,
        identity: Identity,
        config: Optional[Dict[str, Any]] = None,
    ) -> None: ...
    def migration_phase(self, origin_hash: int) -> Optional[str]: ...

# ----- T2: blob -----

class AsyncMeshBlobAdapter:
    """Async sibling of :class:`MeshBlobAdapter`."""

    def __init__(self, adapter: MeshBlobAdapter) -> None: ...
    def prometheus_text(self) -> str: ...
    def tree_node_cache_stats(self) -> Optional[Tuple[int, int, int, int]]: ...
    async def store(self, blob_ref: BlobRef, data: bytes) -> None: ...
    async def fetch(self, blob_ref: BlobRef) -> bytes: ...
    async def fetch_range(
        self, blob_ref: BlobRef, start: int, end: int
    ) -> bytes: ...
    async def exists(self, blob_ref: BlobRef) -> bool: ...
    async def repair_blob(
        self, blob_ref: BlobRef
    ) -> Tuple[int, int, int, int, int, int, int]: ...

async def async_blob_publish(
    adapter_id: str, uri: str, data: bytes
) -> bytes: ...
async def async_blob_resolve(adapter_id: str, payload: bytes) -> bytes: ...

# ----- T3: aggregator -----

class AsyncRegistryClient:
    """Async sibling of :class:`RegistryClient`."""

    def __init__(self, client: RegistryClient) -> None: ...
    @staticmethod
    def from_mesh(mesh: NetMesh) -> AsyncRegistryClient: ...
    def with_deadline(self, millis: int) -> AsyncRegistryClient: ...
    async def list(self, target_node_id: int) -> List[Dict[str, Any]]: ...
    async def spawn(
        self,
        target_node_id: int,
        template_name: str,
        group_name: str,
        replica_count: int,
    ) -> Dict[str, Any]: ...
    async def unregister(
        self, target_node_id: int, group_name: str
    ) -> bool: ...

class AsyncFoldQueryClient:
    """Async sibling of :class:`FoldQueryClient`."""

    def __init__(self, client: FoldQueryClient) -> None: ...
    @staticmethod
    def from_mesh(mesh: NetMesh) -> AsyncFoldQueryClient: ...
    def with_ttl(self, millis: int) -> AsyncFoldQueryClient: ...
    def with_deadline(self, millis: int) -> AsyncFoldQueryClient: ...
    async def query_latest(
        self, target_node_id: int, kind: int
    ) -> List[Dict[str, Any]]: ...
    async def query_summarize_now(
        self, target_node_id: int, kind: int
    ) -> List[Dict[str, Any]]: ...
    def invalidate_cache(self) -> None: ...
    def invalidate_target(self, target_node_id: int) -> None: ...

# ----- T3: meshdb -----

class AsyncMeshQueryRunner:
    """Async sibling of :class:`MeshQueryRunner`."""

    def __init__(
        self, reader: InMemoryChainReader, enable_cache: bool = False
    ) -> None: ...
    @staticmethod
    def from_sync(runner: MeshQueryRunner) -> AsyncMeshQueryRunner: ...
    async def execute(
        self, query: MeshQuery, options: Optional[ExecuteOptions] = None
    ) -> List[ResultRow]: ...

# ----- T3: deck -----

class AsyncSnapshotStream:
    """PEP 525 async sibling of :class:`SnapshotStream`."""

    @staticmethod
    def from_sync(stream: SnapshotStream) -> AsyncSnapshotStream: ...
    def __aiter__(self) -> AsyncSnapshotStream: ...
    async def __anext__(self) -> str: ...
    def close(self) -> None: ...
    async def aclose(self) -> None: ...

class AsyncStatusSummaryStream:
    """PEP 525 async sibling of :class:`StatusSummaryStream`."""

    @staticmethod
    def from_sync(stream: StatusSummaryStream) -> AsyncStatusSummaryStream: ...
    def __aiter__(self) -> AsyncStatusSummaryStream: ...
    async def __anext__(self) -> Dict[str, Any]: ...
    def close(self) -> None: ...
    async def aclose(self) -> None: ...

class AsyncAdminCommands:
    """Async sibling of :class:`AdminCommands`."""

    async def drain(self, node: int, drain_for_ms: int) -> Dict[str, Any]: ...
    async def enter_maintenance(
        self, node: int, drain_for_ms: Optional[int] = None
    ) -> Dict[str, Any]: ...
    async def exit_maintenance(self, node: int) -> Dict[str, Any]: ...
    async def cordon(self, node: int) -> Dict[str, Any]: ...
    async def uncordon(self, node: int) -> Dict[str, Any]: ...
    async def drop_replicas(
        self, node: int, chains: List[int]
    ) -> Dict[str, Any]: ...
    async def invalidate_placement(self, node: int) -> Dict[str, Any]: ...
    async def restart_all_daemons(self, node: int) -> Dict[str, Any]: ...
    async def clear_avoid_list(self, node: int) -> Dict[str, Any]: ...

class AsyncDeckClient:
    """Async sibling of :class:`DeckClient`."""

    def __init__(self, client: DeckClient) -> None: ...
    @staticmethod
    def from_seed(
        operator_seed: bytes,
        meshos_config: Optional[Dict[str, Any]] = None,
        deck_config: Optional[Dict[str, Any]] = None,
    ) -> AsyncDeckClient: ...
    def identity(self) -> OperatorIdentity: ...
    def status(self) -> str: ...
    def status_summary(self) -> Dict[str, Any]: ...
    def snapshots(self) -> AsyncSnapshotStream: ...
    def status_summary_stream(self) -> AsyncStatusSummaryStream: ...
    @property
    def admin(self) -> AsyncAdminCommands: ...
    @property
    def ice(self) -> AsyncIceCommands: ...
    async def close(self) -> None: ...
    def __repr__(self) -> str: ...

class AsyncMeshOsDaemonHandle:
    """Async sibling of :class:`MeshOsDaemonHandle`. Constructed
    via :meth:`from_sync`; consumes the sync handle's inner."""

    @staticmethod
    def from_sync(handle: MeshOsDaemonHandle) -> AsyncMeshOsDaemonHandle: ...
    @property
    def daemon_id(self) -> int: ...
    @property
    def daemon_name(self) -> str: ...
    def try_next_control(self) -> Optional[Dict[str, Any]]: ...
    async def next_control(
        self, timeout_ms: Optional[int] = None
    ) -> Optional[Dict[str, Any]]: ...
    def publish_log(self, level: str, message: str) -> None: ...
    def publish_capabilities(
        self, caps: Optional[Dict[str, Any]] = None
    ) -> None: ...
    async def graceful_shutdown(
        self, grace_ms: Optional[int] = None
    ) -> None: ...
    def __repr__(self) -> str: ...

class AsyncMeshOsDaemonSdk:
    """Async sibling of :class:`MeshOsDaemonSdk`. Constructed via
    :meth:`from_sync`; consumes the sync SDK's inner."""

    @staticmethod
    def from_sync(sdk: MeshOsDaemonSdk) -> AsyncMeshOsDaemonSdk: ...
    def register_daemon(
        self, daemon: Any, identity: Identity
    ) -> AsyncMeshOsDaemonHandle: ...
    def dropped_control_events(self) -> int: ...
    async def shutdown(self) -> None: ...
    def __repr__(self) -> str: ...

class AsyncSimulatedIceProposal:
    """Post-simulation husk. Only class exposing awaitable
    :meth:`commit`. Sync equivalent: :class:`SimulatedIceProposal`."""

    def blast_radius(self) -> str: ...
    @property
    def issued_at_ms(self) -> int: ...
    def blast_hash(self) -> bytes: ...
    def signing_payload(self) -> bytes: ...
    async def commit(
        self, signatures: List[Dict[str, Any]]
    ) -> Dict[str, Any]: ...
    def __repr__(self) -> str: ...

class AsyncIceProposal:
    """Pre-simulation husk. ``await proposal.simulate()`` yields an
    :class:`AsyncSimulatedIceProposal`. Sync equivalent:
    :class:`IceProposal`."""

    @property
    def issued_at_ms(self) -> int: ...
    async def simulate(self) -> AsyncSimulatedIceProposal: ...
    def __repr__(self) -> str: ...

class AsyncIceCommands:
    """Async sibling of :class:`IceCommands`. Factory methods stay
    sync — they only construct a proposal husk locally; the async
    work happens on ``await proposal.simulate()`` /
    ``await simulated.commit(...)``."""

    def freeze_cluster(self, ttl_ms: int) -> AsyncIceProposal: ...
    def flush_avoid_lists(
        self, scope: Dict[str, Any]
    ) -> AsyncIceProposal: ...
    def force_evict_replica(
        self, chain: int, victim: int
    ) -> AsyncIceProposal: ...
    def force_restart_daemon(
        self, id: int, name: str
    ) -> AsyncIceProposal: ...
    def force_cutover(
        self, chain: int, target: int
    ) -> AsyncIceProposal: ...
    def kill_migration(self, migration: int) -> AsyncIceProposal: ...
    def thaw_cluster(self) -> AsyncIceProposal: ...

class PinsError(Exception):
    """Pin-store failure: I/O reading/writing the store file, or a store
    file that exists but does not parse (corrupt stores error rather than
    silently dropping consent decisions). Message prefix: ``pins: ``."""

class CapabilityId:
    """A capability's canonical identity: ``provider/capability``. The
    provider is canonicalized (whitespace, ``0x``-hex node ids), so consent
    and pin records keyed through this type never miss a differently
    spelled twin. Frozen, hashable, comparable."""

    def __init__(self, provider: str, capability: str) -> None: ...
    @staticmethod
    def parse(s: str) -> "CapabilityId":
        """Parse the ``provider/capability`` display form (splits on the
        FIRST ``/``). Raises ``ValueError`` on a missing/empty half."""
        ...
    @property
    def provider(self) -> str: ...
    @property
    def capability(self) -> str: ...
    def display(self) -> str: ...
    def __str__(self) -> str: ...
    def __repr__(self) -> str: ...

def credential_requires_consent(status: str) -> bool:
    """Does a wire-declared credential status require local consent?
    Implements the core trust boundary: a wire ``"none"`` is NOT trusted
    (it gates like ``"unknown"``) — a discovered capability can only ever
    over-gate, never bypass consent."""
    ...

def default_pin_store_path() -> Optional[str]:
    """The per-user default pin-store path
    (``<local data>/net-mesh/mcp-pins.json``, falling back to the home
    directory), or ``None`` if neither resolves. The same file the
    ``net mcp pin`` CLI and a running ``net mcp serve`` shim use — pass it to
    :class:`PinStore` / :class:`AsyncPinStore` / ``CapabilityGateway`` to
    share consent decisions machine-wide without hard-coding the path."""
    ...

class ConsentPolicy:
    """The consumer-side consent gate: config allowlist + pinned set.
    With no entries EVERY discovered capability requires approval. The
    decision logic lives in the Rust SDK; this class only carries state."""

    def __init__(self) -> None: ...
    def allow(self, cap_id: "str | CapabilityId") -> None: ...
    def pin(self, cap_id: "str | CapabilityId") -> None: ...
    def unpin(self, cap_id: "str | CapabilityId") -> None: ...
    def is_pinned(self, cap_id: "str | CapabilityId") -> bool: ...
    def pinned(self) -> list[str]: ...
    def decide(self, cap_id: "str | CapabilityId", credential_status: str) -> str:
        """``"allowed"`` or ``"requires_approval"`` — the SDK enum's stable
        string form; never re-derive the gate in Python."""
        ...
    def requires_approval(
        self, cap_id: "str | CapabilityId", credential_status: str
    ) -> bool: ...
    def __repr__(self) -> str: ...

class PinStore:
    """Path-scoped handle on the persistent, machine-shared pin store —
    the same file the ``net mcp pin`` CLI and a running ``net mcp serve``
    shim use. Reads load a fresh snapshot; every mutation is a full
    locked load->apply->save transaction (cross-process advisory lock),
    with the GIL released. ``request`` is the model-callable verb (only
    ever writes ``"pending"``); ``approve``/``reject`` are operator verbs."""

    def __init__(self, path: str) -> None: ...
    @property
    def path(self) -> str: ...
    def request(self, cap_id: "str | CapabilityId") -> str: ...
    def approve(self, cap_id: "str | CapabilityId") -> bool: ...
    def reject(self, cap_id: "str | CapabilityId") -> bool: ...
    def is_approved(self, cap_id: "str | CapabilityId") -> bool: ...
    def state(self, cap_id: "str | CapabilityId") -> Optional[str]: ...
    def approved(self) -> list[str]: ...
    def pending(self) -> list[str]: ...
    def list(self) -> list[tuple[str, str]]: ...
    def __repr__(self) -> str: ...

class AsyncPinStore:
    """Async dual of :class:`PinStore` — the same path-scoped,
    lock-protected operations as awaitables (the asyncio loop is never
    blocked on the cross-process lock)."""

    def __init__(self, path: str) -> None: ...
    @property
    def path(self) -> str: ...
    async def request(self, cap_id: "str | CapabilityId") -> str: ...
    async def approve(self, cap_id: "str | CapabilityId") -> bool: ...
    async def reject(self, cap_id: "str | CapabilityId") -> bool: ...
    async def is_approved(self, cap_id: "str | CapabilityId") -> bool: ...
    async def state(self, cap_id: "str | CapabilityId") -> Optional[str]: ...
    async def approved(self) -> list[str]: ...
    async def pending(self) -> list[str]: ...
    async def list(self) -> list[tuple[str, str]]: ...
    async def snapshot_and_watch(self) -> tuple[list[str], "AsyncPinWatcher"]:
        """Snapshot the currently-approved capabilities AND subscribe to
        changes, atomically. Returns ``(approved, watcher)`` — promote the
        snapshot, then ``async for change in watcher:`` for subsequent deltas.
        The subscription is an OS file watcher, so a cross-process
        ``net mcp pin approve`` (or another SDK client) arrives as an event,
        not a poll."""
        ...

    async def watch(self) -> "AsyncPinWatcher":
        """Subscribe to approved-pin changes, discarding the initial snapshot."""
        ...

    def __repr__(self) -> str: ...

class PinChange:
    """One approved-pin change, yielded by :class:`AsyncPinWatcher`: the
    capabilities newly approved and those no longer approved (display ids)
    since the previous event."""

    @property
    def added(self) -> list[str]: ...
    @property
    def removed(self) -> list[str]: ...
    def __repr__(self) -> str: ...

class AsyncPinWatcher:
    """An async iterator over approved-pin changes in the machine-shared store,
    backed by an OS file watcher (not polling). ``async for change in
    watcher:`` yields a :class:`PinChange` per approved-set delta."""

    def __aiter__(self) -> "AsyncPinWatcher": ...
    async def __anext__(self) -> PinChange: ...

def classify_mcp_server(
    program: str,
    args: list[str],
    envs: list[tuple[str, str]],
    credential_override: Optional[str] = None,
    force: bool = False,
) -> str:
    """Classify a wrapped MCP server's credential exposure:
    ``"credentialed"`` | ``"external_api"`` | ``"unknown"`` | ``"none"``.
    Conservative by construction — detection never yields the ungated
    ``"none"``; only ``credential_override="no-credentials"`` with
    ``force=True`` can. Only env KEYS drive detection; values never
    appear in the result."""
    ...

def lower_mcp_tool(
    tool_json: str,
    server_version: str,
    credential_status: str,
    substitutability: str = "provider_local",
) -> str:
    """Lower one MCP ``tools/list`` entry (JSON) to the Net discovery
    shape. Returns JSON: ``{"tool_id", "mcp_name", "descriptor",
    "bridge_metadata"}`` — classification labels only, never a secret."""
    ...

class CapabilityGateway:
    """The demand side of the bridge, natively — ``search`` / ``describe`` /
    ``invoke`` over the mesh with the consent gate applied *inside*, no stdio
    MCP shim in the middle. Built with the ``net`` + ``mcp`` features (the
    default wheel has both).

    Wraps a joined :class:`NetMesh` node and the machine-shared pin store (the
    same file ``net mcp pin`` and ``net mcp serve`` use, so an approval made
    anywhere is honored here). It applies the one Rust consent gate the shim
    also uses — describe -> validate arguments -> consent/pins -> invoke — so
    the native path and the MCP-compat path can never diverge.

    Every method returns a JSON **string** with a ``status`` discriminant and
    never raises for a gate outcome, so an embedding agent relays a pin
    instruction or lets a model self-repair a bad argument:

    - ``search(query)`` -> ``{"status":"ok","capabilities":[{cap_id, name,
      description, compat_tier, credential_status, providers,
      requires_approval}, ...]}``
    - ``describe(cap_id)`` -> ``{"status":"ok", cap_id, name, description,
      input_schema, output_schema, compat_tier, credential_status,
      substitutability, version, requires_approval}``
    - ``invoke(cap_id, arguments_json)`` -> ``{"status": "ok" |
      "requires_approval" | "requires_payment_approval" | "validation_error" |
      "denied" | "not_found" | "transport_error" | "no_daemon" | "error",
      ...}``. On ``ok`` inspect ``is_error`` for a tool-level failure; on
      ``requires_approval`` relay ``approve_command``; on
      ``requires_payment_approval`` relay ``{quote_id, policy_reason,
      approve_hint}`` — spend policy wants a human, the decision resolves
      through the payments consent surface, and nothing was charged. On a
      payment refusal (usually ``denied``) a ``failure`` object may ride
      *beside* ``error`` (never instead of it) when the provider attached the
      ``net.payment.failure@1`` schematic — the machine-actionable verdict:
      ``{object, code, stage, reason, message, retryable, recovery: {class,
      actor, safe_to_retry, safe_to_requote, next_action?}, handler_executed,
      funds_moved, prior_payment, quote_id?, tool_id?, ...}``. Branch on
      ``failure["reason"]`` / ``failure["recovery"]`` instead of parsing prose;
      unknown reasons and extra fields are tolerated (the schematic's ``@1``
      additive-forward-compat contract). Its absence means the refusal carried
      no schematic — fall back to ``error``.

    ``describe`` results additionally carry ``pricing_terms`` (the announced
    ``net.pricing.terms@1`` canonical JSON) when the capability is paid;
    ``null`` means free. Displaying a price never implies authorization to
    spend it.

    The methods release the GIL while the mesh call is in flight, so an
    ``async`` caller can await them off the event loop without blocking it::

        result = await asyncio.to_thread(gateway.invoke, cap_id, args_json)
    """

    # Both-or-neither is enforced at runtime (passing exactly one delegation
    # arg raises ValueError), so express the two valid signatures as overloads
    # — a partial call then fails static analysis instead of only at runtime.
    @overload
    def __init__(self, mesh: "NetMesh", pin_store_path: Optional[str] = ...) -> None: ...
    @overload
    def __init__(
        self,
        mesh: "NetMesh",
        pin_store_path: Optional[str],
        delegation_leaf: "Identity",
        delegation_chain: bytes,
    ) -> None: ...
    def __init__(
        self,
        mesh: "NetMesh",
        pin_store_path: Optional[str] = None,
        delegation_leaf: Optional["Identity"] = None,
        delegation_chain: Optional[bytes] = None,
        payment_policy_path: Optional[str] = None,
        payment_profile: Optional[str] = None,
        payment_unsafe_mock_auto_allow: bool = False,
        payment_signer_address: Optional[str] = None,
        payment_signer: Optional[Callable[[str], str]] = None,
        payment_signer_svm_address: Optional[str] = None,
        payment_signer_svm: Optional[Callable[[str], str]] = None,
        payment_signer_xrpl_address: Optional[str] = None,
        payment_signer_xrpl: Optional[Callable[[str], str]] = None,
    ) -> None:
        """Build a gateway over a started ``mesh``. ``pin_store_path`` should
        be the machine-shared pin store so approvals are honored both ways;
        omit it to keep consent in-memory (every gated capability then always
        requires approval).

        Pass ``delegation_leaf`` (the gateway ``Identity`` handle) **and**
        ``delegation_chain`` (a serialized ``DelegationChain``) together to have
        every invoke carry a per-invoke signed delegation (Phase 3); a remote
        provider running a delegation gate then admits by verified delegation
        and audits this gateway's leaf. **Both or neither** — passing exactly
        one raises ``ValueError``.

        Pass ``payment_policy_path`` (the machine-shared spend-policy store)
        to enable paid capabilities: the invoke gate then clears them through
        the Rust payments flow (quote -> spend policy -> x402 payload -> pay
        over the mesh). ``payment_profile`` is ``"production"`` (the
        fail-closed default: every mock spend holds for approval) or
        ``"dev_test"`` (mock auto-allows under the configured limits);
        ``payment_unsafe_mock_auto_allow=True`` is the explicit unsafe flag
        for production-profile demos. Without ``payment_policy_path``, a paid
        capability fails closed as a structured ``denied`` — never a silent
        unpaid serve. Requires the ``payments`` build feature (the default
        wheel has it); passing payment kwargs on a build without it raises
        ``ValueError``.

        The payment identity is the node's mesh identity: quotes are issued
        to, spend is tracked against, and invocation proofs are signed by the
        same ed25519 identity peers see on the mesh.

        Real (non-mock) networks additionally need a settlement signer
        *reference*: pass ``payment_signer_address`` (the payer's ``0x…``
        address) **and** ``payment_signer`` (both or neither), a callable
        ``(typed_data_json: str) -> str`` that forwards the full EIP-712
        typed-data document to your wallet / KMS and returns the 65-byte
        ``0x…``-hex signature. Only the typed document and the signature
        cross the language boundary — there is no way to hand Net a private
        key, and the only thing this surface can ask your signer for is a
        logged, typed transfer authorization (never raw bytes). Enablement
        still requires the network in the spend policy's
        ``allowed_networks`` — the signer is capability, not consent.

        Solana and XRPL settlement use the same seam under their own
        namespaces: ``payment_signer_svm_address`` + ``payment_signer_svm``
        (a ``(intent_json: str) -> str`` returning the base64 partially-signed
        SVM transaction) and ``payment_signer_xrpl_address`` +
        ``payment_signer_xrpl`` (returning the hex presigned XRPL ``Payment``
        blob). Each pair is both-or-neither; an absent pair means that scheme
        is simply unavailable. The callable always sees a typed intent JSON,
        never key material — identical doctrine to the eip155 seam."""
        ...

    @property
    def pin_store_path(self) -> Optional[str]:
        """The machine-shared pin store path this gateway consults, if any."""
        ...

    def search(self, query: str) -> str:
        """Search the mesh for capabilities matching ``query`` (substring over
        id / name / description). Returns the JSON described above; an empty
        index is ``ok`` with an empty list, never an error."""
        ...

    def describe(self, cap_id: str) -> str:
        """Full detail for one capability, including its input schema and the
        caller-side ``requires_approval`` flag. Returns the JSON described
        above."""
        ...

    def invoke(self, cap_id: str, arguments_json: str = "{}") -> str:
        """Invoke a capability through the consent gate. ``arguments_json`` is
        the tool's own arguments as a JSON object string. Returns the
        structured JSON described above; never raises for a gate outcome."""
        ...

    def approve_payment(self, quote_id: str) -> str:
        """Approve a held payment quote under operator policy, resolving a
        prior ``requires_payment_approval`` so the next :meth:`invoke`
        redeems it. Returns
        ``{"status":"ok","quote_id":...,"changed":bool}`` (``changed`` is
        whether the record moved to approved). This is the **operator**
        surface — the model-reachable :meth:`invoke` only ever *requests*
        approval; this grants it. Requires the gateway to have been built
        with ``payment_policy_path`` (else ``{"status":"no_payment_policy"}``)
        and the ``payments`` build feature (else ``{"status":"unsupported"}``)."""
        ...

    def reject_payment(self, quote_id: str) -> str:
        """Reject / remove a payment approval record. Returns
        ``{"status":"ok","quote_id":...,"changed":bool}`` (``changed`` is
        whether a record existed), or a structured
        ``no_payment_policy`` / ``unsupported`` / ``error``."""
        ...

    def pending_payments(self) -> str:
        """The quote ids awaiting approval, for a consent UX to render.
        Returns ``{"status":"ok","pending":[quote_id, ...]}``, or a
        structured ``no_payment_policy`` / ``unsupported`` / ``error``."""
        ...

    def spent_today(self, network: str, asset: str) -> str:
        """Today's reserved spend total for a ``(network, x402 asset)`` pair,
        as the canonical atomic-amount string. Returns
        ``{"status":"ok","network":...,"asset":...,"spent":"<atomic>"}``, or a
        structured ``no_payment_policy`` / ``unsupported`` / ``error``.
        ``network`` / ``asset`` are the x402 wire values (e.g.
        ``"mock:net"`` / ``"musd"``), matching the quote's requirements."""
        ...

    def __repr__(self) -> str: ...

class AsyncCapabilityGateway:
    """Awaitable dual of :class:`CapabilityGateway` — the same ``search`` /
    ``describe`` / ``invoke`` as coroutines for ``asyncio`` code, resolving to
    the same structured JSON strings. Each awaits the gateway op on the mesh's
    own runtime, so mesh I/O stays on the right reactor. Present iff the wheel
    was built with the ``net`` + ``mcp`` features."""

    @overload
    def __init__(self, mesh: "NetMesh", pin_store_path: Optional[str] = ...) -> None: ...
    @overload
    def __init__(
        self,
        mesh: "NetMesh",
        pin_store_path: Optional[str],
        delegation_leaf: "Identity",
        delegation_chain: bytes,
    ) -> None: ...
    def __init__(
        self,
        mesh: "NetMesh",
        pin_store_path: Optional[str] = None,
        delegation_leaf: Optional["Identity"] = None,
        delegation_chain: Optional[bytes] = None,
        payment_policy_path: Optional[str] = None,
        payment_profile: Optional[str] = None,
        payment_unsafe_mock_auto_allow: bool = False,
        payment_signer_address: Optional[str] = None,
        payment_signer: Optional[Callable[[str], str]] = None,
        payment_signer_svm_address: Optional[str] = None,
        payment_signer_svm: Optional[Callable[[str], str]] = None,
        payment_signer_xrpl_address: Optional[str] = None,
        payment_signer_xrpl: Optional[Callable[[str], str]] = None,
    ) -> None:
        """Same as :class:`CapabilityGateway` — pass ``delegation_leaf`` +
        ``delegation_chain`` together (both or neither) to sign + attach a
        delegation on every invoke (Phase 3); pass ``payment_policy_path``
        (+ optional ``payment_profile`` / unsafe flag) to enable paid
        capabilities through the payments flow, and
        ``payment_signer_address`` + ``payment_signer`` (both or neither)
        for real-network settlement — see :class:`CapabilityGateway` for the
        signer-reference contract. The signer callable runs on a blocking
        worker thread, never on your event loop."""
        ...
    @property
    def pin_store_path(self) -> Optional[str]: ...
    async def search(self, query: str) -> str:
        """Awaitable :meth:`CapabilityGateway.search`."""
        ...

    async def describe(self, cap_id: str) -> str:
        """Awaitable :meth:`CapabilityGateway.describe`."""
        ...

    async def invoke(self, cap_id: str, arguments_json: str = "{}") -> str:
        """Awaitable :meth:`CapabilityGateway.invoke`."""
        ...

    async def approve_payment(self, quote_id: str) -> str:
        """Awaitable :meth:`CapabilityGateway.approve_payment`."""
        ...

    async def reject_payment(self, quote_id: str) -> str:
        """Awaitable :meth:`CapabilityGateway.reject_payment`."""
        ...

    async def pending_payments(self) -> str:
        """Awaitable :meth:`CapabilityGateway.pending_payments`."""
        ...

    async def spent_today(self, network: str, asset: str) -> str:
        """Awaitable :meth:`CapabilityGateway.spent_today`."""
        ...

    def __repr__(self) -> str: ...

class PaymentHttpClient:
    """Pay an **external x402 HTTP API** — the outbound two-way door, with
    the same spend policy, signers, and status vocabulary as
    :class:`CapabilityGateway`. Present iff the module was built with the
    ``payments-http`` feature (an opt-in that pulls a bundled HTTP/TLS
    stack; NOT in the default wheel).

    :meth:`fetch_paid` GETs a URL and, if the server answers ``402``, runs
    the caller's own spend policy over a local pseudo-quote, signs, and
    retries — one call authors at most one payment attempt. There is no
    provider identity and no signed quote on this path: the external
    server's demand is the commercial fact, and the caller's spend engine
    (caps, network enablement, approvals) is the entire gate.
    """

    def __init__(
        self,
        payment_policy_path: str,
        payment_profile: Optional[str] = None,
        payment_unsafe_mock_auto_allow: bool = False,
        payment_signer_address: Optional[str] = None,
        payment_signer: Optional[Callable[[str], str]] = None,
        identity: Optional["Identity"] = None,
    ) -> None:
        """Build a client over the shared spend-policy store at
        ``payment_policy_path`` (**required** — the spend gate). The payment
        kwargs mirror :class:`CapabilityGateway`: ``payment_profile``
        (``"production"`` default / ``"dev_test"``),
        ``payment_unsafe_mock_auto_allow``, and the real-network settlement
        signer *reference* ``payment_signer_address`` + ``payment_signer``
        (both or neither — a ``(typed_data_json: str) -> str`` EIP-712
        callback; key material never crosses the boundary). ``identity`` is
        an optional payer :class:`Identity` handle; omit it for an ephemeral
        one (the caller id is bookkeeping on this path — spend is tracked by
        ``(network, asset, day)``, not by caller)."""
        ...

    def fetch_paid(self, url: str) -> tuple[str, bytes]:
        """GET ``url``, paying if the server answers ``402``. Returns
        ``(status_json, body)``: ``status_json`` is
        ``{"status": "fetched" | "paid" | "requires_payment_approval" |
        "denied" | "provider_refused" | "transport_error", ...}`` (``paid``
        carries the byte-preserved ``settlement`` as base64;
        ``requires_payment_approval`` carries ``{quote_id, policy_reason,
        approve_hint}`` and did NOT retry) and ``body`` is the raw response
        bytes (empty for the non-body outcomes). Never raises for a payment
        outcome; releases the GIL while in flight."""
        ...

    def __repr__(self) -> str: ...

class AsyncPaymentHttpClient:
    """Awaitable dual of :class:`PaymentHttpClient` — :meth:`fetch_paid` as
    a coroutine, resolving to the same ``(status_json, body)`` tuple.
    Present iff built with the ``payments-http`` feature."""

    def __init__(
        self,
        payment_policy_path: str,
        payment_profile: Optional[str] = None,
        payment_unsafe_mock_auto_allow: bool = False,
        payment_signer_address: Optional[str] = None,
        payment_signer: Optional[Callable[[str], str]] = None,
        identity: Optional["Identity"] = None,
    ) -> None:
        """Same as :class:`PaymentHttpClient`."""
        ...

    async def fetch_paid(self, url: str) -> tuple[str, bytes]:
        """Awaitable :meth:`PaymentHttpClient.fetch_paid`."""
        ...

    def __repr__(self) -> str: ...

def build_pricing_terms(
    provider_entity_id: bytes,
    capability: str,
    requirements_json: str,
) -> str:
    """Author the canonical ``net.pricing.terms@1`` JSON that prices a
    capability — the provider (supply) side of payments. Present iff the
    module was built with the ``payments`` feature.

    ``provider_entity_id`` is the node's 32-byte mesh entity id
    (``mesh.entity_id``) — the identity that will issue quotes for these terms;
    only the public id crosses, never a key. ``capability`` is the
    ``provider/capability`` display id. ``requirements_json`` is a JSON **array**
    of x402 ``PaymentRequirements`` objects using the camelCase wire names —
    ``scheme``, ``network``, ``amount`` (atomic string), ``asset``, ``payTo``,
    ``maxTimeoutSeconds`` (int), optional ``extra`` — one entry per acceptable
    ``(scheme, network, asset)``. Returns the canonical, byte-preserved terms
    string to hand to the priced-publish path or announce at discovery (opaque
    downstream; displaying a price never implies authorization to spend it).
    Raises ``ValueError`` on a non-32-byte id, malformed JSON, or an empty
    list."""
    ...

class PaymentProvider:
    """A paid-capability provider over an embedded :class:`NetMesh` node — the
    supply side of payments (price + charge). Construction stands up one
    ``PaymentEngine`` behind the quote/pay wire; :meth:`publish_paid_tools`
    publishes priced tools gated by that same engine, so a quote paid over the
    wire is the quote the gate redeems (at-most-once, after payment). Present
    iff the module was built with the ``payments`` **and** ``publish`` features
    (the default wheel has both). Hold the instance to keep the wire served."""

    def __init__(
        self,
        mesh: "NetMesh",
        state_path: str,
        billing_log_path: Optional[str] = None,
    ) -> None:
        """Build a provider over a started ``mesh``. ``state_path`` is the
        settlement store file — it holds the replay/idempotency index and
        **must be durable + single-owner** (a temp path loses paid quotes across
        restarts). ``billing_log_path`` optionally records the immutable
        ``net.billing.event@1`` stream."""
        ...

    @property
    def provider_entity_id(self) -> bytes:
        """The node's 32-byte mesh entity id — the provider identity these tools
        price + quote under. Pass it to :func:`build_pricing_terms`."""
        ...

    def read_billing(self) -> List[str]:
        """The immutable billing events this provider recorded, oldest first —
        each a ``net.billing.event@1`` JSON string. Read-only (billing is
        emitted by the engine; this only reads). Requires a ``billing_log_path``
        at construction, else raises ``ValueError``."""
        ...

    def publish_paid_tools(
        self,
        tools: List[Tuple[str, Optional[str], str]],
        callback: Any,
        pricing: Dict[str, str],
        version: str = ...,
        owner_origin: Optional[int] = ...,
        allow_any_caller: bool = ...,
    ) -> "LocalPublicationHandle":
        """Publish priced tools gated by this provider's payment engine.
        ``tools`` is a list of ``(name, description|None, input_schema_json)``;
        ``callback`` is the same async invoker as ``NetMesh.publish_tools``;
        ``pricing`` maps a tool name to its ``net.pricing.terms@1`` JSON (from
        :func:`build_pricing_terms`). A priced tool serves only **after** its
        quote is paid + redeemed. Fail-closed: an empty ``pricing`` map raises
        ``ValueError`` (use ``NetMesh.publish_tools`` for free tools); a pricing
        key naming no published tool is a publish error. ``version`` /
        ``owner_origin`` / ``allow_any_caller`` are as on
        ``NetMesh.publish_tools``. Hold the returned handle to keep serving."""
        ...


# ---------------------------------------------------------------------------
# Organization capability auth (OSDK-L Workstream P, the `org` feature)
# ---------------------------------------------------------------------------

class OrgError(Exception):
    """Base for organization capability errors."""
    ...

class OrgCredentialsError(OrgError):
    """Local: the credential set could not authorize this call."""
    ...

class OrgDiscoveryError(OrgError):
    """Local: no provider this credential set may call was found."""
    ...

class OrgAdmissionDeniedError(OrgError):
    """Remote: the provider's admission engine refused the call."""
    ...

class OrgUnclassifiedError(OrgError):
    """The `org:` vocabulary could not be parsed — an internal compatibility
    failure, not an admission result."""
    ...

class OrgCredentials:
    """A validated organization credential set.

    Public signed credentials cross as ``bytes``; audience secrets cross as
    file **paths** and never as bytes, so the raw discovery key is never in
    Python memory. Consumed by :meth:`OrgClient.bind`."""

    def __init__(
        self,
        membership: bytes,
        dispatcher: bytes,
        grants: List[bytes],
        audience_secret_paths: List[str],
    ) -> None: ...

class OrgClient:
    """A credential set bound to a live mesh — the caller half of the facade.

    Close it when done (context-manager supported): ``close()`` drops the
    audience lease and the node reference. Teardown order:
    ``org_client.close()`` -> ``serve_handle.close()`` -> ``mesh.shutdown()``."""

    @staticmethod
    def bind(mesh: Any, credentials: OrgCredentials) -> "OrgClient": ...
    def call(self, service: str, request: bytes) -> bytes: ...
    @property
    def acting_org(self) -> bytes: ...
    @property
    def caller(self) -> bytes: ...
    @property
    def is_closed(self) -> bool: ...
    def close(self) -> None: ...
    def __enter__(self) -> "OrgClient": ...
    def __exit__(self, *exc: object) -> bool: ...

class OrgServeHandle:
    """Handle for a served organization service. ``close()`` unregisters."""

    @property
    def is_closed(self) -> bool: ...
    def close(self) -> None: ...
    def __enter__(self) -> "OrgServeHandle": ...
    def __exit__(self, *exc: object) -> bool: ...

def serve_org(
    mesh: Any,
    service: str,
    access: str,
    handler: Any,
    handler_timeout_ms: Optional[int] = ...,
) -> OrgServeHandle:
    """Serve a protected, privately-discoverable service. ``access`` is
    ``"same_org"`` or ``"granted"``. The handler is
    ``handler(caller: dict, request: bytes) -> bytes``."""
    ...

def install_org_authority(mesh: Any, authority_dir: str) -> None:
    """Install an adopted node authority from the directory ``net node adopt``
    wrote. Required before ``OrgClient.bind`` or a ``"granted"`` ``serve_org``."""
    ...

def install_provider_grant_audience(
    mesh: Any, grant: bytes, audience_secret_path: str
) -> None:
    """Install a provider grant audience (grant wire bytes + secret file PATH)
    so a ``"granted"`` service can seal envelopes."""
    ...
