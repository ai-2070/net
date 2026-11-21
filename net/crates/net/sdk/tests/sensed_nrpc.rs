//! S2 — sensed nRPC, live (sensing plan §6 nRPC witnesses 18–24;
//! SENSING_OLB_COMPLETION_PLAN B.2).
//!
//! One consumer and three providers of one public service, over real
//! handshakes. Each provider's handler records that it ran. The readiness
//! capture is built with the fixtures-only `SensingSnapshot::for_test`, so the
//! witnesses can name the exact ordering they assert. How a real capture is
//! produced is the consumer SDK's own contract, witnessed in
//! `sensing_consumer.rs`. What these add is the observable boundary of the
//! sensed CALL: the response comes from the provider the capture selects, the
//! request ran exactly once, and authorization always ran first.
//!
//! The decision table itself (rank order, the unverdicted pool, the
//! all-not-ready fallback) is witnessed as a pure function in core,
//! `mesh_rpc::sensed_service_target_tests`.

#![cfg(all(feature = "net", feature = "cortex", feature = "fixtures"))]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use net_sdk::capabilities::CapabilitySet;
use net_sdk::mesh::{Mesh, MeshBuilder};
use net_sdk::mesh_rpc::{CallOptions, RoutingPolicy, RpcError};
use net_sdk::sensing::{SensedViability, SensingSnapshot};

const PSK: [u8; 32] = [0x6e; 32];
const SERVICE: &str = "sensed.echo";

async fn mesh() -> Mesh {
    MeshBuilder::new("127.0.0.1:0", &PSK)
        .unwrap()
        .build()
        .await
        .unwrap()
}

/// `consumer` dials `provider`. Neither is started yet.
async fn link(consumer: &Mesh, provider: &Mesh) {
    let addr = provider.inner().local_addr();
    let public = *provider.inner().public_key();
    let provider_id = provider.inner().node_id();
    let consumer_id = consumer.inner().node_id();
    let (accepted, connected) = tokio::join!(provider.inner().accept(consumer_id), async {
        tokio::time::sleep(Duration::from_millis(50)).await;
        consumer.inner().connect(addr, &public, provider_id).await
    });
    accepted.expect("accept");
    connected.expect("connect");
}

struct Cell {
    consumer: Mesh,
    providers: Vec<Mesh>,
    /// How many times each provider's handler ran, in `ids` order.
    ran: Vec<Arc<AtomicUsize>>,
    /// Provider node ids, ascending (the deterministic candidate order).
    ids: Vec<u64>,
    _serves: Vec<net_sdk::mesh_rpc::ServeHandle>,
}

impl Cell {
    /// Three providers; the one at index `fails` (if any) answers every
    /// request with a handler error after recording that it ran.
    async fn stand_up(fails: Option<usize>) -> Self {
        let consumer = mesh().await;
        let mut providers = Vec::new();
        for _ in 0..3 {
            let provider = mesh().await;
            link(&consumer, &provider).await;
            providers.push(provider);
        }
        consumer.start().expect("consumer start");
        for provider in &providers {
            provider.start().expect("provider start");
        }
        providers.sort_by_key(|p| p.inner().node_id());

        let mut ran = Vec::new();
        let mut serves = Vec::new();
        for (index, provider) in providers.iter().enumerate() {
            let counter = Arc::new(AtomicUsize::new(0));
            let seen = counter.clone();
            let id = provider.inner().node_id();
            let failing = fails == Some(index);
            serves.push(
                provider
                    .serve_rpc_typed(SERVICE, net_sdk::mesh_rpc::Codec::Json, move |_: u32| {
                        let seen = seen.clone();
                        async move {
                            seen.fetch_add(1, Ordering::SeqCst);
                            if failing {
                                Err("refused by the handler".to_string())
                            } else {
                                Ok(id)
                            }
                        }
                    })
                    .expect("serve"),
            );
            provider
                .inner()
                .announce_capabilities(CapabilitySet::new())
                .await
                .expect("announce");
            ran.push(counter);
        }
        let ids: Vec<u64> = providers.iter().map(|p| p.inner().node_id()).collect();

        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            let mut found = consumer.find_service_nodes(SERVICE);
            found.sort_unstable();
            if found == ids {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the consumer never discovered all three providers: {found:?} vs {ids:?}"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        Self {
            consumer,
            providers,
            ran,
            ids,
            _serves: serves,
        }
    }

