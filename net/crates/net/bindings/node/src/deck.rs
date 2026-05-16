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

use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use napi::bindgen_prelude::*;
use napi_derive::napi;

use net::adapter::net::behavior::deck::{
    AdminCommands as CoreAdminCommands, ChainCommit as CoreChainCommit, DeckClient as CoreClient,
    DeckClientConfig as CoreConfig, DeckError, OperatorIdentity as CoreIdentity,
    SnapshotStream as CoreSnapshotStream, StatusSummary, StatusSummaryStream as CoreStatusStream,
};
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
    pub async fn drop_replicas(
        &self,
        node: BigInt,
        chains: Vec<BigInt>,
    ) -> Result<ChainCommitJs> {
        let node = crate::common::bigint_u64(node)
            .map_err(|e| deck_err("invalid_argument", format!("node: {}", e.reason)))?;
        let mut converted = Vec::with_capacity(chains.len());
        for (i, bi) in chains.into_iter().enumerate() {
            let c = crate::common::bigint_u64(bi).map_err(|e| {
                deck_err("invalid_argument", format!("chains[{i}]: {}", e.reason))
            })?;
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
/// audit surfaces. Construct via `fromMeshos(sdk, identity)`
/// against a running `MeshOsDaemonSdk`.
#[napi]
pub struct DeckClient {
    client: Arc<CoreClient>,
}

#[napi]
impl DeckClient {
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
}
