// `#[napi]` exports functions to JS but leaves them "unused" from
// Rust's POV; clippy's dead-code analysis doesn't apply to this
// module. Suppress at file scope.
#![allow(dead_code)]

//! NAPI surface for the groups feature — `ReplicaGroup` /
//! `ForkGroup` / `StandbyGroup`. Stage 2 of
//! `SDK_GROUPS_SURFACE_PLAN.md`.
//!
//! Each group takes an existing `DaemonRuntime` + a previously-
//! registered factory kind; the SDK's group wrappers reach into
//! the runtime's factory map and re-invoke the same TSFN-backed
//! factory we already use for migration-target reconstruction.
//! No new dispatcher trampolines are needed here.
//!
//! # Error prefix
//!
//! Migration errors use `daemon: migration: <kind>[: detail]`;
//! this module adds the parallel `daemon: group: <kind>[: detail]`
//! namespace for typed `GroupError` dispatch on the TS side.

use napi::bindgen_prelude::*;
use napi_derive::napi;

use std::sync::Arc;

use net_sdk::groups::{
    ForkGroup as SdkForkGroup, ForkGroupConfig as SdkForkGroupConfig, ForkRecord as SdkForkRecord,
    GroupError as SdkGroupError, GroupHealth as SdkGroupHealth, MemberInfo as SdkMemberInfo,
    MemberRole as SdkMemberRole, ReplicaGroup as SdkReplicaGroup,
    ReplicaGroupConfig as SdkReplicaGroupConfig, RequestContext as SdkRequestContext,
    StandbyGroup as SdkStandbyGroup, StandbyGroupConfig as SdkStandbyGroupConfig,
};

use net::adapter::net::behavior::loadbalance::Strategy as CoreStrategy;
use net::adapter::net::compute::DaemonHostConfig;

use net_sdk::compute::DaemonRuntime as SdkDaemonRuntime;

// =========================================================================
// Error prefix — `daemon: group:` namespace
// =========================================================================

const ERR_DAEMON_PREFIX: &str = "daemon:";

/// Validate a caller-supplied `BigInt` before narrowing to `u64`.
///
/// `BigInt::get_u64()` returns `(signed, value, lossless)`; silently
/// dropping either flag lets negative or `>u64::MAX` BigInts cross
/// the boundary as garbage `u64`s — corrupting node IDs and config
/// fields. Reject both with a `daemon: group: invalid-config` prefix
/// so the TS side classifies them as `GroupError` with
/// `kind: 'invalid-config'`.
fn group_bigint_u64(field: &str, b: BigInt) -> Result<u64> {
    crate::common::bigint_u64(b).map_err(|e| {
        Error::from_reason(format!(
            "{} group: invalid-config: {}: {}",
            ERR_DAEMON_PREFIX, field, e.reason,
        ))
    })
}

fn group_err(e: SdkGroupError) -> Error {
    Error::from_reason(format!(
        "{} group: {}",
        ERR_DAEMON_PREFIX,
        format_group_error(&e)
    ))
}

fn format_group_error(e: &SdkGroupError) -> String {
    match e {
        SdkGroupError::NotReady => "not-ready".to_string(),
        SdkGroupError::FactoryNotFound(kind) => {
            format!("factory-not-found: {kind}")
        }
        SdkGroupError::Daemon(d) => format!("daemon: {d}"),
        SdkGroupError::Core(core) => format_core_group_error(core),
    }
}

fn format_core_group_error(e: &net::adapter::net::compute::GroupError) -> String {
    use net::adapter::net::compute::GroupError as C;
    match e {
        C::NoHealthyMember => "no-healthy-member".to_string(),
        C::PlacementFailed(msg) => format!("placement-failed: {msg}"),
        C::RegistryFailed(msg) => format!("registry-failed: {msg}"),
        C::InvalidConfig(msg) => format!("invalid-config: {msg}"),
    }
}

// =========================================================================
// POJOs — config, member info, health, fork record
// =========================================================================

