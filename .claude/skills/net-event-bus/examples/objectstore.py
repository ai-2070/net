"""The object store you no longer run.

Two in-process mesh nodes over loopback UDP and a content-addressed blob store:
a producer stores bytes and mints an address, a second node fetches them by that
address, and storing the same bytes again produces the same address. There is no
bucket to create, no region to pick and no replication factor to configure —
the address *is* the data.

Requires a binding built with the `dataforts` feature (the published wheels
carry it; a `--no-default-features` build must name it).

Run:

    python objectstore.py

Expected final line: ``RESULT ok dedup=1 readback=1 bytes=64``
"""

from __future__ import annotations

import tempfile
import threading
import time

import net_sdk.transport as transport
from net import (
    BlobRef,
    MeshBlobAdapter,
    NetMesh,
    Redex,
    blob_publish,
    register_filesystem_blob_adapter,
)

# 64 hex characters = 32 bytes. Every node in a mesh shares it.
PSK = "42" * 32

# A fixed-size payload, so the byte count in the result line is deterministic.
PAYLOAD = bytes([0x5A]) * 64


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
    holder = build(0x91)
    reader = build(0x92)

    handshake(holder, reader)
    holder.start()
    reader.start()

    try:
        # A content-addressed store backed by a local directory. `blob_publish`
        # returns the *encoded* address, which is what travels on the wire; the
        # `BlobRef` object is its parsed form.
        register_filesystem_blob_adapter("local", tempfile.mkdtemp(prefix="net-blobs-"))
        encoded = blob_publish("local", "file:orders/2026-09/payload", PAYLOAD)
        ref = BlobRef.from_encoded(encoded)
        if ref is None:
            raise RuntimeError("minted address did not decode")
        print(f"stored {len(PAYLOAD)} bytes")
        print(f"minted address: {ref.hash.hex()}")

        # Host the same bytes on the mesh so a peer can pull them. Installing the
        # transfer engine is required before serving or fetching.
        adapter = MeshBlobAdapter(Redex(), "objects")
        adapter.store(ref, PAYLOAD)
        transport.serve_blob_transfer(holder, adapter)
        transport.serve_blob_transfer(reader, MeshBlobAdapter(Redex(), "incoming"))

        # Read your own write. The reader does not have to know which node the
        # bytes settled on — the address resolves through the mesh.
        readback = transport.fetch_blob(reader, holder.node_id, ref)
        same = readback == PAYLOAD

        # Content addressing means the second store is a no-op that produces the
        # same address. Compare the content hash, not the encoded ref: the URI is
        # part of the ref, so two names for identical bytes encode differently
        # while hashing the same.
        again = blob_publish("local", "file:another/name/entirely", PAYLOAD)
        again_ref = BlobRef.from_encoded(again)
        dedup = 1 if again_ref is not None and again_ref.hash == ref.hash else 0

        round_trip = 1 if BlobRef.from_encoded(encoded) is not None else 0

        print(f"read back from the mesh:  {len(readback)} bytes")
        print(f"same bytes:               {same}")
        print(f"storing them again:       same address = {dedup == 1}")
        print(f"ref round-trips on wire:  {round_trip == 1}")

        print(f"RESULT ok dedup={dedup} readback={1 if same else 0} bytes={len(readback)}")
    finally:
        holder.shutdown()
        reader.shutdown()


if __name__ == "__main__":
    main()
