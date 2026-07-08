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

mod rpc_fixture;
use rpc_fixture::HttpJsonServer;

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
        let genesis = Arc::new(parking_lot::Mutex::new(Value::String(GENESIS.to_string())));
        let status = Arc::new(parking_lot::Mutex::new(Value::Null));
        let transaction = Arc::new(parking_lot::Mutex::new(Value::Null));
        let (genesis_r, status_r, tx_r) = (genesis.clone(), status.clone(), transaction.clone());
        let server = HttpJsonServer::start(move |request| {
            let result = match request["method"].as_str() {
                Some("getGenesisHash") => genesis_r.lock().clone(),
                Some("getSignatureStatuses") => {
                    json!({ "context": { "slot": 100 }, "value": [status_r.lock().clone()] })
                }
                Some("getTransaction") => tx_r.lock().clone(),
                _ => Value::Null,
            };
            json!({ "jsonrpc": "2.0", "id": request["id"], "result": result })
        })
        .await;
        Self {
            endpoint: server.endpoint,
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
        "transaction": { "message": message(json!([edge(PAYER, 2, 1, MINT, &amount.to_string())])) },
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

// The fixture's account table: `token_balance(index, …)` indexes into
// these keys, and the parsed transfer edges name them as source /
// destination ATAs (N3b attribution).
const ACCOUNT_KEYS: [&str; 6] = [
    "FeePayer11111111111111111111111111111111111",
    "AtaMerchantA11111111111111111111111111111111", // 1: merchant, MINT
    "AtaPayer111111111111111111111111111111111111", // 2: payer, MINT
    "AtaMerchantB11111111111111111111111111111111", // 3: merchant, MINT
    "AtaMerchantOther1111111111111111111111111111", // 4: merchant, other mint
    "AtaThirdParty1111111111111111111111111111111", // 5: a third party
];

fn message(instructions: Value) -> Value {
    json!({
        "accountKeys": ACCOUNT_KEYS.iter().map(|k| json!({ "pubkey": k })).collect::<Vec<_>>(),
        "instructions": instructions,
    })
}

/// A parsed spl-token `transferChecked` edge moving `amount` of `mint`.
/// The amount rides `tokenAmount` (the transferChecked shape) so the
/// checker's N3b amount-coverage bind can read it.
fn edge(
    authority: &str,
    source_index: usize,
    dest_index: usize,
    mint: &str,
    amount: &str,
) -> Value {
    json!({
        "program": "spl-token",
        "programId": "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA",
        "parsed": {
            "type": "transferChecked",
            "info": {
                "authority": authority,
                "source": ACCOUNT_KEYS[source_index],
                "destination": ACCOUNT_KEYS[dest_index],
                "mint": mint,
                "tokenAmount": { "amount": amount, "decimals": 6 },
            },
        },
    })
}

fn query() -> TransferQuery {
    TransferQuery {
        token: MINT.to_string(),
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

    // The query binds the payer (the production shape — delivery is only
    // attributable through the payer's own debit; see H2).
    let verdict = checker
        .check(NETWORK, TX, Some(&query_from(PAYER)))
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
    // entry) are both handled — and the payer's own debit still binds.
    rpc.set_transaction(json!({
        "slot": 95,
        // Two payer→merchant edges (into accounts 1 and 3) summing to the
        // 10000 delivered net — the amount-coverage bind (N3b).
        "transaction": { "message": message(json!([
            edge(PAYER, 2, 1, MINT, "6000"),
            edge(PAYER, 2, 3, MINT, "4000"),
        ])) },
        "meta": {
            "err": null,
            "preTokenBalances": [
                token_balance(1, MINT, RECIPIENT, "0"),
                token_balance(2, MINT, PAYER, "50000"),
                token_balance(4, "OtherMint1111111111111111111111111111111111", RECIPIENT, "7"),
            ],
            "postTokenBalances": [
                token_balance(1, MINT, RECIPIENT, "6000"),
                // Created in this tx: no pre entry, full post counts.
                token_balance(3, MINT, RECIPIENT, "4000"),
                token_balance(2, MINT, PAYER, "40000"),
                token_balance(4, "OtherMint1111111111111111111111111111111111", RECIPIENT, "999"),
            ],
        },
    }));
    let verdict = checker
        .check(NETWORK, TX, Some(&query_from(PAYER)))
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
            .check(NETWORK, TX, Some(&query_from(PAYER)))
            .await
            .expect("check"),
        ChainVerdict::Pending
    );
}

/// H2 fail-closed: an SPL settlement carries no per-transfer `from` and no
/// per-quote reference, so the payer debit is the *only* way this adapter
/// can attribute a delivery to the caller. When the query names no payer
/// (an opaque-blob scheme whose facilitator reported no settle-time payer),
/// the checker refuses rather than crediting any transfer to the merchant —
/// otherwise a facilitator could point at a stranger's on-chain transfer.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delivery_without_a_payer_is_refused() {
    let rpc = RpcFixture::start().await;
    let checker = SvmChecker::new(NETWORK, &rpc.endpoint).expect("checker");
    rpc.set_status(finalized());
    // A real, well-formed transfer to the merchant — but the query binds no
    // payer, so it must not be attributed to this quote.
    rpc.set_transaction(transfer_tx(10_000));

    let err = checker
        .check(NETWORK, TX, Some(&query()))
        .await
        .expect_err("an unbound (no-payer) delivery query must be refused");
    assert!(
        !err.retryable,
        "the refusal is terminal, not a transient shrug"
    );
    assert!(
        err.message.contains("payer"),
        "the refusal names the missing payer bind: {}",
        err.message
    );

    // A tier-only check (no query at all) still needs no payer — it makes
    // no delivery claim to attribute.
    assert_eq!(
        checker.check(NETWORK, TX, None).await.expect("tier-only"),
        ChainVerdict::Included {
            tier: VerificationTier::Final,
            delivered: None
        }
    );
}

