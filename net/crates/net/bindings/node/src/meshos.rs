// `#[napi]` exports functions to JS but leaves them "unused" from
// Rust's POV, so clippy's dead-code analysis doesn't apply to this
// module. Suppress at file scope.
#![allow(dead_code)]

//! NAPI surface for the MeshOS daemon-author SDK.
//!
//! Slice 1 of `MESHOS_SDK_PLAN.md` Phase 3: `start`,
//! `registerDaemon`, `nextControl` / `tryNextControl`, `publishLog`,
//! `gracefulShutdown`, `metadata` / `refreshMetadata`, and the
//! substrate-side `publishCapabilities` stub.
//!
//! # Error envelope
//!
//! Errors throw `Error` whose `.message` carries the substrate
//! `<<meshos-sdk-kind:KIND>>MSG` discriminator verbatim — the
//! cross-binding format every language uses. The TS wrapper at
//! `sdk-ts/src/meshos.ts` parses the envelope into a typed
//! `MeshOsSdkError` with a `.kind` field.
//!
//! # Trait routing
//!
//! A JS `MeshOsDaemon` object is resolved at `registerDaemon` time
//! into a [`MeshOsDaemonBridge`] holding TSFNs for each callable.
//! Every `MeshDaemon` trait method body either fires its TSFN
//! synchronously (via the `call_with_return_value` + `mpsc` pattern
//! cribbed from [`compute.rs`]) or returns the substrate default
//! when no TSFN is installed.
//!
//! Same `callbackTimeoutMs` bounded-wait as compute — a JS callback
//! that doesn't return inside the budget surfaces as a typed
//! `ProcessFailed` rather than a deadlock.

use std::time::Duration;

use bytes::Bytes;
use napi::bindgen_prelude::*;
use napi_derive::napi;

use net::adapter::net::behavior::capability::{CapabilityFilter, CapabilitySet};
use net::adapter::net::behavior::meshos::logs::LogLevel as CoreLogLevel;
use net::adapter::net::behavior::meshos::{
    LoggingDispatcher, MaintenanceMirrorSnapshot, MaintenanceStateView, MeshOsConfig,
    MeshOsDaemonHandle as CoreHandle, MeshOsDaemonSdk as CoreSdk, MetadataView, PeerHealthSnapshot,
    PeerSnapshot, SdkError, DEFAULT_GRACEFUL_SHUTDOWN,
};
use net::adapter::net::compute::{
    DaemonControl as CoreDaemonControl, DaemonError as CoreDaemonError,
    DaemonHealth as CoreDaemonHealth, MeshDaemon,
};
use net::adapter::net::state::causal::CausalEvent;

use crate::identity::Identity;

/// Default ceiling on how long we block a tokio worker waiting for
/// a JS callback to respond. Same rationale as
/// [`compute.rs::DEFAULT_CALLBACK_TIMEOUT_MS`] — a re-entrant
/// deadlock surfaces as a typed error instead of stalling forever.
const DEFAULT_CALLBACK_TIMEOUT_MS: u32 = 60_000;

// =========================================================================
// Error envelope helpers
// =========================================================================

/// Build a NAPI `Error` carrying the substrate-style envelope. The
/// TS wrapper parses the envelope into a typed `MeshOsSdkError`
/// with a `.kind` attribute.
fn sdk_err(kind: &str, message: impl Into<String>) -> Error {
    Error::from_reason(format!("<<meshos-sdk-kind:{kind}>>{}", message.into()))
}

fn sdk_err_from(e: SdkError) -> Error {
    sdk_err(e.kind, e.message)
}

// =========================================================================
// TSFN types — one per JS daemon callback shape.
// =========================================================================

type ProcessTsfn = napi::threadsafe_function::ThreadsafeFunction<
    CausalEventJs,
    Vec<Buffer>,
    CausalEventJs,
    napi::Status,
    false,
>;

type SnapshotTsfn =
    napi::threadsafe_function::ThreadsafeFunction<(), Option<Buffer>, (), napi::Status, false>;

type RestoreTsfn = napi::threadsafe_function::ThreadsafeFunction<
    Buffer,
    napi::threadsafe_function::UnknownReturnValue,
    Buffer,
    napi::Status,
    false,
>;

type OnControlTsfn = napi::threadsafe_function::ThreadsafeFunction<
    DaemonControlJs,
    napi::threadsafe_function::UnknownReturnValue,
    DaemonControlJs,
    napi::Status,
    false,
>;

/// `health()` returns either a string (`"healthy"` / `"degraded"`
/// / `"unhealthy"`) or an object `{ kind, reason? }`. The TSFN
/// surface accepts an `Either<String, HealthObjJs>` and the
/// `MeshDaemon::health` impl resolves to the substrate enum.
type HealthTsfn = napi::threadsafe_function::ThreadsafeFunction<
    (),
    napi::Either<String, HealthObjJs>,
    (),
    napi::Status,
    false,
>;

/// `saturation()` returns a number clamped to `[0.0, 1.0]`.
type SaturationTsfn =
    napi::threadsafe_function::ThreadsafeFunction<(), f64, (), napi::Status, false>;

/// JS-side health response object form. `kind` is one of
/// `"healthy"` / `"degraded"` / `"unhealthy"`; `reason` rides
/// into the substrate's `Degraded { reason }` variant when
/// present.
#[napi(object)]
pub struct HealthObjJs {
    pub kind: String,
    pub reason: Option<String>,
}

