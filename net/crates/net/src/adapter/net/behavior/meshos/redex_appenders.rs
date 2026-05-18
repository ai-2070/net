//! Production RedEX-backed chain appenders.
//!
//! Thin adapters from the substrate's three chain-seam traits
//! ([`super::audit_chain::AdminAuditChainAppender`],
//! [`super::log_chain::LogChainAppender`],
//! [`super::failure_chain::FailureChainAppender`]) to
//! [`TypedRedexFile<T>`]. Each adapter holds a `TypedRedexFile`
//! and delegates `append` to it; postcard serialization
//! happens inside the typed file, so the substrate side passes
//! `&T` directly.
//!
//! # Wiring
//!
//! ```ignore
//! use net::adapter::net::behavior::meshos::{
//!     AdminAuditRecord, LogRecord, MeshOsRuntime,
//!     redex_appenders::{RedexAdminAuditAppender, RedexLogAppender,
//!                       RedexFailureAppender},
//! };
//! use net::adapter::net::redex::{Redex, RedexFileConfig};
//! use std::sync::Arc;
//!
//! let redex = Redex::new();
//! let audit_file: RedexFile = redex.open_file(&"admin-audit".into(), config)?;
//! let log_file:   RedexFile = redex.open_file(&"log".into(),         config)?;
//! let fail_file:  RedexFile = redex.open_file(&"failures".into(),    config)?;
//!
//! let audit = Arc::new(RedexAdminAuditAppender::new(audit_file));
//! let log   = Arc::new(RedexLogAppender::new(log_file));
//! let fail  = Arc::new(RedexFailureAppender::new(fail_file));
//!
//! let runtime = MeshOsRuntime::start_with_all_chains(
//!     config, dispatcher, probes, scheduler, daemon_registry,
//!     /* control_sink */ None,
//!     /* admin_verifier */ None,
//!     Some(audit), Some(log), Some(fail),
//! );
//! ```
//!
//! Each adapter is `Send + Sync + 'static` — `TypedRedexFile`
//! is internally an `Arc`-cloned handle, safe to share across
//! the loop's task and the executor's task.

use super::audit_chain::{AdminAuditAppendError, AdminAuditChainAppender};
use super::failure_chain::{FailureAppendError, FailureChainAppender};
use super::ice::AdminAuditRecord;
use super::log_chain::{LogAppendError, LogChainAppender};
use super::logs::LogRecord;
use super::snapshot::FailureRecord;
use crate::adapter::net::redex::TypedRedexFile;

/// Production [`AdminAuditChainAppender`] backed by a
/// [`TypedRedexFile<AdminAuditRecord>`]. Construct once at
/// runtime startup; clone the [`std::sync::Arc`] wrapping it
/// for every consumer that needs the appender.
pub struct RedexAdminAuditAppender {
    file: TypedRedexFile<AdminAuditRecord>,
}

impl RedexAdminAuditAppender {
    /// Wrap a [`crate::adapter::net::redex::RedexFile`] opened
    /// against the cluster's `admin-audit` chain.
    pub fn new(file: crate::adapter::net::redex::RedexFile) -> Self {
        Self {
            file: TypedRedexFile::new(file),
        }
    }
}

impl AdminAuditChainAppender for RedexAdminAuditAppender {
    fn append(&self, record: &AdminAuditRecord) -> Result<(), AdminAuditAppendError> {
        self.file
            .append(record)
            .map_err(|e| AdminAuditAppendError {
                reason: e.to_string(),
            })?;
        Ok(())
    }
}

/// Production [`LogChainAppender`] backed by a
/// [`TypedRedexFile<LogRecord>`].
pub struct RedexLogAppender {
    file: TypedRedexFile<LogRecord>,
}

impl RedexLogAppender {
    /// Wrap a [`crate::adapter::net::redex::RedexFile`] opened
    /// against the cluster's `log` chain.
    pub fn new(file: crate::adapter::net::redex::RedexFile) -> Self {
        Self {
            file: TypedRedexFile::new(file),
        }
    }
}