/// L2: an unrelated token account in the same transaction (a different
/// mint, a router/CPI intermediary, some third party) with a malformed or
/// missing `amount` must be skipped, not poison the whole verification.
/// Only accounts that participate in the delivered/payer computation are
/// parsed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unrelated_token_account_does_not_poison_the_check() {
    let rpc = RpcFixture::start().await;
    let checker = SvmChecker::new(NETWORK, &rpc.endpoint).expect("checker");
    rpc.set_status(finalized());
    // A valid USDC transfer PAYER -> RECIPIENT, alongside an unrelated
    // token account whose entry is doubly malformed: `amount` is a number
    // (not the string SPL always returns) and it carries no accountIndex.
    // The old eager parse would turn this into a terminal error.
    rpc.set_transaction(json!({
        "slot": 95,
        "transaction": { "message": message(json!([edge(PAYER, 2, 1, MINT, "10000")])) },
        "meta": {
            "err": null,
            "preTokenBalances": [
                token_balance(1, MINT, RECIPIENT, "0"),
                token_balance(2, MINT, PAYER, "50000"),
                {
                    "mint": "OtherMint1111111111111111111111111111111111",
                    "owner": "Stranger1111111111111111111111111111111111",
                    "uiTokenAmount": { "amount": 123 },
                },
            ],
            "postTokenBalances": [
                token_balance(1, MINT, RECIPIENT, "10000"),
                token_balance(2, MINT, PAYER, "40000"),
            ],
        },
    }));
    let verdict = checker
        .check(NETWORK, TX, Some(&query_from(PAYER)))
        .await
        .expect("an unrelated malformed entry must not poison a valid settlement");
    let ChainVerdict::Included { delivered, .. } = verdict else {
        panic!("expected Included, got {verdict:?}");
    };
    assert_eq!(delivered.as_deref(), Some("10000"));
}

