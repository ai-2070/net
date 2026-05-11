//! Dataforts Phase 3 — content-addressable blob storage.
//!
//! Wraps a customer-supplied storage backend (S3 / IPFS / FS /
//! custom) behind a uniform [`BlobAdapter`] trait so the substrate
//! can ship event payloads larger than the inline-threshold via a
//! [`BlobRef`] pointer + a separate fetch path. Phase 3 of
//! `docs/misc/DATAFORTS_PLAN.md`.
//!
//! The substrate owns hash verification (BLAKE3) and the
//! discriminator byte that distinguishes inline vs blob-ref event
//! payloads. Lifecycle (refcounts, GC, retention) is delegated to
//! the customer's backend — S3 lifecycle policies, IPFS pinning,
//! etc. — by explicit locked decision.

pub mod adapter;
pub mod blob_ref;
pub mod conformance;
pub mod error;
pub mod fs;
pub mod noop;
pub mod registry;

pub use adapter::BlobAdapter;
pub use blob_ref::{BlobRef, BLOB_REF_DISCRIMINATOR, BLOB_REF_VERSION_V1};
pub use conformance::run_conformance_suite;
pub use error::BlobError;
pub use fs::FileSystemAdapter;
pub use noop::NoopAdapter;
pub use registry::{global_blob_adapter_registry, BlobAdapterRegistry, BlobAdapterRegistryError};
