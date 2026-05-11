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
        Self {
            id,
            store,
            fetch,
            fetch_range,
            exists,
            timeout: DEFAULT_JS_ADAPTER_TIMEOUT,
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
    (
        blob_ref.uri.clone(),
        Buffer::from(blob_ref.hash.to_vec()),
        BigInt::from(blob_ref.size),
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
/// timeout defaults to 30 s; longer-running adapters should be
/// implemented in Rust.
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
) -> Result<()> {
    let store: StoreTsfn = store_fn.build_threadsafe_function().build()?;
    let fetch: FetchTsfn = fetch_fn.build_threadsafe_function().build()?;
    let fetch_range: FetchRangeTsfn = fetch_range_fn.build_threadsafe_function().build()?;
    let exists: ExistsTsfn = exists_fn.build_threadsafe_function().build()?;
    let wrapper = NodeBlobAdapter::new(id.clone(), store, fetch, fetch_range, exists);
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
        Self {
            id,
            store,
            fetch,
            fetch_range,
            exists,
            timeout: DEFAULT_JS_ADAPTER_TIMEOUT,
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
/// event loop drives resolution. Per-call timeout defaults to 30 s
/// (the same as the sync bridge) and applies to BOTH stages — JS
/// returning the Promise, and the Promise resolving.
///
/// Rejected Promises collapse to `BlobError::Backend(reason)`.
#[napi]
pub fn register_async_blob_adapter(
    id: String,
    store_fn: Function<'static, JsBlobStoreArgs, Promise<()>>,
    fetch_fn: Function<'static, JsBlobFetchArgs, Promise<Buffer>>,
    fetch_range_fn: Function<'static, JsBlobFetchRangeArgs, Promise<Buffer>>,
    exists_fn: Function<'static, JsBlobFetchArgs, Promise<bool>>,
) -> Result<()> {
    let store: StoreAsyncTsfn = store_fn.build_threadsafe_function().build()?;
    let fetch: FetchAsyncTsfn = fetch_fn.build_threadsafe_function().build()?;
    let fetch_range: FetchRangeAsyncTsfn = fetch_range_fn.build_threadsafe_function().build()?;
    let exists: ExistsAsyncTsfn = exists_fn.build_threadsafe_function().build()?;
    let wrapper = NodeAsyncBlobAdapter::new(id.clone(), store, fetch, fetch_range, exists);
    let arc: Arc<dyn BlobAdapter> = Arc::new(wrapper);
    global_blob_adapter_registry()
        .register(arc)
        .map_err(|e| blob_err("register_async_blob_adapter", e))
}
