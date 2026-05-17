// `#[napi]` exports leave items "unused" from Rust's POV.
#![allow(dead_code)]

//! NAPI surface for the Deck SDK — operator-side bindings.
//!
//! Slice 1 of `DECK_SDK_PLAN.md` Phase 5: `DeckClient` +
//! `AdminCommands` (all 9 methods) + snapshot / status streams +
//! `OperatorIdentity`. Audit / log / failure streams + ICE land
//! in slice 2/3.
//!
//! # Phase 1 substrate constraint
//!
//! The substrate's `DeckClient` is non-signing today —
//! `AdminCommands` records the operator id but doesn't yet route
//! through channel-auth. The Node surface exposes the same API so
//! consumers benefit transparently when the substrate cuts over.
//!
//! # Snapshot wire form
//!
//! `MeshOsSnapshot` is large; the binding emits it as a JSON
//! string that the TS wrapper at `sdk-ts/src/deck.ts` auto-parses
//! into an object. `StatusSummary` is small enough to emit as a
//! typed object.
//!
//! # Error envelope
//!
//! Errors throw `Error` whose `.message` carries the substrate
//! `<<deck-sdk-kind:KIND>>MSG` discriminator verbatim. The TS
//! wrapper parses the envelope into a typed `DeckSdkError`.

use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};

use napi::bindgen_prelude::*;
use napi_derive::napi;

use net::adapter::net::behavior::deck::{
    AdminCommands as CoreAdminCommands, AuditQuery as CoreAuditQuery,
    AuditStream as CoreAuditStream, ChainCommit as CoreChainCommit, DeckClient as CoreClient,
    DeckClientConfig as CoreConfig, DeckError, FailureStream as CoreFailureStream,
    IceProposal as CoreIceProposal, LogFilter as CoreLogFilter, LogStream as CoreLogStream,
    OperatorIdentity as CoreIdentity, SnapshotStream as CoreSnapshotStream, StatusSummary,
    StatusSummaryStream as CoreStatusStream,
};
use net::adapter::net::behavior::meshos::{
    LoggingDispatcher, MeshOsConfig, MeshOsDaemonSdk as CoreSdk,
};
use net::adapter::net::behavior::meshos::logs::LogLevel as CoreLogLevel;
use net::adapter::net::behavior::meshos::{
    blast_radius_hash, ice_proposal_signing_payload, AdminVerifier as CoreAdminVerifier,
    AvoidScope as CoreAvoidScope, ChainId as CoreChainId, DaemonRef as CoreDaemonRef,
    MigrationId as CoreMigrationId, OperatorRegistry as CoreOperatorRegistry,
    OperatorSignature as CoreOperatorSignature, VerifyError as CoreVerifyError,
};
use net::adapter::net::identity::EntityId;
use net::adapter::net::EntityKeypair;

use futures::StreamExt;

// =========================================================================
// Error envelope helpers
// =========================================================================

fn deck_err(kind: &str, message: impl Into<String>) -> Error {
    Error::from_reason(format!("<<deck-sdk-kind:{kind}>>{}", message.into()))
}

fn deck_err_from(e: DeckError) -> Error {
    deck_err(e.kind, e.message)
}

/// Map a substrate `VerifyError` onto the `<<deck-sdk-kind:KIND>>MSG`
/// envelope. The kind comes from the substrate's stable
/// discriminator so cross-binding consumers branch on the same
/// string ("not_authorized", "signature_invalid", etc.).
fn verify_error_to_js(e: CoreVerifyError) -> Error {
    deck_err(e.kind(), e.to_string())
}

// =========================================================================
// Wire form POJOs
// =========================================================================

/// Operator identity. Operator id is the keypair's 64-bit origin
/// hash. Construct via `generate()` (tests) or `fromSeed(buffer)`
/// (production loads).
#[napi]
pub struct OperatorIdentity {
    inner: CoreIdentity,
}

#[napi]
impl OperatorIdentity {
    /// Generate a fresh operator identity.
    #[napi(factory)]
    pub fn generate() -> Self {
        Self {
            inner: CoreIdentity::generate(),
        }
    }

