//! Mixed-version negotiation and the old-relay fallback (plan
//! §4.11).
//!
//! Sensing support is advertised as a capability tag (the
//! `ACK_RANGES_CAPABILITY_TAG` gating pattern). Selection is per
//! candidate branch:
//!
//! ```text
//! next_hop(provider) advertises net.sensing@1
//!     → register interest through it (coalesced path)
//! next_hop does not, but the provider does
//!     → direct non-coalesced sensing over an end-to-end session
//!       (direct if one exists; else a routed session THROUGH the
//!       old relay — routed relays forward encrypted frames opaquely
//!       without dispatching their subprotocols)
//! the provider does not advertise net.sensing@1
//!     → Unknown
//! ```
//!
//! The fallback loses coalescing, never correctness. SI-0 test 10
//! (`tests/sensing_fallback.rs`) exercises the real dispatch path —
//! an actual old-version relay carrying the routed fallback frames —
//! not merely this selection function.

/// The capability tag a sensing-capable node advertises.
pub const SENSING_CAPABILITY_TAG: &str = "net.sensing@1";

/// How to sense one resolved provider branch (plan §4.11).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SensingPath {
    /// Register with `next_hop(provider)` — the coalesced tree path.
    Coalesced,
    /// End-to-end sensing of the provider over a direct or routed
    /// session; an old relay on the path forwards the frames
    /// opaquely. Loses coalescing, never correctness.
    DirectFallback,
    /// The provider itself cannot sense — the branch projects
    /// Unknown; no stream is attempted.
    Unsupported,
}

/// Select the sensing path for one branch from the capability tags
/// of the next hop toward the provider and of the provider itself
/// (both read from the local capability fold). When the provider IS
/// the next hop, the two tag sets coincide and a capable provider
/// yields the (degenerate, single-hop) coalesced path.
pub fn select_sensing_path(next_hop_tags: &[String], provider_tags: &[String]) -> SensingPath {
    let advertises = |tags: &[String]| tags.iter().any(|tag| tag == SENSING_CAPABILITY_TAG);
    if !advertises(provider_tags) {
        return SensingPath::Unsupported;
    }
    if advertises(next_hop_tags) {
        SensingPath::Coalesced
    } else {
        SensingPath::DirectFallback
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(with_sensing: bool) -> Vec<String> {
        let mut out = vec!["hardware.gpu".to_string()];
        if with_sensing {
            out.push(SENSING_CAPABILITY_TAG.to_string());
        }
        out
    }

    #[test]
    fn selection_covers_all_three_arms() {
        // Capable hop + capable provider → coalesce.
        assert_eq!(
            select_sensing_path(&tags(true), &tags(true)),
            SensingPath::Coalesced,
        );
        // Old relay, capable provider → routed end-to-end fallback.
        assert_eq!(
            select_sensing_path(&tags(false), &tags(true)),
            SensingPath::DirectFallback,
        );
        // Incapable provider → Unknown, regardless of the hop.
        assert_eq!(
            select_sensing_path(&tags(true), &tags(false)),
            SensingPath::Unsupported,
        );
        assert_eq!(
            select_sensing_path(&tags(false), &tags(false)),
            SensingPath::Unsupported,
        );
        // Provider IS the next hop: degenerate coalesced path.
        let provider = tags(true);
        assert_eq!(
            select_sensing_path(&provider, &provider),
            SensingPath::Coalesced,
        );
    }
}
