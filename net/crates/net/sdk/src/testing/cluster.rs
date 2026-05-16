//! In-process multi-node `(Mesh, MeshOsRuntime)` harness.
//!
//! Boots N nodes on `127.0.0.1:<ephemeral>` ports, peers every
//! pair via real UDP handshake, and installs bridge probes
//! ([`super::probes::install_mesh_probes`]) so the MeshOS
//! snapshot fold reflects peer state. Returns a typed
//! [`ClusterHarness`] handle whose [`ClusterNode`] entries expose
//! both the network handle and the daemon-SDK wrapper for each
//! node.
//!
//! See `crates/net/docs/plans/DECK_DEMO_HARNESS_PLAN.md` Phase 0
//! + Phase 0.5 + Item C for the full design rationale.

use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::future::{join_all, try_join_all};
use std::net::SocketAddr;

use crate::mesh::{Mesh, MeshBuilder};
use crate::meshos::{
    LoggingDispatcher, MeshOsConfig, MeshOsDaemonSdk, NodeId, RuntimeShutdownError,
};

use super::probes::install_mesh_probes;

/// Default pre-shared key for harness `Mesh` instances. All nodes
/// in a single harness share the same PSK so handshakes succeed.
/// Deterministic so the harness is reproducible run-to-run.
const HARNESS_PSK: [u8; 32] = *b"ai2070-cluster-harness-testing.x";

/// Tuning knobs for [`ClusterHarness::new`]. Defaults are sized
/// for a 5-node loopback cluster on a developer laptop; tests
/// that boot larger clusters or run on constrained CI can extend
/// the budgets via [`Self::default`]-then-mutate.
#[derive(Clone, Debug)]
pub struct ClusterConfig {
    /// PSK every Mesh in the cluster shares. Replace only when
    /// you need to drive negative tests (mismatched PSKs).
    pub psk: [u8; 32],
    /// Budget for the per-pair `accept` + `connect` handshake
    /// fan-out. Includes the kernel-side connect setup. Total
    /// boot budget = handshake + session_stable + snapshot_stable.
    pub handshake_timeout: Duration,
    /// Budget for every `Mesh` to report `peer_count() == n - 1`
    /// once handshakes complete.
    pub mesh_session_stable_timeout: Duration,
    /// Budget for every `MeshOsRuntime` to fold `snapshot.peers`
    /// up to `n - 1` entries via the bridge probes' first few
    /// ticks. Sized as ~3 tick intervals.
    pub meshos_snapshot_stable_timeout: Duration,
    /// Poll cadence while waiting for the two stabilization
    /// barriers above.
    pub poll_interval: Duration,
    /// Tick interval to set on each `MeshOsConfig`. Tighter than
    /// the substrate default (500 ms) so the bridge probes fire
    /// quickly during boot.
    pub meshos_tick_interval: Duration,
}

impl Default for ClusterConfig {
    fn default() -> Self {
        Self {
            psk: HARNESS_PSK,
            handshake_timeout: Duration::from_secs(5),
            mesh_session_stable_timeout: Duration::from_secs(2),
            meshos_snapshot_stable_timeout: Duration::from_secs(3),
            poll_interval: Duration::from_millis(25),
            meshos_tick_interval: Duration::from_millis(100),
        }
    }
}

/// Per-node handle in a [`ClusterHarness`]. Owns one `Mesh` and
/// one `MeshOsDaemonSdk`; consumers register daemons through the
/// SDK and drive RPC / channel work through the `Mesh`.
pub struct ClusterNode {
    /// The Mesh handle, shared `Arc` so bridge probes can hold
    /// long-lived clones without invalidating the harness's own
    /// reference.
    pub mesh: Arc<Mesh>,
    /// MeshOS daemon SDK. Wrapped in `Option` so
    /// [`ClusterHarness::shutdown`] can take it out and drive an
    /// owning `sdk.shutdown().await`. None after shutdown.
    pub sdk: Option<MeshOsDaemonSdk>,
    /// Local UDP bind address (the kernel-assigned ephemeral
    /// port). Useful for cross-checking the harness's expected
    /// peer topology.
    pub local_addr: SocketAddr,
    /// The Mesh-derived 64-bit node id. Stable for the node's
    /// lifetime; the MeshOS layer keys peers by this id.
    pub node_id: NodeId,
    /// The Mesh's Noise public key.
    pub public_key: [u8; 32],
}