    /// Load from a 32-byte ed25519 seed.
    #[napi(factory)]
    pub fn from_seed(seed: Buffer) -> Result<Self> {
        let bytes: &[u8] = seed.as_ref();
        if bytes.len() != 32 {
            return Err(deck_err(
                "invalid_argument",
                format!("seed must be 32 bytes, got {}", bytes.len()),
            ));
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(bytes);
        Ok(Self {
            inner: CoreIdentity::from_keypair(EntityKeypair::from_bytes(arr)),
        })
    }

    /// Build from a `net.Identity` keypair (the daemon-side
    /// identity class). Useful when an operator's identity comes
    /// from the same `Identity` store that daemons use.
    #[napi(factory)]
    pub fn from_identity(identity: &crate::identity::Identity) -> Self {
        Self {
            inner: CoreIdentity::from_keypair(identity.keypair_clone()),
        }
    }

    #[napi(getter)]
    pub fn operator_id(&self) -> BigInt {
        BigInt::from(self.inner.operator_id())
    }

    /// 32-byte ed25519 public key. Used by an offline tool that
    /// authors the cluster's `OperatorRegistry` from a set of
    /// known identities.
    #[napi]
    pub fn public_key(&self) -> Buffer {
        Buffer::from(self.inner.keypair().entity_id().as_bytes().as_ref())
    }

    /// Sign a simulated ICE proposal. Returns an
    /// `OperatorSignatureJs` directly consumable by
    /// `SimulatedIceProposal.commit([sig, ...])`.
    ///
    /// Wraps the substrate's `OperatorIdentity::sign_proposal` —
    /// covers `(ICE_SIGNING_DOMAIN || issued_at_ms ||
    /// blast_hash || postcard(action))` so the verifier rebuilds
    /// the same bytes locally.
    #[napi]
    pub async fn sign_proposal(
        &self,
        simulated: &SimulatedIceProposal,
    ) -> Result<OperatorSignatureJs> {
        let guard = simulated.state.lock().await;
        let state = guard.as_ref().ok_or_else(|| {
            deck_err(
                "already_committed",
                "SimulatedIceProposal was already consumed by commit()",
            )
        })?;
        let hash = blast_radius_hash(&state.blast);
        let sig = self
            .inner
            .sign_proposal(&state.action, simulated.issued_at_ms, &hash);
        Ok(OperatorSignatureJs {
            operator_id: BigInt::from(sig.operator_id),
            signature: Buffer::from(sig.signature),
        })
    }

    /// Sign raw payload bytes with this operator's ed25519 key.
    /// Returns an `OperatorSignatureJs`.
    ///
    /// Useful for offline / cross-deck signing flows where the
    /// `(action, issued_at_ms, blast_hash)` triple is exchanged
    /// out-of-band and the local deck reproduces the signing
    /// payload independently. Most consumers want
    /// `signProposal(simulated)` instead.
    #[napi]
    pub fn sign_payload(&self, payload: Buffer) -> OperatorSignatureJs {
        let sig = self.inner.keypair().sign(payload.as_ref());
        OperatorSignatureJs {
            operator_id: BigInt::from(self.inner.operator_id()),
            signature: Buffer::from(sig.to_bytes().as_ref()),
        }
    }
}

/// `ChainCommit` returned by every admin commit. Carries the
/// substrate's per-commit metadata for audit correlation.
#[napi(object)]
pub struct ChainCommitJs {
    pub commit_id: BigInt,
    pub operator_id: BigInt,
    pub event_kind: String,
    pub committed_at_ms: BigInt,
}

fn chain_commit_to_js(commit: &CoreChainCommit) -> ChainCommitJs {
    let committed_at_ms = commit
        .committed_at()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    ChainCommitJs {
        commit_id: BigInt::from(commit.commit_id()),
        operator_id: BigInt::from(commit.operator_id()),
        event_kind: commit.event_kind().to_string(),
        committed_at_ms: BigInt::from(committed_at_ms),
    }
}

#[napi(object)]
pub struct PeerCountsJs {
    pub healthy: u32,
    pub degraded: u32,
    pub unreachable: u32,
    pub unknown: u32,
}

#[napi(object)]
pub struct DaemonCountsJs {
    pub running: u32,
    pub starting: u32,
    pub stopping: u32,
    pub stopped: u32,
    pub backing_off: u32,
    pub crash_looping: u32,
}

#[napi(object)]
pub struct StatusSummaryJs {
    pub peers: PeerCountsJs,
    pub daemons: DaemonCountsJs,
    pub replica_chains: u32,
    pub avoid_list_entries: u32,
    pub recently_emitted_count: u32,
    pub recent_failure_count: u32,
    pub admin_audit_ring_depth: u32,
    pub freeze_remaining_ms: Option<BigInt>,
    pub local_maintenance_active: bool,
}

fn status_summary_to_js(s: &StatusSummary) -> StatusSummaryJs {
    StatusSummaryJs {
        peers: PeerCountsJs {
            healthy: s.peers.healthy as u32,
            degraded: s.peers.degraded as u32,
            unreachable: s.peers.unreachable as u32,
            unknown: s.peers.unknown as u32,
        },
        daemons: DaemonCountsJs {
            running: s.daemons.running as u32,
            starting: s.daemons.starting as u32,
            stopping: s.daemons.stopping as u32,
            stopped: s.daemons.stopped as u32,
            backing_off: s.daemons.backing_off as u32,
            crash_looping: s.daemons.crash_looping as u32,
        },
        replica_chains: s.replica_chains as u32,
        avoid_list_entries: s.avoid_list_entries as u32,
        recently_emitted_count: s.recently_emitted_count as u32,
        recent_failure_count: s.recent_failure_count as u32,
        admin_audit_ring_depth: s.admin_audit_ring_depth as u32,
        freeze_remaining_ms: s.freeze_remaining_ms.map(BigInt::from),
        local_maintenance_active: s.local_maintenance_active,
    }
}

// =========================================================================
// DeckClientConfig
// =========================================================================

#[napi(object)]
pub struct DeckClientConfigJs {
    pub snapshot_poll_interval_ms: Option<BigInt>,
    pub ice_signature_threshold: Option<u32>,
}

impl DeckClientConfigJs {
    fn into_core(self) -> Result<CoreConfig> {
        let mut cfg = CoreConfig::default();
        if let Some(bi) = self.snapshot_poll_interval_ms {
            let ms = crate::common::bigint_u64(bi).map_err(|e| {
                deck_err(
                    "invalid_config",
                    format!("snapshotPollIntervalMs: {}", e.reason),
                )
            })?;
            cfg.snapshot_poll_interval = Duration::from_millis(ms);
        }
        if let Some(n) = self.ice_signature_threshold {
            cfg.ice_signature_threshold = n as usize;
        }
        Ok(cfg)
    }
}

// =========================================================================
// Snapshot stream — async iterator
// =========================================================================

/// Live `MeshOsSnapshot` stream. The napi handle exposes a
/// `nextSnapshot()` method that resolves to a JSON string (or
/// `null` when the stream is closed). The TS wrapper at
/// `sdk-ts/src/deck.ts` wraps this in an `AsyncIterable` and
/// auto-parses the JSON into an object.
#[napi]
pub struct SnapshotStream {
    inner: tokio::sync::Mutex<Option<CoreSnapshotStream>>,
}

#[napi]
impl SnapshotStream {
    /// Resolve to the next snapshot as a JSON string, or `null`
    /// when the underlying stream ends.
    #[napi]
    pub async fn next_snapshot(&self) -> Result<Option<String>> {
        let mut guard = self.inner.lock().await;
        let stream = match guard.as_mut() {
            Some(s) => s,
            None => return Ok(None),
        };
        match stream.next().await {
            Some(Ok(snap)) => serde_json::to_string(&snap)
                .map(Some)
                .map_err(|e| deck_err("snapshot_serialize_failed", e.to_string())),
            Some(Err(e)) => Err(deck_err_from(e)),
            None => Ok(None),
        }
    }

    /// Close the stream. Subsequent `nextSnapshot` calls resolve
    /// to `null`.
    #[napi]
    pub async fn close(&self) {
        *self.inner.lock().await = None;
    }
}

#[napi]
pub struct StatusSummaryStream {
    inner: tokio::sync::Mutex<Option<CoreStatusStream>>,
}

#[napi]
impl StatusSummaryStream {
    #[napi]
    pub async fn next_summary(&self) -> Result<Option<StatusSummaryJs>> {
        let mut guard = self.inner.lock().await;
        let stream = match guard.as_mut() {
            Some(s) => s,
            None => return Ok(None),
        };
        match stream.next().await {
            Some(Ok(s)) => Ok(Some(status_summary_to_js(&s))),
            Some(Err(e)) => Err(deck_err_from(e)),
            None => Ok(None),
        }
    }

    #[napi]
    pub async fn close(&self) {
        *self.inner.lock().await = None;
    }
}

// =========================================================================
// AdminCommands
// =========================================================================

/// Typed admin-event surface — one method per `AdminEvent`
/// variant. Each commits via the substrate's admin chain + returns
/// a `ChainCommit` for audit correlation.
#[napi]
pub struct AdminCommands {
    client: Arc<CoreClient>,
}

impl AdminCommands {
    fn admin(&self) -> CoreAdminCommands<'_> {
        self.client.admin()
    }
}

#[napi]
impl AdminCommands {
    #[napi]
    pub async fn drain(&self, node: BigInt, drain_for_ms: BigInt) -> Result<ChainCommitJs> {
        let node = crate::common::bigint_u64(node)
            .map_err(|e| deck_err("invalid_argument", format!("node: {}", e.reason)))?;
        let drain_for_ms = crate::common::bigint_u64(drain_for_ms)
            .map_err(|e| deck_err("invalid_argument", format!("drainForMs: {}", e.reason)))?;
        self.admin()
            .drain(node, Duration::from_millis(drain_for_ms))
            .await
            .map(|c| chain_commit_to_js(&c))
            .map_err(deck_err_from)
    }

    #[napi]
    pub async fn enter_maintenance(
        &self,
        node: BigInt,
        drain_for_ms: Option<BigInt>,
    ) -> Result<ChainCommitJs> {
        let node = crate::common::bigint_u64(node)
            .map_err(|e| deck_err("invalid_argument", format!("node: {}", e.reason)))?;
        let drain_for = match drain_for_ms {
            Some(bi) => {
                let ms = crate::common::bigint_u64(bi).map_err(|e| {
                    deck_err("invalid_argument", format!("drainForMs: {}", e.reason))
                })?;
                Some(Duration::from_millis(ms))
            }
            None => None,
        };
        self.admin()
            .enter_maintenance(node, drain_for)
            .await
            .map(|c| chain_commit_to_js(&c))
            .map_err(deck_err_from)
    }

