//! Node binding for Dataforts Phase 3 blob storage.
//!
//! Mirrors the Python `_net.blob` surface:
//! - `BlobRef` napi class with `uri` / `hash` / `size` / `version`
//!   getters, `encode()`, `fromEncoded` factory.
//! - Adapter-registry lifecycle functions for the Rust-backed
//!   `FileSystemAdapter` (`registerFilesystemBlobAdapter`,
//!   `unregisterBlobAdapter`, ...).
//! - `blobPublish` / `blobResolve` functions that route through
//!   the registered adapter.
//! - `registerBlobAdapter(id, storeFn, fetchFn, fetchRangeFn,
//!   existsFn)` for JS-implemented adapters with **sync** methods.
//! - `registerAsyncBlobAdapter(...)` for JS-implemented adapters
//!   with **Promise-returning** methods. Each Promise is awaited
//!   from the substrate's tokio task; napi-rs's `Promise<T>` is
//!   `Send + Future`, so the JS event loop drives resolution back
//!   across the thread boundary.

// napi-derive registers these items via a generated `extern "C"`
// table the dead-code lint can't trace; `cargo clippy --tests`
// otherwise flags every napi function / struct / TSFN type alias
// as unused under the test profile.
#![allow(dead_code)]

use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use napi::bindgen_prelude::*;
use napi::threadsafe_function::{ThreadsafeFunction, ThreadsafeFunctionCallMode};
use napi_derive::napi;

#[cfg(feature = "dataforts")]
use ::net::adapter::net::behavior::TopologyScope;
use ::net::adapter::net::dataforts::{
    global_blob_adapter_registry, publish_blob, resolve_payload, BlobAdapter,
    BlobError as InnerBlobError, BlobRef as InnerBlobRef, FileSystemAdapter,
    MeshBlobAdapter as InnerMeshBlobAdapter, OverflowConfig as InnerOverflowConfig,
};

/// Stable error-prefix for the SDK's error router. JS-side callers
/// should `e.message.startsWith("blob:")` to discriminate; no
/// dedicated `BlobError` class because napi-rs only emits plain
/// `Error`. The full shape is `"blob: <context>: <detail>"`.
pub(crate) const ERR_BLOB_PREFIX: &str = "blob:";

/// Capability tag a node advertises when it supports the v0.3
/// hierarchical-manifest tree path (`BlobRef::Tree`). SDK
/// consumers compare advertisement payloads against this string
/// to decide whether to publish via `BlobRef::Tree` or downgrade
/// to the v0.2 `BlobRef::Manifest` shape.
#[napi]
pub const DATAFORTS_BLOB_TREE_SUPPORTED: &str =
    ::net::adapter::net::dataforts::blob::blob_tree::DATAFORTS_BLOB_TREE_SUPPORTED;

/// Capability tag a node advertises when it supports the v0.3
/// Phase B content-defined-chunking store path
/// (`ChunkingStrategy::Cdc`). Independent of the Tree tag — a
/// node can run Phase A (Tree + Fixed) without Phase B (CDC).
#[napi]
pub const DATAFORTS_BLOB_CDC_SUPPORTED: &str =
    ::net::adapter::net::dataforts::blob::cdc::DATAFORTS_BLOB_CDC_SUPPORTED;

/// Capability tag a node advertises when it supports the v0.3
/// Phase C Reed-Solomon erasure-coding store path
/// (`Encoding.reedSolomon(k, m)`). Independent of Tree/CDC tags:
/// a node can run Phase A + B without Phase C (RS). Producers
/// targeting a peer that doesn't advertise this tag must
/// downgrade to `Encoding.replicated()`.
#[napi]
pub const DATAFORTS_BLOB_ERASURE_SUPPORTED: &str =
    ::net::adapter::net::dataforts::blob::erasure::DATAFORTS_BLOB_ERASURE_SUPPORTED;

/// Capability tag a node advertises when it accepts the v0.3
/// Phase D per-stream `BandwidthClass` hint on store/fetch
/// calls. A peer that doesn't advertise this tag silently
/// drops the hint and serves every call uniformly (graceful
/// degradation — no fetch/store ever fails for missing the
/// capability).
#[napi]
pub const DATAFORTS_BLOB_BANDWIDTH_CLASS_SUPPORTED: &str =
    ::net::adapter::net::dataforts::blob::bandwidth::DATAFORTS_BLOB_BANDWIDTH_CLASS_SUPPORTED;

/// Per-stream bandwidth class hint for the v0.3 Phase D
/// blob path. SDK consumers pass an instance into future
/// chunking-aware store/fetch calls; the substrate uses it to
/// pick admission priority + send-queue ordering when D2/D3
/// land. Today it's accepted as a hint with no behavioral wiring.
#[napi]
#[derive(Clone)]
pub struct BandwidthClass {
    /// Discriminant: `"foreground"`, `"background"`, or
    /// `"realtime"`.
    pub kind: String,
}

#[napi]
impl BandwidthClass {
    /// Interactive workloads — user-driven fetches, normal RPC
    /// responses. Default class.
    #[napi(factory, js_name = "foreground")]
    pub fn foreground() -> Self {
        Self {
            kind: "foreground".to_owned(),
        }
    }

    /// Long-running TB-scale background work. Bounded to a
    /// configured fraction of per-channel rate so it can't
    /// starve Foreground.
    #[napi(factory, js_name = "background")]
    pub fn background() -> Self {
        Self {
            kind: "background".to_owned(),
        }
    }

    /// Operator-pinned. Bypasses per-class rate budget but
    /// still respects disk-pressure circuit-breakers.
    #[napi(factory, js_name = "realtime")]
    pub fn realtime() -> Self {
        Self {
            kind: "realtime".to_owned(),
        }
    }
}

/// Producer-facing encoding-strategy value type. Mirrors the
/// Rust `Encoding` enum (`Replicated` vs `ReedSolomon { k, m }`)
/// as a flat napi class with a `kind` discriminant. Construct
/// via the `replicated()` / `reedSolomon(k, m)` /
/// `defaultReedSolomon()` factories.
#[napi]
#[derive(Clone)]
pub struct Encoding {
    /// Discriminant: `"replicated"` or `"reedSolomon"`.
    pub kind: String,
    /// RS data shards per stripe — populated iff
    /// `kind == "reedSolomon"`.
    pub k: Option<u8>,
    /// RS parity shards per stripe — populated iff
    /// `kind == "reedSolomon"`.
    pub m: Option<u8>,
}

