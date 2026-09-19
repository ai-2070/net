"""The sidecar you no longer run: call a service, not a host.

Two providers serve the same service name. A caller addresses the **service**
— never a node id — and when the provider that answered first goes away, the
next call lands on the survivor, with a bounded retry loop absorbing the
transient while the dead provider is still in the roster.

This is the service-mesh shape collapsed into the bus: no sidecar to
configure, no separate load balancer, no certificate rotation, and no
endpoint list to keep in sync. The roster *is* the capability fold.

Run:

    python failover.py

Expected final line: ``RESULT ok providers=2 moved=1 served=2``
"""

from __future__ import annotations

import threading
import time
from collections.abc import Callable
from typing import Any

from net import NetMesh
from net.mesh_rpc import TypedMeshRpc

# 64 hex characters = 32 bytes. Every node in a mesh shares it.
PSK = "42" * 32

# The service name both providers advertise. The caller never learns a node id.
SERVICE = "work"

# The roster keeps listing the dead provider until the capability fold
# converges, so the first post-death attempt may be spent on the corpse.
# Rust has ``call_service_typed_with_retry``; this binding does not, so the
# loop is written by hand: up to 6 attempts, ~250 ms apart.
RETRY_ATTEMPTS = 6
RETRY_INTERVAL_S = 0.25


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


def handler_for(provider: NetMesh) -> Callable[[dict[str, Any]], dict[str, Any]]:
    """Build a handler that echoes its own provider's node id.

    The caller never asked for a provider by id, so ``served_by`` is the only
    way to prove which one ran the work.
    """
    provider_id = provider.node_id

    def run(req: dict[str, Any]) -> dict[str, Any]:
        return {"served_by": provider_id, "units": req["units"]}

    return run


def main() -> None:
    caller = build(0xC1)
    provider_one = build(0xC2)
    provider_two = build(0xC3)

    # Each provider connects to the caller: the caller is the responder.
    handshake(caller, provider_one)
    handshake(caller, provider_two)
    caller.start()
    provider_one.start()
    provider_two.start()

    caller_rpc = TypedMeshRpc.from_mesh(caller)
    one_rpc = TypedMeshRpc.from_mesh(provider_one)
    two_rpc = TypedMeshRpc.from_mesh(provider_two)

    one_id = provider_one.node_id
    two_id = provider_two.node_id

    try:
        # Two servers, one service name. ``serve`` advertises ``nrpc:work`` for
        # each, which is the whole registration.
        with one_rpc.serve(SERVICE, handler_for(provider_one)), two_rpc.serve(
            SERVICE, handler_for(provider_two)
        ):
            # The caller discovers the service by name. It learns *who can
            # serve it*, never whom to prefer — that is what makes the next
            # part work.
            deadline = time.monotonic() + 5.0
            while time.monotonic() < deadline:
                if len(caller_rpc.find_service_nodes(SERVICE)) == 2:
                    break
                time.sleep(0.025)
            providers = len(caller_rpc.find_service_nodes(SERVICE))
            print(f"providers advertising `{SERVICE}`: {providers}")

            # Call the service, not a host.
            first = caller_rpc.call_service(
                SERVICE, {"units": 2}, opts={"deadline_ms": 5000}
            )
            print(f"first call served by:  {first['served_by']:#x}")

            # Take that provider out of the mesh entirely — the hard version
            # of a deploy: not a drain, a death.
            if first["served_by"] == one_id:
                provider_one.shutdown()
            else:
                provider_two.shutdown()
            print(f"took {first['served_by']:#x} out")

            # The roster still lists the dead provider until the capability
            # fold converges, so the first attempt may be spent on it. Retry
            # by hand to make that a transient rather than a caller-visible
            # failure.
            second: dict[str, Any] | None = None
            last_error: BaseException | None = None
            for _ in range(RETRY_ATTEMPTS):
                try:
                    second = caller_rpc.call_service(
                        SERVICE, {"units": 5}, opts={"deadline_ms": 5000}
                    )
                    break
                except Exception as error:  # noqa: BLE001 - the corpse is retried
                    last_error = error
                    time.sleep(RETRY_INTERVAL_S)
            if second is None:
                raise (
                    last_error
                    if last_error is not None
                    else RuntimeError("no answer after retries")
                )
            print(f"after the death served by: {second['served_by']:#x}")

            moved = int(second["served_by"] != first["served_by"])
            served = int(first["units"] == 2) + int(second["units"] == 5)
            print(f"RESULT ok providers={providers} moved={moved} served={served}")
    finally:
        caller.shutdown()
        provider_one.shutdown()
        provider_two.shutdown()


if __name__ == "__main__":
    main()