    #[napi]
    pub async fn exit_maintenance(&self, node: BigInt) -> Result<ChainCommitJs> {
        let node = crate::common::bigint_u64(node)
            .map_err(|e| deck_err("invalid_argument", format!("node: {}", e.reason)))?;
        self.admin()
            .exit_maintenance(node)
            .await
            .map(|c| chain_commit_to_js(&c))
            .map_err(deck_err_from)
    }

    #[napi]
    pub async fn cordon(&self, node: BigInt) -> Result<ChainCommitJs> {
        let node = crate::common::bigint_u64(node)
            .map_err(|e| deck_err("invalid_argument", format!("node: {}", e.reason)))?;
        self.admin()
            .cordon(node)
            .await
            .map(|c| chain_commit_to_js(&c))
            .map_err(deck_err_from)
    }

    #[napi]
    pub async fn uncordon(&self, node: BigInt) -> Result<ChainCommitJs> {
        let node = crate::common::bigint_u64(node)
            .map_err(|e| deck_err("invalid_argument", format!("node: {}", e.reason)))?;
        self.admin()
            .uncordon(node)
            .await
            .map(|c| chain_commit_to_js(&c))
            .map_err(deck_err_from)
    }

    #[napi]
    pub async fn drop_replicas(&self, node: BigInt, chains: Vec<BigInt>) -> Result<ChainCommitJs> {
        let node = crate::common::bigint_u64(node)
            .map_err(|e| deck_err("invalid_argument", format!("node: {}", e.reason)))?;
        let mut converted = Vec::with_capacity(chains.len());
        for (i, bi) in chains.into_iter().enumerate() {
            let c = crate::common::bigint_u64(bi)
                .map_err(|e| deck_err("invalid_argument", format!("chains[{i}]: {}", e.reason)))?;
            converted.push(c);
        }
        self.admin()
            .drop_replicas(node, converted)
            .await
            .map(|c| chain_commit_to_js(&c))
            .map_err(deck_err_from)
    }

    #[napi]
    pub async fn invalidate_placement(&self, node: BigInt) -> Result<ChainCommitJs> {
        let node = crate::common::bigint_u64(node)
            .map_err(|e| deck_err("invalid_argument", format!("node: {}", e.reason)))?;
        self.admin()
            .invalidate_placement(node)
            .await
            .map(|c| chain_commit_to_js(&c))
            .map_err(deck_err_from)
    }

    #[napi]
    pub async fn restart_all_daemons(&self, node: BigInt) -> Result<ChainCommitJs> {
        let node = crate::common::bigint_u64(node)
            .map_err(|e| deck_err("invalid_argument", format!("node: {}", e.reason)))?;
        self.admin()
            .restart_all_daemons(node)
            .await
            .map(|c| chain_commit_to_js(&c))
            .map_err(deck_err_from)
    }

    #[napi]
    pub async fn clear_avoid_list(&self, node: BigInt) -> Result<ChainCommitJs> {
        let node = crate::common::bigint_u64(node)
            .map_err(|e| deck_err("invalid_argument", format!("node: {}", e.reason)))?;
        self.admin()
            .clear_avoid_list(node)
            .await
            .map(|c| chain_commit_to_js(&c))
            .map_err(deck_err_from)
    }
}

// =========================================================================
// DeckClient
// =========================================================================

/// Operator-facing handle to the cluster's admin / snapshot / log /
/// audit surfaces. Construct via `DeckClient.new(...)` for the
/// standalone "operator-only" mode (binding owns the supervisor),
/// or via `fromMeshos(sdk, identity)` against an externally-managed
/// `MeshOsDaemonSdk`.
#[napi]
pub struct DeckClient {
    client: Arc<CoreClient>,
    /// `Some` only when the client owns its private supervisor
    /// runtime (constructed via `DeckClient.new`); `None` when
    /// built via `fromMeshos` against an externally-managed SDK.
    /// Kept alive for the client's lifetime so the supervisor's
    /// tokio tasks + sockets stay up; the napi GC's drop runs
    /// the SDK's own teardown.
    _owned_sdk: Option<tokio::sync::Mutex<Option<CoreSdk>>>,
}

#[napi]
impl DeckClient {
    /// Construct a deck client owning a private supervisor runtime.
    /// Mirrors the cdylib's `net_deck_client_new` ("operator-only
    /// mode" per `net_deck.h`) for Node consumers who don't already
    /// have a `MeshOsDaemonSdk` to compose against.
    ///
    /// `operatorSeed` must be exactly 32 bytes of ed25519 seed
    /// material — the operator id is derived as the keypair's
    /// origin hash. `meshosConfig` / `deckConfig` accept the same
    /// shapes as the standalone factories; pass `null` for
    /// substrate defaults.
    #[napi(factory)]
    pub async fn new(
        operator_seed: Buffer,
        meshos_config: Option<crate::meshos::MeshOsConfigJs>,
        deck_config: Option<DeckClientConfigJs>,
    ) -> Result<DeckClient> {
        if operator_seed.len() != 32 {
            return Err(deck_err(
                "invalid_argument",
                format!(
                    "operatorSeed must be exactly 32 bytes; got {}",
                    operator_seed.len()
                ),
            ));
        }
        let mut seed = [0u8; 32];
        seed.copy_from_slice(&operator_seed);
        let keypair = EntityKeypair::from_bytes(seed);
        let identity = CoreIdentity::from_keypair(keypair);

        let sdk_cfg = match meshos_config {
            Some(c) => c.into_core()?,
            None => MeshOsConfig::default(),
        };
        let deck_cfg = match deck_config {
            Some(c) => c.into_core()?,
            None => CoreConfig::default(),
        };

        let dispatcher = Arc::new(LoggingDispatcher::new());
        let sdk = CoreSdk::start(sdk_cfg, dispatcher);
        let core_client = CoreClient::new(
            sdk.runtime().handle_clone(),
            sdk.runtime().snapshot_reader().clone(),
            identity,
            deck_cfg,
        );

        Ok(DeckClient {
            client: Arc::new(core_client),
            _owned_sdk: Some(tokio::sync::Mutex::new(Some(sdk))),
        })
    }

    /// Construct against a running `MeshOsDaemonSdk`. The deck
    /// client reuses the SDK's tokio runtime, so streams + admin
    /// commits run on the same supervisor scheduler.
    #[napi(factory)]
    pub async fn from_meshos(
        sdk: &crate::meshos::MeshOsDaemonSdk,
        identity: &OperatorIdentity,
        config: Option<DeckClientConfigJs>,
    ) -> Result<DeckClient> {
        let cfg = match config {
            Some(c) => c.into_core()?,
            None => CoreConfig::default(),
        };
        let core_client = sdk
            .with_core(|core| {
                CoreClient::new(
                    core.runtime().handle_clone(),
                    core.runtime().snapshot_reader().clone(),
                    identity.inner.clone(),
                    cfg,
                )
            })
            .await
            .ok_or_else(|| {
                deck_err(
                    "already_shutdown",
                    "MeshOsDaemonSdk was already consumed by shutdown",
                )
            })?;
        Ok(DeckClient {
            client: Arc::new(core_client),
            _owned_sdk: None,
        })
    }

