//! `net-aggregator-daemon` — long-running process that hosts
//! the Net-mesh substrate's [`AggregatorRegistry`] and one or
//! more aggregator groups loaded from a TOML config file.
//!
//! Slice 8 of `docs/plans/AGGREGATOR_LIFECYCLE_DEFERRED_2026_05_23.md`.
//! Closes the AL-6 "needs daemon process" gap: the substrate
//! primitives (`AggregatorRegistry`, `LifecycleGroup`,
//! `HealthMonitor`, `aggregator.registry` RPC) are already in
//! place; this binary is the operator-facing surface that boots
//! them together.
//!
//! # CLI shape
//!
//! ```text
//! net-aggregator-daemon --config /etc/net/aggregator.toml [--listen ADDR] [--peer ADDR]…
//! ```
//!
//! # Config shape
//!
//! See the crate's internal `Config` / `GroupConfig` types
//! (`src/lib.rs`) for the full schema. Minimum
//! example:
//!
//! ```toml
//! listen = "0.0.0.0:7400"
//! psk_hex = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff"
//!
//! [[group]]
//! name = "primary"
//! source_subnet = "3.7"
//! fold_kinds = [0x0001]
//! replica_count = 3
//! summary_interval_ms = 1000
//! ```
//!
//! # Lifecycle
//!
//! 1. Parse CLI + config.
//! 2. Boot `MeshNode`, install `AggregatorRegistry`, expose
//!    `aggregator.registry` RPC via `install_registry_service`.
//! 3. For each `[[group]]` section, spawn a `LifecycleGroup`
//!    and register it under the operator-chosen name.
//! 4. Block on SIGINT (Ctrl-C). On signal: drain the registry
//!    (stop every group, await teardown) then exit.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use serde::Deserialize;

use net::adapter::net::behavior::aggregator::{
    snapshot_group, AggregatorConfig, AggregatorDaemon, AggregatorRegistry, RegistryRpcError,
    SpawnFn,
};
use net::adapter::net::behavior::fold::capability::CapabilityFold;
use net::adapter::net::behavior::fold::reservation::ReservationFold;
use net::adapter::net::behavior::fold::FoldKind;
use net::adapter::net::behavior::lifecycle::LifecycleGroup;
use net::adapter::net::identity::EntityKeypair;
use net::adapter::net::{MeshNode, MeshNodeConfig, SubnetId};

/// Argv shape. Exposed publicly so the binary entry point in
/// `main.rs` can call `Cli::parse()` against it.
#[derive(Parser, Debug)]
#[command(
    name = "net-aggregator-daemon",
    version,
    about = "Long-running net-mesh aggregator host. Boots a MeshNode, installs an AggregatorRegistry, and spawns aggregator groups from a TOML config."
)]
pub struct Cli {
    /// Path to the TOML config file.
    #[arg(long, short, env = "NET_AGGREGATOR_CONFIG")]
    pub config: PathBuf,
    /// Override the config's `listen` address (e.g.
    /// `0.0.0.0:7400`). Useful when one config file is shared
    /// across nodes that need distinct ports.
    #[arg(long)]
    pub listen: Option<String>,
    /// Print a single JSON line to stdout with the bound
    /// `(node_id, bound_addr, public_key_hex)` triple *before*
    /// entering the signal-wait loop. Binding integration
    /// tests (Node / Python / Go) parse this line to drive
    /// their handshake against the daemon without grepping
    /// tracing output. Has no effect on the daemon's behavior
    /// otherwise.
    #[arg(long, default_value_t = false)]
    pub print_bootstrap: bool,
    /// Increase log verbosity. `-v` = info (default), `-vv` =
    /// debug, `-vvv` = trace.
    #[arg(long, short, action = clap::ArgAction::Count)]
    pub verbose: u8,
}

/// Top-level TOML config shape.
#[derive(Deserialize, Debug, Clone)]
struct Config {
    /// UDP listen address (e.g. `"0.0.0.0:7400"` or
    /// `"127.0.0.1:0"` for ephemeral-port tests).
    listen: String,
    /// 64-char hex pre-shared key (32 bytes) the rest of the
    /// mesh uses for handshake encryption.
    psk_hex: String,
    /// Aggregator groups to spawn at startup. Order is preserved
    /// — registry duplicates fail-fast, so operators see
    /// duplicate-name errors immediately on startup rather than
    /// at first RPC.
    #[serde(default, rename = "group")]
    groups: Vec<GroupConfig>,
    /// Aggregator templates — referenced by name via the
    /// `aggregator.registry` `Spawn` RPC. Operators preregister
    /// the legal shapes here; remote callers can only deploy
    /// groups matching a configured template, keeping the trust
    /// boundary at the operator's config file.
    #[serde(default, rename = "template")]
    templates: Vec<TemplateConfig>,
}