// =========================================================================
// Cross-binding wire form — POJOs marshalled to JS.
// =========================================================================

/// The causal event handed to a daemon's `process(event)` callback.
/// Matches the cross-binding wire form: `{originHash, sequence,
/// payload}`. Sequence + originHash ride as `BigInt` to avoid the
/// `2^53` precision cliff.
#[napi(object)]
pub struct CausalEventJs {
    pub origin_hash: BigInt,
    pub sequence: BigInt,
    pub payload: Buffer,
}

impl From<&CausalEvent> for CausalEventJs {
    fn from(event: &CausalEvent) -> Self {
        Self {
            origin_hash: BigInt::from(event.link.origin_hash),
            sequence: BigInt::from(event.link.sequence),
            payload: Buffer::from(event.payload.as_ref()),
        }
    }
}

/// Supervisor → daemon control event, in the cross-binding wire
/// form. Variants:
///
/// - `{kind: "Shutdown", gracePeriodMs}`
/// - `{kind: "DrainStart", gracePeriodMs}`
/// - `{kind: "DrainFinish"}`
/// - `{kind: "BackpressureOn", level}` — `level` is `[0.0, 1.0]`.
/// - `{kind: "BackpressureOff"}`
/// - `{kind: "Unknown"}` — fallback when the substrate adds a
///   variant the binding hasn't been rebuilt against.
#[napi(object)]
pub struct DaemonControlJs {
    pub kind: String,
    pub grace_period_ms: Option<BigInt>,
    pub level: Option<f64>,
}

impl From<CoreDaemonControl> for DaemonControlJs {
    fn from(ev: CoreDaemonControl) -> Self {
        match ev {
            CoreDaemonControl::Shutdown { grace_period_ms } => Self {
                kind: "Shutdown".into(),
                grace_period_ms: Some(BigInt::from(grace_period_ms)),
                level: None,
            },
            CoreDaemonControl::DrainStart { grace_period_ms } => Self {
                kind: "DrainStart".into(),
                grace_period_ms: Some(BigInt::from(grace_period_ms)),
                level: None,
            },
            CoreDaemonControl::DrainFinish => Self {
                kind: "DrainFinish".into(),
                grace_period_ms: None,
                level: None,
            },
            CoreDaemonControl::BackpressureOn { level } => Self {
                kind: "BackpressureOn".into(),
                grace_period_ms: None,
                level: Some(level as f64),
            },
            CoreDaemonControl::BackpressureOff => Self {
                kind: "BackpressureOff".into(),
                grace_period_ms: None,
                level: None,
            },
            _ => Self {
                kind: "Unknown".into(),
                grace_period_ms: None,
                level: None,
            },
        }
    }
}

/// Maintenance state projection. The `kind` discriminator carries
/// the variant; `sinceMs` / `deadlineRemainingMs` / `reason` are
/// populated only for variants that carry them. Mirrors the
/// Python binding's dict envelope.
#[napi(object)]
pub struct MaintenanceStateJs {
    pub kind: String,
    pub since_ms: Option<BigInt>,
    pub deadline_remaining_ms: Option<BigInt>,
    pub reason: Option<String>,
}

impl From<&MaintenanceStateView> for MaintenanceStateJs {
    fn from(view: &MaintenanceStateView) -> Self {
        match view {
            MaintenanceStateView::Active => Self {
                kind: "Active".into(),
                since_ms: None,
                deadline_remaining_ms: None,
                reason: None,
            },
            MaintenanceStateView::EnteringMaintenance {
                since_ms,
                deadline_remaining_ms,
            } => Self {
                kind: "EnteringMaintenance".into(),
                since_ms: Some(BigInt::from(*since_ms)),
                deadline_remaining_ms: deadline_remaining_ms.map(BigInt::from),
                reason: None,
            },
            MaintenanceStateView::Maintenance { since_ms } => Self {
                kind: "Maintenance".into(),
                since_ms: Some(BigInt::from(*since_ms)),
                deadline_remaining_ms: None,
                reason: None,
            },
            MaintenanceStateView::ExitingMaintenance { since_ms } => Self {
                kind: "ExitingMaintenance".into(),
                since_ms: Some(BigInt::from(*since_ms)),
                deadline_remaining_ms: None,
                reason: None,
            },
            MaintenanceStateView::DrainFailed { since_ms, reason } => Self {
                kind: "DrainFailed".into(),
                since_ms: Some(BigInt::from(*since_ms)),
                deadline_remaining_ms: None,
                reason: Some(reason.clone()),
            },
            MaintenanceStateView::Recovery { since_ms } => Self {
                kind: "Recovery".into(),
                since_ms: Some(BigInt::from(*since_ms)),
                deadline_remaining_ms: None,
                reason: None,
            },
            _ => Self {
                kind: "Unknown".into(),
                since_ms: None,
                deadline_remaining_ms: None,
                reason: None,
            },
        }
    }
}

/// Read-only cluster view a daemon can observe. Peers are emitted
/// as a list of `[nodeId, PeerSnapshotJs]` tuples so JS callers
/// can `.entries()`/`.map()` cleanly while preserving BigInt
/// fidelity on the node id keys.
#[napi(object)]
pub struct MetadataViewJs {
    pub node_id: BigInt,
    pub daemon_id: BigInt,
    pub daemon_name: String,
    pub maintenance_state: MaintenanceStateJs,
    pub peers: Vec<PeerSnapshotEntryJs>,
}