impl LogChainAppender for RedexLogAppender {
    fn append(&self, record: &LogRecord) -> Result<(), LogAppendError> {
        self.file.append(record).map_err(|e| LogAppendError {
            reason: e.to_string(),
        })?;
        Ok(())
    }
}

/// Production [`FailureChainAppender`] backed by a
/// [`TypedRedexFile<FailureRecord>`].
pub struct RedexFailureAppender {
    file: TypedRedexFile<FailureRecord>,
}

impl RedexFailureAppender {
    /// Wrap a [`crate::adapter::net::redex::RedexFile`] opened
    /// against the cluster's `failures` chain.
    pub fn new(file: crate::adapter::net::redex::RedexFile) -> Self {
        Self {
            file: TypedRedexFile::new(file),
        }
    }
}

impl FailureChainAppender for RedexFailureAppender {
    fn append(&self, record: &FailureRecord) -> Result<(), FailureAppendError> {
        self.file.append(record).map_err(|e| FailureAppendError {
            reason: e.to_string(),
        })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::event::AdminEvent;
    use super::super::ice::VerificationOutcome;
    use super::super::logs::LogLevel;
    use super::*;
    use crate::adapter::net::channel::ChannelName;
    use crate::adapter::net::redex::{Redex, RedexFile, RedexFileConfig};
    use std::time::Duration;

    fn fresh_redex_file(name: &str) -> RedexFile {
        let redex = Redex::new();
        let cn = ChannelName::new(name).expect("valid channel name");
        redex
            .open_file(&cn, RedexFileConfig::default())
            .expect("open in-memory redex file")
    }

    #[test]
    fn admin_audit_appender_writes_record_through_typed_file() {
        let file = fresh_redex_file("admin-audit-test");
        // Hold a second handle so we can read back what the
        // appender wrote.
        let reader = TypedRedexFile::<AdminAuditRecord>::new(file.clone());
        let appender = RedexAdminAuditAppender::new(file);

        let record = AdminAuditRecord {
            seq: 1,
            committed_at_ms: 1_700_000_000_000,
            event: AdminEvent::FreezeCluster {
                ttl: Duration::from_secs(30),
            },
            operator_ids: vec![7],
            outcome: VerificationOutcome::Accepted,
            chain_pending: false,
        };
        appender.append(&record).expect("append");

        let read = reader.file().read_range(0, 1);
        assert_eq!(read.len(), 1);
        let decoded: AdminAuditRecord = postcard::from_bytes(&read[0].payload).expect("decode");
        assert_eq!(decoded, record);
    }

    #[test]
    fn log_appender_writes_record_through_typed_file() {
        let file = fresh_redex_file("log-test");
        let reader = TypedRedexFile::<LogRecord>::new(file.clone());
        let appender = RedexLogAppender::new(file);

        let record = LogRecord {
            seq: 5,
            ts_ms: 1_700_000_000_000,
            level: LogLevel::Warn,
            daemon_id: Some(7),
            node_id: Some(100),
            message: "throttling".into(),
            chain_pending: false,
        };
        appender.append(&record).expect("append");

        let read = reader.file().read_range(0, 1);
        assert_eq!(read.len(), 1);
        let decoded: LogRecord = postcard::from_bytes(&read[0].payload).expect("decode");
        assert_eq!(decoded, record);
    }

    #[test]
    fn failure_appender_writes_record_through_typed_file() {
        let file = fresh_redex_file("failures-test");
        let reader = TypedRedexFile::<FailureRecord>::new(file.clone());
        let appender = RedexFailureAppender::new(file);

        let record = FailureRecord {
            seq: 2,
            source: "daemon:telemetry".into(),
            reason: "drain timeout".into(),
            recorded_at_ms: 1_700_000_000_000,
        };
        appender.append(&record).expect("append");

        let read = reader.file().read_range(0, 1);
        assert_eq!(read.len(), 1);
        let decoded: FailureRecord = postcard::from_bytes(&read[0].payload).expect("decode");
        assert_eq!(decoded, record);
    }
}