    /// Operator identity bound to this client.
    #[napi]
    pub fn identity(&self) -> OperatorIdentity {
        OperatorIdentity {
            inner: self.client.identity().clone(),
        }
    }

    /// Typed admin-event surface.
    #[napi(getter)]
    pub fn admin(&self) -> AdminCommands {
        AdminCommands {
            client: self.client.clone(),
        }
    }

    /// Break-glass surface. Returns `IceCommands` whose 7
    /// factories produce `IceProposal`s. Each must be
    /// `simulate()`-d (yielding a `SimulatedIceProposal`) before
    /// `commit(signatures)`. The typestate is enforced at the
    /// class level: `IceProposal` has no `commit` method.
    #[napi(getter)]
    pub fn ice(&self) -> IceCommands {
        IceCommands {
            client: self.client.clone(),
        }
    }

    /// One-shot read of the latest `MeshOsSnapshot` as a JSON
    /// string. The TS wrapper parses the JSON into an object.
    #[napi]
    pub fn status(&self) -> Result<String> {
        serde_json::to_string(&self.client.status())
            .map_err(|e| deck_err("snapshot_serialize_failed", e.to_string()))
    }

    /// One-shot read of the rolled-up `StatusSummary`.
    #[napi]
    pub fn status_summary(&self) -> StatusSummaryJs {
        status_summary_to_js(&self.client.status_summary())
    }

    /// Live `MeshOsSnapshot` stream. `nextSnapshot()` on the
    /// returned handle resolves to the next JSON-encoded snapshot.
    ///
    /// Async because the substrate constructs a
    /// `tokio::time::Interval` inside the stream which requires
    /// a runtime context. Running this via `napi async` puts us on
    /// the napi tokio runtime so the interval reactor binds
    /// correctly.
    #[napi]
    pub async fn snapshots(&self) -> SnapshotStream {
        SnapshotStream {
            inner: tokio::sync::Mutex::new(Some(self.client.snapshots())),
        }
    }

    /// Live `StatusSummary` stream. Same runtime-context
    /// requirement as `snapshots`.
    #[napi]
    pub async fn status_summary_stream(&self) -> StatusSummaryStream {
        StatusSummaryStream {
            inner: tokio::sync::Mutex::new(Some(self.client.status_summary_stream())),
        }
    }

    /// Audit query builder. Returns an `AuditQuery` whose chain
    /// methods (`recent` / `byOperator` / `between` / `forceOnly`
    /// / `since`) configure the filter; `collect()` returns a list
    /// of JSON strings and `stream()` returns a sync iterator
    /// (resolved through `nextRecord()`).
    #[napi]
    pub fn audit(&self) -> AuditQuery {
        AuditQuery {
            client: self.client.clone(),
            recent_limit: None,
            by_operator: None,
            between: None,
            force_only: false,
            since: None,
        }
    }

    /// Subscribe to per-daemon / per-node log records.
    /// `filter` is an optional `LogFilterJs` object — every
    /// field is optional and missing fields match every record.
    /// Same runtime-context requirement as `snapshots`.
    #[napi]
    pub async fn subscribe_logs(&self, filter: Option<LogFilterJs>) -> Result<LogStream> {
        let core_filter = match filter {
            Some(f) => f.into_core()?,
            None => CoreLogFilter::default(),
        };
        Ok(LogStream {
            inner: tokio::sync::Mutex::new(Some(self.client.subscribe_logs(core_filter))),
        })
    }

    /// Subscribe to executor failure records starting from
    /// `since_seq + 1`. Pass `0n` (or omit) to start from
    /// whatever is still in the ring.
    #[napi]
    pub async fn subscribe_failures(&self, since_seq: Option<BigInt>) -> Result<FailureStream> {
        let seq = match since_seq {
            Some(bi) => crate::common::bigint_u64(bi)
                .map_err(|e| deck_err("invalid_argument", format!("sinceSeq: {}", e.reason)))?,
            None => 0,
        };
        Ok(FailureStream {
            inner: tokio::sync::Mutex::new(Some(self.client.subscribe_failures(seq))),
        })
    }
}

// =========================================================================
// Slice 2 — LogFilter POJO
// =========================================================================

/// Optional fields for filtering the log stream. Every field is
/// optional; missing fields match every record.
#[napi(object)]
pub struct LogFilterJs {
    /// `"trace"` | `"debug"` | `"info"` | `"warn"` | `"error"`.
    pub min_level: Option<String>,
    pub daemon_id: Option<BigInt>,
    pub node_id: Option<BigInt>,
    pub since_seq: Option<BigInt>,
}

impl LogFilterJs {
    fn into_core(self) -> Result<CoreLogFilter> {
        let mut f = CoreLogFilter::default();
        if let Some(s) = self.min_level {
            f.min_level = Some(parse_log_level_str(&s)?);
        }
        if let Some(bi) = self.daemon_id {
            f.daemon_id = Some(
                crate::common::bigint_u64(bi)
                    .map_err(|e| deck_err("invalid_filter", format!("daemonId: {}", e.reason)))?,
            );
        }
        if let Some(bi) = self.node_id {
            f.node_id = Some(
                crate::common::bigint_u64(bi)
                    .map_err(|e| deck_err("invalid_filter", format!("nodeId: {}", e.reason)))?,
            );
        }
        if let Some(bi) = self.since_seq {
            f.since_seq = Some(
                crate::common::bigint_u64(bi)
                    .map_err(|e| deck_err("invalid_filter", format!("sinceSeq: {}", e.reason)))?,
            );
        }
        Ok(f)
    }
}

fn parse_log_level_str(s: &str) -> Result<CoreLogLevel> {
    Ok(match s {
        "trace" | "TRACE" | "Trace" => CoreLogLevel::Trace,
        "debug" | "DEBUG" | "Debug" => CoreLogLevel::Debug,
        "info" | "INFO" | "Info" => CoreLogLevel::Info,
        "warn" | "WARN" | "Warn" | "warning" | "WARNING" => CoreLogLevel::Warn,
        "error" | "ERROR" | "Error" => CoreLogLevel::Error,
        other => {
            return Err(deck_err(
                "invalid_log_level",
                format!("log level must be one of trace|debug|info|warn|error; got {other:?}"),
            ));
        }
    })
}

fn log_level_to_str(level: CoreLogLevel) -> &'static str {
    match level {
        CoreLogLevel::Trace => "trace",
        CoreLogLevel::Debug => "debug",
        CoreLogLevel::Info => "info",
        CoreLogLevel::Warn => "warn",
        CoreLogLevel::Error => "error",
        _ => "unknown",
    }
}

// =========================================================================
// Slice 2 — LogRecord + FailureRecord wire forms
// =========================================================================

#[napi(object)]
pub struct LogRecordJs {
    pub seq: BigInt,
    pub ts_ms: BigInt,
    pub level: String,
    pub daemon_id: Option<BigInt>,
    pub node_id: Option<BigInt>,
    pub message: String,
}

fn log_record_to_js(record: &net::adapter::net::behavior::meshos::LogRecord) -> LogRecordJs {
    LogRecordJs {
        seq: BigInt::from(record.seq),
        ts_ms: BigInt::from(record.ts_ms),
        level: log_level_to_str(record.level).to_string(),
        daemon_id: record.daemon_id.map(BigInt::from),
        node_id: record.node_id.map(BigInt::from),
        message: record.message.clone(),
    }
}

