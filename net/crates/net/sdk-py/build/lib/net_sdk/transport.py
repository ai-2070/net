"""
Transport — on-demand cross-peer blob + directory transfer.

Sits on top of the PyO3 binding at ``net._net``. Moves content-addressed
bytes (and whole directory trees) between peers over the substrate's
reliable, fair-scheduled stream transport — distinct from RedEX
replication (a push primitive) and nRPC (request/reply). The transfer is
multiplexed fairly, so a bulk pull can't starve interactive streams.

Mirrors the Rust ``net_sdk::transport`` surface and the C ABI in
``include/net_transport.h``.

A node must install the transfer engine via :func:`serve_blob_transfer`
before it can serve chunks to peers *or* issue its own fetches.

Example::

    import net_sdk.transport as transport
    from net_sdk import MeshNode
    from net_sdk.dataforts import MeshBlobAdapter  # storage side

    transport.serve_blob_transfer(mesh, adapter)   # install once
    data = transport.fetch_blob(mesh, holder_id, blob_ref)

These symbols are exported by the PyO3 module only when the wheel is
built with the ``dataforts`` Cargo feature.
"""

from __future__ import annotations

try:
    from net import (  # type: ignore[attr-defined]
        TransferControl,
        TransferError,
        TransferHeader,
        fetch_blob,
        fetch_blob_discovered,
        fetch_dir,
        is_transfer_stream_id,
        next_transfer_stream_id,
        serve_blob_transfer,
        store_dir,
        transfer_stream_id,
    )
except ImportError as e:  # pragma: no cover — surface a clean message
    raise ImportError(
        "Transport SDK symbols not present in `net._net`. Rebuild the wheel "
        "with `--features dataforts`, e.g. `maturin develop --features dataforts`."
    ) from e


__all__ = [
    "TransferControl",
    "TransferError",
    "TransferHeader",
    "fetch_blob",
    "fetch_blob_discovered",
    "fetch_dir",
    "is_transfer_stream_id",
    "next_transfer_stream_id",
    "serve_blob_transfer",
    "store_dir",
    "transfer_stream_id",
]
