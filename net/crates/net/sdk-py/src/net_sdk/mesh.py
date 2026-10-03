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

import asyncio
from dataclasses import dataclass
from typing import Any, Callable, List, Literal, Optional, TypedDict

# The PyO3 module is `_net`; binding classes and exceptions come from it.
# `BackpressureError`, `NotConnectedError` and `SessionSupersededError`
# are `PyException` subclasses defined via `pyo3::create_exception!` —
# re-export them here so users import from `net_sdk`, not the private
# binding module.
from net import (  # type: ignore[attr-defined]
    NetMesh as _NetMesh,
    BackpressureError,
    ChannelAuthError,
    ChannelError,
    NotConnectedError,
    SessionSupersededError,
)


Reliability = Literal["fire_and_forget", "reliable"]

# Mesh-channel vocabulary. Each `Literal` lists exactly the strings the
# native parsers accept (`bindings/python/src/lib.rs`: `parse_visibility`,
# `parse_reliability_cfg`, `parse_on_failure`); anything else raises
# `ChannelError` there. Read from the parsers, not from TS.
Visibility = Literal["subnet-local", "parent-visible", "exported", "global"]
OnFailure = Literal["best_effort", "fail_fast", "collect"]


class ChannelConfig(TypedDict, total=False):
    """Keyword options for :meth:`MeshNode.register_channel`, so a config
    can be built once and splatted: ``node.register_channel(name, **cfg)``."""

    visibility: Visibility
    reliable: bool
    require_token: bool
    token_roots: List[bytes]
    """32-byte entity ids whose signature may root a presented chain."""
    priority: int
    max_rate_pps: int
    publish_caps: dict
    """``CapabilityFilter`` dict a publisher's announcement must satisfy."""
    subscribe_caps: dict
    """``CapabilityFilter`` dict a subscriber's announcement must satisfy."""


class PublishConfig(TypedDict, total=False):
    """Keyword options for :meth:`MeshNode.publish`."""

    reliability: Reliability
    on_failure: OnFailure
    max_inflight: int


class PublishError(TypedDict):
    """One subscriber the publish didn't reach."""

    node_id: int
    message: str