#[napi(object)]
pub struct PeerSnapshotEntryJs {
    pub node_id: BigInt,
    pub snapshot: PeerSnapshotJs,
}

#[napi(object)]
pub struct PeerSnapshotJs {
    pub rtt_ms: Option<BigInt>,
    pub health: Option<String>,
    pub maintenance: Option<String>,
    pub cpu_load_1m: Option<f64>,
    pub mem_used_bytes: Option<BigInt>,
    pub mem_total_bytes: Option<BigInt>,
    pub disk_used_bytes: Option<BigInt>,
    pub disk_total_bytes: Option<BigInt>,
    pub saturation_trend: Option<f64>,
    pub capability_set: Vec<String>,
    pub software_version: Option<String>,
    pub forked_from: Option<BigInt>,
}

impl From<&PeerSnapshot> for PeerSnapshotJs {
    fn from(snap: &PeerSnapshot) -> Self {
        Self {
            rtt_ms: snap.rtt_ms.map(BigInt::from),
            health: snap.health.map(peer_health_str).map(String::from),
            maintenance: snap
                .maintenance
                .map(maintenance_mirror_str)
                .map(String::from),
            cpu_load_1m: snap.cpu_load_1m,
            mem_used_bytes: snap.mem_used_bytes.map(BigInt::from),
            mem_total_bytes: snap.mem_total_bytes.map(BigInt::from),
            disk_used_bytes: snap.disk_used_bytes.map(BigInt::from),
            disk_total_bytes: snap.disk_total_bytes.map(BigInt::from),
            saturation_trend: snap.saturation_trend.map(|x| x as f64),
            capability_set: snap.capability_set.iter().cloned().collect(),
            software_version: snap.software_version.clone(),
            forked_from: snap.forked_from.map(BigInt::from),
        }
    }
}

impl From<&MetadataView> for MetadataViewJs {
    fn from(view: &MetadataView) -> Self {
        Self {
            node_id: BigInt::from(view.node_id),
            daemon_id: BigInt::from(view.daemon_id),
            daemon_name: view.daemon_name.clone(),
            maintenance_state: (&view.maintenance_state).into(),
            peers: view
                .peers
                .iter()
                .map(|(node_id, snap)| PeerSnapshotEntryJs {
                    node_id: BigInt::from(*node_id),
                    snapshot: snap.into(),
                })
                .collect(),
        }
    }
}

fn peer_health_str(h: PeerHealthSnapshot) -> &'static str {
    match h {
        PeerHealthSnapshot::Healthy => "Healthy",
        PeerHealthSnapshot::Degraded => "Degraded",
        PeerHealthSnapshot::Unreachable => "Unreachable",
        _ => "Unknown",
    }
}

fn maintenance_mirror_str(m: MaintenanceMirrorSnapshot) -> &'static str {
    match m {
        MaintenanceMirrorSnapshot::Active => "Active",
        MaintenanceMirrorSnapshot::EnteringMaintenance => "EnteringMaintenance",
        MaintenanceMirrorSnapshot::Maintenance => "Maintenance",
        MaintenanceMirrorSnapshot::ExitingMaintenance => "ExitingMaintenance",
        MaintenanceMirrorSnapshot::DrainFailed => "DrainFailed",
        MaintenanceMirrorSnapshot::Recovery => "Recovery",
        _ => "Unknown",
    }
}

// =========================================================================
// Config POJOs
// =========================================================================

#[napi(object)]
pub struct MeshOsConfigJs {
    pub this_node: Option<BigInt>,
    pub tick_interval_ms: Option<BigInt>,
    pub event_queue_capacity: Option<u32>,
    pub action_queue_capacity: Option<u32>,
}

impl MeshOsConfigJs {
    fn into_core(self) -> Result<MeshOsConfig> {
        let mut cfg = MeshOsConfig::default();
        if let Some(bi) = self.this_node {
            cfg.this_node = crate::common::bigint_u64(bi)
                .map_err(|e| sdk_err("invalid_config", format!("thisNode: {}", e.reason)))?;
        }
        if let Some(bi) = self.tick_interval_ms {
            let ms = crate::common::bigint_u64(bi)
                .map_err(|e| sdk_err("invalid_config", format!("tickIntervalMs: {}", e.reason)))?;
            cfg.tick_interval = Duration::from_millis(ms);
        }
        if let Some(n) = self.event_queue_capacity {
            cfg.event_queue_capacity = n as usize;
        }
        if let Some(n) = self.action_queue_capacity {
            cfg.action_queue_capacity = n as usize;
        }
        Ok(cfg)
    }
}

// =========================================================================
// Log level — string-keyed parser
// =========================================================================

fn parse_log_level(level: &str) -> Result<CoreLogLevel> {
    Ok(match level {
        "trace" | "TRACE" | "Trace" => CoreLogLevel::Trace,
        "debug" | "DEBUG" | "Debug" => CoreLogLevel::Debug,
        "info" | "INFO" | "Info" => CoreLogLevel::Info,
        "warn" | "WARN" | "Warn" | "warning" | "WARNING" => CoreLogLevel::Warn,
        "error" | "ERROR" | "Error" => CoreLogLevel::Error,
        other => {
            return Err(sdk_err(
                "invalid_log_level",
                format!("log level must be one of trace|debug|info|warn|error; got {other:?}"),
            ));
        }
    })
}

