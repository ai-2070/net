//! `FoldQueryClient` — client-side helper that wraps
//! [`MeshNode::call`] with typed `fold.query` serialization and
//! a per-`(target, service, kind)` TTL cache.
//!
//! Phase C slice 2 of `SCALING_SUBNET_SPEC.md`. The caching layer
//! is the operator-facing contract the plan calls out:
//!
//! > **Caching:** the RPC client caches recent query results with
//! > a short TTL (configurable, default 5s). Repeated queries for
//! > the same data don't re-hit the aggregator.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use parking_lot::RwLock;

use super::query_service::{
    FoldQueryError, FoldQueryOp, FoldQueryRequest, FoldQueryResponse, FOLD_QUERY_SERVICE,
};
use super::summarizer::SummaryAnnouncement;
use crate::adapter::net::mesh_rpc::{CallOptions, RpcError};
use crate::adapter::net::MeshNode;

/// Default cache TTL — the plan's locked value.
pub const DEFAULT_QUERY_CACHE_TTL: Duration = Duration::from_secs(5);

/// Default RPC call deadline. Wraps `MeshNode::call`'s
/// `CallOptions::deadline` with a sensible operator-tooling
/// default; long enough to absorb cross-subnet latency, short
/// enough that a wedged aggregator surfaces quickly.
pub const DEFAULT_QUERY_DEADLINE: Duration = Duration::from_secs(3);

/// Client-side errors the typed surface produces. Distinct from
/// `RpcError` (transport) and `FoldQueryError` (handler-level)
/// so the caller can match on the failure shape they care about.
#[derive(Debug, thiserror::Error)]
pub enum FoldQueryClientError {
    /// Transport-level failure — no route, timeout, server
    /// returned a non-Ok status before invoking the handler.
    #[error("transport: {0}")]
    Transport(RpcError),
    /// Request serialization or response deserialization failed.
    /// Postcard errors carry a `Debug`-rendered message so the
    /// error stays cross-version stable.
    #[error("codec: {0}")]
    Codec(String),
    /// Aggregator handler rejected the request (e.g. unknown
    /// fold kind). Forwarded from
    /// [`super::FoldQueryResponse::Error`].
    #[error("server: {0:?}")]
    Server(FoldQueryError),
}

impl From<RpcError> for FoldQueryClientError {
    fn from(e: RpcError) -> Self {
        Self::Transport(e)
    }
}

impl From<postcard::Error> for FoldQueryClientError {
    fn from(e: postcard::Error) -> Self {
        Self::Codec(format!("{e:?}"))
    }
}

#[derive(Clone, Eq, PartialEq, Hash)]
struct CacheKey {
    target: u64,
    service: String,
    kind: u16,
}

struct CacheEntry {
    summaries: Vec<SummaryAnnouncement>,
    fetched_at: Instant,
}

/// Typed `fold.query` client. Cheap to clone (just clones the
/// `Arc`s); operator tooling typically constructs one per
/// process and shares it.
#[derive(Clone)]
pub struct FoldQueryClient {
    mesh: Arc<MeshNode>,
    cache: Arc<RwLock<HashMap<CacheKey, CacheEntry>>>,
    ttl: Duration,
    deadline: Duration,
}

impl FoldQueryClient {
    /// Build a client backed by `mesh` with the default TTL +
    /// deadline. Callers wanting non-defaults use
    /// [`Self::with_ttl`] / [`Self::with_deadline`].
    pub fn new(mesh: Arc<MeshNode>) -> Self {
        Self {
            mesh,
            cache: Arc::new(RwLock::new(HashMap::new())),
            ttl: DEFAULT_QUERY_CACHE_TTL,
            deadline: DEFAULT_QUERY_DEADLINE,
        }
    }

