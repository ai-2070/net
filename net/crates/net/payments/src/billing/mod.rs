//! The SDK billing surface (Workstream 5): stream + export.
//!
//! **A billing event is a signed technical record linking invocation,
//! quote, settlement verification, and amount — input to accounting
//! systems, never an accounting artifact itself.** (Verbatim by doctrine;
//! this sentence also lives on [`crate::core::billing_event`].) Never an
//! invoice, receipt, or tax artifact — partners and customers turn the
//! stream into those under their own policy and posture. Net ships zero
//! dashboards and zero reports.
//!
//! The surface is two things and deliberately nothing more:
//!
//! - **Subscribe/watch**: an in-process stream of billing events as the
//!   engine emits them ([`BillingLog::subscribe`]).
//! - **JSONL export**: the log *is* canonical JSONL — one canonical-bytes
//!   envelope per line, signatures intact — and
//!   [`BillingLog::export_jsonl`] copies verified lines to a destination.
//!   Consumers verify signatures and hold protocol facts, not a
//!   notification rendering.
//!
//! Appends are cross-process safe (the locked-store sidecar lock) and the
//! log is append-only: billing events are immutable; adjustments arrive
//! as *new* events referencing old ones, never rewrites.

use std::path::{Path, PathBuf};

use tokio::io::AsyncWriteExt as _;
use tokio::sync::broadcast;

use crate::core::billing_event::BillingEvent;
use crate::core::canonical::canonical_bytes;
use crate::policy::store::LockGuard;

/// Errors from the billing log.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum BillingError {
    #[error("billing log I/O error at {path}: {reason}")]
    Io { path: String, reason: String },
    #[error("billing log at {path} holds an invalid record (line {line}): {reason}")]
    BadRecord {
        path: String,
        line: usize,
        reason: String,
    },
}

impl BillingError {
    fn io(path: &Path, e: impl std::fmt::Display) -> Self {
        Self::Io {
            path: path.display().to_string(),
            reason: e.to_string(),
        }
    }
}

/// The append-only billing event log + in-process stream.
pub struct BillingLog {
    path: PathBuf,
    tx: broadcast::Sender<BillingEvent>,
}

