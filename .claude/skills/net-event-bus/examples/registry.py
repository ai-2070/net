"""The service registry you no longer run.

Four in-process mesh nodes over loopback UDP: two providers announce the same
capability, a caller discovers them and ranks them locally, a third provider
appears, and the caller's next lookup ranks it first — with no registry process,
no health-check poller, no config reload and no announcement to any address.

Run:

    python registry.py

Expected final line: ``RESULT ok providers=3 joined=1 best_moved=1``
"""

from __future__ import annotations

import threading
import time
from collections.abc import Callable

from net import NetMesh

# 64 hex characters = 32 bytes. Every node in a mesh shares it.
PSK = "42" * 32

CONVERGE_S = 5.0


def build(seed: int) -> NetMesh:
    return NetMesh(
        "127.0.0.1:0",
        PSK,
        identity_seed=bytes([seed]) * 32,
        # Tight heartbeat, like the binding's own tests, so gossip settles fast.
        heartbeat_interval_ms=200,
    )


def handshake(responder: NetMesh, initiator: NetMesh) -> None:
    """One side connects, the other accepts.

    Both calls block, so the responder's ``accept`` runs on its own thread —
    the same shape the binding's own test fixture uses. ``accept`` must be
    registered before ``start``.
    """
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


def until(what: str, predicate: Callable[[], bool]) -> None:
    """Poll the caller's local capability fold until ``predicate`` holds."""
    deadline = time.monotonic() + CONVERGE_S
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(0.025)
    raise TimeoutError(f"timed out waiting for {what}")


def main() -> None:
    # Announcements reach directly-connected peers, so the caller connects to
    # every provider it wants to see. There is no directory to join.
    a = build(0xA1)  # provider, 16 GB
    b = build(0xB2)  # provider, 64 GB
    c = build(0xC3)  # provider that appears later, 256 GB
    caller = build(0xD4)

    # Every accept() completes before any start().
    handshake(b, a)  # A <-> B
    handshake(a, caller)  # A <-> caller
    handshake(caller, b)  # B <-> caller
    handshake(caller, c)  # C <-> caller

    a.start()
    b.start()
    c.start()
    caller.start()

    try:
        # The two original providers announce. That is the entire registration.
        a.announce_capabilities({"hardware": {"memory_gb": 16}, "tags": ["api"]})
        b.announce_capabilities({"hardware": {"memory_gb": 64}, "tags": ["api"]})

        query = {"require_tags": ["api"]}
        until("both providers to appear", lambda: len(caller.find_nodes(query)) == 2)
        providers = len(caller.find_nodes(query))

        # Ranking is local and free: prefer more memory, so the bigger machine
        # wins without any scheduler deciding it centrally.
        requirement = {"filter": query, "prefer_more_memory": 1.0}
        first = caller.find_best_node(requirement)
        if first is None:
            raise RuntimeError("no provider matched")

        # A new provider appears: it announces once and is immediately
        # addressable — no registry entry, no discovery config, no reload.
        c.announce_capabilities({"hardware": {"memory_gb": 256}, "tags": ["api"]})
        until("the new provider to appear", lambda: len(caller.find_nodes(query)) == 3)
        after = len(caller.find_nodes(query))

        best = caller.find_best_node(requirement)
        if best is None:
            raise RuntimeError("no provider matched after the join")

        moved = 1 if first != best else 0
        print(f"providers found at first lookup: {providers}")
        print(f"ranked best:                     0x{first:x}")
        print(f"providers after the join:        {after}")
        print(f"ranked best:                     0x{best:x}")

        print(f"RESULT ok providers={after} joined=1 best_moved={moved}")
    finally:
        a.shutdown()
        b.shutdown()
        c.shutdown()
        caller.shutdown()


if __name__ == "__main__":
    main()
