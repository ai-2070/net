//! OSDK §1 — the consumer-audience **lease**: reference-counted, ownership-safe
//! installation of grant audiences on behalf of bound clients.
//!
//! # Why a lease and not a plain install
//!
//! Binding credentials must install each DISCOVER grant's audience into the
//! node's consumer registry, or private discovery has nothing to ingest. But an
//! install that outlives the credentials that justified it is ambient
//! decryption authority: the client is gone, its secrets dropped, and the node
//! still opens envelopes for that grant. So installed ingest authority is kept
//! in exact correspondence with live credential possession:
//!
//! ```text
//! first client using grant G   → install, remember the ownership token
//! additional client using G    → refcount++ (no second install)
//! client clone                 → shares its client's lease
//! last such client drops       → remove — but ONLY the installation we own
//! ```
//!
//! # Ownership safety
//!
//! Removing by grant id alone is unsafe: between install and removal, the
//! low-level operator API may have removed the record and installed a DIFFERENT
//! grant under the same id, and a bare remove would destroy an installation
//! this lease never owned. Two defenses, together:
//!
//! - if the core reports [`ConsumerAudienceInstall::AlreadyPresent`], someone
//!   else installed the record first — the entry is marked NON-OWNING and its
//!   final release removes nothing;
//! - an owning entry removes via `remove_consumer_grant_audience_if_current`,
//!   which compares the installation stamp and removes under the registry mutex,
//!   so a stale token (already replaced, already removed) removes nothing.
//!
//! The SDK-side mutex is held across the whole 0→1 install and 1→0 removal, not
//! merely the counter mutation, so a final release racing a fresh bind cannot
//! leave the new client without its audience.

use std::collections::HashMap;
use std::sync::Arc;

use net::adapter::net::behavior::org_grant_registry::{
    ConsumerAudienceInstall, ConsumerAudienceLease,
};
use net::adapter::net::MeshNode;

use super::types::{GrantAudienceInstallError, OrgAudienceSecret, OrgCapabilityGrant};

/// One grant id's shared installation state.
struct LeaseEntry {
    /// How many live [`AudienceLeaseGuard`]s reference this grant id.
    count: usize,
    /// `Some` iff THIS registry performed the install and may remove it.
    /// `None` when the record was already present (installed by low-level code
    /// or another owner) — release must then remove nothing.
    owned: Option<ConsumerAudienceLease>,
}

/// Per-mesh registry of consumer-audience leases. Lives on
/// [`Mesh`](crate::mesh::Mesh) so every client bound to the same mesh shares
/// one refcount per grant id.
#[derive(Default)]
pub(crate) struct OrgAudienceLeases {
    entries: parking_lot::Mutex<HashMap<[u8; 32], LeaseEntry>>,
}

impl OrgAudienceLeases {
    /// Acquire references for `pairs` (grant + its out-of-band secret),
    /// installing any not already leased. All-or-nothing: if any install is
    /// refused, every reference taken so far in this call is released before
    /// returning, so a failed bind leaves the registry exactly as it found it.
    ///
    /// Returns the grant ids now referenced by the caller.
    pub(crate) fn acquire(
        self: &Arc<Self>,
        node: &Arc<MeshNode>,
        pairs: Vec<(OrgCapabilityGrant, OrgAudienceSecret)>,
    ) -> Result<Vec<[u8; 32]>, ([u8; 32], GrantAudienceInstallError)> {
        let mut taken: Vec<[u8; 32]> = Vec::with_capacity(pairs.len());
        for (grant, secret) in pairs {
            let grant_id = grant.grant_id;
            // The mutex spans the "is it already leased?" read AND the install,
            // so two concurrent binds cannot both decide they are first.
            let mut entries = self.entries.lock();
            match entries.get_mut(&grant_id) {
                Some(entry) => {
                    entry.count += 1;
                    // `secret` drops here (zeroized): the record it would have
                    // installed is already live and identical by construction —
                    // a differing secret for the same grant id would have been
                    // a Conflict at the first install.
                }
                None => match node.install_consumer_grant_audience_leased(grant, secret) {
                    Ok(ConsumerAudienceInstall::Installed(lease)) => {
                        entries.insert(
                            grant_id,
                            LeaseEntry {
                                count: 1,
                                owned: Some(lease),
                            },
                        );
                    }
                    Ok(ConsumerAudienceInstall::AlreadyPresent) => {
                        entries.insert(
                            grant_id,
                            LeaseEntry {
                                count: 1,
                                owned: None,
                            },
                        );
                    }
                    Err(e) => {
                        drop(entries);
                        self.release(node, &taken);
                        return Err((grant_id, e));
                    }
                },
            }
            drop(entries);
            taken.push(grant_id);
        }
        Ok(taken)
    }

    /// Release one reference per id; the last reference to an OWNED
    /// installation removes it.
    pub(crate) fn release(&self, node: &Arc<MeshNode>, grant_ids: &[[u8; 32]]) {
        for grant_id in grant_ids {
            let mut entries = self.entries.lock();
            let Some(entry) = entries.get_mut(grant_id) else {
                continue;
            };
            entry.count = entry.count.saturating_sub(1);
            if entry.count > 0 {
                continue;
            }
            let removed = entries.remove(grant_id);
            // Still under the lock: the 1→0 transition and the registry removal
            // are one step, so a bind arriving now either sees the entry gone
            // (and re-installs) or waits — never inherits a half-released one.
            if let Some(LeaseEntry {
                owned: Some(lease), ..
            }) = removed
            {
                node.remove_consumer_grant_audience_if_current(&lease);
            }
        }
    }

    /// Test seam: how many grant ids this registry currently references.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.lock().len()
    }

    /// Test seam: the refcount for one grant id, and whether the entry owns its
    /// installation.
    #[cfg(test)]
    pub(crate) fn entry_for_test(&self, grant_id: &[u8; 32]) -> Option<(usize, bool)> {
        self.entries
            .lock()
            .get(grant_id)
            .map(|e| (e.count, e.owned.is_some()))
    }
}

/// RAII handle: releases its references when the last clone drops.
///
/// Held inside an `Arc` by [`OrgClient`](super::OrgClient), so cloning a client
/// shares one guard rather than taking a second reference.
pub(crate) struct AudienceLeaseGuard {
    node: Arc<MeshNode>,
    leases: Arc<OrgAudienceLeases>,
    grant_ids: Vec<[u8; 32]>,
}

impl AudienceLeaseGuard {
    pub(crate) fn new(
        node: Arc<MeshNode>,
        leases: Arc<OrgAudienceLeases>,
        grant_ids: Vec<[u8; 32]>,
    ) -> Self {
        Self {
            node,
            leases,
            grant_ids,
        }
    }
}

impl Drop for AudienceLeaseGuard {
    fn drop(&mut self) {
        self.leases.release(&self.node, &self.grant_ids);
    }
}

impl std::fmt::Debug for AudienceLeaseGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudienceLeaseGuard")
            .field("grants", &self.grant_ids.len())
            .finish()
    }
}