#[napi(object)]
pub struct FailureRecordJs {
    pub seq: BigInt,
    pub source: String,
    pub reason: String,
    pub recorded_at_ms: BigInt,
}

fn failure_record_to_js(
    record: &net::adapter::net::behavior::meshos::FailureRecord,
) -> FailureRecordJs {
    FailureRecordJs {
        seq: BigInt::from(record.seq),
        source: record.source.clone(),
        reason: record.reason.clone(),
        recorded_at_ms: BigInt::from(record.recorded_at_ms),
    }
}

// =========================================================================
// Slice 2 — Log + Failure + Audit streams
// =========================================================================

#[napi]
pub struct LogStream {
    inner: tokio::sync::Mutex<Option<CoreLogStream>>,
}

#[napi]
impl LogStream {
    /// Resolve to the next log record, or `null` when the stream
    /// closes.
    #[napi]
    pub async fn next_record(&self) -> Result<Option<LogRecordJs>> {
        let mut guard = self.inner.lock().await;
        let stream = match guard.as_mut() {
            Some(s) => s,
            None => return Ok(None),
        };
        match stream.next().await {
            Some(Ok(r)) => Ok(Some(log_record_to_js(&r))),
            Some(Err(e)) => Err(deck_err_from(e)),
            None => Ok(None),
        }
    }

    /// Close the stream. Subsequent `nextRecord()` calls resolve
    /// to `null`.
    #[napi]
    pub async fn close(&self) {
        *self.inner.lock().await = None;
    }
}

#[napi]
pub struct FailureStream {
    inner: tokio::sync::Mutex<Option<CoreFailureStream>>,
}

#[napi]
impl FailureStream {
    #[napi]
    pub async fn next_record(&self) -> Result<Option<FailureRecordJs>> {
        let mut guard = self.inner.lock().await;
        let stream = match guard.as_mut() {
            Some(s) => s,
            None => return Ok(None),
        };
        match stream.next().await {
            Some(Ok(r)) => Ok(Some(failure_record_to_js(&r))),
            Some(Err(e)) => Err(deck_err_from(e)),
            None => Ok(None),
        }
    }

    #[napi]
    pub async fn close(&self) {
        *self.inner.lock().await = None;
    }
}

#[napi]
pub struct AuditStream {
    inner: tokio::sync::Mutex<Option<CoreAuditStream>>,
}

#[napi]
impl AuditStream {
    /// Resolve to the next audit record as a JSON string, or
    /// `null` when the stream closes. The TS wrapper parses the
    /// JSON into a native object.
    #[napi]
    pub async fn next_record(&self) -> Result<Option<String>> {
        let mut guard = self.inner.lock().await;
        let stream = match guard.as_mut() {
            Some(s) => s,
            None => return Ok(None),
        };
        match stream.next().await {
            Some(Ok(r)) => serde_json::to_string(&r)
                .map(Some)
                .map_err(|e| deck_err("audit_serialize_failed", e.to_string())),
            Some(Err(e)) => Err(deck_err_from(e)),
            None => Ok(None),
        }
    }

    #[napi]
    pub async fn close(&self) {
        *self.inner.lock().await = None;
    }
}

// =========================================================================
// Slice 2 — AuditQuery (fluent builder)
// =========================================================================

/// Fluent admin-audit query builder. Chain `recent` / `byOperator`
/// / `between` / `forceOnly` / `since` before calling `collect()`
/// (eager list of JSON strings) or `stream()` (async iterator).
#[napi]
pub struct AuditQuery {
    client: Arc<CoreClient>,
    recent_limit: Option<u32>,
    by_operator: Option<u64>,
    between: Option<(u64, u64)>,
    force_only: bool,
    since: Option<u64>,
}

impl AuditQuery {
    fn build<'a>(&self, client: &'a CoreClient) -> CoreAuditQuery<'a> {
        let mut q = client.audit();
        if let Some(n) = self.recent_limit {
            q = q.recent(n as usize);
        }
        if let Some(op) = self.by_operator {
            q = q.by_operator(op);
        }
        if let Some((start, end)) = self.between {
            q = q.between(start, end);
        }
        if self.force_only {
            q = q.force_only();
        }
        if let Some(s) = self.since {
            q = q.since(s);
        }
        q
    }
}

#[napi]
impl AuditQuery {
    #[napi]
    pub fn recent(&mut self, limit: u32) {
        self.recent_limit = Some(limit);
    }

    #[napi]
    pub fn by_operator(&mut self, operator_id: BigInt) -> Result<()> {
        self.by_operator = Some(
            crate::common::bigint_u64(operator_id)
                .map_err(|e| deck_err("invalid_argument", format!("operatorId: {}", e.reason)))?,
        );
        Ok(())
    }

    #[napi]
    pub fn between(&mut self, start_ms: BigInt, end_ms: BigInt) -> Result<()> {
        let start = crate::common::bigint_u64(start_ms)
            .map_err(|e| deck_err("invalid_argument", format!("startMs: {}", e.reason)))?;
        let end = crate::common::bigint_u64(end_ms)
            .map_err(|e| deck_err("invalid_argument", format!("endMs: {}", e.reason)))?;
        self.between = Some((start, end));
        Ok(())
    }

    #[napi]
    pub fn force_only(&mut self) {
        self.force_only = true;
    }

    #[napi]
    pub fn since(&mut self, seq: BigInt) -> Result<()> {
        self.since = Some(
            crate::common::bigint_u64(seq)
                .map_err(|e| deck_err("invalid_argument", format!("since: {}", e.reason)))?,
        );
        Ok(())
    }

    /// Eager — returns a list of JSON-encoded audit records. The
    /// TS wrapper parses each entry into a native object.
    #[napi]
    pub fn collect(&self) -> Result<Vec<String>> {
        let client = self.client.clone();
        let records = self.build(&client).collect();
        let mut out = Vec::with_capacity(records.len());
        for r in records {
            out.push(
                serde_json::to_string(&r)
                    .map_err(|e| deck_err("audit_serialize_failed", e.to_string()))?,
            );
        }
        Ok(out)
    }

    /// Returns an `AuditStream` for sync iteration over JSON-
    /// encoded audit records.
    #[napi]
    pub async fn stream(&self) -> AuditStream {
        AuditStream {
            inner: tokio::sync::Mutex::new(Some(self.build(&self.client).stream())),
        }
    }
}

// =========================================================================
// Slice 3 — ICE break-glass surface
//
// Typestate: IceProposal exposes only `simulate()`. The
// SimulatedIceProposal returned from `simulate()` is the only
// class exposing `commit(signatures)`. Direct commit on an
// IceProposal is unreachable at the class level — mirrors the
// substrate's compile-time typestate enforcement.
// =========================================================================

/// Avoid-list flush scope. Variants:
///
/// - `{ kind: 'global' }` — clear cluster-wide avoid lists.
/// - `{ kind: 'local', node: bigint }` — clear `node`'s avoid list.
/// - `{ kind: 'onPeer', peer: bigint }` — remove `peer` from every
///   node's avoid list.
#[napi(object)]
pub struct AvoidScopeJs {
    pub kind: String,
    pub node: Option<BigInt>,
    pub peer: Option<BigInt>,
}

