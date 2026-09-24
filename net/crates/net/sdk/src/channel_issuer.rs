// SPDX-License-Identifier: MIT OR Apache-2.0
//! Delegated channel credentials (NET_CLI_PLAN_V3 V3-2A).
//!
//! An offline channel root signs ONE grant — a `DELEGATE`-bearing
//! [`PermissionToken`] for exactly one channel — to an issuing node's
//! identity. The node ([`ChannelLeafIssuer`]) mints each device a leaf from
//! it, so every device holds a full chain `root → node → device` that a
//! publisher whose `ChannelConfig::token_roots` names the root verifies link
//! by link. The root never goes online, and a leaf can never exceed the
//! grant: its rights are a subset of the grant's, its expiry is the grant's
//! (delegation copies it), and it cannot delegate further unless the grant
//! left depth for it.
//!
//! `ADMIN` and `WILDCARD` are refused outright: a channel link grants
//! publish/subscribe on one canonical channel, nothing else.

use net::adapter::net::identity::{EntityKeypair, PermissionToken, TokenChain, TokenScope};

use crate::identity::EntityId;

/// Why a grant or a leaf request was refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ChannelIssueError {
    /// The grant is not for this issuing identity.
    #[error("the grant is for another identity than this issuer")]
    NotTheGrantee,
    /// The grant cannot delegate (no DELEGATE right, or no depth left).
    #[error("the grant does not allow delegation")]
    CannotDelegate,
    /// The grant, or the request, carries ADMIN or WILDCARD.
    #[error("ADMIN and WILDCARD are never issued through a channel link")]
    Forbidden,
    /// The requested rights are not publish/subscribe within the grant.
    #[error("the requested rights exceed the grant or are not publish/subscribe")]
    Rights,
    /// The grant failed verification (signature or time window).
    #[error("the grant does not verify: {0}")]
    Grant(String),
}

/// The rights a channel link may carry.
pub const CHANNEL_LINK_RIGHTS: TokenScope =
    TokenScope::from_bits(TokenScope::PUBLISH.bits() | TokenScope::SUBSCRIBE.bits());

/// Mints device leaves for one channel from a root-signed grant.
#[derive(Clone)]
pub struct ChannelLeafIssuer {
    grant: PermissionToken,
    key: EntityKeypair,
}

impl core::fmt::Debug for ChannelLeafIssuer {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ChannelLeafIssuer")
            .field("root", &self.grant.issuer)
            .field("issuer", &self.grant.subject)
            .field("channel_hash", &self.grant.channel_hash)
            .field("scope", &self.grant.scope.bits())
            .finish()
    }
}

impl ChannelLeafIssuer {
    /// Issue as `key` under `grant` (the root's DELEGATE token naming
    /// `key`'s identity). The grant must verify, be delegable, and carry
    /// neither ADMIN nor WILDCARD.
    pub fn new(grant: PermissionToken, key: EntityKeypair) -> Result<Self, ChannelIssueError> {
        if &grant.subject != key.entity_id() {
            return Err(ChannelIssueError::NotTheGrantee);
        }
        if grant.scope.contains(TokenScope::ADMIN) || grant.scope.contains(TokenScope::WILDCARD) {
            return Err(ChannelIssueError::Forbidden);
        }
        if !grant.scope.contains(TokenScope::DELEGATE) || grant.delegation_depth == 0 {
            return Err(ChannelIssueError::CannotDelegate);
        }
        grant
            .verify()
            .map_err(|e| ChannelIssueError::Grant(e.to_string()))?;
        Ok(Self { grant, key })
    }

    /// The root the chains anchor at (a publisher's `token_roots` must name it).
    pub fn root(&self) -> &EntityId {
        &self.grant.issuer
    }

    /// The canonical channel hash the grant (and every leaf) is scoped to.
    pub fn channel_hash(&self) -> u64 {
        self.grant.channel_hash
    }

    /// The rights the grant allows a leaf (publish/subscribe only).
    pub fn grantable(&self) -> TokenScope {
        self.grant.scope.intersect(CHANNEL_LINK_RIGHTS)
    }

    /// When every leaf expires (the grant's own expiry).
    pub fn not_after(&self) -> u64 {
        self.grant.not_after
    }

    /// Mint `subject`'s chain `root → issuer → subject` carrying exactly
    /// `rights` (non-empty publish/subscribe, within the grant). The leaf
    /// cannot delegate further.
    pub fn issue(
        &self,
        subject: &EntityId,
        rights: TokenScope,
    ) -> Result<TokenChain, ChannelIssueError> {
        if rights.contains(TokenScope::ADMIN)
            || rights.contains(TokenScope::WILDCARD)
            || rights.contains(TokenScope::DELEGATE)
        {
            return Err(ChannelIssueError::Forbidden);
        }
        if rights.bits() == 0 || !self.grantable().contains(rights) {
            return Err(ChannelIssueError::Rights);
        }
        let leaf = self
            .grant
            .delegate(&self.key, subject.clone(), rights)
            .map_err(|e| ChannelIssueError::Grant(e.to_string()))?;
        Ok(TokenChain {
            tokens: vec![self.grant.clone(), leaf],
        })
    }
}
