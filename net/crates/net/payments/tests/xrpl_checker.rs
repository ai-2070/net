//! XRPL enablement WS-3, adapter side: the `xrpl` JSON-RPC checker
//! against a rippled-shaped fixture — the two-runged deterministic
//! ladder (validated → Final), the closed non-`tesSUCCESS` → Reverted
//! rule, `delivered_amount`-only extraction with the satisfaction-form
//! rejections (non-Payment, `tfPartialPayment` even at full delivery),
//! payer/tag/invoice binding, and the `network_id` confirmation.
//! Row list per the plan's review tightening (Kyra, 2026-07-08).
#![cfg(feature = "http-facilitator")]

use std::sync::Arc;

use net_payments::checker::xrpl::XrplChecker;
use net_payments::checker::{ChainChecker, ChainVerdict, TransferQuery};
use net_payments::core::verification::VerificationTier;
use serde_json::{json, Value};
use sha2::Digest as _;

mod rpc_fixture;
use rpc_fixture::HttpJsonServer;

const NETWORK: &str = "xrpl:0";
const TX: &str = "C53ECF838647FA5A4C780377025FEC7999AB4182590510CA461444B207AB74A9";
const PAYER: &str = "rPayerAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
const RECIPIENT: &str = "rMerchant1111111111111111111111111";
const INVOICE: &str = "inv-quote-42";

/// Scripted rippled node: `(tx result, network_id)`.
struct RpcFixture {
    endpoint: String,
    tx: Arc<parking_lot::Mutex<Value>>,
    network_id: Arc<parking_lot::Mutex<Option<u64>>>,
}

impl RpcFixture {
    async fn start() -> Self {
        let tx = Arc::new(parking_lot::Mutex::new(Value::Null));
        let network_id = Arc::new(parking_lot::Mutex::new(Some(0u64)));
        let (tx_r, net_r) = (tx.clone(), network_id.clone());
        let server = HttpJsonServer::start(move |request| {
            // rippled envelope: everything (including errors) rides inside
            // `result`.
            let result = match request["method"].as_str() {
                Some("tx") => tx_r.lock().clone(),
                Some("server_info") => match *net_r.lock() {
                    Some(id) => json!({ "info": { "network_id": id } }),
                    None => json!({ "info": {} }),
                },
                _ => json!({ "status": "error", "error": "unknownCmd" }),
            };
            json!({ "result": result })
        })
        .await;
        Self {
            endpoint: server.endpoint,
            tx,
            network_id,
        }
    }

    fn set_tx(&self, tx: Value) {
        *self.tx.lock() = tx;
    }
    fn set_network_id(&self, id: Option<u64>) {
        *self.network_id.lock() = id;
    }
}

/// A validated tesSUCCESS Payment of `delivered` drops PAYER → RECIPIENT
/// carrying the invoice binding via MemoData (method A).
fn payment_tx(delivered: u128) -> Value {
    json!({
        "validated": true,
        "TransactionType": "Payment",
        "Account": PAYER,
        "Destination": RECIPIENT,
        "DestinationTag": 7,
        "Flags": 0,
        "Amount": "999999999",
        "Memos": [
            { "Memo": { "MemoData": hex::encode(INVOICE.as_bytes()).to_uppercase() } }
        ],
        "meta": {
            "TransactionResult": "tesSUCCESS",
            "delivered_amount": delivered.to_string(),
        },
    })
}

fn query() -> TransferQuery {
    TransferQuery {
        token: "XRP".to_string(),
        to: RECIPIENT.to_string(),
        from: None,
        reference: None,
        to_tag: None,
    }
}

fn full_query() -> TransferQuery {
    TransferQuery {
        from: Some(PAYER.to_string()),
        reference: Some(INVOICE.to_string()),
        to_tag: Some(7),
        ..query()
    }
}

