//! Shared money-path HTTP boundary policy: scheme, destination, and
//! response-body bounds.
//!
//! Three HTTP clients sit on the money path — the facilitator client
//! ([`crate::facilitator::client`]), the chain-checker transport
//! ([`crate::checker`]), and the outbound HTTP-402 door
//! ([`crate::flow::http402`]) — and each needs the same three guards.
//! They used to be implemented per-client, which meant they drifted:
//! scheme enforcement existed in two of the three (the checker, whose
//! whole job is *independent* verification, was the one without it), and
//! body bounds existed in two of the three (the outbound door, the one
//! most likely to face a hostile server, was the one without them).
//!
//! So they live here once. A new money-path client gets all three by
//! construction or none by omission, and the omission is visible.
//!
//! ## Why a resolver, not a check
//!
//! Validating a hostname, or even resolving it and checking the result,
//! leaves a window: DNS can answer differently between the check and the
//! connect (rebinding), so what you validated is not necessarily what you
//! connect to. [`GuardedResolver`] closes that by construction — it *is*
//! the resolver reqwest uses, so the addresses it approves are exactly
//! the addresses the client dials. There is no second lookup to disagree
//! with the first.

#![cfg(feature = "http-facilitator")]

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;

/// A refusal from the money-path HTTP policy. Callers map this into
/// their own error type (facilitator / checker / flow) — the policy
/// deliberately has no opinion about retryability, because every refusal
/// here is a configuration or policy fact, never a transient one.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct PolicyError(pub String);

impl PolicyError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

// ---------------------------------------------------------------------
// Scheme
// ---------------------------------------------------------------------

/// Reject an endpoint that would put credentials or a signed payment
/// authorization on the wire in the clear: **https required, except to a
/// loopback host** (local and self-hosted testing).
///
/// Applied at construction for the facilitator client and the chain
/// checker, and before the paid retry on the outbound door.
pub fn require_secure_endpoint(endpoint: &str) -> Result<(), PolicyError> {
    let url = reqwest::Url::parse(endpoint)
        .map_err(|e| PolicyError::new(format!("endpoint `{endpoint}`: {e}")))?;
    match url.scheme() {
        "https" => Ok(()),
        "http" => {
            if url.host_str().is_some_and(is_loopback_host) {
                Ok(())
            } else {
                Err(PolicyError::new(format!(
                    "endpoint `{endpoint}` is plaintext http to a non-loopback host — refusing to \
                     send credentials in cleartext; use https"
                )))
            }
        }
        other => Err(PolicyError::new(format!(
            "endpoint `{endpoint}` uses unsupported scheme `{other}` (want https)"
        ))),
    }
}

/// Is this host a loopback **address literal**?
///
/// Deliberately literals only. `localhost` is a name, and a name is
/// whatever DNS says it is — a host file or a resolver can point it at a
/// public address, and then "http to localhost" is a cleartext request to
/// a remote host, which is exactly what the scheme rule exists to
/// prevent. The exception has to be address-level to mean anything.
///
/// `host_str` keeps IPv6 brackets (`[::1]`), so strip them before
/// parsing.
fn is_loopback_host(host: &str) -> bool {
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    bare.parse::<IpAddr>()
        .map(|ip| normalize(ip).is_loopback())
        .unwrap_or(false)
}

// ---------------------------------------------------------------------
// Destination
// ---------------------------------------------------------------------

/// Which destination addresses a money-path client may connect to.
///
/// The distinction that matters is **who chose the URL**. A facilitator
/// or RPC endpoint is operator configuration — someone typed it into a
/// config file, and pointing at a LAN host is a legitimate self-hosted
/// deployment. An outbound HTTP-402 fetch URL may be *agent-supplied*,
/// and there the same permissiveness is an SSRF primitive with the
/// host's network position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DestinationPolicy {
    /// Public unicast addresses only. The strictest setting; correct for
    /// any client whose URL can be influenced by a model or a remote
    /// party.
    PublicOnly,
    /// Public addresses plus loopback. The default for the outbound
    /// door: it keeps the documented local-testing path working while
    /// refusing link-local (including the cloud metadata address),
    /// private, and carrier-NAT ranges.
    PublicOrLoopback,
    /// Public, loopback, and private/LAN ranges. For self-hosted
    /// deployments whose facilitator or RPC node is on an internal
    /// network.
    AllowPrivate,
    /// No destination restriction. For endpoints that are pure operator
    /// configuration, where the operator's choice is the policy.
    Unrestricted,
}

