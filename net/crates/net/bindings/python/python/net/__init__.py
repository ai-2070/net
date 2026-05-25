"""
Net - High-performance event bus for AI runtime workloads.

Example usage:

    from net import Net

    # Create event bus
    bus = Net(num_shards=4)

    # Ingest events (fast path with raw JSON string)
    result = bus.ingest_raw('{"token": "hello", "index": 0}')
    print(f"Ingested to shard {result.shard_id}")

    # Or ingest a dict (convenience method)
    bus.ingest({"token": "world", "index": 1})

    # Batch ingestion for maximum throughput
    events = [f'{{"token": "tok_{i}"}}' for i in range(1000)]
    count = bus.ingest_raw_batch(events)
    print(f"Ingested {count} events")

    # Poll events
    response = bus.poll(limit=100)
    for event in response:
        print(event.raw)

    # Get stats
    stats = bus.stats()
    print(f"Total ingested: {stats.events_ingested}")

    # Shutdown
    bus.shutdown()

    # Or use as context manager
    with Net() as bus:
        bus.ingest_raw('{"data": "value"}')
"""

from ._net import (
    IngestResult,
    Net,
    PollResponse,
    Stats,
    StoredEvent,
)

__all__ = [
    "Net",
    "IngestResult",
    "StoredEvent",
    "PollResponse",
    "Stats",
]

# Redis Streams consumer-side dedup helper. Present iff the native
# module was built with the `redis` feature.
try:
    from ._net import RedisStreamDedup
except ImportError:
    # `redis` feature not compiled in; symbol stays undefined.
    pass
else:
    __all__.append("RedisStreamDedup")


# CortEX + NetDB surface. Present iff the native module was built with
# the `cortex` feature (maturin's default picks it up).
try:
    from ._net import (
        AsyncMemoriesAdapter,
        AsyncMemoryWatchIter,
        AsyncRedexFile,
        AsyncRedexTailIter,
        AsyncTasksAdapter,
        AsyncTaskWatchIter,
        CortexError,
        MemoriesAdapter,
        Memory,
        MemoryWatchIter,
        NetDb,
        NetDbError,
        Redex,
        RedexError,
        RedexEvent,
        RedexFile,
        RedexTailIter,
        Task,
        TasksAdapter,
        TaskWatchIter,
    )
except ImportError:
    # `cortex` feature not compiled in; symbols stay undefined.
    pass
else:
    __all__.extend(
        [
            "AsyncMemoriesAdapter",
            "AsyncMemoryWatchIter",
            "AsyncRedexFile",
            "AsyncRedexTailIter",
            "AsyncTasksAdapter",
            "AsyncTaskWatchIter",
            "CortexError",
            "MemoriesAdapter",
            "Memory",
            "MemoryWatchIter",
            "NetDb",
            "NetDbError",
            "Redex",
            "RedexError",
            "RedexEvent",
            "RedexFile",
            "RedexTailIter",
            "Task",
            "TasksAdapter",
            "TaskWatchIter",
        ]
    )

# nRPC surface (Phase B3). Separate try block from the broader
# cortex import above so a wheel built with cortex but BEFORE the
# B3 surface landed (e.g. an upgrade-in-progress install) still
# exposes the legacy cortex symbols. New users get both groups
# once the wheel is rebuilt with `maturin develop`.
try:
    from ._net import (
        AsyncClientStreamCall,
        AsyncDuplexCall,
        AsyncDuplexSink,
        AsyncDuplexStream,
        AsyncMeshRpc,
        AsyncRpcStream,
        Cancellable,
        MeshRpc,
        RpcAppError,
        RpcCancelledError,
        RpcCapabilityDeniedError,
        RpcCodecError,
        RpcError,
        RpcNoRouteError,
        RpcServerError,
        RpcStream,
        RpcTimeoutError,
        RpcTransportError,
        ServeHandle,
    )
except ImportError:
    pass
else:
    __all__.extend(
        [
            "AsyncClientStreamCall",
            "AsyncDuplexCall",
            "AsyncDuplexSink",
            "AsyncDuplexStream",
            "AsyncMeshRpc",
            "AsyncRpcStream",
            "Cancellable",
            "MeshRpc",
            "RpcAppError",
            "RpcCancelledError",
            "RpcCapabilityDeniedError",
            "RpcCodecError",
            "RpcError",
            "RpcNoRouteError",
            "RpcServerError",
            "RpcStream",
            "RpcTimeoutError",
            "RpcTransportError",
            "ServeHandle",
        ]
    )