/// Load-balancing strategy for inbound group events. Exposed as a
/// string enum so TS callers pick via a stable name rather than an
/// integer discriminator. Only the strategies commonly useful for
/// group routing are surfaced; the core `loadbalance::Strategy`
/// enum has more variants (`WeightedRoundRobin`, `PowerOfTwo`,
/// `Adaptive`, etc.) that make sense for raw mesh streams but not
/// for group membership.
#[napi(string_enum = "kebab-case")]
pub enum StrategyJs {
    /// Rotate across healthy members per request.
    RoundRobin,
    /// Consistent-hash on the request's `routing_key`.
    ConsistentHash,
    /// Pick the least-loaded healthy member (by resource
    /// utilization metrics tracked in the LB state).
    LeastLoad,
    /// Pick the member with the fewest in-flight connections.
    LeastConnections,
    /// Select randomly among healthy members.
    Random,
}

impl From<StrategyJs> for CoreStrategy {
    fn from(s: StrategyJs) -> Self {
        match s {
            StrategyJs::RoundRobin => CoreStrategy::RoundRobin,
            StrategyJs::ConsistentHash => CoreStrategy::ConsistentHash,
            StrategyJs::LeastLoad => CoreStrategy::LeastLoad,
            StrategyJs::LeastConnections => CoreStrategy::LeastConnections,
            StrategyJs::Random => CoreStrategy::Random,
        }
    }
}

/// Daemon host config passed through to every group member.
/// Matches `DaemonHostConfigJs` in `compute.rs` but duplicated here
/// so this module stays self-contained.
#[napi(object)]
pub struct GroupHostConfigJs {
    pub auto_snapshot_interval: Option<BigInt>,
    pub max_log_entries: Option<u32>,
}

impl GroupHostConfigJs {
    /// Fallible conversion to the core `DaemonHostConfig`. Replaces
    /// the prior `From` impl which silently accepted negative /
    /// overflow BigInts on `auto_snapshot_interval`.
    pub(crate) fn into_core(self) -> Result<DaemonHostConfig> {
        let mut cfg = DaemonHostConfig::default();
        if let Some(v) = self.auto_snapshot_interval {
            cfg.auto_snapshot_interval = group_bigint_u64("hostConfig.autoSnapshotInterval", v)?;
        }
        if let Some(n) = self.max_log_entries {
            cfg.max_log_entries = n;
        }
        Ok(cfg)
    }
}

#[napi(object)]
pub struct ReplicaGroupConfigJs {
    /// Desired number of replicas. Must be ≥ 1.
    pub replica_count: u32,
    /// 32-byte seed for deterministic keypair derivation. Passed
    /// as `Buffer` of length 32; anything else rejects at spawn.
    pub group_seed: Buffer,
    pub lb_strategy: StrategyJs,
    pub host_config: Option<GroupHostConfigJs>,
}

#[napi(object)]
pub struct ForkGroupConfigJs {
    pub fork_count: u32,
    pub lb_strategy: StrategyJs,
    pub host_config: Option<GroupHostConfigJs>,
}

#[napi(object)]
pub struct StandbyGroupConfigJs {
    pub member_count: u32,
    pub group_seed: Buffer,
    pub host_config: Option<GroupHostConfigJs>,
}

/// Aggregate group health. Matches the core `GroupHealth` enum
/// as a tagged object so TS callers can discriminate on `status`.
#[napi(object)]
pub struct GroupHealthJs {
    /// `"healthy"` | `"degraded"` | `"dead"`.
    pub status: String,
    /// Populated on `"degraded"` — the current healthy count.
    pub healthy: Option<u32>,
    /// Populated on `"degraded"` — the total member count.
    pub total: Option<u32>,
}

impl From<SdkGroupHealth> for GroupHealthJs {
    fn from(h: SdkGroupHealth) -> Self {
        match h {
            SdkGroupHealth::Healthy => Self {
                status: "healthy".to_string(),
                healthy: None,
                total: None,
            },
            SdkGroupHealth::Degraded { healthy, total } => Self {
                status: "degraded".to_string(),
                healthy: Some(healthy as u32),
                total: Some(total as u32),
            },
            SdkGroupHealth::Dead => Self {
                status: "dead".to_string(),
                healthy: None,
                total: None,
            },
        }
    }
}

#[napi(object)]
pub struct MemberInfoJs {
    pub index: u32,
    pub origin_hash: BigInt,
    pub node_id: BigInt,
    pub entity_id: Buffer,
    pub healthy: bool,
}