#[napi]
impl Encoding {
    /// Replication-only encoding. Each chunk stored verbatim;
    /// cross-node replication provides redundancy. Default for
    /// Phase A + B blobs.
    #[napi(factory, js_name = "replicated")]
    pub fn replicated() -> Self {
        Self {
            kind: "replicated".to_owned(),
            k: None,
            m: None,
        }
    }

    /// Reed-Solomon erasure encoding: each stripe of `k` data
    /// chunks gets `m` parity chunks; stripe survives any `m`
    /// chunk losses.
    #[napi(factory, js_name = "reedSolomon")]
    pub fn reed_solomon(k: u8, m: u8) -> Self {
        Self {
            kind: "reedSolomon".to_owned(),
            k: Some(k),
            m: Some(m),
        }
    }

    /// Production RS defaults: `(k=10, m=4)` — 1.4× storage
    /// overhead, 4-loss tolerance.
    #[napi(factory, js_name = "defaultReedSolomon")]
    pub fn default_reed_solomon() -> Self {
        Self::reed_solomon(
            ::net::adapter::net::dataforts::blob::erasure::DEFAULT_RS_K,
            ::net::adapter::net::dataforts::blob::erasure::DEFAULT_RS_M,
        )
    }
}

/// Producer-facing chunking-strategy value type for the v0.3
/// Tree store path. SDK consumers construct an instance via the
/// `fixed(size)` or `cdc(min, avg, max)` factories and pass it
/// to the future chunking-aware binding store call. The Rust
/// core's `MeshBlobAdapter::store_stream_tree` already consumes
/// the corresponding `ChunkingStrategy` enum — the binding-side
/// value is a discriminated record that round-trips through
/// JS without exposing a Rust enum directly (napi-rs has no
/// native discriminated-union encoding).
///
/// `kind` is the discriminant ("fixed" or "cdc"). The
/// shape-relevant fields are populated based on the
/// discriminant; the others are `None`. SDK consumers do not
/// hand-construct the struct — the factories ensure consistency.
#[napi]
#[derive(Clone)]
pub struct ChunkingStrategy {
    /// Discriminant: `"fixed"` for fixed-size chunks,
    /// `"cdc"` for content-defined chunking.
    pub kind: String,
    /// Chunk size in bytes. Populated iff `kind == "fixed"`.
    pub size: Option<u32>,
    /// CDC minimum chunk size in bytes. Populated iff
    /// `kind == "cdc"`.
    pub min: Option<u32>,
    /// CDC target average chunk size in bytes. Populated iff
    /// `kind == "cdc"`.
    pub avg: Option<u32>,
    /// CDC maximum chunk size in bytes. Populated iff
    /// `kind == "cdc"`.
    pub max: Option<u32>,
}

#[napi]
impl ChunkingStrategy {
    /// Fixed-size chunks. `size` must equal the v0.2-compatible
    /// `BLOB_CHUNK_SIZE_BYTES` (4 MiB) when stored via the
    /// production Tree path — other sizes fragment the cluster's
    /// chunk-level dedup pool against v0.2 blobs.
    #[napi(factory, js_name = "fixed")]
    pub fn fixed(size: u32) -> Self {
        Self {
            kind: "fixed".to_owned(),
            size: Some(size),
            min: None,
            avg: None,
            max: None,
        }
    }

    /// Content-defined chunking (FastCDC). For cluster-wide
    /// CDC dedup, pass the spec'd production triple — see
    /// `productionCdc()` for the convenience factory.
    #[napi(factory, js_name = "cdc")]
    pub fn cdc(min: u32, avg: u32, max: u32) -> Self {
        Self {
            kind: "cdc".to_owned(),
            size: None,
            min: Some(min),
            avg: Some(avg),
            max: Some(max),
        }
    }

    /// Production CDC parameters pinned by Phase B of the v0.3
    /// blob plan: `min = 1 MiB`, `avg = 4 MiB`, `max = 16 MiB`.
    /// All CDC-stored blobs on a cluster must use these exact
    /// values to dedup against each other.
    #[napi(factory, js_name = "productionCdc")]
    pub fn production_cdc() -> Self {
        let p = ::net::adapter::net::dataforts::blob::cdc::PRODUCTION_CDC_PARAMS;
        Self::cdc(p.min, p.avg, p.max)
    }
}

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
            inner: InnerBlobRef::small(uri, arr, size_u64),
        })
    }

    #[napi(getter)]
    pub fn version(&self) -> u8 {
        self.inner.version()
    }

    #[napi(getter)]
    pub fn uri(&self) -> &str {
        self.inner.uri()
    }

    /// 32-byte BLAKE3 hash. For Small (the only variant the Node
    /// constructor produces today); v0.2 will surface chunked
    /// manifests via a separate accessor.
    #[napi(getter)]
    pub fn hash(&self) -> Buffer {
        let hash = self.inner.small_hash().copied().unwrap_or([0; 32]);
        Buffer::from(hash.to_vec())
    }

    #[napi(getter)]
    pub fn size(&self) -> BigInt {
        BigInt::from(self.inner.size())
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

    /// `true` for the v0.3 [`BlobRef::Tree`] variant; `false` for
    /// Small or Manifest.
    #[napi(getter)]
    pub fn is_tree(&self) -> bool {
        self.inner.is_tree()
    }

    /// `true` for any chunked variant (Manifest or Tree); `false`
    /// for Small.
    #[napi(getter)]
    pub fn is_chunked(&self) -> bool {
        self.inner.is_chunked()
    }

    /// 32-byte BLAKE3 hash of the root `TreeNode` body. Defined
    /// only for the v0.3 `Tree` variant; returns an empty Buffer
    /// (zeros) for Small / Manifest. JS callers should check
    /// `isTree` first.
    #[napi(getter)]
    pub fn tree_root_hash(&self) -> Buffer {
        Buffer::from(
            self.inner
                .tree_root_hash()
                .copied()
                .unwrap_or([0; 32])
                .to_vec(),
        )
    }

    /// Tree depth (1..=`MAX_TREE_DEPTH`). Defined only for the
    /// v0.3 `Tree` variant; returns `0` for Small / Manifest.
    /// JS callers should check `isTree` first.
    #[napi(getter)]
    pub fn tree_depth(&self) -> u8 {
        self.inner.tree_depth().unwrap_or(0)
    }

    /// Construct a v0.3 [`BlobRef::Tree`] from `(uri,
    /// rootHash, totalSize, depth)`. `rootHash` must be exactly
    /// 32 bytes; `depth` must be in `1..=MAX_TREE_DEPTH` (= 4);
    /// `totalSize` must be in `1..=128 PiB`. Encoding defaults
    /// to `Replicated` (only encoding supported in v0.3 Phase A).
    ///
    /// Producers usually construct trees implicitly via
    /// `MeshBlobAdapter::store_stream_tree`; this factory exists
    /// for callers that hold pre-built tree state (e.g. tests,
    /// cross-language migration tooling).
    #[napi(factory, js_name = "treeFromParts")]
    pub fn tree_from_parts(
        uri: String,
        root_hash: Buffer,
        total_size: BigInt,
        depth: u8,
    ) -> Result<Self> {
        if root_hash.len() != 32 {
            return Err(blob_err(
                "treeFromParts",
                format!("rootHash must be 32 bytes, got {}", root_hash.len()),
            ));
        }
        let mut hash = [0u8; 32];
        hash.copy_from_slice(&root_hash);
        // `get_u64()` returns `(signed, value, lossless)`. The
        // lossless flag is `true` only when the JS BigInt
        // round-trips through u64 without truncation. JS BigInts
        // can hold arbitrarily large integers; a value above
        // u64::MAX would silently truncate without this check,
        // letting the constructor accept a forged "small" Tree
        // shape derived from a producer's accidental 2^64+1.
        let (signed, total_u64, lossless) = total_size.get_u64();
        if signed {
            return Err(blob_err("treeFromParts", "totalSize must be non-negative"));
        }
        if !lossless {
            return Err(blob_err(
                "treeFromParts",
                "totalSize exceeds u64::MAX; tree total_size is a u64 field",
            ));
        }
        InnerBlobRef::tree(
            uri,
            ::net::adapter::net::dataforts::Encoding::Replicated,
            hash,
            total_u64,
            depth,
        )
        .map(|inner| Self { inner })
        .map_err(map_blob_err)
    }
}