# Encrypted mesh transport + per-peer streams. Present iff the native
# module was built with the `net` feature.
try:
    from ._net import (
        AsyncNetMesh,
        AsyncNetStream,
        BackpressureError,
        ChannelAuthError,
        ChannelError,
        NetKeypair,
        NetMesh,
        NetStream,
        NetStreamStats,
        NotConnectedError,
        generate_net_keypair,
    )
except ImportError:
    # `net` feature not compiled in; symbols stay undefined.
    pass
else:
    __all__.extend(
        [
            "AsyncNetMesh",
            "AsyncNetStream",
            "BackpressureError",
            "ChannelAuthError",
            "ChannelError",
            "NetKeypair",
            "NetMesh",
            "NetStream",
            "NetStreamStats",
            "NotConnectedError",
            "generate_net_keypair",
        ]
    )

# Identity + tokens surface. Present iff the native module was built
# with the `net` feature.
try:
    from ._net import (
        Identity,
        IdentityError,
        TokenError,
        channel_hash,
        delegate_token,
        normalize_gpu_vendor,
        parse_token,
        token_is_expired,
        verify_token,
    )
except ImportError:
    pass
else:
    __all__.extend(
        [
            "Identity",
            "IdentityError",
            "TokenError",
            "channel_hash",
            "delegate_token",
            "normalize_gpu_vendor",
            "parse_token",
            "token_is_expired",
            "verify_token",
        ]
    )

# Compute runtime surface. Present iff the native module was built
# with the `compute` feature. Stage 5 of SDK_COMPUTE_SURFACE_PLAN.md.
try:
    from ._net import (
        AsyncDaemonRuntime,
        AsyncMigrationHandle,
        CausalEvent,
        DaemonError,
        DaemonHandle,
        DaemonRuntime,
        MigrationError,
        MigrationHandle,
    )
except ImportError:
    pass
else:
    __all__.extend(
        [
            "AsyncDaemonRuntime",
            "AsyncMigrationHandle",
            "CausalEvent",
            "DaemonError",
            "DaemonHandle",
            "DaemonRuntime",
            "MigrationError",
            "MigrationHandle",
            "migration_error_kind",
        ]
    )

    def migration_error_kind(exc: "MigrationError") -> str | None:
        """Extract the migration-failure kind from a caught
        ``MigrationError``.

        The Rust side encodes migration failures as messages of the form
        ``"daemon: migration: <kind>[: <detail>]"``. This helper parses
        the kind out so callers can dispatch programmatically::

            try:
                migration.wait()
            except MigrationError as e:
                kind = migration_error_kind(e)
                if kind == "not-ready":
                    # ...retriable...
                elif kind == "factory-not-found":
                    # ...terminal, target mis-configured...

        Returns ``None`` if the message doesn't start with the expected
        prefix (shouldn't happen for exceptions raised by this module).
        """
        msg = str(exc)
        prefix = "daemon: migration:"
        if not msg.startswith(prefix):
            return None
        body = msg[len(prefix) :].strip()
        colon = body.find(":")
        return body if colon == -1 else body[:colon].strip()


# Groups surface. Present iff the native module was built with
# the `groups` feature. Stage 3 of SDK_GROUPS_SURFACE_PLAN.md.
try:
    from ._net import ForkGroup, GroupError, ReplicaGroup, StandbyGroup
except ImportError:
    pass
else:
    __all__.extend(
        [
            "ForkGroup",
            "GroupError",
            "ReplicaGroup",
            "StandbyGroup",
            "group_error_kind",
        ]
    )

    def group_error_kind(exc: "GroupError") -> str | None:
        """Extract the group-failure kind from a caught
        ``GroupError``.

        The Rust side encodes group failures as messages of the form
        ``"daemon: group: <kind>[: <detail>]"``. This helper parses
        the kind out so callers can dispatch programmatically::

            try:
                ReplicaGroup.spawn(rt, "counter", ...)
            except GroupError as e:
                kind = group_error_kind(e)
                if kind == "not-ready":
                    # ...runtime not started...
                elif kind == "factory-not-found":
                    # ...kind was never registered...
                elif kind == "no-healthy-member":
                    # ...all members down...

        Returns ``None`` if the message doesn't start with the expected
        prefix (shouldn't happen for exceptions raised by this module).
        """
        msg = str(exc)
        prefix = "daemon: group:"
        if not msg.startswith(prefix):
            return None
        body = msg[len(prefix) :].strip()
        colon = body.find(":")
        return body if colon == -1 else body[:colon].strip()


