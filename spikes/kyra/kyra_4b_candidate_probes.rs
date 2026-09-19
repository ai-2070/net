#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn kyra_bootstrap_candidates_must_keep_the_native_size_bound() {
    use net::adapter::net::rtc::RtcSignalMsg;
    const MAX_SDP_BYTES: usize = 16 * 1024; // pinned source signal.rs:51; native decoder is the control
    let anchor = kyra_long_lived_anchor().await;
    let offerer = offerer().await;
    let sdp = offerer
        .rtc_driver()
        .unwrap()
        .create_offer()
        .await
        .unwrap()
        .1;
    anchor
        .accept_bootstrap_offer(offerer.node_id(), 71, sdp)
        .await
        .unwrap();
    let candidate = offerer.bootstrap_host_candidate().unwrap();
    let mid = "0".repeat(MAX_SDP_BYTES + 1);
    let native = RtcSignalMsg::Candidate {
        dialog: 71,
        candidate: candidate.clone(),
        mid: mid.clone(),
    };
    assert!(
        RtcSignalMsg::from_bytes(&native.to_bytes().unwrap()).is_err(),
        "native decoder must refuse this oversized frame"
    );
    let accepted = anchor
        .apply_bootstrap_candidate(offerer.node_id(), 71, candidate, mid)
        .await
        .is_ok();
    anchor.shutdown().await.unwrap();
    offerer.shutdown().await.unwrap();
    assert!(
        !accepted,
        "bootstrap candidate hook accepted a frame rejected by the native codec size bound"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn kyra_bootstrap_candidates_must_keep_the_native_frame_budget() {
    use net::adapter::net::rtc::{RtcSignalMsg, SignalAdmit, SignalBudget};
    const MAX_FRAMES_PER_WINDOW: u32 = 64; // pinned source signal.rs:41; real SignalBudget is the control
    let anchor = kyra_long_lived_anchor().await;
    let offerer = offerer().await;
    let sdp = offerer
        .rtc_driver()
        .unwrap()
        .create_offer()
        .await
        .unwrap()
        .1;
    anchor
        .accept_bootstrap_offer(offerer.node_id(), 72, sdp)
        .await
        .unwrap();
    let candidate = offerer.bootstrap_host_candidate().unwrap();
    let mut native = SignalBudget::new();
    let now = std::time::Instant::now();
    native.admit(
        offerer.node_id(),
        &RtcSignalMsg::Offer {
            dialog: 72,
            sdp: String::new(),
        },
        now,
    );
    let start = tokio::time::Instant::now();
    let mut bootstrap_ok = 0;
    let mut native_refused = 0;
    for _ in 0..=MAX_FRAMES_PER_WINDOW {
        let msg = RtcSignalMsg::Candidate {
            dialog: 72,
            candidate: candidate.clone(),
            mid: "0".into(),
        };
        if matches!(
            native.admit(offerer.node_id(), &msg, now),
            SignalAdmit::Refused(_)
        ) {
            native_refused += 1;
        }
        if anchor
            .apply_bootstrap_candidate(offerer.node_id(), 72, candidate.clone(), "0".into())
            .await
            .is_ok()
        {
            bootstrap_ok += 1;
        }
    }
    let elapsed = start.elapsed();
    anchor.shutdown().await.unwrap();
    offerer.shutdown().await.unwrap();
    assert!(
        elapsed < Duration::from_secs(10),
        "probe did not fit in one real budget window"
    );
    assert!(
        native_refused > 0,
        "native positive control must reach refusal"
    );
    eprintln!("kyra_candidate_budget: bootstrap_accepted={bootstrap_ok} native_refused={native_refused} elapsed_ms={}",elapsed.as_millis());
    assert!(
        bootstrap_ok < MAX_FRAMES_PER_WINDOW,
        "bootstrap hook applied every candidate beyond the shared64-frame limit"
    );
}