// =========================================================================
// DaemonObjectTsfns — built from a JS daemon object at register
// time. napi-rs's `FromNapiValue` runs on the Node main thread,
// where the JS object is alive, and converts each callable into
// a Send + Sync TSFN.
// =========================================================================

pub struct DaemonObjectTsfns {
    name: String,
    process: ProcessTsfn,
    snapshot: Option<SnapshotTsfn>,
    restore: Option<RestoreTsfn>,
    on_control: Option<OnControlTsfn>,
    health: Option<HealthTsfn>,
    saturation: Option<SaturationTsfn>,
    /// Capability advertisement resolved synchronously at
    /// registration time. The Node main thread is the only place
    /// `FromNapiValue` runs, so we read these once here and
    /// cache the built `CapabilitySet`s — `MeshDaemon::*` calls
    /// from substrate workers then clone the cached set rather
    /// than re-entering JS land.
    required_capabilities: CapabilitySet,
    optional_capabilities: CapabilitySet,
}

impl napi::bindgen_prelude::TypeName for DaemonObjectTsfns {
    fn type_name() -> &'static str {
        "MeshOsDaemonObject"
    }
    fn value_type() -> napi::ValueType {
        napi::ValueType::Object
    }
}

impl napi::bindgen_prelude::ValidateNapiValue for DaemonObjectTsfns {}

impl napi::bindgen_prelude::FromNapiValue for DaemonObjectTsfns {
    unsafe fn from_napi_value(
        env: napi::sys::napi_env,
        napi_val: napi::sys::napi_value,
    ) -> Result<Self> {
        use napi::bindgen_prelude::{JsObjectValue as _, Object};

        let obj = unsafe { Object::from_napi_value(env, napi_val) }?;

        // `name` — string property. Resolved once at registration
        // so `MeshDaemon::name(&self) -> &str` can return a borrow
        // without crossing into JS land. JS callers who keep `name`
        // as a method should call it on the user side before
        // building the daemon object.
        let name: String = obj
            .get_named_property::<Option<String>>("name")?
            .ok_or_else(|| sdk_err("invalid_daemon", "daemon `name` must be a string property"))?;

        // Required: `process(event) -> Buffer[]`.
        let process_fn: Function<'_, CausalEventJs, Vec<Buffer>> =
            obj.get_named_property("process").map_err(|e| {
                sdk_err(
                    "invalid_daemon",
                    format!("daemon has no `process` method: {e}"),
                )
            })?;
        let process: ProcessTsfn = process_fn.build_threadsafe_function().build()?;

        // Optional: `snapshot()`, `restore(state)`, `onControl(event)`.
        let snapshot_fn: Option<Function<'_, (), Option<Buffer>>> =
            obj.get_named_property("snapshot")?;
        let snapshot: Option<SnapshotTsfn> = match snapshot_fn {
            Some(f) => Some(f.build_threadsafe_function().build()?),
            None => None,
        };

        let restore_fn: Option<
            Function<'_, Buffer, napi::threadsafe_function::UnknownReturnValue>,
        > = obj.get_named_property("restore")?;
        let restore: Option<RestoreTsfn> = match restore_fn {
            Some(f) => Some(f.build_threadsafe_function().build()?),
            None => None,
        };

        let on_control_fn: Option<
            Function<'_, DaemonControlJs, napi::threadsafe_function::UnknownReturnValue>,
        > = obj.get_named_property("onControl")?;
        let on_control: Option<OnControlTsfn> = match on_control_fn {
            Some(f) => Some(f.build_threadsafe_function().build()?),
            None => None,
        };

        let health_fn: Option<Function<'_, (), napi::Either<String, HealthObjJs>>> =
            obj.get_named_property("health")?;
        let health: Option<HealthTsfn> = match health_fn {
            Some(f) => Some(f.build_threadsafe_function().build()?),
            None => None,
        };

        let saturation_fn: Option<Function<'_, (), f64>> =
            obj.get_named_property("saturation")?;
        let saturation: Option<SaturationTsfn> = match saturation_fn {
            Some(f) => Some(f.build_threadsafe_function().build()?),
            None => None,
        };

        let required_capabilities = resolve_capabilities_napi(&obj, "requiredCapabilities")?;
        let optional_capabilities = resolve_capabilities_napi(&obj, "optionalCapabilities")?;

        Ok(DaemonObjectTsfns {
            name,
            process,
            snapshot,
            restore,
            on_control,
            health,
            saturation,
            required_capabilities,
            optional_capabilities,
        })
    }
}

/// Resolve a daemon-side capability advertisement (either a
/// `string[]` property or a `() -> string[]` callable) on the JS
/// main thread. Caches the result as a `CapabilitySet` for
/// substrate-side polls. Invalid shapes raise an `invalid_daemon`
/// error at registration time.
fn resolve_capabilities_napi(
    obj: &napi::bindgen_prelude::Object,
    attr: &str,
) -> Result<CapabilitySet> {
    use napi::bindgen_prelude::JsObjectValue as _;

    // First try as a `string[]` property — the common case.
    if let Ok(Some(tags)) = obj.get_named_property::<Option<Vec<String>>>(attr) {
        let mut set = CapabilitySet::new();
        for tag in tags {
            set = set.add_tag(tag);
        }
        return Ok(set);
    }
    // Fall back to a callable: `() -> string[]`.
    let callable: Option<Function<'_, (), Vec<String>>> = obj.get_named_property(attr)?;
    let Some(f) = callable else {
        return Ok(CapabilitySet::default());
    };
    let tags = f.call(()).map_err(|e| {
        sdk_err(
            "invalid_daemon",
            format!("`{attr}()` threw: {e}"),
        )
    })?;
    let mut set = CapabilitySet::new();
    for tag in tags {
        set = set.add_tag(tag);
    }
    Ok(set)
}

