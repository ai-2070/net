//! napi-rs bindings for the transport surface — blob transfer +
//! directory transfer over the fairscheduler stream transport
//! (Transport SDK plan, T-E).
//!
//! Two groups of surface:
//!
//! - **Wire contract + helpers** (standalone, no mesh handle):
//!   `TransferControl` / `TransferHeader` classes with `encode()` /
//!   static `decode()` — the postcard wire form, byte-identical across
//!   every language tier (locked by the T-B golden vectors) — and
//!   `transferStreamId` / `isTransferStreamId` / `nextTransferStreamId`.
//! - **Node-driven ops** as methods on the `NetMesh` class
//!   (`serveBlobTransfer`, `fetchBlob`, `fetchBlobDiscovered`,
//!   `storeDir`, `fetchDir`) — mirror the Python `net_sdk.transport`
//!   methods. Transfer is node-driven, so these hang off the mesh
//!   handle; the store/serve side also takes a `MeshBlobAdapter`.
//!
//! napi-derive auto-exports every `#[napi]` item here; the `transport.ts`
//! facade re-exports the standalone surface (the `NetMesh` methods ride
//! the existing `MeshNode` wrapper).

use std::fmt::Display;
use std::path::Path;
use std::sync::Arc;

use napi::bindgen_prelude::*;
use napi_derive::napi;

use net::adapter::net::dataforts::blob::transfer::{
    TransferControl as InnerTransferControl, TransferHeader as InnerTransferHeader,
};
use net::adapter::net::dataforts::blob::{
    is_transfer_stream_id as core_is_transfer_stream_id,
    next_transfer_stream_id as core_next_transfer_stream_id,
    transfer_stream_id as core_transfer_stream_id, BlobError as InnerBlobError,
    BlobRef as InnerBlobRef,
};
use net::adapter::net::dataforts::{
    fetch_dir as core_fetch_dir, store_dir as core_store_dir, DirError as InnerDirError,
};
use net::adapter::net::MeshNode;

use crate::blob::{BlobRef, MeshBlobAdapter};
use crate::mesh_bindings::NetMesh;

fn codec_err(detail: impl Display) -> Error {
    Error::from_reason(format!("transfer: {detail}"))
}

/// Transfer control frame (requester → holder): "send me the chunk
/// addressed by `hash`".
#[napi]
pub struct TransferControl {
    inner: InnerTransferControl,
}

#[napi]
impl TransferControl {
    /// Build a `Request` for the 32-byte BLAKE3 content `hash`.
    #[napi(factory)]
    pub fn request(hash: Buffer) -> Result<TransferControl> {
        let hash: [u8; 32] = hash
            .to_vec()
            .try_into()
            .map_err(|_| Error::from_reason("hash must be exactly 32 bytes"))?;
        Ok(TransferControl {
            inner: InnerTransferControl::Request { hash },
        })
    }

    /// The 32-byte content hash this control requests.
    #[napi(getter)]
    pub fn hash(&self) -> Buffer {
        let InnerTransferControl::Request { hash } = &self.inner;
        Buffer::from(hash.to_vec())
    }

    /// Postcard wire bytes (byte-identical across language tiers).
    #[napi]
    pub fn encode(&self) -> Result<Buffer> {
        Ok(Buffer::from(
            postcard::to_allocvec(&self.inner).map_err(codec_err)?,
        ))
    }

    /// Decode postcard wire bytes into a `TransferControl`.
    #[napi(factory)]
    pub fn decode(bytes: Buffer) -> Result<TransferControl> {
        Ok(TransferControl {
            inner: postcard::from_bytes(&bytes).map_err(codec_err)?,
        })
    }
}

/// Transfer header (holder → requester): the first data-plane frame,
/// declaring the total length (`Found`) or that the holder lacks the
/// chunk (`NotFound`).
#[napi]
pub struct TransferHeader {
    inner: InnerTransferHeader,
}

#[napi]
impl TransferHeader {
    /// `Found` — `totalLen` bytes of chunk data follow.
    #[napi(factory)]
    pub fn found(total_len: BigInt) -> TransferHeader {
        let (_, total_len, _) = total_len.get_u64();
        TransferHeader {
            inner: InnerTransferHeader::Found { total_len },
        }
    }

    /// `NotFound` — the holder does not have the chunk.
    #[napi(factory)]
    pub fn not_found() -> TransferHeader {
        TransferHeader {
            inner: InnerTransferHeader::NotFound,
        }
    }

