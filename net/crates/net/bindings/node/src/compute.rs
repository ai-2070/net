// `#[napi]` exports functions to JS but leaves them "unused" from
// Rust's POV, so clippy's dead-code analysis doesn't apply to this
// module. Suppress at file scope.
#![allow(dead_code)]

//! NAPI surface for the compute runtime — `MeshDaemon` + migration.
//!
//! Stage 3 of `SDK_COMPUTE_SURFACE_PLAN.md`.
//!
//! **Sub-step 2a** (this file): a TS caller can `spawn` and `stop`
//! daemons; the spawned daemon is a `NoopBridge` that implements
//! `MeshDaemon` with no-op methods. Event delivery is not yet
//! wired — sub-step 2b will invoke the factory TSFN, extract JS
//! `process` / `snapshot` / `restore` methods, and replace the
//! `NoopBridge` with an `EventDispatchBridge`.
//!
//! # Error prefix
//!
//! Every `Error` produced here is prefixed with `daemon:` so the TS
//! side's `classifyError` can route to a dedicated `DaemonError`
//! class. Mirrors the `identity:` / `cortex:` / `token:` convention
//! used by other modules.

use napi::bindgen_prelude::*;
use napi_derive::napi;

use bytes::Bytes;
use dashmap::DashMap;
use std::sync::Arc;
use std::time::Duration;

/// Default ceiling on how long we block a tokio worker waiting for a
/// JS callback (`process` / `snapshot` / `restore`) to respond.
///
/// **Why bounded:** `DaemonRegistry::deliver` holds the per-daemon
/// `parking_lot::Mutex` across `MeshDaemon::process`. If any
/// re-entrant path — a user callback that reaches back into the
/// runtime synchronously, or a Node main-thread blockage that
/// prevents the TSFN callback from firing — tries to reacquire that
/// mutex, an unbounded `rx.recv()` would deadlock with no diagnostic.
/// A bounded wait converts the deadlock into a typed
/// `DaemonError::ProcessFailed` (or `RestoreFailed`) with a clear
/// message, bounding blast radius to a single event.
///
/// Configurable per-daemon via `DaemonHostConfigJs.callbackTimeoutMs`
/// — default 60 s, which is long enough for legitimate heavy JS work
/// and short enough that a genuine deadlock surfaces promptly.
const DEFAULT_CALLBACK_TIMEOUT_MS: u32 = 60_000;

use net::adapter::net::behavior::capability::CapabilityFilter;
use net::adapter::net::compute::{DaemonError as CoreDaemonError, DaemonHostConfig, MeshDaemon};
use net::adapter::net::state::causal::CausalEvent;
use net_sdk::compute::{
    DaemonError as SdkDaemonError, DaemonHandle as SdkDaemonHandle,
    DaemonRuntime as SdkDaemonRuntime, MigrationError as SdkMigrationError, MigrationFailureReason,
    MigrationHandle as SdkMigrationHandle, MigrationOpts, MigrationPhase as CoreMigrationPhase,
    StateSnapshot,
};
use net_sdk::mesh::Mesh as SdkMesh;

use crate::NetMesh;

// =========================================================================
// Error prefix — stable string the TS layer dispatches on
// =========================================================================

const ERR_DAEMON_PREFIX: &str = "daemon:";

fn daemon_err(msg: impl Into<String>) -> Error {
    Error::from_reason(format!("{} {}", ERR_DAEMON_PREFIX, msg.into()))
}

/// Validate a caller-supplied `BigInt` before narrowing to `u64`.
///
/// `BigInt::get_u64()` returns `(signed, value, lossless)`; silently
/// discarding either flag lets a negative or `>u64::MAX` BigInt
/// cross the FFI boundary as a garbage `u64` — corrupting daemon
/// identifiers, sequence numbers, node IDs, or timeouts. Reject
/// both with a `daemon:`-prefixed error so the TS side classifies
/// them as `DaemonError`.
pub(crate) fn daemon_bigint_u64(field: &str, b: BigInt) -> Result<u64> {
    crate::common::bigint_u64(b).map_err(|e| daemon_err(format!("{}: {}", field, e.reason)))
}

/// Map an SDK `DaemonError` to a NAPI error with a stable
/// machine-readable kind prefix for migration failures. TS side
/// dispatches on the kind rather than parsing free-form messages.
///
/// Non-migration variants fall through to the default `daemon:`
/// prefix via `e.to_string()`, preserving pre-existing behavior.
///
/// **Wire format** (after the `daemon: ` prefix):
/// - Migration failures: `migration: <kind>[: <detail>]`
/// - Orchestrator errors: `migration: <kind>[: <detail>]`
/// - Everything else: verbatim `e.to_string()`
///
/// See [`MigrationErrorKind`] on the TS side for the full kind
/// vocabulary. Keep the kind strings stable — they're part of
/// the SDK's public API once callers `catch (e) { if (e.kind === ... )}`.
fn daemon_err_from_sdk(e: SdkDaemonError) -> Error {
    match e {
        SdkDaemonError::MigrationFailed(reason) => {
            daemon_err(format_migration_failure_reason(&reason))
        }
        SdkDaemonError::Migration(mig_err) => daemon_err(format_migration_error(&mig_err)),
        other => daemon_err(other.to_string()),
    }
}

fn format_migration_failure_reason(reason: &MigrationFailureReason) -> String {
    match reason {
        MigrationFailureReason::NotReady => "migration: not-ready".to_string(),
        MigrationFailureReason::FactoryNotFound => "migration: factory-not-found".to_string(),
        MigrationFailureReason::ComputeNotSupported => {
            "migration: compute-not-supported".to_string()
        }
        MigrationFailureReason::StateFailed(msg) => format!("migration: state-failed: {msg}"),
        MigrationFailureReason::AlreadyMigrating => "migration: already-migrating".to_string(),
        MigrationFailureReason::IdentityTransportFailed(msg) => {
            format!("migration: identity-transport-failed: {msg}")
        }
        MigrationFailureReason::NotReadyTimeout { attempts } => {
            format!("migration: not-ready-timeout: {attempts}")
        }
    }
}

fn format_migration_error(err: &SdkMigrationError) -> String {
    match err {
        SdkMigrationError::DaemonNotFound(origin) => {
            format!("migration: daemon-not-found: {origin:#x}")
        }
        SdkMigrationError::TargetUnavailable(node) => {
            format!("migration: target-unavailable: {node:#x}")
        }
        SdkMigrationError::NoTargetAvailable => "migration: no-target-available".to_string(),
        SdkMigrationError::StateFailed(msg) => format!("migration: state-failed: {msg}"),
        SdkMigrationError::AlreadyMigrating(origin) => {
            format!("migration: already-migrating: {origin:#x}")
        }
        SdkMigrationError::WrongPhase { expected, got } => {
            format!("migration: wrong-phase: {expected:?}: {got:?}")
        }
        SdkMigrationError::SnapshotTooLarge { size, max } => {
            format!("migration: snapshot-too-large: {size}: {max}")
        }
    }
}

// =========================================================================
// NAPI class — DaemonRuntime
// =========================================================================

/// Factory closure handle: a `ThreadsafeFunction` built from the
/// JS function passed to `register_factory`. Once built, the TSFN
/// is `Send + Sync + Clone` — it can be called from any tokio task
/// without being pinned to the Node main thread.
///
/// Return type is [`DaemonBridgeTsfns`] — on each invocation the
/// JS factory returns a `MeshDaemon`-shaped object; napi-rs's
/// `FromNapiValue` impl for `DaemonBridgeTsfns` runs inline on
/// the Node main thread (where TSFN callbacks execute) and
/// extracts the `process` / `snapshot` / `restore` functions into
/// fresh per-instance TSFNs. The resulting triple is Send + Sync
/// and can cross threads as a whole. Used by both the initial
/// spawn path and by the migration-target reconstruction closure
/// mirrored into the SDK factory map.
type FactoryTsfn =
    napi::threadsafe_function::ThreadsafeFunction<(), DaemonBridgeTsfns, (), napi::Status, false>;

