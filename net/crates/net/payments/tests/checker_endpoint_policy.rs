//! H3: the independent chain checker must not accept a cleartext remote
//! RPC endpoint.
//!
//! This is the path that mints `confirmed(n)` and `final` — the tiers
//! that exist precisely so a facilitator need not be trusted. Over
//! cleartext, an on-path attacker fabricates `eth_getTransactionReceipt`,
//! `eth_blockNumber`, and `eth_chainId` at will and manufactures `final`
//! for a transaction that never landed. `ensure_chain_id` cannot help:
//! it reads its answer from the same unauthenticated channel.
//!
//! The facilitator client has enforced this since it was written; the
//! checker — the component with the *strictest* trust requirement of the
//! three money-path HTTP clients — was the one without it. Both now go
//! through `net_payments::http_policy`.

#![cfg(feature = "http-facilitator")]

use net_payments::checker::eip155::Eip155Checker;
use net_payments::checker::svm::SvmChecker;
use net_payments::checker::xrpl::XrplChecker;

/// Cleartext to a remote host is refused at construction, for every
/// checker adapter — the transport is shared, so this is one rule, but
/// each adapter is pinned so a future bespoke transport cannot quietly
/// opt out.
#[test]
fn cleartext_remote_rpc_endpoints_are_refused() {
    let eip = Eip155Checker::new("eip155:8453", "http://rpc.example.com:8545");
    assert!(
        eip.is_err(),
        "an eip155 checker must refuse a cleartext remote RPC endpoint"
    );
    let message = eip.err().map(|e| e.message).unwrap_or_default();
    assert!(
        message.contains("cleartext") || message.contains("https"),
        "the error should explain the https requirement: {message}"
    );

    assert!(
        SvmChecker::new(
            "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp",
            "http://rpc.example.com"
        )
        .is_err(),
        "an svm checker must refuse a cleartext remote RPC endpoint"
    );
    assert!(
        XrplChecker::new("xrpl:0", "http://rpc.example.com").is_err(),
        "an xrpl checker must refuse a cleartext remote RPC endpoint"
    );
}

/// https anywhere, and cleartext to a loopback **literal** for local
/// nodes — the documented self-hosted path stays open.
#[test]
fn https_and_loopback_literal_endpoints_are_accepted() {
    for ok in [
        "https://mainnet.base.org",
        "https://localhost:8545",
        "http://127.0.0.1:8545",
        "http://[::1]:8545",
    ] {
        assert!(
            Eip155Checker::new("eip155:8453", ok).is_ok(),
            "should accept {ok}"
        );
    }
}

/// The cleartext exception does not extend to the *name* `localhost`.
///
/// A name resolves to whatever DNS says, so `http://localhost` can be a
/// cleartext request to a public address — and on the checker that is
/// the worst place for it, since this is the path that mints
/// `confirmed(n)` and `final`. Only a literal loopback address is
/// self-evidently local.
#[test]
fn cleartext_to_the_name_localhost_is_refused() {
    let err = Eip155Checker::new("eip155:8453", "http://localhost:8545")
        .err()
        .map(|e| e.message)
        .unwrap_or_default();
    assert!(
        err.contains("cleartext") || err.contains("https"),
        "the name `localhost` must not get the cleartext exception: {err}"
    );
    // Over https it is fine — the guard is about cleartext, not the name.
    assert!(Eip155Checker::new("eip155:8453", "https://localhost:8545").is_ok());
}

/// A non-web scheme is refused rather than passed to the HTTP client.
#[test]
fn non_http_schemes_are_refused() {
    for bad in ["ftp://rpc.example.com", "file:///etc/passwd", "not-a-url"] {
        assert!(
            Eip155Checker::new("eip155:8453", bad).is_err(),
            "should refuse {bad}"
        );
    }
}