    /// `true` if this is a `Found` header.
    #[napi(getter)]
    pub fn is_found(&self) -> bool {
        matches!(self.inner, InnerTransferHeader::Found { .. })
    }

    /// The declared total length for a `Found` header, else `null`.
    #[napi(getter)]
    pub fn total_len(&self) -> Option<BigInt> {
        match self.inner {
            InnerTransferHeader::Found { total_len } => Some(BigInt::from(total_len)),
            InnerTransferHeader::NotFound => None,
        }
    }

    /// Postcard wire bytes (byte-identical across language tiers).
    #[napi]
    pub fn encode(&self) -> Result<Buffer> {
        Ok(Buffer::from(
            postcard::to_allocvec(&self.inner).map_err(codec_err)?,
        ))
    }

    /// Decode postcard wire bytes into a `TransferHeader`.
    #[napi(factory)]
    pub fn decode(bytes: Buffer) -> Result<TransferHeader> {
        Ok(TransferHeader {
            inner: postcard::from_bytes(&bytes).map_err(codec_err)?,
        })
    }
}

/// Construct a transfer stream id from a per-transfer `nonce`.
#[napi]
pub fn transfer_stream_id(nonce: BigInt) -> BigInt {
    let (_, nonce, _) = nonce.get_u64();
    BigInt::from(core_transfer_stream_id(nonce))
}

/// True iff `streamId` is a blob-transfer stream id.
#[napi]
pub fn is_transfer_stream_id(stream_id: BigInt) -> bool {
    let (_, stream_id, _) = stream_id.get_u64();
    core_is_transfer_stream_id(stream_id)
}

/// Allocate a fresh, process-unique transfer stream id.
#[napi]
pub fn next_transfer_stream_id() -> BigInt {
    BigInt::from(core_next_transfer_stream_id())
}

// ── Node-driven ops (methods on NetMesh) ────────────────────────────

fn transfer_blob_err(e: InnerBlobError) -> Error {
    Error::from_reason(format!("transfer: {e}"))
}

/// Like [`transfer_blob_err`], but for holder-discovery fetches. The
/// substrate reports "no connected peer served it" as a bare
/// `NotFound`; re-tag it as "all peers failed" so the message
/// distinguishes it from a named-holder miss — mirroring the Rust SDK's
/// `TransferError::AllPeersFailed` and the C/Go `all-peers-failed` code.
fn transfer_discovered_blob_err(e: InnerBlobError) -> Error {
    match e {
        InnerBlobError::NotFound(m) => {
            Error::from_reason(format!("transfer: all peers failed: {m}"))
        }
        other => transfer_blob_err(other),
    }
}

fn transfer_dir_err(e: InnerDirError) -> Error {
    Error::from_reason(format!("transfer: {e}"))
}

/// What a `fetchDir` reconstructed.
#[napi(object)]
pub struct TransferDirStats {
    /// Files written.
    pub files: BigInt,
    /// Total file bytes written.
    pub bytes: BigInt,
}

/// Reassemble a whole blob from a known holder.
async fn fetch_blob_bytes(
    node: &Arc<MeshNode>,
    holder: u64,
    blob_ref: &InnerBlobRef,
) -> std::result::Result<Vec<u8>, InnerBlobError> {
    match blob_ref {
        InnerBlobRef::Small { hash, .. } => {
            Ok(node.transfer_fetch_chunk(holder, *hash).await?.to_vec())
        }
        InnerBlobRef::Manifest {
            chunks, total_size, ..
        } => {
            let mut buf = Vec::with_capacity(*total_size as usize);
            for chunk in chunks {
                buf.extend_from_slice(&node.transfer_fetch_chunk(holder, chunk.hash).await?);
            }
            Ok(buf)
        }
        InnerBlobRef::Tree { .. } => Err(InnerBlobError::Backend(
            "transfer: BlobRef::Tree not supported by the transport bindings".into(),
        )),
    }
}