/// `process` TSFN — invoked by [`EventDispatchBridge::process`]
/// on every inbound causal event. Takes a [`CausalEventJs`] by
/// value, returns `Buffer[]` which NAPI marshals into
/// `Vec<Buffer>` here.
///
/// `CalleeHandled = false`: we deal with JS-thrown errors by
/// routing them through the `Result<Return>` callback in
/// `call_with_return_value` (wrapped as `DaemonError::ProcessFailed`
/// on the way back into the SDK). No need for napi-rs's
/// callee-side error-propagation plumbing on top of that.
type ProcessTsfn = napi::threadsafe_function::ThreadsafeFunction<
    CausalEventJs,
    Vec<Buffer>,
    CausalEventJs,
    napi::Status,
    false,
>;

/// `snapshot` TSFN — invoked by the SDK's `take_snapshot` path.
/// Takes no args; returns `Buffer | null` which napi-rs marshals
/// into `Option<Buffer>` via `FromNapiValue`. A stateless daemon
/// returns `null`, which propagates as `Option<Bytes>::None` to
/// the core.
type SnapshotTsfn =
    napi::threadsafe_function::ThreadsafeFunction<(), Option<Buffer>, (), napi::Status, false>;

/// `restore` TSFN — invoked by the SDK on
/// `DaemonHost::from_snapshot`. Takes one `Buffer` (the daemon's
/// serialized state) and returns nothing useful — we only care
/// whether JS throws. `UnknownReturnValue` is napi-rs's `'static`
/// placeholder for "discard the return value."
type RestoreTsfn = napi::threadsafe_function::ThreadsafeFunction<
    Buffer,
    napi::threadsafe_function::UnknownReturnValue,
    Buffer,
    napi::Status,
    false,
>;

/// Extracted TSFNs for the three daemon methods. Produced by the
/// factory TSFN on every invocation — napi-rs calls our
/// [`FromNapiValue`] impl on the Node main thread, which pulls
/// the `process` / `snapshot` / `restore` function properties off
/// the factory's return object and builds a fresh TSFN per
/// method.
///
/// The TSFNs themselves are `Send + Sync + Clone` so the whole
/// triple can be packaged into a Rust closure that crosses
/// threads (e.g., the SDK kind-factory that the migration
/// dispatcher invokes from a tokio worker).
///
/// If the user's factory returned a Promise instead of a plain
/// object, the `FromNapiValue` impl fails because the Promise
/// has no `process` property — that's intentional. Migration-
/// target reconstruction requires a synchronous factory; async
/// factories work only for the initial `spawn` path (where the
/// TS layer awaits in JS before calling NAPI).
pub struct DaemonBridgeTsfns {
    process: ProcessTsfn,
    snapshot: Option<SnapshotTsfn>,
    restore: Option<RestoreTsfn>,
}

impl napi::bindgen_prelude::TypeName for DaemonBridgeTsfns {
    fn type_name() -> &'static str {
        "DaemonBridgeTsfns"
    }

    fn value_type() -> napi::ValueType {
        napi::ValueType::Object
    }
}

impl napi::bindgen_prelude::ValidateNapiValue for DaemonBridgeTsfns {}

impl napi::bindgen_prelude::FromNapiValue for DaemonBridgeTsfns {
    unsafe fn from_napi_value(
        env: napi::sys::napi_env,
        napi_val: napi::sys::napi_value,
    ) -> Result<Self> {
        use napi::bindgen_prelude::{JsObjectValue as _, Object};

        // Hydrate the JS return as an Object<'_>. Lifetime is
        // bound to this function scope — we consume it fully
        // inside here by extracting functions and building TSFNs.
        let obj = unsafe { Object::from_napi_value(env, napi_val) }?;

        // Required: `process`. `get_named_property::<Function<...>>`
        // validates the property is callable before returning;
        // a missing `process` surfaces as an InvalidArg error
        // that the TSFN's `Result<DaemonBridgeTsfns>` callback
        // will receive as `Err(_)`.
        let process_fn: Function<'_, CausalEventJs, Vec<Buffer>> =
            obj.get_named_property("process")?;
        let process: ProcessTsfn = process_fn.build_threadsafe_function().build()?;

        // Optional: `snapshot`, `restore`. Missing or `undefined`
        // properties decode to `None` via napi-rs's `Option<T>`
        // impl. A property that's present but not a function
        // still errors out — that's the right call (users who
        // provide the field must provide a function).
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

        Ok(DaemonBridgeTsfns {
            process,
            snapshot,
            restore,
        })
    }
}

/// Per-runtime compute surface. One instance per `NetMesh`; clone
/// handles are `Arc`-shared internally.
#[napi]
pub struct DaemonRuntime {
    /// SDK-level runtime. Owns the daemon registry, factory
    /// registry, migration orchestrator, and lifecycle state. The
    /// NAPI layer is a thin envelope — behavior lives in the SDK.
    inner: Arc<SdkDaemonRuntime>,
    /// Concurrent set of registered kinds. Used only by the
    /// NAPI-level `spawn` / `spawnFromSnapshot` guards that reject
    /// spawns against unregistered kinds with a friendlier error
    /// than the SDK's downstream `FactoryNotFound`. The authoritative
    /// factory storage lives on the SDK side (via the TSFN-backed
    /// closure registered in `register_factory`); the TSFN itself
    /// is owned by that SDK closure, not by this set.
    factories: Arc<DashMap<String, ()>>,
}

impl DaemonRuntime {
    /// Access the underlying SDK runtime. Used by the `groups`
    /// module to pass a `&SdkDaemonRuntime` into the group
    /// constructors.
    #[cfg(feature = "groups")]
    pub(crate) fn sdk_runtime(&self) -> &SdkDaemonRuntime {
        &self.inner
    }
}

#[napi]
impl DaemonRuntime {
    /// Build a compute runtime against an existing `NetMesh`.
    ///
    /// Shares the mesh's live `MeshNode` — no new socket, no new
    /// handshake table. The caller keeps ownership of their
    /// `NetMesh`; shutting down the `DaemonRuntime` does **not**
    /// shut down the underlying mesh.
    #[napi(factory)]
    pub fn create(mesh: &NetMesh) -> Result<DaemonRuntime> {
        let node = mesh.node_arc_clone()?;
        let channel_configs = mesh.channel_configs_arc();
        // Build an SDK-level `Mesh` that shares the same live
        // `MeshNode` as the caller's `NetMesh`. Identity is `None`
        // here because the NAPI layer manages identity separately
        // (via the `Identity` class); the daemon runtime only
        // needs the mesh for node_id, peer lookup, and subprotocol
        // handler install.
        let sdk_mesh = SdkMesh::from_node_arc(node, channel_configs, None);
        let sdk_rt = SdkDaemonRuntime::new(Arc::new(sdk_mesh));
        Ok(DaemonRuntime {
            inner: Arc::new(sdk_rt),
            factories: Arc::new(DashMap::new()),
        })
    }

    /// Transition to `Ready`. Installs the migration subprotocol
    /// handler on the underlying mesh. Idempotent — a second call
    /// on a runtime that's already `Ready` is a no-op; a call on
    /// a `ShuttingDown` runtime returns `daemon: shutting down`.
    #[napi]
    pub async fn start(&self) -> Result<()> {
        self.inner
            .start()
            .await
            .map_err(|e| daemon_err(e.to_string()))
    }

    /// Tear down the runtime. Drains daemons, clears factory
    /// registrations, uninstalls the migration handler. The
    /// underlying `NetMesh` is untouched.
    ///
    /// Factory TSFNs held by this runtime are dropped here, which
    /// releases their JS-side refs so the Node process can exit
    /// cleanly. Sub-step 1 only stores TSFNs — no per-daemon
    /// bridge TSFNs to clean up yet.
    #[napi]
    pub async fn shutdown(&self) -> Result<()> {
        self.inner
            .shutdown()
            .await
            .map_err(|e| daemon_err(e.to_string()))?;
        self.factories.clear();
        Ok(())
    }

    /// `true` iff the runtime has transitioned to `Ready` and
    /// has not yet begun shutting down.
    #[napi]
    pub fn is_ready(&self) -> bool {
        self.inner.is_ready()
    }

    /// Number of daemons currently registered with the runtime.
    #[napi]
    pub fn daemon_count(&self) -> u32 {
        // Cast: the SDK returns `usize`. Daemon counts are
        // realistically << 2^32; NAPI needs a number type.
        self.inner.daemon_count() as u32
    }

