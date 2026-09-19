//! Single-node in-process MeshOS runtime. The deck always
//! spawns a live `MeshOsRuntime` *and* a live mesh node, so both
//! halves of its operator surface are wired: the snapshot reader
//! (tabs render their "waiting / no data" states until a real
//! cluster source is wired by the operator) and the substrate
//! accessors that do not ride the snapshot — subnet, gateway,
//! channel visibility, and the announced RTC anchors the NODES
//! table's ANCHOR column reads.
//!
//! The mesh node is the deck's own: it binds loopback, runs its
//! dispatch loop, and folds the signed announcements of every
//! peer that handshakes with it. `DeckClient::rtc_anchors` reads
//! that fold, so the ANCHOR column shows what this node has
//! actually ingested rather than the empty default a mesh-less
//! client returns.
//!
//! For a full multi-node "real-cluster" experience, see
//! `crate::demo::spawn` (gated behind the `demo` feature).

use std::sync::Arc;
use std::time::Duration;

use net_sdk::dataforts::MeshBlobAdapter;
use net_sdk::deck::{AdminVerifier, DeckClient, OperatorIdentity, OperatorRegistry};
use net_sdk::meshos::{EntityKeypair, MeshOsConfig, MeshOsDaemonSdk, NodeId};
use net_sdk::{Mesh, MeshBuilder};

/// Bind address for the deck's own mesh node. Loopback with an
/// ephemeral port: the deck initiates, it is not a daemon peers
/// dial.
const MESH_BIND: &str = "127.0.0.1:0";

/// Handle returned by [`spawn`]. Hold for the app lifetime;
/// dropping it tears the runtime down.
pub struct Harness {
    /// Keeps the runtime alive. Dropping the SDK shuts the
    /// underlying `MeshOsRuntime` down.
    _sdk: MeshOsDaemonSdk,
    /// The deck's own live mesh node — the one whose ingested
    /// announcements the `DeckClient` below reads (it holds an
    /// `Arc<MeshNode>` clone of it). Held for the session so the
    /// socket and the dispatch loop outlive [`spawn`]; read back
    /// only by the in-crate witness, which stands a peer up
    /// against this same node.
    #[cfg_attr(not(all(test, feature = "webrtc")), allow(dead_code))]
    mesh: Mesh,
    deck: Arc<DeckClient>,
    /// Registered `MeshBlobAdapter` instances. Default mode
    /// leaves this empty; operators wire their own. BLOBS
    /// reads from whichever adapter the operator cursors on
    /// the DATAFORTS list.
    blob_adapters: Vec<Arc<MeshBlobAdapter>>,
    /// The `this_node` id the substrate runtime was configured
    /// with. The App uses it for placement-based pivots and
    /// admin commits without hardcoding a literal that drifts
    /// from the runtime config.
    this_node: NodeId,
}

impl Harness {
    pub fn deck(&self) -> Arc<DeckClient> {
        Arc::clone(&self.deck)
    }

    pub fn blob_adapters(&self) -> Vec<Arc<MeshBlobAdapter>> {
        self.blob_adapters.clone()
    }

    pub fn this_node(&self) -> NodeId {
        self.this_node
    }

    /// The live mesh node this harness attached to the
    /// `DeckClient`. Test-only: production reads the mesh
    /// through the client's accessors, not around them.
    #[cfg(all(test, feature = "webrtc"))]
    pub fn mesh(&self) -> &Mesh {
        &self.mesh
    }
}

/// Spawn the in-process runtime. The MeshOS snapshot starts
/// empty — the deck shows the empty cluster view, ready to
/// connect to real cluster sources — while the mesh node it
/// attaches is live from the first frame. For a fully populated
/// demo cluster, build with `--features demo` and use the
/// `crate::demo::spawn` path instead.
pub async fn spawn() -> color_eyre::Result<Harness> {
    // The deck's mesh has no configured cluster to join yet, so
    // its PSK is minted per process: a fresh trust domain of one.
    // `spawn_with_psk` is the whole body, so the witness exercises
    // this same construction with a PSK it can hand a peer.
    let mut psk = [0u8; 32];
    getrandom::fill(&mut psk)
        .map_err(|e| color_eyre::eyre::eyre!("no system entropy for the deck's mesh PSK: {e}"))?;
    spawn_with_psk(&psk).await
}

/// [`spawn`] with the deck mesh's pre-shared key supplied by the
/// caller. Every construction step below is what production
/// runs; `spawn` differs only in minting the key.
pub async fn spawn_with_psk(psk: &[u8; 32]) -> color_eyre::Result<Harness> {
    // The mesh node comes first: its id is the id the MeshOS
    // layer is configured with, so daemons, peers and anchor rows
    // are all keyed in one id space (the same ordering
    // `ClusterHarness` uses). A hardcoded `this_node` would have
    // the ANCHOR column keyed off a different space than the
    // NODES rows it decorates.
    let mesh = MeshBuilder::new(MESH_BIND, psk)
        .map_err(|e| color_eyre::eyre::eyre!("the deck's mesh bind address was rejected: {e}"))?
        .build()
        .await
        .map_err(|e| color_eyre::eyre::eyre!("the deck's mesh node failed to start: {e}"))?;
    // Dispatch loop + periodic re-announce. Without it nothing is
    // ever received, so nothing is ever ingested.
    mesh.start();
    let this_node = mesh.node_id();

    // Faster tick than the production default so the UI's
    // snapshot refresh feels responsive.
    let mut cfg = MeshOsConfig::default();
    cfg.this_node = this_node;
    cfg.tick_interval = Duration::from_millis(250);
    let dispatcher = Arc::new(net_sdk::meshos::LoggingDispatcher::new());

    // Single operator keypair used for both:
    //  1. the `OperatorRegistry` the verifier checks signatures
    //     against, and
    //  2. the `OperatorIdentity` the `DeckClient` signs with.
    // Threshold=1 — single-operator demo cluster. Real
    // deployments wire a populated registry + a higher M-of-N
    // threshold per `DECK_SDK_PLAN.md`.
    let operator_keypair = EntityKeypair::generate();
    let mut registry = OperatorRegistry::new();
    registry.register(&operator_keypair);
    let verifier = Arc::new(AdminVerifier::new(Arc::new(registry), 1));

    let sdk = MeshOsDaemonSdk::start_with_verifier_and_migration_source(
        cfg,
        dispatcher,
        Some(verifier),
        None,
    );

    let identity = OperatorIdentity::from_keypair(operator_keypair);
    // `with_mesh` is the wiring: without it `rtc_anchors` (and
    // every other substrate accessor) returns the mesh-less
    // default, which is the empty ANCHOR column the operator used
    // to see no matter how many anchors announced.
    let deck =
        Arc::new(DeckClient::from_runtime(sdk.runtime(), identity).with_mesh(mesh.node_arc()));

    Ok(Harness {
        _sdk: sdk,
        mesh,
        deck,
        blob_adapters: Vec::new(),
        this_node,
    })
}