    fn ran(&self) -> Vec<usize> {
        self.ran.iter().map(|c| c.load(Ordering::SeqCst)).collect()
    }

    fn snapshot(&self, rows: &[(usize, SensedViability)], ranked: &[usize]) -> SensingSnapshot {
        let rows: Vec<(u64, SensedViability)> =
            rows.iter().map(|&(i, v)| (self.ids[i], v)).collect();
        let ranked: Vec<u64> = ranked.iter().map(|&i| self.ids[i]).collect();
        SensingSnapshot::for_test(&format!("nrpc:{SERVICE}"), &rows, &ranked)
    }

    /// One sensed call; returns the node id that answered.
    async fn call(&self, readiness: &SensingSnapshot) -> Result<u64, RpcError> {
        let body = Bytes::from(serde_json::to_vec(&0u32).unwrap());
        let reply = self
            .consumer
            .call_service_sensed(SERVICE, body, opts(), readiness)
            .await?;
        Ok(serde_json::from_slice(&reply.body).expect("decode"))
    }
}

fn opts() -> CallOptions {
    CallOptions {
        // Round-robin, so a test that forgets the sensed preference would
        // land on a DIFFERENT provider across calls and be caught.
        routing_policy: RoutingPolicy::RoundRobin,
        ..Default::default()
    }
}

/// §6 #18 and #19: P1 viable/preferred, P2 viable/second, P3 not-ready →
/// P1, on every call despite round-robin; when P1 turns not-ready, the next
/// call goes to P2. Each call ran exactly one handler.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_call_reaches_the_best_ranked_viable_provider_and_follows_it() {
    use SensedViability::*;
    let cell = Cell::stand_up(None).await;
    let preferred = cell.snapshot(&[(1, Viable), (0, Viable), (2, NotViable)], &[1, 0]);

    for _ in 0..3 {
        assert_eq!(cell.call(&preferred).await.expect("admitted"), cell.ids[1]);
    }
    assert_eq!(cell.ran(), vec![0, 3, 0], "only the preferred provider ran");

    let moved = cell.snapshot(&[(1, NotViable), (0, Viable), (2, NotViable)], &[0]);
    assert_eq!(cell.call(&moved).await.expect("admitted"), cell.ids[0]);
    assert_eq!(cell.ran(), vec![1, 3, 0]);
}

/// §6 #20 and the D7.2 rule: with nothing viable, a provider the capture has
/// no verdict for still gets the call, ahead of the not-ready ones. With
/// every provider not-ready, the call still goes out, to one of them, once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unknown_providers_stay_eligible_and_not_ready_is_never_a_refusal() {
    use SensedViability::*;
    let cell = Cell::stand_up(None).await;

    let unknown_only = cell.snapshot(&[(0, NotViable), (2, NotViable)], &[]);
    for _ in 0..3 {
        assert_eq!(
            cell.call(&unknown_only).await.expect("admitted"),
            cell.ids[1]
        );
    }
    assert_eq!(
        cell.ran(),
        vec![0, 3, 0],
        "the unverdicted provider took every call"
    );

    let all_not_ready = cell.snapshot(&[(0, NotViable), (1, NotViable), (2, NotViable)], &[]);
    let answered = cell.call(&all_not_ready).await.expect("still routed");
    assert!(cell.ids.contains(&answered));
    assert_eq!(
        cell.ran().iter().sum::<usize>(),
        4,
        "one more handler run, not zero"
    );
}

