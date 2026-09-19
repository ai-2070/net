"""Live config — the config service you no longer run.

One publisher and two subscribers, three in-process mesh nodes over loopback
UDP. The publisher registers a channel, both subscribers join by name, and every
config revision is pushed once and applied by each subscriber locally. There is
no config server to poll, no cache to invalidate and no reload to coordinate.

The distributed-channel verbs live on the low-level binding (``net.NetMesh``),
not on ``net_sdk.MeshNode`` — the wrapper has no channel surface at all.

Run:

    python liveconfig.py

Expected final line: ``RESULT ok subscribers=2 applied=2 version=2``
"""

from __future__ import annotations

import threading
import time

from net import NetMesh

# 64 hex characters = 32 bytes. Every node in a mesh shares it.
PSK = "42" * 32

DELIVER_S = 5.0

# Both subscribers are in the roster before the first publish, because
# subscribe_channel blocks on the publisher's ack.
SUBSCRIBERS = 2


def build(seed: int) -> NetMesh:
    return NetMesh(
        "127.0.0.1:0",
        PSK,
        identity_seed=bytes([seed]) * 32,
        heartbeat_interval_ms=200,
    )


def handshake(responder: NetMesh, initiator: NetMesh) -> None:
    """One side connects, the other accepts; both calls block."""
    errors: list[BaseException] = []

    def accept() -> None:
        try:
            responder.accept(initiator.node_id)
        except BaseException as error:  # noqa: BLE001 - surfaced below
            errors.append(error)

    thread = threading.Thread(target=accept, daemon=True)
    thread.start()
    time.sleep(0.05)
    initiator.connect(responder.local_addr, responder.public_key, responder.node_id)
    thread.join(timeout=5.0)
    if thread.is_alive():
        raise RuntimeError("handshake timed out")
    if errors:
        raise errors[0]


def parse(text: str) -> tuple[int, str] | None:
    """Parse ``v=<n>;mode=<name>``; unreadable revisions are ignored.

    ``StoredEvent.raw`` is the payload string itself, not a JSON envelope.
    """
    version = None
    mode = None
    for field in text.split(";"):
        if field.startswith("v="):
            try:
                version = int(field[2:])
            except ValueError:
                return None
        elif field.startswith("mode="):
            mode = field[5:]
    if version is None or mode is None:
        return None
    return (version, mode)


def check_delivery(version: int, report: dict) -> None:
    """A revision that did not reach every subscriber is not a config update.

    The publisher's own report is the evidence, so read it instead of assuming
    the fan-out worked.
    """
    if report["attempted"] != SUBSCRIBERS:
        raise RuntimeError(
            f"v{version}: roster held {report['attempted']} subscribers, "
            f"expected {SUBSCRIBERS}"
        )
    if report["delivered"] != report["attempted"] or report["errors"]:
        raise RuntimeError(
            f"v{version}: delivered to {report['delivered']} of "
            f"{report['attempted']} subscribers, errors: {report['errors']}"
        )


def drain(node: NetMesh, applied: dict[int, str], expect: tuple[int, ...]) -> int:
    """Drain every shard the bus could have routed a channel event to.

    Published events land on the shard derived from the stream id, so a consumer
    polls all of them — the low-level binding's ``poll_shard`` is the receive
    half that the ergonomic wrapper does not expose. An empty shard is an empty
    list, so a raised error means the receive itself failed: it propagates
    rather than passing for "nothing arrived".

    A quiet poll is not the end of the stream: two revisions published
    back-to-back can land one poll apart, so the only reason to stop early is
    holding every revision in ``expect``.
    """
    deadline = time.monotonic() + DELIVER_S
    seen = 0
    while True:
        for shard in range(4):
            for event in node.poll_shard(shard, 64):
                seen += 1
                parsed = parse(event.raw)
                if parsed is not None:
                    applied[parsed[0]] = parsed[1]
        if all(version in applied for version in expect):
            return seen
        if time.monotonic() >= deadline:
            return seen
        time.sleep(0.02)


def main() -> None:
    publisher = build(0xF1)
    one = build(0xF2)
    two = build(0xF3)

    handshake(publisher, one)
    handshake(publisher, two)

    publisher.start()
    one.start()
    two.start()

    try:
        # The publisher owns the channel config. No broker registers it.
        publisher.register_channel("config/edge", visibility="global", reliable=True)

        # Subscribers join by name; subscribe_channel blocks on the publisher's
        # ack, so by the time it returns this node is in the roster.
        one.subscribe_channel(publisher.node_id, "config/edge")
        two.subscribe_channel(publisher.node_id, "config/edge")

        first = publisher.publish(
            "config/edge", b"v=1;mode=blue", reliability="reliable"
        )
        print(f"published v1 to {first['delivered']} of {first['attempted']} subscribers")
        check_delivery(1, first)

        second = publisher.publish(
            "config/edge", b"v=2;mode=green", reliability="reliable"
        )
        print(f"published v2 to {second['delivered']} of {second['attempted']} subscribers")
        check_delivery(2, second)

        # Both revisions are expected on both subscribers, so drain until each
        # has them rather than until a poll comes back quiet.
        applied_one: dict[int, str] = {}
        applied_two: dict[int, str] = {}
        drain(one, applied_one, (1, 2))
        drain(two, applied_two, (1, 2))

        print(f"subscriber one applied:    {sorted(applied_one.items())}")
        print(f"subscriber two applied:    {sorted(applied_two.items())}")

        # The final line claims both subscribers applied both revisions, with
        # the modes the publisher sent. Check that before claiming it: a
        # revision that never arrived is a failure, not a quieter success.
        for name, applied_map in (("one", applied_one), ("two", applied_two)):
            for version, mode in ((1, "blue"), (2, "green")):
                if applied_map.get(version) != mode:
                    raise RuntimeError(
                        f"subscriber {name} never applied v{version}={mode}: "
                        f"{sorted(applied_map.items())}"
                    )

        applied = sum(1 for applied_map in (applied_one, applied_two) if 2 in applied_map)
        print(f"roster at publish time:    {second['attempted']}")

        print(
            f"RESULT ok subscribers={second['attempted']} applied={applied} version=2"
        )
    finally:
        publisher.shutdown()
        one.shutdown()
        two.shutdown()


if __name__ == "__main__":
    main()