/// Per-group config section spawned at startup. Carries the
/// operator-chosen group name + the aggregator's per-replica
/// config.
#[derive(Deserialize, Debug, Clone)]
struct GroupConfig {
    /// Operator-chosen name (the registry key). Must be unique
    /// within the daemon process.
    name: String,
    /// Dotted-notation `SubnetId` (e.g. `"3.7"`) — the subnet
    /// this aggregator summarizes detail from.
    source_subnet: String,
    /// `FoldKind::KIND_ID`s to aggregate. Accepts decimal or
    /// `0x`-prefixed hex via [`u16`] deserialization.
    fold_kinds: Vec<u16>,
    /// Number of replicas. `1..=255`.
    replica_count: u8,
    /// Summary interval in milliseconds. `>= 10`.
    summary_interval_ms: u64,
    /// Optional 64-char hex group seed. When absent, derived
    /// deterministically from the group name.
    group_seed: Option<String>,
}

/// Per-template config section. Lookup key for `Spawn` RPC.
/// Same per-replica shape as `[[group]]` but the operator
/// supplies `group_name` + `replica_count` at spawn time
/// rather than in the file.
#[derive(Deserialize, Debug, Clone)]
struct TemplateConfig {
    /// Template name — the `Spawn` RPC's `template_name`
    /// resolves against this. Must be unique within the daemon's
    /// template registry.
    name: String,
    /// Dotted-notation `SubnetId` for the spawned group.
    source_subnet: String,
    /// `FoldKind::KIND_ID`s the template aggregates.
    fold_kinds: Vec<u16>,
    /// Summary interval in milliseconds. `>= 10`.
    summary_interval_ms: u64,
}

/// Daemon startup errors. Cover config parsing, MeshNode boot,
/// and group registration in one typed surface so the binary's
/// `main` exit code maps cleanly.
#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    #[error("read config {path:?}: {error}")]
    ConfigRead {
        path: PathBuf,
        error: std::io::Error,
    },
    #[error("parse config: {0}")]
    ConfigParse(toml::de::Error),
    #[error("psk_hex must decode to 32 bytes: {0}")]
    PskInvalid(String),
    #[error("listen address {addr:?} is not a valid SocketAddr: {error}")]
    ListenAddrInvalid {
        addr: String,
        error: std::net::AddrParseError,
    },
    #[error("subnet identifier {raw:?}: {error}")]
    SubnetInvalid { raw: String, error: String },
    #[error("group seed for {name:?} must be 64 hex chars: {error}")]
    GroupSeedInvalid { name: String, error: String },
    #[error("mesh: {0}")]
    Mesh(String),
    #[error("aggregator config for {name:?}: {error}")]
    AggregatorConfig { name: String, error: String },
    #[error("registry: {0}")]
    Registry(String),
    #[error("serve: {0}")]
    Serve(String),
}

/// Configure the tracing subscriber for the daemon's stderr
/// output. `verbose == 0` → info, `1` → debug, `2+` → trace.
/// `RUST_LOG` env var overrides this if set.
pub fn init_tracing(verbose: u8) {
    use tracing_subscriber::EnvFilter;
    let level = match verbose {
        0 => "info",
        1 => "debug",
        _ => "trace",
    };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}

/// Boot from `cli`, install signal handlers, run until SIGINT
/// (or SIGTERM on Unix), then drain the registry and exit.
/// This is the binary's `main` body.
pub async fn run(cli: Cli) -> Result<(), DaemonError> {
    let print_bootstrap = cli.print_bootstrap;
    let booted = boot(cli).await?;
    let registry = booted.registry.clone();
    // `boot()` deliberately doesn't `start()` the mesh —
    // integration tests need a window between boot and start
    // to perform handshakes. The production path starts
    // immediately.
    booted.mesh.start();
    if print_bootstrap {
        print_bootstrap_line(&booted);
    }
    // Hold `booted` until shutdown so the ServeHandle's Drop
    // fires after we've stopped the groups.
    wait_for_shutdown().await;
    tracing::info!("shutdown signal received; draining registry");
    drain_registry(&registry).await;
    drop(booted);
    tracing::info!("aggregator daemon stopped cleanly");
    Ok(())
}

