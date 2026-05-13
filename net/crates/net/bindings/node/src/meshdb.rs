//! Node bindings for MeshDB — federated query layer.
//!
//! # Slice 1 scope
//!
//! Mirrors the Python SDK's slice 1 with Node-native async:
//!
//! - [`MeshQuery`] — 1:1-with-AST factory surface. Slice 1
//!   exposes the three atomic operators (`at` / `between` /
//!   `latest`); composite variants land in follow-up slices.
//! - [`InMemoryChainReader`] — `append(originHash, seq, payload)`
//!   populator that implements the substrate's `ChainReader`
//!   trait. Phase B+ adds a Redex-backed adapter.
//! - [`MeshQueryRunner`] — owns a `LocalMeshQueryExecutor`. Its
//!   `execute(query, options?)` is `async`, returning a
//!   [`MeshQueryStream`] handle.
//! - [`MeshQueryStream`] — `async next() -> ResultRow | null`.
//!   The JS wrapper layered on top (slice 1 + TS shim) makes
//!   this `AsyncIterable<ResultRow>` for `for await` ergonomics.
//! - [`ResultRow`] — `{ originHash: BigInt, seq: BigInt,
//!   payload: Buffer }`.
//! - [`CachePolicy`] — static factory class (`permanent()` /
//!   `timeBound(seconds)`).
//! - [`ExecuteOptions`] — `{ bypassCache?: boolean, cachePolicy?:
//!   CachePolicy }`.
//! - [`MeshDbError`] — error type with stable `kind` discriminator.
//!
//! # Async story
//!
//! Locked decision: Node = `Promise<AsyncIterable<Row>>`. The
//! Rust side exposes `MeshQueryStream.next()` (async, returns
//! `Option<ResultRow>`); the slice-1 TS shim adds the
//! `Symbol.asyncIterator` so `for await (const row of stream)`
//! works. Internal impl drains the executor's row stream into
//! a `tokio::sync::Mutex<Vec<ResultRow>>` at `execute()` time
//! and pops in `next()` — true streaming (mpsc-backed) is a
//! follow-up if profiling justifies it.

use std::sync::Arc;

use napi::bindgen_prelude::*;
use napi_derive::napi;
use tokio::sync::Mutex as AsyncMutex;

use net::adapter::net::behavior::meshdb::{
    cache::{CachePolicy as InnerCachePolicy, LruResultCache},
    executor::{
        ChainReader as InnerChainReader, ExecuteOptions as InnerExecuteOptions,
        LocalMeshQueryExecutor, MeshQueryExecutor,
    },
    planner::{CostEstimate, OperatorNode, OperatorPlan},
    query::ResultRow as InnerResultRow,
    ExecutionPlan, SeqNum,
};

use crate::common::bigint_u64;

/// One row from a query result.
///
/// `originHash` is the 16-hex chain identifier; `seq` is the
/// per-chain monotonic sequence; `payload` is opaque bytes
/// (event body for plain reads, or a postcard-encoded envelope
/// for aggregate / join / window sentinel rows — slice 2 adds
/// the decoder methods).
#[napi]
#[derive(Clone)]
pub struct ResultRow {
    origin: u64,
    seq: u64,
    payload: Vec<u8>,
}

#[napi]
impl ResultRow {
    #[napi(getter, js_name = "originHash")]
    pub fn origin_hash(&self) -> BigInt {
        BigInt::from(self.origin)
    }

    #[napi(getter)]
    pub fn seq(&self) -> BigInt {
        BigInt::from(self.seq)
    }

    #[napi(getter)]
    pub fn payload(&self) -> Buffer {
        Buffer::from(self.payload.clone())
    }
}

impl From<InnerResultRow> for ResultRow {
    fn from(r: InnerResultRow) -> Self {
        Self {
            origin: r.origin,
            seq: r.seq.0,
            payload: r.payload,
        }
    }
}

/// Cache policy as a tagged plain-object shape. `kind` is one
/// of `"permanent"` (cache until LRU eviction; use only when
/// the query's result is immutable under substrate semantics)
/// or `"time_bound"` (TTL expiry, `ttlSeconds` defaults to
/// 5 s per the locked Phase F join-watermark mirror).
///
/// Construct via the [`cachePolicyPermanent`] /
/// [`cachePolicyTimeBound`] module-level factories for
/// type-safe defaults, or build the object literal directly
/// if you're sure about the shape.
#[napi(object)]
pub struct CachePolicy {
    /// `"permanent"` or `"time_bound"`. Unknown kinds map to
    /// the default `TimeBound(5s)`.
    pub kind: String,
    /// TTL in seconds (only meaningful when `kind ==
    /// "time_bound"`). Omitted / non-finite → 5 s.
    #[napi(js_name = "ttlSeconds")]
    pub ttl_seconds: Option<f64>,
}

/// Build a `"permanent"` cache policy object. Equivalent to
/// `{ kind: "permanent" }`.
#[napi(js_name = "cachePolicyPermanent")]
pub fn cache_policy_permanent() -> CachePolicy {
    CachePolicy {
        kind: "permanent".to_string(),
        ttl_seconds: None,
    }
}

