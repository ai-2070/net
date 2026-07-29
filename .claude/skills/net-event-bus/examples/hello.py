"""Minimal sanity check for net-sdk (Python).

Run: python hello.py

What it proves: the SDK installs, a node starts, a typed event is accepted,
and shutdown is clean.

WHAT IT DELIBERATELY DOES NOT DO: receive the event back.

The default transport is memory, which selects the **Noop adapter**. Events
flow producer -> ring buffer -> drain worker -> adapter, and the Noop adapter
counts batches and discards them (`adapter/noop.rs`: "Just count, don't store";
its `poll_shard` returns an empty result). So on memory transport `subscribe()`
never yields — a round-trip example here would hang, not fail.

To actually receive events you need an adapter that retains them: Redis or
JetStream, or the mesh transport between two nodes. See `mesh.md`.
"""

from dataclasses import dataclass

from net_sdk import NetNode


@dataclass
class Hello:
    msg: str


def main() -> None:
    with NetNode(shards=1) as node:
        ch = node.channel("hello/world", Hello)
        ch.publish(Hello(msg="hello, mesh"))

        # `events_ingested` counts at the *producer* boundary — it says the bus
        # accepted the event, not that anything received or stored it.
        stats = node.stats()
        assert stats.events_ingested == 1, "the bus did not accept the event"
        print(f"accepted: ingested={stats.events_ingested} dropped={stats.events_dropped}")


if __name__ == "__main__":
    main()