impl From<&SdkMemberInfo> for MemberInfoJs {
    fn from(m: &SdkMemberInfo) -> Self {
        Self {
            index: m.index as u32,
            origin_hash: BigInt::from(m.origin_hash),
            node_id: BigInt::from(m.node_id),
            entity_id: Buffer::from(m.entity_id_bytes.to_vec()),
            healthy: m.healthy,
        }
    }
}

#[napi(object)]
pub struct ForkRecordJs {
    pub original_origin: BigInt,
    pub forked_origin: BigInt,
    pub fork_seq: BigInt,
    pub from_snapshot_seq: Option<BigInt>,
}

impl From<&SdkForkRecord> for ForkRecordJs {
    fn from(r: &SdkForkRecord) -> Self {
        Self {
            original_origin: BigInt::from(r.original_origin),
            forked_origin: BigInt::from(r.forked_origin),
            fork_seq: BigInt::from(r.fork_seq),
            from_snapshot_seq: r.from_snapshot_seq.map(BigInt::from),
        }
    }
}

/// Routing context handed to `routeEvent`. A single `routingKey`
/// covers the common stickiness case; callers who need session /
/// zone routing build a richer context via builder chaining in a
/// future expansion.
#[napi(object)]
pub struct RequestContextJs {
    pub routing_key: Option<String>,
    pub session_id: Option<String>,
    pub request_id: Option<String>,
}

impl From<RequestContextJs> for SdkRequestContext {
    fn from(ctx: RequestContextJs) -> Self {
        let mut rc = SdkRequestContext::new();
        if let Some(k) = ctx.routing_key {
            rc = rc.with_routing_key(k);
        }
        if let Some(s) = ctx.session_id {
            rc = rc.with_session(s);
        }
        if let Some(rid) = ctx.request_id {
            rc.request_id = Some(rid);
        }
        rc
    }
}

// =========================================================================
// Config parsing
// =========================================================================

fn parse_seed(buf: Buffer) -> Result<[u8; 32]> {
    let bytes = buf.as_ref();
    if bytes.len() != 32 {
        return Err(Error::from_reason(format!(
            "{} group: invalid-config: group_seed must be 32 bytes, got {}",
            ERR_DAEMON_PREFIX,
            bytes.len()
        )));
    }
    let mut seed = [0u8; 32];
    seed.copy_from_slice(bytes);
    Ok(seed)
}

// =========================================================================
// Async constructor helpers — run on tokio workers via napi's async
// method wrapping. Called from `DaemonRuntime::spawn_*_group` so the
// TSFN factory round-trip can unblock on the Node main thread.
// =========================================================================

/// Narrow a `u32` member-count to `u8` with a typed error on
/// overflow. Naming the error through `daemon: group: invalid-config`
/// keeps it in the same typed-error namespace as the other group
/// validation paths; a silent `as u8` wrap would turn a caller's
/// `replicaCount: 300` into 44 replicas with no diagnostic.
fn count_u8(field: &str, n: u32) -> Result<u8> {
    u8::try_from(n).map_err(|_| {
        Error::from_reason(format!(
            "{} group: invalid-config: {} {} exceeds {}",
            ERR_DAEMON_PREFIX,
            field,
            n,
            u8::MAX,
        ))
    })
}

pub(crate) async fn spawn_replica_group(
    runtime: SdkDaemonRuntime,
    kind: String,
    config: ReplicaGroupConfigJs,
) -> Result<ReplicaGroup> {
    let seed = parse_seed(config.group_seed)?;
    let cfg = SdkReplicaGroupConfig {
        replica_count: count_u8("replicaCount", config.replica_count)?,
        group_seed: seed,
        lb_strategy: config.lb_strategy.into(),
        host_config: match config.host_config {
            Some(h) => h.into_core()?,
            None => DaemonHostConfig::default(),
        },
    };
    // `SdkReplicaGroup::spawn` is a sync function that invokes the
    // factory inline. Running it on this tokio worker (rather than
    // the Node main thread) lets the TSFN factory callback complete
    // without deadlock — see the docstring on `spawnReplicaGroup`.
    let group = SdkReplicaGroup::spawn(&runtime, &kind, cfg).map_err(group_err)?;
    Ok(ReplicaGroup {
        inner: Arc::new(group),
    })
}