impl BlobRef {
    #[allow(dead_code)]
    pub(crate) fn as_inner(&self) -> &InnerBlobRef {
        &self.inner
    }

    /// Wrap a Rust `BlobRef` into the napi facade. Used by
    /// adapter methods (e.g. `store_stream_tree_from_bytes`)
    /// that produce a fresh BlobRef on the Rust side.
    pub(crate) fn from_inner(inner: InnerBlobRef) -> Self {
        Self { inner }
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
    let adapter: Arc<dyn BlobAdapter> = Arc::new(FileSystemAdapter::new(
        adapter_id.clone(),
        PathBuf::from(root),
    ));
    global_blob_adapter_registry()
        .register(adapter)
        .map_err(|e| blob_err("register", e))
}

/// Remove an adapter registration. Returns `true` if an adapter
/// was removed, `false` if no adapter was registered under that id.
#[napi]
pub fn unregister_blob_adapter(adapter_id: String) -> bool {
    global_blob_adapter_registry()
        .unregister(&adapter_id)
        .is_some()
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
        .ok_or_else(|| {
            blob_err(
                "publish",
                format!("adapter {:?} not registered", adapter_id),
            )
        })?;
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
        .ok_or_else(|| {
            blob_err(
                "resolve",
                format!("adapter {:?} not registered", adapter_id),
            )
        })?;
    let bytes = resolve_payload(&payload, adapter.as_ref())
        .await
        .map_err(map_blob_err)?;
    Ok(Buffer::from(bytes))
}

// =========================================================================
// JS-implemented BlobAdapter — TSFN bridge
// =========================================================================

/// Per-call timeout for any TSFN method on a JS-implemented blob
/// adapter. JS work runs on the Node event loop; a slow / blocked
/// loop should not hang the substrate forever. Operators that
/// genuinely need longer call budgets can register a
/// Rust-backed adapter instead.
const DEFAULT_JS_ADAPTER_TIMEOUT: Duration = Duration::from_secs(30);

/// Args handed to the JS `store` function.
#[napi(object)]
pub struct JsBlobStoreArgs {
    pub uri: String,
    pub hash: Buffer,
    pub size: BigInt,
    pub data: Buffer,
}

/// Args handed to the JS `fetch` / `exists` functions.
#[napi(object)]
pub struct JsBlobFetchArgs {
    pub uri: String,
    pub hash: Buffer,
    pub size: BigInt,
}

/// Args handed to the JS `fetchRange` function.
#[napi(object)]
pub struct JsBlobFetchRangeArgs {
    pub uri: String,
    pub hash: Buffer,
    pub size: BigInt,
    pub start: BigInt,
    pub end: BigInt,
}

type StoreTsfn = ThreadsafeFunction<JsBlobStoreArgs, (), JsBlobStoreArgs, napi::Status, false>;
type FetchTsfn = ThreadsafeFunction<JsBlobFetchArgs, Buffer, JsBlobFetchArgs, napi::Status, false>;
type FetchRangeTsfn =
    ThreadsafeFunction<JsBlobFetchRangeArgs, Buffer, JsBlobFetchRangeArgs, napi::Status, false>;
type ExistsTsfn = ThreadsafeFunction<JsBlobFetchArgs, bool, JsBlobFetchArgs, napi::Status, false>;

/// `BlobAdapter` impl that bridges to JS-side functions via four
/// TSFNs (one per trait method). Each call posts work onto the
/// Node event loop and awaits its return via a `tokio::oneshot`
/// channel, so the substrate's tokio worker doesn't block on the
/// Node thread.
pub struct NodeBlobAdapter {
    id: String,
    store: StoreTsfn,
    fetch: FetchTsfn,
    fetch_range: FetchRangeTsfn,
    exists: ExistsTsfn,
    timeout: Duration,
}

impl NodeBlobAdapter {
    pub fn new(
        id: String,
        store: StoreTsfn,
        fetch: FetchTsfn,
        fetch_range: FetchRangeTsfn,
        exists: ExistsTsfn,
    ) -> Self {
        Self::new_with_timeout(
            id,
            store,
            fetch,
            fetch_range,
            exists,
            DEFAULT_JS_ADAPTER_TIMEOUT,
        )
    }