async fn delivered_of(checker: &XrplChecker, q: &TransferQuery) -> String {
    let verdict = checker.check(NETWORK, TX, Some(q)).await.expect("check");
    let ChainVerdict::Included { tier, delivered } = verdict else {
        panic!("expected Included, got {verdict:?}");
    };
    assert_eq!(tier, VerificationTier::Final, "validated XRPL is Final");
    delivered.expect("query present ⇒ delivered reported")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_validated_ladder_maps_into_the_tier_vocabulary() {
    let rpc = RpcFixture::start().await;
    let checker = XrplChecker::new(NETWORK, &rpc.endpoint).expect("checker");

    // txnNotFound: pending, no claim either way.
    rpc.set_tx(json!({ "status": "error", "error": "txnNotFound" }));
    assert_eq!(
        checker.check(NETWORK, TX, None).await.expect("check"),
        ChainVerdict::Pending
    );

    // validated_false_pending: candidate ledgers revert.
    let mut unvalidated = payment_tx(1_000_000);
    unvalidated["validated"] = json!(false);
    rpc.set_tx(unvalidated);
    assert_eq!(
        checker.check(NETWORK, TX, None).await.expect("check"),
        ChainVerdict::Pending
    );

    // non_tes_success_result_reverted — the closed inequality rule,
    // with the trust-line code as the canonical example.
    for code in ["tecNO_LINE", "tecPATH_DRY", "tefPAST_SEQ"] {
        let mut failed = payment_tx(1_000_000);
        failed["meta"]["TransactionResult"] = json!(code);
        rpc.set_tx(failed);
        assert_eq!(
            checker.check(NETWORK, TX, None).await.expect("check"),
            ChainVerdict::Reverted,
            "{code} in a validated ledger must revert"
        );
    }

    // Validated tesSUCCESS: deterministic Final.
    rpc.set_tx(payment_tx(1_000_000));
    assert_eq!(
        checker.check(NETWORK, TX, None).await.expect("check"),
        ChainVerdict::Included {
            tier: VerificationTier::Final,
            delivered: None
        }
    );

    // Wrong network is a configuration error, terminal.
    let err = checker
        .check("xrpl:1", TX, None)
        .await
        .expect_err("wrong network");
    assert!(!err.retryable);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn network_id_mismatch_is_terminal_and_mainnet_tolerates_legacy_servers() {
    // network_id_mismatch_terminal: a testnet endpoint under a mainnet
    // checker refuses before any tx is trusted.
    let rpc = RpcFixture::start().await;
    rpc.set_network_id(Some(1));
    rpc.set_tx(payment_tx(1_000_000));
    let checker = XrplChecker::new(NETWORK, &rpc.endpoint).expect("checker");
    let err = checker
        .check(NETWORK, TX, None)
        .await
        .expect_err("mismatched network_id must refuse");
    assert!(!err.retryable);
    assert!(err.message.contains("network_id"), "{}", err.message);

    // Heal: the right id verifies and proceeds.
    rpc.set_network_id(Some(0));
    assert!(checker.check(NETWORK, TX, None).await.is_ok());

    // Legacy mainnet servers may omit network_id — tolerated for
    // mainnet (expected id 0) only.
    let rpc2 = RpcFixture::start().await;
    rpc2.set_network_id(None);
    rpc2.set_tx(payment_tx(1_000_000));
    let mainnet = XrplChecker::new("xrpl:0", &rpc2.endpoint).expect("checker");
    assert!(mainnet.check("xrpl:0", TX, None).await.is_ok());
    let testnet = XrplChecker::new("xrpl:1", &rpc2.endpoint).expect("checker");
    assert!(
        testnet.check("xrpl:1", TX, None).await.is_err(),
        "a non-mainnet checker requires an explicit network_id"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delivered_amount_is_canonical_and_the_satisfaction_form_is_enforced() {
    let rpc = RpcFixture::start().await;
    let checker = XrplChecker::new(NETWORK, &rpc.endpoint).expect("checker");

    // xrp_drops_delivered_exact_success — and the full bind (payer +
    // tag + invoice) all match.
    rpc.set_tx(payment_tx(1_000_000));
    assert_eq!(delivered_of(&checker, &full_query()).await, "1000000");

    // xrp_drops_delivered_less_invalid: the checker reports the honest
    // lesser delivered_amount (tx.Amount says 999999999 — never read);
    // the engine's under/over/exact policy turns it into a mismatch.
    rpc.set_tx(payment_tx(500_000));
    assert_eq!(delivered_of(&checker, &full_query()).await, "500000");

    // tes_success_but_delivered_amount_missing_rejected: canonical field
    // absent on a tesSUCCESS Payment → honest zero, never tx.Amount.
    let mut missing = payment_tx(1_000_000);
    missing["meta"]
        .as_object_mut()
        .unwrap()
        .remove("delivered_amount");
    rpc.set_tx(missing);
    assert_eq!(delivered_of(&checker, &full_query()).await, "0");

    // wrong_transaction_type_rejected: balance effects of a non-Payment
    // never satisfy settlement.
    let mut escrow = payment_tx(1_000_000);
    escrow["TransactionType"] = json!("EscrowFinish");
    rpc.set_tx(escrow);
    assert_eq!(delivered_of(&checker, &full_query()).await, "0");

    // partial_payment_flag_rejected: not an accepted satisfaction form
    // EVEN when delivered_amount equals the full amount — this checker
    // verifies settlements it did not author.
    let mut partial = payment_tx(1_000_000);
    partial["Flags"] = json!(0x0002_0000u64);
    rpc.set_tx(partial);
    assert_eq!(delivered_of(&checker, &full_query()).await, "0");

    // An IOU delivered_amount (object shape) is a token mismatch for the
    // XRP-only rung.
    let mut iou = payment_tx(1_000_000);
    iou["meta"]["delivered_amount"] = json!({
        "currency": "524C555344000000000000000000000000000000",
        "issuer": "rIssuer111111111111111111111111111",
        "value": "0.01",
    });
    rpc.set_tx(iou);
    assert_eq!(delivered_of(&checker, &full_query()).await, "0");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn payer_tag_and_invoice_bindings_hold() {
    let rpc = RpcFixture::start().await;
    let checker = XrplChecker::new(NETWORK, &rpc.endpoint).expect("checker");
    rpc.set_tx(payment_tx(1_000_000));

    // wrong-payer zero (H3 parity): a stranger's payment to the same
    // merchant never counts as THIS quote's delivery.
    let stranger = TransferQuery {
        from: Some("rSomebodyE1se111111111111111111111".to_string()),
        ..query()
    };
    assert_eq!(delivered_of(&checker, &stranger).await, "0");

    // wrong destination tag → zero; missing-tag expectation → zero.
    let wrong_tag = TransferQuery {
        to_tag: Some(8),
        ..query()
    };
    assert_eq!(delivered_of(&checker, &wrong_tag).await, "0");
    let mut untagged_tx = payment_tx(1_000_000);
    untagged_tx
        .as_object_mut()
        .unwrap()
        .remove("DestinationTag");
    rpc.set_tx(untagged_tx);
    let wants_tag = TransferQuery {
        to_tag: Some(7),
        ..query()
    };
    assert_eq!(delivered_of(&checker, &wants_tag).await, "0");

    // Invoice binding, method A (MemoData = hex(invoiceId)): matches. The
    // fixture tx carries DestinationTag 7, so the query authorizes it too
    // (M3) — this row isolates the invoice bind, not the tag.
    rpc.set_tx(payment_tx(1_000_000));
    let invoiced = TransferQuery {
        reference: Some(INVOICE.to_string()),
        to_tag: Some(7),
        ..query()
    };
    assert_eq!(delivered_of(&checker, &invoiced).await, "1000000");

    // Invoice binding, method B (InvoiceID = SHA256(invoiceId)): a tx
    // with no memo but the hashed InvoiceID field also binds.
    let mut method_b = payment_tx(1_000_000);
    method_b.as_object_mut().unwrap().remove("Memos");
    method_b["InvoiceID"] =
        json!(hex::encode(sha2::Sha256::digest(INVOICE.as_bytes())).to_uppercase());
    rpc.set_tx(method_b);
    assert_eq!(delivered_of(&checker, &invoiced).await, "1000000");

    // A payment bound to a DIFFERENT invoice never satisfies this quote.
    let other = TransferQuery {
        reference: Some("inv-other-quote".to_string()),
        to_tag: Some(7),
        ..query()
    };
    assert_eq!(delivered_of(&checker, &other).await, "0");
}

/// M3 defense in depth: a quote that authorized no sub-account tag must
/// not be satisfied by a payment that carries one — on a shared/tag-routed
/// custodial address a stray `DestinationTag` sends the XRP to a different
/// sub-account. An untagged quote matches only an untagged payment.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_untagged_quote_rejects_a_tagged_payment() {
    let rpc = RpcFixture::start().await;
    let checker = XrplChecker::new(NETWORK, &rpc.endpoint).expect("checker");

    // A full, correctly-bound Payment that happens to carry DestinationTag
    // 7 (routed to a sub-account). A quote with no tag must NOT be credited.
    rpc.set_tx(payment_tx(1_000_000));
    let untagged_quote = TransferQuery {
        from: Some(PAYER.to_string()),
        reference: Some(INVOICE.to_string()),
        to_tag: None,
        ..query()
    };
    assert_eq!(
        delivered_of(&checker, &untagged_quote).await,
        "0",
        "a tagged payment must not satisfy an untagged quote"
    );

    // The same quote against a genuinely untagged payment: satisfied.
    let mut untagged_tx = payment_tx(1_000_000);
    untagged_tx
        .as_object_mut()
        .unwrap()
        .remove("DestinationTag");
    rpc.set_tx(untagged_tx);
    assert_eq!(delivered_of(&checker, &untagged_quote).await, "1000000");
}

/// M2: rippled api_version 2 (and Clio's default) nest the transaction
/// fields under `tx_json`, leaving `meta`/`validated` at `result` level.
/// The checker pins api_version 1, but must still read the v2 shape
/// correctly if a server returns it — otherwise every field reads as
/// absent, delivery sums to zero, and every settlement silently
/// invalidates. Bind, tag, invoice, and delivered all resolve from
/// `tx_json`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tx_json_v2_shape_is_read_correctly() {
    let rpc = RpcFixture::start().await;
    let checker = XrplChecker::new(NETWORK, &rpc.endpoint).expect("checker");

    // Reshape the v1 fixture into the v2 envelope: transaction fields move
    // under `tx_json`; `validated` and `meta` stay at `result` level.
    let v1 = payment_tx(1_000_000);
    let obj = v1.as_object().unwrap();
    let mut tx_json = serde_json::Map::new();
    let mut result = serde_json::Map::new();
    for (k, v) in obj {
        match k.as_str() {
            "validated" | "meta" => {
                result.insert(k.clone(), v.clone());
            }
            _ => {
                tx_json.insert(k.clone(), v.clone());
            }
        }
    }
    result.insert("tx_json".to_string(), Value::Object(tx_json));
    rpc.set_tx(Value::Object(result));

    // Full bind (payer + tag + invoice) resolves from tx_json, delivered
    // from the result-level meta.
    assert_eq!(delivered_of(&checker, &full_query()).await, "1000000");

    // And the satisfaction-form rejection still fires on the v2 shape: a
    // wrong payer nested in tx_json is caught, not silently accepted.
    let stranger = TransferQuery {
        from: Some("rSomebodyE1se111111111111111111111".to_string()),
        ..query()
    };
    assert_eq!(delivered_of(&checker, &stranger).await, "0");
}
