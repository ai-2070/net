"""The event log you no longer run.

One node, one local append-only log. Eight records are appended, replayed
from the start, then replayed again from a consumer checkpoint — and the
second replay is three records, not eight, because the offset is the
consumer's own bookkeeping and the log is not asked to remember it.

Kafka needs a cluster for this. ``RedexFile`` is one file with a monotonic
sequence, and the consumer's position lives in the consumer.

Run:

    python eventlog.py

Expected final line: ``RESULT ok records=8 replayed=8 resumed=3``
"""

from __future__ import annotations

from net import Redex

# Records appended to the log.
RECORDS = 8
# The consumer's checkpoint: the next sequence it has *not* processed.
# Records 0..4 are done, so it resumes at 5.
CHECKPOINT = 5


def main() -> None:
    redex = Redex()
    log = redex.open_file("audit/events")

    try:
        for index in range(RECORDS):
            seq = log.append(f"event:{index}".encode())
            print(f"appended event {index} at seq {seq}")

        # Full replay. Every record, in append order — the log does not need a
        # broker to hand them back.
        all_events = log.read_range(0, len(log))

        # The consumer's checkpoint is application state: it holds the next
        # sequence the consumer has not processed, and the consumer reads
        # from there. ``read_range`` is half-open — ``[start, end)`` — so the
        # checkpoint is the start bound verbatim: no ``+ 1``, and no record
        # replayed twice. Nothing is committed on the log's side, which is
        # why a second consumer with a different checkpoint costs the log
        # nothing.
        resumed = log.read_range(CHECKPOINT, len(log))

        # Replay is a read, not a transformation: the same range yields the same
        # sequence, so a fold over it is reproducible.
        again = log.read_range(0, len(log))
        stable = [event.seq for event in again] == [event.seq for event in all_events]

        print(f"records in the log: {len(log)}")
        print(f"replayed from 0:    {len(all_events)}")
        print(f"resumed from {CHECKPOINT}:     {len(resumed)}")
        print(f"replay is stable:   {str(stable).lower()}")

        print(
            f"RESULT ok records={len(log)} replayed={len(all_events)} "
            f"resumed={len(resumed)}"
        )
    finally:
        log.close()


if __name__ == "__main__":
    main()
