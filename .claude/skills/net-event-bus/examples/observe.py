"""Observe a drop (Python).

Run: python observe.py

What it proves: stats() reports drops, and ingestion stays quiet while they
happen.

WHY THIS EXISTS. Under the default backpressure modes nothing raises — the only
evidence a producer has that events were lost is `events_dropped`. Alert on it;
nothing else will tell you.

Python does not surface a batch counter. `events_ingested` and `events_dropped`
are the two fields to rely on.
"""

from dataclasses import dataclass

from net_sdk import NetNode


@dataclass
class Tick:
    seq: int


def main() -> None:
    with NetNode(shards=1) as node:
        ch = node.channel("ticks", Tick)

        # Far more events than the buffer holds, with nothing subscribed.
        for seq in range(1000):
            # Returns without raising, even while events are being dropped.
            ch.publish(Tick(seq=seq))

        stats = node.stats()
        print(f"ingested={stats.events_ingested} dropped={stats.events_dropped}")

        if stats.events_dropped:
            print("drops happened and nothing raised — alert on this counter")


if __name__ == "__main__":
    main()