    pub fn new_with_timeout(
        id: String,
        store: StoreTsfn,
        fetch: FetchTsfn,
        fetch_range: FetchRangeTsfn,
        exists: ExistsTsfn,
        timeout: Duration,
    ) -> Self {
        Self {
            id,
            store,
            fetch,
            fetch_range,
            exists,
            timeout,
        }
    }
}

async fn await_tsfn<T, F>(
    timeout: Duration,
    label: &'static str,
    enqueue: F,
) -> std::result::Result<T, InnerBlobError>
where
    T: Send + 'static,
    F: FnOnce(Box<dyn FnOnce(napi::Result<T>) + Send>) -> napi::Status,
{
    let (tx, rx) = tokio::sync::oneshot::channel::<napi::Result<T>>();
    let callback: Box<dyn FnOnce(napi::Result<T>) + Send> = Box::new(move |ret| {
        let _ = tx.send(ret);
    });
    let status = enqueue(callback);
    if status != napi::Status::Ok {
        return Err(InnerBlobError::Backend(format!(
            "{}: TSFN enqueue status {:?}",
            label, status
        )));
    }
    match tokio::time::timeout(timeout, rx).await {
        Ok(Ok(Ok(value))) => Ok(value),
        Ok(Ok(Err(e))) => Err(InnerBlobError::Backend(format!(
            "{}: JS error: {}",
            label, e
        ))),
        Ok(Err(_)) => Err(InnerBlobError::Backend(format!(
            "{}: TSFN callback channel disconnected",
            label
        ))),
        Err(_) => Err(InnerBlobError::Backend(format!(
            "{}: JS did not respond within {} ms",
            label,
            timeout.as_millis()
        ))),
    }
}

fn js_blob_ref_parts(blob_ref: &InnerBlobRef) -> (String, Buffer, BigInt) {
    // Node adapter callbacks operate on Small blobs only; the
    // substrate's MeshBlobAdapter (v0.2) handles manifest dispatch
    // before reaching the FFI shim. A Manifest reaching this helper
    // is a layering bug — fall through with a zero hash so the
    // downstream JS error path surfaces a typed mismatch rather
    // than a panic.
    let hash = blob_ref.small_hash().copied().unwrap_or([0; 32]);
    (
        blob_ref.uri().to_owned(),
        Buffer::from(hash.to_vec()),
        BigInt::from(blob_ref.size()),
    )
}

#[async_trait]
impl BlobAdapter for NodeBlobAdapter {
    fn adapter_id(&self) -> &str {
        &self.id
    }

    async fn store(
        &self,
        blob_ref: &InnerBlobRef,
        bytes: &[u8],
    ) -> std::result::Result<(), InnerBlobError> {
        let (uri, hash, size) = js_blob_ref_parts(blob_ref);
        let args = JsBlobStoreArgs {
            uri,
            hash,
            size,
            data: Buffer::from(bytes.to_vec()),
        };
        await_tsfn(self.timeout, "store", |cb| {
            self.store.call_with_return_value(
                args,
                ThreadsafeFunctionCallMode::NonBlocking,
                move |ret, _env| {
                    cb(ret);
                    Ok(())
                },
            )
        })
        .await
    }

    async fn fetch(&self, blob_ref: &InnerBlobRef) -> std::result::Result<Vec<u8>, InnerBlobError> {
        let (uri, hash, size) = js_blob_ref_parts(blob_ref);
        let args = JsBlobFetchArgs { uri, hash, size };
        let buf = await_tsfn::<Buffer, _>(self.timeout, "fetch", |cb| {
            self.fetch.call_with_return_value(
                args,
                ThreadsafeFunctionCallMode::NonBlocking,
                move |ret, _env| {
                    cb(ret);
                    Ok(())
                },
            )
        })
        .await?;
        Ok(buf.to_vec())
    }

    async fn fetch_range(
        &self,
        blob_ref: &InnerBlobRef,
        range: Range<u64>,
    ) -> std::result::Result<Vec<u8>, InnerBlobError> {
        let (uri, hash, size) = js_blob_ref_parts(blob_ref);
        let args = JsBlobFetchRangeArgs {
            uri,
            hash,
            size,
            start: BigInt::from(range.start),
            end: BigInt::from(range.end),
        };
        let buf = await_tsfn::<Buffer, _>(self.timeout, "fetch_range", |cb| {
            self.fetch_range.call_with_return_value(
                args,
                ThreadsafeFunctionCallMode::NonBlocking,
                move |ret, _env| {
                    cb(ret);
                    Ok(())
                },
            )
        })
        .await?;
        Ok(buf.to_vec())
    }

    async fn exists(&self, blob_ref: &InnerBlobRef) -> std::result::Result<bool, InnerBlobError> {
        let (uri, hash, size) = js_blob_ref_parts(blob_ref);
        let args = JsBlobFetchArgs { uri, hash, size };
        await_tsfn(self.timeout, "exists", |cb| {
            self.exists.call_with_return_value(
                args,
                ThreadsafeFunctionCallMode::NonBlocking,
                move |ret, _env| {
                    cb(ret);
                    Ok(())
                },
            )
        })
        .await
    }
}

/// Register a JS-implemented BlobAdapter. The four function args
/// MUST be sync JS functions returning the per-method shape:
///
/// - `storeFn({ uri, hash, size, data }) -> void`
/// - `fetchFn({ uri, hash, size }) -> Buffer`
/// - `fetchRangeFn({ uri, hash, size, start, end }) -> Buffer`
/// - `existsFn({ uri, hash, size }) -> boolean`
///
/// JS-thrown errors collapse to `BlobError::Backend(err)`. Per-call
/// `timeoutMs` defaults to 30 s; longer-running adapters should be
/// implemented in Rust. Pass `null` / omit to keep the default.
///
/// For Promise-returning JS methods, use
/// [`register_async_blob_adapter`] instead.
#[napi]
pub fn register_blob_adapter(
    id: String,
    store_fn: Function<'static, JsBlobStoreArgs, ()>,
    fetch_fn: Function<'static, JsBlobFetchArgs, Buffer>,
    fetch_range_fn: Function<'static, JsBlobFetchRangeArgs, Buffer>,
    exists_fn: Function<'static, JsBlobFetchArgs, bool>,
    timeout_ms: Option<u32>,
) -> Result<()> {
    let store: StoreTsfn = store_fn.build_threadsafe_function().build()?;
    let fetch: FetchTsfn = fetch_fn.build_threadsafe_function().build()?;
    let fetch_range: FetchRangeTsfn = fetch_range_fn.build_threadsafe_function().build()?;
    let exists: ExistsTsfn = exists_fn.build_threadsafe_function().build()?;
    let timeout = timeout_ms
        .map(|ms| Duration::from_millis(ms as u64))
        .unwrap_or(DEFAULT_JS_ADAPTER_TIMEOUT);
    let wrapper =
        NodeBlobAdapter::new_with_timeout(id.clone(), store, fetch, fetch_range, exists, timeout);
    let arc: Arc<dyn BlobAdapter> = Arc::new(wrapper);
    global_blob_adapter_registry()
        .register(arc)
        .map_err(|e| blob_err("register_blob_adapter", e))
}

// =========================================================================
// JS-implemented BlobAdapter (async) — TSFN bridge over Promise<T>
// =========================================================================

type StoreAsyncTsfn =
    ThreadsafeFunction<JsBlobStoreArgs, Promise<()>, JsBlobStoreArgs, napi::Status, false>;
type FetchAsyncTsfn =
    ThreadsafeFunction<JsBlobFetchArgs, Promise<Buffer>, JsBlobFetchArgs, napi::Status, false>;
type FetchRangeAsyncTsfn = ThreadsafeFunction<
    JsBlobFetchRangeArgs,
    Promise<Buffer>,
    JsBlobFetchRangeArgs,
    napi::Status,
    false,
>;
type ExistsAsyncTsfn =
    ThreadsafeFunction<JsBlobFetchArgs, Promise<bool>, JsBlobFetchArgs, napi::Status, false>;

/// `BlobAdapter` impl that bridges to JS async functions (returning
/// Promises). Each call goes TSFN → await the returned Promise from
/// the substrate's tokio task. napi-rs's `Promise<T>` is `Send` +
/// `Future<Output = napi::Result<T>>`, so the await drives the JS
/// event loop's resolution back to this thread.
pub struct NodeAsyncBlobAdapter {
    id: String,
    store: StoreAsyncTsfn,
    fetch: FetchAsyncTsfn,
    fetch_range: FetchRangeAsyncTsfn,
    exists: ExistsAsyncTsfn,
    timeout: Duration,
}

impl NodeAsyncBlobAdapter {
    pub fn new(
        id: String,
        store: StoreAsyncTsfn,
        fetch: FetchAsyncTsfn,
        fetch_range: FetchRangeAsyncTsfn,
        exists: ExistsAsyncTsfn,
    ) -> Self {
        Self::new_with_timeout(
            id,
            store,
            fetch,
            fetch_range,
            exists,
            DEFAULT_JS_ADAPTER_TIMEOUT,
        )
    }