impl BillingLog {
    /// Open (or start) a log at `path`. The file appears on first append.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        // Capacity bounds slow-subscriber memory; a lagging subscriber
        // gets `RecvError::Lagged` and recovers by re-reading the log —
        // the file is the truth, the stream is a projection of it.
        let (tx, _) = broadcast::channel(1024);
        Self {
            path: path.into(),
            tx,
        }
    }

    /// Where the JSONL log lives.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Watch billing events as they are emitted (in-process). Missed
    /// deliveries are recoverable by re-reading the log; never treat the
    /// stream as the record.
    pub fn subscribe(&self) -> broadcast::Receiver<BillingEvent> {
        self.tx.subscribe()
    }

    /// Append one signed billing event: one canonical-bytes line, written
    /// under the cross-process lock, fsync'd, then published to
    /// subscribers.
    pub async fn append(&self, event: &BillingEvent) -> Result<(), BillingError> {
        let mut line = canonical_bytes(event).map_err(|e| BillingError::BadRecord {
            path: self.path.display().to_string(),
            line: 0,
            reason: e.to_string(),
        })?;
        line.push(b'\n');

        let _guard = LockGuard::acquire(&self.path)
            .await
            .map_err(|e| BillingError::io(&self.path, e))?;
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|e| BillingError::io(&self.path, e))?;
            }
        }
        let mut opts = tokio::fs::OpenOptions::new();
        opts.append(true).create(true);
        #[cfg(unix)]
        {
            // `mode` is inherent on tokio's OpenOptions (no trait import).
            opts.mode(0o600);
        }
        let mut file = opts
            .open(&self.path)
            .await
            .map_err(|e| BillingError::io(&self.path, e))?;
        file.write_all(&line)
            .await
            .map_err(|e| BillingError::io(&self.path, e))?;
        file.sync_all()
            .await
            .map_err(|e| BillingError::io(&self.path, e))?;
        drop(file);

        // Publish after the durable write; a send error only means no
        // subscribers right now, which is fine — the log is the record.
        let _ = self.tx.send(event.clone());
        Ok(())
    }

    /// Read the whole log, verifying every record's tag, id derivation,
    /// and signature. A bad line is a loud error, never skipped.
    pub async fn read_all(&self) -> Result<Vec<BillingEvent>, BillingError> {
        let raw = match tokio::fs::read(&self.path).await {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(BillingError::io(&self.path, e)),
        };
        let mut events = Vec::new();
        for (i, line) in raw.split(|b| *b == b'\n').enumerate() {
            if line.is_empty() {
                continue;
            }
            let event =
                BillingEvent::from_json_bytes(line).map_err(|e| BillingError::BadRecord {
                    path: self.path.display().to_string(),
                    line: i + 1,
                    reason: e.to_string(),
                })?;
            events.push(event);
        }
        // One charge per billing_event_id. A billing event that reaches the
        // log more than once — a re-publish after a lost append, or a crash
        // between the durable append and the engine's published-mark —
        // would otherwise double-count. The id is content-derived from the
        // idempotency scope, so first-occurrence-wins is exact.
        let mut seen = std::collections::HashSet::new();
        events.retain(|e| seen.insert(e.billing_event_id.clone()));
        Ok(events)
    }

    /// Export the verified log as JSONL to `dest`; returns the record
    /// count. The export re-emits canonical bytes, so a downstream
    /// consumer can verify each line independently of this process.
    pub async fn export_jsonl(&self, dest: &Path) -> Result<usize, BillingError> {
        let events = self.read_all().await?;
        let mut out = Vec::new();
        for event in &events {
            let bytes = canonical_bytes(event).map_err(|e| BillingError::BadRecord {
                path: self.path.display().to_string(),
                line: 0,
                reason: e.to_string(),
            })?;
            out.extend_from_slice(&bytes);
            out.push(b'\n');
        }
        tokio::fs::write(dest, out)
            .await
            .map_err(|e| BillingError::io(dest, e))?;
        Ok(events.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::canonical::{ExtraFields, SignedEnvelope as _};
    use crate::core::idempotency::IdempotencyScope;
    use crate::core::units::AtomicAmount;
    use crate::core::versioning::TAG_BILLING_EVENT;
    use net::adapter::net::identity::EntityKeypair;

    fn signed_event(kp: &EntityKeypair, quote_id: &str) -> BillingEvent {
        let payer = EntityKeypair::from_bytes([9u8; 32]).entity_id().clone();
        let scope = IdempotencyScope {
            caller: payer.clone(),
            provider: kp.entity_id().clone(),
            capability: "prov/tool".into(),
            quote_id: quote_id.to_string(),
        };
        let idem = scope.key();
        let mut ev = BillingEvent {
            object: TAG_BILLING_EVENT.to_string(),
            billing_event_id: BillingEvent::derive_id(&idem),
            idempotency_key: idem,
            capability: "prov/tool".into(),
            invocation_id: None,
            quote_id: quote_id.to_string(),
            transaction: Some("0xabc".into()),
            verification_ref: None,
            payer,
            payee: kp.entity_id().clone(),
            network: "mock:net".into(),
            asset: "musd".into(),
            amount: AtomicAmount::from_u128(2_500),
            occurred_at_ns: 42,
            signature: None,
            extra: ExtraFields::new(),
        };
        ev.sign_with(kp).unwrap();
        ev
    }

    /// M4 safety net: a billing event appended twice (a re-publish after a
    /// lost append) is one charge to every reader.
    #[tokio::test]
    async fn read_all_dedups_a_duplicated_append_by_event_id() {
        let dir = tempfile::tempdir().unwrap();
        let log = BillingLog::new(dir.path().join("b.jsonl"));
        let kp = EntityKeypair::generate();
        let ev = signed_event(&kp, "q1");

        log.append(&ev).await.unwrap();
        log.append(&ev).await.unwrap();

        let events = log.read_all().await.unwrap();
        assert_eq!(events.len(), 1, "one charge per billing_event_id");
        assert_eq!(events[0].billing_event_id, ev.billing_event_id);
    }
}