pub(crate) async fn spawn_fork_group(
    runtime: SdkDaemonRuntime,
    kind: String,
    parent_origin: u64,
    fork_seq: u64,
    config: ForkGroupConfigJs,
) -> Result<ForkGroup> {
    let cfg = SdkForkGroupConfig {
        fork_count: count_u8("forkCount", config.fork_count)?,
        lb_strategy: config.lb_strategy.into(),
        host_config: match config.host_config {
            Some(h) => h.into_core()?,
            None => DaemonHostConfig::default(),
        },
    };
    let group =
        SdkForkGroup::fork(&runtime, &kind, parent_origin, fork_seq, cfg).map_err(group_err)?;
    Ok(ForkGroup {
        inner: Arc::new(group),
    })
}

pub(crate) async fn spawn_standby_group(
    runtime: SdkDaemonRuntime,
    kind: String,
    config: StandbyGroupConfigJs,
) -> Result<StandbyGroup> {
    let seed = parse_seed(config.group_seed)?;
    let cfg = SdkStandbyGroupConfig {
        member_count: count_u8("memberCount", config.member_count)?,
        group_seed: seed,
        host_config: match config.host_config {
            Some(h) => h.into_core()?,
            None => DaemonHostConfig::default(),
        },
    };
    let group = SdkStandbyGroup::spawn(&runtime, &kind, cfg).map_err(group_err)?;
    Ok(StandbyGroup {
        inner: Arc::new(group),
    })
}

// =========================================================================
// ReplicaGroup
// =========================================================================

// Pre-fix this struct (and ForkGroup, StandbyGroup
// below) carried a `kind: String` field that was captured at
// spawn but never read on `self.kind` anywhere. The
// `Arc<SdkReplicaGroup>` (and friends) hold their own copy of
// the kind, so the binding-side field was dead code AND a
// maintenance trap: any future "use" of `self.kind` would read
// a frozen snapshot that could drift from the SDK-side kind.
// Removed for both correctness and footprint.
#[napi]
pub struct ReplicaGroup {
    inner: Arc<SdkReplicaGroup>,
}

#[napi]
impl ReplicaGroup {
    /// Resolve `ctx` to the best-available replica's `origin_hash`.
    /// Caller hands the returned hash to `runtime.deliver(...)`.
    #[napi]
    pub fn route_event(&self, ctx: RequestContextJs) -> Result<BigInt> {
        let rc: SdkRequestContext = ctx.into();
        self.inner
            .route_event(&rc)
            .map(BigInt::from)
            .map_err(group_err)
    }

    /// Resize the group to `n` replicas. The kind is fixed at
    /// spawn time — no external `kind` parameter is accepted so
    /// callers can't accidentally grow a group with a different
    /// factory than the one that produced the existing replicas.
    ///
    /// Async: growing calls the factory once per new replica, which
    /// fires the TSFN dispatcher. Main-thread invocation would
    /// deadlock on the TSFN callback — same argument as
    /// `spawnReplicaGroup`.
    #[napi]
    pub async fn scale_to(&self, n: u32) -> Result<()> {
        let n_u8 = u8::try_from(n).map_err(|_| {
            Error::from_reason(format!(
                "{} group: invalid-config: replica count {} exceeds {}",
                ERR_DAEMON_PREFIX,
                n,
                u8::MAX,
            ))
        })?;
        let inner = self.inner.clone();
        inner.scale_to(n_u8).map_err(group_err)
    }

    /// Handle failure of a node hosting one or more replicas.
    /// Returns the indices of replicas that were successfully
    /// respawned on other nodes. Async for the same
    /// deadlock-avoidance reason as `scaleTo`. Reuses the group's
    /// spawn kind — see `scaleTo` for the rationale.
    #[napi]
    pub async fn on_node_failure(&self, failed_node_id: BigInt) -> Result<Vec<u32>> {
        let node = group_bigint_u64("failedNodeId", failed_node_id)?;
        let inner = self.inner.clone();
        let replaced = inner.on_node_failure(node).map_err(group_err)?;
        Ok(replaced.into_iter().map(|i| i as u32).collect())
    }

    #[napi]
    pub fn on_node_recovery(&self, recovered_node_id: BigInt) -> Result<()> {
        let node = group_bigint_u64("recoveredNodeId", recovered_node_id)?;
        self.inner.on_node_recovery(node);
        Ok(())
    }

    #[napi(getter)]
    pub fn health(&self) -> GroupHealthJs {
        self.inner.health().into()
    }