class PublishReport(TypedDict):
    """What :meth:`MeshNode.publish` returns. ``attempted`` counts
    subscribers on the roster when the publish ran; ``attempted == 0`` is
    a successful publish to nobody, not an error."""

    attempted: int
    delivered: int
    errors: List[PublishError]


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
        identity_seed: Optional[bytes] = None,
        subnet: Optional[list] = None,
        subnet_policy: Optional[dict] = None,
        subnet_authorities: Optional[list] = None,
        subnet_attachment: Optional[list] = None,
        subnet_control_channel: Optional[str] = None,
        subnet_exports: Optional[list] = None,
        capability_gc_interval_ms: Optional[int] = None,
        require_signed_capabilities: Optional[bool] = None,
        reflex_override: Optional[str] = None,
        try_port_mapping: Optional[bool] = None,
        auto_direct_upgrade: Optional[bool] = None,
        permissive_channels: Optional[bool] = None,
    ) -> None:
        """Construct a mesh node. Every keyword is forwarded unchanged to
        the native ``NetMesh`` constructor, which validates it.

        - ``require_signed_capabilities``: reject capability
          announcements that aren't signed.
        - ``capability_gc_interval_ms``: capability-index GC cadence.
        - ``reflex_override``: pin this node's public reflex to an
          external ``"ip:port"`` (``nat-traversal`` builds; ignored
          otherwise).
        - ``try_port_mapping``: opportunistic UPnP / NAT-PMP / PCP at
          startup (``port-mapping`` builds; ignored otherwise).
        - ``auto_direct_upgrade``: migrate relay-routed sessions to a
          direct path when one appears. Native default ``True``; pass
          ``False`` to pin traffic to the relay.
        - ``permissive_channels``: install no channel registry, which
          turns channel authorization off for **every** channel on this
          node: any peer may subscribe to any channel. Test-only; nRPC and
          the tool surface work with the strict default.

        ``tests/test_sdk_mesh_ctor_parity.py`` (in the binding's test
        suite) fails if this signature drifts from the native one.
        """
        # SSDK P4 forwarded the topology kwargs this wrapper used to drop
        # (`identity_seed`, `subnet`, `subnet_policy`) and the subnet
        # AUTHORITY kwargs; the six options after `subnet_exports` were
        # dropped the same way until `PYTHON_SDK_WRAPPER_PARITY_PLAN.md`
        # S1a. All are validated by the native constructor — this layer
        # only threads them through.
        self._native = _NetMesh(
            bind_addr,
            psk,
            heartbeat_interval_ms=heartbeat_interval_ms,
            session_timeout_ms=session_timeout_ms,
            num_shards=num_shards,
            identity_seed=identity_seed,
            capability_gc_interval_ms=capability_gc_interval_ms,
            require_signed_capabilities=require_signed_capabilities,
            reflex_override=reflex_override,
            try_port_mapping=try_port_mapping,
            auto_direct_upgrade=auto_direct_upgrade,
            permissive_channels=permissive_channels,
            subnet=subnet,
            subnet_policy=subnet_policy,
            subnet_authorities=subnet_authorities,
            subnet_attachment=subnet_attachment,
            subnet_control_channel=subnet_control_channel,
            subnet_exports=subnet_exports,
        )

    def serve_subnet_exported(
        self,
        service: str,
        export_name: str,
        handler: Callable[[dict, Any], Any],
        handler_timeout_ms: Optional[int] = None,
    ) -> Any:
        """Serve a subnet-exported, organization-protected service.

        One of the two ordinary subnet verbs (``SUBNET_AUTH_SDK_PLAN.md``
        §3.5); the caller's counterpart is ``org.call_exported(service,
        request)``. Name the service, name an export configured in
        ``subnet_exports`` at construction, provide the handler — this
        constructs no authority objects. The export name is
        provider-local configuration: never announced, never accepted
        from a caller.

        An unknown ``export_name`` raises ``SubnetProvisionError`` with
        ``.kind == "unknown_export_name"`` HERE, before anything is
        registered or announced. Dispatch revalidates the exact crossing
        against this node's live gateway authority on every call, before
        organization admission. Announcement visibility is always public;
        the external caller never joins this node's subnet.

        ``handler`` is ``handler(caller: dict, request) -> response``,
        with ``caller`` carrying the same verified fields as
        ``serve_org``. Returns a handle whose ``close()`` unregisters.

        review-10 P1-5: this facade exists so an application using the
        ergonomic constructor can serve a named export without reaching
        into ``self._native``.
        """
        from net.subnet import serve_subnet_exported as _serve

        return _serve(self._native, service, export_name, handler, handler_timeout_ms)

    @property
    def public_key(self) -> str:
        """Hex-encoded Noise static public key."""
        return self._native.public_key

    @property
    def node_id(self) -> int:
        """This node's id."""
        return self._native.node_id

    @property
    def local_addr(self) -> str:
        """The resolved local socket address.

        Required whenever ``bind_addr`` ends in ``:0`` — the OS picks
        the port and this is the only way to learn which one, so a peer
        can be told where to connect. The README's own ``127.0.0.1:0``
        example could not be completed without it.
        """
        return self._native.local_addr

    def connect(self, peer_addr: str, peer_public_key: str, peer_node_id: int) -> None:
        """Connect to a peer as initiator.

        BLOCKS until the handshake completes or times out. Pair it with a
        concurrent :meth:`accept` on the responder — see that method for
        why the two cannot run in sequence on one thread.
        """
        self._native.connect(peer_addr, peer_public_key, peer_node_id)

    def accept(self, peer_node_id: int) -> str:
        """Accept an incoming connection as responder.

        Returns the peer's wire address.

        BLOCKS until the initiator connects. The handshake needs both
        halves in flight at once, so calling ``accept`` and then
        ``connect`` on the same thread cannot work: ``accept`` never
        returns, the initiating call is never reached, and the failure
        arrives as a handshake timeout that blames the network rather
        than the ordering::

            RuntimeError: accept: connection error: handshake timeout

        Run the responder side concurrently::

            import threading

            t = threading.Thread(target=host.accept, args=(agent.node_id,))
            t.start()
            agent.connect(HOST_ADDR, host.public_key, host.node_id)
            t.join()

        A thread is enough — the call releases the GIL while it waits.
        """
        return self._native.accept(peer_node_id)

    def start(self) -> None:
        """Start the receive loop / heartbeats / router."""
        self._native.start()

    def peer_count(self) -> int:
        """Number of connected peers."""
        return self._native.peer_count()

    # ── Mesh channels (distributed pub/sub) ──────────────────────────
    #
    # Not to be confused with :class:`net_sdk.TypedChannel`, which is a
    # channel on the *local* event bus (``NetNode.channel``). These are
    # mesh channels: a publisher registers one, remote peers subscribe to
    # it, and :meth:`publish` fans a payload out to that roster.

    def register_channel(
        self,
        name: str,
        *,
        visibility: Optional[Visibility] = None,
        reliable: Optional[bool] = None,
        require_token: Optional[bool] = None,
        token_roots: Optional[List[bytes]] = None,
        priority: Optional[int] = None,
        max_rate_pps: Optional[int] = None,
        publish_caps: Optional[dict] = None,
        subscribe_caps: Optional[dict] = None,
    ) -> None:
        """Register ``name`` as a channel this node publishes. Subscribers
        are validated against this config before joining the roster.

        ``require_token`` on its own (no ``token_roots``) fails closed.
        Raises :class:`ChannelError` for an invalid name or option.
        See :class:`ChannelConfig` for the options as a dict.
        """
        self._native.register_channel(
            name,
            visibility=visibility,
            reliable=reliable,
            require_token=require_token,
            token_roots=token_roots,
            priority=priority,
            max_rate_pps=max_rate_pps,
            publish_caps=publish_caps,
            subscribe_caps=subscribe_caps,
        )

    def subscribe_channel(
        self,
        publisher_node_id: int,
        channel: str,
        token: Optional[bytes] = None,
    ) -> None:
        """Subscribe to ``channel`` on ``publisher_node_id``.

        ``token`` is a serialized ``PermissionToken``, needed when the
        channel requires one or this node's capabilities don't satisfy
        its ``subscribe_caps``. Raises :class:`ChannelAuthError` when the
        publisher refuses, :class:`ChannelError` for other failures.
        """
        self._native.subscribe_channel(publisher_node_id, channel, token)

    def unsubscribe_channel(self, publisher_node_id: int, channel: str) -> None:
        """Idempotent counterpart of :meth:`subscribe_channel`."""
        self._native.unsubscribe_channel(publisher_node_id, channel)

    def publish(
        self,
        channel: str,
        payload: bytes,
        *,
        reliability: Optional[Reliability] = None,
        on_failure: Optional[OnFailure] = None,
        max_inflight: Optional[int] = None,
    ) -> PublishReport:
        """Fan ``payload`` out to every subscriber of ``channel``.

        ``payload`` is raw bytes; this method doesn't encode for you.
        Subscribers receive it through :meth:`recv` /
        :meth:`poll_shard`, or per stream through
        :meth:`open_stream_inbox`.
        """
        return self._native.publish(
            channel,
            payload,
            reliability=reliability,
            on_failure=on_failure,
            max_inflight=max_inflight,
        )

    # ── Receiving ────────────────────────────────────────────────────

    def recv(self, limit: int) -> list:
        """Drain up to ``limit`` received events across every shard.

        The native sweep starts from a rotating shard so a busy shard
        can't starve the others; events are not in shard order. Each is
        a ``StoredEvent`` whose ``raw`` is the payload. It carries no
        sender: use :meth:`open_stream_inbox` when you need one.
        """
        return self._native.poll(limit)

    def num_shards(self) -> int:
        """Number of shards inbound traffic is spread across. A stream's
        events land on ``stream_id % num_shards``."""
        return self._native.num_shards()

    def shard_for_stream(self, stream_id: int) -> int:
        """The inbound shard ``stream_id``'s events land on."""
        return self._native.shard_for_stream(stream_id)

    def poll_shard(self, shard_id: int, limit: int) -> list:
        """Drain up to ``limit`` events from one shard. Pair with
        :meth:`shard_for_stream` to read a single stream."""
        return self._native.poll_shard(shard_id, limit)

    def open_stream_inbox(self, stream_id: int, capacity: int = 4096) -> Any:
        """A pull queue of every event arriving on ``stream_id``, from any
        peer, WITH the authenticated sender (``StreamData.peer_node_id``),
        instead of the shard queue.

        One receiver per stream: raises ``RuntimeError`` if the stream
        already has one. Past ``capacity``, events are dropped and
        counted (``inbox.dropped``) rather than stalling the receive
        loop. Close it, or use it as a context manager, to return the
        stream's events to the shard queue.
        """
        return self._native.open_stream_inbox(stream_id, capacity)

    # ── Identity and nRPC ────────────────────────────────────────────

    @property
    def entity_id(self) -> bytes:
        """32-byte ed25519 entity id. Equals ``Identity.from_seed(seed)
        .entity_id`` when the node was built with ``identity_seed=seed``."""
        return self._native.entity_id

    def rpc(self) -> Any:
        """A ``net.mesh_rpc.TypedMeshRpc`` bound to this node: typed
        request/response and streaming over the mesh, and the handle
        ``net_sdk.tool.serve_tool`` / ``call_tool`` take.

        Construction is cheap, but build one per node and reuse it.
        Requires the ``cortex`` build (the default one is)."""
        from net.mesh_rpc import TypedMeshRpc  # type: ignore[import-not-found]

        return TypedMeshRpc.from_mesh(self._native)

    # ── Capability aggregation ───────────────────────────────────────

    def capability_aggregate(
        self, matcher: Optional[Any], group_by: Any, aggregation: Any
    ) -> List[Any]:
        """Bucketed aggregation over this node's capability fold.

        Takes the :mod:`net_sdk.capability_aggregation` dataclasses
        (``TagMatcher`` or ``None`` for every entry, ``GroupBy``,
        ``Aggregation``) and returns ``AggregateRow``s sorted by bucket.
        """
        from net_sdk import capability_aggregation as agg

        rows = self._native.capability_aggregate(
            None if matcher is None else agg.tag_matcher_to_json(matcher),
            agg.group_by_to_json(group_by),
            agg.aggregation_to_json(aggregation),
        )
        return [agg.AggregateRow(**row) for row in rows]

    def capability_capacity_ranking(
        self, query: Any, rtt_map: Optional[dict] = None
    ) -> List[Any]:
        """Per-bucket capacity ranking for a ``CapacityQuery``, most
        available first. ``rtt_map`` maps node id to RTT (ms) for the
        query's ``max_rtt_ms`` filter. Returns ``CapacityRow``s."""
        from net_sdk import capability_aggregation as agg

        rows = self._native.capability_capacity_ranking(
            agg.capacity_query_to_json(query), rtt_map
        )
        return [agg.CapacityRow(**row) for row in rows]

    # ── Tools ────────────────────────────────────────────────────────

    def list_tools(self) -> List[Any]:
        """Every AI tool published in this node's capability fold, as
        ``ToolDescriptor``s. Same as ``net_sdk.tool.list_tools(node)``."""
        from net_sdk import tool

        return tool.list_tools(self._native)

    def watch_tools(self, *, interval: Optional[float] = None) -> Any:
        """Async iterator of ``ToolListChange``s for this node's tool view.
        Same as ``net_sdk.tool.watch_tools(node)``; see it for the
        lifecycle (consume or cancel it so the watch is closed)."""
        from net_sdk import tool

        return tool.watch_tools(self._native, interval=interval)

    # ── Blob and directory transfer (dataforts builds) ───────────────
    #
    # The types these take and return (``MeshBlobAdapter``, ``BlobRef``)
    # live in :mod:`net_sdk.blob`. Fetching needs the transfer engine on
    # the FETCHING node too: call :meth:`serve_blob_transfer` on both ends,
    # or a fetch raises ``TransferError`` / ``BlobError`` ("engine not
    # installed").

    def serve_blob_transfer(self, adapter: Any) -> None:
        """Install the blob-transfer engine over ``adapter`` (a
        ``MeshBlobAdapter``): serves its blobs to peers, and is required
        before this node can fetch."""
        from net import serve_blob_transfer  # type: ignore[attr-defined]

        serve_blob_transfer(self._native, adapter)

    def fetch_blob(self, holder_id: int, blob_ref: Any) -> bytes:
        """Fetch ``blob_ref`` from the node ``holder_id``."""
        from net import fetch_blob  # type: ignore[attr-defined]

        return fetch_blob(self._native, holder_id, blob_ref)

    def fetch_blob_discovered(self, blob_ref: Any) -> bytes:
        """Fetch ``blob_ref`` from whichever node is discovered to hold it."""
        from net import fetch_blob_discovered  # type: ignore[attr-defined]

        return fetch_blob_discovered(self._native, blob_ref)

    def store_dir(self, adapter: Any, root: str) -> Any:
        """Store the directory tree at ``root`` through ``adapter``; returns
        the manifest's ``BlobRef``."""
        from net import store_dir  # type: ignore[attr-defined]

        return store_dir(self._native, adapter, root)

    def fetch_dir(self, source_id: int, manifest_ref: Any, dest: str) -> tuple:
        """Rebuild the directory ``manifest_ref`` names, fetched from
        ``source_id``, under ``dest``. Returns ``(files_written,
        bytes_written)``."""
        from net import fetch_dir  # type: ignore[attr-defined]

        return fetch_dir(self._native, source_id, manifest_ref, dest)

    # ── Agent-to-agent (A2A) tasks (``a2a`` builds) ──────────────────
    #
    # Optional arguments are forwarded only when set, so the wheel's own
    # defaults (computed in Rust) stay the single source of truth.

    def serve_a2a(self, callback: Callable[..., Any]) -> Any:
        """Serve the A2A task lifecycle, backed by an **async** executor
        ``async (task_id, prompt, context_refs, tags) -> str`` that returns
        the result's artifact ref. Hold the returned handle to keep
        accepting tasks. The node must be ``start()``ed."""
        return self._native.serve_a2a(callback)

    def submit_task(
        self,
        target_node_id: int,
        prompt: str,
        context_refs: Optional[List[str]] = None,
        tags: Optional[List[str]] = None,
        *,
        task_id: Optional[str] = None,
        service: Optional[str] = None,
        revision: Optional[str] = None,
    ) -> str:
        """Hand a task to the executor at ``target_node_id``; returns the
        accepted task id. ``task_id`` keeps a caller-chosen id, which makes
        a resubmission idempotent on a provider with durable admission.
        ``service`` + ``revision`` (both or neither) name a catalog entry
        on a configured provider. Raises if the executor rejects it."""
        kwargs = _set_only(
            context_refs=context_refs,
            tags=tags,
            task_id=task_id,
            service=service,
            revision=revision,
        )
        return self._native.submit_task(target_node_id, prompt, **kwargs)

    def submit_task_paid(self, prepared_json: str, proof_json: str) -> str:
        """Submit a prepared, paid task (a ``PreparedTask`` document plus
        its ``TaskPaymentProof``). The raw verb: it keeps no records; use
        ``CapabilityGateway.submit_task`` for the durable attempt. Safe to
        resend. Raises ``PaymentRefused`` on a refusal."""
        return self._native.submit_task_paid(prepared_json, proof_json)

    def set_a2a_org_caller(self, org: Any) -> None:
        """Install (or clear with ``None``) the organization identity this
        mesh's A2A requester verbs present to a PROTECTED provider. ``org`` is
        a :class:`net_sdk.org.OrgClient` or a native ``net.OrgClient``. A
        ``CapabilityGateway`` has its own slot
        (:func:`net_sdk.payments.set_a2a_org_caller`)."""
        from net_sdk.payments import _native_org_client

        self._native.set_a2a_org_caller(_native_org_client(org))

    def describe_a2a(self, target_node_id: int) -> str:
        """What ``target_node_id`` serves, as a JSON array of ``A2aOffer``s.
        Uncharged; the only sanctioned way to learn a price. A node serving
        the free path (:meth:`serve_a2a`) has no describe service and
        raises at once: it answers the request ``NotFound``."""
        return self._native.describe_a2a(target_node_id)

    def task_status(self, target_node_id: int, task_id: str) -> Optional[str]:
        """The executor's status for ``task_id`` as JSON
        (``{brief, state, updated_at}``), or ``None`` if unknown."""
        return self._native.task_status(target_node_id, task_id)

    def cancel_task(self, target_node_id: int, task_id: str) -> bool:
        """Cancel ``task_id`` on the executor (its coroutine is cancelled);
        returns whether it was in flight."""
        return self._native.cancel_task(target_node_id, task_id)

    # ── Publishing this node's own tools (``publish`` builds) ────────

    def publish_tools(
        self,
        tools: List[tuple],
        callback: Callable[..., Any],
        version: Optional[str] = None,
        owner_origin: Optional[int] = None,
        allow_any_caller: Optional[bool] = None,
    ) -> Any:
        """Publish this node's own tools as mesh capabilities. ``tools`` is
        a list of ``(name, description | None, input_schema_json)``;
        ``callback`` is an async ``(tool_name, args_json) -> str | (str,
        bool)``. ``owner_origin=None`` admits only this node (fail-closed);
        ``allow_any_caller=True`` admits every peer. Hold the returned
        handle to keep the tools published. Needs ``start()`` and
        ``permissive_channels=True``."""
        kwargs = _set_only(
            version=version,
            owner_origin=owner_origin,
            allow_any_caller=allow_any_caller,
        )
        return self._native.publish_tools(tools, callback, **kwargs)

    # ── Device enrollment (``delegation`` builds) ────────────────────

    def rendezvous_string(self) -> str:
        """This node's invite rendezvous locator, for
        ``OperatorEnrollment.invite``."""
        return self._native.rendezvous_string()

    def serve_enrollment_auto(
        self,
        operator: Any,
        grant_ttl_seconds: int,
        max_depth: Optional[int] = None,
    ) -> Any:
        """Operator side: serve device enrollment (join + renew) on this
        node; the invite is the authorization. Hold the returned handle."""
        kwargs = _set_only(max_depth=max_depth)
        return self._native.serve_enrollment_auto(operator, grant_ttl_seconds, **kwargs)

    def join(self, device: Any, invite: str, name: str, tags: List[str]) -> Any:
        """Device side: enroll ``device``'s key into the mesh the ``invite``
        names; returns the verified ``root -> device`` ``DelegationChain``."""
        return self._native.join(device, invite, name, tags)

    def renew(self, enrollment: Any) -> Any:
        """Device side: refresh ``enrollment``'s grant over the mesh; returns
        the fresh chain. Needs ``start()`` and ``permissive_channels=True``."""
        return self._native.renew(enrollment)

    # ── Low-level escape hatches ─────────────────────────────────────

    def push_to(self, peer_addr: str, json: str) -> bool:
        """Send a raw JSON payload to a direct peer address. Low-level;
        prefer streams or channels."""
        return self._native.push_to(peer_addr, json)

    def add_route(self, dest_node_id: int, next_hop_addr: str) -> None:
        """Add a routing-table entry. Low-level; routing is normally
        learned."""
        self._native.add_route(dest_node_id, next_hop_addr)

    # ── Connectivity ─────────────────────────────────────────────────

    def discovered_nodes(self) -> int:
        """How many nodes this node's proximity graph knows about."""
        return self._native.discovered_nodes()

    def traversal_stats(self) -> dict:
        """Cumulative NAT-traversal counters. ``nat-traversal`` builds
        only; ``AttributeError`` otherwise."""
        return self._native.traversal_stats()

    def connect_direct(
        self, peer_node_id: int, peer_public_key: str, coordinator: int
    ) -> None:
        """Establish a session to ``peer_node_id`` via the rendezvous path,
        ``coordinator`` mediating. An optimization: if the punch fails it
        falls back to the routed path rather than raising.
        ``nat-traversal`` builds only."""
        self._native.connect_direct(peer_node_id, peer_public_key, coordinator)

    def nat_type(self) -> str:
        """``"open" | "cone" | "symmetric" | "unknown"``; ``"unknown"`` until
        classified, or ``"open"`` at once under a reflex override.
        ``nat-traversal`` builds only."""
        return self._native.nat_type()

    def reflex_addr(self) -> Optional[str]:
        """This node's public ``ip:port`` as a peer saw it, or ``None``.
        ``nat-traversal`` builds only."""
        return self._native.reflex_addr()

    def peer_nat_type(self, peer_node_id: int) -> str:
        """The NAT class ``peer_node_id`` last advertised. ``nat-traversal``
        builds only."""
        return self._native.peer_nat_type(peer_node_id)

    def probe_reflex(self, peer_node_id: int) -> str:
        """Probe ``peer_node_id`` for this node's observed ``ip:port``.
        ``nat-traversal`` builds only."""
        return self._native.probe_reflex(peer_node_id)

    def reclassify_nat(self) -> None:
        """Re-run NAT classification now. ``nat-traversal`` builds only."""
        self._native.reclassify_nat()

    def set_reflex_override(self, external: str) -> None:
        """Pin the public reflex to ``external`` (``"ip:port"``).
        ``nat-traversal`` builds only."""
        self._native.set_reflex_override(external)

    def clear_reflex_override(self) -> None:
        """Drop a reflex override. ``nat-traversal`` builds only."""
        self._native.clear_reflex_override()

    def connect_direct_auto(self, peer_node_id: int, peer_public_key: str) -> None:
        """:meth:`connect_direct` with the coordinator chosen for you.
        ``nat-traversal`` builds only."""
        self._native.connect_direct_auto(peer_node_id, peer_public_key)

    # ── Capabilities and discovery ───────────────────────────────────
    #
    # These forward to the low-level binding. Without them the whole
    # announce/discover lifecycle was reachable only through the
    # private ``node._native`` attribute, and the published Python
    # guides said so — application code was being pushed onto an
    # internal name with no stability promise.

    def announce_capabilities(self, caps: dict) -> None:
        """Announce this node's capabilities to connected peers.

        Also self-indexes, so :meth:`find_nodes` can match this node.
        """
        self._native.announce_capabilities(caps)

    def find_nodes(self, filter: dict) -> List[int]:
        """Node ids whose latest announcement matches ``filter``.

        Returns a list — possibly empty. Compare with
        :meth:`find_best_node`, which applies the requirement's weights
        and returns a single winner.
        """
        return self._native.find_nodes(filter)

    def find_nodes_scoped(self, filter: dict, scope: dict) -> List[int]:
        """:meth:`find_nodes`, narrowed by a scope filter."""
        return self._native.find_nodes_scoped(filter, scope)

    def find_best_node(self, requirement: dict) -> Optional[int]:
        """The single best-scoring node for ``requirement``.

        ``None`` means no match. ``0`` is a real node id, so test
        ``is None`` rather than truthiness.
        """
        return self._native.find_best_node(requirement)

    def find_best_node_scoped(self, requirement: dict, scope: dict) -> Optional[int]:
        """:meth:`find_best_node`, narrowed by a scope filter."""
        return self._native.find_best_node_scoped(requirement, scope)

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
        tags_any: Optional[List[str]] = None,
        tag_groups_all: Optional[List[List[str]]] = None,
        region: Optional[str] = None,
        min_units: Optional[int] = None,
        max_load: Optional[float] = None,
        max_p50_latency_us: Optional[int] = None,
        require_all: Optional[List[str]] = None,
        require_any: Optional[List[str]] = None,
        selection: Optional[str] = None,
        load_band_target: Optional[float] = None,
        prefer_capability: Optional[str] = None,
    ) -> List[int]:
        """Match islands against the criteria over this node's folds
        (read-only; no claim). Best island first. `tags_*` / `region` filter
        the host capability match; `require_*` filter the island's resident
        capabilities. `selection` is one of ``least_loaded`` (default) /
        ``pack`` / ``load_band`` / ``lowest_id``."""
        return self._native.match_islands(
            tags_all,
            tags_any=tags_any or [],
            tag_groups_all=tag_groups_all or [],
            region=region,
            min_units=min_units,
            max_load=max_load,
            max_p50_latency_us=max_p50_latency_us,
            require_all=require_all or [],
            require_any=require_any or [],
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
        tags_any: Optional[List[str]] = None,
        tag_groups_all: Optional[List[List[str]]] = None,
        region: Optional[str] = None,
        min_units: Optional[int] = None,
        max_load: Optional[float] = None,
        max_p50_latency_us: Optional[int] = None,
        require_all: Optional[List[str]] = None,
        require_any: Optional[List[str]] = None,
        selection: Optional[str] = None,
        load_band_target: Optional[float] = None,
        prefer_capability: Optional[str] = None,
    ) -> Optional[int]:
        """Match + reserve the first available island in one call. Returns
        its id, or ``None`` when nothing matched / all contended."""
        return self._native.claim_island(
            tags_all,
            until_unix_us,
            tags_any=tags_any or [],
            tag_groups_all=tag_groups_all or [],
            region=region,
            min_units=min_units,
            max_load=max_load,
            max_p50_latency_us=max_p50_latency_us,
            require_all=require_all or [],
            require_any=require_any or [],
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


class AsyncMeshNode:
    """The asyncio twin of :class:`MeshNode`.

    Built over the same native node as a sync :class:`MeshNode` (available
    as :attr:`sync`): the verbs the wheel has async versions of are
    ``await``-able here (``connect``, ``accept``, ``subscribe_channel``,
    ``unsubscribe_channel``, ``publish``, ``announce_capabilities``,
    ``recv``, ``push_to``, ``shutdown``); everything else is a cheap sync
    call forwarded to :attr:`sync`, or reachable through it.

    Example:
        >>> node = AsyncMeshNode("127.0.0.1:0", psk)
        >>> await node.connect(addr, pubkey, peer_id)
        >>> node.start()
        >>> await node.subscribe_channel(peer_id, "sensors/temp")
        >>> async for event in node.events():
        ...     handle(event.raw)
    """

    def __init__(self, bind_addr: str, psk: str, **options: Any) -> None:
        """Same options as :class:`MeshNode`."""
        self._attach(MeshNode(bind_addr, psk, **options))

    @classmethod
    def from_node(cls, node: "MeshNode") -> "AsyncMeshNode":
        """An async view of an existing :class:`MeshNode`. Shares its
        sessions; no second socket, no re-handshake."""
        self = cls.__new__(cls)
        self._attach(node)
        return self

    def _attach(self, node: "MeshNode") -> None:
        from net import AsyncNetMesh  # type: ignore[attr-defined]

        self._sync = node
        self._native = AsyncNetMesh(node._native)

    def set_a2a_org_caller(self, org: Any) -> None:
        """:meth:`MeshNode.set_a2a_org_caller` on the wrapped node (the slot
        is the node's; installing it does not block)."""
        self._sync.set_a2a_org_caller(org)

    @property
    def sync(self) -> "MeshNode":
        """The sync :class:`MeshNode` over the same native node."""
        return self._sync

    # ── Identity and lifecycle ───────────────────────────────────────

    @property
    def node_id(self) -> int:
        return self._native.node_id

    @property
    def public_key(self) -> str:
        return self._native.public_key

    @property
    def entity_id(self) -> bytes:
        return self._native.entity_id

    @property
    def local_addr(self) -> str:
        return self._sync.local_addr

    def start(self) -> None:
        self._native.start()

    def peer_count(self) -> int:
        return self._native.peer_count()

    def discovered_nodes(self) -> int:
        return self._native.discovered_nodes()

    async def connect(self, peer_addr: str, peer_public_key: str, peer_node_id: int) -> None:
        await self._native.connect(peer_addr, peer_public_key, peer_node_id)

    async def accept(self, peer_node_id: int) -> str:
        return await self._native.accept(peer_node_id)

    async def shutdown(self) -> None:
        await self._native.shutdown()

    # ── Channels ─────────────────────────────────────────────────────

    def register_channel(self, name: str, **config: Any) -> None:
        """Sync (the wheel has no async variant; it doesn't block on the
        network). Same options as :meth:`MeshNode.register_channel`."""
        self._sync.register_channel(name, **config)

    async def subscribe_channel(
        self, publisher_node_id: int, channel: str, token: Optional[bytes] = None
    ) -> None:
        await self._native.subscribe_channel(publisher_node_id, channel, token)

    async def unsubscribe_channel(self, publisher_node_id: int, channel: str) -> None:
        await self._native.unsubscribe_channel(publisher_node_id, channel)

    async def publish(
        self,
        channel: str,
        payload: bytes,
        *,
        reliability: Optional[Reliability] = None,
        on_failure: Optional[OnFailure] = None,
        max_inflight: Optional[int] = None,
    ) -> PublishReport:
        return await self._native.publish(
            channel,
            payload,
            reliability=reliability,
            on_failure=on_failure,
            max_inflight=max_inflight,
        )

    # ── Receiving ────────────────────────────────────────────────────

    async def recv(self, limit: int) -> list:
        """Drain up to ``limit`` received events across every shard."""
        return await self._native.poll(limit)

    async def events(self, limit: int = 256, idle_sleep: float = 0.01) -> Any:
        """Yield received events as they arrive, forever.

        The shard queue has no push notification, so this drains with
        :meth:`recv` and sleeps ``idle_sleep`` seconds when it comes back
        empty. Cancel the consuming task (or ``break``) to stop.
        """
        while True:
            batch = await self.recv(limit)
            for event in batch:
                yield event
            if not batch:
                await asyncio.sleep(idle_sleep)

    def num_shards(self) -> int:
        return self._sync.num_shards()

    def shard_for_stream(self, stream_id: int) -> int:
        return self._sync.shard_for_stream(stream_id)

    def open_stream_inbox(self, stream_id: int, capacity: int = 4096) -> Any:
        """Sync: returns the wheel's pull queue (see
        :meth:`MeshNode.open_stream_inbox`)."""
        return self._sync.open_stream_inbox(stream_id, capacity)

    # ── Capabilities ─────────────────────────────────────────────────

    async def announce_capabilities(self, caps: dict) -> None:
        await self._native.announce_capabilities(caps)

    def find_nodes(self, filter: dict) -> List[int]:
        return self._native.find_nodes(filter)

    def find_nodes_scoped(self, filter: dict, scope: dict) -> List[int]:
        return self._native.find_nodes_scoped(filter, scope)

    def find_best_node(self, requirement: dict) -> Optional[int]:
        return self._native.find_best_node(requirement)

    def find_best_node_scoped(self, requirement: dict, scope: dict) -> Optional[int]:
        return self._native.find_best_node_scoped(requirement, scope)

    # ── Low-level ────────────────────────────────────────────────────

    async def push_to(self, peer_addr: str, json: str) -> bool:
        return await self._native.push_to(peer_addr, json)

    def __repr__(self) -> str:
        return f"AsyncMeshNode(node_id={self.node_id:#x})"


def _set_only(**kwargs: Any) -> dict:
    """The keyword arguments the caller actually set. Optional native
    parameters with Rust-computed defaults reject an explicit ``None``, so
    unset ones are left out rather than forwarded as ``None``."""
    return {k: v for k, v in kwargs.items() if v is not None}


def _native_mesh(mesh: Any) -> Any:
    """The native ``NetMesh`` behind ``mesh``, for native constructors
    (``DaemonRuntime``, ``MeshRpc``, …) that need one. Accepts a
    :class:`MeshNode`, an :class:`AsyncMeshNode` or a raw ``NetMesh``; anything else is a
    ``TypeError`` here rather than an opaque extraction error in Rust."""
    if isinstance(mesh, MeshNode):
        return mesh._native
    if isinstance(mesh, AsyncMeshNode):
        return mesh.sync._native
    if isinstance(mesh, _NetMesh):
        return mesh
    raise TypeError(
        f"expected a net_sdk.MeshNode or a net.NetMesh, got {type(mesh).__name__}"
    )


__all__ = [
    "AsyncMeshNode",
    "MeshNode",
    "MeshStream",
    "StreamStats",
    "Reliability",
    "Visibility",
    "OnFailure",
    "ChannelConfig",
    "PublishConfig",
    "PublishError",
    "PublishReport",
    "BackpressureError",
    "ChannelAuthError",
    "ChannelError",
    "NotConnectedError",
]
