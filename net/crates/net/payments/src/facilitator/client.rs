//! Real-facilitator client — P1.
//!
//! In P1 this module implements [`crate::facilitator::Facilitator`]
//! against production x402 facilitators over HTTP (`POST /verify`,
//! `POST /settle`), with facilitator identity/endpoint recorded in every
//! result and structured retryable errors on degradation. Enabling a real
//! network is adapter/facilitator config + registry entries + conformance
//! runs — no new envelope types, no core changes ("config, not code" is
//! an acceptance criterion).
//!
//! Deliberately empty in P0: the mock facilitator exercises the same
//! trait, and P1 must require zero interface changes to land here.