/// Mid-run health check. Returns counts of nodes whose `Mesh` and
/// `MeshOsRuntime` views match the expected full-mesh topology.
#[derive(Clone, Copy, Debug)]
pub struct ClusterHealth {
    pub total_nodes: usize,
    pub meshes_with_full_peers: usize,
    pub runtimes_with_full_peers: usize,
}

impl ClusterHealth {
    /// True iff every node sees every other node both at the
    /// Mesh layer and the MeshOS-snapshot layer.
    pub fn fully_converged(&self) -> bool {
        self.meshes_with_full_peers == self.total_nodes
            && self.runtimes_with_full_peers == self.total_nodes
    }
}

/// Errors surfaced by the harness boot + lifecycle path.
#[derive(Debug, thiserror::Error)]
pub enum ClusterError {
    /// `n == 0` or any other pre-build invariant violated.
    #[error("cluster build: {0}")]
    Invariant(String),
    /// `MeshBuilder::new` / `MeshBuilder::build` returned an
    /// error. Wraps the underlying SDK error message.
    #[error("mesh build failed: {0}")]
    MeshBuild(String),
    /// One of the pairwise `accept` + `connect` handshakes
    /// failed. The pair indices are 0-based positions in the
    /// harness's node list. The reason field carries the
    /// underlying SDK error message; not typed as `#[source]`
    /// because the underlying error types are heterogeneous
    /// (accept vs connect vs timeout).
    #[error("handshake failed between node[{from}] and node[{to}]: {reason}")]
    Handshake {
        from: usize,
        to: usize,
        reason: String,
    },
    /// Timed out waiting for one of the stabilization barriers
    /// (Mesh session table or MeshOS snapshot peer fold).
    #[error("timed out waiting for {what} after {budget_ms}ms")]
    Timeout { what: String, budget_ms: u64 },
    /// `MeshOsDaemonSdk::shutdown` returned an error.
    #[error("shutdown failed: {0}")]
    Shutdown(String),
}

/// Multi-node harness. Hand-rolls N `(Mesh, MeshOsDaemonSdk)`
/// pairs on loopback ports, peers them, and lets the
/// [`ClusterNode`] consumers drive registration / RPC / state
/// queries.
pub struct ClusterHarness {
    nodes: Vec<ClusterNode>,
    /// Set after [`Self::shutdown`] completes so [`Drop`] knows
    /// resources have already been released.
    shutdown_called: bool,
}

impl ClusterHarness {
    /// Boot a cluster of `n` nodes with default budgets. Returns
    /// once every Mesh has seen `n - 1` peer sessions and every
    /// MeshOsRuntime has folded `n - 1` peers into its snapshot.
    pub async fn new(n: usize) -> Result<Self, ClusterError> {
        Self::with_config(n, ClusterConfig::default()).await
    }