/// Print a single-line JSON object describing the daemon's
/// bootstrap state to stdout. Format is locked across SDK
/// bindings — see `SDK_AGGREGATOR_SUBNET_PLAN.md` Stage 6:
///
/// ```text
/// {"node_id":12345,"bound_addr":"127.0.0.1:54321","public_key_hex":"<64 hex>"}
/// ```
///
/// `node_id` is a JSON number; `bound_addr` is a JSON string
/// (always IP:port — no special escaping); `public_key_hex` is
/// 64 lowercase hex chars (no escaping). One line, terminated
/// by `\n`, flushed.
fn print_bootstrap_line(booted: &BootedDaemon) {
    let line = serde_json::json!({
        "node_id": booted.mesh.node_id(),
        "bound_addr": booted.bound_addr.to_string(),
        "public_key_hex": hex::encode(booted.public_key),
    });
    println!("{line}");
    // Force a flush so subprocess test fixtures see the line
    // before the daemon enters wait_for_shutdown (where stdout
    // is otherwise quiet for the program's lifetime).
    use std::io::Write as _;
    let _ = std::io::stdout().flush();
}

/// Boot the MeshNode + registry + groups described by `cli`,
/// returning the live handles. Used by both `run()` (production
/// path: hold until shutdown signal) and integration tests
/// (assert against the booted state in-process).
pub struct BootedDaemon {
    pub mesh: Arc<MeshNode>,
    pub registry: Arc<AggregatorRegistry>,
    /// Held to keep the registry RPC service registration alive.
    /// Dropping un-installs the service.
    pub _serve: net::adapter::net::mesh_rpc::ServeHandle,
    /// Listen address the mesh bound to (post-`MeshNode::new`
    /// resolution — ephemeral `:0` ports surface as the
    /// concrete bound port here).
    pub bound_addr: std::net::SocketAddr,
    /// Public key the listening node accepts handshakes against.
    /// Test fixtures pass this to `MeshNode::connect`.
    pub public_key: [u8; 32],
}

/// Boot the registry + groups without entering the
/// signal-wait loop. Returns the live handles for tests +
/// embedders that need to drive their own shutdown.
pub async fn boot(cli: Cli) -> Result<BootedDaemon, DaemonError> {
    // Parse config.
    let raw =
        tokio::fs::read_to_string(&cli.config)
            .await
            .map_err(|e| DaemonError::ConfigRead {
                path: cli.config.clone(),
                error: e,
            })?;
    let config: Config = toml::from_str(&raw).map_err(DaemonError::ConfigParse)?;

    // CLI listen override.
    let listen = cli.listen.unwrap_or(config.listen.clone());
    let listen_addr: std::net::SocketAddr =
        listen.parse().map_err(|e| DaemonError::ListenAddrInvalid {
            addr: listen.clone(),
            error: e,
        })?;
    let psk = decode_psk(&config.psk_hex)?;

    // Boot the MeshNode, install the registry, expose the RPC
    // service. Order matters: `set_aggregator_registry`
    // requires `&mut MeshNode`, so install before wrapping in
    // Arc.
    let mut mesh_node = MeshNode::new(
        EntityKeypair::generate(),
        MeshNodeConfig::new(listen_addr, psk),
    )
    .await
    .map_err(|e| DaemonError::Mesh(format!("{e:?}")))?;
    let registry = Arc::new(AggregatorRegistry::new());
    mesh_node.set_aggregator_registry(registry.clone());
    let mesh = Arc::new(mesh_node);

    // Build the SpawnFn + ScaleFn from the operator's template
    // registry, then install the registry service with both
    // handlers. Templates and the mesh handle are captured by
    // each closure for the daemon's lifetime.
    let spawner = make_spawner(config.templates.clone(), registry.clone(), mesh.clone());
    let scaler = make_scaler(config.templates.clone(), registry.clone(), mesh.clone());
    let serve = registry
        .install_registry_service_with_handlers(&mesh, spawner, scaler)
        .map_err(|e| DaemonError::Serve(format!("{e:?}")))?;

    let bound_addr = mesh.local_addr();
    let public_key = *mesh.public_key();

    tracing::info!(
        listen = %bound_addr,
        node_id = mesh.node_id(),
        groups = config.groups.len(),
        templates = config.templates.len(),
        "aggregator daemon booted",
    );

    // Validate templates eagerly — including a dry
    // `AggregatorDaemon::new` against the resolved config — so
    // operator typos surface at boot time, not on the first
    // Spawn RPC. This is what makes the
    // `expect("aggregator config validated")` chains in the
    // per-replica factory closures actually safe: anything
    // `AggregatorDaemon::new` would reject we've already
    // rejected at boot.
    for tpl in &config.templates {
        validate_template(tpl, &mesh)?;
    }

    // Spawn every configured group in parallel — `spawn_group`
    // already parallelizes its replicas via `join_all` (see
    // `LifecycleGroup::spawn`), but groups themselves were
    // sequential. Boot of N groups of M replicas now takes
    // ~max(per-replica start latency) instead of N × max.
    let spawn_futures = config.groups.iter().map(|group_cfg| {
        let registry = registry.clone();
        let mesh = mesh.clone();
        async move {
            let res = spawn_group(&registry, &mesh, group_cfg).await;
            (group_cfg.name.clone(), res)
        }
    });
    let results = futures::future::join_all(spawn_futures).await;
    for (name, res) in results {
        res?;
        tracing::info!(name, "group spawned + registered");
    }

    // NOTE: the mesh's receive loop is NOT started here.
    // Callers (`run()`, integration tests) call
    // `booted.mesh.start()` themselves — tests want a window
    // between boot + start to perform peer handshakes.

    Ok(BootedDaemon {
        mesh,
        registry,
        _serve: serve,
        bound_addr,
        public_key,
    })
}

