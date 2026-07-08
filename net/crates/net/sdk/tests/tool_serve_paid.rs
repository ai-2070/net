//! `Mesh::serve_tool_paid` — the native payment gate (Gap-1 "Best").
//!
//! The invariant under test: **an announced price is an enforced price
//! on every serving path**, now including tools served straight from
//! the SDK with no MCP adapter. A scripted [`ToolPaymentGate`] stands in
//! for the engine-backed one (`net-payments` tests cover that impl):
//!
//! - an unpaid call is refused with the payment error before the
//!   handler runs;
//! - a gate denial propagates its reason;
//! - a paid call (quote header riding [`HDR_PAYMENT_QUOTE`]) redeems
//!   through the gate exactly once and the handler serves;
//! - a structurally invalid body is rejected BEFORE the gate — a call
//!   that can never execute must never consume the quote;
//! - a descriptor without `pricing_terms` is refused
//!   (`MissingPricingTerms`) — the gated path requires the announced
//!   price, the ungated path (`serve_tool`) refuses it.

#![cfg(all(feature = "tool", feature = "cortex"))]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use net_sdk::mesh::{Mesh, MeshBuilder};
use net_sdk::mesh_rpc::{CallOptions, RpcError, ServeError};
use net_sdk::tool::metadata_for;
use net_sdk::tool_payment::{
    failure_vocab, FailureSchematic, GateDenial, Recovery, ToolPaymentGate, ERR_PAYMENT,
    HDR_FAILURE_SCHEMATIC, HDR_PAYMENT_QUOTE, TAG_PAYMENT_FAILURE,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

const PSK: [u8; 32] = [0x61u8; 32];
const TERMS: &str = r#"{"object":"net.pricing.terms@1"}"#;

#[derive(JsonSchema, Deserialize, Serialize, Debug, PartialEq, Eq)]
struct EchoReq {
    message: String,
}

#[derive(JsonSchema, Deserialize, Serialize, Debug, PartialEq, Eq)]
struct EchoResp {
    echoed: String,
}

/// Records every redemption; admits unless the quote id is `q-deny`.
struct RecordingGate {
    redeemed: parking_lot::Mutex<Vec<(String, String, bool)>>,
}

impl RecordingGate {
    fn new() -> Self {
        Self {
            redeemed: parking_lot::Mutex::new(Vec::new()),
        }
    }
}

#[async_trait::async_trait]
impl ToolPaymentGate for RecordingGate {
    async fn redeem(
        &self,
        tool_id: &str,
        quote_id: &str,
        binding: Option<&[u8]>,
    ) -> Result<(), GateDenial> {
        self.redeemed
            .lock()
            .push((tool_id.to_string(), quote_id.to_string(), binding.is_some()));
        if quote_id == "q-deny" {
            // A scripted denial with a scripted schematic: the handler
            // must pass BOTH through untouched (message → body,
            // schematic → reply header).
            let message = "quote already redeemed (scripted denial)".to_string();
            return Err(GateDenial {
                schematic: FailureSchematic {
                    object: TAG_PAYMENT_FAILURE.to_string(),
                    code: failure_vocab::CODE_PAYMENT.to_string(),
                    stage: failure_vocab::STAGE_REDEEM.to_string(),
                    reason: "scripted_denial".to_string(),
                    message: message.clone(),
                    retryable: false,
                    recovery: Recovery {
                        class: failure_vocab::CLASS_NEW_QUOTE_REQUIRED.to_string(),
                        actor: failure_vocab::ACTOR_CALLER_AGENT.to_string(),
                        safe_to_retry: false,
                        safe_to_requote: true,
                        next_action: None,
                    },
                    handler_executed: false,
                    funds_moved: failure_vocab::FUNDS_UNKNOWN.to_string(),
                    prior_payment: failure_vocab::PRIOR_UNKNOWN.to_string(),
                    quote_id: Some(quote_id.to_string()),
                    tool_id: Some(tool_id.to_string()),
                    extra: Default::default(),
                },
                message,
            });
        }
        Ok(())
    }
}

async fn build_pair() -> (Mesh, Mesh, SocketAddr) {
    let a = MeshBuilder::new("127.0.0.1:0", &PSK)
        .unwrap()
        .build()
        .await
        .unwrap();
    let b = MeshBuilder::new("127.0.0.1:0", &PSK)
        .unwrap()
        .build()
        .await
        .unwrap();
    let addr_b = b.inner().local_addr();
    (a, b, addr_b)
}

async fn handshake(a: &Mesh, b: &Mesh, addr_b: SocketAddr) {
    let pub_b = *b.inner().public_key();
    let nid_b = b.inner().node_id();
    let nid_a = a.inner().node_id();
    let (r1, r2) = tokio::join!(
        b.inner().accept(nid_a),
        a.inner().connect(addr_b, &pub_b, nid_b),
    );
    r1.expect("accept");
    r2.expect("connect");
    a.inner().start();
    b.inner().start();
}

/// A server refusal, both renderings: the wire status + human message,
/// and the schematic decoded from the reply header per the discipline
/// rule (exactly one valid header, else `None` and the human path).
#[derive(Debug)]
struct ServerRefusal {
    status: u16,
    message: String,
    schematic: Option<FailureSchematic>,
}

/// A bounded call with request headers, retried a few times (the first
/// cross-node call can lose its reply before the per-caller reply
/// subscription propagates — the round-trip suite's idiom). A server
/// refusal is terminal (never retried) and returns both renderings.
async fn call_with_headers(
    caller: &Mesh,
    target: u64,
    service: &str,
    body: &[u8],
    headers: Vec<(String, Vec<u8>)>,
) -> Result<Vec<u8>, ServerRefusal> {
    let mut last = String::new();
    for _ in 0..5 {
        let opts = CallOptions {
            request_headers: headers.clone(),
            ..CallOptions::default()
        };
        match tokio::time::timeout(
            Duration::from_secs(5),
            caller.call(target, service, bytes::Bytes::copy_from_slice(body), opts),
        )
        .await
        {
            Ok(Ok(reply)) => return Ok(reply.body.to_vec()),
            Ok(Err(RpcError::ServerError {
                status,
                message,
                headers,
            })) => {
                let entries: Vec<&Vec<u8>> = headers
                    .iter()
                    .filter(|(name, _)| name == HDR_FAILURE_SCHEMATIC)
                    .map(|(_, value)| value)
                    .collect();
                // The discipline rule: exactly one valid schematic
                // header counts; zero, duplicates, or malformed → the
                // human error alone.
                let schematic = match entries.as_slice() {
                    [bytes] => FailureSchematic::from_header_bytes(bytes),
                    _ => None,
                };
                return Err(ServerRefusal {
                    status,
                    message,
                    schematic,
                });
            }
            Ok(Err(e)) => last = format!("rpc error: {e:?}"),
            Err(_) => last = "call timed out".to_string(),
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("call never reached the server: {last}");
}

#[tokio::test]
async fn a_paid_native_tool_redeems_before_the_handler_runs() {
    let (caller, host, host_addr) = build_pair().await;
    handshake(&caller, &host, host_addr).await;
    let host_id = host.inner().node_id();

    let gate = Arc::new(RecordingGate::new());
    let descriptor = metadata_for::<EchoReq, EchoResp>("paid_echo")
        .description("Echo, for money.")
        .pricing_terms(TERMS)
        .build();
    let _handle = host
        .serve_tool_paid::<EchoReq, EchoResp, _, _>(descriptor, gate.clone(), |req| async move {
            Ok(EchoResp {
                echoed: req.message,
            })
        })
        .expect("a priced descriptor serves through the gate");

    let body = serde_json::to_vec(&EchoReq {
        message: "hi".into(),
    })
    .unwrap();

    // Unpaid: refused with the payment error, handler never ran, gate
    // never consulted — and the handler-authored `missing_quote`
    // schematic rides the reply header beside the human message.
    let err = call_with_headers(&caller, host_id, "paid_echo", &body, vec![])
        .await
        .expect_err("an unpaid call must be refused");
    assert_eq!(err.status, ERR_PAYMENT);
    assert!(err.message.contains("payment quote"), "{}", err.message);
    let schematic = err.schematic.expect("the refusal carries its schematic");
    assert_eq!(schematic.reason, "missing_quote");
    assert_eq!(schematic.stage, "admission");
    assert!(!schematic.handler_executed);
    assert_eq!(schematic.recovery.class, "new_quote_required");
    assert_eq!(schematic.tool_id.as_deref(), Some("paid_echo"));
    assert!(gate.redeemed.lock().is_empty());

    // Gate denial: BOTH renderings travel to the caller — the message
    // as the error body, the gate's schematic untouched on the header.
    let err = call_with_headers(
        &caller,
        host_id,
        "paid_echo",
        &body,
        vec![(HDR_PAYMENT_QUOTE.to_string(), b"q-deny".to_vec())],
    )
    .await
    .expect_err("a denied quote must be refused");
    assert_eq!(err.status, ERR_PAYMENT);
    assert!(err.message.contains("scripted denial"), "{}", err.message);
    let schematic = err.schematic.expect("the gate's schematic passes through");
    assert_eq!(schematic.reason, "scripted_denial");
    assert_eq!(schematic.quote_id.as_deref(), Some("q-deny"));

    // Paid: redeems through the gate, handler serves.
    let reply = call_with_headers(
        &caller,
        host_id,
        "paid_echo",
        &body,
        vec![(HDR_PAYMENT_QUOTE.to_string(), b"q-1".to_vec())],
    )
    .await
    .expect("the paid call serves");
    let resp: EchoResp = serde_json::from_slice(&reply).expect("decode");
    assert_eq!(resp.echoed, "hi");
    {
        let redeemed = gate.redeemed.lock();
        assert!(redeemed
            .iter()
            .any(|r| r == &("paid_echo".to_string(), "q-1".to_string(), false)));
    }

    // Ordering: a structurally invalid body is rejected BEFORE the gate —
    // the quote is not consumed by a call that can never execute.
    let before = gate.redeemed.lock().len();
    let err = call_with_headers(
        &caller,
        host_id,
        "paid_echo",
        b"not json",
        vec![(HDR_PAYMENT_QUOTE.to_string(), b"q-2".to_vec())],
    )
    .await
    .expect_err("a bad body is refused");
    assert!(err.message.contains("bad request body"), "{}", err.message);
    assert!(
        err.schematic.is_none(),
        "a codec refusal carries no payment schematic"
    );
    assert_eq!(
        gate.redeemed.lock().len(),
        before,
        "a call that can never execute must never consume the quote"
    );

    caller.shutdown().await.ok();
    host.shutdown().await.ok();
}

#[tokio::test]
async fn the_gated_path_requires_pricing_and_the_ungated_path_refuses_it() {
    let mesh = MeshBuilder::new("127.0.0.1:0", &[0x62u8; 32])
        .unwrap()
        .build()
        .await
        .unwrap();
    let gate = Arc::new(RecordingGate::new());

    // No pricing_terms through the gate: refused — a gate on an
    // unannounced price refuses every caller invisibly.
    let unpriced = metadata_for::<EchoReq, EchoResp>("free_echo").build();
    let err = match mesh.serve_tool_paid::<EchoReq, EchoResp, _, _>(
        unpriced,
        gate.clone(),
        |req| async move {
            Ok(EchoResp {
                echoed: req.message,
            })
        },
    ) {
        Ok(_) => panic!("an unpriced descriptor must not serve through the gate"),
        Err(e) => e,
    };
    assert!(
        matches!(&err, ServeError::MissingPricingTerms(id) if id == "free_echo"),
        "{err}"
    );

    // And the invariant's other half still holds: the ungated path
    // refuses a priced descriptor.
    let priced = metadata_for::<EchoReq, EchoResp>("paid_echo")
        .pricing_terms(TERMS)
        .build();
    let err = match mesh.serve_tool::<EchoReq, EchoResp, _, _>(priced, |req| async move {
        Ok(EchoResp {
            echoed: req.message,
        })
    }) {
        Ok(_) => panic!("serve_tool must refuse a priced descriptor"),
        Err(e) => e,
    };
    assert!(
        matches!(&err, ServeError::UnenforceablePricing(id) if id == "paid_echo"),
        "{err}"
    );

    mesh.shutdown().await.ok();
}