impl DestinationPolicy {
    /// Does this policy admit `addr`?
    pub fn admits(&self, addr: IpAddr) -> bool {
        let ip = normalize(addr);
        match self {
            Self::Unrestricted => true,
            Self::AllowPrivate => is_public(ip) || ip.is_loopback() || is_private_use(ip),
            Self::PublicOrLoopback => is_public(ip) || ip.is_loopback(),
            Self::PublicOnly => is_public(ip),
        }
    }

    /// Human description of what this policy admits, for diagnostics —
    /// a refusal otherwise surfaces to the caller as an opaque connect
    /// error, since it happens inside the resolver.
    pub fn describe(&self) -> &'static str {
        match self {
            Self::Unrestricted => "unrestricted",
            Self::AllowPrivate => "public, loopback, or private",
            Self::PublicOrLoopback => "public or loopback",
            Self::PublicOnly => "public only",
        }
    }
}

/// Unwrap IPv4-**mapped** IPv6 (`::ffff:127.0.0.1`) to the v4 address it
/// actually reaches. Without this every v4 rule below is bypassable by
/// spelling the address in v6.
///
/// Deliberately `to_ipv4_mapped`, never `to_ipv4`: the latter also
/// converts the deprecated IPv4-**compatible** form, and it does so for
/// `::1` — which becomes `0.0.0.1`, losing the loopback classification
/// entirely. The IPv4-compatible block is handled instead by refusing
/// all of `::/96` in [`is_public_v6`].
fn normalize(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => IpAddr::V6(v6),
        },
        v4 => v4,
    }
}

/// Private-use / LAN ranges an operator might legitimately self-host on.
fn is_private_use(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_private(),
        IpAddr::V6(v6) => {
            let first = v6.segments()[0];
            // fc00::/7 unique local, plus deprecated fec0::/10 site local
            // — both are "an operator's own network", which is what
            // `AllowPrivate` is for. The stricter policies refuse them via
            // `is_public`.
            (first & 0xfe00) == 0xfc00 || (first & 0xffc0) == 0xfec0
        }
    }
}

/// Public unicast: reachable on the internet and not special-purpose.
///
/// `IpAddr::is_global` is still unstable, so the special-purpose ranges
/// are enumerated here. The list is deliberately conservative — anything
/// not recognised as globally routable unicast is refused, so a range
/// this misses fails closed rather than open.
fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => is_public_v6(v6),
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, _, _] = ip.octets();
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()        // 169.254/16 — includes 169.254.169.254
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_multicast()
        || a == 0                    // "this network"
        || (a == 100 && (64..128).contains(&b))  // 100.64/10 carrier NAT
        || (a == 192 && b == 0)      // 192.0.0/24 IETF protocol assignments
        || (a == 198 && (18..20).contains(&b))   // 198.18/15 benchmarking
        || a >= 240) // 240/4 reserved (incl. 255.255.255.255)
}

fn is_public_v6(ip: Ipv6Addr) -> bool {
    let s = ip.segments();
    let first = s[0];
    // `::/96` — the deprecated IPv4-compatible block, plus `::` and
    // `::1`. Refused wholesale rather than translated: `to_ipv4` would
    // map `::1` to `0.0.0.1` and lose its loopback meaning, so the block
    // is simply never public.
    let ipv4_compatible_block = s[..6].iter().all(|seg| *seg == 0);
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_multicast()
        || ipv4_compatible_block
        || (first & 0xfe00) == 0xfc00   // fc00::/7  unique local
        || (first & 0xffc0) == 0xfe80   // fe80::/10 link local
        || (first & 0xffc0) == 0xfec0   // fec0::/10 site local (deprecated,
                                        // still routable on many stacks)
        || (first == 0x2001 && (s[1] & 0xff00) == 0x0d00) // 2001:db8::/32 doc
        || (first == 0x2001 && s[1] == 0x0002)            // 2001:2::/48 benchmarking
        // 64:ff9b::/96 well-known NAT64 and 64:ff9b:1::/48 local-use
        // translation. Both carry an embedded IPv4 destination, so a
        // translating host turns them into a v4 reach — including into
        // the private ranges the v4 rules refuse. Left admitted, they are
        // a way round every guard above.
        || (first == 0x0064 && s[1] == 0xff9b)
        || first == 0x0100) // 100::/64  discard-only
}

