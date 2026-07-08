//! P1 WS3, adapter side: the eip155 JSON-RPC checker against a fixture
//! RPC node — pending/reverted/confirmed/final mapping, and the ERC-20
//! Transfer-log delivered-amount extraction (right token, right
//! recipient, wrong ones ignored).
#![cfg(feature = "http-facilitator")]

use std::sync::Arc;

use net_payments::checker::eip155::Eip155Checker;
use net_payments::checker::{ChainChecker, ChainVerdict, TransferQuery};
use net_payments::core::verification::VerificationTier;
use serde_json::{json, Value};

mod rpc_fixture;
use rpc_fixture::HttpJsonServer;

const TX: &str = "0x1d31c8c8c283f9e5a766a4363b3cd6d34ef2ec89bcbf4b3c1c9b338d9e05d10f";
const TOKEN: &str = "0x036CbD53842c5426634e7929541eC2318f3dCF7e";
const RECIPIENT: &str = "0x209693Bc6afc0C5328bA36FaF03C514EF312287C";
const TRANSFER_TOPIC: &str = "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef";

/// Scripted RPC node: `(receipt result, head block, chain id)`.
struct RpcFixture {
    endpoint: String,
    receipt: Arc<parking_lot::Mutex<Value>>,
    head: Arc<parking_lot::Mutex<u64>>,
    chain_id: Arc<parking_lot::Mutex<u64>>,
}

impl RpcFixture {
    async fn start() -> Self {
        let receipt = Arc::new(parking_lot::Mutex::new(Value::Null));
        let head = Arc::new(parking_lot::Mutex::new(100u64));
        // Default to Base Sepolia (84532), matching the tests' CAIP-2.
        let chain_id = Arc::new(parking_lot::Mutex::new(84_532u64));
        let (receipt_r, head_r, chain_id_r) = (receipt.clone(), head.clone(), chain_id.clone());
        let server = HttpJsonServer::start(move |request| {
            let result = match request["method"].as_str() {
                Some("eth_getTransactionReceipt") => receipt_r.lock().clone(),
                Some("eth_blockNumber") => json!(format!("0x{:x}", *head_r.lock())),
                Some("eth_chainId") => json!(format!("0x{:x}", *chain_id_r.lock())),
                _ => Value::Null,
            };
            json!({ "jsonrpc": "2.0", "id": request["id"], "result": result })
        })
        .await;
        Self {
            endpoint: server.endpoint,
            receipt,
            head,
            chain_id,
        }
    }

    fn set_receipt(&self, receipt: Value) {
        *self.receipt.lock() = receipt;
    }
    fn set_head(&self, head: u64) {
        *self.head.lock() = head;
    }
    fn set_chain_id(&self, chain_id: u64) {
        *self.chain_id.lock() = chain_id;
    }
}

fn topic_for(address: &str) -> String {
    format!(
        "0x{}{}",
        "0".repeat(24),
        address.trim_start_matches("0x").to_lowercase()
    )
}

const PAYER: &str = "0x857b06519E91e3A54538791bDbb0E22373e36b66";

fn transfer_log(token: &str, to: &str, amount_hex: &str) -> Value {
    transfer_log_from(token, PAYER, to, amount_hex)
}

fn transfer_log_from(token: &str, from: &str, to: &str, amount_hex: &str) -> Value {
    json!({
        "address": token,
        "topics": [
            TRANSFER_TOPIC,
            topic_for(from),
            topic_for(to),
        ],
        "data": amount_hex,
    })
}

fn query() -> TransferQuery {
    TransferQuery {
        token: TOKEN.to_string(),
        to: RECIPIENT.to_string(),
        from: None,
        reference: None,
        to_tag: None,
    }
}

