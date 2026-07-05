//! Wrap orchestration — publish stdio MCP servers as mesh capabilities
//! (`MCP_BRIDGE_PLAN.md` Phase 1; public surface per `MCP_BRIDGE_SDK_PLAN.md`
//! P0: `publish_server(cmd, opts) -> PublicationHandle`, `handle.withdraw()`).
//!
//! [`ServerPublisher::publish_server`] spawns a stdio MCP server, discovers
//! its tools, lowers them, announces them as capabilities, and serves one
//! [`WrapInvokeHandler`] per tool over nRPC. The returned
//! [`PublicationHandle`] is that publication's lifetime: [`refresh`]
//! reconciles a changed tool set, [`withdraw`] reverses everything
//! immediately, and dropping the handle stops the services and the wrapped
//! process (leaving the announcement to the next registry change — Drop
//! cannot announce).
//!
//! **Why a publisher object.** Capability announcements are whole-node-set
//! replacements, and the describe service is one nRPC service per node — so
//! per-publication state must merge somewhere before it reaches the mesh. The
//! publisher owns that merge: each publication contributes its lowered tools,
//! and every publish / refresh / withdraw re-announces the union and swaps
//! the merged, per-publication-scoped describe catalog. Publications are
//! handle-scoped with **no global withdraw** (plan doctrine): withdrawing one
//! never touches the others. Tool ids must be distinct across a node's
//! publications — the per-tool nRPC registration surfaces a collision as
//! [`WrapError::Serve`] and the publish rolls back.
//!
//! Everything mesh-facing goes through the `net-mesh-sdk` [`Mesh`] surface
//! (doctrine #1). Owner-only enforcement is the handler's [`OwnerScope`];
//! announcement-level allow-list scoping (so non-owners can't even discover
//! the capability) is a later hardening — for v0 the visibility / scope ride
//! as announcement *metadata*, honest even before the full permission system.
//!
//! [`refresh`]: PublicationHandle::refresh
//! [`withdraw`]: PublicationHandle::withdraw

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use net_sdk::capabilities::CapabilitySet;
use net_sdk::mesh::Mesh;
use net_sdk::mesh_rpc::ServeHandle;
use net_sdk::tool::description_metadata_key;
use parking_lot::Mutex;

use super::catalog::{build_catalog, shared_catalog, CatalogPart, DescribeHandler, SharedCatalog};
use super::credentials::{classify, ClassifyError, CredentialOverride, WrapEnv};
use super::descriptor::{lower_tool, LoweredTool, LoweringContext, Substitutability};
use super::invoke::{OwnerScope, WrapInvokeHandler};
use super::stdio::StdioMcpClient;
use super::McpError;
use crate::bridge::{BRIDGE_PROVIDER_TAG, DESCRIBE_SERVICE};
use crate::spec::Implementation;

/// A failure setting up or reconciling a publication.
#[derive(Debug, thiserror::Error)]
pub enum WrapError {
    /// Talking to the wrapped MCP server failed (spawn / initialize / list).
    #[error("wrapped MCP server error: {0}")]
    Mcp(#[from] McpError),
    /// Credential classification rejected the operator's flags.
    #[error("credential classification: {0}")]
    Classify(#[from] ClassifyError),
    /// Announcing the capability set on the mesh failed.
    #[error("capability announce failed: {0}")]
    Announce(String),
    /// Serving a tool's nRPC handler failed.
    #[error("serving tool {tool:?} failed: {reason}")]
    Serve {
        /// The tool whose serve failed.
        tool: String,
        /// The underlying serve error, stringified (the SDK error type is
        /// not part of this crate's surface).
        reason: String,
    },
    /// A wrapped tool's stored schema could not be parsed while building the
    /// describe catalog.
    #[error("describe catalog build failed: {0}")]
    Catalog(#[from] super::catalog::CatalogError),
    /// A publish/refresh step failed AND the rollback re-announce that would
    /// restore a consistent mesh state also failed — the node may be left
    /// advertising tools with no live handler. Both errors are surfaced so the
    /// inconsistency is diagnosable rather than silent.
    #[error(
        "{original}; ALSO failed to roll back the announcement \
         (the mesh may still advertise stale tools): {rollback}"
    )]
    RollbackFailed {
        /// The original failure that triggered the rollback.
        original: String,
        /// The failure of the rollback re-announce.
        rollback: String,
    },
}

/// Fold a rollback re-announce result into the error to return: if the
/// rollback succeeded, return the original failure unchanged; if it also
/// failed, surface both as [`WrapError::RollbackFailed`] so a left-inconsistent
/// mesh state is never hidden behind a single error.
fn rollback_result(original: WrapError, rollback: Result<(), WrapError>) -> WrapError {
    match rollback {
        Ok(()) => original,
        Err(rb) => WrapError::RollbackFailed {
            original: original.to_string(),
            rollback: rb.to_string(),
        },
    }
}

/// What a [`PublicationHandle::refresh`] changed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RefreshDelta {
    /// Tool ids newly served + announced.
    pub added: Vec<String>,
    /// Tool ids withdrawn (the wrapped server dropped them).
    pub removed: Vec<String>,
}