    /// Register a factory closure under `kind`. The factory is a
    /// JS function that returns a `MeshDaemon`-shaped object
    /// (with `process` / `snapshot` / `restore` methods).
    ///
    /// Sub-step 1 stores the factory but does **not invoke it**
    /// — event dispatch + daemon construction land in sub-step 2.
    /// Second registration of the same `kind` returns
    /// `daemon: factory for kind '<kind>' is already registered`.
    #[napi]
    pub fn register_factory(
        &self,
        kind: String,
        factory: Function<'_, (), DaemonBridgeTsfns>,
    ) -> Result<()> {
        // Build a threadsafe handle so the factory can be invoked
        // from any tokio task. The TSFN carries its own JS ref to
        // the user's factory function; cloning the TSFN is cheap
        // and thread-safe.
        let tsfn: FactoryTsfn = factory.build_threadsafe_function().build()?;

        // Atomic insert-or-error via DashMap's entry API. Matches
        // the SDK's `register_factory` contract — "second
        // registration fails." Do the NAPI-set insert first so we
        // fail fast on duplicates before touching the SDK registry.
        use dashmap::mapref::entry::Entry;
        match self.factories.entry(kind.clone()) {
            Entry::Occupied(_) => {
                return Err(daemon_err(format!(
                    "factory for kind '{kind}' is already registered"
                )));
            }
            Entry::Vacant(slot) => {
                slot.insert(());
            }
        }

        // Mirror into the SDK factory map. The closure calls back
        // into JS synchronously via the TSFN + mpsc pattern: fires
        // the factory on the Node main thread, waits for the
        // `DaemonBridgeTsfns` to come back, wraps them in an
        // `EventDispatchBridge`. Falls back to `ReconstructionErrorBridge`
        // if the JS call throws or the channel drops — the next `restore`
        // / `process` returns a typed error so the migration fails
        // visibly rather than silently "succeeding" with a noop daemon.
        //
        // The SDK closure owns the TSFN; NAPI only tracks the
        // kind string for fast spawn-time lookup.
        //
        // **Caveat (sub-step 5 landing note):** requires a
        // *synchronous* JS factory. An async factory returns a
        // Promise, which has no `process` property, so
        // `DaemonBridgeTsfns::from_napi_value` rejects with an
        // InvalidArg error. Local `spawn()` is unaffected — the
        // TS wrapper awaits async factories before calling NAPI,
        // so by the time NAPI sees the three methods they're
        // concrete functions.
        //
        // We need to call `tsfn` potentially multiple times (once
        // per migrated-in daemon), but `FactoryTsfn` is not
        // `Clone`. Wrap in an `Arc` so the SDK closure can share
        // the same TSFN across invocations.
        let factory_tsfn = Arc::new(tsfn);
        let kind_for_bridge = kind.clone();
        if let Err(e) = self.inner.register_factory(&kind, move || {
            build_bridge_from_tsfn(factory_tsfn.clone(), kind_for_bridge.clone())
        }) {
            // SDK registration failed — roll back the NAPI-side
            // insert so `register_factory` stays atomic across
            // both registries.
            self.factories.remove(&kind);
            return Err(daemon_err(e.to_string()));
        }
        Ok(())
    }