fn query_from(payer: &str) -> TransferQuery {
    TransferQuery {
        from: Some(payer.to_string()),
        ..query()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pending_reverted_confirmed_and_final_map_into_the_tier_vocabulary() {
    let rpc = RpcFixture::start().await;
    let checker = Eip155Checker::new("eip155:84532", &rpc.endpoint)
        .expect("checker")
        .with_final_depth(12);

    // No receipt yet: pending.
    rpc.set_receipt(Value::Null);
    assert_eq!(
        checker
            .check("eip155:84532", TX, None)
            .await
            .expect("check"),
        ChainVerdict::Pending
    );

    // Reverted.
    rpc.set_receipt(json!({ "status": "0x0", "blockNumber": "0x5f", "logs": [] }));
    assert_eq!(
        checker
            .check("eip155:84532", TX, None)
            .await
            .expect("check"),
        ChainVerdict::Reverted
    );

    // Included at depth 6 (head 100, block 95): confirmed(6).
    rpc.set_receipt(json!({ "status": "0x1", "blockNumber": "0x5f", "logs": [] }));
    rpc.set_head(100);
    assert_eq!(
        checker
            .check("eip155:84532", TX, None)
            .await
            .expect("check"),
        ChainVerdict::Included {
            tier: VerificationTier::Confirmed(6),
            delivered: None
        }
    );

    // Depth crosses final_depth: final.
    rpc.set_head(200);
    assert_eq!(
        checker
            .check("eip155:84532", TX, None)
            .await
            .expect("check"),
        ChainVerdict::Included {
            tier: VerificationTier::Final,
            delivered: None
        }
    );

    // Wrong network is a configuration error, terminal.
    let err = checker
        .check("eip155:8453", TX, None)
        .await
        .expect_err("wrong network");
    assert!(!err.retryable);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delivered_amount_comes_from_the_right_transfer_logs_only() {
    let rpc = RpcFixture::start().await;
    let checker = Eip155Checker::new("eip155:84532", &rpc.endpoint).expect("checker");
    rpc.set_head(100);

    // Three logs: the real transfer, one from another token, one to
    // another recipient — only the first counts.
    rpc.set_receipt(json!({
        "status": "0x1",
        "blockNumber": "0x5f",
        "logs": [
            transfer_log(TOKEN, RECIPIENT, "0x2710"),                  // 10000 ✓
            transfer_log("0xDifferentToken000000000000000000000000ff", RECIPIENT, "0xffff"),
            transfer_log(TOKEN, "0x1111111111111111111111111111111111111111", "0xffff"),
        ],
    }));

    let verdict = checker
        .check("eip155:84532", TX, Some(&query()))
        .await
        .expect("check");
    let ChainVerdict::Included { delivered, .. } = verdict else {
        panic!("expected Included, got {verdict:?}");
    };
    assert_eq!(delivered.as_deref(), Some("10000"));

    // No matching logs at all: delivered = 0 — an honest zero the
    // engine turns into an amount-mismatch invalidation.
    rpc.set_receipt(json!({ "status": "0x1", "blockNumber": "0x5f", "logs": [] }));
    let verdict = checker
        .check("eip155:84532", TX, Some(&query()))
        .await
        .expect("check");
    let ChainVerdict::Included { delivered, .. } = verdict else {
        panic!("expected Included, got {verdict:?}");
    };
    assert_eq!(delivered.as_deref(), Some("0"));
}

/// H3 regression: when the query carries the authorized payer, a
/// qualifying transfer to the same merchant from a *different* payer does
/// not count. This is the leg that stops a facilitator satisfying a quote
/// with some other customer's payment to the same `payTo`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delivered_amount_binds_to_the_authorized_payer() {
    let rpc = RpcFixture::start().await;
    let checker = Eip155Checker::new("eip155:84532", &rpc.endpoint).expect("checker");
    rpc.set_head(100);

    let other_payer = "0x00000000000000000000000000000000deadbeef";
    // Right token, right recipient, right amount — but a different payer.
    rpc.set_receipt(json!({
        "status": "0x1",
        "blockNumber": "0x5f",
        "logs": [ transfer_log_from(TOKEN, other_payer, RECIPIENT, "0x2710") ],
    }));

    // Bound to the real payer: the stranger's transfer contributes nothing.
    let verdict = checker
        .check("eip155:84532", TX, Some(&query_from(PAYER)))
        .await
        .expect("check");
    let ChainVerdict::Included { delivered, .. } = verdict else {
        panic!("expected Included, got {verdict:?}");
    };
    assert_eq!(
        delivered.as_deref(),
        Some("0"),
        "a transfer from a different payer must not count as delivery"
    );

    // Bound to the stranger who actually sent it: now it counts. Proves
    // the filter is the `from` bind, not an accident of the fixture.
    let verdict = checker
        .check("eip155:84532", TX, Some(&query_from(other_payer)))
        .await
        .expect("check");
    let ChainVerdict::Included { delivered, .. } = verdict else {
        panic!("expected Included, got {verdict:?}");
    };
    assert_eq!(delivered.as_deref(), Some("10000"));
}

/// M7 regression: the checker confirms the RPC serves the CAIP-2 chain it
/// is configured for before trusting any receipt. An RPC reporting a
/// different chain id (a swapped/typo'd endpoint) is a terminal error —
/// never a settlement on the wrong chain.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_chain_id_mismatch_is_a_terminal_error() {
    let rpc = RpcFixture::start().await;
    // The endpoint actually serves Base *mainnet* (8453) while the checker
    // is configured for Base Sepolia (84532).
    rpc.set_chain_id(8453);
    rpc.set_head(100);
    rpc.set_receipt(json!({ "status": "0x1", "blockNumber": "0x5f", "logs": [] }));

    let checker = Eip155Checker::new("eip155:84532", &rpc.endpoint).expect("checker");
    let err = checker
        .check("eip155:84532", TX, None)
        .await
        .expect_err("a chain-id mismatch must be terminal");
    assert!(!err.retryable, "a wrong-chain RPC is a configuration fault");
    assert!(
        err.message.contains("chain id") || err.message.contains("chain"),
        "error should name the chain mismatch: {}",
        err.message
    );

    // Corrected to the right chain id, the same checker validates.
    rpc.set_chain_id(84532);
    let checker_ok = Eip155Checker::new("eip155:84532", &rpc.endpoint).expect("checker");
    assert!(matches!(
        checker_ok
            .check("eip155:84532", TX, None)
            .await
            .expect("check"),
        ChainVerdict::Included { .. }
    ));
}

/// M10 regression: the checker's `final` depth comes from the config pack
/// per network, not a hardcoded 12. Built via `from_config` with a
/// configured final_depth of 100, a settlement 50 blocks deep is
/// `Confirmed(50)` — where the old hardcoded 12 would have wrongly called
/// it `Final`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn final_depth_comes_from_the_config_pack() {
    let rpc = RpcFixture::start().await;
    // 50 confirmations: block 95 (0x5f), head 144.
    rpc.set_receipt(json!({ "status": "0x1", "blockNumber": "0x5f", "logs": [] }));
    rpc.set_head(144);

    let mut config = net_payments::facilitator::packs::x402_org_base_sepolia();
    config
        .rpc_endpoints
        .insert("eip155:84532".to_string(), rpc.endpoint.clone());
    config.final_depth.insert("eip155:84532".to_string(), 100);

    let checker = Eip155Checker::from_config(&config, "eip155:84532").expect("checker from config");
    assert_eq!(
        checker
            .check("eip155:84532", TX, None)
            .await
            .expect("check"),
        ChainVerdict::Included {
            tier: VerificationTier::Confirmed(50),
            delivered: None
        },
        "50 < configured final_depth 100 → Confirmed, not Final"
    );
}