    /// Same as [`Self::new`] but with caller-tuned budgets / PSK.
    pub async fn with_config(n: usize, cfg: ClusterConfig) -> Result<Self, ClusterError> {
        if n == 0 {
            return Err(ClusterError::Invariant("n must be > 0".into()));
        }

        // (1) Build N Mesh instances in parallel on
        //     `127.0.0.1:0` so the kernel assigns ephemeral ports
        //     we can read back via `mesh.local_addr()`.
        let mesh_futures = (0..n).map(|_| async {
            let builder = MeshBuilder::new("127.0.0.1:0", &cfg.psk)
                .map_err(|e| ClusterError::MeshBuild(e.to_string()))?;
            builder
                .build()
                .await
                .map(Arc::new)
                .map_err(|e| ClusterError::MeshBuild(e.to_string()))
        });
        let meshes: Vec<Arc<Mesh>> = try_join_all(mesh_futures).await?;

        // (2) Read identity off each so we can drive the
        //     handshakes + populate the expected-peer set the
        //     bridge probes filter on.
        let identities: Vec<(NodeId, [u8; 32], SocketAddr)> = meshes
            .iter()
            .map(|m| (m.node_id(), *m.public_key(), m.local_addr()))
            .collect();

        // (3) Drive every ordered pair (i, j) with i < j through
        //     an `accept` + `connect` handshake. Each pair runs
        //     concurrently within itself, but pairs are
        //     sequenced — running every pair concurrently has
        //     each mesh fielding multiple in-flight handshakes
        //     against itself before its receive loop is started,
        //     which races the substrate's handshake state machine.
        //     At N²/2 pairs with ~50 ms each the serialized path
        //     is still well under the boot budget.
        let handshake_budget = cfg.handshake_timeout;
        for i in 0..n {
            for j in (i + 1)..n {
                let mesh_i = Arc::clone(&meshes[i]);
                let mesh_j = Arc::clone(&meshes[j]);
                let (id_i, _, _addr_i) = identities[i];
                let (id_j, pubkey_j, addr_j) = identities[j];
                let accept = async move { mesh_j.accept(id_i).await };
                let connect = async move {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    let peer_addr = format!("{addr_j}");
                    mesh_i.connect(&peer_addr, &pubkey_j, id_j).await
                };
                let result = tokio::time::timeout(
                    handshake_budget,
                    async move { tokio::join!(accept, connect) },
                )
                .await;
                match result {
                    Err(_) => {
                        return Err(ClusterError::Handshake {
                            from: i,
                            to: j,
                            reason: format!(
                                "timed out after {}ms",
                                handshake_budget.as_millis()
                            ),
                        });
                    }
                    Ok((accept_res, connect_res)) => {
                        if let Err(e) = accept_res {
                            return Err(ClusterError::Handshake {
                                from: i,
                                to: j,
                                reason: format!("accept: {e}"),
                            });
                        }
                        if let Err(e) = connect_res {
                            return Err(ClusterError::Handshake {
                                from: i,
                                to: j,
                                reason: format!("connect: {e}"),
                            });
                        }
                    }
                }
            }
        }

        // (4) Start each Mesh's receive loop. Handshake completes
        //     without start() (it's a synchronous Noise XX
        //     exchange driven by the accept/connect futures);
        //     the receive loop is needed for post-handshake
        //     message traffic + the future daemon RPC paths.
        for m in &meshes {
            m.start();
        }

        // (5) Build N MeshOsDaemonSdk instances. Each is wired
        //     to the matching Mesh's node_id so the MeshOS layer
        //     keys daemons + peers under the same id space the
        //     network uses.
        let dispatcher = Arc::new(LoggingDispatcher::new());
        let expected_peers: Arc<Vec<NodeId>> =
            Arc::new(identities.iter().map(|(id, _, _)| *id).collect());

        let mut nodes = Vec::with_capacity(n);
        for (i, mesh) in meshes.iter().enumerate() {
            let (node_id, public_key, local_addr) = identities[i];
            let mut mesh_cfg = MeshOsConfig::default();
            mesh_cfg.this_node = node_id;
            mesh_cfg.tick_interval = cfg.meshos_tick_interval;
            let sdk = MeshOsDaemonSdk::start_with_verifier_and_migration_source(
                mesh_cfg,
                Arc::clone(&dispatcher) as Arc<LoggingDispatcher>,
                None,
                None,
            );
            // (6) Install bridge probes on each runtime so its
            //     tick loop folds peer state derived from the
            //     real Mesh sessions.
            install_mesh_probes(
                sdk.runtime(),
                Arc::clone(mesh),
                Arc::clone(&expected_peers),
            );
            nodes.push(ClusterNode {
                mesh: Arc::clone(mesh),
                sdk: Some(sdk),
                local_addr,
                node_id,
                public_key,
            });
        }

        // (7) Wait for the Mesh-side peer table to report
        //     n - 1 sessions on every node.
        wait_for(
            "mesh session table",
            cfg.mesh_session_stable_timeout,
            cfg.poll_interval,
            || nodes.iter().all(|n| n.mesh.peer_count() == nodes.len() - 1),
        )
        .await?;

        // (8) Wait for the bridge probes to drive
        //     `snapshot.peers.len() == n - 1` on every runtime.
        let expected_remote = nodes.len() - 1;
        wait_for(
            "meshos snapshot.peers fold",
            cfg.meshos_snapshot_stable_timeout,
            cfg.poll_interval,
            || {
                nodes.iter().all(|n| {
                    n.sdk
                        .as_ref()
                        .map(|sdk| sdk.runtime().snapshot().peers.len() == expected_remote)
                        .unwrap_or(false)
                })
            },
        )
        .await?;

        Ok(Self {
            nodes,
            shutdown_called: false,
        })
    }

