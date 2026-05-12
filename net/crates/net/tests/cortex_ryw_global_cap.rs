//! Integration test for the process-wide RYW in-flight cap.
//! Runs in its own test binary so the `OnceLock`-based global
//! doesn't interfere with the per-adapter unit tests.

#![cfg(all(feature = "net", feature = "cortex"))]

use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::channel::ChannelName;
use net::adapter::net::cortex::{
    set_global_ryw_inflight_cap, CortexAdapter, CortexAdapterConfig, WaitForTokenError,
};
use net::adapter::net::redex::{
    Redex, RedexError, RedexEvent, RedexFileConfig, RedexFold, WriteToken,
};

/// Trivial fold: count event appends. We only need an adapter
/// that runs the fold task; the test doesn't drive ingest.
struct CountFold;

impl RedexFold<u64> for CountFold {
    fn apply(&mut self, _ev: &RedexEvent, state: &mut u64) -> Result<(), RedexError> {
        *state += 1;
        Ok(())
    }
}

fn cn(s: &str) -> ChannelName {
    ChannelName::new(s).unwrap()
}

/// Install a global RYW cap of 2; open two adapters each with a
/// per-adapter cap of 10. The global cap dominates — the third
/// concurrent wait against either adapter fails with QueueFull
/// even though each adapter's local cap has plenty of headroom.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn global_ryw_cap_dominates_per_adapter_cap() {
    // Install the global cap at 2 (may already be installed by
    // another test in this binary — OnceLock; either way the cap
    // ends up at "first install wins").
    let _ = set_global_ryw_inflight_cap(2);

    let redex = Redex::new();
    let cfg = CortexAdapterConfig::default().with_ryw_inflight_cap(10);
    let adapter_a = Arc::new(
        CortexAdapter::<u64>::open(
            &redex,
            &cn("cortex/global-cap-a"),
            RedexFileConfig::default(),
            cfg,
            CountFold,
            0u64,
        )
        .unwrap(),
    );
    let adapter_b = Arc::new(
        CortexAdapter::<u64>::open(
            &redex,
            &cn("cortex/global-cap-b"),
            RedexFileConfig::default(),
            cfg,
            CountFold,
            0u64,
        )
        .unwrap(),
    );

    let token = WriteToken::new(0xDEAD_BEEF, 999);
    // Two waiters take the two global permits.
    let a_handle = {
        let adapter_a = adapter_a.clone();
        tokio::spawn(async move {
            let _ = adapter_a
                .wait_for_token(token, Duration::from_secs(2))
                .await;
        })
    };
    let b_handle = {
        let adapter_b = adapter_b.clone();
        tokio::spawn(async move {
            let _ = adapter_b
                .wait_for_token(token, Duration::from_secs(2))
                .await;
        })
    };
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Third waiter — either adapter — must reject on the global cap.
    let third = adapter_a
        .wait_for_token(token, Duration::from_secs(1))
        .await;
    assert_eq!(third.unwrap_err(), WaitForTokenError::QueueFull);

    a_handle.await.unwrap();
    b_handle.await.unwrap();
}

/// Calling `set_global_ryw_inflight_cap` a second time is a no-op;
/// the first install wins. Pins the OnceLock contract so operators
/// can't quietly raise the cap after the fact.
#[tokio::test]
async fn set_global_ryw_cap_is_one_shot() {
    let _ = set_global_ryw_inflight_cap(2);
    let second = set_global_ryw_inflight_cap(1000);
    assert!(!second, "second install must return false");
}