# Dataforts blob surface. Present iff the native module was built
# with the `dataforts` Cargo feature. Includes the v0.15 external-
# hook adapter helpers (`register_filesystem_blob_adapter`,
# `blob_publish`, `blob_resolve`) and the v0.2 substrate-owned
# `MeshBlobAdapter` Python class.
try:
    from ._net import (
        AsyncMeshBlobAdapter,
        BlobError,
        BlobRef,
        MeshBlobAdapter,
        async_blob_publish,
        async_blob_resolve,
        blob_adapter_ids,
        blob_adapter_registered,
        blob_publish,
        blob_resolve,
        register_blob_adapter,
        register_filesystem_blob_adapter,
        unregister_blob_adapter,
    )
except ImportError:
    pass
else:
    __all__.extend(
        [
            "AsyncMeshBlobAdapter",
            "BlobError",
            "BlobRef",
            "MeshBlobAdapter",
            "async_blob_publish",
            "async_blob_resolve",
            "blob_adapter_ids",
            "blob_adapter_registered",
            "blob_publish",
            "blob_resolve",
            "register_blob_adapter",
            "register_filesystem_blob_adapter",
            "unregister_blob_adapter",
        ]
    )


# MeshOS daemon-author SDK. Present iff the native module was built
# with the `meshos` Cargo feature. Slice 1 surface — register /
# control receive / publish_log / graceful_shutdown / metadata.
try:
    from ._net import (
        MeshOsDaemonHandle,
        MeshOsDaemonSdk,
        MeshOsSdkError,
    )
except ImportError:
    # `meshos` feature not compiled in; symbols stay undefined.
    pass
else:
    __all__.extend(
        [
            "MeshOsDaemonHandle",
            "MeshOsDaemonSdk",
            "MeshOsSdkError",
            "meshos_sdk_error_kind",
        ]
    )

    def meshos_sdk_error_kind(exc: "MeshOsSdkError") -> str | None:
        """Extract the kind discriminator from a caught
        ``MeshOsSdkError``.

        The Rust side wraps every SDK error in the
        ``<<meshos-sdk-kind:KIND>>MSG`` envelope and attaches a
        ``.kind`` attribute on the exception instance. Prefer
        ``exc.kind`` over parsing; this helper is a fallback for
        callers that hold the exception's stringified form (e.g.
        bubbled through logs).

        Returns ``None`` when the message doesn't carry the
        envelope (shouldn't happen for exceptions raised by this
        module).
        """
        kind = getattr(exc, "kind", None)
        if isinstance(kind, str):
            return kind
        msg = str(exc)
        marker = "<<meshos-sdk-kind:"
        start = msg.find(marker)
        if start == -1:
            return None
        start += len(marker)
        end = msg.find(">>", start)
        if end == -1:
            return None
        return msg[start:end]


# Deck SDK surface — operator-side bindings. Present iff the
# native module was built with the `deck` Cargo feature. Slice 1
# ships client + admin + snapshot/status streams; audit / logs /
# failures land in slice 2, ICE in slice 3.
try:
    from ._net import (
        AdminCommands as _DeckAdminCommands,
    )
    from ._net import (
        DeckClient,
        DeckSdkError,
        OperatorIdentity,
    )
    from ._net import (
        SnapshotStream as _DeckSnapshotStream,
    )
    from ._net import (
        StatusSummaryStream as _DeckStatusSummaryStream,
    )
except ImportError:
    # `deck` feature not compiled in; symbols stay undefined.
    pass