    /// Spawn a daemon of `kind` under the given identity with
    /// pre-bound `process` / `snapshot` / `restore` callbacks.
    ///
    /// The TS wrapper invokes the user-supplied factory in JS
    /// land, extracts the returned daemon object's methods, and
    /// passes them here as three separate `Function`s. NAPI wraps
    /// each in a `ThreadsafeFunction` so the eventual event
    /// dispatch (sub-step 3) can call them from any tokio task
    /// without being pinned to the Node main thread.
    ///
    /// **Sub-step 2b** (this file): the `EventDispatchBridge`
    /// stores the three TSFNs but its `MeshDaemon::process`
    /// implementation returns an empty output. Sub-step 3 wires
    /// that method to call `process_tsfn` synchronously and
    /// marshal the result.
    ///
    /// `snapshot` / `restore` are optional — stateless daemons
    /// omit them. If `snapshot` is present, the stored TSFN will
    /// be called by the host on `take_snapshot`; if absent, the
    /// host reports the daemon as stateless.
    ///
    /// # Method is sync but returns a `PromiseRaw`
    ///
    /// napi-rs `Function` values are `!Send`, so an `async fn`
    /// taking them would produce a non-`Send` future that tokio's
    /// worker pool can't schedule. The two-stage shape here
    /// (sync consumes the `Function`s to build `TSFN`s → then
    /// hands all-`Send` state to `env.spawn_future`) is the
    /// idiomatic napi-rs pattern for "sync setup, async
    /// continuation."
    // The argument list is the public NAPI/TS contract; bundling
    // into a single struct would force every TS caller to wrap
    // arguments in an object and break ABI compatibility.
    #[allow(clippy::too_many_arguments)]
    #[napi]
    pub fn spawn<'env>(
        &'env self,
        env: &'env Env,
        kind: String,
        identity: &crate::identity::Identity,
        process: Function<'_, CausalEventJs, Vec<Buffer>>,
        snapshot: Option<Function<'_, (), Option<Buffer>>>,
        restore: Option<Function<'_, Buffer, napi::threadsafe_function::UnknownReturnValue>>,
        config: Option<DaemonHostConfigJs>,
    ) -> Result<napi::bindgen_prelude::PromiseRaw<'env, DaemonHandle>> {
        // Guard: kind must have been registered. Registration is
        // the TS API's contract for "the target knows about this
        // daemon type"; skipping it would hide mis-configured TS
        // callers until a downstream operation fails cryptically.
        if !self.factories.contains_key(&kind) {
            return Err(daemon_err(format!(
                "no factory registered for kind '{kind}'"
            )));
        }

        // Build TSFNs synchronously — the napi-rs `Function`
        // values are `!Send`, so we can't carry them into the
        // async future. Each TSFN is `Send + Sync + Clone`; the
        // future below holds only TSFNs.
        let process_tsfn: ProcessTsfn = process.build_threadsafe_function().build()?;
        let snapshot_tsfn: Option<SnapshotTsfn> = match snapshot {
            Some(f) => Some(f.build_threadsafe_function().build()?),
            None => None,
        };
        let restore_tsfn: Option<RestoreTsfn> = match restore {
            Some(f) => Some(f.build_threadsafe_function().build()?),
            None => None,
        };

        let sdk_identity = identity.to_sdk_identity();
        let callback_timeout = config
            .as_ref()
            .map(|c| c.callback_timeout())
            .unwrap_or(Duration::from_millis(DEFAULT_CALLBACK_TIMEOUT_MS as u64));
        let sdk_config = match config {
            Some(c) => c.into_core()?,
            None => DaemonHostConfig::default(),
        };
        let runtime = self.inner.clone();
        let kind_label = kind.clone();

        let bridge = Box::new(EventDispatchBridge {
            name: kind,
            process: process_tsfn,
            snapshot: snapshot_tsfn,
            restore: restore_tsfn,
            callback_timeout,
        });

        // Kind-factory closure for migration-target reconstruction.
        // The TS node can't yet serve migrations targeting it
        // because the factory TSFN's return value (a JS daemon
        // object with method closures) can't be reconstructed from
        // Rust without re-running the TS factory. Sub-step 4+ will
        // address this; for now hand out a `ReconstructionErrorBridge`
        // so that *if* a migration lands here before the real path is
        // wired up, the failure is visible (first `restore` / `process`
        // returns a typed error) rather than a silent-noop "success."
        let kind_factory = move || -> Box<dyn MeshDaemon> {
            Box::new(ReconstructionErrorBridge::new(
                kind_label.clone(),
                "local-spawn path does not yet re-run the JS factory; register the kind via DaemonRuntime.registerFactory to enable migration-target reconstruction",
            ))
        };

        env.spawn_future(async move {
            runtime
                .spawn_with_daemon(sdk_identity, sdk_config, bridge, kind_factory)
                .await
                .map(DaemonHandle::from_sdk)
                .map_err(|e| daemon_err(e.to_string()))
        })
    }

    /// Stop a daemon, removing it from the runtime's registry.
    ///
    /// `origin_hash` is the 32-bit identifier carried on
    /// [`DaemonHandle`]. A second call for the same origin is a
    /// no-op during `ShuttingDown` and an error otherwise; the
    /// SDK's error is surfaced verbatim with the `daemon:` prefix.
    #[napi]
    pub async fn stop(&self, origin_hash: u32) -> Result<()> {
        self.inner
            .stop(origin_hash)
            .await
            .map_err(|e| daemon_err(e.to_string()))
    }

    /// Take a snapshot of a running daemon by `origin_hash`.
    ///
    /// Returns the daemon's serialized state as a `Buffer`, or
    /// `null` when the daemon is stateless (no `snapshot` method,
    /// or its `snapshot` returned null). The wire format is the
    /// core's `StateSnapshot::to_bytes` encoding — opaque to JS
    /// callers, but round-trippable via
    /// [`Self::spawn_from_snapshot`].
    ///
    /// Calls into `MeshDaemon::snapshot` on the bridge, which in
    /// turn fires the JS `snapshot` TSFN stored at spawn time.
    /// Same TSFN-blocking pattern as `deliver`.
    #[napi]
    pub async fn snapshot(&self, origin_hash: u32) -> Result<Option<Buffer>> {
        let snap = self
            .inner
            .snapshot(origin_hash)
            .await
            .map_err(|e| daemon_err(e.to_string()))?;
        Ok(snap.map(|s| Buffer::from(s.to_bytes())))
    }

    /// Spawn a daemon from a previously-taken `snapshot_bytes`
    /// payload. The daemon instance is built from the
    /// caller-supplied `process` / `snapshot` / `restore` functions
    /// (same shape as [`Self::spawn`]); its state is seeded from
    /// the snapshot via the `restore` TSFN.
    ///
    /// `snapshot_bytes` must be the exact `Buffer` returned by a
    /// prior call to [`Self::snapshot`]; the core validates the
    /// wire magic/version and rejects mismatched bytes as
    /// `daemon: snapshot decode failed`.
    ///
    /// Identity check: the snapshot's `entity_id` must match the
    /// caller's `identity`; mismatch surfaces as
    /// `daemon: snapshot identity mismatch`.
    ///
    /// Same sync-with-PromiseRaw shape as `spawn` — `Function`
    /// values are `!Send`, so we build TSFNs synchronously before
    /// handing off to the async continuation.
    // Same NAPI/TS-contract reasoning as `spawn` above.
    #[allow(clippy::too_many_arguments)]
    #[napi]
    pub fn spawn_from_snapshot<'env>(
        &'env self,
        env: &'env Env,
        kind: String,
        identity: &crate::identity::Identity,
        snapshot_bytes: Buffer,
        process: Function<'_, CausalEventJs, Vec<Buffer>>,
        snapshot: Option<Function<'_, (), Option<Buffer>>>,
        restore: Option<Function<'_, Buffer, napi::threadsafe_function::UnknownReturnValue>>,
        config: Option<DaemonHostConfigJs>,
    ) -> Result<napi::bindgen_prelude::PromiseRaw<'env, DaemonHandle>> {
        if !self.factories.contains_key(&kind) {
            return Err(daemon_err(format!(
                "no factory registered for kind '{kind}'"
            )));
        }

        // Decode the snapshot synchronously — cheap, and it lets
        // us surface a clean error before spinning up any TSFNs.
        // `from_bytes` returns `Option` (no error detail); the
        // core would reject mis-framed bytes anyway, but failing
        // fast here saves allocating the TSFNs and the bridge.
        let snapshot_decoded = StateSnapshot::from_bytes(snapshot_bytes.as_ref())
            .ok_or_else(|| daemon_err("snapshot decode failed"))?;

        let process_tsfn: ProcessTsfn = process.build_threadsafe_function().build()?;
        let snapshot_tsfn: Option<SnapshotTsfn> = match snapshot {
            Some(f) => Some(f.build_threadsafe_function().build()?),
            None => None,
        };
        let restore_tsfn: Option<RestoreTsfn> = match restore {
            Some(f) => Some(f.build_threadsafe_function().build()?),
            None => None,
        };

        let sdk_identity = identity.to_sdk_identity();
        let callback_timeout = config
            .as_ref()
            .map(|c| c.callback_timeout())
            .unwrap_or(Duration::from_millis(DEFAULT_CALLBACK_TIMEOUT_MS as u64));
        let sdk_config = match config {
            Some(c) => c.into_core()?,
            None => DaemonHostConfig::default(),
        };
        let runtime = self.inner.clone();
        let kind_label = kind.clone();

        let bridge = Box::new(EventDispatchBridge {
            name: kind,
            process: process_tsfn,
            snapshot: snapshot_tsfn,
            restore: restore_tsfn,
            callback_timeout,
        });

        // Same `ReconstructionErrorBridge` fallback for migration-target
        // reconstruction as `spawn` — see comment there for why loud
        // over silent.
        let kind_factory = move || -> Box<dyn MeshDaemon> {
            Box::new(ReconstructionErrorBridge::new(
                kind_label.clone(),
                "spawn-from-snapshot path does not yet re-run the JS factory; register the kind via DaemonRuntime.registerFactory to enable migration-target reconstruction",
            ))
        };

        env.spawn_future(async move {
            runtime
                .spawn_from_snapshot_with_daemon(
                    sdk_identity,
                    snapshot_decoded,
                    sdk_config,
                    bridge,
                    kind_factory,
                )
                .await
                .map(DaemonHandle::from_sdk)
                .map_err(|e| daemon_err(e.to_string()))
        })
    }

    /// Deliver a single causal event to the daemon identified by
    /// `origin_hash`. Invokes the daemon's JS `process(event)`
    /// callback via the `ThreadsafeFunction` stored at `spawn`
    /// time, waits for the `Buffer[]` return, and surfaces each
    /// output back to JS as a `Buffer`.
    ///
    /// Direct ingress — Stage 1 convenience. Mesh-dispatched
    /// delivery (inbound via the causal subprotocol) lands in a
    /// later stage, at which point this method becomes test
    /// sugar rather than the primary entry point.
    #[napi]
    pub async fn deliver(&self, origin_hash: u32, event: CausalEventJs) -> Result<Vec<Buffer>> {
        use bytes::Bytes as BytesType;
        use net::adapter::net::state::causal::CausalLink;

        let sequence = daemon_bigint_u64("event.sequence", event.sequence)?;
        let core_event = CausalEvent {
            link: CausalLink {
                origin_hash: event.origin_hash,
                horizon_encoded: 0,
                sequence,
                parent_hash: 0,
            },
            payload: BytesType::copy_from_slice(&event.payload),
            received_at: 0,
        };

        // SDK's `deliver` routes through `DaemonRegistry::deliver`
        // → `DaemonHost::deliver` → `MeshDaemon::process` on the
        // bridge, which is where the TSFN round-trip to JS
        // happens. The outputs come back as `Vec<Bytes>` wrapped
        // in causal events; we discard the chain wrapping and
        // return just the payload buffers.
        let outputs = self
            .inner
            .deliver(origin_hash, &core_event)
            .map_err(|e| daemon_err(e.to_string()))?;

        Ok(outputs
            .into_iter()
            .map(|ev| Buffer::from(ev.payload.as_ref()))
            .collect())
    }

    /// Initiate a migration for the daemon identified by
    /// `originHash`, moving it from `sourceNode` to `targetNode`.
    ///
    /// Returns a [`MigrationHandle`] whose `wait()` resolves when
    /// the migration reaches a terminal state — `Complete` on
    /// success, or throws a `DaemonError` on abort / failure.
    ///
    /// `sourceNode` / `targetNode` are `u64` node IDs (the hash of
    /// each node's static pubkey); pass them as `BigInt` to avoid
    /// silent precision loss past 2^53.
    ///
    /// **Local-source migration** — the common case where
    /// `sourceNode` is the current node — snapshots synchronously
    /// and ships `SnapshotReady` to the target. Remote-source
    /// migrations drive the state machine entirely via inbound
    /// wire messages.
    #[napi]
    pub async fn start_migration(
        &self,
        origin_hash: u32,
        source_node: BigInt,
        target_node: BigInt,
    ) -> Result<MigrationHandle> {
        let source = daemon_bigint_u64("sourceNode", source_node)?;
        let target = daemon_bigint_u64("targetNode", target_node)?;
        let handle = self
            .inner
            .start_migration(origin_hash, source, target)
            .await
            .map_err(daemon_err_from_sdk)?;
        Ok(MigrationHandle::from_sdk(handle))
    }

    /// `startMigration` with caller-supplied options. Use this to
    /// opt out of identity transport (when the daemon doesn't need
    /// to sign on the target) or to disable / shorten the
    /// NotReady-retry budget.
    #[napi]
    pub async fn start_migration_with(
        &self,
        origin_hash: u32,
        source_node: BigInt,
        target_node: BigInt,
        opts: MigrationOptsJs,
    ) -> Result<MigrationHandle> {
        let source = daemon_bigint_u64("sourceNode", source_node)?;
        let target = daemon_bigint_u64("targetNode", target_node)?;
        let core_opts = opts.into_core()?;
        let handle = self
            .inner
            .start_migration_with(origin_hash, source, target, core_opts)
            .await
            .map_err(daemon_err_from_sdk)?;
        Ok(MigrationHandle::from_sdk(handle))
    }

    /// Declare on the target node that a migration will land here
    /// for `origin_hash` of the given `kind`. Registers a
    /// **placeholder** factory — the migration snapshot's identity
    /// envelope supplies the real keypair at restore time.
    ///
    /// Must be called BEFORE the source starts the migration;
    /// otherwise the dispatcher has no factory entry and rejects
    /// the migration with `FactoryNotFound`.
    ///
    /// Fails with `daemon: factory not found for kind '<kind>'` if
    /// `kind` hasn't been registered via
    /// [`Self::register_factory`]. The source must migrate with
    /// `transport_identity: true` (default); without the envelope
    /// the dispatcher emits `IdentityTransportFailed`.
    #[napi]
    pub fn expect_migration(
        &self,
        kind: String,
        origin_hash: u32,
        config: Option<DaemonHostConfigJs>,
    ) -> Result<()> {
        let sdk_config = match config {
            Some(c) => c.into_core()?,
            None => DaemonHostConfig::default(),
        };
        self.inner
            .expect_migration(&kind, origin_hash, sdk_config)
            .map_err(|e| daemon_err(e.to_string()))
    }

    /// Pre-register a target-side identity for a migration that
    /// will NOT carry an identity envelope (e.g., the source used
    /// `transportIdentity: false`). The target must already hold
    /// the matching [`Identity`]; the dispatcher restores the
    /// daemon with that identity instead of overriding it from an
    /// envelope.
    ///
    /// For the common envelope-transport case, prefer
    /// [`Self::expect_migration`] — the caller doesn't need to
    /// know the daemon's private key ahead of time.
    #[napi]
    pub fn register_migration_target_identity(
        &self,
        kind: String,
        identity: &crate::identity::Identity,
        config: Option<DaemonHostConfigJs>,
    ) -> Result<()> {
        let sdk_identity = identity.to_sdk_identity();
        let sdk_config = match config {
            Some(c) => c.into_core()?,
            None => DaemonHostConfig::default(),
        };
        self.inner
            .register_migration_target_identity(&kind, sdk_identity, sdk_config)
            .map_err(|e| daemon_err(e.to_string()))
    }

    /// Query the orchestrator's current migration phase for
    /// `origin_hash`, returned as a string
    /// (`'snapshot' | 'transfer' | 'restore' | 'replay' | 'cutover' | 'complete'`)
    /// or `null` if no migration is in flight for that origin.
    ///
    /// Works on any node — source, target, or an observer that
    /// heard the migration on the mesh.
    #[napi]
    pub fn migration_phase(&self, origin_hash: u32) -> Option<String> {
        self.inner
            .migration_phase(origin_hash)
            .map(migration_phase_str)
    }
}