// =========================================================================
// MeshOsDaemonBridge — MeshDaemon impl driven by TSFNs
// =========================================================================

struct MeshOsDaemonBridge {
    name: String,
    process: ProcessTsfn,
    snapshot: Option<SnapshotTsfn>,
    restore: Option<RestoreTsfn>,
    on_control: Option<OnControlTsfn>,
    health: Option<HealthTsfn>,
    saturation: Option<SaturationTsfn>,
    required_capabilities: CapabilitySet,
    optional_capabilities: CapabilitySet,
    callback_timeout: Duration,
}

impl MeshOsDaemonBridge {
    fn from_tsfns(tsfns: DaemonObjectTsfns, callback_timeout: Duration) -> Self {
        Self {
            name: tsfns.name,
            process: tsfns.process,
            snapshot: tsfns.snapshot,
            restore: tsfns.restore,
            on_control: tsfns.on_control,
            health: tsfns.health,
            saturation: tsfns.saturation,
            required_capabilities: tsfns.required_capabilities,
            optional_capabilities: tsfns.optional_capabilities,
            callback_timeout,
        }
    }
}

impl MeshDaemon for MeshOsDaemonBridge {
    fn name(&self) -> &str {
        &self.name
    }

    fn requirements(&self) -> CapabilityFilter {
        // `requirements()` is a placement filter — what *kind*
        // of node the daemon needs. Distinct from the daemon's
        // own advertisement (`required_capabilities` /
        // `optional_capabilities`). The Node surface exposes
        // only the advertise side today; placement-filter
        // routing remains substrate-internal until a consumer
        // workflow needs it.
        CapabilityFilter::default()
    }

    fn required_capabilities(&self) -> CapabilitySet {
        self.required_capabilities.clone()
    }

    fn optional_capabilities(&self) -> CapabilitySet {
        self.optional_capabilities.clone()
    }

    fn process(&mut self, event: &CausalEvent) -> std::result::Result<Vec<Bytes>, CoreDaemonError> {
        let event_js = CausalEventJs::from(event);
        let (tx, rx) = std::sync::mpsc::sync_channel::<Result<Vec<Buffer>>>(1);

        let status = self.process.call_with_return_value(
            event_js,
            napi::threadsafe_function::ThreadsafeFunctionCallMode::NonBlocking,
            move |ret: Result<Vec<Buffer>>, _env| {
                let _ = tx.send(ret);
                Ok(())
            },
        );
        if status != napi::Status::Ok {
            return Err(CoreDaemonError::ProcessFailed(format!(
                "threadsafe_function enqueue failed: {status:?}"
            )));
        }

        let result = rx.recv_timeout(self.callback_timeout).map_err(|e| match e {
            std::sync::mpsc::RecvTimeoutError::Timeout => CoreDaemonError::ProcessFailed(format!(
                "JS `process` callback did not respond within {} ms (possible re-entrant deadlock or blocked Node main thread)",
                self.callback_timeout.as_millis(),
            )),
            std::sync::mpsc::RecvTimeoutError::Disconnected => {
                CoreDaemonError::ProcessFailed("JS `process` callback channel disconnected".into())
            }
        })?;

        match result {
            Ok(buffers) => Ok(buffers
                .into_iter()
                .map(|b| Bytes::copy_from_slice(b.as_ref()))
                .collect()),
            Err(e) => Err(CoreDaemonError::ProcessFailed(format!(
                "JS `process` threw: {e}"
            ))),
        }
    }

    fn snapshot(&self) -> Option<Bytes> {
        let tsfn = self.snapshot.as_ref()?;
        let (tx, rx) = std::sync::mpsc::sync_channel::<Result<Option<Buffer>>>(1);
        let status = tsfn.call_with_return_value(
            (),
            napi::threadsafe_function::ThreadsafeFunctionCallMode::NonBlocking,
            move |ret: Result<Option<Buffer>>, _env| {
                let _ = tx.send(ret);
                Ok(())
            },
        );
        if status != napi::Status::Ok {
            eprintln!("MeshOsDaemonBridge::snapshot enqueue failed: {status:?}");
            return None;
        }
        match rx.recv_timeout(self.callback_timeout) {
            Ok(Ok(Some(buf))) => Some(Bytes::copy_from_slice(buf.as_ref())),
            Ok(Ok(None)) => None,
            Ok(Err(e)) => {
                eprintln!("MeshOsDaemonBridge::snapshot JS callback threw: {e}");
                None
            }
            Err(_) => None,
        }
    }