impl RefreshDelta {
    /// True when nothing changed.
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty()
    }
}

/// Assemble the capability set announced for a set of bridged tools: one tag
/// per tool (so `cap search <tool>` finds it) plus the bridge metadata and
/// description under the `tool::<id>::<field>` convention, and — when there is
/// at least one tool — the [`BRIDGE_PROVIDER_TAG`] so a demand-side gateway can
/// find this node as a bridge provider and fetch its catalog via the describe
/// service.
///
/// One set for the whole node — capability announcements are whole-node-set
/// replacements, so every publication's tags and metadata must ride together
/// in a single announcement (the publisher passes the union here).
pub fn build_capability_set<'a>(
    lowered: impl IntoIterator<Item = &'a LoweredTool>,
) -> CapabilitySet {
    let mut caps = CapabilitySet::new();
    // The bridge-provider tag rides only when there is at least one tool, and
    // ahead of the per-tool tags. `peekable` preserves that ordering without
    // materializing the iterator, so the caller can stream tools borrowed
    // straight from the live contributions instead of cloning them.
    let mut lowered = lowered.into_iter().peekable();
    if lowered.peek().is_some() {
        caps = caps.add_tag(BRIDGE_PROVIDER_TAG.to_string());
    }
    for lt in lowered {
        let d = &lt.descriptor;
        caps = caps.add_tag(d.tool_id.clone());
        for (key, value) in &lt.bridge_metadata {
            caps = caps.with_metadata(key.clone(), value.clone());
        }
        if let Some(description) = &d.description {
            caps = caps.with_metadata(description_metadata_key(&d.tool_id), description.clone());
        }
    }
    caps
}

/// Configuration for a publication: the credential-classification overrides
/// and the substitutability the operator declared, plus who may invoke.
#[derive(Debug, Clone)]
pub struct WrapConfig {
    /// How this server identifies itself to the wrapped MCP server.
    pub client_info: Implementation,
    /// Who may invoke the wrapped tools (owner-only by default).
    pub scope: OwnerScope,
    /// Credential-status override (`--credentialed` / `--no-credentials`).
    pub credential_override: CredentialOverride,
    /// Confirms a downward credential override (`--force`).
    pub force: bool,
    /// Whether the tools are declared interchangeable across providers.
    pub substitutability: Substitutability,
}

impl WrapConfig {
    /// A default config: owner-only to `owner_origin`, detect credentials,
    /// provider-local.
    pub fn owner_only(client_info: Implementation, owner_origin: u64) -> Self {
        Self {
            client_info,
            scope: OwnerScope::owner_only(owner_origin),
            credential_override: CredentialOverride::Detect,
            force: false,
            substitutability: Substitutability::ProviderLocal,
        }
    }
}

/// One publication's contribution to the node's merged mesh state: its
/// lowered tools (announcement tags/metadata + describe catalog) and the
/// owner scope gating their describe visibility.
#[derive(Clone)]
struct Contribution {
    lowered: Vec<LoweredTool>,
    scope: OwnerScope,
}

/// The per-mesh state every publication of a [`ServerPublisher`] shares.
struct PublisherShared {
    /// Live contributions keyed by publication id. `BTreeMap` so the merged
    /// announcement and catalog are assembled in a deterministic order.
    contributions: Mutex<BTreeMap<u64, Contribution>>,
    /// The next publication id.
    next_id: AtomicU64,
    /// Serializes every announce + catalog swap: announcements are
    /// whole-node-set replacements, so two interleaved re-announces could
    /// otherwise let a stale union overtake a newer one.
    sync: tokio::sync::Mutex<()>,
    /// The merged, per-publication-scoped catalog the one describe service
    /// reads.
    catalog: SharedCatalog,
    /// The describe-service handle: served when the first publication lands,
    /// dropped (withdrawing the service) when the last one leaves.
    describe: Mutex<Option<ServeHandle>>,
}