    #[napi(getter)]
    pub fn group_id(&self) -> u32 {
        self.inner.group_id()
    }

    #[napi(getter)]
    pub fn replicas(&self) -> Vec<MemberInfoJs> {
        self.inner.replicas().iter().map(Into::into).collect()
    }

    #[napi(getter)]
    pub fn replica_count(&self) -> u32 {
        self.inner.replica_count() as u32
    }

    #[napi(getter)]
    pub fn healthy_count(&self) -> u32 {
        self.inner.healthy_count() as u32
    }
}

// =========================================================================
// ForkGroup
// =========================================================================

#[napi]
pub struct ForkGroup {
    inner: Arc<SdkForkGroup>,
    // Removed unused `kind` field; see ReplicaGroup.
}

#[napi]
impl ForkGroup {
    #[napi]
    pub fn route_event(&self, ctx: RequestContextJs) -> Result<BigInt> {
        self.inner
            .route_event(&ctx.into())
            .map(BigInt::from)
            .map_err(group_err)
    }

    #[napi]
    pub async fn scale_to(&self, n: u32) -> Result<()> {
        let n_u8 = u8::try_from(n).map_err(|_| {
            Error::from_reason(format!(
                "{} group: invalid-config: fork count {} exceeds {}",
                ERR_DAEMON_PREFIX,
                n,
                u8::MAX,
            ))
        })?;
        self.inner.clone().scale_to(n_u8).map_err(group_err)
    }

    #[napi]
    pub async fn on_node_failure(&self, failed_node_id: BigInt) -> Result<Vec<u32>> {
        let node = group_bigint_u64("failedNodeId", failed_node_id)?;
        self.inner
            .clone()
            .on_node_failure(node)
            .map_err(group_err)
            .map(|v| v.into_iter().map(|i| i as u32).collect())
    }

    #[napi]
    pub fn on_node_recovery(&self, recovered_node_id: BigInt) -> Result<()> {
        let node = group_bigint_u64("recoveredNodeId", recovered_node_id)?;
        self.inner.on_node_recovery(node);
        Ok(())
    }

    #[napi(getter)]
    pub fn health(&self) -> GroupHealthJs {
        self.inner.health().into()
    }

    #[napi(getter)]
    pub fn parent_origin(&self) -> BigInt {
        BigInt::from(self.inner.parent_origin())
    }

    #[napi(getter)]
    pub fn fork_seq(&self) -> BigInt {
        BigInt::from(self.inner.fork_seq())
    }

    #[napi(getter)]
    pub fn fork_records(&self) -> Vec<ForkRecordJs> {
        self.inner.fork_records().iter().map(Into::into).collect()
    }

    #[napi]
    pub fn verify_lineage(&self) -> bool {
        self.inner.verify_lineage()
    }

    #[napi(getter)]
    pub fn members(&self) -> Vec<MemberInfoJs> {
        self.inner.members().iter().map(Into::into).collect()
    }

    #[napi(getter)]
    pub fn fork_count(&self) -> u32 {
        self.inner.fork_count() as u32
    }

    #[napi(getter)]
    pub fn healthy_count(&self) -> u32 {
        self.inner.healthy_count() as u32
    }
}

// =========================================================================
// StandbyGroup
// =========================================================================

#[napi]
pub struct StandbyGroup {
    inner: Arc<SdkStandbyGroup>,
    // Removed unused `kind` field; see ReplicaGroup.
}

#[napi]
impl StandbyGroup {
    /// `origin_hash` of the current active. Deliver events by
    /// calling `runtime.deliver(group.activeOrigin, event)`; the
    /// group installs an internal post-delivery observer at spawn
    /// so every delivery is automatically captured in the replay
    /// buffer — no caller-side pairing required.
    #[napi(getter)]
    pub fn active_origin(&self) -> BigInt {
        BigInt::from(self.inner.active_origin())
    }

    #[napi]
    pub async fn sync_standbys(&self) -> Result<BigInt> {
        let seq = self.inner.clone().sync_standbys().map_err(group_err)?;
        Ok(BigInt::from(seq))
    }

