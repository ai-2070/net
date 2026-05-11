//! Node binding for Dataforts Phase 3 blob storage.
//!
//! Mirrors the Python `_net.blob` surface:
//! - `BlobRef` napi class with `uri` / `hash` / `size` / `version`
//!   getters, `encode()`, `fromEncoded` factory.
//! - Adapter-registry lifecycle functions for the Rust-backed
//!   `FileSystemAdapter`.
//! - `blobPublish` / `blobResolve` functions that route through
//!   the registered adapter.
//!
//! Node-implemented BlobAdapters (a JS class with `store`/`fetch`/
//! `fetchRange`/`exists` methods bridged via TSFN) ship as a
//! follow-up slice — requires the async/threadsafe-function dance
//! that the placement-filter bridge already established, but blob
//! adapters need a separate dispatcher because the trait is `async`
//! end-to-end (placement is sync).

use std::path::PathBuf;
use std::sync::Arc;

use napi::bindgen_prelude::*;
use napi_derive::napi;

use ::net::adapter::net::dataforts::{
    global_blob_adapter_registry, publish_blob, resolve_payload, BlobAdapter,
    BlobError as InnerBlobError, BlobRef as InnerBlobRef, FileSystemAdapter,
};

/// Stable error-prefix for the SDK's error router. JS-side callers
/// should `e.message.startsWith("blob:")` to discriminate; no
/// dedicated `BlobError` class because napi-rs only emits plain
/// `Error`. The full shape is `"blob: <context>: <detail>"`.
pub(crate) const ERR_BLOB_PREFIX: &str = "blob:";

#[inline]
fn blob_err(context: &str, detail: impl std::fmt::Display) -> Error {
    Error::from_reason(format!("{} {}: {}", ERR_BLOB_PREFIX, context, detail))
}

fn map_blob_err(e: InnerBlobError) -> Error {
    blob_err("error", e)
}

/// Typed handle to a single content-addressed blob.
#[napi]
#[derive(Clone)]
pub struct BlobRef {
    inner: InnerBlobRef,
}

#[napi]
impl BlobRef {
    /// Construct from `(uri, hash, size)`. `hash` MUST be exactly
    /// 32 bytes; throws otherwise. The version byte is auto-set to
    /// v1 on construction.
    #[napi(constructor)]
    pub fn new(uri: String, hash: Buffer, size: BigInt) -> Result<Self> {
        if hash.len() != 32 {
            return Err(blob_err(
                "BlobRef",
                format!("hash must be 32 bytes, got {}", hash.len()),
            ));
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&hash);
        let (signed, size_u64, _losses) = size.get_u64();
        if signed {
            return Err(blob_err("BlobRef", "size must be non-negative"));
        }
        Ok(Self {
            inner: InnerBlobRef::new(uri, arr, size_u64),
        })
    }

    #[napi(getter)]
    pub fn version(&self) -> u8 {
        self.inner.version
    }

    #[napi(getter)]
    pub fn uri(&self) -> &str {
        &self.inner.uri
    }

    /// 32-byte BLAKE3 hash.
    #[napi(getter)]
    pub fn hash(&self) -> Buffer {
        Buffer::from(self.inner.hash.to_vec())
    }

    #[napi(getter)]
    pub fn size(&self) -> BigInt {
        BigInt::from(self.inner.size)
    }

    /// Wire-encoded form (discriminator + version + hash + size + uri).
    /// Pass this as the payload to `RedexFile.append` / `Mesh.publish`.
    #[napi]
    pub fn encode(&self) -> Buffer {
        Buffer::from(self.inner.encode())
    }

    /// Parse a wire-encoded form. Throws when the bytes do not
    /// start with the discriminator (use `isBlobRef(bytes)` first
    /// to peek without throwing) or when the frame is malformed /
    /// unsupported-version.
    #[napi(factory)]
    pub fn from_encoded(bytes: Buffer) -> Result<Self> {
        match InnerBlobRef::decode(&bytes).map_err(map_blob_err)? {
            Some(inner) => Ok(Self { inner }),
            None => Err(blob_err(
                "fromEncoded",
                "payload is not a blob ref (discriminator byte missing)",
            )),
        }
    }
}

impl BlobRef {
    #[allow(dead_code)]
    pub(crate) fn as_inner(&self) -> &InnerBlobRef {
        &self.inner
    }
}

/// Peek the first byte to determine whether `bytes` is a wire-
/// encoded BlobRef. Cheap; no decode, no allocation.
#[napi]
pub fn is_blob_ref(bytes: Buffer) -> bool {
    bytes
        .first()
        .copied()
        .map(|b| b == ::net::adapter::net::dataforts::BLOB_REF_DISCRIMINATOR)
        .unwrap_or(false)
}

/// Register a filesystem-backed BlobAdapter under `adapterId`.
/// `root` is the on-disk directory the adapter content-addresses
/// blobs under. Throws if `adapterId` is already in use.
#[napi]
pub fn register_filesystem_blob_adapter(adapter_id: String, root: String) -> Result<()> {
    let adapter: Arc<dyn BlobAdapter> =
        Arc::new(FileSystemAdapter::new(adapter_id.clone(), PathBuf::from(root)));
    global_blob_adapter_registry()
        .register(adapter)
        .map_err(|e| blob_err("register", e))
}

/// Remove an adapter registration. Returns `true` if an adapter
/// was removed, `false` if no adapter was registered under that id.
#[napi]
pub fn unregister_blob_adapter(adapter_id: String) -> bool {
    global_blob_adapter_registry().unregister(&adapter_id).is_some()
}

/// True if `adapterId` resolves to a registered adapter.
#[napi]
pub fn blob_adapter_registered(adapter_id: String) -> bool {
    global_blob_adapter_registry().get(&adapter_id).is_some()
}

/// Snapshot of currently-registered adapter ids.
#[napi]
pub fn blob_adapter_ids() -> Vec<String> {
    global_blob_adapter_registry().ids()
}

/// Write `data` to the registered adapter and return the encoded
/// BlobRef bytes — drop these straight into `RedexFile.append` or
/// `Mesh.publish` as the event payload.
#[napi]
pub async fn blob_publish(adapter_id: String, uri: String, data: Buffer) -> Result<Buffer> {
    let adapter = global_blob_adapter_registry()
        .get(&adapter_id)
        .ok_or_else(|| blob_err("publish", format!("adapter {:?} not registered", adapter_id)))?;
    let bytes = publish_blob(adapter.as_ref(), uri, &data)
        .await
        .map_err(map_blob_err)?;
    Ok(Buffer::from(bytes))
}

/// Resolve `payload` to its content bytes. Inline payloads come
/// back as-is; encoded-BlobRef payloads route through the adapter
/// registered under `adapterId`, fetch + verify, and return the
/// resolved bytes.
#[napi]
pub async fn blob_resolve(adapter_id: String, payload: Buffer) -> Result<Buffer> {
    let adapter = global_blob_adapter_registry()
        .get(&adapter_id)
        .ok_or_else(|| blob_err("resolve", format!("adapter {:?} not registered", adapter_id)))?;
    let bytes = resolve_payload(&payload, adapter.as_ref())
        .await
        .map_err(map_blob_err)?;
    Ok(Buffer::from(bytes))
}