impl PublisherShared {
    fn insert(&self, id: u64, contribution: Contribution) {
        self.contributions.lock().insert(id, contribution);
    }

    fn remove(&self, id: u64) {
        self.contributions.lock().remove(&id);
    }

    /// Replace publication `id`'s contribution, returning the prior one (if
    /// any) so a failed reconcile can put it back via [`Self::restore`].
    fn swap(&self, id: u64, contribution: Contribution) -> Option<Contribution> {
        self.contributions.lock().insert(id, contribution)
    }

    /// Restore a contribution captured by [`Self::swap`]: reinstate the prior
    /// one, or remove the entry entirely if there was none.
    fn restore(&self, id: u64, prior: Option<Contribution>) {
        let mut contributions = self.contributions.lock();
        match prior {
            Some(p) => {
                contributions.insert(id, p);
            }
            None => {
                contributions.remove(&id);
            }
        }
    }

    /// Compute the merged mesh state from the live contributions: the union
    /// capability set to announce and the scoped describe-catalog parts.
    fn merged(&self) -> Result<(CapabilitySet, Vec<CatalogPart>), WrapError> {
        let contributions = self.contributions.lock();
        // Announce the union of every publication's lowered tools, borrowing
        // each contribution's tools directly rather than deep-cloning them all
        // into a throwaway Vec on every publish / refresh / withdraw.
        let caps = build_capability_set(contributions.values().flat_map(|c| c.lowered.iter()));
        let parts = contributions
            .values()
            .map(|c| {
                Ok(CatalogPart {
                    scope: c.scope.clone(),
                    catalog: build_catalog(&c.lowered)?,
                })
            })
            .collect::<Result<Vec<_>, WrapError>>()?;
        Ok((caps, parts))
    }

    /// Re-announce the union and swap the merged describe catalog. Callers
    /// hold [`Self::sync`] so a stale union can never overtake a newer one.
    /// Announce comes first — the substrate re-broadcasts the merged
    /// capability set as each service registers, so tool tags only propagate
    /// to peers when they are already in the announced baseline.
    async fn sync_mesh(&self, mesh: &Mesh) -> Result<(), WrapError> {
        let (caps, parts) = self.merged()?;
        mesh.announce_capabilities(caps)
            .await
            .map_err(|e| WrapError::Announce(e.to_string()))?;
        *self.catalog.write().await = Arc::new(parts);
        Ok(())
    }

    /// Serve the describe service if it isn't already — the one
    /// [`DescribeHandler`] reads the merged catalog and gates each part by
    /// its own publication's scope.
    fn ensure_describe(&self, mesh: &Mesh) -> Result<(), WrapError> {
        let mut describe = self.describe.lock();
        if describe.is_none() {
            let handle = mesh
                .serve_rpc(
                    DESCRIBE_SERVICE,
                    Arc::new(DescribeHandler::new(self.catalog.clone())),
                )
                .map_err(|e| WrapError::Serve {
                    tool: DESCRIBE_SERVICE.to_string(),
                    reason: e.to_string(),
                })?;
            *describe = Some(handle);
        }
        Ok(())
    }

    /// Withdraw the describe service when no publication is left (dropping
    /// the handle reverses its `serve_rpc`).
    fn drop_describe_if_idle(&self) {
        // Hold the contributions lock across the describe teardown so the
        // service can't be dropped between the emptiness check and the take —
        // e.g. by a concurrent publish inserting a contribution. (Belt and
        // suspenders: publish/withdraw already serialize through `sync`.)
        let contributions = self.contributions.lock();
        if contributions.is_empty() {
            self.describe.lock().take();
        }
    }
}

/// Publishes stdio MCP servers onto one mesh node. Owns the merge every
/// publication shares (see the module docs for why the merge must exist);
/// cheap to clone — clones publish into the same merged node state.
#[derive(Clone)]
pub struct ServerPublisher {
    mesh: Arc<Mesh>,
    shared: Arc<PublisherShared>,
}

