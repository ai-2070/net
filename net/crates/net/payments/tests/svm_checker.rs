//! P2 WS-A, adapter side: the `solana` JSON-RPC checker against a
//! fixture RPC node — the commitment ladder mapped into the tier
//! vocabulary, genesis-hash confirmation before any status is trusted,
//! and SPL token-balance-delta delivered extraction with the
//! payer-binding leg (a stranger's transfer to the same merchant sums to
//! an honest zero).
#![cfg(feature = "http-facilitator")]

use std::sync::Arc;

use net_payments::checker::svm::SvmChecker;
use net_payments::checker::{ChainChecker, ChainVerdict, TransferQuery};
use net_payments::core::verification::VerificationTier;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const NETWORK: &str = "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp";
/// The full genesis hash the fixture reports — its first 32 characters
/// are the CAIP-2 reference above.
const GENESIS: &str = "5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpjNhBqoPnpwXfWt";
const TX: &str =
    "3AsdoALgZFuq2oUVWrDYhg2pNeaLJKPLf8hU2mQ6U8qJxeJ6hsrPVpMn9ma39DtfYCrDQSvngWRP8NnTpEhezJpE";
const MINT: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
const RECIPIENT: &str = "MerchantWa11et111111111111111111111111111111";
const PAYER: &str = "PayerWa11et11111111111111111111111111111111";

/// Scripted RPC node: `(genesis result, signature-status entry, parsed tx)`.
struct RpcFixture {
    endpoint: String,
    genesis: Arc<parking_lot::Mutex<Value>>,
    status: Arc<parking_lot::Mutex<Value>>,
    transaction: Arc<parking_lot::Mutex<Value>>,
}

impl RpcFixture {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let endpoint = format!("http://{}", listener.local_addr().expect("addr"));
        let genesis = Arc::new(parking_lot::Mutex::new(Value::String(GENESIS.to_string())));
        let status = Arc::new(parking_lot::Mutex::new(Value::Null));
        let transaction = Arc::new(parking_lot::Mutex::new(Value::Null));
        let genesis_task = genesis.clone();
        let status_task = status.clone();
        let tx_task = transaction.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let genesis = genesis_task.clone();
                let status = status_task.clone();
                let transaction = tx_task.clone();
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut tmp = [0u8; 4096];
                    let header_end = loop {
                        let Ok(n) = stream.read(&mut tmp).await else {
                            return;
                        };
                        if n == 0 {
                            return;
                        }
                        buf.extend_from_slice(&tmp[..n]);
                        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            break pos + 4;
                        }
                    };
                    let head_text = String::from_utf8_lossy(&buf[..header_end]).into_owned();
                    let content_length: usize = head_text
                        .lines()
                        .filter_map(|l| l.split_once(':'))
                        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                        .and_then(|(_, v)| v.trim().parse().ok())
                        .unwrap_or(0);
                    let mut body = buf[header_end..].to_vec();
                    while body.len() < content_length {
                        let Ok(n) = stream.read(&mut tmp).await else {
                            return;
                        };
                        if n == 0 {
                            break;
                        }
                        body.extend_from_slice(&tmp[..n]);
                    }
                    let request: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                    let result = match request["method"].as_str() {
                        Some("getGenesisHash") => genesis.lock().clone(),
                        Some("getSignatureStatuses") => {
                            json!({ "context": { "slot": 100 }, "value": [status.lock().clone()] })
                        }
                        Some("getTransaction") => transaction.lock().clone(),
                        _ => Value::Null,
                    };
                    let response =
                        json!({ "jsonrpc": "2.0", "id": request["id"], "result": result })
                            .to_string();
                    let head_out = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                        response.len()
                    );
                    let _ = stream.write_all(head_out.as_bytes()).await;
                    let _ = stream.write_all(response.as_bytes()).await;
                    let _ = stream.shutdown().await;
                });
            }
        });
        Self {
            endpoint,
            genesis,
            status,
            transaction,
        }
    }

    fn set_genesis(&self, genesis: &str) {
        *self.genesis.lock() = Value::String(genesis.to_string());
    }
    fn set_status(&self, status: Value) {
        *self.status.lock() = status;
    }
    fn set_transaction(&self, tx: Value) {
        *self.transaction.lock() = tx;
    }
}