    fn restore(&mut self, state: Bytes) -> std::result::Result<(), CoreDaemonError> {
        let tsfn = match self.restore.as_ref() {
            Some(t) => t,
            None => return Ok(()),
        };
        let buf = Buffer::from(state.as_ref());
        let (tx, rx) = std::sync::mpsc::sync_channel::<
            Result<napi::threadsafe_function::UnknownReturnValue>,
        >(1);
        let status = tsfn.call_with_return_value(
            buf,
            napi::threadsafe_function::ThreadsafeFunctionCallMode::NonBlocking,
            move |ret, _env| {
                let _ = tx.send(ret);
                Ok(())
            },
        );
        if status != napi::Status::Ok {
            return Err(CoreDaemonError::RestoreFailed(format!(
                "threadsafe_function enqueue failed: {status:?}"
            )));
        }
        match rx.recv_timeout(self.callback_timeout) {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(e)) => Err(CoreDaemonError::RestoreFailed(format!(
                "JS `restore` threw: {e}"
            ))),
            Err(_) => Err(CoreDaemonError::RestoreFailed(format!(
                "JS `restore` callback did not respond within {} ms",
                self.callback_timeout.as_millis(),
            ))),
        }
    }

    fn on_control(&mut self, event: CoreDaemonControl) {
        // Fire-and-forget — the substrate's on_control callback
        // returns `()`, and the SDK's `next_control` channel is the
        // canonical delivery path. The trait hook fires whenever
        // the supervisor routes a daemon-targeted action; the JS
        // side observes the same event through either path.
        let Some(tsfn) = self.on_control.as_ref() else {
            return;
        };
        let ev_js = DaemonControlJs::from(event);
        let _ = tsfn.call(
            ev_js,
            napi::threadsafe_function::ThreadsafeFunctionCallMode::NonBlocking,
        );
    }

    fn health(&self) -> CoreDaemonHealth {
        let Some(tsfn) = self.health.as_ref() else {
            return CoreDaemonHealth::Healthy;
        };
        let (tx, rx) = std::sync::mpsc::sync_channel::<Result<napi::Either<String, HealthObjJs>>>(1);
        let status = tsfn.call_with_return_value(
            (),
            napi::threadsafe_function::ThreadsafeFunctionCallMode::NonBlocking,
            move |ret, _env| {
                let _ = tx.send(ret);
                Ok(())
            },
        );
        if status != napi::Status::Ok {
            return CoreDaemonHealth::Healthy;
        }
        let resp = match rx.recv_timeout(self.callback_timeout) {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => {
                eprintln!("MeshOsDaemonBridge::health JS callback threw: {e}");
                return CoreDaemonHealth::Healthy;
            }
            Err(_) => return CoreDaemonHealth::Healthy,
        };
        parse_health_response(resp)
    }

    fn saturation(&self) -> f32 {
        let Some(tsfn) = self.saturation.as_ref() else {
            return 0.0;
        };
        let (tx, rx) = std::sync::mpsc::sync_channel::<Result<f64>>(1);
        let status = tsfn.call_with_return_value(
            (),
            napi::threadsafe_function::ThreadsafeFunctionCallMode::NonBlocking,
            move |ret, _env| {
                let _ = tx.send(ret);
                Ok(())
            },
        );
        if status != napi::Status::Ok {
            return 0.0;
        }
        match rx.recv_timeout(self.callback_timeout) {
            Ok(Ok(v)) => (v as f32).clamp(0.0, 1.0),
            Ok(Err(e)) => {
                eprintln!("MeshOsDaemonBridge::saturation JS callback threw: {e}");
                0.0
            }
            Err(_) => 0.0,
        }
    }
}

/// Resolve a JS health response into the substrate enum. Accepts
/// either a string discriminator or a `{ kind, reason? }` object.
/// Unknown kinds degrade to `Healthy` so a typo on the JS side
/// can't wedge the supervisor — operators see the daemon as
/// healthy until they fix the callback.
fn parse_health_response(resp: napi::Either<String, HealthObjJs>) -> CoreDaemonHealth {
    let (kind, reason) = match resp {
        napi::Either::A(s) => (s, None),
        napi::Either::B(obj) => (obj.kind, obj.reason),
    };
    match kind.as_str() {
        "healthy" => CoreDaemonHealth::Healthy,
        "degraded" => CoreDaemonHealth::Degraded {
            reason: reason.unwrap_or_else(|| "(unspecified)".into()),
        },
        "unhealthy" => CoreDaemonHealth::Unhealthy,
        _ => CoreDaemonHealth::Healthy,
    }
}

// =========================================================================
// MeshOsDaemonHandle — NAPI class wrapping the SDK handle
// =========================================================================

#[napi]
pub struct MeshOsDaemonHandle {
    /// `Option` because `gracefulShutdown` consumes the handle by
    /// value — afterward the wrapper holds `None` and every
    /// subsequent method throws `already_shutdown`.
    ///
    /// Wrapped in a `tokio::sync::Mutex` so `nextControl` /
    /// `gracefulShutdown` can hold the lock across `.await` points.
    /// napi-rs runs every `#[napi] async fn` on its own tokio
    /// runtime, so we just `.await` directly instead of spinning
    /// up a second runtime (which would deadlock with
    /// "Cannot start a runtime from within a runtime").
    inner: tokio::sync::Mutex<Option<CoreHandle>>,
    /// Cached identity so getters keep working after shutdown.
    daemon_id: u64,
    daemon_name: String,
}

#[napi]
impl MeshOsDaemonHandle {
    /// Substrate identifier (the keypair's origin hash). Stable
    /// across the handle's lifetime — readable after shutdown.
    #[napi(getter)]
    pub fn daemon_id(&self) -> BigInt {
        BigInt::from(self.daemon_id)
    }

