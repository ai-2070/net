//! CAIP-2 chain identifiers and CAIP-19 asset identifiers.
//!
//! An asset id names a specific issued asset on a specific chain — native
//! vs bridged vs wrapped are all distinct unless a participant's policy
//! declares equivalence (that policy lives in the registry, not here).
//!
//! Comparison is exact and case-sensitive, per CAIP. Two spellings of the
//! same on-chain asset (e.g. EIP-55 checksum variants) are *different ids*
//! here; normalization, if any, is a registry-policy decision. The
//! cross-language confusion vectors pin this: a verifier that
//! case-normalizes or trims will fail them.

use serde::{Deserialize, Serialize};

/// Errors from CAIP identifier parsing.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CaipError {
    #[error("CAIP-2 chain id `{0}` is malformed: {1}")]
    Chain(String, &'static str),
    #[error("CAIP-19 asset id `{0}` is malformed: {1}")]
    Asset(String, &'static str),
}

/// A CAIP-2 chain (network) identifier: `namespace:reference`.
///
/// - namespace: `[-a-z0-9]{3,8}`
/// - reference: `[-_a-zA-Z0-9]{1,32}`
///
/// Examples: `eip155:8453` (Base), `solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp`,
/// and the P0 mock network `mock:net`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ChainId {
    canonical: String,
    namespace_len: usize,
}

impl ChainId {
    /// Parse and validate a CAIP-2 string.
    pub fn parse(s: &str) -> Result<Self, CaipError> {
        let err = |reason| CaipError::Chain(s.to_string(), reason);
        let (namespace, reference) = s.split_once(':').ok_or_else(|| err("missing `:`"))?;
        if !(3..=8).contains(&namespace.len()) {
            return Err(err("namespace must be 3-8 chars"));
        }
        if !namespace
            .bytes()
            .all(|b| b == b'-' || b.is_ascii_lowercase() || b.is_ascii_digit())
        {
            return Err(err("namespace chars must be [-a-z0-9]"));
        }
        if !(1..=32).contains(&reference.len()) {
            return Err(err("reference must be 1-32 chars"));
        }
        if !reference
            .bytes()
            .all(|b| b == b'-' || b == b'_' || b.is_ascii_alphanumeric())
        {
            return Err(err("reference chars must be [-_a-zA-Z0-9]"));
        }
        Ok(Self {
            canonical: s.to_string(),
            namespace_len: namespace.len(),
        })
    }

    /// The chain namespace (e.g. `eip155`, `solana`, `mock`).
    pub fn namespace(&self) -> &str {
        &self.canonical[..self.namespace_len]
    }

    /// The chain reference (e.g. `8453`).
    pub fn reference(&self) -> &str {
        &self.canonical[self.namespace_len + 1..]
    }

    /// The full `namespace:reference` form.
    pub fn as_str(&self) -> &str {
        &self.canonical
    }
}

impl std::fmt::Display for ChainId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.canonical)
    }
}

impl Serialize for ChainId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.canonical)
    }
}

impl<'de> Deserialize<'de> for ChainId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Self::parse(&s).map_err(serde::de::Error::custom)
    }
}

/// A CAIP-19 asset identifier:
/// `chain_id/asset_namespace:asset_reference[/token_id]`.
///
/// - asset_namespace: `[-a-z0-9]{3,8}`
/// - asset_reference: `[-.%a-zA-Z0-9]{1,128}`
/// - token_id (optional): `[-.%a-zA-Z0-9]{1,78}`
///
/// Examples: `eip155:8453/erc20:0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913`
/// (USDC on Base), and the P0 mock asset `mock:net/token:musd`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AssetId {
    canonical: String,
    chain: ChainId,
    namespace_range: (usize, usize),
    reference_range: (usize, usize),
    token_id_start: Option<usize>,
}

fn is_asset_char(b: u8) -> bool {
    b == b'-' || b == b'.' || b == b'%' || b.is_ascii_alphanumeric()
}

