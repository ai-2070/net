//! A [`ChainChecker`] with a scripted verdict queue.
//!
//! Shared because two suites need one and they were maintained as two
//! copies: `checker_verification.rs` (which drives the tier/invalidation
//! matrix) and `lifecycle_modes.rs` (which needs a checker only to prove
//! that a tier above `observed` can come from nowhere else). Integration
//! tests are separate crates, so the only way to share is a module like
//! this — the same shape `rpc_fixture/` already uses.
//!
//! Records every query it was asked, because several tests assert on what
//! the engine *threads through* (the payer bind, the EIP-3009 nonce
//! reference, the destination tag) rather than only on the verdict.

#![allow(dead_code)] // each suite uses a different subset

use async_trait::async_trait;
use net_payments::checker::{ChainChecker, ChainVerdict, CheckerError, TransferQuery};
use net_payments::core::verification::VerifierRef;

pub struct ScriptedChecker {
    verdicts: parking_lot::Mutex<std::collections::VecDeque<ChainVerdict>>,
    pub queries: parking_lot::Mutex<Vec<(String, String, Option<TransferQuery>)>>,
}

impl ScriptedChecker {
    /// Verdicts are consumed **front to back**, so the vec reads in the
    /// order the checker will be asked.
    pub fn new(verdicts: Vec<ChainVerdict>) -> Self {
        Self {
            verdicts: parking_lot::Mutex::new(verdicts.into()),
            queries: parking_lot::Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl ChainChecker for ScriptedChecker {
    fn reference(&self) -> VerifierRef {
        VerifierRef {
            identity: None,
            endpoint: "independent-chain-check:scripted".into(),
        }
    }

    async fn check(
        &self,
        network: &str,
        transaction: &str,
        query: Option<&TransferQuery>,
    ) -> Result<ChainVerdict, CheckerError> {
        self.queries
            .lock()
            .push((network.to_string(), transaction.to_string(), query.cloned()));
        // Running out is a test-authoring mistake, not a chain condition —
        // retryable so it surfaces as a distinct outcome rather than being
        // mistaken for a real terminal verdict.
        self.verdicts
            .lock()
            .pop_front()
            .ok_or_else(|| CheckerError::retryable("scripted checker: verdicts exhausted"))
    }
}