impl ServerPublisher {
    /// A publisher for `mesh`. All publications made through it (and its
    /// clones) merge into the node's single announcement + describe catalog.
    pub fn new(mesh: Arc<Mesh>) -> Self {
        Self {
            mesh,
            shared: Arc::new(PublisherShared {
                contributions: Mutex::new(BTreeMap::new()),
                next_id: AtomicU64::new(0),
                sync: tokio::sync::Mutex::new(()),
                catalog: shared_catalog(Vec::new()),
                describe: Mutex::new(None),
            }),
        }
    }

    /// Publish a stdio MCP server as mesh capabilities: spawn, discover,
    /// lower, announce, and serve every tool. Returns the live
    /// [`PublicationHandle`].
    pub async fn publish_server(
        &self,
        program: &str,
        args: &[String],
        envs: &[(String, String)],
        config: WrapConfig,
    ) -> Result<PublicationHandle, WrapError> {
        // Classify BEFORE spawning: an invalid override (e.g. downward without
        // --force) should fail fast without starting a process.
        let credential_status = classify(
            &WrapEnv {
                program,
                args,
                envs,
            },
            config.credential_override,
            config.force,
        )?;

        // Connect and discover.
        let client =
            Arc::new(StdioMcpClient::spawn(program, args, envs, config.client_info.clone()).await?);
        let init = client.initialize().await?;

        let ctx = LoweringContext {
            server_version: init.server_info.version,
            credential_status,
            substitutability: config.substitutability,
        };
        let (lowered, skipped) = discover_and_lower(&client, &ctx).await?;

        let id = self.shared.next_id.fetch_add(1, Ordering::Relaxed);

        // One writer at a time to the announced set / merged catalog.
        let _sync = self.shared.sync.lock().await;

        self.shared.insert(
            id,
            Contribution {
                lowered: lowered.clone(),
                scope: config.scope.clone(),
            },
        );

        // Announce the merged set + swap the catalog, then make sure the
        // describe service is up so demand-side gateways can read the bridged
        // descriptors (schema + credential status). Any failure rolls this
        // contribution back out so a failed publish never leaves stale,
        // uninvocable capabilities discoverable.
        if let Err(e) = self.shared.sync_mesh(&self.mesh).await {
            self.shared.remove(id);
            return Err(e);
        }
        if let Err(e) = self.shared.ensure_describe(&self.mesh) {
            self.shared.remove(id);
            return Err(rollback_result(e, self.shared.sync_mesh(&self.mesh).await));
        }

        // Serve one caller-aware handler per tool. If any serve fails
        // (including a tool-id collision with another publication), roll back:
        // drop the handles served so far (each reverses its `serve_rpc`),
        // remove the contribution, and re-announce the remainder.
        let mut handles: HashMap<String, ServeHandle> = HashMap::with_capacity(lowered.len());
        for lt in &lowered {
            let tool_id = lt.descriptor.tool_id.clone();
            // Serve under the channel-safe `tool_id`, invoke by the original
            // `mcp_name` (they differ only for a sanitized name).
            let handler = WrapInvokeHandler::new(
                Arc::clone(&client),
                lt.mcp_name.clone(),
                config.scope.clone(),
            );
            match self.mesh.serve_rpc(&tool_id, Arc::new(handler)) {
                Ok(handle) => {
                    handles.insert(tool_id, handle);
                }
                Err(e) => {
                    drop(handles);
                    self.shared.remove(id);
                    self.shared.drop_describe_if_idle();
                    let serve_err = WrapError::Serve {
                        tool: tool_id,
                        reason: e.to_string(),
                    };
                    return Err(rollback_result(
                        serve_err,
                        self.shared.sync_mesh(&self.mesh).await,
                    ));
                }
            }
        }

        let mut tools: Vec<String> = handles.keys().cloned().collect();
        tools.sort();
        Ok(PublicationHandle {
            client,
            handles,
            ctx,
            scope: config.scope,
            tools,
            skipped,
            registration: Registration {
                mesh: Arc::clone(&self.mesh),
                shared: Arc::clone(&self.shared),
                id,
            },
        })
    }
}

/// De-registers a publication's contribution when its handle goes away, so no
/// later publish / refresh / withdraw re-announces a dead publication's
/// tools. Split from [`PublicationHandle`] so `withdraw(self)` can consume the
/// handle while Drop stays idempotent.
struct Registration {
    mesh: Arc<Mesh>,
    shared: Arc<PublisherShared>,
    id: u64,
}