/// Internal: resolved + validated per-replica spec. The two
/// caller paths — static `[[group]]` spawn at boot and the
/// `Spawn` RPC's template-based dynamic deployment — funnel
/// through this shape so [`spawn_and_register`] runs the same
/// LifecycleGroup::spawn + register_with_monitor flow for
/// both.
#[derive(Debug, Clone)]
struct AggregatorSpec {
    name: String,
    source_subnet: SubnetId,
    fold_kinds: Vec<u16>,
    replica_count: u8,
    summary_interval_ms: u64,
    group_seed: [u8; 32],
}

impl AggregatorSpec {
    /// Resolve a static `[[group]]` config into a spec, doing
    /// the field-wise validation (subnet parse, replica_count,
    /// interval floor, fold_kinds).
    fn from_group(group_cfg: &GroupConfig) -> Result<Self, DaemonError> {
        if group_cfg.replica_count == 0 {
            return Err(DaemonError::AggregatorConfig {
                name: group_cfg.name.clone(),
                error: "replica_count must be > 0".into(),
            });
        }
        if group_cfg.summary_interval_ms < 10 {
            return Err(DaemonError::AggregatorConfig {
                name: group_cfg.name.clone(),
                error: "summary_interval_ms must be >= 10".into(),
            });
        }
        let source_subnet =
            parse_subnet(&group_cfg.source_subnet).map_err(|e| DaemonError::SubnetInvalid {
                raw: group_cfg.source_subnet.clone(),
                error: e,
            })?;
        for kind in &group_cfg.fold_kinds {
            check_known_fold_kind(*kind, &group_cfg.name)?;
        }
        let group_seed = match &group_cfg.group_seed {
            Some(s) => decode_seed(s).map_err(|e| DaemonError::GroupSeedInvalid {
                name: group_cfg.name.clone(),
                error: e,
            })?,
            None => derive_seed_from_name(&group_cfg.name),
        };
        Ok(Self {
            name: group_cfg.name.clone(),
            source_subnet,
            fold_kinds: group_cfg.fold_kinds.clone(),
            replica_count: group_cfg.replica_count,
            summary_interval_ms: group_cfg.summary_interval_ms,
            group_seed,
        })
    }

    /// Build a spec from a `Spawn` RPC request + the matching
    /// template. Returns RegistryRpcError shapes since the
    /// caller is the spawner closure surfaced over the wire.
    fn from_template(
        template: &TemplateConfig,
        group_name: String,
        replica_count: u8,
    ) -> Result<Self, RegistryRpcError> {
        if replica_count == 0 {
            return Err(RegistryRpcError::SpawnRejected(
                "replica_count must be > 0".into(),
            ));
        }
        let source_subnet = parse_subnet(&template.source_subnet)
            .map_err(|e| RegistryRpcError::SpawnRejected(format!("source_subnet: {e}")))?;
        Ok(Self {
            group_seed: derive_seed_from_name(&group_name),
            name: group_name,
            source_subnet,
            fold_kinds: template.fold_kinds.clone(),
            replica_count,
            summary_interval_ms: template.summary_interval_ms,
        })
    }

    /// Build the `AggregatorConfig` used per-replica.
    fn aggregator_config(&self) -> AggregatorConfig {
        let mut cfg = AggregatorConfig::new(self.source_subnet)
            .with_interval(Duration::from_millis(self.summary_interval_ms));
        for kind in &self.fold_kinds {
            cfg = cfg.with_fold_kind(*kind);
        }
        cfg
    }
}

/// Single fold-kind check used by both `validate_template` and
/// `AggregatorSpec::from_group`. Returns a typed
/// `DaemonError::AggregatorConfig` so the error path is
/// identical at the source.
fn check_known_fold_kind(kind: u16, name: &str) -> Result<(), DaemonError> {
    match kind {
        k if k == CapabilityFold::KIND_ID => Ok(()),
        k if k == ReservationFold::KIND_ID => Ok(()),
        other => Err(DaemonError::AggregatorConfig {
            name: name.to_string(),
            error: format!(
                "unknown fold_kind 0x{other:04x}; built-in summarizers cover {:#06x} (capability) and {:#06x} (reservation)",
                CapabilityFold::KIND_ID,
                ReservationFold::KIND_ID,
            ),
        }),
    }
}