/// L1: delivered is the merchant's NET receipt across the accounts it
/// owns, not the sum of per-account positive deltas. When the merchant
/// receives into one account but an offsetting debit leaves another of the
/// same mint in the same transaction, the honest delivered amount is the
/// net — flooring each account at zero would over-credit the merchant.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delivered_nets_a_same_owner_debit() {
    let rpc = RpcFixture::start().await;
    let checker = SvmChecker::new(NETWORK, &rpc.endpoint).expect("checker");
    rpc.set_status(finalized());
    // Merchant receives 10000 into account 1 but 3000 leaves account 3
    // (same mint, same owner) in the same tx; the payer funds it.
    rpc.set_transaction(json!({
        "slot": 95,
        // Gross 10000 payer→merchant into account 1 (covers the 7000 net;
        // the 3000 leaving account 3 is a merchant-side debit, not a
        // payer edge).
        "transaction": { "message": message(json!([edge(PAYER, 2, 1, MINT, "10000")])) },
        "meta": {
            "err": null,
            "preTokenBalances": [
                token_balance(1, MINT, RECIPIENT, "0"),
                token_balance(2, MINT, PAYER, "50000"),
                token_balance(3, MINT, RECIPIENT, "3000"),
            ],
            "postTokenBalances": [
                token_balance(1, MINT, RECIPIENT, "10000"),
                token_balance(2, MINT, PAYER, "40000"),
                token_balance(3, MINT, RECIPIENT, "0"),
            ],
        },
    }));
    let verdict = checker
        .check(NETWORK, TX, Some(&query_from(PAYER)))
        .await
        .expect("check");
    let ChainVerdict::Included { delivered, .. } = verdict else {
        panic!("expected Included, got {verdict:?}");
    };
    assert_eq!(
        delivered.as_deref(),
        Some("7000"),
        "delivered is the net receipt (10000 in − 3000 out), not the gross 10000"
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

/// N3b — per-transfer attribution: the co-sign residual is closed. On
/// top of the delta binds, the transaction must carry a parseable
/// spl-token transfer edge payer→merchant; the edge is the attribution,
/// deltas stay the amount source.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn attribution_requires_a_payer_to_merchant_transfer_edge() {
    const THIRD: &str = "ThirdParty1111111111111111111111111111111111";
    const STRANGER: &str = "SomebodyE1se1111111111111111111111111111111";
    let rpc = RpcFixture::start().await;
    let checker = SvmChecker::new(NETWORK, &rpc.endpoint).expect("checker");
    rpc.set_status(finalized());

    // The co-sign attack: one atomic transaction where the payer's debit
    // funds a THIRD party while a STRANGER credits the merchant. Both
    // transaction-level legs hold (payer debited, merchant credited) —
    // but no payer→merchant edge exists, so nothing is attributed.
    rpc.set_transaction(json!({
        "slot": 95,
        "transaction": { "message": message(json!([
            edge(PAYER, 2, 5, MINT, "10000"),    // payer → third party
            edge(STRANGER, 4, 1, MINT, "10000"), // stranger → merchant
        ])) },
        "meta": {
            "err": null,
            "preTokenBalances": [
                token_balance(1, MINT, RECIPIENT, "0"),
                token_balance(2, MINT, PAYER, "50000"),
                token_balance(5, MINT, THIRD, "0"),
            ],
            "postTokenBalances": [
                token_balance(1, MINT, RECIPIENT, "10000"),
                token_balance(2, MINT, PAYER, "40000"),
                token_balance(5, MINT, THIRD, "10000"),
            ],
        },
    }));
    let verdict = checker
        .check(NETWORK, TX, Some(&query_from(PAYER)))
        .await
        .expect("check");
    let ChainVerdict::Included { delivered, .. } = verdict else {
        panic!("expected Included, got {verdict:?}");
    };
    assert_eq!(
        delivered.as_deref(),
        Some("0"),
        "a payer debit toward a third party must not attribute a stranger's credit"
    );

    // The dust-decoy variant (N-3): same stranger credit, but the payer
    // now adds a 0-amount payer→merchant edge to fake attribution. Edge
    // *existence* is satisfied; amount *coverage* is not (0 < 10000), so
    // it stays an honest zero.
    rpc.set_transaction(json!({
        "slot": 95,
        "transaction": { "message": message(json!([
            edge(PAYER, 2, 5, MINT, "10000"),    // payer → third party (real debit)
            edge(STRANGER, 4, 1, MINT, "10000"), // stranger → merchant (real credit)
            edge(PAYER, 2, 1, MINT, "0"),        // 0-amount decoy payer → merchant
        ])) },
        "meta": {
            "err": null,
            "preTokenBalances": [
                token_balance(1, MINT, RECIPIENT, "0"),
                token_balance(2, MINT, PAYER, "50000"),
                token_balance(5, MINT, THIRD, "0"),
            ],
            "postTokenBalances": [
                token_balance(1, MINT, RECIPIENT, "10000"),
                token_balance(2, MINT, PAYER, "40000"),
                token_balance(5, MINT, THIRD, "10000"),
            ],
        },
    }));
    let verdict = checker
        .check(NETWORK, TX, Some(&query_from(PAYER)))
        .await
        .expect("check");
    let ChainVerdict::Included { delivered, .. } = verdict else {
        panic!("expected Included, got {verdict:?}");
    };
    assert_eq!(
        delivered.as_deref(),
        Some("0"),
        "a zero/dust decoy payer→merchant edge must not attribute a stranger's credit"
    );

    // A CPI settlement: the payer→merchant edge lives in the INNER
    // instructions (a router program invoked spl-token) — still counts.
    rpc.set_transaction(json!({
        "slot": 95,
        "transaction": { "message": message(json!([])) },
        "meta": {
            "err": null,
            "innerInstructions": [
                { "index": 0, "instructions": [edge(PAYER, 2, 1, MINT, "10000")] }
            ],
            "preTokenBalances": [
                token_balance(1, MINT, RECIPIENT, "0"),
                token_balance(2, MINT, PAYER, "50000"),
            ],
            "postTokenBalances": [
                token_balance(1, MINT, RECIPIENT, "10000"),
                token_balance(2, MINT, PAYER, "40000"),
            ],
        },
    }));
    let verdict = checker
        .check(NETWORK, TX, Some(&query_from(PAYER)))
        .await
        .expect("check");
    let ChainVerdict::Included { delivered, .. } = verdict else {
        panic!("expected Included, got {verdict:?}");
    };
    assert_eq!(delivered.as_deref(), Some("10000"));

    // A plain `transfer` (no mint field, amount under `amount` not
    // `tokenAmount`) resolves the destination's mint through the balance
    // map and its amount from `info.amount` — still counts.
    let mut plain = edge(PAYER, 2, 1, MINT, "10000");
    plain["parsed"]["type"] = json!("transfer");
    {
        let info = plain["parsed"]["info"].as_object_mut().unwrap();
        info.remove("mint");
        info.remove("tokenAmount");
        info.insert("amount".into(), json!("10000"));
    }
    rpc.set_transaction(json!({
        "slot": 95,
        "transaction": { "message": message(json!([plain])) },
        "meta": {
            "err": null,
            "preTokenBalances": [
                token_balance(1, MINT, RECIPIENT, "0"),
                token_balance(2, MINT, PAYER, "50000"),
            ],
            "postTokenBalances": [
                token_balance(1, MINT, RECIPIENT, "10000"),
                token_balance(2, MINT, PAYER, "40000"),
            ],
        },
    }));
    let verdict = checker
        .check(NETWORK, TX, Some(&query_from(PAYER)))
        .await
        .expect("check");
    let ChainVerdict::Included { delivered, .. } = verdict else {
        panic!("expected Included, got {verdict:?}");
    };
    assert_eq!(delivered.as_deref(), Some("10000"));

    // Fail-closed: correct deltas but no parseable instructions at all —
    // nothing is attributed.
    rpc.set_transaction(json!({
        "slot": 95,
        "transaction": { "message": message(json!([])) },
        "meta": {
            "err": null,
            "preTokenBalances": [
                token_balance(1, MINT, RECIPIENT, "0"),
                token_balance(2, MINT, PAYER, "50000"),
            ],
            "postTokenBalances": [
                token_balance(1, MINT, RECIPIENT, "10000"),
                token_balance(2, MINT, PAYER, "40000"),
            ],
        },
    }));
    let verdict = checker
        .check(NETWORK, TX, Some(&query_from(PAYER)))
        .await
        .expect("check");
    let ChainVerdict::Included { delivered, .. } = verdict else {
        panic!("expected Included, got {verdict:?}");
    };
    assert_eq!(delivered.as_deref(), Some("0"));
}
