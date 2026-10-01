"""Blobs — content-addressed storage and transfer (dataforts).

The types the blob methods on :class:`net_sdk.MeshNode` take and return
(``serve_blob_transfer``, ``fetch_blob``, ``fetch_blob_discovered``,
``store_dir``, ``fetch_dir``), and the adapter registry that backs them.
Re-exported from the wheel so callers import from ``net_sdk``, not
``net``.

Example:
    >>> from net_sdk import MeshNode
    >>> from net_sdk.blob import MeshBlobAdapter
    >>> adapter = MeshBlobAdapter(...)
    >>> manifest = node.store_dir(adapter, "./assets")
    >>> peer.fetch_dir(node.node_id, manifest, "./copy")

Present iff the wheel was built with the ``dataforts`` feature (the
default build is).
"""

from __future__ import annotations

from net import (  # type: ignore[attr-defined]
    AsyncMeshBlobAdapter,
    BlobError,
    BlobRef,
    MeshBlobAdapter,
    TransferError,
    async_blob_publish,
    async_blob_resolve,
    blob_adapter_ids,
    blob_adapter_registered,
    blob_publish,
    blob_resolve,
    register_blob_adapter,
    register_filesystem_blob_adapter,
    unregister_blob_adapter,
)

__all__ = [
    "AsyncMeshBlobAdapter",
    "BlobError",
    "BlobRef",
    "MeshBlobAdapter",
    "TransferError",
    "async_blob_publish",
    "async_blob_resolve",
    "blob_adapter_ids",
    "blob_adapter_registered",
    "blob_publish",
    "blob_resolve",
    "register_blob_adapter",
    "register_filesystem_blob_adapter",
    "unregister_blob_adapter",
]