    pub fn new_with_timeout(
        id: String,
        store: StoreAsyncTsfn,
        fetch: FetchAsyncTsfn,
        fetch_range: FetchRangeAsyncTsfn,
        exists: ExistsAsyncTsfn,
        timeout: Duration,
    ) -> Self {
        Self {
            id,
            store,
            fetch,
            fetch_range,
            exists,
            timeout,
        }
    }
}

/// Get the Promise back from the TSFN, then await it, all inside
/// the same `tokio::time::timeout` window. `enqueue` is invoked
/// synchronously before the first await.
async fn await_tsfn_promise<T, F>(
    timeout: Duration,
    label: &'static str,
    enqueue: F,
) -> std::result::Result<T, InnerBlobError>
where
    T: FromNapiValue + Send + 'static,
    F: FnOnce(Box<dyn FnOnce(napi::Result<Promise<T>>) + Send>) -> napi::Status,
{
    let (tx, rx) = tokio::sync::oneshot::channel::<napi::Result<Promise<T>>>();
    let callback: Box<dyn FnOnce(napi::Result<Promise<T>>) + Send> = Box::new(move |ret| {
        let _ = tx.send(ret);
    });
    let status = enqueue(callback);
    if status != napi::Status::Ok {
        return Err(InnerBlobError::Backend(format!(
            "{}: TSFN enqueue status {:?}",
            label, status
        )));
    }
    // Total-budget the two stages (TSFN-to-Promise + Promise resolve)
    // against a single deadline so the worst case is `timeout`, not
    // 2*timeout. Without this an async adapter waiting on a slow
    // Promise after the TSFN stage already consumed most of the
    // budget would see effective `2*timeout`.
    let deadline = tokio::time::Instant::now() + timeout;
    let promise_step = tokio::time::timeout_at(deadline, rx).await;
    let promise = match promise_step {
        Ok(Ok(Ok(p))) => p,
        Ok(Ok(Err(e))) => {
            return Err(InnerBlobError::Backend(format!(
                "{}: JS threw before returning Promise: {}",
                label, e
            )))
        }
        Ok(Err(_)) => {
            return Err(InnerBlobError::Backend(format!(
                "{}: TSFN callback channel disconnected",
                label
            )))
        }
        Err(_) => {
            return Err(InnerBlobError::Backend(format!(
                "{}: JS did not return Promise within {} ms",
                label,
                timeout.as_millis()
            )))
        }
    };
    match tokio::time::timeout_at(deadline, promise).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(e)) => Err(InnerBlobError::Backend(format!(
            "{}: Promise rejected: {}",
            label, e
        ))),
        Err(_) => Err(InnerBlobError::Backend(format!(
            "{}: Promise did not resolve within {} ms (total budget)",
            label,
            timeout.as_millis()
        ))),
    }
}

#[async_trait]
impl BlobAdapter for NodeAsyncBlobAdapter {
    fn adapter_id(&self) -> &str {
        &self.id
    }

    async fn store(
        &self,
        blob_ref: &InnerBlobRef,
        bytes: &[u8],
    ) -> std::result::Result<(), InnerBlobError> {
        let (uri, hash, size) = js_blob_ref_parts(blob_ref);
        let args = JsBlobStoreArgs {
            uri,
            hash,
            size,
            data: Buffer::from(bytes.to_vec()),
        };
        await_tsfn_promise(self.timeout, "store", |cb| {
            self.store.call_with_return_value(
                args,
                ThreadsafeFunctionCallMode::NonBlocking,
                move |ret, _env| {
                    cb(ret);
                    Ok(())
                },
            )
        })
        .await
    }

    async fn fetch(&self, blob_ref: &InnerBlobRef) -> std::result::Result<Vec<u8>, InnerBlobError> {
        let (uri, hash, size) = js_blob_ref_parts(blob_ref);
        let args = JsBlobFetchArgs { uri, hash, size };
        let buf = await_tsfn_promise::<Buffer, _>(self.timeout, "fetch", |cb| {
            self.fetch.call_with_return_value(
                args,
                ThreadsafeFunctionCallMode::NonBlocking,
                move |ret, _env| {
                    cb(ret);
                    Ok(())
                },
            )
        })
        .await?;
        Ok(buf.to_vec())
    }