    /// **Test-only.** Manually push an event into the replay
    /// buffer. Production code does NOT need to call this — a
    /// post-delivery observer installed at `spawn` / `promote`
    /// automatically feeds the buffer on every
    /// `runtime.deliver(group.activeOrigin, event)`. Exposed only
    /// so JS tests can simulate a gap between the last sync and
    /// a failure without driving a live runtime. Not part of the
    /// stable public API.
    #[napi]
    pub fn on_event_delivered(&self, event: crate::compute::CausalEventJs) -> Result<()> {
        use ::net::adapter::net::state::causal::{CausalEvent, CausalLink};
        // Route through the group validator (not the compute one)
        // so an invalid BigInt lands as `daemon: group: invalid-config`
        // — the TS wrapper's `toGroupError` classifier then turns it
        // into `GroupError { kind: 'invalid-config' }`. Using
        // `daemon_bigint_u64` would leak through as plain `DaemonError`
        // and break the typed-error contract this class promises.
        let sequence = group_bigint_u64("event.sequence", event.sequence)?;
        let event_origin = group_bigint_u64("event.originHash", event.origin_hash)?;
        let core_event = CausalEvent {
            link: CausalLink {
                origin_hash: event_origin,
                horizon_encoded: 0,
                sequence,
                parent_hash: 0,
            },
            payload: bytes::Bytes::copy_from_slice(event.payload.as_ref()),
            received_at: 0,
        };
        self.inner.on_event_delivered(core_event);
        Ok(())
    }

    #[napi]
    pub async fn promote(&self) -> Result<BigInt> {
        self.inner
            .clone()
            .promote()
            .map(BigInt::from)
            .map_err(group_err)
    }

    #[napi]
    pub async fn on_node_failure(&self, failed_node_id: BigInt) -> Result<Option<BigInt>> {
        let node = group_bigint_u64("failedNodeId", failed_node_id)?;
        self.inner
            .clone()
            .on_node_failure(node)
            .map(|opt| opt.map(BigInt::from))
            .map_err(group_err)
    }

    #[napi]
    pub fn on_node_recovery(&self, recovered_node_id: BigInt) -> Result<()> {
        let node = group_bigint_u64("recoveredNodeId", recovered_node_id)?;
        self.inner.on_node_recovery(node);
        Ok(())
    }

    #[napi(getter)]
    pub fn health(&self) -> GroupHealthJs {
        self.inner.health().into()
    }

    #[napi(getter)]
    pub fn active_healthy(&self) -> bool {
        self.inner.active_healthy()
    }

    #[napi(getter)]
    pub fn active_index(&self) -> u32 {
        self.inner.active_index() as u32
    }

    /// `"active"` | `"standby"` | `null` (out-of-range index).
    ///
    /// Pre-fix, `index as u8` silently wrapped — a JS caller
    /// passing 256 expected null (out-of-range) but received the role
    /// for member 0. `u8::try_from` returns `Err` on overflow, which
    /// we map to `None` for the documented contract.
    #[napi]
    pub fn member_role(&self, index: u32) -> Option<String> {
        let idx = u8::try_from(index).ok()?;
        self.inner
            .member_role(idx)
            .map(member_role_str)
            .map(String::from)
    }

    /// Returns the highest event sequence the standby member at
    /// `index` has acknowledged, or `null` if `index` is out of range.
    ///
    /// Same wrapping hazard as `member_role`. A JS caller
    /// polling sync progress for index 257 would silently get the
    /// lag of member 1 and could make an incorrect failover decision.
    #[napi]
    pub fn synced_through(&self, index: u32) -> Option<BigInt> {
        let idx = u8::try_from(index).ok()?;
        self.inner.synced_through(idx).map(BigInt::from)
    }

    #[napi(getter)]
    pub fn buffered_event_count(&self) -> u32 {
        self.inner.buffered_event_count() as u32
    }

    #[napi(getter)]
    pub fn group_id(&self) -> u32 {
        self.inner.group_id()
    }

    #[napi(getter)]
    pub fn members(&self) -> Vec<MemberInfoJs> {
        self.inner.members().iter().map(Into::into).collect()
    }

    #[napi(getter)]
    pub fn member_count(&self) -> u32 {
        self.inner.member_count() as u32
    }

    #[napi(getter)]
    pub fn standby_count(&self) -> u32 {
        self.inner.standby_count() as u32
    }
}

fn member_role_str(role: SdkMemberRole) -> &'static str {
    match role {
        SdkMemberRole::Active => "active",
        SdkMemberRole::Standby => "standby",
    }
}