/// Shared lifecycle-spawn + register-with-monitor path used by
/// both the static `[[group]]` boot loop and the Spawn RPC's
/// dynamic deployment. The monitor factory closure rebuilds an
/// `AggregatorDaemon` from the same config the initial spawn
/// used — auto-respawn on unhealthy preserves identity (same
/// `group_seed`) but the daemon instance itself is fresh.
async fn spawn_and_register(
    spec: &AggregatorSpec,
    registry: &Arc<AggregatorRegistry>,
    mesh: &Arc<MeshNode>,
) -> Result<Arc<net::adapter::net::behavior::aggregator::AggregatorGroupEntry>, SpawnAndRegisterError>
{
    let cfg = spec.aggregator_config();
    let group = LifecycleGroup::<AggregatorDaemon>::spawn(spec.replica_count, spec.group_seed, {
        let cfg = cfg.clone();
        let mesh = mesh.clone();
        move |_idx| {
            #[allow(clippy::expect_used)]
            Arc::new(
                AggregatorDaemon::new(cfg.clone(), mesh.clone())
                    .expect("aggregator config validated by AggregatorSpec resolution"),
            )
        }
    })
    .await
    .map_err(|e| SpawnAndRegisterError::SpawnFailed(format!("{e}")))?;
    let monitor_factory = {
        let cfg = cfg.clone();
        let mesh = mesh.clone();
        move |_idx: u8| -> Arc<AggregatorDaemon> {
            #[allow(clippy::expect_used)]
            Arc::new(
                AggregatorDaemon::new(cfg.clone(), mesh.clone())
                    .expect("aggregator config validated by AggregatorSpec resolution"),
            )
        }
    };
    let monitor_interval = Duration::from_millis(spec.summary_interval_ms.saturating_mul(4));
    registry
        .register_with_monitor(spec.name.clone(), group, monitor_factory, monitor_interval)
        .map_err(|e| SpawnAndRegisterError::RegisterFailed(format!("{e}")))
}

/// Internal error from [`spawn_and_register`]. Each variant
/// maps trivially to either `DaemonError` (static path) or
/// `RegistryRpcError` (RPC path) at the caller.
#[derive(Debug)]
enum SpawnAndRegisterError {
    SpawnFailed(String),
    RegisterFailed(String),
}

async fn spawn_group(
    registry: &Arc<AggregatorRegistry>,
    mesh: &Arc<MeshNode>,
    group_cfg: &GroupConfig,
) -> Result<(), DaemonError> {
    let spec = AggregatorSpec::from_group(group_cfg)?;
    spawn_and_register(&spec, registry, mesh)
        .await
        .map_err(|e| match e {
            SpawnAndRegisterError::SpawnFailed(s) => DaemonError::AggregatorConfig {
                name: spec.name.clone(),
                error: s,
            },
            SpawnAndRegisterError::RegisterFailed(s) => DaemonError::Registry(s),
        })?;
    Ok(())
}

/// Validate a `[[template]]` block at boot time so operator
/// typos surface immediately, not on first Spawn RPC.
///
/// Field-level checks (subnet parse, interval floor, known
/// fold_kinds) plus a dry `AggregatorDaemon::new` against the
/// resolved config. The dry-new is what guarantees the
/// `expect("aggregator config validated")` chains in the
/// per-replica factory closures are safe: anything `new` would
/// reject we've already rejected here.
fn validate_template(tpl: &TemplateConfig, mesh: &Arc<MeshNode>) -> Result<(), DaemonError> {
    if tpl.summary_interval_ms < 10 {
        return Err(DaemonError::AggregatorConfig {
            name: tpl.name.clone(),
            error: "summary_interval_ms must be >= 10".into(),
        });
    }
    let source_subnet =
        parse_subnet(&tpl.source_subnet).map_err(|e| DaemonError::SubnetInvalid {
            raw: tpl.source_subnet.clone(),
            error: e,
        })?;
    for kind in &tpl.fold_kinds {
        check_known_fold_kind(*kind, &tpl.name)?;
    }
    // Dry-build the AggregatorConfig + AggregatorDaemon::new to
    // catch anything the field-wise checks miss. The throwaway
    // daemon is dropped before returning; `new` doesn't mutate
    // the mesh.
    let mut agg_cfg = AggregatorConfig::new(source_subnet)
        .with_interval(Duration::from_millis(tpl.summary_interval_ms));
    for kind in &tpl.fold_kinds {
        agg_cfg = agg_cfg.with_fold_kind(*kind);
    }
    drop(AggregatorDaemon::new(agg_cfg, mesh.clone()).map_err(|e| {
        DaemonError::AggregatorConfig {
            name: tpl.name.clone(),
            error: format!("{e}"),
        }
    })?);
    Ok(())
}

