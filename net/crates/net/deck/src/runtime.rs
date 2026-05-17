//! Single-node in-process MeshOS runtime. The deck always
//! spawns a live `MeshOsRuntime` so its snapshot reader is wired
//! even when no real cluster is attached; tabs render their
//! "waiting / no data" states until a real cluster source is
//! wired by the operator.
//!
//! For a full multi-node "real-cluster" experience, see
//! `crate::demo::spawn` (gated behind the `demo` feature).

use std::sync::Arc;
use std::time::Duration;

use net_sdk::dataforts::MeshBlobAdapter;
use net_sdk::deck::{AdminVerifier, DeckClient, OperatorIdentity, OperatorRegistry};
use net_sdk::meshos::{EntityKeypair, MeshOsConfig, MeshOsDaemonSdk, NodeId};

/// Handle returned by [`spawn`]. Hold for the app lifetime;
/// dropping it tears the runtime down.
pub struct Harness {
    /// Keeps the runtime alive. Dropping the SDK shuts the
    /// underlying `MeshOsRuntime` down.
    _sdk: MeshOsDaemonSdk,
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
}

/// Spawn the in-process runtime. The runtime starts empty —
/// the deck shows the empty cluster view, ready to connect
/// to real cluster sources. For a fully populated demo
/// cluster, build with `--features demo` and use the
/// `crate::demo::spawn` path instead.
pub async fn spawn() -> color_eyre::Result<Harness> {
    // Faster tick than the production default so the UI's
    // snapshot refresh feels responsive.
    let mut cfg = MeshOsConfig::default();
    cfg.this_node = 0x0001;
    cfg.tick_interval = Duration::from_millis(250);
    let this_node = cfg.this_node;
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
    let deck = Arc::new(DeckClient::from_runtime(sdk.runtime(), identity));

    Ok(Harness {
        _sdk: sdk,
        deck,
        blob_adapters: Vec::new(),
        this_node,
    })
}