// =========================================================================
// Groups surface — separate `#[napi] impl` block so the outer
// `#[napi]` on the main `impl DaemonRuntime` doesn't try to
// register `spawn_*_group_c_callback` symbols that don't exist
// when the `groups` feature is off. napi-derive collects method
// names at macro-expansion time regardless of inner `#[cfg]`
// attributes, so the cfg gate has to live on the *impl block*
// itself to keep the compute-only build green.
// =========================================================================

#[cfg(feature = "groups")]
#[napi]
impl DaemonRuntime {
    /// Spawn a `ReplicaGroup` of `config.replicaCount` members
    /// using the factory registered under `kind`.
    ///
    /// Async so the SDK-level spawn runs on a tokio worker. The
    /// factory round-trip goes through the TSFN dispatcher — if
    /// this method were sync on the Node main thread, the TSFN
    /// callback would queue behind the blocked spawn and deadlock.
    #[napi]
    pub async fn spawn_replica_group(
        &self,
        kind: String,
        config: crate::groups::ReplicaGroupConfigJs,
    ) -> Result<crate::groups::ReplicaGroup> {
        crate::groups::spawn_replica_group(self.sdk_runtime().clone(), kind, config).await
    }

    /// Fork `config.forkCount` new daemons from a parent at
    /// `forkSeq`. Same deadlock-avoidance argument as
    /// `spawnReplicaGroup`.
    #[napi]
    pub async fn spawn_fork_group(
        &self,
        kind: String,
        parent_origin: u32,
        fork_seq: BigInt,
        config: crate::groups::ForkGroupConfigJs,
    ) -> Result<crate::groups::ForkGroup> {
        let seq = daemon_bigint_u64("forkSeq", fork_seq)?;
        crate::groups::spawn_fork_group(
            self.sdk_runtime().clone(),
            kind,
            parent_origin,
            seq,
            config,
        )
        .await
    }

    /// Spawn a `StandbyGroup`. Same deadlock-avoidance argument
    /// as `spawnReplicaGroup`.
    #[napi]
    pub async fn spawn_standby_group(
        &self,
        kind: String,
        config: crate::groups::StandbyGroupConfigJs,
    ) -> Result<crate::groups::StandbyGroup> {
        crate::groups::spawn_standby_group(self.sdk_runtime().clone(), kind, config).await
    }
}

// =========================================================================
// MigrationOpts POJO — mirrors the SDK struct
// =========================================================================

/// Options for `startMigrationWith`. All fields optional — omit to
/// take the runtime default.
///
/// See [`MigrationOpts`] on the Rust side for the full semantics of
/// each field.
#[napi(object)]
pub struct MigrationOptsJs {
    /// Seal the daemon's ed25519 seed into the outbound snapshot
    /// so the target keeps full signing capability. Default
    /// `true`; set `false` for pure compute daemons that only
    /// consume events.
    pub transport_identity: Option<bool>,
    /// Retry budget for `NotReady` targets, in milliseconds.
    /// Default 30_000 (30 s). Pass `0` to disable retry — the
    /// first `NotReady` surfaces as a terminal failure.
    pub retry_not_ready_ms: Option<BigInt>,
}

impl MigrationOptsJs {
    /// Fallible conversion to the core `MigrationOpts`. Replaces
    /// the prior `From` impl which silently accepted negative /
    /// overflow BigInts on `retry_not_ready_ms`.
    pub(crate) fn into_core(self) -> Result<MigrationOpts> {
        let mut opts = MigrationOpts::default();
        if let Some(t) = self.transport_identity {
            opts.transport_identity = t;
        }
        if let Some(ms_bi) = self.retry_not_ready_ms {
            let ms = daemon_bigint_u64("opts.retryNotReadyMs", ms_bi)?;
            opts.retry_not_ready = if ms == 0 {
                None
            } else {
                Some(std::time::Duration::from_millis(ms))
            };
        }
        Ok(opts)
    }
}

// =========================================================================
// MigrationHandle — NAPI class wrapping the SDK handle
// =========================================================================

/// Handle to an in-flight migration. Created by
/// [`DaemonRuntime::start_migration`] /
/// [`DaemonRuntime::start_migration_with`]; cloneable (shares
/// state) and cheap to pass across async boundaries.
///
/// Dropping the handle does NOT cancel the migration — the
/// orchestrator keeps driving it to completion in the background.
/// Callers who want to observe or abort must hold onto the handle.
#[napi]
pub struct MigrationHandle {
    origin_hash: u32,
    source_node: u64,
    target_node: u64,
    inner: SdkMigrationHandle,
}