else:
    # Re-export under a deck-scoped name so the symbol doesn't
    # collide with the existing `AdminCommands` namespace used by
    # the deck binary's own re-exports.
    DeckAdminCommands = _DeckAdminCommands
    DeckSnapshotStream = _DeckSnapshotStream
    DeckStatusSummaryStream = _DeckStatusSummaryStream
    __all__.extend(
        [
            "DeckClient",
            "DeckAdminCommands",
            "DeckSnapshotStream",
            "DeckStatusSummaryStream",
            "DeckSdkError",
            "OperatorIdentity",
            "deck_sdk_error_kind",
        ]
    )

    # Slice 3 — ICE break-glass surface. Try-import so wheels
    # built before slice 3 still load.
    try:
        from ._net import (
            IceCommands as _DeckIceCommands,
        )
        from ._net import (
            IceProposal as _DeckIceProposal,
        )
        from ._net import (
            SimulatedIceProposal as _DeckSimulatedIceProposal,
        )

        DeckIceCommands = _DeckIceCommands
        DeckIceProposal = _DeckIceProposal
        DeckSimulatedIceProposal = _DeckSimulatedIceProposal
        __all__.extend(
            [
                "DeckIceCommands",
                "DeckIceProposal",
                "DeckSimulatedIceProposal",
            ]
        )
    except ImportError:  # pragma: no cover
        pass

    # Operator-policy verifier surface — `OperatorRegistry` +
    # `AdminVerifier`. Try-import so wheels built before this
    # surface landed still load.
    try:
        from ._net import (
            AdminVerifier as _DeckAdminVerifier,
        )
        from ._net import (
            OperatorRegistry as _DeckOperatorRegistry,
        )

        DeckOperatorRegistry = _DeckOperatorRegistry
        DeckAdminVerifier = _DeckAdminVerifier
        __all__.extend(
            [
                "DeckOperatorRegistry",
                "DeckAdminVerifier",
            ]
        )
    except ImportError:  # pragma: no cover
        pass

    def deck_sdk_error_kind(exc: "DeckSdkError") -> str | None:
        """Extract the kind discriminator from a caught
        ``DeckSdkError``.

        The Rust side wraps every SDK error in the
        ``<<deck-sdk-kind:KIND>>MSG`` envelope and attaches a
        ``.kind`` attribute on the exception instance. Prefer
        ``exc.kind`` over parsing; this helper is a fallback for
        callers that hold the stringified form.

        Returns ``None`` when the message doesn't carry the
        envelope.
        """
        kind = getattr(exc, "kind", None)
        if isinstance(kind, str):
            return kind
        msg = str(exc)
        marker = "<<deck-sdk-kind:"
        start = msg.find(marker)
        if start == -1:
            return None
        start += len(marker)
        end = msg.find(">>", start)
        if end == -1:
            return None
        return msg[start:end]


# MeshDB surface. Present iff the native module was built with
# the `meshdb` Cargo feature. Slice 1 shipped the atomic factory
# AST + sync runner + Phase F cache options; slice 2 added the
# composite-operator factories (window, aggregates, joins) and
# the result-payload decoders; slice 3 adds the `Predicate`
# builder and the `MeshQuery.filter()` factory.
try:
    from ._net import (
        AggregateResult,
        CachePolicy,
        ExecuteOptions,
        GroupKey,
        InMemoryChainReader,
        JoinedRow,
        LineageEntry,
        MeshDbError,
        MeshQuery,
        MeshQueryRunner,
        Predicate,
        QueryBuilder,
        ResultRow,
        WindowBoundary,
    )
except ImportError:
    # `meshdb` feature not compiled in; symbols stay undefined.
    pass
else:
    __all__.extend(
        [
            "AggregateResult",
            "CachePolicy",
            "ExecuteOptions",
            "GroupKey",
            "InMemoryChainReader",
            "JoinedRow",
            "LineageEntry",
            "MeshDbError",
            "MeshQuery",
            "MeshQueryRunner",
            "Predicate",
            "QueryBuilder",
            "ResultRow",
            "WindowBoundary",
        ]
    )


# Aggregator-registry RPC client surface (`SDK_AGGREGATOR_SUBNET_PLAN.md`
# Stage 3). Present iff the native module was built with the
# `aggregator` feature (default in the maturin-shipped wheel).
try:
    from ._net import (
        DuplicateGroupName,
        FoldQueryClient,
        FoldQueryClientError,
        RegistryClient,
        RegistryClientError,
        SpawnNotSupported,
        SpawnRejected,
        UnknownFoldKind,
        UnknownTemplate,
    )
except ImportError:
    # `aggregator` feature not compiled in; symbols stay undefined.
    pass
else:
    __all__.extend(
        [
            "DuplicateGroupName",
            "FoldQueryClient",
            "FoldQueryClientError",
            "RegistryClient",
            "RegistryClientError",
            "SpawnNotSupported",
            "SpawnRejected",
            "UnknownFoldKind",
            "UnknownTemplate",
        ]
    )


__version__ = "0.23.0"
