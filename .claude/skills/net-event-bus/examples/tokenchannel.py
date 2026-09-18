"""Scoped credentials, not an open channel.

Two nodes. The publisher owns a channel whose subscriber ACL is rooted at
its own entity id; a subscriber holding a token minted for *its* entity id,
for *this* channel, with the subscribe scope is admitted. The same
subscriber asking without the token is refused.

This is the shape a broker makes you build out of ACL files and a separate
auth service: there is one identity, one token, one place the decision is
made, and the credential is presented per subscribe rather than cached by a
connection.

Run:

    python tokenchannel.py

Expected final line: ``RESULT ok granted=1 refused=1``
"""

from __future__ import annotations

import threading
import time

from net import Identity, NetMesh

# 64 hex characters = 32 bytes. Every node in a mesh shares it.
PSK = "42" * 32

# The token's lifetime. Long enough for the example, short enough that a
# minted credential is never a standing secret.
TOKEN_TTL_S = 300

CHANNEL = "config/gated"


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


def main() -> None:
    publisher = build(0xE1)
    subscriber = build(0xE2)
    publisher_identity = Identity.from_seed(bytes([0xE1]) * 32)

    handshake(publisher, subscriber)
    publisher.start()
    subscriber.start()

    try:
        # Both nodes announce before anything is gated. A token's leaf binds to
        # the subscribing peer's EntityId, and the publisher only learns that
        # EntityId from a signature-verified announcement — so a subscriber that
        # has announced nothing is unauthorized no matter what it presents.
        publisher.announce_capabilities({})
        subscriber.announce_capabilities({})

        # Wait for the publisher to index it; the announcement is what populates
        # the publisher's peer-entity map.
        subscriber_node_id = subscriber.node_id
        deadline = time.monotonic() + 2.0
        while time.monotonic() < deadline:
            if subscriber_node_id in publisher.find_nodes({}):
                break
            time.sleep(0.025)

        # The channel's subscriber ACL is rooted at the publisher's own entity
        # id. ``token_roots`` is what turns ``require_token`` on, so this is one
        # declaration rather than a flag plus a trust anchor that can disagree.
        publisher.register_channel(
            CHANNEL,
            visibility="global",
            token_roots=[publisher_identity.entity_id],
        )
        print(
            "channel gated on a token rooted at "
            f"0x{publisher_identity.origin_hash:x}"
        )

        # A credential scoped three ways: to this subscriber's entity id, to
        # this channel, and to the subscribe action alone. It cannot publish,
        # and it is useless to any other node.
        token = publisher_identity.issue_token(
            subscriber.entity_id,
            ["subscribe"],
            CHANNEL,
            TOKEN_TTL_S,
            0,
        )
        print("issued a subscribe-only token to the subscriber")

        # Without it: refused. The publisher answers and says no, which is a
        # different outcome from "the publisher never answered".
        try:
            subscriber.subscribe_channel(publisher.node_id, CHANNEL)
            refused = False
        except Exception:  # noqa: BLE001 - a refusal is the expected outcome
            refused = True
        print(f"bare subscribe refused:            {str(refused).lower()}")

        # With it: admitted. The credential is presented on the subscribe
        # request itself, not negotiated once per connection.
        try:
            subscriber.subscribe_channel(publisher.node_id, CHANNEL, token=token)
            granted = True
        except Exception:  # noqa: BLE001 - a denial surfaces as granted=False
            granted = False
        print(f"token-carrying subscribe admitted: {str(granted).lower()}")

        print(
            f"RESULT ok granted={int(granted)} refused={int(refused)}"
        )
    finally:
        publisher.shutdown()
        subscriber.shutdown()


if __name__ == "__main__":
    main()