impl MigrationHandle {
    fn from_sdk(handle: SdkMigrationHandle) -> Self {
        Self {
            origin_hash: handle.origin_hash,
            source_node: handle.source_node,
            target_node: handle.target_node,
            inner: handle,
        }
    }
}

#[napi]
impl MigrationHandle {
    /// 32-bit origin hash of the daemon being migrated.
    #[napi(getter)]
    pub fn origin_hash(&self) -> u32 {
        self.origin_hash
    }

    /// Node ID of the source (currently hosting) node. BigInt to
    /// match the u64 hash without precision loss.
    #[napi(getter)]
    pub fn source_node(&self) -> BigInt {
        BigInt::from(self.source_node)
    }

    /// Node ID of the target (post-cutover) node.
    #[napi(getter)]
    pub fn target_node(&self) -> BigInt {
        BigInt::from(self.target_node)
    }

    /// Current migration phase as a string:
    /// `'snapshot' | 'transfer' | 'restore' | 'replay' | 'cutover' | 'complete'`,
    /// or `null` once the migration has left the orchestrator's
    /// records (terminal success or abort). Callers distinguish
    /// success from abort by remembering the last non-null phase.
    #[napi]
    pub fn phase(&self) -> Option<String> {
        self.inner.phase().map(migration_phase_str)
    }

    /// Block until the migration reaches a terminal state. Resolves
    /// void on normal completion (saw `Complete`, then the
    /// orchestrator cleaned up); rejects with `DaemonError`
    /// carrying the failure reason otherwise.
    ///
    /// No wall-clock timeout — a migration stalled against an
    /// unresponsive peer will block indefinitely. Callers that
    /// need a bound should use [`MigrationHandle::wait_with_timeout`].
    #[napi]
    pub async fn wait(&self) -> Result<()> {
        // Clone because SDK's `wait(self)` consumes the handle; we
        // want the JS-side handle to stay usable for e.g. `phase()`
        // polling after wait returns.
        self.inner.clone().wait().await.map_err(daemon_err_from_sdk)
    }

    /// Like [`wait`] with a caller-controlled timeout. On timeout
    /// the orchestrator record is aborted and the promise rejects
    /// with a `DaemonError` describing the stall.
    #[napi]
    pub async fn wait_with_timeout(&self, timeout_ms: BigInt) -> Result<()> {
        let ms = daemon_bigint_u64("timeoutMs", timeout_ms)?;
        self.inner
            .clone()
            .wait_with_timeout(std::time::Duration::from_millis(ms))
            .await
            .map_err(daemon_err_from_sdk)
    }

    /// Request cancellation of the migration. Best-effort: a
    /// migration that has already passed `Cutover` cannot be
    /// cleanly undone (routing has flipped), and this call resolves
    /// without aborting.
    #[napi]
    pub async fn cancel(&self) -> Result<()> {
        self.inner.cancel().await.map_err(daemon_err_from_sdk)
    }
}

fn migration_phase_str(phase: CoreMigrationPhase) -> String {
    match phase {
        CoreMigrationPhase::Snapshot => "snapshot",
        CoreMigrationPhase::Transfer => "transfer",
        CoreMigrationPhase::Restore => "restore",
        CoreMigrationPhase::Replay => "replay",
        CoreMigrationPhase::Cutover => "cutover",
        CoreMigrationPhase::Complete => "complete",
    }
    .to_string()
}

// =========================================================================
// CausalEvent POJO — marshalled across NAPI into the JS daemon's `process`.
// =========================================================================

/// The causal event handed to a daemon's `process(event)` method.
///
/// Field shape matches
/// [`net::adapter::net::state::causal::CausalEvent`] with the
/// 64-bit `sequence` exposed as `BigInt` so JS doesn't silently
/// truncate.
#[napi(object)]
pub struct CausalEventJs {
    /// 32-bit hash of the emitting entity.
    pub origin_hash: u32,
    /// Sequence number in the emitter's causal chain.
    pub sequence: BigInt,
    /// Opaque payload bytes — identical to `event.payload` on the
    /// Rust side.
    pub payload: Buffer,
}

impl From<&CausalEvent> for CausalEventJs {
    fn from(event: &CausalEvent) -> Self {
        Self {
            origin_hash: event.link.origin_hash,
            sequence: BigInt::from(event.link.sequence),
            payload: Buffer::from(event.payload.as_ref()),
        }
    }
}

// =========================================================================
// DaemonHostConfig POJO — maps to core's struct
// =========================================================================

/// Host configuration for a daemon. Omitted fields fall back to
/// the core defaults (`auto_snapshot_interval: 0`,
/// `max_log_entries: 10_000`).
#[napi(object)]
pub struct DaemonHostConfigJs {
    /// Auto-snapshot cadence in events processed. `0` or absent =
    /// manual snapshots only.
    pub auto_snapshot_interval: Option<BigInt>,
    /// Maximum events to buffer before forcing a snapshot.
    pub max_log_entries: Option<u32>,
    /// Maximum time (milliseconds) to wait for a JS `process` /
    /// `snapshot` / `restore` callback to respond before surfacing
    /// a timeout error. Default 60_000 (60 s). See
    /// [`DEFAULT_CALLBACK_TIMEOUT_MS`] for the rationale.
    pub callback_timeout_ms: Option<u32>,
}

impl DaemonHostConfigJs {
    /// Fallible conversion to the core `DaemonHostConfig`. Replaces
    /// the prior `From` impl which silently accepted negative /
    /// overflow BigInts on `auto_snapshot_interval`.
    pub(crate) fn into_core(self) -> Result<DaemonHostConfig> {
        let mut cfg = DaemonHostConfig::default();
        if let Some(interval) = self.auto_snapshot_interval {
            cfg.auto_snapshot_interval =
                daemon_bigint_u64("hostConfig.autoSnapshotInterval", interval)?;
        }
        if let Some(max) = self.max_log_entries {
            cfg.max_log_entries = max;
        }
        Ok(cfg)
    }

    /// Resolve the per-bridge callback timeout. Borrowing here (vs.
    /// consuming via `into_core`) lets the caller read it at spawn
    /// time and still hand the config to the core conversion.
    pub(crate) fn callback_timeout(&self) -> Duration {
        Duration::from_millis(
            self.callback_timeout_ms
                .unwrap_or(DEFAULT_CALLBACK_TIMEOUT_MS) as u64,
        )
    }
}

// =========================================================================
// DaemonHandle — thin NAPI class over the SDK handle
// =========================================================================

/// Handle returned by [`DaemonRuntime::spawn`]. Identifies a
/// specific daemon by its `origin_hash`; cloning the JS object
/// shares the same underlying daemon. Dropping the handle does
/// **not** stop the daemon — callers must explicitly
/// [`DaemonRuntime::stop`] the origin.
#[napi]
pub struct DaemonHandle {
    origin_hash: u32,
    entity_id: [u8; 32],
    inner: SdkDaemonHandle,
}

impl DaemonHandle {
    fn from_sdk(handle: SdkDaemonHandle) -> Self {
        let origin_hash = handle.origin_hash;
        let entity_id = *handle.entity_id.as_bytes();
        Self {
            origin_hash,
            entity_id,
            inner: handle,
        }
    }
}

#[napi]
impl DaemonHandle {
    /// 32-bit hash of the daemon's identity — the key used by the
    /// registry, factory registry, and migration dispatcher.
    #[napi(getter)]
    pub fn origin_hash(&self) -> u32 {
        self.origin_hash
    }

    /// Full 32-byte `EntityId` (ed25519 public key) of the
    /// daemon's identity. Returned as a `Buffer` to match the
    /// convention used by `Identity.entityId`.
    #[napi(getter)]
    pub fn entity_id(&self) -> Buffer {
        Buffer::from(self.entity_id.to_vec())
    }

    /// Current runtime statistics for the daemon — event counters
    /// and snapshot count. Reads a live atomic snapshot from the
    /// registry; no TSFN round-trip, so the call is cheap enough
    /// to poll.
    ///
    /// Rejects with `daemon: not found` if the daemon has been
    /// stopped (or never successfully registered).
    #[napi]
    pub fn stats(&self) -> Result<DaemonStatsJs> {
        let stats = self.inner.stats().map_err(|e| daemon_err(e.to_string()))?;
        Ok(DaemonStatsJs {
            events_processed: BigInt::from(stats.events_processed),
            events_emitted: BigInt::from(stats.events_emitted),
            errors: BigInt::from(stats.errors),
            snapshots_taken: BigInt::from(stats.snapshots_taken),
        })
    }
}