/// Reassemble a whole blob, discovering the holder per chunk.
async fn fetch_blob_bytes_discovered(
    node: &Arc<MeshNode>,
    blob_ref: &InnerBlobRef,
) -> std::result::Result<Vec<u8>, InnerBlobError> {
    match blob_ref {
        InnerBlobRef::Small { hash, .. } => {
            Ok(node.transfer_fetch_chunk_discovered(*hash).await?.to_vec())
        }
        InnerBlobRef::Manifest {
            chunks, total_size, ..
        } => {
            let mut buf = Vec::with_capacity(*total_size as usize);
            for chunk in chunks {
                buf.extend_from_slice(&node.transfer_fetch_chunk_discovered(chunk.hash).await?);
            }
            Ok(buf)
        }
        InnerBlobRef::Tree { .. } => Err(InnerBlobError::Backend(
            "transfer: BlobRef::Tree not supported by the transport bindings".into(),
        )),
    }
}

#[napi]
impl NetMesh {
    /// Install the blob-transfer engine over `adapter`. Required before
    /// the node can serve chunks OR fetch. Idempotent.
    #[napi]
    pub fn serve_blob_transfer(&self, adapter: &MeshBlobAdapter) -> Result<()> {
        let node = self.node_arc_clone()?;
        node.serve_blob_transfer(adapter.inner_arc());
        Ok(())
    }

    /// Fetch a whole blob from the known holder `holderId`, returning the
    /// reassembled, BLAKE3-verified bytes.
    #[napi]
    pub async fn fetch_blob(&self, holder_id: BigInt, blob_ref: &BlobRef) -> Result<Buffer> {
        let (_, holder, _) = holder_id.get_u64();
        let node = self.node_arc_clone()?;
        let blob_ref = blob_ref.as_inner().clone();
        let bytes = fetch_blob_bytes(&node, holder, &blob_ref)
            .await
            .map_err(transfer_blob_err)?;
        Ok(Buffer::from(bytes))
    }

    /// Like `fetchBlob` but discovers the holder among connected peers.
    #[napi]
    pub async fn fetch_blob_discovered(&self, blob_ref: &BlobRef) -> Result<Buffer> {
        let node = self.node_arc_clone()?;
        let blob_ref = blob_ref.as_inner().clone();
        let bytes = fetch_blob_bytes_discovered(&node, &blob_ref)
            .await
            .map_err(transfer_discovered_blob_err)?;
        Ok(Buffer::from(bytes))
    }

    /// Store the local directory at `root` as content-addressed blobs in
    /// `adapter`, returning the directory-manifest `BlobRef`.
    #[napi]
    pub async fn store_dir(&self, adapter: &MeshBlobAdapter, root: String) -> Result<BlobRef> {
        let adapter = adapter.inner_arc();
        let blob_ref = core_store_dir(adapter.as_ref(), Path::new(&root))
            .await
            .map_err(transfer_dir_err)?;
        Ok(BlobRef::from_inner(blob_ref))
    }

    /// Fetch the directory whose manifest is `manifestRef` from
    /// `sourceId` and reconstruct it under `dest`.
    #[napi]
    pub async fn fetch_dir(
        &self,
        source_id: BigInt,
        manifest_ref: &BlobRef,
        dest: String,
    ) -> Result<TransferDirStats> {
        let (_, source, _) = source_id.get_u64();
        let node = self.node_arc_clone()?;
        let manifest_ref = manifest_ref.as_inner().clone();
        let stats = core_fetch_dir(&node, source, &manifest_ref, Path::new(&dest), 0)
            .await
            .map_err(transfer_dir_err)?;
        Ok(TransferDirStats {
            files: BigInt::from(stats.files as u64),
            bytes: BigInt::from(stats.bytes),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovered_not_found_is_retagged_all_peers_failed() {
        // A discovery NotFound must read as "all peers failed", distinct
        // from the named-holder "not found" a plain fetch surfaces.
        let e = transfer_discovered_blob_err(InnerBlobError::NotFound("hash xyz".into()));
        assert_eq!(e.reason, "transfer: all peers failed: hash xyz");
        // A plain fetch keeps the bare not-found message.
        let plain = transfer_blob_err(InnerBlobError::NotFound("hash xyz".into()));
        assert!(plain.reason.contains("not found"));
        assert!(!plain.reason.contains("all peers failed"));
    }

    #[test]
    fn discovered_non_not_found_falls_through_unchanged() {
        // Non-NotFound errors are not re-tagged — they map identically
        // through both paths.
        let backend = || InnerBlobError::Backend("boom".into());
        assert_eq!(
            transfer_discovered_blob_err(backend()).reason,
            transfer_blob_err(backend()).reason
        );
    }
}