    async fn fetch_range(
        &self,
        blob_ref: &InnerBlobRef,
        range: Range<u64>,
    ) -> std::result::Result<Vec<u8>, InnerBlobError> {
        let (uri, hash, size) = js_blob_ref_parts(blob_ref);
        let args = JsBlobFetchRangeArgs {
            uri,
            hash,
            size,
            start: BigInt::from(range.start),
            end: BigInt::from(range.end),
        };
        let buf = await_tsfn_promise::<Buffer, _>(self.timeout, "fetch_range", |cb| {
            self.fetch_range.call_with_return_value(
                args,
                ThreadsafeFunctionCallMode::NonBlocking,
                move |ret, _env| {
                    cb(ret);
                    Ok(())
                },
            )
        })
        .await?;
        Ok(buf.to_vec())
    }

    async fn exists(&self, blob_ref: &InnerBlobRef) -> std::result::Result<bool, InnerBlobError> {
        let (uri, hash, size) = js_blob_ref_parts(blob_ref);
        let args = JsBlobFetchArgs { uri, hash, size };
        await_tsfn_promise(self.timeout, "exists", |cb| {
            self.exists.call_with_return_value(
                args,
                ThreadsafeFunctionCallMode::NonBlocking,
                move |ret, _env| {
                    cb(ret);
                    Ok(())
                },
            )
        })
        .await
    }
}

/// Register a JS-implemented BlobAdapter whose methods return
/// Promises. The four function args MUST be JS functions returning
/// `Promise<...>` of the per-method shape:
///
/// - `storeFn({ uri, hash, size, data }) -> Promise<void>`
/// - `fetchFn({ uri, hash, size }) -> Promise<Buffer>`
/// - `fetchRangeFn({ uri, hash, size, start, end }) -> Promise<Buffer>`
/// - `existsFn({ uri, hash, size }) -> Promise<boolean>`
///
/// The substrate awaits each Promise from a tokio task; the JS
/// event loop drives resolution. Per-call `timeoutMs` defaults to
/// 30 s (the same as the sync bridge) and is the **total budget**
/// across both stages — JS returning the Promise, and the Promise
/// resolving. Pass `null` / omit to keep the default.
///
/// Rejected Promises collapse to `BlobError::Backend(reason)`.
#[napi]
pub fn register_async_blob_adapter(
    id: String,
    store_fn: Function<'static, JsBlobStoreArgs, Promise<()>>,
    fetch_fn: Function<'static, JsBlobFetchArgs, Promise<Buffer>>,
    fetch_range_fn: Function<'static, JsBlobFetchRangeArgs, Promise<Buffer>>,
    exists_fn: Function<'static, JsBlobFetchArgs, Promise<bool>>,
    timeout_ms: Option<u32>,
) -> Result<()> {
    let store: StoreAsyncTsfn = store_fn.build_threadsafe_function().build()?;
    let fetch: FetchAsyncTsfn = fetch_fn.build_threadsafe_function().build()?;
    let fetch_range: FetchRangeAsyncTsfn = fetch_range_fn.build_threadsafe_function().build()?;
    let exists: ExistsAsyncTsfn = exists_fn.build_threadsafe_function().build()?;
    let timeout = timeout_ms
        .map(|ms| Duration::from_millis(ms as u64))
        .unwrap_or(DEFAULT_JS_ADAPTER_TIMEOUT);
    let wrapper = NodeAsyncBlobAdapter::new_with_timeout(
        id.clone(),
        store,
        fetch,
        fetch_range,
        exists,
        timeout,
    );
    let arc: Arc<dyn BlobAdapter> = Arc::new(wrapper);
    global_blob_adapter_registry()
        .register(arc)
        .map_err(|e| blob_err("register_async_blob_adapter", e))
}

// =========================================================================
// MeshBlobAdapter — v0.2 substrate-owned blob CAS (+ v0.3 overflow)
// =========================================================================
//
// Mirrors the Python `MeshBlobAdapter` surface for Node operator
// scripts. The adapter is Rust-backed (chunks live in Redex as
// content-addressed files); JS callers get a thin napi wrapper
// over `Arc<InnerMeshBlobAdapter>`.

/// Operator-tunable knobs for the v0.3 active-overflow controller.
/// Mirrors the Rust [`InnerOverflowConfig`] shape. Pass at
/// construction via [`MeshBlobAdapter::new`]'s `overflow` option,
/// or replace at runtime via [`MeshBlobAdapter::setOverflowConfig`].
///
/// Every field except `enabled` is optional — omit any knob to
/// inherit the Rust-side default. Matches the Go and Python
/// bindings' partial-config posture. Pass `{ enabled: true }` to
/// turn overflow on with all-default thresholds; pass
/// `{ enabled: true, highWaterRatio: 0.90 }` to override one knob.
#[napi(object)]
pub struct OverflowConfigJs {
    /// Master switch. `false` = adapter never pushes, never
    /// advertises the `dataforts.blob.overflow` capability tag,
    /// never accepts inbound `OverflowPush` requests. Default
    /// for new adapters.
    pub enabled: bool,
    /// Disk usage ratio at or above which the overflow tick
    /// fires. Default `0.85`.
    pub high_water_ratio: Option<f64>,
    /// Disk usage ratio at or below which the controller
    /// re-enters the inactive state. Hysteresis band between
    /// `lowWaterRatio` and `highWaterRatio` preserves the
    /// prior active state. Default `0.70`.
    pub low_water_ratio: Option<f64>,
    /// Per-tick push budget. Each push opens a chunk channel
    /// with replication armed; this caps the bandwidth burst.
    /// Default `16`.
    pub max_pushes_per_tick: Option<u32>,
    /// Topology scope bound on push-target selection: one of
    /// `"node"` / `"zone"` / `"region"` / `"mesh"`. Default
    /// `"mesh"`.
    pub scope: Option<String>,
    /// Tick cadence in milliseconds. Default `30000`.
    pub tick_interval_ms: Option<u32>,
}

#[cfg(feature = "dataforts")]
fn overflow_config_from_inner(cfg: InnerOverflowConfig) -> OverflowConfigJs {
    OverflowConfigJs {
        enabled: cfg.enabled,
        high_water_ratio: Some(cfg.high_water_ratio),
        low_water_ratio: Some(cfg.low_water_ratio),
        max_pushes_per_tick: Some(cfg.max_pushes_per_tick as u32),
        scope: Some(match cfg.scope {
            TopologyScope::Node => "node".to_string(),
            TopologyScope::Zone => "zone".to_string(),
            TopologyScope::Region => "region".to_string(),
            TopologyScope::Mesh => "mesh".to_string(),
        }),
        tick_interval_ms: Some(cfg.tick_interval_ms as u32),
    }
}