    /// Borrow every node's handle. Order matches the
    /// construction order, NOT the node-id order.
    pub fn nodes(&self) -> &[ClusterNode] {
        &self.nodes
    }

    /// Borrow the i-th node by position. Panics on out-of-range
    /// index — tests are expected to know how many nodes they
    /// built.
    pub fn nth(&self, i: usize) -> &ClusterNode {
        &self.nodes[i]
    }

    /// Number of nodes in the cluster.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// True iff the harness was built with `n == 0`. Today
    /// [`Self::new`] rejects that case at construction; this
    /// exists for parity with `len()` and to satisfy clippy.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Sample mesh + runtime peer counts against the expected
    /// full-mesh topology. Useful for long-running demos that
    /// want to confirm the cluster hasn't drifted apart.
    pub fn health(&self) -> ClusterHealth {
        let total = self.nodes.len();
        let expected_remote = total.saturating_sub(1);
        let meshes_with_full_peers = self
            .nodes
            .iter()
            .filter(|n| n.mesh.peer_count() == expected_remote)
            .count();
        let runtimes_with_full_peers = self
            .nodes
            .iter()
            .filter(|n| {
                n.sdk
                    .as_ref()
                    .map(|sdk| sdk.runtime().snapshot().peers.len() == expected_remote)
                    .unwrap_or(false)
            })
            .count();
        ClusterHealth {
            total_nodes: total,
            meshes_with_full_peers,
            runtimes_with_full_peers,
        }
    }

    /// Drive a clean shutdown: drains the MeshOsDaemonSdk on
    /// every node (in parallel) and lets the `Mesh` `Drop`
    /// impls release their UDP sockets. Idempotent — calling
    /// shutdown a second time is a no-op.
    ///
    /// `Drop` runs the same path best-effort if the caller
    /// forgot to await this; see [`Drop for ClusterHarness`].
    pub async fn shutdown(mut self) -> Result<(), ClusterError> {
        if self.shutdown_called {
            return Ok(());
        }
        self.shutdown_called = true;
        let sdks: Vec<MeshOsDaemonSdk> = self
            .nodes
            .iter_mut()
            .filter_map(|n| n.sdk.take())
            .collect();
        let results = join_all(sdks.into_iter().map(|sdk| async move { sdk.shutdown().await }))
            .await;
        for r in results {
            // RuntimeShutdownError doesn't implement Display
            // today; fall back to Debug so callers still see
            // which variant fired.
            r.map_err(|e: RuntimeShutdownError| ClusterError::Shutdown(format!("{e:?}")))?;
        }
        Ok(())
    }
}

impl Drop for ClusterHarness {
    fn drop(&mut self) {
        if self.shutdown_called {
            return;
        }
        // The MeshOsRuntime + MeshNode Drop impls release their
        // own tasks + sockets. We can't await an async shutdown
        // here, so the caller forfeits the chance to surface a
        // RuntimeShutdownError; the data path still tears down
        // cleanly. Log via stderr so a test that forgets to
        // call shutdown gets a hint.
        eprintln!(
            "[net-sdk testing] ClusterHarness dropped without \
             explicit shutdown — relying on Drop impls. \
             Awaiting `harness.shutdown().await` is the clean path."
        );
    }
}

/// Poll `cond` every `poll_interval` for up to `budget`. Returns
/// `Ok(())` the first time `cond()` returns true; returns a
/// [`ClusterError::Timeout`] tagged with `what` if the budget
/// elapses first.
async fn wait_for<F: FnMut() -> bool>(
    what: &'static str,
    budget: Duration,
    poll_interval: Duration,
    mut cond: F,
) -> Result<(), ClusterError> {
    let start = Instant::now();
    loop {
        if cond() {
            return Ok(());
        }
        if start.elapsed() >= budget {
            return Err(ClusterError::Timeout {
                what: what.to_string(),
                budget_ms: budget.as_millis() as u64,
            });
        }
        tokio::time::sleep(poll_interval).await;
    }
}
