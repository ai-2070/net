"""What the drop counters actually tell you (Python).

Run: python observe.py

THE POINT OF THIS FILE, AND IT IS NOT WHAT YOU EXPECT.

The usual advice — "backpressure is silent, so watch events_dropped" — is wrong
for the one mode where it matters most, and that mode is the default.

Both counters sit at the **producer boundary**: they record what the bus
accepted or refused from you, not what survived to an adapter.

  - drop_oldest (the default) evicts to make room, so publish always succeeds
    and events_dropped never moves. Events are lost and nothing in-process
    says so.
  - drop_newest and fail_producer refuse the producer instead, so the publish
    call and events_dropped both see it.

The exact counts vary between runs and between bindings — the drain worker
races the producer. The shape is what matters, not the numbers.

Python surfaces two stats fields; there is no batch counter here.
"""

from dataclasses import dataclass

from net_sdk import NetNode


@dataclass
class Tick:
    seq: int


def measure(mode: str) -> None:
    # buffer_capacity must be a power of two and >= 1024 — both enforced at
    # construction, and neither is caught by a type checker.
    with NetNode(shards=1, buffer_capacity=1024, backpressure=mode) as node:
        ch = node.channel("ticks", Tick)
        for seq in range(20_000):
            ch.publish(Tick(seq=seq))

        stats = node.stats()
        print(
            f"{mode:<14} ingested={stats.events_ingested:<6} "
            f"dropped={stats.events_dropped}"
        )


def main() -> None:
    print("20,000 events into a 1024-slot buffer, nothing consuming:\n")
    for mode in ("drop_oldest", "drop_newest", "fail_producer"):
        measure(mode)
    print("\ndrop_oldest loses events silently — and it is the default.")
    print("The other two refuse the producer, so publish() and the counter agree.")


if __name__ == "__main__":
    main()