fn token_balance(index: u64, mint: &str, owner: &str, amount: &str) -> Value {
    json!({
        "accountIndex": index,
        "mint": mint,
        "owner": owner,
        "uiTokenAmount": { "amount": amount, "decimals": 6 },
    })
}

/// A successful parsed transaction whose meta moves `amount` of MINT
/// from PAYER (account 2) to RECIPIENT (account 1).
fn transfer_tx(amount: u128) -> Value {
    json!({
        "slot": 95,
        "meta": {
            "err": null,
            "preTokenBalances": [
                token_balance(1, MINT, RECIPIENT, "1000"),
                token_balance(2, MINT, PAYER, "50000"),
            ],
            "postTokenBalances": [
                token_balance(1, MINT, RECIPIENT, &(1000 + amount).to_string()),
                token_balance(2, MINT, PAYER, &(50000 - amount).to_string()),
            ],
        },
    })
}

fn query() -> TransferQuery {
    TransferQuery {
        token: MINT.to_string(),
        to: RECIPIENT.to_string(),
        from: None,
    }
}

fn query_from(payer: &str) -> TransferQuery {
    TransferQuery {
        from: Some(payer.to_string()),
        ..query()
    }
}

fn confirmed(n: u64) -> Value {
    json!({ "slot": 95, "confirmations": n, "err": null, "confirmationStatus": "confirmed" })
}