/// Apply the destination policy to a URL whose host is an **IP literal**.
///
/// [`GuardedResolver`] covers hostnames, but it only ever sees names that
/// need resolving: a URL like `http://169.254.169.254/` is dialled
/// directly and never reaches DNS, so the resolver cannot refuse it. That
/// is the SSRF spelling an attacker reaches for first, so the literal
/// case gets its own check, applied before the request is sent.
///
/// A domain host returns `Ok` here and is enforced by the resolver
/// instead. The two halves together cover every host form; neither alone
/// does.
pub fn check_url_destination(
    url: &reqwest::Url,
    policy: DestinationPolicy,
) -> Result<(), PolicyError> {
    let Some(host) = url.host_str() else {
        return Ok(());
    };
    // `host_str` keeps IPv6 brackets (`[::1]`); strip them to parse.
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    let Ok(ip) = bare.parse::<IpAddr>() else {
        // A name, not a literal — the resolver enforces the policy, and
        // does so rebinding-safely.
        return Ok(());
    };
    if policy.admits(ip) {
        return Ok(());
    }
    Err(PolicyError::new(format!(
        "destination policy ({}) refuses the address `{ip}`",
        policy.describe()
    )))
}

/// A [`reqwest::dns::Resolve`] that applies a [`DestinationPolicy`].
///
/// Being the resolver — rather than a check run beside one — is what
/// makes this rebinding-safe: reqwest dials exactly the addresses
/// returned here, so there is no interval in which DNS can change its
/// mind between validation and connection.
///
/// A name that resolves to a mix of admitted and refused addresses
/// yields only the admitted ones. A name with no admitted address is an
/// error naming the policy, not an empty list — an empty iterator would
/// surface as an opaque connect failure.
pub struct GuardedResolver {
    policy: DestinationPolicy,
}

impl GuardedResolver {
    pub fn new(policy: DestinationPolicy) -> Self {
        Self { policy }
    }
}