impl AvoidScopeJs {
    fn into_core(self) -> Result<CoreAvoidScope> {
        match self.kind.as_str() {
            "global" | "Global" => Ok(CoreAvoidScope::Global),
            "local" | "Local" => {
                let bi = self.node.ok_or_else(|| {
                    deck_err(
                        "invalid_avoid_scope",
                        "scope 'local' requires 'node' BigInt".to_string(),
                    )
                })?;
                let node = crate::common::bigint_u64(bi)
                    .map_err(|e| deck_err("invalid_avoid_scope", format!("node: {}", e.reason)))?;
                Ok(CoreAvoidScope::Local { node })
            }
            "onPeer" | "on_peer" | "OnPeer" => {
                let bi = self.peer.ok_or_else(|| {
                    deck_err(
                        "invalid_avoid_scope",
                        "scope 'onPeer' requires 'peer' BigInt".to_string(),
                    )
                })?;
                let peer = crate::common::bigint_u64(bi)
                    .map_err(|e| deck_err("invalid_avoid_scope", format!("peer: {}", e.reason)))?;
                Ok(CoreAvoidScope::OnPeer { peer })
            }
            other => Err(deck_err(
                "invalid_avoid_scope",
                format!("scope.kind must be 'global' | 'local' | 'onPeer'; got {other:?}"),
            )),
        }
    }
}

/// `OperatorSignature` carried by ICE commits. `signature` must
/// be exactly 64 ed25519 signature bytes.
#[napi(object)]
pub struct OperatorSignatureJs {
    pub operator_id: BigInt,
    pub signature: Buffer,
}

impl OperatorSignatureJs {
    fn into_core(self) -> Result<CoreOperatorSignature> {
        let operator_id = crate::common::bigint_u64(self.operator_id)
            .map_err(|e| deck_err("invalid_signature", format!("operatorId: {}", e.reason)))?;
        Ok(CoreOperatorSignature {
            operator_id,
            signature: self.signature.as_ref().to_vec(),
        })
    }
}

/// Build a substrate `IceProposal` from a saved action. The
/// substrate's factories pin a fresh `issued_at_ms` per call;
/// the simulator is pure over the latest snapshot so the
/// committed envelope still binds to a stable `(action,
/// issued_at_ms, blast_hash)` triple.
///
/// `IceActionProposal` is `#[non_exhaustive]` — if the binding
/// is loaded against a substrate that introduced a new variant,
/// we refuse to map it instead of silently substituting
/// `ThawCluster` (the most destructive action). Caller must
/// rebuild the binding against the substrate it actually links.
fn build_core_proposal<'a>(
    client: &'a CoreClient,
    action: net::adapter::net::behavior::meshos::IceActionProposal,
) -> Result<CoreIceProposal<'a>> {
    use net::adapter::net::behavior::meshos::IceActionProposal as A;
    match action {
        A::FreezeCluster { ttl } => Ok(client.ice().freeze_cluster(ttl)),
        A::FlushAvoidLists { scope } => Ok(client.ice().flush_avoid_lists(scope)),
        A::ForceEvictReplica { chain, victim } => {
            Ok(client.ice().force_evict_replica(chain, victim))
        }
        A::ForceRestartDaemon { daemon } => Ok(client.ice().force_restart_daemon(daemon)),
        A::ForceCutover { chain, target } => Ok(client.ice().force_cutover(chain, target)),
        A::KillMigration { migration } => Ok(client.ice().kill_migration(migration)),
        A::ThawCluster => Ok(client.ice().thaw_cluster()),
        other => Err(deck_err(
            "unknown_action",
            format!(
                "IceActionProposal carries an unknown variant ({other:?}); \
                 rebuild the SDK binding against the current substrate"
            ),
        )),
    }
}

/// `IceCommands` — operator-side break-glass surface. Every
/// factory returns an `IceProposal` that must be `simulate()`-d
/// before commit.
#[napi]
pub struct IceCommands {
    client: Arc<CoreClient>,
}

#[napi]
impl IceCommands {
    #[napi]
    pub fn freeze_cluster(&self, ttl_ms: BigInt) -> Result<IceProposal> {
        let ttl_ms = crate::common::bigint_u64(ttl_ms)
            .map_err(|e| deck_err("invalid_argument", format!("ttlMs: {}", e.reason)))?;
        let p = self
            .client
            .ice()
            .freeze_cluster(Duration::from_millis(ttl_ms));
        Ok(IceProposal::new_from(
            self.client.clone(),
            p.action().clone(),
            p.issued_at_ms(),
        ))
    }

    #[napi]
    pub fn flush_avoid_lists(&self, scope: AvoidScopeJs) -> Result<IceProposal> {
        let scope = scope.into_core()?;
        let p = self.client.ice().flush_avoid_lists(scope);
        Ok(IceProposal::new_from(
            self.client.clone(),
            p.action().clone(),
            p.issued_at_ms(),
        ))
    }

    #[napi]
    pub fn force_evict_replica(&self, chain: BigInt, victim: BigInt) -> Result<IceProposal> {
        let chain = crate::common::bigint_u64(chain)
            .map_err(|e| deck_err("invalid_argument", format!("chain: {}", e.reason)))?;
        let victim = crate::common::bigint_u64(victim)
            .map_err(|e| deck_err("invalid_argument", format!("victim: {}", e.reason)))?;
        let p = self
            .client
            .ice()
            .force_evict_replica(chain as CoreChainId, victim);
        Ok(IceProposal::new_from(
            self.client.clone(),
            p.action().clone(),
            p.issued_at_ms(),
        ))
    }

    /// Propose force-restarting a daemon. `id` is the registry-
    /// local daemon id; `name` is `MeshDaemon::name()`.
    #[napi]
    pub fn force_restart_daemon(&self, id: BigInt, name: String) -> Result<IceProposal> {
        let id = crate::common::bigint_u64(id)
            .map_err(|e| deck_err("invalid_argument", format!("id: {}", e.reason)))?;
        let daemon = CoreDaemonRef { id, name };
        let p = self.client.ice().force_restart_daemon(daemon);
        Ok(IceProposal::new_from(
            self.client.clone(),
            p.action().clone(),
            p.issued_at_ms(),
        ))
    }

    #[napi]
    pub fn force_cutover(&self, chain: BigInt, target: BigInt) -> Result<IceProposal> {
        let chain = crate::common::bigint_u64(chain)
            .map_err(|e| deck_err("invalid_argument", format!("chain: {}", e.reason)))?;
        let target = crate::common::bigint_u64(target)
            .map_err(|e| deck_err("invalid_argument", format!("target: {}", e.reason)))?;
        let p = self
            .client
            .ice()
            .force_cutover(chain as CoreChainId, target);
        Ok(IceProposal::new_from(
            self.client.clone(),
            p.action().clone(),
            p.issued_at_ms(),
        ))
    }

    #[napi]
    pub fn kill_migration(&self, migration: BigInt) -> Result<IceProposal> {
        let migration = crate::common::bigint_u64(migration)
            .map_err(|e| deck_err("invalid_argument", format!("migration: {}", e.reason)))?;
        let p = self
            .client
            .ice()
            .kill_migration(migration as CoreMigrationId);
        Ok(IceProposal::new_from(
            self.client.clone(),
            p.action().clone(),
            p.issued_at_ms(),
        ))
    }

