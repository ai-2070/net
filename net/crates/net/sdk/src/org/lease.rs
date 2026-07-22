//! OSDK §1 — the consumer-audience **lease guard**.
//!
//! The reference-counted registry itself lives on the NODE
//! (`MeshNode::acquire_consumer_audience_leases`), because it guards the node's
//! consumer-audience registry and `Mesh::from_node_arc` is public — two `Mesh`
//! wrappers over one node must share the refcount or one can withdraw an
//! audience the other is still using. This module is the RAII sugar that
//! releases on last-clone drop.
//!
//! # Why a lease at all
//!
//! Binding credentials must install each DISCOVER grant's audience into the
//! node's consumer registry, or private discovery has nothing to ingest. But an
//! install that outlives the credentials that justified it is ambient
//! decryption authority: the client is gone, its secrets dropped, and the node
//! still opens envelopes for that grant. So installed ingest authority is kept
//! in exact correspondence with live credential possession.

use std::sync::Arc;

use net::adapter::net::MeshNode;

/// RAII handle: releases its references when the last clone drops.
///
/// Held inside an `Arc` by [`OrgClient`](super::OrgClient), so cloning a client
/// shares one guard rather than taking a second reference.
pub(crate) struct AudienceLeaseGuard {
    node: Arc<MeshNode>,
    grant_ids: Vec<[u8; 32]>,
}

impl AudienceLeaseGuard {
    pub(crate) fn new(node: Arc<MeshNode>, grant_ids: Vec<[u8; 32]>) -> Self {
        Self { node, grant_ids }
    }
}

impl Drop for AudienceLeaseGuard {
    fn drop(&mut self) {
        self.node.release_consumer_audience_leases(&self.grant_ids);
    }
}

impl std::fmt::Debug for AudienceLeaseGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudienceLeaseGuard")
            .field("grants", &self.grant_ids.len())
            .finish()
    }
}