fn finalized() -> Value {
    json!({ "slot": 95, "confirmations": null, "err": null, "confirmationStatus": "finalized" })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_commitment_ladder_maps_into_the_tier_vocabulary() {
    let rpc = RpcFixture::start().await;
    let checker = SvmChecker::new(NETWORK, &rpc.endpoint).expect("checker");

    // Unknown signature: pending, no claim either way.
    rpc.set_status(Value::Null);
    assert_eq!(
        checker.check(NETWORK, TX, None).await.expect("check"),
        ChainVerdict::Pending
    );

    // `processed` is a single validator's view: still pending.
    rpc.set_status(
        json!({ "slot": 95, "confirmations": 0, "err": null, "confirmationStatus": "processed" }),
    );
    assert_eq!(
        checker.check(NETWORK, TX, None).await.expect("check"),
        ChainVerdict::Pending
    );

    // Failed on-chain: reverted, regardless of commitment.
    rpc.set_status(json!({
        "slot": 95, "confirmations": null,
        "err": { "InstructionError": [0, { "Custom": 1 }] },
        "confirmationStatus": "finalized",
    }));
    assert_eq!(
        checker.check(NETWORK, TX, None).await.expect("check"),
        ChainVerdict::Reverted
    );

    // Confirmed at n; finalized is deterministic Final.
    rpc.set_status(confirmed(5));
    assert_eq!(
        checker.check(NETWORK, TX, None).await.expect("check"),
        ChainVerdict::Included {
            tier: VerificationTier::Confirmed(5),
            delivered: None
        }
    );
    rpc.set_status(finalized());
    assert_eq!(
        checker.check(NETWORK, TX, None).await.expect("check"),
        ChainVerdict::Included {
            tier: VerificationTier::Final,
            delivered: None
        }
    );

    // Wrong network is a configuration error, terminal.
    let err = checker
        .check("solana:EtWTRABZaYq6iMfeYKouRu166VU2xqa1", TX, None)
        .await
        .expect_err("wrong network");
    assert!(!err.retryable);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_wrong_chain_rpc_is_refused_before_any_status_is_trusted() {
    let rpc = RpcFixture::start().await;
    // The endpoint serves some other chain (devnet genesis).
    rpc.set_genesis("EtWTRABZaYq6iMfeYKouRu166VU2xqa11111111111111");
    rpc.set_status(finalized());

    let checker = SvmChecker::new(NETWORK, &rpc.endpoint).expect("checker");
    let err = checker
        .check(NETWORK, TX, None)
        .await
        .expect_err("mismatched genesis must refuse");
    assert!(!err.retryable);
    assert!(err.message.contains("genesis"), "{}", err.message);

    // Heal the endpoint: the same checker verifies and proceeds.
    rpc.set_genesis(GENESIS);
    assert_eq!(
        checker.check(NETWORK, TX, None).await.expect("check"),
        ChainVerdict::Included {
            tier: VerificationTier::Final,
            delivered: None
        }
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delivered_amount_comes_from_token_balance_deltas() {
    let rpc = RpcFixture::start().await;
    let checker = SvmChecker::new(NETWORK, &rpc.endpoint).expect("checker");
    rpc.set_status(finalized());
    rpc.set_transaction(transfer_tx(10_000));

    let verdict = checker
        .check(NETWORK, TX, Some(&query()))
        .await
        .expect("check");
    assert_eq!(
        verdict,
        ChainVerdict::Included {
            tier: VerificationTier::Final,
            delivered: Some("10000".to_string())
        }
    );

    // Multi-ATA recipient: two accounts owned by the merchant sum; a
    // different mint and an account created mid-transaction (no pre
    // entry) are both handled.
    rpc.set_transaction(json!({
        "slot": 95,
        "meta": {
            "err": null,
            "preTokenBalances": [
                token_balance(1, MINT, RECIPIENT, "0"),
                token_balance(3, "OtherMint1111111111111111111111111111111111", RECIPIENT, "7"),
            ],
            "postTokenBalances": [
                token_balance(1, MINT, RECIPIENT, "6000"),
                // Created in this tx: no pre entry, full post counts.
                token_balance(2, MINT, RECIPIENT, "4000"),
                token_balance(3, "OtherMint1111111111111111111111111111111111", RECIPIENT, "999"),
            ],
        },
    }));
    let verdict = checker
        .check(NETWORK, TX, Some(&query()))
        .await
        .expect("check");
    let ChainVerdict::Included { delivered, .. } = verdict else {
        panic!("expected Included, got {verdict:?}");
    };
    assert_eq!(delivered.as_deref(), Some("10000"));

    // The status said included but the tx fetch missed (RPC lag): claim
    // nothing rather than a tier with an unverifiable amount.
    rpc.set_transaction(Value::Null);
    assert_eq!(
        checker
            .check(NETWORK, TX, Some(&query()))
            .await
            .expect("check"),
        ChainVerdict::Pending
    );
}

/// H3 parity: when the query carries the authorized payer, a qualifying
/// transfer to the same merchant funded by a *different* payer does not
/// count. Balances are transaction-level facts, so the bind is
/// transaction-level: the queried payer's balance for the mint must have
/// decreased in this transaction.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delivered_amount_binds_to_the_authorized_payer() {
    let rpc = RpcFixture::start().await;
    let checker = SvmChecker::new(NETWORK, &rpc.endpoint).expect("checker");
    rpc.set_status(finalized());
    rpc.set_transaction(transfer_tx(10_000));

    // Bound to the payer who actually funded it: counts.
    let verdict = checker
        .check(NETWORK, TX, Some(&query_from(PAYER)))
        .await
        .expect("check");
    let ChainVerdict::Included { delivered, .. } = verdict else {
        panic!("expected Included, got {verdict:?}");
    };
    assert_eq!(delivered.as_deref(), Some("10000"));

    // Bound to a stranger: the merchant was paid, but not by THIS quote's
    // payer — an honest zero the engine turns into an amount mismatch.
    let stranger = "SomebodyE1se1111111111111111111111111111111";
    let verdict = checker
        .check(NETWORK, TX, Some(&query_from(stranger)))
        .await
        .expect("check");
    let ChainVerdict::Included { delivered, .. } = verdict else {
        panic!("expected Included, got {verdict:?}");
    };
    assert_eq!(
        delivered.as_deref(),
        Some("0"),
        "a transfer funded by a different payer must not count as delivery"
    );

    // base58 is case-sensitive: a case-twiddled payer is a different key.
    let twiddled = PAYER.to_lowercase();
    let verdict = checker
        .check(NETWORK, TX, Some(&query_from(&twiddled)))
        .await
        .expect("check");
    let ChainVerdict::Included { delivered, .. } = verdict else {
        panic!("expected Included, got {verdict:?}");
    };
    assert_eq!(delivered.as_deref(), Some("0"));
}
