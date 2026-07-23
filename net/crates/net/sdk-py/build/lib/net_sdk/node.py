"""NetNode — the main SDK handle."""

from __future__ import annotations

import json
from dataclasses import dataclass
from typing import Any, Callable, Generic, Iterator, Optional, TypeVar, overload

from net import Net, IngestResult, StoredEvent, PollResponse, Stats

from net_sdk.stream import EventStream, SubscribeOpts, TypedEventStream
from net_sdk.channel import TypedChannel

T = TypeVar("T")


@dataclass
class Receipt:
    """Receipt from a successful ingestion."""

    shard_id: int
    timestamp: int


class NetNode:
    """
    A node on the Net mesh.

    Every computer, device, and application is a NetNode.
    There are no clients, no servers, no coordinators.

    Example:
        >>> node = NetNode(shards=4)
        >>> node.emit({'token': 'hello', 'index': 0})
        >>> for event in node.subscribe():
        ...     print(event.raw)
        >>> node.shutdown()

    Context manager:
        >>> with NetNode(shards=4) as node:
        ...     node.emit({'hello': 'world'})
    """

    def __init__(
        self,
        shards: Optional[int] = None,
        buffer_capacity: Optional[int] = None,
        backpressure: Optional[str] = None,
        *,
        # Redis transport
        redis_url: Optional[str] = None,
        redis_prefix: Optional[str] = None,
        redis_pipeline_size: Optional[int] = None,
        redis_pool_size: Optional[int] = None,
        redis_connect_timeout_ms: Optional[int] = None,
        redis_command_timeout_ms: Optional[int] = None,
        redis_max_stream_len: Optional[int] = None,
        # JetStream transport
        jetstream_url: Optional[str] = None,
        jetstream_prefix: Optional[str] = None,
        jetstream_connect_timeout_ms: Optional[int] = None,
        jetstream_request_timeout_ms: Optional[int] = None,
        jetstream_max_messages: Optional[int] = None,
        jetstream_max_bytes: Optional[int] = None,
        jetstream_max_age_ms: Optional[int] = None,
        jetstream_replicas: Optional[int] = None,
        # Mesh transport
        mesh_bind: Optional[str] = None,
        mesh_peer: Optional[str] = None,
        mesh_psk: Optional[str] = None,
        mesh_role: Optional[str] = None,
        mesh_peer_public_key: Optional[str] = None,
        mesh_secret_key: Optional[str] = None,
        mesh_public_key: Optional[str] = None,
        mesh_reliability: Optional[str] = None,
        mesh_heartbeat_interval_ms: Optional[int] = None,
        mesh_session_timeout_ms: Optional[int] = None,
        mesh_batched_io: Optional[bool] = None,
        mesh_packet_pool_size: Optional[int] = None,
    ) -> None:
        self._bus = Net(
            num_shards=shards,
            ring_buffer_capacity=buffer_capacity,
            backpressure_mode=backpressure,
            redis_url=redis_url,
            redis_prefix=redis_prefix,
            redis_pipeline_size=redis_pipeline_size,
            redis_pool_size=redis_pool_size,
            redis_connect_timeout_ms=redis_connect_timeout_ms,
            redis_command_timeout_ms=redis_command_timeout_ms,
            redis_max_stream_len=redis_max_stream_len,
            jetstream_url=jetstream_url,
            jetstream_prefix=jetstream_prefix,
            jetstream_connect_timeout_ms=jetstream_connect_timeout_ms,
            jetstream_request_timeout_ms=jetstream_request_timeout_ms,
            jetstream_max_messages=jetstream_max_messages,
            jetstream_max_bytes=jetstream_max_bytes,
            jetstream_max_age_ms=jetstream_max_age_ms,
            jetstream_replicas=jetstream_replicas,
            net_bind_addr=mesh_bind,
            net_peer_addr=mesh_peer,
            net_psk=mesh_psk,
            net_role=mesh_role,
            net_peer_public_key=mesh_peer_public_key,
            net_secret_key=mesh_secret_key,
            net_public_key=mesh_public_key,
            net_reliability=mesh_reliability,
            net_heartbeat_interval_ms=mesh_heartbeat_interval_ms,
            net_session_timeout_ms=mesh_session_timeout_ms,
            net_batched_io=mesh_batched_io,
            net_packet_pool_size=mesh_packet_pool_size,
        )

    @classmethod
    def from_bus(cls, bus: Net) -> NetNode:
        """Create a NetNode from an existing Net (PyO3) instance."""
        node = cls.__new__(cls)
        node._bus = bus
        return node

    # ---- Ingestion ----

    def emit(self, event: Any) -> Receipt:
        """
        Emit a dict or object (serializes to JSON).

        Args:
            event: Dict, dataclass, or Pydantic model to emit.

        Returns:
            Receipt with shard_id and timestamp.
        """
        if hasattr(event, "model_dump"):
            data = event.model_dump()
        elif hasattr(event, "__dict__") and not isinstance(event, dict):
            data = event.__dict__
        else:
            data = event
        result = self._bus.ingest(data) if isinstance(data, dict) else self._bus.ingest_raw(json.dumps(data))
        return Receipt(shard_id=result.shard_id, timestamp=result.timestamp)

    def emit_raw(self, json_str: str) -> Receipt:
        """
        Emit a raw JSON string (fast path).

        Args:
            json_str: JSON string to ingest.

        Returns:
            Receipt with shard_id and timestamp.
        """
        result = self._bus.ingest_raw(json_str)
        return Receipt(shard_id=result.shard_id, timestamp=result.timestamp)

    def emit_batch(self, events: list[Any]) -> int:
        """
        Emit a batch of events. Returns number ingested.

        Args:
            events: List of dicts, dataclasses, or Pydantic models.

        Returns:
            Number of events successfully ingested.
        """
        payloads: list[str] = []
        for event in events:
            if hasattr(event, "model_dump"):
                payloads.append(json.dumps(event.model_dump()))
            elif isinstance(event, dict):
                payloads.append(json.dumps(event))
            elif hasattr(event, "__dict__"):
                payloads.append(json.dumps(event.__dict__))
            else:
                payloads.append(json.dumps(event))
        return self._bus.ingest_raw_batch(payloads)

    def emit_raw_batch(self, json_strs: list[str]) -> int:
        """
        Emit a batch of raw JSON strings. Returns number ingested.
        """
        return self._bus.ingest_raw_batch(json_strs)

    def fire(self, json_str: str) -> None:
        """Fire-and-forget ingestion (no return value)."""
        self._bus.ingest_raw(json_str)

    # ---- Consumption ----

    def poll(
        self,
        limit: int = 100,
        cursor: Optional[str] = None,
        filter: Optional[str] = None,
        ordering: Optional[str] = None,
    ) -> PollResponse:
        """One-shot poll for events."""
        return self._bus.poll(limit=limit, cursor=cursor, filter=filter, ordering=ordering)

    def poll_one(self) -> Optional[StoredEvent]:
        """Poll a single event (convenience)."""
        response = self.poll(limit=1)
        if len(response) > 0:
            return response.events[0]
        return None

    def subscribe(
        self,
        limit: int = 100,
        filter: Optional[str] = None,
        ordering: Optional[str] = None,
        timeout: Optional[float] = None,
        poll_interval: float = 0.001,
        max_backoff: float = 0.1,
    ) -> EventStream:
        """
        Subscribe to a stream of events (generator).

        Example:
            >>> for event in node.subscribe(limit=100):
            ...     print(event.raw)
        """
        opts = SubscribeOpts(
            limit=limit,
            filter=filter,
            ordering=ordering,
            poll_interval=poll_interval,
            max_backoff=max_backoff,
            timeout=timeout,
        )
        return EventStream(self._bus, opts)

    def subscribe_typed(
        self,
        model: type[T],
        limit: int = 100,
        filter: Optional[str] = None,
        ordering: Optional[str] = None,
        timeout: Optional[float] = None,
    ) -> TypedEventStream[T]:
        """
        Subscribe to a typed stream of events.

        Each event is deserialized into the given model.

        Example:
            >>> for reading in node.subscribe_typed(TemperatureReading):
            ...     print(f'{reading.sensor_id}: {reading.celsius}°C')
        """
        opts = SubscribeOpts(
            limit=limit,
            filter=filter,
            ordering=ordering,
            timeout=timeout,
        )

        if hasattr(model, "model_validate_json"):
            # Pydantic model
            def parse(raw: str) -> T:
                return model.model_validate_json(raw)  # type: ignore[return-value]
        else:
            # Dataclass or plain class
            def parse(raw: str) -> T:
                return model(**json.loads(raw))  # type: ignore[return-value]

        return TypedEventStream(self._bus, parse, opts)

    def channel(
        self,
        name: str,
        model: Optional[type[T]] = None,
    ) -> TypedChannel[T]:
        """
        Create a typed channel for pub/sub.

        Example:
            >>> temps = node.channel('sensors/temperature', TemperatureReading)
            >>> temps.publish(TemperatureReading(sensor_id='A1', celsius=22.5))
        """
        return TypedChannel(self._bus, name, model=model)

    # ---- Lifecycle ----

    def stats(self) -> Stats:
        """Get ingestion statistics."""
        return self._bus.stats()

    def shards(self) -> int:
        """Get the number of active shards."""
        return self._bus.num_shards()

    def shutdown(self) -> None:
        """Gracefully shut down the node."""
        self._bus.shutdown()

    @property
    def bus(self) -> Net:
        """Get the underlying PyO3 binding (escape hatch)."""
        return self._bus

    # ---- Context manager ----

    def __enter__(self) -> NetNode:
        return self

    def __exit__(self, exc_type: Any, exc_val: Any, exc_tb: Any) -> bool:
        self.shutdown()
        return False

    def __repr__(self) -> str:
        return f"NetNode(shards={self.shards()})"