impl AssetId {
    /// Parse and validate a CAIP-19 string.
    pub fn parse(s: &str) -> Result<Self, CaipError> {
        let err = |reason| CaipError::Asset(s.to_string(), reason);
        let (chain_part, asset_part) = s
            .split_once('/')
            .ok_or_else(|| err("missing `/` between chain and asset"))?;
        let chain =
            ChainId::parse(chain_part).map_err(|_| err("chain segment is not valid CAIP-2"))?;

        let (asset_main, token_id) = match asset_part.split_once('/') {
            Some((main, tid)) => (main, Some(tid)),
            None => (asset_part, None),
        };
        let (namespace, reference) = asset_main
            .split_once(':')
            .ok_or_else(|| err("missing `:` in asset segment"))?;
        if !(3..=8).contains(&namespace.len()) {
            return Err(err("asset namespace must be 3-8 chars"));
        }
        if !namespace
            .bytes()
            .all(|b| b == b'-' || b.is_ascii_lowercase() || b.is_ascii_digit())
        {
            return Err(err("asset namespace chars must be [-a-z0-9]"));
        }
        if !(1..=128).contains(&reference.len()) {
            return Err(err("asset reference must be 1-128 chars"));
        }
        if !reference.bytes().all(is_asset_char) {
            return Err(err("asset reference chars must be [-.%a-zA-Z0-9]"));
        }
        if let Some(tid) = token_id {
            if !(1..=78).contains(&tid.len()) {
                return Err(err("token id must be 1-78 chars"));
            }
            if !tid.bytes().all(is_asset_char) {
                return Err(err("token id chars must be [-.%a-zA-Z0-9]"));
            }
        }

        let ns_start = chain_part.len() + 1;
        let ns_end = ns_start + namespace.len();
        let ref_start = ns_end + 1;
        let ref_end = ref_start + reference.len();
        Ok(Self {
            canonical: s.to_string(),
            chain,
            namespace_range: (ns_start, ns_end),
            reference_range: (ref_start, ref_end),
            token_id_start: token_id.map(|_| ref_end + 1),
        })
    }

    /// The chain this asset lives on.
    pub fn chain(&self) -> &ChainId {
        &self.chain
    }

    /// The asset namespace (e.g. `erc20`, `token`).
    pub fn namespace(&self) -> &str {
        &self.canonical[self.namespace_range.0..self.namespace_range.1]
    }

    /// The asset reference (e.g. the token contract address).
    pub fn reference(&self) -> &str {
        &self.canonical[self.reference_range.0..self.reference_range.1]
    }

    /// The optional token id (e.g. an NFT id).
    pub fn token_id(&self) -> Option<&str> {
        self.token_id_start.map(|start| &self.canonical[start..])
    }

    /// The full CAIP-19 form.
    pub fn as_str(&self) -> &str {
        &self.canonical
    }
}

impl std::fmt::Display for AssetId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.canonical)
    }
}

impl Serialize for AssetId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.canonical)
    }
}

impl<'de> Deserialize<'de> for AssetId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Self::parse(&s).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_real_and_mock_chain_ids() {
        for ok in [
            "eip155:8453",
            "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp",
            "mock:net",
        ] {
            let c = ChainId::parse(ok).unwrap();
            assert_eq!(c.as_str(), ok);
        }
        let c = ChainId::parse("eip155:8453").unwrap();
        assert_eq!(c.namespace(), "eip155");
        assert_eq!(c.reference(), "8453");
    }

    #[test]
    fn rejects_malformed_chain_ids() {
        for bad in [
            "",
            "eip155",                // no colon
            "EIP155:1",              // uppercase namespace
            "ei:1",                  // namespace too short
            "verylongns:1",          // namespace too long
            "eip155:",               // empty reference
            "eip155:a b",            // space in reference
            "eip155:8453/erc20:0x0", // that's a CAIP-19
        ] {
            assert!(ChainId::parse(bad).is_err(), "should reject {bad:?}");
        }
    }

    #[test]
    fn parses_asset_ids() {
        let a =
            AssetId::parse("eip155:8453/erc20:0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913").unwrap();
        assert_eq!(a.chain().as_str(), "eip155:8453");
        assert_eq!(a.namespace(), "erc20");
        assert_eq!(a.reference(), "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913");
        assert_eq!(a.token_id(), None);

        let nft = AssetId::parse("eip155:1/erc721:0xabc/42").unwrap();
        assert_eq!(nft.token_id(), Some("42"));

        let mock = AssetId::parse("mock:net/token:musd").unwrap();
        assert_eq!(mock.chain().namespace(), "mock");
    }

    #[test]
    fn rejects_malformed_asset_ids() {
        for bad in [
            "",
            "eip155:8453",            // chain only
            "eip155:8453/erc20",      // no asset reference
            "eip155:8453/ERC20:0x0",  // uppercase asset namespace
            "notachain/erc20:0x0",    // bad chain segment
            "eip155:8453/erc20:0x 0", // space
        ] {
            assert!(AssetId::parse(bad).is_err(), "should reject {bad:?}");
        }
    }

    #[test]
    fn comparison_is_case_sensitive() {
        let lower = AssetId::parse("eip155:8453/erc20:0xabc").unwrap();
        let upper = AssetId::parse("eip155:8453/erc20:0xABC").unwrap();
        assert_ne!(
            lower, upper,
            "CAIP ids compare exact; equivalence is registry policy"
        );
    }

    #[test]
    fn serde_round_trips_as_string() {
        let c = ChainId::parse("eip155:8453").unwrap();
        assert_eq!(serde_json::to_string(&c).unwrap(), "\"eip155:8453\"");
        let back: ChainId = serde_json::from_str("\"eip155:8453\"").unwrap();
        assert_eq!(back, c);
        assert!(serde_json::from_str::<ChainId>("\"EIP155:1\"").is_err());
    }
}