/// Build a [`SpawnFn`] backed by `templates`. The closure
/// captures `registry` + `mesh` so it can resolve names, build
/// daemons, and register the spawned group all in one place.
fn make_spawner(
    templates: Vec<TemplateConfig>,
    registry: Arc<AggregatorRegistry>,
    mesh: Arc<MeshNode>,
) -> SpawnFn {
    use std::collections::HashMap;
    // Index by template name for O(1) lookup. Cloning is fine
    // — templates are small and the operator's config is
    // immutable for the daemon's lifetime.
    let by_name: HashMap<String, TemplateConfig> =
        templates.into_iter().map(|t| (t.name.clone(), t)).collect();
    let by_name = Arc::new(by_name);
    Box::new(move |req| {
        let registry = registry.clone();
        let mesh = mesh.clone();
        let by_name = by_name.clone();
        Box::pin(async move {
            let template = by_name
                .get(&req.template_name)
                .cloned()
                .ok_or_else(|| RegistryRpcError::UnknownTemplate(req.template_name.clone()))?;
            let spec = AggregatorSpec::from_template(
                &template,
                req.group_name.clone(),
                req.replica_count,
            )?;
            let entry = spawn_and_register(&spec, &registry, &mesh)
                .await
                .map_err(|e| match e {
                    SpawnAndRegisterError::SpawnFailed(s)
                    | SpawnAndRegisterError::RegisterFailed(s) => {
                        RegistryRpcError::SpawnRejected(s)
                    }
                })?;
            Ok(snapshot_group(&entry).await)
        })
    })
}

/// Build a [`ScaleFn`] that resolves the supplied template,
/// verifies it matches the existing group's `source_subnet` +
/// `fold_kinds`, and invokes [`AggregatorRegistry::scale_group`]
/// with a factory that constructs fresh `AggregatorDaemon`s
/// from the same spec. The factory closure mirrors the one in
/// [`spawn_and_register`] so grow-added replicas use the exact
/// per-replica config the original spawn used — preserving
/// identity continuity (group_seed-derived) for the lifetime
/// of the group.
fn make_scaler(
    templates: Vec<TemplateConfig>,
    registry: Arc<AggregatorRegistry>,
    mesh: Arc<MeshNode>,
) -> net::adapter::net::behavior::aggregator::ScaleFn {
    use std::collections::HashMap;
    let by_name: HashMap<String, TemplateConfig> =
        templates.into_iter().map(|t| (t.name.clone(), t)).collect();
    let by_name = Arc::new(by_name);
    Box::new(move |req| {
        let registry = registry.clone();
        let mesh = mesh.clone();
        let by_name = by_name.clone();
        Box::pin(async move {
            let template = by_name
                .get(&req.template_name)
                .cloned()
                .ok_or_else(|| RegistryRpcError::UnknownTemplate(req.template_name.clone()))?;
            // Build the spec from the template + supplied group
            // name. `replica_count` here is the *target* — used
            // only to materialize the spec for factory closure
            // construction; the actual grow/shrink delta lives
            // in scale_group.
            let spec = AggregatorSpec::from_template(
                &template,
                req.group_name.clone(),
                req.target_replica_count,
            )
            .map_err(|e| match e {
                RegistryRpcError::SpawnRejected(d) => RegistryRpcError::ScaleRejected(d),
                other => other,
            })?;
            let existing_entry = registry
                .get(&req.group_name)
                .ok_or_else(|| RegistryRpcError::UnknownGroup(req.group_name.clone()))?;
            // Fast path: target == current. The replica_count
            // accessor reads through a brief lock without
            // allocating a per-replica snapshot or polling each
            // replica's health() — skip the (potentially
            // expensive) validation + scale_group invocation and
            // return the current snapshot directly.
            if req.target_replica_count as usize == existing_entry.replica_count().await {
                return Ok(snapshot_group(&existing_entry).await);
            }
            // Validate the resolved spec against the existing
            // group's live config. Read from the first replica's
            // AggregatorConfig — every replica shares the same
            // spec, so any one is representative.
            {
                let snap = existing_entry.snapshot().await;
                if let Some(replica) = snap.replicas.first() {
                    let cfg = replica.config();
                    if cfg.source_subnet != spec.source_subnet {
                        return Err(RegistryRpcError::ScaleRejected(format!(
                            "template `{}` source_subnet {} does not match \
                             existing group's source_subnet {}",
                            req.template_name, spec.source_subnet, cfg.source_subnet
                        )));
                    }
                    if cfg.fold_kinds != spec.fold_kinds {
                        return Err(RegistryRpcError::ScaleRejected(format!(
                            "template `{}` fold_kinds differ from existing \
                             group's fold_kinds",
                            req.template_name
                        )));
                    }
                }
            }
            let aggregator_cfg = spec.aggregator_config();
            let factory_mesh = mesh.clone();
            let factory_cfg = aggregator_cfg.clone();
            let factory = move |_idx: u8| -> Arc<AggregatorDaemon> {
                #[allow(clippy::expect_used)]
                Arc::new(
                    AggregatorDaemon::new(factory_cfg.clone(), factory_mesh.clone())
                        .expect("aggregator config validated by AggregatorSpec resolution"),
                )
            };
            let entry = registry
                .scale_group(&req.group_name, req.target_replica_count, factory)
                .await
                .map_err(|e| match e {
                    net::adapter::net::behavior::aggregator::AggregatorRegistryError::NotFound(g) => {
                        RegistryRpcError::UnknownGroup(g)
                    }
                    net::adapter::net::behavior::aggregator::AggregatorRegistryError::ScaleFailed(d) => {
                        RegistryRpcError::ScaleRejected(d)
                    }
                    other => RegistryRpcError::ScaleRejected(format!("{other}")),
                })?;
            Ok(snapshot_group(&entry).await)
        })
    })
}