impl Drop for Registration {
    fn drop(&mut self) {
        // Registry cleanup only — Drop cannot await an announce, so the
        // node's announcement is reconciled at the next registry change (or
        // dies with the node). `withdraw()` is the immediate path.
        self.shared.remove(self.id);
        self.shared.drop_describe_if_idle();
    }
}

/// A live publication: the announced + served tools of one wrapped MCP
/// server. [`withdraw`](Self::withdraw) to reverse everything immediately;
/// dropping the handle stops the nRPC services and the wrapped process but
/// leaves the announcement to the next registry change (Drop cannot
/// announce).
pub struct PublicationHandle {
    /// The connected client. Held so the wrapped process outlives the
    /// publication; dropping it kills the child.
    client: Arc<StdioMcpClient>,
    /// `tool_id -> serve handle`. Dropping a handle reverses its `serve_rpc`.
    /// Keyed so [`refresh`](Self::refresh) can reconcile against the new set.
    handles: HashMap<String, ServeHandle>,
    /// Lowering context reused when re-lowering on refresh (the wrapped
    /// server's version + the per-publication credential status /
    /// substitutability).
    ctx: LoweringContext,
    /// Owner scope reused when serving tools that appear on refresh.
    scope: OwnerScope,
    /// The tool ids served, sorted, for diagnostics.
    tools: Vec<String>,
    /// MCP tool names skipped because they have no usable id (an empty name).
    /// Non-charset-safe names are sanitized and bridged, not skipped. Surfaced
    /// so the operator sees what wasn't bridged instead of it failing silently.
    skipped: Vec<String>,
    /// The publisher linkage; its Drop de-registers this publication.
    registration: Registration,
}

impl PublicationHandle {
    /// The tool ids this publication serves.
    pub fn tools(&self) -> &[String] {
        &self.tools
    }

    /// MCP tool names that were skipped because they have no usable id (an
    /// empty name). A non-charset-safe name is sanitized and bridged instead.
    pub fn skipped_tools(&self) -> &[String] {
        &self.skipped
    }

    /// The wrapped client (e.g. to subscribe to `tools/list_changed`).
    pub fn client(&self) -> &Arc<StdioMcpClient> {
        &self.client
    }

    /// Re-read the wrapped server's tools and reconcile the mesh: announce the
    /// merged set, serve tools that appeared, and withdraw tools that
    /// vanished. Call this on a `tools/list_changed` notification (subscribe
    /// via [`client`](Self::client)`().subscribe_list_changed()`) so bridged
    /// descriptors stay current — "always up-to-date types" holds for bridged
    /// tools too.
    pub async fn refresh(&mut self) -> Result<RefreshDelta, WrapError> {
        let (lowered, skipped) = discover_and_lower(&self.client, &self.ctx).await?;
        let desired: HashSet<String> = lowered
            .iter()
            .map(|lt| lt.descriptor.tool_id.clone())
            .collect();

        let shared = &self.registration.shared;
        let mesh = &self.registration.mesh;
        let id = self.registration.id;
        let _sync = shared.sync.lock().await;

        // Swap this publication's contribution, keeping the prior one so any
        // failure past this point can restore it. Without the rollback, a
        // failed re-announce or a tool-id collision would leave the shared map
        // advertising this publication's new tool set with no live handler —
        // and a sibling publication's later re-announce would keep propagating
        // it. `publish_server` already rolls back the same way.
        let prior = shared.swap(
            id,
            Contribution {
                lowered: lowered.clone(),
                scope: self.scope.clone(),
            },
        );

        // Re-announce the union and refresh the merged describe catalog so
        // demand-side describes see the new schemas/status ("always
        // up-to-date types" for describe too). On failure, put the prior
        // contribution back and re-announce the prior union.
        if let Err(e) = shared.sync_mesh(mesh).await {
            shared.restore(id, prior.clone());
            return Err(rollback_result(e, shared.sync_mesh(mesh).await));
        }

        // Serve tools that appeared. On a serve failure (e.g. a tool-id
        // collision with a sibling publication), undo this refresh: drop the
        // handles served in this call (each reverses its `serve_rpc`), restore
        // the prior contribution, and re-announce the prior union — so the mesh
        // never keeps advertising a tool this publication can't handle.
        let mut added = Vec::new();
        for lt in &lowered {
            let tool_id = lt.descriptor.tool_id.clone();
            if !self.handles.contains_key(&tool_id) {
                // Serve under the channel-safe `tool_id`, but invoke the wrapped
                // tool by its original `mcp_name` (they differ for a sanitized
                // name).
                let handler = WrapInvokeHandler::new(
                    Arc::clone(&self.client),
                    lt.mcp_name.clone(),
                    self.scope.clone(),
                );
                match mesh.serve_rpc(&tool_id, Arc::new(handler)) {
                    Ok(handle) => {
                        self.handles.insert(tool_id.clone(), handle);
                        added.push(tool_id);
                    }
                    Err(e) => {
                        for served in &added {
                            self.handles.remove(served);
                        }
                        shared.restore(id, prior.clone());
                        let serve_err = WrapError::Serve {
                            tool: tool_id,
                            reason: e.to_string(),
                        };
                        return Err(rollback_result(serve_err, shared.sync_mesh(mesh).await));
                    }
                }
            }
        }
        // Withdraw tools that vanished (dropping the handle reverses serve_rpc).
        let removed: Vec<String> = self
            .handles
            .keys()
            .filter(|id| !desired.contains(*id))
            .cloned()
            .collect();
        for tool_id in &removed {
            self.handles.remove(tool_id);
        }

        self.tools = self.handles.keys().cloned().collect();
        self.tools.sort();
        self.skipped = skipped;
        added.sort();
        Ok(RefreshDelta {
            added,
            removed: {
                let mut r = removed;
                r.sort();
                r
            },
        })
    }