    #[napi]
    pub fn thaw_cluster(&self) -> IceProposal {
        let p = self.client.ice().thaw_cluster();
        IceProposal::new_from(self.client.clone(), p.action().clone(), p.issued_at_ms())
    }
}

/// Pre-simulation ICE proposal. Has no `commit` method —
/// typestate enforces `simulate()` first.
#[napi]
pub struct IceProposal {
    client: Arc<CoreClient>,
    /// Stored under a mutex so async `simulate` can consume the
    /// action without breaking napi's `&self` requirement.
    state: tokio::sync::Mutex<Option<net::adapter::net::behavior::meshos::IceActionProposal>>,
    issued_at_ms: u64,
}

impl IceProposal {
    fn new_from(
        client: Arc<CoreClient>,
        action: net::adapter::net::behavior::meshos::IceActionProposal,
        issued_at_ms: u64,
    ) -> Self {
        Self {
            client,
            state: tokio::sync::Mutex::new(Some(action)),
            issued_at_ms,
        }
    }
}

#[napi]
impl IceProposal {
    /// Milliseconds-since-`UNIX_EPOCH` stamp pinned at proposal
    /// construction. Signatures must cover this exact value.
    #[napi(getter)]
    pub fn issued_at_ms(&self) -> BigInt {
        BigInt::from(self.issued_at_ms)
    }

    /// Pre-execution preview. Consumes the proposal — subsequent
    /// `simulate()` calls throw `DeckSdkError(kind: "already_simulated")`.
    #[napi]
    pub async fn simulate(&self) -> Result<SimulatedIceProposal> {
        let action = self.state.lock().await.take().ok_or_else(|| {
            deck_err(
                "already_simulated",
                "IceProposal was already consumed by simulate()",
            )
        })?;
        let issued_at_ms = self.issued_at_ms;
        let action_for_commit = action.clone();
        let proposal = build_core_proposal(&self.client, action)?;
        let blast = match proposal.simulate().await {
            Ok(sim) => sim.blast_radius().clone(),
            Err(e) => return Err(deck_err_from(e)),
        };
        Ok(SimulatedIceProposal {
            client: self.client.clone(),
            state: tokio::sync::Mutex::new(Some(SimulatedState {
                action: action_for_commit,
                blast,
            })),
            issued_at_ms,
        })
    }
}

struct SimulatedState {
    action: net::adapter::net::behavior::meshos::IceActionProposal,
    blast: net::adapter::net::behavior::meshos::BlastRadius,
}

/// A simulated ICE proposal. The only class exposing `commit`.
#[napi]
pub struct SimulatedIceProposal {
    client: Arc<CoreClient>,
    state: tokio::sync::Mutex<Option<SimulatedState>>,
    issued_at_ms: u64,
}

#[napi]
impl SimulatedIceProposal {
    /// Milliseconds-since-`UNIX_EPOCH` stamp from the original
    /// `IceProposal`. Signatures must cover this exact value.
    #[napi(getter)]
    pub fn issued_at_ms(&self) -> BigInt {
        BigInt::from(self.issued_at_ms)
    }

    /// Pre-execution blast radius as a JSON string. The TS
    /// wrapper parses to a native object.
    #[napi]
    pub async fn blast_radius(&self) -> Result<String> {
        let guard = self.state.lock().await;
        let state = guard.as_ref().ok_or_else(|| {
            deck_err(
                "already_committed",
                "SimulatedIceProposal was already consumed by commit()",
            )
        })?;
        serde_json::to_string(&state.blast)
            .map_err(|e| deck_err("blast_serialize_failed", e.to_string()))
    }

    /// Blake3 digest of the blast radius. Signers must cover
    /// this exact hash.
    #[napi]
    pub async fn blast_hash(&self) -> Result<Buffer> {
        let guard = self.state.lock().await;
        let state = guard.as_ref().ok_or_else(|| {
            deck_err(
                "already_committed",
                "SimulatedIceProposal was already consumed by commit()",
            )
        })?;
        let hash = blast_radius_hash(&state.blast);
        Ok(Buffer::from(hash.as_ref()))
    }

    /// Deterministic signing payload: `ICE_SIGNING_DOMAIN ||
    /// issued_at_ms (le u64) || blast_hash (32) ||
    /// postcard(action)`. Returned for the offline / cross-deck
    /// signing flow — pair with
    /// `OperatorIdentity.signPayload(payload)` on a remote deck
    /// to produce a signature the local deck can pass into
    /// `commit([sig, ...])`.
    #[napi]
    pub async fn signing_payload(&self) -> Result<Buffer> {
        let guard = self.state.lock().await;
        let state = guard.as_ref().ok_or_else(|| {
            deck_err(
                "already_committed",
                "SimulatedIceProposal was already consumed by commit()",
            )
        })?;
        let hash = blast_radius_hash(&state.blast);
        let payload = ice_proposal_signing_payload(&state.action, self.issued_at_ms, &hash);
        Ok(Buffer::from(payload))
    }

    /// Commit with the supplied operator signatures. Consumes the
    /// proposal — subsequent calls throw `already_committed`.
    #[napi]
    pub async fn commit(&self, signatures: Vec<OperatorSignatureJs>) -> Result<ChainCommitJs> {
        let state = self.state.lock().await.take().ok_or_else(|| {
            deck_err(
                "already_committed",
                "SimulatedIceProposal was already consumed by commit()",
            )
        })?;
        let mut sigs = Vec::with_capacity(signatures.len());
        for s in signatures {
            sigs.push(s.into_core()?);
        }
        let client = self.client.clone();
        let proposal = build_core_proposal(&client, state.action)?;
        let simulated = proposal.simulate().await.map_err(deck_err_from)?;
        simulated
            .commit(&sigs)
            .await
            .map(|c| chain_commit_to_js(&c))
            .map_err(deck_err_from)
    }
}

// =========================================================================
// OperatorRegistry — operator-policy authoring + offline verify
// =========================================================================

/// Cluster operator-policy registry. Holds known operator public
/// keys keyed by 64-bit operator id; `verify` / `verifyBundle`
/// authenticate `OperatorSignatureJs` bundles against the
/// policy.
///
/// Use cases: authoring the cluster's operator-policy snapshot,
/// pre-verifying bundles before invoking
/// `SimulatedIceProposal.commit`, unit-testing operator
/// workflows. Mutations are thread-safe via an internal mutex.
#[napi]
pub struct OperatorRegistry {
    inner: Arc<Mutex<CoreOperatorRegistry>>,
}

impl OperatorRegistry {
    /// Snapshot the registry into an `Arc<CoreOperatorRegistry>`
    /// suitable for handing to `AdminVerifier::new`. The
    /// snapshot is detached — later mutations on the source
    /// registry don't propagate.
    fn snapshot(&self) -> std::sync::Arc<CoreOperatorRegistry> {
        let g = self.inner.lock().expect("registry mutex poisoned");
        std::sync::Arc::new(g.clone())
    }
}