async fn wait_for_shutdown() {
    // SIGINT (Ctrl-C) and (on Unix) SIGTERM both trigger a
    // clean drain. Windows only fires SIGINT.
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sigterm = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "SIGTERM handler install failed; relying on SIGINT only");
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = sigterm.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// Stop every group in the registry, awaiting each `stop()`
/// before continuing. Used by `run()`'s shutdown path and
/// available for tests that want to drive a clean teardown.
pub async fn drain_registry(registry: &Arc<AggregatorRegistry>) {
    for name in registry.names() {
        match registry.unregister(&name).await {
            Ok(group) => {
                tracing::debug!(name = %name, "stopping group");
                group.stop().await;
            }
            Err(e) => {
                tracing::warn!(name = %name, error = %e, "unregister failed during drain");
            }
        }
    }
}

/// Decode a hex string into 32 bytes. Accepts an optional
/// `0x` prefix. Used by both `psk_hex` and `group_seed`
/// parsing — same shape, same error message.
fn decode_hex_32(s: &str) -> Result<[u8; 32], String> {
    let trimmed = s.trim_start_matches("0x");
    let bytes = hex::decode(trimmed).map_err(|e| format!("{e}"))?;
    bytes
        .try_into()
        .map_err(|v: Vec<u8>| format!("expected 32 bytes, got {}", v.len()))
}

fn decode_psk(s: &str) -> Result<[u8; 32], DaemonError> {
    decode_hex_32(s).map_err(DaemonError::PskInvalid)
}

fn decode_seed(s: &str) -> Result<[u8; 32], String> {
    decode_hex_32(s)
}

/// Derive a deterministic 32-byte seed from a group name via
/// `blake3(label || name)`. The label is repo-pinned so the
/// derivation is stable across:
///
/// - Rust releases — `DefaultHasher` (the prior implementation)
///   is explicitly not stable across releases; an operator
///   upgrading the daemon binary would silently get different
///   derived seeds → different replica identities → fold-state
///   churn on upgrade. The bug-class this prevents.
/// - Daemon binary patch releases — the label string never
///   changes; bumping it constitutes a deliberate identity
///   migration that operators must opt into.
fn derive_seed_from_name(name: &str) -> [u8; 32] {
    const LABEL: &[u8] = b"net-aggregator-daemon-seed-v1";
    let mut hasher = blake3::Hasher::new();
    hasher.update(LABEL);
    // Domain-separate the label from the name to defeat
    // length-extension corner cases (LABEL || name vs
    // LABEL + suffix || name_without_suffix).
    hasher.update(&[0u8]);
    hasher.update(name.as_bytes());
    *hasher.finalize().as_bytes()
}