/// Build a `"time_bound"` cache policy object. `seconds`
/// defaults to 5 s when omitted.
#[napi(js_name = "cachePolicyTimeBound")]
pub fn cache_policy_time_bound(seconds: Option<f64>) -> CachePolicy {
    CachePolicy {
        kind: "time_bound".to_string(),
        ttl_seconds: seconds,
    }
}

fn cache_policy_to_inner(p: Option<CachePolicy>) -> InnerCachePolicy {
    match p {
        None => InnerCachePolicy::default(),
        Some(p) => match p.kind.as_str() {
            "permanent" => InnerCachePolicy::Permanent,
            // Default + "time_bound" + any unknown kind →
            // TimeBound, with the ttl_seconds field if
            // present, else 5 s.
            _ => {
                let secs = p.ttl_seconds.unwrap_or(5.0);
                let secs = if secs.is_finite() && secs >= 0.0 {
                    secs
                } else {
                    5.0
                };
                InnerCachePolicy::TimeBound {
                    ttl: std::time::Duration::from_secs_f64(secs),
                }
            }
        },
    }
}

/// Per-execute options. `bypassCache` skips both lookup AND
/// writeback (Phase F decision); `cachePolicy` defaults to
/// `TimeBound(5s)` when omitted.
#[napi(object)]
pub struct ExecuteOptions {
    #[napi(js_name = "bypassCache")]
    pub bypass_cache: Option<bool>,
    #[napi(js_name = "cachePolicy")]
    pub cache_policy: Option<CachePolicy>,
}

fn execute_options_to_inner(opts: Option<ExecuteOptions>) -> InnerExecuteOptions {
    let Some(opts) = opts else {
        return InnerExecuteOptions::default();
    };
    InnerExecuteOptions {
        bypass_cache: opts.bypass_cache.unwrap_or(false),
        cache_policy: cache_policy_to_inner(opts.cache_policy),
    }
}

/// 1:1 AST factory surface. Construct via static methods that
/// mirror the Rust `OperatorPlan` variants. Internally carries
/// a fully-planned `OperatorNode`; slice 1 exposes only the
/// atomic operators that don't need planner-side resolution.
#[napi]
#[derive(Clone)]
pub struct MeshQuery {
    plan: ExecutionPlan,
}

#[napi]
impl MeshQuery {
    /// Read the event at `seq` from chain `originHash`.
    #[napi(factory)]
    pub fn at(origin_hash: BigInt, seq: BigInt) -> Result<Self> {
        let origin = bigint_u64(origin_hash)?;
        let seq = bigint_u64(seq)?;
        let op = OperatorPlan::AtRead {
            origin,
            seq: SeqNum(seq),
        };
        Ok(Self {
            plan: plan_of(op),
        })
    }

    /// Read events in the half-open seq range `[start, end)`.
    #[napi(factory)]
    pub fn between(origin_hash: BigInt, start: BigInt, end: BigInt) -> Result<Self> {
        let origin = bigint_u64(origin_hash)?;
        let start = bigint_u64(start)?;
        let end = bigint_u64(end)?;
        if start >= end {
            return Err(mesh_err(format!(
                "between: start ({start}) must be < end ({end})"
            )));
        }
        let op = OperatorPlan::BetweenRead {
            origin,
            start: SeqNum(start),
            end: SeqNum(end),
        };
        Ok(Self {
            plan: plan_of(op),
        })
    }

    /// Read the tip event from chain `originHash`.
    #[napi(factory)]
    pub fn latest(origin_hash: BigInt) -> Result<Self> {
        let origin = bigint_u64(origin_hash)?;
        Ok(Self {
            plan: plan_of(OperatorPlan::LatestRead { origin }),
        })
    }
}

fn plan_of(op: OperatorPlan) -> ExecutionPlan {
    ExecutionPlan {
        root: OperatorNode {
            operator: op,
            target_nodes: vec![],
            cost: CostEstimate::default(),
        },
        total_cost: CostEstimate::default(),
    }
}

/// In-process `ChainReader` Node wrapper. Slice 1 ships the
/// in-memory variant; populate via `.append(originHash, seq,
/// payload)` then hand to `MeshQueryRunner`. Phase B+ will
/// expose a `fromRedex(...)` adapter.
#[napi]
pub struct InMemoryChainReader {
    inner: Arc<InMemoryStore>,
}

#[derive(Default)]
struct InMemoryStore {
    chains: std::sync::Mutex<
        std::collections::BTreeMap<u64, std::collections::BTreeMap<SeqNum, Vec<u8>>>,
    >,
}

impl InnerChainReader for InMemoryStore {
    fn read_one(&self, origin: u64, seq: SeqNum) -> Option<Vec<u8>> {
        self.chains.lock().unwrap().get(&origin)?.get(&seq).cloned()
    }

