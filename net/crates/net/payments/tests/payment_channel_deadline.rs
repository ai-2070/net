//! The mesh payment channel is **bounded in time**: a provider that
//! takes the request and never answers must become an observable
//! ambiguity, not a hang.
//!
//! `CallOptions::deadline` defaults to `None` — wait forever. Both
//! payment verbs were built with `Default::default()`, so a request
//! delivered to a live `net.payments.pay.v1` whose handler never replied
//! parked the caller permanently. That is the worst possible place for
//! an unbounded wait: the payload has left the process, the money may or
//! may not have moved, and the caller cannot record the ambiguity
//! because it never regains control.
//!
//! With a deadline, `MeshPaymentChannel::map_rpc_error` turns
//! `RpcError::Timeout` into a **retryable** `ChannelError`, which is what
//! drives a durable purchase attempt to `Unknown` — resumable later by
//! re-sending the identical stored payload rather than by buying a second
//! quote.
//!
//! A peer serving no payment service at all is deliberately *not* the
//! scenario: that fails fast with a no-route error. The hang needs a
//! live, subscribed service with a wedged handler, which is what the
//! parking handler here provides.

#![cfg(feature = "mesh")]

use std::sync::Arc;
use std::time::{Duration, Instant};

use net::adapter::net::identity::EntityKeypair;
use net_payments::core::canonical::SignedEnvelope as _;
use net_payments::core::quote::PaymentQuote;
use net_payments::core::registry::default_mock_registry;
use net_payments::flow::mesh::{MeshPaymentChannel, PAY_SERVICE};
use net_payments::flow::{Clock, ProviderChannel};
use net_payments::x402::payload::PaymentPayload;
use net_payments::x402::requirements::PaymentRequirements;
use net_payments::x402::X402Carry;
use net_sdk::mesh::{Mesh, MeshBuilder};
use net_sdk::mesh_rpc::{RpcContext, RpcHandler, RpcHandlerError, RpcResponsePayload, ServeHandle};

const PSK: [u8; 32] = [0x7Cu8; 32];

/// The channel's own deadline, mirrored: the constant is private, and a
/// test asserting "it returned at all" only needs an upper bound to wait
/// against. If the production value grows past this, the outer bound
/// below fails loudly rather than silently passing.
const EXPECTED_DEADLINE: Duration = Duration::from_secs(30);

struct Fixed;
impl Clock for Fixed {
    fn now_ns(&self) -> u64 {
        1_700_000_000_000_000_000
    }
}

/// Takes the request, answers nothing, ever.
struct Parks;

#[async_trait::async_trait]
impl RpcHandler for Parks {
    async fn call(&self, _ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
        std::future::pending::<()>().await;
        unreachable!("pending() never resolves")
    }
}

async fn mesh() -> Mesh {
    MeshBuilder::new("127.0.0.1:0", &PSK)
        .expect("builder")
        .build()
        .await
        .expect("build")
}

/// Handshake while unstarted, then start — a started node's receive loop
/// auto-accepts and races the responder handshake.
async fn connect(provider: &Mesh, caller: &Mesh) {
    let addr = provider.inner().local_addr();
    let pubkey = *provider.inner().public_key();
    let nid_provider = provider.inner().node_id();
    let nid_caller = caller.inner().node_id();
    let (accepted, connected) = tokio::join!(provider.inner().accept(nid_caller), async {
        tokio::time::sleep(Duration::from_millis(50)).await;
        caller.inner().connect(addr, &pubkey, nid_provider).await
    });
    accepted.expect("accept");
    connected.expect("connect");
    caller.start();
    provider.start();
}

/// A `pay` whose reply never comes returns a **retryable** channel error
/// on the deadline.
///
/// Retryable is the load-bearing half: a hard failure would tell a caller
/// the payment did not happen, which is exactly the claim that cannot be
/// made once the payload is on the wire. Retryable is what the durable
/// attempt reads as `Unknown`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unanswered_pay_times_out_retryably_instead_of_hanging() {
    let provider = mesh().await;
    let caller_mesh = Arc::new(mesh().await);
    let _stuck: ServeHandle = provider
        .serve_rpc(PAY_SERVICE, Arc::new(Parks))
        .expect("serve a parking pay handler");
    connect(&provider, &caller_mesh).await;

    let channel = MeshPaymentChannel::new(
        Arc::clone(&caller_mesh),
        Arc::new(EntityKeypair::generate()),
        Arc::new(Fixed),
    );

    // A quote whose capability routes to the parking provider. Only the
    // routing prefix matters: the handler never looks at the body, and
    // the caller must fail on the deadline rather than on the content.
    let quote_bytes = quote_for(provider.inner().node_id());
    let payload = payload();

    let started = Instant::now();
    // The outer bound IS the regression assertion: with no deadline this
    // never resolves and `expect` reports the hang.
    let outcome = tokio::time::timeout(
        EXPECTED_DEADLINE + Duration::from_secs(10),
        channel.pay(&quote_bytes, &payload),
    )
    .await
    .expect("pay hung past its own deadline — the channel has no bound");

    let err = outcome.expect_err("a parking provider cannot produce a PayResponse");
    assert!(
        err.retryable,
        "an expired pay deadline must be retryable — the payload left the process, so \
         'did not happen' is not a claim this can make: {err:?}"
    );
    // It waited for the deadline rather than failing early for an
    // unrelated reason (a routing or decode error would return at once,
    // and would satisfy the retryable assertion by accident on some
    // paths).
    assert!(
        started.elapsed() >= Duration::from_secs(5),
        "pay returned after only {:?} — that is not the deadline firing",
        started.elapsed()
    );
}