#[cfg(feature = "dataforts")]
fn overflow_config_to_inner(cfg: OverflowConfigJs) -> Result<InnerOverflowConfig> {
    let mut inner = InnerOverflowConfig {
        enabled: cfg.enabled,
        ..InnerOverflowConfig::default()
    };
    if let Some(v) = cfg.high_water_ratio {
        inner.high_water_ratio = v;
    }
    if let Some(v) = cfg.low_water_ratio {
        inner.low_water_ratio = v;
    }
    if let Some(v) = cfg.max_pushes_per_tick {
        inner.max_pushes_per_tick = v as usize;
    }
    if let Some(s) = cfg.scope {
        inner.scope = match s.to_ascii_lowercase().as_str() {
            "node" => TopologyScope::Node,
            "zone" => TopologyScope::Zone,
            "region" => TopologyScope::Region,
            "mesh" => TopologyScope::Mesh,
            other => {
                return Err(blob_err(
                    "overflow.scope",
                    format!(
                        "expected 'node'|'zone'|'region'|'mesh'; got {:?}",
                        other.to_owned()
                    ),
                ));
            }
        };
    }
    if let Some(v) = cfg.tick_interval_ms {
        inner.tick_interval_ms = v as u64;
    }
    Ok(inner)
}

/// Options for the `MeshBlobAdapter` constructor. Both fields
/// optional — defaults match the Rust + Python bindings (no
/// persistence, no overflow).
#[napi(object)]
pub struct MeshBlobAdapterOptions {
    /// Opt every per-chunk file into disk persistence. Default
    /// `false` (in-memory). Requires the underlying `Redex` to
    /// have been constructed with `{ persistentDir: ... }`.
    pub persistent: Option<bool>,
    /// Active-overflow initial config. Pass `{ enabled: true }`
    /// to opt in at defaults; pass a full
    /// [`OverflowConfigJs`] to tune. Omit entirely for the v0.2
    /// pull-only posture (the default).
    pub overflow: Option<OverflowConfigJs>,
    /// v0.3 tree-walker LRU cache byte cap. `None` disables the
    /// cache; `Some(n)` wires a byte-bounded LRU at cap = n.
    /// Default `None`. Operators size this in MiB at construction
    /// time; the per-fetch promotion/eviction stays O(1)
    /// regardless of cap.
    pub tree_node_cache_bytes: Option<u32>,
}

/// v0.3 RepairReport surfaced to JS as a plain object. Counter
/// fields mirror the Rust struct one-for-one.
#[cfg(feature = "dataforts")]
#[napi(object)]
pub struct RepairReportJs {
    pub stripes_walked: BigInt,
    pub stripes_already_healthy: BigInt,
    pub stripes_repaired: BigInt,
    pub chunks_restored: BigInt,
    pub stripes_unrecoverable: BigInt,
    pub replicated_stripes_skipped: BigInt,
    pub replicated_leaves_skipped: BigInt,
}

/// v0.3 tree-walker cache stats surfaced to JS. `null` means the
/// cache wasn't wired at construction.
#[cfg(feature = "dataforts")]
#[napi(object)]
pub struct TreeNodeCacheStatsJs {
    pub hits: BigInt,
    pub misses: BigInt,
    pub bytes: BigInt,
    pub entries: BigInt,
}

/// Substrate-owned blob storage adapter. Stores chunks as
/// content-addressed `RedexFile`s and rides the existing
/// per-chunk replication runtime for cross-node placement.
///
/// Mirrors the Python `MeshBlobAdapter` class. The async
/// methods (`store` / `fetch` / `fetchRange` / `exists`) run
/// the substrate's tokio runtime under napi's `tokio_rt`
/// feature; the JS event loop isn't blocked.
#[cfg(feature = "dataforts")]
#[napi]
pub struct MeshBlobAdapter {
    inner: Arc<InnerMeshBlobAdapter>,
    id: String,
}

#[cfg(feature = "dataforts")]
#[napi]
impl MeshBlobAdapter {
    /// Construct the adapter. `redex` must outlive the adapter
    /// (the napi class holds an `Arc` clone internally so this
    /// is automatic). `adapterId` surfaces in the Prometheus
    /// body's `adapter=...` label.
    #[napi(constructor)]
    pub fn new(
        redex: &crate::cortex::Redex,
        adapter_id: String,
        options: Option<MeshBlobAdapterOptions>,
    ) -> Result<Self> {
        let opts = options.unwrap_or(MeshBlobAdapterOptions {
            persistent: None,
            overflow: None,
            tree_node_cache_bytes: None,
        });
        let persistent = opts.persistent.unwrap_or(false);
        let mut builder = InnerMeshBlobAdapter::new(adapter_id.clone(), redex.inner_arc())
            .with_persistent(persistent);
        if let Some(overflow_cfg) = opts.overflow {
            let cfg = overflow_config_to_inner(overflow_cfg)?;
            builder = builder.with_overflow(cfg);
        }
        if let Some(cap) = opts.tree_node_cache_bytes {
            builder = builder.with_tree_node_cache(cap as usize);
        }
        Ok(Self {
            inner: Arc::new(builder),
            id: adapter_id,
        })
    }

    /// Adapter identity tag — surfaces in the Prometheus body.
    #[napi(getter)]
    pub fn adapter_id(&self) -> String {
        self.id.clone()
    }

    /// Store `data` under the content-address declared by
    /// `blobRef`. Verifies `blake3(data) == blobRef.hash`
    /// before persisting; mismatches throw `"blob: ..."`.
    /// Idempotent on identical bytes.
    #[napi]
    pub async fn store(&self, blob_ref: &BlobRef, data: Buffer) -> Result<()> {
        let adapter = self.inner.clone();
        let blob = blob_ref.inner.clone();
        let data_owned: Vec<u8> = data.to_vec();
        adapter
            .store(&blob, &data_owned)
            .await
            .map_err(map_blob_err)
    }

    /// Fetch the content-addressed bytes for `blobRef`.
    /// Verifies BLAKE3 against the supplied hash; throws on
    /// mismatch or missing content.
    #[napi]
    pub async fn fetch(&self, blob_ref: &BlobRef) -> Result<Buffer> {
        let adapter = self.inner.clone();
        let blob = blob_ref.inner.clone();
        let bytes = adapter.fetch(&blob).await.map_err(map_blob_err)?;
        Ok(Buffer::from(bytes))
    }