    fn read_range(&self, origin: u64, start: SeqNum, end: SeqNum) -> Vec<(SeqNum, Vec<u8>)> {
        self.chains
            .lock()
            .unwrap()
            .get(&origin)
            .map(|chain| {
                chain
                    .range(start..end)
                    .map(|(s, p)| (*s, p.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn latest_seq(&self, origin: u64) -> Option<SeqNum> {
        self.chains
            .lock()
            .unwrap()
            .get(&origin)?
            .keys()
            .next_back()
            .copied()
    }
}

#[napi]
impl InMemoryChainReader {
    #[napi(constructor)]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(InMemoryStore::default()),
        }
    }

    /// Append a single event to the in-memory store.
    #[napi]
    pub fn append(&self, origin_hash: BigInt, seq: BigInt, payload: Buffer) -> Result<()> {
        let origin = bigint_u64(origin_hash)?;
        let seq = bigint_u64(seq)?;
        self.inner
            .chains
            .lock()
            .unwrap()
            .entry(origin)
            .or_default()
            .insert(SeqNum(seq), payload.to_vec());
        Ok(())
    }

    /// Tip of chain `originHash`, or `null` if unknown.
    #[napi(js_name = "latestSeq")]
    pub fn latest_seq(&self, origin_hash: BigInt) -> Result<Option<BigInt>> {
        let origin = bigint_u64(origin_hash)?;
        Ok(self.inner.latest_seq(origin).map(|s| BigInt::from(s.0)))
    }
}

/// Runs queries against a [`InMemoryChainReader`] via the
/// substrate's `LocalMeshQueryExecutor`. Async by design —
/// `execute()` returns a `MeshQueryStream` whose `next()` is
/// async. The TS-side wrapper layers `Symbol.asyncIterator`
/// so callers can `for await (const row of stream)`.
#[napi]
pub struct MeshQueryRunner {
    executor: Arc<LocalMeshQueryExecutor<InMemoryStore>>,
}

#[napi]
impl MeshQueryRunner {
    /// Build a runner. `enableCache` wires the Phase F LRU
    /// (default: false). Capability-version source is hard-
    /// wired to `0` while there's no `CapabilityIndex` plumbed
    /// (slice 1 is local-executor-only).
    #[napi(constructor)]
    pub fn new(reader: &InMemoryChainReader, enable_cache: Option<bool>) -> Self {
        let store = reader.inner.clone();
        let executor = if enable_cache.unwrap_or(false) {
            let cache: Arc<dyn net::adapter::net::behavior::meshdb::cache::ResultCache> =
                Arc::new(LruResultCache::default());
            let version_fn: Arc<dyn Fn() -> u64 + Send + Sync> = Arc::new(|| 0);
            LocalMeshQueryExecutor::with_cache(store, cache, version_fn)
        } else {
            LocalMeshQueryExecutor::new(store)
        };
        Self {
            executor: Arc::new(executor),
        }
    }

    /// Execute `query`. Returns a stream whose `next()` yields
    /// the next [`ResultRow`] (or `null` on EOF). The full row
    /// list is drained at execute time and buffered inside the
    /// stream; true row-by-row streaming lands when a consumer
    /// needs it.
    #[napi]
    pub async fn execute(
        &self,
        query: &MeshQuery,
        options: Option<ExecuteOptions>,
    ) -> Result<MeshQueryStream> {
        use futures::StreamExt;
        let plan = query.plan.clone();
        let opts = execute_options_to_inner(options);
        let running = self
            .executor
            .execute_with(plan, opts)
            .await
            .map_err(|e| mesh_err(format!("{e}")))?;
        let mut stream = running.rows;
        let mut out: Vec<ResultRow> = Vec::new();
        while let Some(item) = stream.next().await {
            match item {
                Ok(row) => out.push(row.into()),
                Err(e) => return Err(mesh_err(format!("{e}"))),
            }
        }
        Ok(MeshQueryStream {
            // Reverse so `.pop()` returns rows in original order.
            rows: Arc::new(AsyncMutex::new({
                out.reverse();
                out
            })),
        })
    }
}

/// Pull-based row stream. The JS-side TS shim adds
/// `Symbol.asyncIterator` over this; raw callers can use
/// `await stream.next()` in a loop themselves.
#[napi]
pub struct MeshQueryStream {
    rows: Arc<AsyncMutex<Vec<ResultRow>>>,
}

#[napi]
impl MeshQueryStream {
    /// The next row, or `null` on end-of-stream. Idempotent
    /// post-EOF — repeated calls keep returning `null`.
    #[napi]
    pub async fn next(&self) -> Result<Option<ResultRow>> {
        Ok(self.rows.lock().await.pop())
    }

    /// Drain the remaining rows into a list. Convenience for
    /// callers that don't want to write the `await next()`
    /// loop. Subsequent `.next()` calls return `null`.
    #[napi(js_name = "toArray")]
    pub async fn to_array(&self) -> Result<Vec<ResultRow>> {
        let mut g = self.rows.lock().await;
        let mut out: Vec<ResultRow> = std::mem::take(&mut *g);
        // We stored reversed; un-reverse on drain so callers
        // get original insertion order.
        out.reverse();
        Ok(out)
    }
}

fn mesh_err(msg: String) -> Error {
    Error::new(Status::GenericFailure, msg)
}