    /// Override the cache TTL. `Duration::ZERO` disables the
    /// cache entirely (every call hits the wire).
    pub fn with_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl;
        self
    }

    /// Override the per-call deadline.
    pub fn with_deadline(mut self, deadline: Duration) -> Self {
        self.deadline = deadline;
        self
    }

    /// Query the aggregator for its latest cached summaries.
    /// Cache hit → return immediately; miss → issue RPC, cache
    /// the result, return.
    ///
    /// `target_node_id` is the aggregator replica to query;
    /// operator tooling typically finds it via the capability
    /// index (`role:aggregator` tag) or the existing
    /// `MeshNode::find_*` helpers.
    pub async fn query_latest(
        &self,
        target_node_id: u64,
        kind: u16,
    ) -> Result<Vec<SummaryAnnouncement>, FoldQueryClientError> {
        self.query_with_service(target_node_id, FOLD_QUERY_SERVICE, kind)
            .await
    }

    /// Same as [`Self::query_latest`] but with a caller-supplied
    /// service name. Useful when a node runs multiple
    /// aggregators registered under distinct service names.
    pub async fn query_with_service(
        &self,
        target_node_id: u64,
        service: &str,
        kind: u16,
    ) -> Result<Vec<SummaryAnnouncement>, FoldQueryClientError> {
        let key = CacheKey {
            target: target_node_id,
            service: service.to_string(),
            kind,
        };
        if !self.ttl.is_zero() {
            if let Some(entry) = self.cache.read().get(&key) {
                if entry.fetched_at.elapsed() < self.ttl {
                    return Ok(entry.summaries.clone());
                }
            }
        }
        let summaries = self
            .issue_call(target_node_id, service, kind, FoldQueryOp::LatestSummary)
            .await?;
        if !self.ttl.is_zero() {
            self.cache.write().insert(
                key,
                CacheEntry {
                    summaries: summaries.clone(),
                    fetched_at: Instant::now(),
                },
            );
        }
        Ok(summaries)
    }

    /// Issue a `SummarizeNow` query — never cached; always
    /// hits the wire. Use when the staleness tolerance is
    /// tighter than `summary_interval`.
    pub async fn query_summarize_now(
        &self,
        target_node_id: u64,
        kind: u16,
    ) -> Result<Vec<SummaryAnnouncement>, FoldQueryClientError> {
        self.issue_call(
            target_node_id,
            FOLD_QUERY_SERVICE,
            kind,
            FoldQueryOp::SummarizeNow,
        )
        .await
    }

    /// Drop every cached entry. Operator tooling calls this after
    /// a topology change (e.g. a placement migration) so the next
    /// query re-resolves against the new aggregator replica.
    pub fn invalidate_cache(&self) {
        self.cache.write().clear();
    }

    /// Drop just the entries matching `target_node_id`. Used when
    /// a single replica is known stale but the rest of the cache
    /// is still warm.
    pub fn invalidate_target(&self, target_node_id: u64) {
        let mut cache = self.cache.write();
        cache.retain(|k, _| k.target != target_node_id);
    }

    async fn issue_call(
        &self,
        target_node_id: u64,
        service: &str,
        kind: u16,
        op: FoldQueryOp,
    ) -> Result<Vec<SummaryAnnouncement>, FoldQueryClientError> {
        let request = FoldQueryRequest { kind, op };
        let body = postcard::to_allocvec(&request)?;
        let opts = CallOptions {
            deadline: Some(Instant::now() + self.deadline),
            ..Default::default()
        };
        let reply = self
            .mesh
            .call(target_node_id, service, Bytes::from(body), opts)
            .await?;
        let response: FoldQueryResponse = postcard::from_bytes(&reply.body)?;
        match response {
            FoldQueryResponse::Summaries { summaries, .. } => Ok(summaries),
            FoldQueryResponse::Error(e) => Err(FoldQueryClientError::Server(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::behavior::aggregator::{
        AggregatorConfig, AggregatorDaemon,
    };
    use crate::adapter::net::behavior::fold::capability::{
        CapabilityFold, CapabilityMembership,
    };
    use crate::adapter::net::behavior::fold::wire::SignedAnnouncement;
    use crate::adapter::net::behavior::fold::{EnvelopeMeta, FoldKind, NodeState};
    use crate::adapter::net::identity::EntityKeypair;
    use crate::adapter::net::{MeshNodeConfig, SubnetId};
    use std::collections::BTreeMap;
    use std::net::SocketAddr;

    async fn build_mesh() -> Arc<MeshNode> {
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let cfg = MeshNodeConfig::new(addr, [0x17u8; 32]);
        Arc::new(
            MeshNode::new(EntityKeypair::generate(), cfg)
                .await
                .expect("MeshNode::new"),
        )
    }

    fn sign_cap(
        kp: &EntityKeypair,
        publisher: u64,
        class: u64,
        state: NodeState,
    ) -> SignedAnnouncement<CapabilityMembership> {
        SignedAnnouncement::sign(
            kp,
            CapabilityFold::KIND_ID,
            class,
            publisher,
            1,
            EnvelopeMeta::default(),
            CapabilityMembership {
                class_hash: class,
                tags: Vec::new(),
                hardware: None,
                state,
                region: None,
                price_quote: None,
                reflex_addr: None,
                allowed_nodes: Vec::new(),
                allowed_subnets: Vec::new(),
                allowed_groups: Vec::new(),
                metadata: BTreeMap::new(),
            },
        )
        .expect("sign")
    }

    #[tokio::test]
    async fn new_carries_default_ttl_and_deadline() {
        let mesh = build_mesh().await;
        let client = FoldQueryClient::new(mesh);
        assert_eq!(client.ttl, DEFAULT_QUERY_CACHE_TTL);
        assert_eq!(client.deadline, DEFAULT_QUERY_DEADLINE);
    }

    #[tokio::test]
    async fn with_ttl_zero_disables_cache() {
        let mesh = build_mesh().await;
        let client = FoldQueryClient::new(mesh).with_ttl(Duration::ZERO);
        assert_eq!(client.ttl, Duration::ZERO);
    }

    #[tokio::test]
    async fn invalidate_cache_clears_every_entry() {
        let mesh = build_mesh().await;
        let client = FoldQueryClient::new(mesh);
        // Prime the cache directly — bypass the wire (we're not
        // testing transport here).
        let key = CacheKey {
            target: 0xAAAA,
            service: FOLD_QUERY_SERVICE.to_string(),
            kind: CapabilityFold::KIND_ID,
        };
        client.cache.write().insert(
            key.clone(),
            CacheEntry {
                summaries: vec![SummaryAnnouncement {
                    source_subnet: SubnetId::GLOBAL,
                    fold_kind: CapabilityFold::KIND_ID,
                    generation: 1,
                    buckets: vec![("idle".to_string(), 1)],
                }],
                fetched_at: Instant::now(),
            },
        );
        assert_eq!(client.cache.read().len(), 1);
        client.invalidate_cache();
        assert_eq!(client.cache.read().len(), 0);
    }

    #[tokio::test]
    async fn invalidate_target_drops_only_matching_entries() {
        let mesh = build_mesh().await;
        let client = FoldQueryClient::new(mesh);
        let now = Instant::now();
        for target in [0xAAAA_u64, 0xBBBB, 0xCCCC] {
            client.cache.write().insert(
                CacheKey {
                    target,
                    service: FOLD_QUERY_SERVICE.to_string(),
                    kind: CapabilityFold::KIND_ID,
                },
                CacheEntry {
                    summaries: Vec::new(),
                    fetched_at: now,
                },
            );
        }
        assert_eq!(client.cache.read().len(), 3);
        client.invalidate_target(0xBBBB);
        let remaining: Vec<u64> = client
            .cache
            .read()
            .keys()
            .map(|k| k.target)
            .collect();
        assert!(remaining.contains(&0xAAAA));
        assert!(remaining.contains(&0xCCCC));
        assert!(!remaining.contains(&0xBBBB));
        assert_eq!(remaining.len(), 2);
    }

    #[tokio::test]
    async fn cache_hit_returns_without_hitting_wire() {
        // Pin the cache-fast-path: priming the cache and querying
        // for the same `(target, service, kind)` returns the
        // primed entry without ever calling `mesh.call`. Validates
        // the cache contract without a live nRPC harness — the
        // mesh handle would be needed to issue a real call, but
        // the cache layer short-circuits first.
        let mesh = build_mesh().await;
        let client = FoldQueryClient::new(mesh.clone()).with_ttl(Duration::from_secs(60));
        let target = 0xDEAD_u64;
        let kind = CapabilityFold::KIND_ID;
        let cached = SummaryAnnouncement {
            source_subnet: SubnetId::new(&[3]),
            fold_kind: kind,
            generation: 7,
            buckets: vec![("idle".to_string(), 4)],
        };
        client.cache.write().insert(
            CacheKey {
                target,
                service: FOLD_QUERY_SERVICE.to_string(),
                kind,
            },
            CacheEntry {
                summaries: vec![cached.clone()],
                fetched_at: Instant::now(),
            },
        );
        let result = client.query_latest(target, kind).await.expect("cache hit");
        assert_eq!(result, vec![cached]);
    }

    // The full RPC end-to-end (client → server → response →
    // cache) needs two MeshNode handshakes + a `serve_rpc`
    // registration. That's a heavier integration setup; it ships
    // in a separate test file alongside the existing
    // `tests/nrpc_*.rs` patterns when the install-and-call wiring
    // lands as a clear `AggregatorDaemon::install_query_service`
    // helper. The pure-fn `answer()` in `query_service.rs` already
    // pins the server-side dispatch logic; the client-side cache
    // is pinned above; this leaves the transport seam as the
    // only un-tested edge.

    // Compile-time check that `aggregator_with_summary` shape
    // lines up. Suppresses dead_code on the helper that's used
    // by the integration test once it lands.
    #[allow(dead_code)]
    fn _aggregator_with_summary(
        mesh: Arc<MeshNode>,
    ) -> impl std::future::Future<Output = Arc<AggregatorDaemon>> {
        async move {
            let kp = EntityKeypair::generate();
            let fold = mesh.capability_fold();
            fold.apply(sign_cap(&kp, 0xA, 1, NodeState::Idle)).unwrap();
            let cfg = AggregatorConfig::new(SubnetId::new(&[3]))
                .with_fold_kind(CapabilityFold::KIND_ID)
                .with_interval(Duration::from_secs(60));
            Arc::new(AggregatorDaemon::new(cfg, mesh).expect("new"))
        }
    }
}
