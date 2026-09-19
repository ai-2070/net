"""The job queue you no longer run.

A producer, two workers, and an append-only job log — three in-process mesh
nodes over loopback UDP plus a local log. Jobs are appended to the log (the
queue), dispatch reads them back out of it, and a worker that *refuses* a job
re-issues it to its peer. Nothing is re-executed, and the results log is the
record you reconcile from.

Both logs live in memory for the life of the process — that is all this route
needs, and all it claims. Surviving a restart is two more arguments:
``Redex(persistent_dir=...)`` and ``open_file(name, persistent=True)``.

Run:

    python jobqueue.py

Expected final line: ``RESULT ok jobs=6 done=6 retried=1 duplicates=0``
"""

from __future__ import annotations

import threading
import time
from collections.abc import Callable
from typing import Any

from net import NetMesh, Redex
from net.mesh_rpc import (
    NRPC_TYPED_HANDLER_ERROR,
    RpcAppError,
    RpcServerError,
    TypedMeshRpc,
)

# The wire status rides in the message the binding formats:
# ``nrpc:server_error: status=0x8001 message=...``.
REFUSAL_STATUS = f"status=0x{NRPC_TYPED_HANDLER_ERROR:04x}"

# 64 hex characters = 32 bytes. Every node in a mesh shares it.
PSK = "42" * 32

JOBS = 6
# Job 3 is refused by the first worker that sees it, so the retry is observable.
POISON = 3


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


def worker_handler(
    worker_hex: str, refuse_poison: bool
) -> Callable[[dict[str, Any]], dict[str, Any]]:
    """Build a worker's handler.

    The handler echoes its own worker so a caller can prove which one ran the
    job. Only ONE worker refuses the poison job — if both did, the retry would
    fail too and nothing would be demonstrated.
    """

    def run(job: dict[str, Any]) -> dict[str, Any]:
        if refuse_poison and job["id"] == POISON:
            # `RpcAppError` so the refusal reaches the caller as a typed
            # application status rather than the generic `Internal` a bare
            # `raise` maps to — and it is raised BEFORE the job does any work,
            # which is what makes re-issuing it safe. The caller decides to
            # retry; the substrate never does it silently.
            raise RpcAppError(
                NRPC_TYPED_HANDLER_ERROR,
                f"worker {worker_hex} refused job {job['id']}",
            )
        return {"id": job["id"], "worker": worker_hex}

    return run


def main() -> None:
    producer = build(0x71)
    one = build(0x72)
    two = build(0x73)

    handshake(producer, one)
    handshake(producer, two)
    handshake(one, two)

    producer.start()
    one.start()
    two.start()

    try:
        # Announce before any call: an nRPC reply channel is bound to the
        # caller's announced identity.
        producer.announce_capabilities({"tags": ["dispatcher"]})
        one.announce_capabilities({"tags": ["worker"]})
        two.announce_capabilities({"tags": ["worker"]})
        time.sleep(0.25)

        one_id = one.node_id
        two_id = two.node_id
        one_hex = f"0x{one_id:x}"
        two_hex = f"0x{two_id:x}"

        # The queue: a local append-only log, one record per submitted job. A
        # `Redex` with no `persistent_dir` is the in-memory manager — these
        # logs are the queue for as long as the process lives, and no longer.
        redex = Redex()
        queue = redex.open_file("jobs/queue")
        results = redex.open_file("jobs/results")

        try:
            for job_id in range(1, JOBS + 1):
                seq = queue.append(f"job:{job_id}".encode())
                print(f"queued job {job_id} at seq {seq}")

            # Dispatch. The work list comes back out of the queue log, not out
            # of the loop that wrote it — the log IS the queue.
            queued: list[int] = []
            for event in queue.read_range(0, len(queue)):
                text = event.payload.decode("utf-8", "replace")
                if text.startswith("job:"):
                    queued.append(int(text[len("job:") :]))

            one_rpc = TypedMeshRpc.from_mesh(one)
            two_rpc = TypedMeshRpc.from_mesh(two)
            client = TypedMeshRpc.from_mesh(producer)

            targets = [one_id, two_id]
            retried = 0
            with one_rpc.serve(
                "run", worker_handler(one_hex, refuse_poison=True)
            ), two_rpc.serve("run", worker_handler(two_hex, refuse_poison=False)):
                for index, job_id in enumerate(queued):
                    job = {"id": job_id}
                    primary = index % len(targets)
                    secondary = (index + 1) % len(targets)
                    try:
                        done = client.call(
                            targets[primary], "run", job, opts={"deadline_ms": 5000}
                        )
                    except RpcServerError as error:
                        # ONLY the typed refusal is re-issued. A timeout or a
                        # transport fault means the call failed, not that the
                        # job did — the worker may have run it already, and
                        # re-issuing would execute it twice. Those propagate;
                        # `duplicates=0` is a claim this guard earns.
                        if REFUSAL_STATUS not in str(error):
                            raise
                        retried += 1
                        print(
                            f"job {job_id} refused by 0x{targets[primary]:x}; "
                            f"re-issuing to 0x{targets[secondary]:x}"
                        )
                        done = client.call(
                            targets[secondary], "run", job, opts={"deadline_ms": 5000}
                        )
                    results.append(f"done:{done['id']}:{done['worker']}".encode())

            # Reconcile from the results log, not from a counter kept beside
            # the dispatch loop: the completion count is whatever the log says.
            # A job id with one result record ran exactly once.
            counts: dict[str, int] = {}
            for event in results.read_range(0, len(results)):
                text = event.payload.decode("utf-8", "replace")
                if not text.startswith("done:"):
                    continue
                done_id = text[len("done:") :].split(":", 1)[0]
                counts[done_id] = counts.get(done_id, 0) + 1

            jobs_queued = len(queue)
            jobs_done = len(counts)
            duplicates = sum(1 for count in counts.values() if count > 1)

            print(f"queued:     {jobs_queued}")
            print(f"completed:  {jobs_done} (one result record each)")
            print(f"re-issued:  {retried}")

            print(
                f"RESULT ok jobs={jobs_queued} done={jobs_done} "
                f"retried={retried} duplicates={duplicates}"
            )
        finally:
            queue.close()
            results.close()
    finally:
        producer.shutdown()
        one.shutdown()
        two.shutdown()


if __name__ == "__main__":
    main()