    /// Daemon's `name` at registration. Readable after shutdown.
    #[napi(getter)]
    pub fn daemon_name(&self) -> String {
        self.daemon_name.clone()
    }

    /// Cached metadata view. Refresh via `refreshMetadata()` for
    /// fresh peer / maintenance state.
    #[napi]
    pub async fn metadata(&self) -> Result<MetadataViewJs> {
        let guard = self.inner.lock().await;
        let h = guard.as_ref().ok_or_else(|| {
            sdk_err(
                "already_shutdown",
                "daemon handle was already consumed by gracefulShutdown",
            )
        })?;
        Ok(h.metadata().into())
    }

    /// Rebuild the metadata view from the runtime's latest
    /// snapshot. Cheap.
    #[napi]
    pub async fn refresh_metadata(&self) -> Result<MetadataViewJs> {
        let mut guard = self.inner.lock().await;
        let h = guard.as_mut().ok_or_else(|| {
            sdk_err(
                "already_shutdown",
                "daemon handle was already consumed by gracefulShutdown",
            )
        })?;
        Ok(h.refresh_metadata().into())
    }

    /// Block until the next control event arrives, the runtime
    /// shuts down, or `timeoutMs` elapses. Resolves to the event
    /// or `null` on timeout / shutdown.
    #[napi]
    pub async fn next_control(
        &self,
        timeout_ms: Option<BigInt>,
    ) -> Result<Option<DaemonControlJs>> {
        let timeout = match timeout_ms {
            Some(bi) => Some(Duration::from_millis(
                crate::common::bigint_u64(bi)
                    .map_err(|e| sdk_err("invalid_argument", format!("timeoutMs: {}", e.reason)))?,
            )),
            None => None,
        };
        // Hold the lock across the awaited recv — control events
        // are per-daemon single-consumer, so serializing here is
        // correct. napi-rs already runs us on its tokio runtime;
        // no nested `block_on` needed.
        let mut guard = self.inner.lock().await;
        let h = guard.as_mut().ok_or_else(|| {
            sdk_err(
                "already_shutdown",
                "daemon handle was already consumed by gracefulShutdown",
            )
        })?;
        let result = match timeout {
            Some(d) => match tokio::time::timeout(d, h.next_control()).await {
                Ok(ev) => ev,
                Err(_) => None,
            },
            None => h.next_control().await,
        };
        Ok(result.map(DaemonControlJs::from))
    }

    /// Non-blocking control-event receive. Returns the next event
    /// or `null` if the channel is empty.
    #[napi]
    pub async fn try_next_control(&self) -> Result<Option<DaemonControlJs>> {
        let mut guard = self.inner.lock().await;
        let h = guard.as_mut().ok_or_else(|| {
            sdk_err(
                "already_shutdown",
                "daemon handle was already consumed by gracefulShutdown",
            )
        })?;
        Ok(h.try_next_control().map(DaemonControlJs::from))
    }

    /// Publish a log line tagged with this daemon's id. Non-blocking;
    /// throws `MeshOsSdkError` with `kind` `queue_full` or
    /// `loop_closed` when the substrate's log ring is saturated.
    #[napi]
    pub async fn publish_log(&self, level: String, message: String) -> Result<()> {
        let lvl = parse_log_level(&level)?;
        let guard = self.inner.lock().await;
        let h = guard.as_ref().ok_or_else(|| {
            sdk_err(
                "already_shutdown",
                "daemon handle was already consumed by gracefulShutdown",
            )
        })?;
        h.publish_log(lvl, message).map_err(sdk_err_from)
    }

    /// Publish (or update) the daemon's capability set.
    ///
    /// `caps` is a `CapabilitySetJs` POJO matching the cross-
    /// binding capability shape (`hardware?, software?, models?,
    /// tools?, tags?, limits?, metadata?`). `null` clears to the
    /// empty default.
    ///
    /// The substrate-side commit is a stub today (returns
    /// `Ok(())` without committing); the conversion still runs
    /// so a malformed schema surfaces a typed error at the
    /// binding boundary rather than silently lost when the
    /// chain commit lands.
    #[napi]
    pub async fn publish_capabilities(
        &self,
        caps: Option<crate::capabilities::CapabilitySetJs>,
    ) -> Result<()> {
        let cap_set = match caps {
            Some(c) => crate::capabilities::capability_set_from_js(c),
            None => CapabilitySet::default(),
        };
        let guard = self.inner.lock().await;
        let h = guard.as_ref().ok_or_else(|| {
            sdk_err(
                "already_shutdown",
                "daemon handle was already consumed by gracefulShutdown",
            )
        })?;
        h.publish_capabilities(cap_set).map_err(sdk_err_from)
    }

    /// Drive a graceful shutdown. Sends
    /// `Shutdown { gracePeriodMs }` on the daemon's control
    /// channel, parks for `graceMs`, then unregisters. Consumes
    /// the handle — subsequent method calls throw
    /// `MeshOsSdkError(kind: "already_shutdown")`.
    #[napi]
    pub async fn graceful_shutdown(&self, grace_ms: Option<BigInt>) -> Result<()> {
        let handle = self.inner.lock().await.take().ok_or_else(|| {
            sdk_err(
                "already_shutdown",
                "daemon handle was already consumed by gracefulShutdown",
            )
        })?;
        let grace = match grace_ms {
            Some(bi) => Duration::from_millis(
                crate::common::bigint_u64(bi)
                    .map_err(|e| sdk_err("invalid_argument", format!("graceMs: {}", e.reason)))?,
            ),
            None => DEFAULT_GRACEFUL_SHUTDOWN,
        };
        handle.graceful_shutdown(grace).await.map_err(sdk_err_from)
    }
}

