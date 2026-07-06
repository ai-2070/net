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
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const TX: &str = "0x1d31c8c8c283f9e5a766a4363b3cd6d34ef2ec89bcbf4b3c1c9b338d9e05d10f";
const TOKEN: &str = "0x036CbD53842c5426634e7929541eC2318f3dCF7e";
const RECIPIENT: &str = "0x209693Bc6afc0C5328bA36FaF03C514EF312287C";
const TRANSFER_TOPIC: &str = "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef";

/// Scripted RPC node: `(receipt result, head block)`.
struct RpcFixture {
    endpoint: String,
    receipt: Arc<parking_lot::Mutex<Value>>,
    head: Arc<parking_lot::Mutex<u64>>,
}

impl RpcFixture {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let endpoint = format!("http://{}", listener.local_addr().expect("addr"));
        let receipt = Arc::new(parking_lot::Mutex::new(Value::Null));
        let head = Arc::new(parking_lot::Mutex::new(100u64));
        let receipt_task = receipt.clone();
        let head_task = head.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let receipt = receipt_task.clone();
                let head = head_task.clone();
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut tmp = [0u8; 2048];
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
                        Some("eth_getTransactionReceipt") => receipt.lock().clone(),
                        Some("eth_blockNumber") => json!(format!("0x{:x}", *head.lock())),
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
            receipt,
            head,
        }
    }

    fn set_receipt(&self, receipt: Value) {
        *self.receipt.lock() = receipt;
    }
    fn set_head(&self, head: u64) {
        *self.head.lock() = head;
    }
}

fn topic_for(address: &str) -> String {
    format!(
        "0x{}{}",
        "0".repeat(24),
        address.trim_start_matches("0x").to_lowercase()
    )
}

fn transfer_log(token: &str, to: &str, amount_hex: &str) -> Value {
    json!({
        "address": token,
        "topics": [
            TRANSFER_TOPIC,
            topic_for("0x857b06519E91e3A54538791bDbb0E22373e36b66"),
            topic_for(to),
        ],
        "data": amount_hex,
    })
}

fn query() -> TransferQuery {
    TransferQuery {
        token: TOKEN.to_string(),
        to: RECIPIENT.to_string(),
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