    /// Withdraw this publication immediately: re-announce the remaining
    /// publications' union (empty when this was the last) so peers stop
    /// advertising tools whose handlers are about to drop, withdraw the
    /// describe service if idle, then stop the tool services and the wrapped
    /// process (both drop with `self`). Other publications on the node are
    /// untouched — there is no global withdraw.
    pub async fn withdraw(self) -> Result<(), WrapError> {
        let shared = Arc::clone(&self.registration.shared);
        let mesh = Arc::clone(&self.registration.mesh);
        let _sync = shared.sync.lock().await;
        shared.remove(self.registration.id);
        let result = shared.sync_mesh(&mesh).await;
        shared.drop_describe_if_idle();
        // `self` drops here: tool handles reverse their serve_rpc, the client
        // kills the wrapped process, and the Registration drop is a no-op
        // (already de-registered).
        result
    }
}

/// Discover the wrapped server's tools and lower them, returning
/// `(lowered, skipped_names)`. Shared by [`ServerPublisher::publish_server`]
/// and [`PublicationHandle::refresh`].
///
/// A name that isn't already a valid nRPC service id is *sanitized* into one by
/// [`lower_tool`] (via `channel_safe_tool_id`) rather than dropped, so a tool
/// with an uppercase / spaced / punctuated name is still bridged. Only a tool
/// with an empty name — which has no usable id — is skipped.
async fn discover_and_lower(
    client: &StdioMcpClient,
    ctx: &LoweringContext,
) -> Result<(Vec<LoweredTool>, Vec<String>), WrapError> {
    let tools = client.list_tools().await?;
    let (usable, empty_named): (Vec<_>, Vec<_>) =
        tools.into_iter().partition(|t| !t.name.trim().is_empty());
    let skipped: Vec<String> = empty_named.into_iter().map(|t| t.name).collect();
    let lowered: Vec<LoweredTool> = usable.iter().map(|t| lower_tool(t, ctx)).collect();
    Ok((lowered, skipped))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::Tool;
    use net_sdk::capabilities::CapabilityFilter;
    use serde_json::json;

    fn tool(name: &str, description: &str) -> Tool {
        Tool {
            name: name.to_string(),
            title: None,
            description: Some(description.to_string()),
            input_schema: json!({ "type": "object" }),
            output_schema: None,
        }
    }

    fn lowered(names: &[(&str, &str)]) -> Vec<LoweredTool> {
        let ctx = LoweringContext {
            server_version: "1.0.0".to_string(),
            credential_status: super::super::CredentialStatus::Unknown,
            substitutability: Substitutability::ProviderLocal,
        };
        names
            .iter()
            .map(|(n, d)| lower_tool(&tool(n, d), &ctx))
            .collect()
    }

    fn shared_with(contributions: Vec<(u64, Vec<LoweredTool>)>) -> PublisherShared {
        let shared = PublisherShared {
            contributions: Mutex::new(BTreeMap::new()),
            next_id: AtomicU64::new(0),
            sync: tokio::sync::Mutex::new(()),
            catalog: shared_catalog(Vec::new()),
            describe: Mutex::new(None),
        };
        for (id, lowered) in contributions {
            shared.insert(
                id,
                Contribution {
                    lowered,
                    scope: OwnerScope::owner_only(id),
                },
            );
        }
        shared
    }

    #[test]
    fn capability_set_carries_a_tag_and_metadata_per_tool() {
        let caps = build_capability_set(&lowered(&[("echo", "echo it"), ("add", "sum")]));

        // Each tool's name is a discoverable tag.
        assert!(CapabilityFilter::new().require_tag("echo").matches(&caps));
        assert!(CapabilityFilter::new().require_tag("add").matches(&caps));

        // Bridge metadata + description ride under tool::<id>::<field>.
        assert_eq!(
            caps.metadata
                .get(&super::super::descriptor::compat_tier_key("echo"))
                .map(String::as_str),
            Some("mcp_bridge"),
        );
        assert_eq!(
            caps.metadata
                .get(&super::super::descriptor::credential_status_key("add"))
                .map(String::as_str),
            Some("unknown"),
        );
        assert_eq!(
            caps.metadata
                .get(&description_metadata_key("echo"))
                .map(String::as_str),
            Some("echo it"),
        );
    }

    #[test]
    fn empty_tool_list_yields_an_empty_capability_set() {
        let caps = build_capability_set(&[]);
        assert!(!CapabilityFilter::new().require_tag("echo").matches(&caps));
        // No tools ⇒ not advertised as a bridge provider either.
        assert!(!CapabilityFilter::new()
            .require_tag(BRIDGE_PROVIDER_TAG)
            .matches(&caps));
    }

    #[test]
    fn a_non_empty_set_advertises_the_bridge_provider_tag() {
        // Demand-side gateways find providers via this tag, then fetch each
        // one's catalog through the describe service.
        let caps = build_capability_set(&lowered(&[("echo", "echo it")]));
        assert!(CapabilityFilter::new()
            .require_tag(BRIDGE_PROVIDER_TAG)
            .matches(&caps));
    }

    #[test]
    fn merged_unions_every_live_publication() {
        // Two publications contribute distinct tools; the merged announcement
        // carries both publications' tags and the catalog has one scoped part
        // per publication — the mechanism behind "multiple publications per
        // process" over whole-node-set announcements.
        let shared = shared_with(vec![
            (0, lowered(&[("echo", "echo it")])),
            (1, lowered(&[("add", "sum")])),
        ]);
        let (caps, parts) = shared.merged().expect("merge");
        assert!(CapabilityFilter::new().require_tag("echo").matches(&caps));
        assert!(CapabilityFilter::new().require_tag("add").matches(&caps));
        assert!(CapabilityFilter::new()
            .require_tag(BRIDGE_PROVIDER_TAG)
            .matches(&caps));
        assert_eq!(parts.len(), 2, "one scoped catalog part per publication");
        assert_eq!(parts[0].catalog.tools[0].tool_id, "echo");
        assert_eq!(parts[1].catalog.tools[0].tool_id, "add");
        // Each part keeps its own publication's scope (distinct origins here).
        assert!(parts[0].scope.allows(0) && !parts[0].scope.allows(1));
        assert!(parts[1].scope.allows(1) && !parts[1].scope.allows(0));
    }

    #[test]
    fn removing_a_publication_removes_exactly_its_contribution() {
        // Withdraw semantics: after one publication de-registers, the merged
        // state is the other's alone — handle-scoped, no global withdraw.
        let shared = shared_with(vec![
            (0, lowered(&[("echo", "echo it")])),
            (1, lowered(&[("add", "sum")])),
        ]);
        shared.remove(0);
        let (caps, parts) = shared.merged().expect("merge");
        assert!(!CapabilityFilter::new().require_tag("echo").matches(&caps));
        assert!(CapabilityFilter::new().require_tag("add").matches(&caps));
        assert!(
            CapabilityFilter::new()
                .require_tag(BRIDGE_PROVIDER_TAG)
                .matches(&caps),
            "still a bridge provider while one publication lives",
        );
        assert_eq!(parts.len(), 1);

        // Removing the last publication empties the announcement entirely.
        shared.remove(1);
        let (caps, parts) = shared.merged().expect("merge");
        assert!(!CapabilityFilter::new()
            .require_tag(BRIDGE_PROVIDER_TAG)
            .matches(&caps));
        assert!(parts.is_empty());
    }

    #[test]
    fn swap_captures_the_prior_contribution_and_restore_reverts_it() {
        // The refresh() rollback primitive. swap() replaces a contribution and
        // hands back the prior one; restore() puts it back, or removes the
        // entry when there was none. This is what lets a failed refresh (a
        // sync_mesh error or a tool-id collision) revert the shared merged
        // state instead of leaving it advertising a tool set with no handler.
        let shared = shared_with(vec![(0, lowered(&[("echo", "echo it")]))]);

        // Swapping in a new tool set returns the prior contribution and the
        // merged view goes live with the new tools.
        let prior = shared.swap(
            0,
            Contribution {
                lowered: lowered(&[("write", "writes")]),
                scope: OwnerScope::owner_only(0),
            },
        );
        assert!(prior.is_some(), "swap returns the prior contribution");
        let (caps, _) = shared.merged().expect("merge");
        assert!(CapabilityFilter::new().require_tag("write").matches(&caps));
        assert!(!CapabilityFilter::new().require_tag("echo").matches(&caps));

        // Restoring the prior reverts to the pre-swap tool set exactly.
        shared.restore(0, prior);
        let (caps, parts) = shared.merged().expect("merge");
        assert!(CapabilityFilter::new().require_tag("echo").matches(&caps));
        assert!(!CapabilityFilter::new().require_tag("write").matches(&caps));
        assert_eq!(parts.len(), 1);

        // A publication that had no prior: restore(None) removes it entirely,
        // so a first-time refresh that fails leaves nothing advertised.
        let none_prior = shared.swap(
            9,
            Contribution {
                lowered: lowered(&[("temp", "temp")]),
                scope: OwnerScope::owner_only(9),
            },
        );
        assert!(none_prior.is_none());
        shared.restore(9, none_prior);
        assert!(shared.contributions.lock().get(&9).is_none());
    }

    #[test]
    fn rollback_result_surfaces_both_failures() {
        // A rollback that succeeds returns the original error unchanged.
        let folded = rollback_result(WrapError::Announce("boom".to_string()), Ok(()));
        assert!(matches!(folded, WrapError::Announce(ref m) if m == "boom"));

        // A rollback that ALSO fails is surfaced as RollbackFailed carrying
        // both, so a left-inconsistent mesh state is never hidden behind a
        // single error.
        let folded = rollback_result(
            WrapError::Announce("original boom".to_string()),
            Err(WrapError::Announce("rollback boom".to_string())),
        );
        match folded {
            WrapError::RollbackFailed { original, rollback } => {
                assert!(original.contains("original boom"), "{original}");
                assert!(rollback.contains("rollback boom"), "{rollback}");
            }
            other => panic!("expected RollbackFailed, got {other}"),
        }

        // The rendered message flags the stale-advertisement risk.
        let msg = WrapError::RollbackFailed {
            original: "x".to_string(),
            rollback: "y".to_string(),
        }
        .to_string();
        assert!(msg.contains("may still advertise stale tools"), "{msg}");
    }

    #[test]
    fn announced_metadata_carries_classification_not_secrets() {
        // Structural token-leak guard (doctrine #100). A wrapped server's
        // credentials go to the child process env (StdioMcpClient::spawn),
        // never into lowering/announce — so the announcement can only ever
        // carry the credential *classification* label, never a token.
        let ctx = LoweringContext {
            server_version: "1.0.0".to_string(),
            credential_status: super::super::CredentialStatus::Credentialed,
            substitutability: Substitutability::ProviderLocal,
        };
        let caps = build_capability_set(&[lower_tool(&tool("secretive", "uses a token"), &ctx)]);
        // The credential_status value is a fixed classification label.
        assert_eq!(
            caps.metadata
                .get(&super::super::descriptor::credential_status_key(
                    "secretive"
                ))
                .map(String::as_str),
            Some("credentialed"),
        );
        // No announced metadata value is anything but the small set of
        // classification labels + the human description — never a secret.
        for value in caps.metadata.values() {
            assert!(
                !value.contains("token") || value == "uses a token",
                "announced metadata must not carry secret-shaped values: {value:?}",
            );
        }
    }
}
