//! A system proxy must not be able to switch the destination policy off.
//!
//! `reqwest` reads `HTTP_PROXY` / `HTTPS_PROXY` / `ALL_PROXY` from the
//! environment by default, and a proxied request resolves the target **at
//! the proxy** — the client connects to the proxy and hands over the
//! hostname, so `GuardedResolver` never sees the address actually
//! reached. Left on, that turns every destination guarantee in
//! `http_policy` into something that silently stops applying on any host
//! with a proxy configured, which is worse than not having it: the source
//! still says the guard is there.
//!
//! **Its own test binary because the environment is not per-test.**
//! `std::env::set_var` is process-global, and cargo runs the tests in one
//! binary on parallel threads — setting a proxy variable in a unit test
//! would leak into every other test sharing that process. A separate
//! integration target is a separate process.

#![cfg(feature = "http-facilitator")]

use std::time::Duration;

use net_payments::http_policy::{self, DestinationPolicy};

/// The host every case below aims at. A name, not a literal, on purpose:
/// literals are refused up front by `check_url_destination` and never
/// reach the resolver, so a name is the only spelling that actually
/// exercises the resolver — and the only one a proxy could take away.
const TARGET: &str = "http://localhost:9/";

/// A proxy that cannot be connected to, so a request that *does* try to
/// use it fails in a way that is unmistakably not a policy refusal.
/// `.invalid` is reserved by RFC 2606 and never resolves.
const DEAD_PROXY: &str = "http://proxy.invalid:8080";

fn set_proxy_env() {
    for var in ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "all_proxy"] {
        std::env::set_var(var, DEAD_PROXY);
    }
    // Make sure nothing in the ambient environment exempts the target.
    std::env::remove_var("NO_PROXY");
    std::env::remove_var("no_proxy");
}

#[tokio::test]
async fn a_restricting_policy_ignores_the_system_proxy_and_still_refuses_at_the_resolver() {
    set_proxy_env();

    let client = http_policy::client(
        DestinationPolicy::PublicOnly,
        Duration::from_secs(5),
        Duration::from_secs(2),
        reqwest::redirect::Policy::none(),
    )
    .expect("client");

    let error = client
        .get(TARGET)
        .send()
        .await
        .expect_err("PublicOnly must refuse a name that resolves to loopback");

    assert!(
        http_policy::is_policy_refusal(&error),
        "the refusal must come from our own policy, not from the dead proxy: {error:?}"
    );
    // Naming the target host is what proves the resolver saw it. Had the
    // proxy been honoured, `localhost` would have been resolved at
    // `proxy.invalid` and could not appear in a refusal we produced.
    let rendered = format!("{error:?}");
    assert!(
        rendered.contains("localhost"),
        "the refusal must name the target host the resolver judged, got {rendered}"
    );
    assert!(
        !rendered.contains("proxy.invalid"),
        "the request must not have been routed at the proxy, got {rendered}"
    );
}

/// The escape hatch works, and is the only one: `Unrestricted` is the
/// policy that restricts nothing, so there is no guarantee for a proxy to
/// take away. A request under it reaches the (dead) proxy — which is the
/// observable difference from the case above.
#[tokio::test]
async fn the_unrestricted_policy_still_honours_a_system_proxy() {
    set_proxy_env();

    let client = http_policy::client(
        DestinationPolicy::Unrestricted,
        Duration::from_secs(5),
        Duration::from_secs(2),
        reqwest::redirect::Policy::none(),
    )
    .expect("client");

    let error = client
        .get(TARGET)
        .send()
        .await
        .expect_err("the dead proxy cannot be reached");

    let rendered = format!("{error:?}");
    assert!(
        rendered.contains("proxy.invalid"),
        "Unrestricted must still route through the configured proxy, got {rendered}"
    );
}