// ---------------------------------------------------------------------
// N3a: the settlement must have consumed THIS quote's authorization
// ---------------------------------------------------------------------

const NONCE: &str = "0xf3746613c2d920b5fdabc0856f2aeb2d4f88ee6037b8cc5d04a71a4462f13480";

/// Computed here independently of the checker's own helper — a drift in
/// either spelling of the EIP-3009 event signature fails the suite.
fn authorization_used_topic() -> String {
    use sha3::Digest as _;
    format!(
        "0x{}",
        hex::encode(sha3::Keccak256::digest(
            b"AuthorizationUsed(address,bytes32)"
        ))
    )
}

fn authorization_used_log(token: &str, authorizer: &str, nonce: &str) -> Value {
    json!({
        "address": token,
        "topics": [authorization_used_topic(), topic_for(authorizer), nonce],
        "data": "0x",
    })
}

fn query_with_nonce(payer: &str, nonce: &str) -> TransferQuery {
    TransferQuery {
        from: Some(payer.to_string()),
        reference: Some(nonce.to_string()),
        ..query()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delivered_amount_binds_to_the_authorization_nonce() {
    let rpc = RpcFixture::start().await;
    let checker = Eip155Checker::new("eip155:84532", &rpc.endpoint).expect("checker");
    rpc.set_head(100);

    // The settlement consumed THIS authorization: Transfer + the token's
    // AuthorizationUsed(authorizer, nonce) in the same receipt.
    rpc.set_receipt(json!({
        "status": "0x1",
        "blockNumber": "0x5f",
        "logs": [
            transfer_log(TOKEN, RECIPIENT, "0x2710"),
            authorization_used_log(TOKEN, PAYER, NONCE),
        ],
    }));
    let verdict = checker
        .check("eip155:84532", TX, Some(&query_with_nonce(PAYER, NONCE)))
        .await
        .expect("check");
    let ChainVerdict::Included { delivered, .. } = verdict else {
        panic!("expected Included, got {verdict:?}");
    };
    assert_eq!(delivered.as_deref(), Some("10000"));

    // A qualifying transfer WITHOUT this quote's AuthorizationUsed —
    // e.g. the same payer's OTHER purchase from the same merchant —
    // delivers nothing for this quote.
    rpc.set_receipt(json!({
        "status": "0x1",
        "blockNumber": "0x5f",
        "logs": [ transfer_log(TOKEN, RECIPIENT, "0x2710") ],
    }));
    let verdict = checker
        .check("eip155:84532", TX, Some(&query_with_nonce(PAYER, NONCE)))
        .await
        .expect("check");
    let ChainVerdict::Included { delivered, .. } = verdict else {
        panic!("expected Included, got {verdict:?}");
    };
    assert_eq!(
        delivered.as_deref(),
        Some("0"),
        "a settlement that never consumed this authorization must not count"
    );

    // A DIFFERENT nonce (another authorization by the same payer): zero.
    let other_nonce = format!("0x{}", "22".repeat(32));
    rpc.set_receipt(json!({
        "status": "0x1",
        "blockNumber": "0x5f",
        "logs": [
            transfer_log(TOKEN, RECIPIENT, "0x2710"),
            authorization_used_log(TOKEN, PAYER, &other_nonce),
        ],
    }));
    let verdict = checker
        .check("eip155:84532", TX, Some(&query_with_nonce(PAYER, NONCE)))
        .await
        .expect("check");
    let ChainVerdict::Included { delivered, .. } = verdict else {
        panic!("expected Included, got {verdict:?}");
    };
    assert_eq!(delivered.as_deref(), Some("0"));

    // A non-nonce-shaped reference belongs to another adapter's
    // vocabulary (e.g. an xrpl invoiceId): ignored here, delivery
    // counts on the (token, payer, recipient) binds alone.
    rpc.set_receipt(json!({
        "status": "0x1",
        "blockNumber": "0x5f",
        "logs": [ transfer_log(TOKEN, RECIPIENT, "0x2710") ],
    }));
    let foreign_ref = TransferQuery {
        from: Some(PAYER.to_string()),
        reference: Some("inv-quote-42".to_string()),
        ..query()
    };
    let verdict = checker
        .check("eip155:84532", TX, Some(&foreign_ref))
        .await
        .expect("check");
    let ChainVerdict::Included { delivered, .. } = verdict else {
        panic!("expected Included, got {verdict:?}");
    };
    assert_eq!(delivered.as_deref(), Some("10000"));
}