/// §6 #21: a capture of a DIFFERENT interest changes nothing. The call
/// routes exactly as the unsensed policy would, here round-robin over all
/// three providers.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unrelated_capture_does_not_steer_the_call() {
    let cell = Cell::stand_up(None).await;
    let unrelated = SensingSnapshot::for_test(
        "nrpc:some.other.service",
        &[(cell.ids[2], SensedViability::Viable)],
        &[cell.ids[2]],
    );
    for _ in 0..6 {
        cell.call(&unrelated).await.expect("admitted");
    }
    assert_eq!(
        cell.ran(),
        vec![2, 2, 2],
        "round-robin over all three: the unrelated capture's rank was ignored"
    );
}

/// §6 #22: a provider the capture ranks viable but that is not an
/// authorized candidate is never called. Here the rank names a node that does
/// not serve the service at all; authorization (discovery plus `may_execute`)
/// ran first, so it is not a candidate.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_ranked_provider_outside_the_authorized_set_is_never_called() {
    let cell = Cell::stand_up(None).await;
    let stranger = cell.consumer.inner().node_id();
    let readiness = SensingSnapshot::for_test(
        &format!("nrpc:{SERVICE}"),
        &[
            (stranger, SensedViability::Viable),
            (cell.ids[2], SensedViability::Viable),
        ],
        &[stranger, cell.ids[2]],
    );
    assert_eq!(cell.call(&readiness).await.expect("admitted"), cell.ids[2]);
    assert_eq!(cell.ran(), vec![0, 0, 1]);
}

/// §6 #23 and #24: a failure at the SELECTED provider is the call's result.
/// It is not retried against the next ranked provider, and no other handler
/// ran: the request may already have executed once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failure_at_the_selected_provider_is_final_and_never_re_executed() {
    use SensedViability::*;
    let cell = Cell::stand_up(Some(1)).await;
    let readiness = cell.snapshot(&[(1, Viable), (0, Viable), (2, Viable)], &[1, 0, 2]);

    let outcome = cell.call(&readiness).await;
    assert!(
        matches!(outcome, Err(RpcError::ServerError { .. })),
        "the selected provider's refusal is returned as-is: {outcome:?}"
    );
    assert_eq!(
        cell.ran(),
        vec![0, 1, 0],
        "exactly one execution, at the selected provider; nothing fell through to the next rank"
    );
}

/// The streaming verb chooses its target the same way (S2d): the stream is
/// opened against the best-ranked viable provider, despite round-robin.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_streaming_verb_opens_against_the_sensed_provider() {
    use futures::StreamExt;
    use SensedViability::*;
    let cell = Cell::stand_up(None).await;
    let mut streams = Vec::new();
    for provider in &cell.providers {
        let id = provider.inner().node_id();
        streams.push(
            provider
                .serve_rpc_streaming_typed(
                    STREAM_SERVICE,
                    net_sdk::mesh_rpc::Codec::Json,
                    move |_: u32, sink: net_sdk::mesh_rpc::ResponseSinkTyped<u64>| async move {
                        sink.send(&id)?;
                        Ok(())
                    },
                )
                .expect("serve streaming"),
        );
        provider
            .inner()
            .announce_capabilities(CapabilitySet::new())
            .await
            .expect("announce");
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while cell.consumer.find_service_nodes(STREAM_SERVICE).len() < 3 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "streaming service never discovered"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    let readiness = SensingSnapshot::for_test(
        &format!("nrpc:{STREAM_SERVICE}"),
        &[(cell.ids[2], Viable), (cell.ids[0], NotViable)],
        &[cell.ids[2]],
    );
    for _ in 0..3 {
        let body = Bytes::from(serde_json::to_vec(&0u32).unwrap());
        let mut stream = cell
            .consumer
            .call_service_streaming_sensed(STREAM_SERVICE, body, opts(), &readiness)
            .await
            .expect("stream opened");
        let first = stream.next().await.expect("one chunk").expect("ok chunk");
        let answered: u64 = serde_json::from_slice(&first).expect("decode");
        assert_eq!(
            answered, cell.ids[2],
            "the stream came from the sensed provider"
        );
    }
    drop(streams);
}

const STREAM_SERVICE: &str = "sensed.stream";