// =========================================================================
// DaemonStats POJO — mirrors the core struct, u64 fields as BigInt.
// =========================================================================

/// Runtime statistics for a single daemon.
///
/// All counters are monotonically increasing for the daemon's
/// lifetime and reset to zero when the daemon is stopped + respawned
/// (including via `spawnFromSnapshot`, because the core's registry
/// replaces the host). Field shape mirrors
/// [`net::adapter::net::compute::DaemonStats`] with `u64` → `BigInt`
/// so JS doesn't silently lose precision past 2^53.
#[napi(object)]
pub struct DaemonStatsJs {
    /// Total events processed since spawn.
    pub events_processed: BigInt,
    /// Total output events emitted since spawn.
    pub events_emitted: BigInt,
    /// Total processing errors surfaced from `process`.
    pub errors: BigInt,
    /// Number of snapshots taken (manual + auto combined).
    pub snapshots_taken: BigInt,
}

// =========================================================================
// EventDispatchBridge — real daemon bridge holding method TSFNs.
// =========================================================================

/// Daemon bridge built at `spawn` time from three TSFNs extracted
/// by the TS wrapper from the user's factory return value.
///
/// **Sub-step 2b** (this file): the TSFNs are stored but not yet
/// invoked — `process` returns an empty output, `snapshot` /
/// `restore` are ignored. The storage + lifecycle paths work
/// end-to-end; sub-step 3 will wire the method implementations to
/// call the TSFNs and marshal arguments / return values.
struct EventDispatchBridge {
    name: String,
    process: ProcessTsfn,
    snapshot: Option<SnapshotTsfn>,
    restore: Option<RestoreTsfn>,
    /// Bounded wait applied to every `rx.recv_timeout(...)` below.
    /// See [`DEFAULT_CALLBACK_TIMEOUT_MS`] for why this exists.
    callback_timeout: Duration,
}

impl MeshDaemon for EventDispatchBridge {
    fn name(&self) -> &str {
        &self.name
    }

    fn requirements(&self) -> CapabilityFilter {
        CapabilityFilter::default()
    }

    /// Synchronously dispatch the event to the JS `process`
    /// callback and wait for the returned `Buffer[]`.
    ///
    /// The TSFN call itself is asynchronous w.r.t. the Node
    /// event loop — napi-rs queues the JS callback and fires it
    /// when Node gets around to it. We use
    /// `call_with_return_value` so napi-rs invokes our Rust
    /// callback with the parsed `Result<Vec<Buffer>>` once JS
    /// returns; that callback sends through an `std::sync::mpsc`
    /// channel which this tokio task blocks on.
    ///
    /// **Bounded wait.** `DaemonRegistry::deliver` holds the
    /// per-daemon `parking_lot::Mutex` across this call. If a
    /// re-entrant path (user callback calling back into the
    /// runtime, or main-thread blockage preventing the TSFN
    /// callback from firing) ever needs that same mutex, an
    /// unbounded `rx.recv()` would deadlock silently. We use
    /// `recv_timeout(self.callback_timeout)` instead so a real
    /// deadlock surfaces as a typed `ProcessFailed` error within
    /// a bounded budget (default 60 s; configurable via
    /// `DaemonHostConfigJs.callbackTimeoutMs`).
    fn process(&mut self, event: &CausalEvent) -> std::result::Result<Vec<Bytes>, CoreDaemonError> {
        let event_js = CausalEventJs::from(event);
        let (tx, rx) = std::sync::mpsc::sync_channel::<Result<Vec<Buffer>>>(1);

        let status = self.process.call_with_return_value(
            event_js,
            napi::threadsafe_function::ThreadsafeFunctionCallMode::NonBlocking,
            move |ret: Result<Vec<Buffer>>, _env| {
                // `send` only fails if the receiver was dropped —
                // that means the Rust caller gave up before we
                // got a chance to reply. Nothing productive to
                // do here; swallow to avoid napi-rs escalating
                // to a fatal process exit.
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
            Ok(buffers) => {
                // Convert `Vec<Buffer>` → `Vec<Bytes>` without
                // extra copies. `Buffer` derefs to `&[u8]`, and
                // `Bytes::copy_from_slice` allocates an
                // Arc-tracked payload. The daemon's contract
                // says outputs may be held indefinitely by the
                // causal chain, so we must not alias the
                // `Buffer`'s V8-managed memory — the copy is
                // load-bearing.
                Ok(buffers
                    .into_iter()
                    .map(|b| Bytes::copy_from_slice(b.as_ref()))
                    .collect())
            }
            Err(e) => Err(CoreDaemonError::ProcessFailed(format!(
                "JS `process` threw: {e}"
            ))),
        }
    }

    /// Synchronously ask the JS `snapshot()` callback for the
    /// daemon's current state. Same channel-and-block pattern as
    /// [`Self::process`].
    ///
    /// `MeshDaemon::snapshot` returns `Option<Bytes>` (no `Result`),
    /// so there's no way to surface an error from here. If the JS
    /// `snapshot` throws or the TSFN enqueue fails, we log a
    /// warning via `eprintln!` and return `None` — the core
    /// interprets that as "stateless at this moment" rather than
    /// "snapshot attempted but failed." Callers who need strict
    /// error propagation should rely on [`Self::restore`] round-
    /// trips to catch corrupted state.
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
            eprintln!("EventDispatchBridge::snapshot enqueue failed: {status:?}");
            return None;
        }
        // Bounded wait — same rationale as `process`. On timeout we
        // return `None` (the `snapshot()` contract is fallible-by-
        // absence since the signature returns `Option<Bytes>`), and
        // emit an `eprintln!` trail so operators can spot the stall.
        match rx.recv_timeout(self.callback_timeout) {
            Ok(Ok(Some(buf))) => Some(Bytes::copy_from_slice(buf.as_ref())),
            Ok(Ok(None)) => None,
            Ok(Err(e)) => {
                eprintln!("EventDispatchBridge::snapshot JS callback threw: {e}");
                None
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                eprintln!(
                    "EventDispatchBridge::snapshot timed out after {} ms (possible re-entrant deadlock)",
                    self.callback_timeout.as_millis(),
                );
                None
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                eprintln!("EventDispatchBridge::snapshot channel disconnected");
                None
            }
        }
    }

    /// Synchronously invoke the JS `restore(state)` callback.
    /// Errors propagate back as `CoreDaemonError::RestoreFailed` so
    /// the core's `DaemonHost::from_snapshot` can reject a bad
    /// snapshot before any events are processed.
    ///
    /// If no `restore` TSFN is installed (user's daemon didn't
    /// provide one), the state is silently ignored — matches the
    /// default `MeshDaemon::restore` behaviour in core.
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
        let result = rx.recv_timeout(self.callback_timeout).map_err(|e| match e {
            std::sync::mpsc::RecvTimeoutError::Timeout => CoreDaemonError::RestoreFailed(format!(
                "JS `restore` callback did not respond within {} ms (possible re-entrant deadlock or blocked Node main thread)",
                self.callback_timeout.as_millis(),
            )),
            std::sync::mpsc::RecvTimeoutError::Disconnected => {
                CoreDaemonError::RestoreFailed("JS `restore` callback channel disconnected".into())
            }
        })?;
        match result {
            Ok(_) => Ok(()),
            Err(e) => Err(CoreDaemonError::RestoreFailed(format!(
                "JS `restore` threw: {e}"
            ))),
        }
    }
}

// =========================================================================
// ReconstructionErrorBridge — loud-failure fallback for factory errors
// =========================================================================