// =========================================================================
// MeshOsDaemonSdk — NAPI class wrapping the SDK
// =========================================================================

#[napi]
pub struct MeshOsDaemonSdk {
    inner: tokio::sync::Mutex<Option<CoreSdk>>,
    callback_timeout: Duration,
}

#[napi]
impl MeshOsDaemonSdk {
    /// Start the SDK with optional config + the substrate's
    /// `LoggingDispatcher` as the action consumer.
    ///
    /// Async because `CoreSdk::start` calls `tokio::spawn`
    /// internally — that requires a tokio context, which the napi
    /// `async fn` runtime provides. Sync-factory would force us
    /// to build a second tokio runtime here, which collides with
    /// napi-rs's own (the "Cannot start a runtime from within a
    /// runtime" panic).
    ///
    /// `controlCapacity` overrides the per-daemon control-channel
    /// capacity (default 8 events). `callbackTimeoutMs` bounds how
    /// long the bridge waits for each JS callback to respond
    /// (default 60_000 ms) — see [`DEFAULT_CALLBACK_TIMEOUT_MS`]
    /// for the deadlock-prevention rationale.
    #[napi(factory)]
    pub async fn start(
        config: Option<MeshOsConfigJs>,
        control_capacity: Option<u32>,
        callback_timeout_ms: Option<u32>,
    ) -> Result<MeshOsDaemonSdk> {
        let cfg = match config {
            Some(c) => c.into_core()?,
            None => MeshOsConfig::default(),
        };
        let dispatcher = std::sync::Arc::new(LoggingDispatcher::new());
        let mut sdk = CoreSdk::start(cfg, dispatcher);
        if let Some(cap) = control_capacity {
            sdk = sdk.with_control_capacity(cap as usize);
        }
        let callback_timeout = Duration::from_millis(
            callback_timeout_ms.unwrap_or(DEFAULT_CALLBACK_TIMEOUT_MS) as u64,
        );
        Ok(MeshOsDaemonSdk {
            inner: tokio::sync::Mutex::new(Some(sdk)),
            callback_timeout,
        })
    }

    /// Register a JS daemon under the supplied identity. The
    /// daemon object must expose `name` (string property) and
    /// `process(event)`; optional `snapshot`, `restore`,
    /// `onControl`.
    ///
    /// Returns the handle that owns the daemon's lifecycle. Drop
    /// the handle to unregister (the Rust-side `Drop` impl still
    /// cleans up the registry slot); call `gracefulShutdown` for
    /// the explicit drain path.
    #[napi]
    pub async fn register_daemon(
        &self,
        daemon: DaemonObjectTsfns,
        identity: &Identity,
    ) -> Result<MeshOsDaemonHandle> {
        let bridge = MeshOsDaemonBridge::from_tsfns(daemon, self.callback_timeout);
        let keypair = identity.keypair_clone();
        let guard = self.inner.lock().await;
        let sdk = guard.as_ref().ok_or_else(|| {
            sdk_err(
                "already_shutdown",
                "MeshOsDaemonSdk was already consumed by shutdown",
            )
        })?;
        let handle = sdk
            .register_daemon(Box::new(bridge), keypair)
            .map_err(sdk_err_from)?;
        let daemon_id = handle.daemon_id();
        let daemon_name = handle.daemon_name().to_string();
        Ok(MeshOsDaemonHandle {
            inner: tokio::sync::Mutex::new(Some(handle)),
            daemon_id,
            daemon_name,
        })
    }

    /// Diagnostic counter — total control events the router
    /// dropped across every registered daemon because a daemon's
    /// channel was full.
    #[napi]
    pub async fn dropped_control_events(&self) -> Result<BigInt> {
        let guard = self.inner.lock().await;
        let sdk = guard.as_ref().ok_or_else(|| {
            sdk_err(
                "already_shutdown",
                "MeshOsDaemonSdk was already consumed by shutdown",
            )
        })?;
        Ok(BigInt::from(sdk.dropped_control_events()))
    }

    /// Tear down the wrapped runtime. Consumes the SDK — subsequent
    /// method calls throw `already_shutdown`.
    #[napi]
    pub async fn shutdown(&self) -> Result<()> {
        let sdk = self.inner.lock().await.take().ok_or_else(|| {
            sdk_err(
                "already_shutdown",
                "MeshOsDaemonSdk was already consumed by shutdown",
            )
        })?;
        sdk.shutdown()
            .await
            .map(|_stats| ())
            .map_err(|e| sdk_err("shutdown_failed", format!("runtime shutdown failed: {e:?}")))
    }
}

// Sibling-module accessor for the Deck binding. Returns `None`
// when the SDK has been consumed by `shutdown`. Same shape as
// the PyO3 binding's `with_core` accessor.
#[cfg(feature = "deck")]
impl MeshOsDaemonSdk {
    pub(crate) async fn with_core<R>(&self, f: impl FnOnce(&CoreSdk) -> R) -> Option<R> {
        let guard = self.inner.lock().await;
        guard.as_ref().map(f)
    }
}