/// Parse a subnet identifier via the substrate's
/// `SubnetId::FromStr`. Accepts dotted notation (e.g.
/// `"3.7"`, `"1.2.3.4"`) and the literal `"global"`.
fn parse_subnet(raw: &str) -> Result<SubnetId, String> {
    raw.trim().parse::<SubnetId>().map_err(|e| format!("{e:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    // `parse_subnet` is now a thin shim over `SubnetId::FromStr`
    // (which has its own tests under
    // `adapter::net::subnet::id::tests`). No daemon-local
    // duplicates here.

    #[test]
    fn decode_psk_accepts_64_char_hex() {
        let psk = decode_psk("00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff")
            .expect("decode");
        assert_eq!(psk[0], 0x00);
        assert_eq!(psk[31], 0xff);
    }

    #[test]
    fn decode_psk_rejects_wrong_length() {
        assert!(decode_psk("0011").is_err());
    }

    #[test]
    fn derive_seed_from_name_is_deterministic_and_per_name() {
        let s1 = derive_seed_from_name("alpha");
        let s2 = derive_seed_from_name("alpha");
        let s3 = derive_seed_from_name("beta");
        assert_eq!(s1, s2);
        assert_ne!(s1, s3);
    }

    #[test]
    fn config_parses_minimum_viable_toml() {
        let raw = r#"
            listen = "127.0.0.1:0"
            psk_hex = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff"

            [[group]]
            name = "primary"
            source_subnet = "3.7"
            fold_kinds = [1]
            replica_count = 2
            summary_interval_ms = 50
        "#;
        let cfg: Config = toml::from_str(raw).expect("parse");
        assert_eq!(cfg.listen, "127.0.0.1:0");
        assert_eq!(cfg.groups.len(), 1);
        let g = &cfg.groups[0];
        assert_eq!(g.name, "primary");
        assert_eq!(g.replica_count, 2);
        assert_eq!(g.fold_kinds, vec![1]);
        assert!(cfg.templates.is_empty());
    }

    #[test]
    fn config_parses_templates_alongside_groups() {
        let raw = r#"
            listen = "127.0.0.1:0"
            psk_hex = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff"

            [[group]]
            name = "primary"
            source_subnet = "3.7"
            fold_kinds = [1]
            replica_count = 2
            summary_interval_ms = 50

            [[template]]
            name = "scale-out"
            source_subnet = "3.8"
            fold_kinds = [1]
            summary_interval_ms = 100
        "#;
        let cfg: Config = toml::from_str(raw).expect("parse");
        assert_eq!(cfg.groups.len(), 1);
        assert_eq!(cfg.templates.len(), 1);
        let t = &cfg.templates[0];
        assert_eq!(t.name, "scale-out");
        assert_eq!(t.source_subnet, "3.8");
        assert_eq!(t.fold_kinds, vec![1]);
        assert_eq!(t.summary_interval_ms, 100);
    }

    async fn test_mesh() -> Arc<MeshNode> {
        let addr: std::net::SocketAddr = "127.0.0.1:0".parse().unwrap();
        let cfg = MeshNodeConfig::new(addr, [0u8; 32]);
        Arc::new(
            MeshNode::new(EntityKeypair::generate(), cfg)
                .await
                .expect("MeshNode::new"),
        )
    }

    #[tokio::test]
    async fn validate_template_rejects_unknown_fold_kind() {
        let tpl = TemplateConfig {
            name: "t".into(),
            source_subnet: "3.7".into(),
            fold_kinds: vec![0xDEAD],
            summary_interval_ms: 50,
        };
        let mesh = test_mesh().await;
        match validate_template(&tpl, &mesh) {
            Err(DaemonError::AggregatorConfig { name, error }) => {
                assert_eq!(name, "t");
                assert!(error.contains("unknown fold_kind"), "msg was: {error}");
            }
            other => panic!("expected AggregatorConfig, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn validate_template_rejects_short_interval() {
        let tpl = TemplateConfig {
            name: "t".into(),
            source_subnet: "3.7".into(),
            fold_kinds: vec![1],
            summary_interval_ms: 5,
        };
        let mesh = test_mesh().await;
        match validate_template(&tpl, &mesh) {
            Err(DaemonError::AggregatorConfig { name, error }) => {
                assert_eq!(name, "t");
                assert!(error.contains("interval"), "msg was: {error}");
            }
            other => panic!("expected AggregatorConfig, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn validate_template_dry_new_catches_empty_fold_kinds() {
        // No fold_kinds → `AggregatorDaemon::new` returns
        // `NoFoldKinds`. The dry-new path is what catches this;
        // the field-wise checks above don't.
        let tpl = TemplateConfig {
            name: "empty".into(),
            source_subnet: "3.7".into(),
            fold_kinds: vec![],
            summary_interval_ms: 50,
        };
        let mesh = test_mesh().await;
        match validate_template(&tpl, &mesh) {
            Err(DaemonError::AggregatorConfig { name, error }) => {
                assert_eq!(name, "empty");
                assert!(error.contains("fold_kinds"), "msg was: {error}");
            }
            other => panic!("expected AggregatorConfig, got {other:?}"),
        }
    }
}
