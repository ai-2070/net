//! One ACTIVE subnet attachment per verifier (NET_CLI_PLAN_V3 decision 8).
//!
//! A verifier holds one admission context per peer: presenting a second
//! credential replaces the first. A device may still HOLD several subnet
//! relations at one verifier (its join's own and standalone memberships), so
//! exactly one of them is active there and only that one is presented. The
//! others stay stored (and renewed), and are reported as inactive.
//!
//! The choice is durable (`<state>/subnet.active.json`, verifier node →
//! scope):
//! - a relation becomes active by itself only when its verifier has no
//!   active attachment (the first relation there, or after the active one
//!   was left);
//! - replacing an active attachment is always explicit (`subnet join
//!   --switch`, `subnet activate <scope>`), so no supervisor ever flips one
//!   admission with another.
//!
//! This is not multi-attachment routing: holding credentials for several
//! scopes is distinct from being attached at several topology points.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The durable record, under the state root.
pub(crate) const ACTIVE_FILE: &str = "subnet.active.json";

/// Which scope is active at which verifier node.
#[derive(Debug)]
pub(crate) struct Attachments {
    path: PathBuf,
    by_verifier: BTreeMap<u64, String>,
}

pub(crate) type SharedAttachments = Arc<parking_lot::Mutex<Attachments>>;

impl Attachments {
    /// Load the record (none yet is empty; a corrupt one is an error, so a
    /// damaged choice never silently re-resolves to another attachment).
    pub(crate) fn load(state_root: &Path) -> Result<Self, String> {
        let path = state_root.join(ACTIVE_FILE);
        let by_verifier = match std::fs::read(&path) {
            Ok(bytes) => {
                let raw: BTreeMap<String, String> = serde_json::from_slice(&bytes)
                    .map_err(|e| format!("{}: {e}", path.display()))?;
                raw.into_iter()
                    .map(|(k, v)| {
                        u64::from_str_radix(k.trim_start_matches("0x"), 16)
                            .map(|node| (node, v))
                            .map_err(|_| format!("{}: bad verifier id {k}", path.display()))
                    })
                    .collect::<Result<_, _>>()?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        Ok(Self { path, by_verifier })
    }

    /// The active scope at `verifier`, if any.
    pub(crate) fn active(&self, verifier: u64) -> Option<&str> {
        self.by_verifier.get(&verifier).map(String::as_str)
    }

    /// Whether `scope` is the active attachment at `verifier`.
    pub(crate) fn is_active(&self, verifier: u64, scope: &str) -> bool {
        self.active(verifier) == Some(scope)
    }

    /// Make `scope` active at `verifier` (persisted before it takes effect).
    pub(crate) fn set(&mut self, verifier: u64, scope: &str) -> std::io::Result<()> {
        let previous = self.by_verifier.insert(verifier, scope.to_string());
        if let Err(e) = self.persist() {
            match previous {
                Some(p) => self.by_verifier.insert(verifier, p),
                None => self.by_verifier.remove(&verifier),
            };
            return Err(e);
        }
        Ok(())
    }

    /// Activate `scope` only if `verifier` has no active attachment. Returns
    /// whether `scope` is active afterwards.
    pub(crate) fn activate_if_vacant(
        &mut self,
        verifier: u64,
        scope: &str,
    ) -> std::io::Result<bool> {
        match self.active(verifier) {
            Some(active) => Ok(active == scope),
            None => self.set(verifier, scope).map(|()| true),
        }
    }

    /// Clear `verifier`'s entry only if it is `scope`. Returns whether it was.
    pub(crate) fn clear_if(&mut self, verifier: u64, scope: &str) -> std::io::Result<bool> {
        if !self.is_active(verifier, scope) {
            return Ok(false);
        }
        let removed = self.by_verifier.remove(&verifier);
        if let Err(e) = self.persist() {
            if let Some(r) = removed {
                self.by_verifier.insert(verifier, r);
            }
            return Err(e);
        }
        Ok(true)
    }

    fn persist(&self) -> std::io::Result<()> {
        let raw: BTreeMap<String, &String> = self
            .by_verifier
            .iter()
            .map(|(k, v)| (format!("0x{k:016x}"), v))
            .collect();
        let bytes = serde_json::to_vec_pretty(&raw).map_err(std::io::Error::other)?;
        let tmp = self.path.with_extension("tmp");
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, &self.path)
    }
}