#[napi]
impl OperatorRegistry {
    #[napi(constructor)]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(CoreOperatorRegistry::new())),
        }
    }

    /// Insert an operator's 32-byte ed25519 public key under
    /// `operatorId`.
    #[napi]
    pub fn insert(&self, operator_id: BigInt, public_key: Buffer) -> Result<()> {
        let op_id = crate::common::bigint_u64(operator_id)
            .map_err(|e| deck_err("invalid_public_key", format!("operatorId: {}", e.reason)))?;
        if public_key.len() != 32 {
            return Err(deck_err(
                "invalid_public_key",
                format!("publicKey must be 32 bytes, got {}", public_key.len()),
            ));
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(public_key.as_ref());
        let entity_id = EntityId::from_bytes(arr);
        let mut g = self
            .inner
            .lock()
            .map_err(|_| deck_err("registry_poisoned", "operator registry mutex poisoned"))?;
        g.insert(op_id, entity_id);
        Ok(())
    }

    /// Convenience — register `identity`'s public key under its
    /// derived operator id.
    #[napi]
    pub fn register(&self, identity: &OperatorIdentity) -> Result<()> {
        let mut g = self
            .inner
            .lock()
            .map_err(|_| deck_err("registry_poisoned", "operator registry mutex poisoned"))?;
        g.register(identity.inner.keypair());
        Ok(())
    }

    /// `true` iff `operatorId` is registered.
    #[napi]
    pub fn contains(&self, operator_id: BigInt) -> Result<bool> {
        let op_id = crate::common::bigint_u64(operator_id)
            .map_err(|e| deck_err("invalid_public_key", format!("operatorId: {}", e.reason)))?;
        let g = self
            .inner
            .lock()
            .map_err(|_| deck_err("registry_poisoned", "operator registry mutex poisoned"))?;
        Ok(g.contains(op_id))
    }

    /// Number of registered operators.
    #[napi(getter, js_name = "size")]
    pub fn size(&self) -> Result<u32> {
        let g = self
            .inner
            .lock()
            .map_err(|_| deck_err("registry_poisoned", "operator registry mutex poisoned"))?;
        Ok(g.len() as u32)
    }

    /// `true` iff no operators are registered.
    #[napi]
    pub fn is_empty(&self) -> Result<bool> {
        let g = self
            .inner
            .lock()
            .map_err(|_| deck_err("registry_poisoned", "operator registry mutex poisoned"))?;
        Ok(g.is_empty())
    }

    /// Verify a single signature over `payload`. Throws a
    /// `DeckSdkError`-shaped envelope with the appropriate kind
    /// on failure.
    #[napi]
    pub fn verify(&self, signature: OperatorSignatureJs, payload: Buffer) -> Result<()> {
        let sig = signature.into_core()?;
        let g = self
            .inner
            .lock()
            .map_err(|_| deck_err("registry_poisoned", "operator registry mutex poisoned"))?;
        g.verify(&sig, payload.as_ref()).map_err(verify_error_to_js)
    }

    /// Verify every signature in the bundle and confirm at least
    /// `threshold` *distinct* operator ids signed `payload`.
    /// The distinct-operator dedup gate is the M-of-N guarantee.
    #[napi]
    pub fn verify_bundle(
        &self,
        signatures: Vec<OperatorSignatureJs>,
        payload: Buffer,
        threshold: u32,
    ) -> Result<()> {
        let mut sigs = Vec::with_capacity(signatures.len());
        for s in signatures {
            sigs.push(s.into_core()?);
        }
        let g = self
            .inner
            .lock()
            .map_err(|_| deck_err("registry_poisoned", "operator registry mutex poisoned"))?;
        g.verify_bundle(&sigs, payload.as_ref(), threshold as usize)
            .map_err(verify_error_to_js)
    }
}

// =========================================================================
// AdminVerifier — substrate verifier wrapper
// =========================================================================

/// Substrate-side admin commit verifier. Bundles an
/// `OperatorRegistry` snapshot with the cluster's signature
/// threshold + freshness/skew/ICE-cooldown windows. Useful for
/// offline unit testing of operator-policy decisions.
///
/// Constructors snapshot the registry at build time — later
/// mutations on the source registry are not reflected. Rebuild
/// the verifier after every policy change.
#[napi]
pub struct AdminVerifier {
    inner: CoreAdminVerifier,
}

#[napi]
impl AdminVerifier {
    /// Build a verifier with `threshold` minimum signatures and
    /// the substrate defaults (300s freshness, 30s future-skew,
    /// 300s ICE cooldown). `threshold = 0` is clamped to `1`.
    #[napi(constructor)]
    pub fn new(registry: &OperatorRegistry, threshold: u32) -> Self {
        Self {
            inner: CoreAdminVerifier::new(registry.snapshot(), threshold as usize),
        }
    }

    /// Build with explicit freshness + future-skew windows and
    /// the default ICE cooldown.
    #[napi(factory)]
    pub fn with_freshness(
        registry: &OperatorRegistry,
        threshold: u32,
        freshness_window_ms: BigInt,
        future_skew_ms: BigInt,
    ) -> Result<Self> {
        let fresh_ms = crate::common::bigint_u64(freshness_window_ms).map_err(|e| {
            deck_err(
                "invalid_argument",
                format!("freshnessWindowMs: {}", e.reason),
            )
        })?;
        let skew_ms = crate::common::bigint_u64(future_skew_ms)
            .map_err(|e| deck_err("invalid_argument", format!("futureSkewMs: {}", e.reason)))?;
        Ok(Self {
            inner: CoreAdminVerifier::with_freshness(
                registry.snapshot(),
                threshold as usize,
                Duration::from_millis(fresh_ms),
                Duration::from_millis(skew_ms),
            ),
        })
    }

    /// Build with every policy knob explicit. Primarily for
    /// tests that need a short cooldown window.
    #[napi(factory)]
    pub fn with_full_policy(
        registry: &OperatorRegistry,
        threshold: u32,
        freshness_window_ms: BigInt,
        future_skew_ms: BigInt,
        ice_cooldown_ms: BigInt,
    ) -> Result<Self> {
        let fresh_ms = crate::common::bigint_u64(freshness_window_ms).map_err(|e| {
            deck_err(
                "invalid_argument",
                format!("freshnessWindowMs: {}", e.reason),
            )
        })?;
        let skew_ms = crate::common::bigint_u64(future_skew_ms)
            .map_err(|e| deck_err("invalid_argument", format!("futureSkewMs: {}", e.reason)))?;
        let cool_ms = crate::common::bigint_u64(ice_cooldown_ms)
            .map_err(|e| deck_err("invalid_argument", format!("iceCooldownMs: {}", e.reason)))?;
        Ok(Self {
            inner: CoreAdminVerifier::with_full_policy(
                registry.snapshot(),
                threshold as usize,
                Duration::from_millis(fresh_ms),
                Duration::from_millis(skew_ms),
                Duration::from_millis(cool_ms),
            ),
        })
    }

    #[napi(getter)]
    pub fn threshold(&self) -> u32 {
        self.inner.threshold() as u32
    }

    #[napi(getter)]
    pub fn freshness_window_ms(&self) -> BigInt {
        BigInt::from(self.inner.freshness_window().as_millis() as u64)
    }

    #[napi(getter)]
    pub fn future_skew_ms(&self) -> BigInt {
        BigInt::from(self.inner.future_skew().as_millis() as u64)
    }

    #[napi(getter)]
    pub fn ice_cooldown_ms(&self) -> BigInt {
        BigInt::from(self.inner.ice_cooldown().as_millis() as u64)
    }
}