    /// Fetch a half-open `[start, end)` byte range. The
    /// substrate does NOT verify partial fetches against the
    /// full-content hash — callers accept the trade-off.
    #[napi]
    pub async fn fetch_range(
        &self,
        blob_ref: &BlobRef,
        start: BigInt,
        end: BigInt,
    ) -> Result<Buffer> {
        let adapter = self.inner.clone();
        let blob = blob_ref.inner.clone();
        let (_, start_u, _) = start.get_u64();
        let (_, end_u, _) = end.get_u64();
        let bytes = adapter
            .fetch_range(&blob, start_u..end_u)
            .await
            .map_err(map_blob_err)?;
        Ok(Buffer::from(bytes))
    }

    /// Probe local presence. Returns `true` when every chunk of
    /// `blobRef` is locally reachable.
    #[napi]
    pub async fn exists(&self, blob_ref: &BlobRef) -> Result<bool> {
        let adapter = self.inner.clone();
        let blob = blob_ref.inner.clone();
        adapter.exists(&blob).await.map_err(map_blob_err)
    }

    /// Render the adapter's Prometheus text body. Includes the
    /// v0.2 counter family + the v0.3 overflow counter family.
    #[napi]
    pub fn prometheus_text(&self) -> String {
        self.inner.prometheus_text()
    }

    /// v0.3: store `data` as a hierarchical-manifest blob
    /// (`BlobRef::Tree`). Wraps the substrate's
    /// `store_stream_tree` for the in-memory case; true streaming
    /// from JS callers requires the substrate's streaming-store-
    /// token API (post-v0.3 work).
    ///
    /// `encoding` is optional. Omit for `Replicated`; pass an
    /// `Encoding.reedSolomon(k, m)` value for RS-encoded storage.
    /// Returns a fresh `BlobRef` carrying the Tree's (uri,
    /// rootHash, totalSize, depth).
    #[napi]
    pub async fn store_stream_tree_from_bytes(
        &self,
        data: Buffer,
        encoding: Option<&Encoding>,
    ) -> Result<BlobRef> {
        use bytes::Bytes;
        use futures::stream;
        use net::adapter::net::dataforts::blob::blob_tree::ChunkingStrategy;
        use net::adapter::net::dataforts::Encoding as InnerEncoding;

        let enc = match encoding {
            Some(e) if e.kind == "reedSolomon" => InnerEncoding::ReedSolomon {
                k: e.k.unwrap_or(
                    net::adapter::net::dataforts::blob::erasure::DEFAULT_RS_K,
                ),
                m: e.m.unwrap_or(
                    net::adapter::net::dataforts::blob::erasure::DEFAULT_RS_M,
                ),
            },
            _ => InnerEncoding::Replicated,
        };
        let adapter = self.inner.clone();
        let owned: Vec<u8> = data.to_vec();
        let s = stream::once(async move {
            Ok::<_, InnerBlobError>(Bytes::from(owned))
        });
        let blob = adapter
            .store_stream_tree(Box::pin(s), enc, ChunkingStrategy::default())
            .await
            .map_err(map_blob_err)?;
        Ok(BlobRef::from_inner(blob))
    }

    /// v0.3: repair a Tree-encoded RS blob in-place. Walks every
    /// stripe, reconstructs missing data chunks from parity, and
    /// re-stores them under their original content-addressed
    /// hashes. Returns the report's counter fields.
    ///
    /// This is the unauthenticated system-internal entry point;
    /// callers behind a network surface should route through
    /// `repair_blob_authorized` (not yet exposed in bindings).
    #[napi]
    pub async fn repair_blob(&self, blob_ref: &BlobRef) -> Result<RepairReportJs> {
        let adapter = self.inner.clone();
        let blob = blob_ref.inner.clone();
        let report = adapter
            .repair_blob(&blob)
            .await
            .map_err(map_blob_err)?;
        Ok(RepairReportJs {
            stripes_walked: BigInt::from(report.stripes_walked),
            stripes_already_healthy: BigInt::from(report.stripes_already_healthy),
            stripes_repaired: BigInt::from(report.stripes_repaired),
            chunks_restored: BigInt::from(report.chunks_restored),
            stripes_unrecoverable: BigInt::from(report.stripes_unrecoverable),
            replicated_stripes_skipped: BigInt::from(report.replicated_stripes_skipped),
            replicated_leaves_skipped: BigInt::from(report.replicated_leaves_skipped),
        })
    }

    /// v0.3 tree-walker LRU cache statistics. Returns `null`
    /// when the cache wasn't wired at construction; otherwise
    /// `{ hits, misses, bytes, entries }`.
    #[napi]
    pub fn tree_node_cache_stats(&self) -> Option<TreeNodeCacheStatsJs> {
        self.inner
            .tree_node_cache_stats()
            .map(|(hits, misses, bytes, entries)| TreeNodeCacheStatsJs {
                hits: BigInt::from(hits),
                misses: BigInt::from(misses),
                bytes: BigInt::from(bytes as u64),
                entries: BigInt::from(entries as u64),
            })
    }

    // ---- v0.3 active-overflow surface ----

    /// True iff the adapter is currently advertising
    /// `dataforts.blob.overflow` and accepting inbound
    /// `OverflowPush` requests.
    #[napi(getter)]
    pub fn overflow_enabled(&self) -> bool {
        self.inner.overflow_enabled()
    }

    /// True iff the most recent overflow tick observed local
    /// disk at or above the high-water threshold. Tracks
    /// `dataforts_blob_overflow_active`.
    #[napi(getter)]
    pub fn overflow_active(&self) -> bool {
        self.inner.overflow_active()
    }

    /// Snapshot the current overflow configuration. Returns a
    /// plain JS object with the typed knobs.
    #[napi(getter)]
    pub fn overflow_config(&self) -> OverflowConfigJs {
        overflow_config_from_inner(self.inner.overflow_config())
    }

    /// Flip the overflow master switch at runtime. The
    /// adapter's next capability rebroadcast adds (or removes)
    /// the `dataforts.blob.overflow` tag.
    #[napi]
    pub fn set_overflow_enabled(&self, enabled: bool) {
        self.inner.set_overflow_enabled(enabled);
    }

    /// Replace the entire overflow configuration. Useful for
    /// atomic enable + tune in one call.
    #[napi]
    pub fn set_overflow_config(&self, config: OverflowConfigJs) -> Result<()> {
        let cfg = overflow_config_to_inner(config)?;
        self.inner.set_overflow_config(cfg);
        Ok(())
    }
}