/// Fallback `MeshDaemon` used when a migration-target factory
/// reconstruction fails (JS factory threw, TSFN timed out, channel
/// disconnected, local-spawn kind factory not yet implemented).
///
/// **Why loud, not silent.** The previous implementation
/// (`NoopBridge`) returned `Ok(vec![])` from `process`, which made
/// migrations appear to succeed on the target even though the
/// daemon's actual work was not running. Operators saw "migration
/// completed" alongside vanishing event throughput and no error.
/// This bridge instead:
///
/// - Returns `CoreDaemonError::RestoreFailed` from `restore`, so
///   the migration's restore phase fails visibly with a typed error
///   carrying the underlying reason.
/// - Returns `CoreDaemonError::ProcessFailed` from every `process`
///   call, so even stateless migrations (where `restore` is not
///   exercised) produce a visible failure on the first event.
///
/// The `reason` is plumbed from the specific `build_bridge_from_tsfn`
/// failure branch (enqueue / throw / timeout / disconnect) or the
/// local-spawn "not yet wired" placeholder, so diagnostic output is
/// precise about why reconstruction failed.
struct ReconstructionErrorBridge {
    name: String,
    reason: String,
}

impl ReconstructionErrorBridge {
    fn new(name: String, reason: impl Into<String>) -> Self {
        Self {
            name,
            reason: reason.into(),
        }
    }

    fn err_msg(&self, op: &str) -> String {
        format!(
            "reconstruction failed for daemon kind '{}' on {}: {}",
            self.name, op, self.reason,
        )
    }
}

impl MeshDaemon for ReconstructionErrorBridge {
    fn name(&self) -> &str {
        &self.name
    }

    fn requirements(&self) -> CapabilityFilter {
        CapabilityFilter::default()
    }

    fn process(
        &mut self,
        _event: &CausalEvent,
    ) -> std::result::Result<Vec<Bytes>, CoreDaemonError> {
        Err(CoreDaemonError::ProcessFailed(self.err_msg("process")))
    }

    fn restore(&mut self, _state: Bytes) -> std::result::Result<(), CoreDaemonError> {
        Err(CoreDaemonError::RestoreFailed(self.err_msg("restore")))
    }
}

// =========================================================================
// Bridge construction from a factory TSFN — sub-step 5
// =========================================================================

/// Invoke the JS factory TSFN synchronously and build an
/// `EventDispatchBridge` from the returned method triple. Used
/// both by `spawn`'s migration-target reconstruction path (via
/// the SDK kind_factory closure) and directly when the dispatcher
/// needs to rebuild a daemon after a cross-node migration.
///
/// Blocks the current thread (expected to be a tokio worker, not
/// the Node main thread) until the TSFN callback sends the
/// extracted TSFNs back over the mpsc channel.
///
/// Falls back to [`ReconstructionErrorBridge`] — **never**
/// `NoopBridge` — if:
/// - The TSFN enqueue fails (runtime shutting down)
/// - The JS factory throws
/// - The user's factory returned a Promise (async), which lacks
///   the expected `process` property
/// - The TSFN callback does not respond within the bounded wait
/// - The mpsc receiver drops
///
/// The bridge's `restore` / `process` return typed
/// `CoreDaemonError::{RestoreFailed, ProcessFailed}` carrying the
/// underlying reason, so a migration that reconstructs through a
/// broken factory fails visibly (either at the restore phase or
/// on the first event), rather than silently "succeeding" with a
/// daemon that eats every event it receives. The `eprintln!` trail
/// preserves the operator-visible breadcrumb.
fn build_bridge_from_tsfn(factory: Arc<FactoryTsfn>, kind: String) -> Box<dyn MeshDaemon> {
    let (tx, rx) = std::sync::mpsc::sync_channel::<Result<DaemonBridgeTsfns>>(1);
    let status = factory.call_with_return_value(
        (),
        napi::threadsafe_function::ThreadsafeFunctionCallMode::NonBlocking,
        move |ret: Result<DaemonBridgeTsfns>, _env| {
            let _ = tx.send(ret);
            Ok(())
        },
    );
    if status != napi::Status::Ok {
        let reason = format!("TSFN enqueue failed: {status:?}");
        eprintln!("build_bridge_from_tsfn: kind '{kind}': {reason}");
        return Box::new(ReconstructionErrorBridge::new(kind, reason));
    }
    // Bounded wait — same deadlock rationale as `EventDispatchBridge::process`.
    // If the Node main thread is wedged we want the migration to fail
    // loudly (via `ReconstructionErrorBridge`) rather than hang forever.
    let factory_timeout = Duration::from_millis(DEFAULT_CALLBACK_TIMEOUT_MS as u64);
    match rx.recv_timeout(factory_timeout) {
        Ok(Ok(tsfns)) => Box::new(EventDispatchBridge {
            name: kind,
            process: tsfns.process,
            snapshot: tsfns.snapshot,
            restore: tsfns.restore,
            callback_timeout: factory_timeout,
        }),
        Ok(Err(e)) => {
            let reason = format!("JS factory threw: {e}");
            eprintln!("build_bridge_from_tsfn: kind '{kind}': {reason}");
            Box::new(ReconstructionErrorBridge::new(kind, reason))
        }
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            let reason = format!(
                "TSFN factory did not respond within {} ms",
                factory_timeout.as_millis(),
            );
            eprintln!("build_bridge_from_tsfn: kind '{kind}': {reason}");
            Box::new(ReconstructionErrorBridge::new(kind, reason))
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            let reason = "TSFN channel disconnected".to_string();
            eprintln!("build_bridge_from_tsfn: kind '{kind}': {reason}");
            Box::new(ReconstructionErrorBridge::new(kind, reason))
        }
    }
}

// =========================================================================
// Tests
// =========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // Regression: migration-target reconstruction fallback must fail
    // loudly, never silently. Before this fix the NAPI layer installed
    // a `NoopBridge` whose `process` returned `Ok(vec![])`, so a
    // migration could "succeed" on a target where the JS factory
    // threw / returned a Promise / timed out — the daemon would then
    // silently swallow every event with no diagnostic. The replacement
    // `ReconstructionErrorBridge` returns typed `RestoreFailed` from
    // `restore` and typed `ProcessFailed` from `process`, each
    // carrying the underlying reason, so the migration fails visibly
    // at either the restore phase or the first event.
    //
    // Testing the bridge directly (without spinning up a real
    // migration) proves the contract at the bridge layer. The TS-side
    // integration test in `test/compute.test.ts` (existing
    // target-unavailable coverage) guards the end-to-end path.

    #[test]
    fn reconstruction_error_bridge_process_returns_typed_error() {
        let mut bridge = ReconstructionErrorBridge::new(
            "echo".to_string(),
            "JS factory threw: TypeError: process is not a function",
        );
        let event = CausalEvent {
            link: ::net::adapter::net::state::causal::CausalLink {
                origin_hash: 0xdead_beef,
                horizon_encoded: 0,
                sequence: 1,
                parent_hash: 0,
            },
            payload: bytes::Bytes::from_static(b"x"),
            received_at: 0,
        };

        let result = bridge.process(&event);
        match result {
            Err(CoreDaemonError::ProcessFailed(msg)) => {
                // Message must identify kind + the underlying reason,
                // so an operator staring at logs can diagnose without
                // cross-referencing source lines.
                assert!(msg.contains("echo"), "missing kind: {msg}");
                assert!(
                    msg.contains("TypeError: process is not a function"),
                    "missing underlying reason: {msg}",
                );
                assert!(msg.contains("process"), "missing op label: {msg}");
            }
            Err(other) => panic!("expected ProcessFailed, got {other:?}"),
            Ok(outputs) => panic!(
                "silent-noop regression: ReconstructionErrorBridge must never return Ok; got {} outputs",
                outputs.len(),
            ),
        }
    }

    #[test]
    fn reconstruction_error_bridge_restore_returns_typed_error() {
        let mut bridge = ReconstructionErrorBridge::new(
            "counter".to_string(),
            "TSFN factory did not respond within 60000 ms",
        );
        let state = bytes::Bytes::from_static(&[0u8; 16]);

        let result = bridge.restore(state);
        match result {
            Err(CoreDaemonError::RestoreFailed(msg)) => {
                assert!(msg.contains("counter"), "missing kind: {msg}");
                assert!(
                    msg.contains("did not respond within 60000 ms"),
                    "missing underlying reason: {msg}",
                );
                assert!(msg.contains("restore"), "missing op label: {msg}");
            }
            Err(other) => panic!("expected RestoreFailed, got {other:?}"),
            Ok(()) => panic!(
                "silent-noop regression: ReconstructionErrorBridge::restore must never return Ok",
            ),
        }
    }
}