impl reqwest::dns::Resolve for GuardedResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let policy = self.policy;
        Box::pin(async move {
            let host = name.as_str().to_string();
            // Port 0: reqwest replaces it with the URL's port (or the
            // scheme default), per the `Resolve` contract.
            let resolved: Vec<SocketAddr> = match tokio::net::lookup_host((host.as_str(), 0)).await
            {
                Ok(addrs) => addrs.collect(),
                Err(e) => {
                    return Err(
                        Box::new(PolicyError::new(format!("resolving `{host}`: {e}")))
                            as Box<dyn std::error::Error + Send + Sync>,
                    )
                }
            };

            let admitted: Vec<SocketAddr> = resolved
                .iter()
                .copied()
                .filter(|addr| policy.admits(addr.ip()))
                .collect();

            if admitted.is_empty() {
                let seen: Vec<String> = resolved.iter().map(|a| a.ip().to_string()).collect();
                return Err(Box::new(PolicyError::new(format!(
                    "destination policy ({}) refuses every address `{host}` resolves to: {:?}",
                    policy.describe(),
                    seen
                )))
                    as Box<dyn std::error::Error + Send + Sync>);
            }
            Ok(Box::new(admitted.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

/// Did this request failure come from *our* destination policy rather
/// than from the network?
///
/// A policy refusal happens inside the resolver, so reqwest reports it as
/// a connect failure — which every caller classifies as retryable. It is
/// not: the policy will refuse the same address again, forever. Callers
/// use this to map it to a terminal error instead, matching how the same
/// refusal is reported for an IP literal (which never reaches the
/// resolver and is refused up front).
///
/// Walks the source chain and downcasts, rather than matching on the
/// message: the boxed error is preserved through hyper's layers, so the
/// type is available and is not a formatting detail that can drift.
pub fn is_policy_refusal(error: &reqwest::Error) -> bool {
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(error);
    while let Some(err) = source {
        if err.downcast_ref::<PolicyError>().is_some() {
            return true;
        }
        source = err.source();
    }
    false
}

/// Build a money-path [`reqwest::Client`]: pinned TLS roots, the
/// destination policy wired in as the resolver, and the supplied
/// timeouts. Every money-path client is constructed through here.
pub fn client(
    policy: DestinationPolicy,
    timeout: std::time::Duration,
    connect_timeout: std::time::Duration,
    redirects: reqwest::redirect::Policy,
) -> Result<reqwest::Client, PolicyError> {
    let tls = crate::tls_roots::tls_config()
        .map_err(|e| PolicyError::new(format!("http tls config: {e}")))?;
    reqwest::Client::builder()
        .timeout(timeout)
        .connect_timeout(connect_timeout)
        .redirect(redirects)
        .use_preconfigured_tls(tls)
        .dns_resolver(Arc::new(GuardedResolver::new(policy)))
        .build()
        .map_err(|e| PolicyError::new(format!("http client build: {e}")))
}

// ---------------------------------------------------------------------
// Body bounds
// ---------------------------------------------------------------------

/// Why a bounded read stopped.
#[derive(Debug)]
pub enum ReadError {
    /// Transport failure mid-body. Retryability is the caller's call.
    Transport(reqwest::Error),
    /// The body exceeded the cap — declared up front or streamed past
    /// it. Always terminal: a peer sending more than the cap is not
    /// going to send less on retry.
    TooLarge { cap: usize },
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport(e) => write!(f, "{e}"),
            Self::TooLarge { cap } => {
                write!(f, "response body exceeded the {cap}-byte cap")
            }
        }
    }
}

/// Read a response body, capped at `cap` bytes.
///
/// A declared over-cap `content-length` is refused before a byte is
/// read; a body that streams past the cap (absent or understated length)
/// is refused mid-stream. Bounds memory against a hostile or
/// compromised endpoint that would otherwise stream until the timeout.
pub async fn read_bounded(response: reqwest::Response, cap: usize) -> Result<Vec<u8>, ReadError> {
    if let Some(len) = response.content_length() {
        if len as usize > cap {
            return Err(ReadError::TooLarge { cap });
        }
    }
    let mut response = response;
    let mut out = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(ReadError::Transport)? {
        if out.len().saturating_add(chunk.len()) > cap {
            return Err(ReadError::TooLarge { cap });
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn https_is_required_except_for_loopback_literals() {
        for ok in [
            "https://facilitator.example.com",
            "https://localhost:8080",
            "http://127.0.0.1:8080",
            "http://[::1]:8080",
            "http://[::ffff:127.0.0.1]:8080",
        ] {
            assert!(require_secure_endpoint(ok).is_ok(), "should accept {ok}");
        }
        for bad in [
            "http://facilitator.example.com",
            "http://10.0.0.5:8080",
            "ftp://facilitator.example.com",
            "not-a-url",
        ] {
            assert!(require_secure_endpoint(bad).is_err(), "should reject {bad}");
        }
    }

    /// The cleartext exception is address-level, so the *name* `localhost`
    /// does not get it.
    ///
    /// A name is whatever DNS says it is: a hosts file or a resolver can
    /// point `localhost` at a public address, and then "http to localhost"
    /// is a cleartext request to a remote host — precisely what the scheme
    /// rule exists to prevent. Only a literal loopback address is
    /// self-evidently local.
    #[test]
    fn cleartext_to_the_name_localhost_is_refused() {
        for name in [
            "http://localhost:8080",
            "http://localhost:8080/base",
            "http://LOCALHOST:8080",
            "http://localhost.localdomain:8080",
        ] {
            assert!(
                require_secure_endpoint(name).is_err(),
                "`{name}` is a name, not a loopback literal — it must not get the cleartext \
                 exception"
            );
        }
        // https to the same name is fine: the guard is about cleartext.
        assert!(require_secure_endpoint("https://localhost:8080").is_ok());
    }

    /// The v4 rules must not be bypassable by spelling the address in
    /// IPv6. `::ffff:127.0.0.1` reaches loopback and is classified as
    /// loopback.
    #[test]
    fn ipv4_mapped_v6_is_classified_as_its_v4_address() {
        let mapped: IpAddr = "::ffff:127.0.0.1".parse().unwrap();
        assert!(normalize(mapped).is_loopback());
        assert!(!DestinationPolicy::PublicOnly.admits(mapped));
        assert!(DestinationPolicy::PublicOrLoopback.admits(mapped));

        let mapped_private: IpAddr = "::ffff:10.0.0.5".parse().unwrap();
        assert!(!DestinationPolicy::PublicOrLoopback.admits(mapped_private));
        assert!(DestinationPolicy::AllowPrivate.admits(mapped_private));
    }

    /// `Ipv6Addr::to_ipv4` maps `::1` to `0.0.0.1`, which is not
    /// loopback — normalising through it would silently declassify the
    /// v6 loopback address. `::1` must stay loopback, and the whole
    /// deprecated IPv4-compatible block must stay non-public.
    #[test]
    fn v6_loopback_survives_normalization_and_the_compatible_block_is_refused() {
        let v6_loopback: IpAddr = "::1".parse().unwrap();
        assert!(
            normalize(v6_loopback).is_loopback(),
            "::1 must not be normalised into 0.0.0.1"
        );
        assert!(is_loopback_host("[::1]"), "bracketed v6 loopback host");
        assert!(!DestinationPolicy::PublicOnly.admits(v6_loopback));
        assert!(DestinationPolicy::PublicOrLoopback.admits(v6_loopback));

        // Deprecated IPv4-compatible spellings are never public.
        for compat in ["::127.0.0.1", "::10.0.0.5", "::1.1.1.1"] {
            let ip: IpAddr = compat.parse().expect(compat);
            assert!(
                !DestinationPolicy::PublicOnly.admits(ip),
                "{compat} must not be public"
            );
        }
    }

    #[test]
    fn the_metadata_address_is_never_public() {
        let metadata: IpAddr = "169.254.169.254".parse().unwrap();
        assert!(!DestinationPolicy::PublicOnly.admits(metadata));
        assert!(!DestinationPolicy::PublicOrLoopback.admits(metadata));
        assert!(!DestinationPolicy::AllowPrivate.admits(metadata));
        assert!(DestinationPolicy::Unrestricted.admits(metadata));
    }

    #[test]
    fn special_purpose_ranges_are_refused_and_public_unicast_is_not() {
        let refused = [
            "0.0.0.0",
            "10.0.0.5",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1", // carrier NAT
            "192.0.0.1",  // IETF protocol assignments
            "198.18.0.1", // benchmarking
            "224.0.0.1",  // multicast
            "240.0.0.1",  // reserved
            "255.255.255.255",
            "::",
            "::1",
            "fc00::1",     // unique local
            "fe80::1",     // link local
            "fec0::1",     // site local (deprecated but still routed)
            "ff02::1",     // multicast
            "2001:db8::1", // documentation
        ];
        for r in refused {
            let ip: IpAddr = r.parse().expect(r);
            assert!(
                !DestinationPolicy::PublicOnly.admits(ip),
                "{r} must not be public"
            );
        }
        for ok in ["1.1.1.1", "93.184.216.34", "2606:4700:4700::1111"] {
            let ip: IpAddr = ok.parse().expect(ok);
            assert!(
                DestinationPolicy::PublicOnly.admits(ip),
                "{ok} must be public"
            );
        }
    }

    /// Loopback is admitted by the outbound door's default but refused
    /// by `PublicOnly` — the setting a host exposing agent-supplied URLs
    /// should choose.
    #[test]
    fn policy_ladder_is_ordered() {
        let loopback: IpAddr = "127.0.0.1".parse().unwrap();
        let private: IpAddr = "192.168.1.1".parse().unwrap();
        let public: IpAddr = "1.1.1.1".parse().unwrap();

        assert!(!DestinationPolicy::PublicOnly.admits(loopback));
        assert!(DestinationPolicy::PublicOrLoopback.admits(loopback));
        assert!(DestinationPolicy::AllowPrivate.admits(loopback));

        assert!(!DestinationPolicy::PublicOrLoopback.admits(private));
        assert!(DestinationPolicy::AllowPrivate.admits(private));

        // Deprecated IPv6 site-local is an operator's own network: never
        // public, but reachable under `AllowPrivate` so a self-hosted node
        // there is not collateral damage.
        let site_local: IpAddr = "fec0::1".parse().unwrap();
        assert!(!DestinationPolicy::PublicOnly.admits(site_local));
        assert!(!DestinationPolicy::PublicOrLoopback.admits(site_local));
        assert!(DestinationPolicy::AllowPrivate.admits(site_local));

        for p in [
            DestinationPolicy::PublicOnly,
            DestinationPolicy::PublicOrLoopback,
            DestinationPolicy::AllowPrivate,
            DestinationPolicy::Unrestricted,
        ] {
            assert!(p.admits(public), "{p:?} must admit public unicast");
        }
    }
}