/// The control: the same channel against a provider that answers gets a
/// prompt verdict, so the timeout above is a statement about the wedged
/// handler and not something the deadline does to every call.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_responsive_provider_answers_well_inside_the_deadline() {
    struct Answers;
    #[async_trait::async_trait]
    impl RpcHandler for Answers {
        async fn call(&self, _ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
            // A structurally valid reply the channel will try to decode.
            // Whether it decodes is irrelevant: the property is that a
            // verdict arrives promptly instead of on a 30-second
            // deadline.
            Err(RpcHandlerError::Application {
                code: 0x8001,
                message: "provider refused".to_string(),
            })
        }
    }

    let provider = mesh().await;
    let caller_mesh = Arc::new(mesh().await);
    let _h: ServeHandle = provider
        .serve_rpc(PAY_SERVICE, Arc::new(Answers))
        .expect("serve an answering pay handler");
    connect(&provider, &caller_mesh).await;

    let channel = MeshPaymentChannel::new(
        Arc::clone(&caller_mesh),
        Arc::new(EntityKeypair::generate()),
        Arc::new(Fixed),
    );
    let quote_bytes = quote_for(provider.inner().node_id());
    let payload = payload();

    let started = Instant::now();
    let outcome = tokio::time::timeout(
        EXPECTED_DEADLINE + Duration::from_secs(10),
        channel.pay(&quote_bytes, &payload),
    )
    .await
    .expect("a responsive provider must not need the deadline");
    assert!(
        outcome.is_err(),
        "this fixture's provider refuses; the point is only that it answered"
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "an answered call took {:?} — it should not approach the deadline",
        started.elapsed()
    );
}

/// Real canonical quote bytes whose `capability` routes to `node`.
///
/// Built through [`PaymentQuote::new`] and the canonical encoder rather
/// than hand-written JSON: `pay` decodes the quote before it dials, so a
/// hand-rolled fixture fails *there* and the call returns instantly —
/// which would make both witnesses below pass without ever reaching the
/// wire. (That is not hypothetical: the first draft of this file did
/// exactly that, and the deadline assertion is what caught it.)
fn quote_for(node: u64) -> Vec<u8> {
    let requirements = X402Carry::author(&PaymentRequirements {
        scheme: "mock".into(),
        network: "mock:net".into(),
        amount: "2500".into(),
        asset: "musd".into(),
        pay_to: "mock-provider-settle-addr".into(),
        max_timeout_seconds: 60,
        extra: None,
    })
    .expect("the mock requirements author");
    let signer = EntityKeypair::generate();
    let registry = default_mock_registry(signer.entity_id().clone());
    let provider = EntityKeypair::generate();
    let mut quote = PaymentQuote::new(
        provider.entity_id().clone(),
        EntityKeypair::generate().entity_id().clone(),
        format!("{node}/net.a2a.task/summarize"),
        None,
        requirements,
        registry.reference().expect("the mock registry references"),
        1_700_000_000_000_000_000,
        1_900_000_000_000_000_000,
    );
    quote
        .sign_with(&provider)
        .expect("the provider signs its quote");
    net_payments::core::canonical::canonical_bytes(&quote).expect("the quote canonicalizes")
}

/// A payload whose `accepted` echoes the quote's requirements.
fn payload() -> X402Carry<PaymentPayload> {
    let accepted = PaymentRequirements {
        scheme: "mock".into(),
        network: "mock:net".into(),
        amount: "2500".into(),
        asset: "musd".into(),
        pay_to: "mock-provider-settle-addr".into(),
        max_timeout_seconds: 60,
        extra: None,
    };
    X402Carry::author(&PaymentPayload {
        x402_version: 2,
        resource: None,
        accepted,
        payload: serde_json::json!({ "note": "fixture" }),
        extensions: None,
    })
    .expect("the fixture payload authors")
}
