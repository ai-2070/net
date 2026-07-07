//! Delegated agent identity — `root → machine → gateway → subagent`
//! delegation chains for capability-invocation **attribution** (Hermes
//! integration plan, Phase 3).
//!
//! # Why this exists
//!
//! A capability invoke over the mesh is authorized at the provider by an
//! *owner-scope* check on the AEAD-verified caller origin (see the wrap
//! adapter's `OwnerScope`). That answers "may this node call?" but not
//! "*which* agent, under whose authority, is calling?" — and it can't be
//! narrowed or revoked per-agent. This module builds the missing piece: a
//! [`DelegationChain`] anchored at the user's **root** identity, delegated
//! down through a **machine** identity to a **gateway** agent identity
//! (and optionally to per-task **subagents**), so that:
//!
//! * a provider's audit can name the terminal agent (which gateway / which
//!   subagent) rather than just the machine, and
//! * revoking one link transitively kills everything below it **without
//!   touching a sibling** — revoke machine `M`'s floor and `M`'s gateway
//!   chain (and its subagents) fail to verify, while machine `M2`'s chain
//!   is untouched.
//!
//! # Reused machinery, not new crypto
//!
//! The chain *is* a [`TokenChain`] of ed25519 [`PermissionToken`](crate::PermissionToken)s — the
//! exact type that already gates channel subscribe/publish. Verification
//! ([`TokenChain::verify_authorizes`]) enforces root-anchoring, leaf
//! binding, per-link validity + revocation, delegation continuity, and
//! monotonic scope-narrowing. This module only adds the *convention*:
//! which channel the delegation binds to, which scope stands in for "may
//! invoke", and the `root → machine → gateway → subagent` shape — so every
//! binding (Python, Node, the shim) derives and verifies chains
//! identically.
//!
//! # Revocation model
//!
//! Two tiers, both already supported by the core:
//! * **Gateway / machine** — bump the issuer's floor in the shared
//!   [`RevocationRegistry`]. Because every link inherits its parent's
//!   `issuer_generation` and `verify_authorizes` revocation-checks *every*
//!   link, revoking the machine's floor invalidates the whole subtree
//!   immediately.
//! * **Individual subagent** — the SDK's documented v1 answer: short TTLs
//!   plus stop renewing that subagent's leaf. There is no per-token CRL.

use std::time::Duration;

use net::adapter::net::identity::{EntityId, TokenError, TokenScope};
use net::adapter::net::ChannelName;

use crate::identity::Identity;

// Re-export the core types a caller composes with, so `net_sdk::delegation`
// is a complete surface without reaching into the core crate. These names
// are also used unqualified throughout this module.
pub use net::adapter::net::identity::{RevocationRegistry, TokenChain, MAX_CHAIN_DEPTH};

/// The well-known channel every gateway delegation binds to.
///
/// Capability invocations aren't channel pub/sub, but [`TokenChain`]
/// authority is channel-scoped, so the whole delegation tree binds to one
/// conventional channel that the deriver and the verifier both agree on.
/// It is never actually published to.
pub const GATEWAY_DELEGATION_CHANNEL: &str = "net.mcp.capability-invoke";

/// The scope that stands in for "may invoke a mesh capability".
///
/// A capability invoke is a request/response the caller initiates over the
/// mesh — semantically a read the caller subscribes to — so `SUBSCRIBE` is
/// the natural bit. Verification checks this action against every link, so
/// a leaf can never claim authority an ancestor didn't grant.
pub const INVOKE_ACTION: TokenScope = TokenScope::SUBSCRIBE;

/// Default delegation depth minted at the root. `root → machine → gateway
/// → subagent` is three hops; 4 leaves one spare so a subagent *could*
/// spawn a child if a future slice wants it (the leaf drops `DELEGATE`
/// today, so it can't).
pub const DEFAULT_DELEGATION_DEPTH: u8 = 4;

/// blake3 KDF context — fixed, unique, and versioned so a future change to
/// the derivation is a distinct namespace rather than a silent collision.
const CHILD_SEED_CONTEXT: &str = "net-mesh-sdk delegation child-seed v1";

/// Derive a stable child ed25519 seed from a parent seed and a label.
///
/// Deterministic (`blake3::derive_key`) so a machine / gateway identity is
/// reproducible across restarts from the root seed alone — no extra
/// persistence, and every process that knows the root derives the same
/// child. The label namespaces siblings (`"machine:hostA"` vs
/// `"gateway:hostA:hermes"`). The root seed is secret material; treat the
/// returned bytes the same way.
pub fn derive_child_seed(parent_seed: &[u8; 32], label: &str) -> [u8; 32] {
    // Feed the secret seed straight into the derive-key hasher via incremental
    // `update`s — NOT via an intermediate `Vec` that would linger in the heap
    // un-zeroized after this returns (swap / core-dump / heap-inspection
    // exposure). `update(seed).update(label)` hashes exactly `seed ‖ label`, so
    // the derived value is identical to the prior `derive_key(ctx, seed‖label)`.
    let mut hasher = blake3::Hasher::new_derive_key(CHILD_SEED_CONTEXT);
    hasher.update(parent_seed);
    hasher.update(label.as_bytes());
    *hasher.finalize().as_bytes()
}

/// An ordered `root → … → leaf` delegation chain that attributes a
/// capability invocation to the terminal (leaf) agent identity.
///
/// Cheap to clone (a `Vec<PermissionToken>` of 169-byte tokens). Serialize
/// with [`Self::to_bytes`] to carry it, `verify` against the shared
/// [`RevocationRegistry`] to check it's still live.
#[derive(Clone, Debug)]
pub struct DelegationChain {
    chain: TokenChain,
}

impl DelegationChain {
    /// Build a `root → machine → gateway` chain.
    ///
    /// `root` and `machine` must own their signing keys — each signs its
    /// own delegation. `gateway` is the entity id the gateway agent node
    /// runs as (its keypair stays with the gateway). All three links bind
    /// to [`GATEWAY_DELEGATION_CHANNEL`] with `SUBSCRIBE | DELEGATE` so the
    /// gateway can further delegate to subagents.
    ///
    /// `ttl` is the root grant's lifetime; the delegated link inherits the
    /// root's `not_after`, so the whole chain expires together. Renew by
    /// re-deriving before expiry.
    pub fn derive_gateway(
        root: &Identity,
        machine: &Identity,
        gateway: &EntityId,
        ttl: Duration,
        max_depth: u8,
    ) -> Result<Self, TokenError> {
        let channel = Self::channel();
        let delegable = INVOKE_ACTION.union(TokenScope::DELEGATE);

        // root → machine (root signs).
        let root_to_machine = root.try_issue_token(
            machine.entity_id().clone(),
            delegable,
            &channel,
            ttl,
            max_depth,
        )?;

        // machine → gateway (machine signs; must be the root token's subject).
        let machine_to_gateway =
            root_to_machine.delegate(machine.keypair(), gateway.clone(), delegable)?;

        Ok(Self {
            chain: TokenChain {
                tokens: vec![root_to_machine, machine_to_gateway],
            },
        })
    }

    /// Build a single-link `root → device` delegation to an **externally
    /// generated** device identity (enrollment — Hermes V2 Phase 1).
    ///
    /// Unlike [`Self::derive_gateway`], the device keypair is **not** derived
    /// from the root seed: the device generated it locally and only presented
    /// its [`EntityId`] (public key) during the enrollment handshake — keys
    /// never travel. The root signs a delegable grant
    /// ([`INVOKE_ACTION`]` | DELEGATE`) so the device can locally extend the
    /// chain to its own gateway ([`Self::extend_delegate`]) and per-task
    /// subagents ([`Self::extend_to_subagent`]) without going back to the
    /// root.
    ///
    /// This is the primitive that deprecates the shared-identity-file pattern
    /// (root on every box): the root stays on one machine, each device holds a
    /// delegation to *its own* key, and revoking one device — bumping the
    /// **device's** floor in the [`RevocationRegistry`] — kills that device's
    /// gateway subtree without touching a sibling, exactly as revoking a
    /// machine does in [`Self::derive_gateway`].
    ///
    /// `ttl` is the grant's lifetime; renew by re-issuing before expiry.
    pub fn derive_device(
        root: &Identity,
        device: &EntityId,
        ttl: Duration,
        max_depth: u8,
    ) -> Result<Self, TokenError> {
        let channel = Self::channel();
        let delegable = INVOKE_ACTION.union(TokenScope::DELEGATE);
        let root_to_device =
            root.try_issue_token(device.clone(), delegable, &channel, ttl, max_depth)?;
        Ok(Self {
            chain: TokenChain {
                tokens: vec![root_to_device],
            },
        })
    }

    /// Extend this chain with a `… → child` link that **keeps `DELEGATE`**,
    /// signed by the current leaf's owner (`leaf_signer`, whose entity id must
    /// equal this chain's leaf subject).
    ///
    /// This is the delegable sibling of [`Self::extend_to_subagent`]: use it
    /// for an intermediate link that must itself delegate further — e.g. an
    /// enrolled **device** ([`Self::derive_device`]) extending to its
    /// **gateway** agent, which then extends to its own subagents. The child
    /// keeps [`INVOKE_ACTION`]` | DELEGATE`; use [`Self::extend_to_subagent`]
    /// for a terminal (non-delegating) leaf. Returns a *new* chain; `self` is
    /// unchanged.
    pub fn extend_delegate(
        &self,
        leaf_signer: &Identity,
        child: &EntityId,
    ) -> Result<Self, TokenError> {
        let parent = self.chain.tokens.last().ok_or(TokenError::InvalidFormat)?;
        let delegable = INVOKE_ACTION.union(TokenScope::DELEGATE);
        let child_tok = parent.delegate(leaf_signer.keypair(), child.clone(), delegable)?;
        let mut tokens = self.chain.tokens.clone();
        tokens.push(child_tok);
        Ok(Self {
            chain: TokenChain { tokens },
        })
    }

    /// Extend this chain with a `… → subagent` link, signed by the current
    /// leaf's owner (`leaf_signer`, whose entity id must equal this chain's
    /// leaf subject — e.g. the gateway extending to one of its subagents).
    ///
    /// The subagent link drops `DELEGATE` (a subagent can't re-delegate)
    /// but keeps [`INVOKE_ACTION`], so the subagent's own invocations
    /// verify and are individually attributable. Returns a *new* chain;
    /// `self` is unchanged.
    pub fn extend_to_subagent(
        &self,
        leaf_signer: &Identity,
        subagent: &EntityId,
    ) -> Result<Self, TokenError> {
        let parent = self.chain.tokens.last().ok_or(TokenError::InvalidFormat)?;
        let child = parent.delegate(leaf_signer.keypair(), subagent.clone(), INVOKE_ACTION)?;
        let mut tokens = self.chain.tokens.clone();
        tokens.push(child);
        Ok(Self {
            chain: TokenChain { tokens },
        })
    }

    /// Verify the chain still authorizes an invocation by `presenter`,
    /// anchored at `root`, honoring `revocation`.
    ///
    /// Returns `Ok(())` when the chain roots at `root`, its leaf subject is
    /// `presenter`, no link is expired or revoked, and delegation
    /// continuity + scope hold. `skew_secs` tolerates clock drift on the
    /// time-bound checks (0 = strict).
    pub fn verify(
        &self,
        presenter: &EntityId,
        root: &EntityId,
        revocation: &RevocationRegistry,
        skew_secs: u64,
    ) -> Result<(), TokenError> {
        let channel = Self::channel();
        self.chain.verify_authorizes(
            INVOKE_ACTION,
            channel.hash(),
            presenter,
            std::slice::from_ref(root),
            revocation,
            skew_secs,
        )
    }

    /// The subject entity ids of each link, root-to-leaf. `subjects()[0]`
    /// is the machine (root's grantee); the last is the terminal agent.
    pub fn subjects(&self) -> Vec<EntityId> {
        self.chain
            .tokens
            .iter()
            .map(|t| t.subject.clone())
            .collect()
    }

    /// The terminal (leaf) subject — the agent this chain attributes to
    /// (the gateway, or a subagent after [`Self::extend_to_subagent`]).
    pub fn leaf(&self) -> EntityId {
        self.chain
            .tokens
            .last()
            .map(|t| t.subject.clone())
            .expect("a DelegationChain always has at least one link")
    }

    /// The root issuer the chain anchors at (`tokens[0].issuer`).
    pub fn root(&self) -> EntityId {
        self.chain
            .tokens
            .first()
            .map(|t| t.issuer.clone())
            .expect("a DelegationChain always has at least one link")
    }

    /// Unix-seconds the chain expires — the **earliest** link's `not_after`
    /// (the whole chain is only live while every link is). For a bare
    /// `root → device` grant this is that single grant's expiry.
    pub fn expires_at(&self) -> u64 {
        self.chain
            .tokens
            .iter()
            .map(|t| t.not_after)
            .min()
            .expect("a DelegationChain always has at least one link")
    }

    /// Number of delegation links (2 for a bare gateway chain, +1 per
    /// subagent hop).
    pub fn len(&self) -> usize {
        self.chain.tokens.len()
    }

    /// Always `false` — a chain is never empty by construction; provided so
    /// `len()` doesn't trip the `len_without_is_empty` lint.
    pub fn is_empty(&self) -> bool {
        self.chain.tokens.is_empty()
    }

    /// Serialize to the wire (a `TokenChain` blob) for carriage on an
    /// invoke or hand-off to another process.
    pub fn to_bytes(&self) -> Vec<u8> {
        self.chain.to_bytes()
    }

    /// Parse a serialized chain. Rejects an empty chain, a link count past
    /// [`MAX_CHAIN_DEPTH`], or trailing garbage.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, TokenError> {
        Ok(Self {
            chain: TokenChain::from_bytes(bytes)?,
        })
    }

    /// Borrow the underlying [`TokenChain`] (for callers that verify with a
    /// custom action/channel, or carry it on a channel op).
    pub fn inner(&self) -> &TokenChain {
        &self.chain
    }

    fn channel() -> ChannelName {
        ChannelName::new(GATEWAY_DELEGATION_CHANNEL)
            .expect("GATEWAY_DELEGATION_CHANNEL is a valid channel name")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root_machine_gateway() -> (Identity, Identity, Identity) {
        // Derive machine + gateway from the root seed so the identities are
        // exactly what a real deployment would use.
        let root = Identity::generate();
        let root_seed = root.to_bytes();
        let machine = Identity::from_seed(derive_child_seed(&root_seed, "machine:test-host"));
        let gateway =
            Identity::from_seed(derive_child_seed(&root_seed, "gateway:test-host:hermes"));
        (root, machine, gateway)
    }

    #[test]
    fn gateway_chain_derives_and_verifies() {
        let (root, machine, gateway) = root_machine_gateway();
        let chain = DelegationChain::derive_gateway(
            &root,
            &machine,
            gateway.entity_id(),
            Duration::from_secs(3600),
            DEFAULT_DELEGATION_DEPTH,
        )
        .unwrap();
        let reg = RevocationRegistry::new();

        assert_eq!(chain.len(), 2);
        assert_eq!(&chain.root(), root.entity_id());
        assert_eq!(&chain.leaf(), gateway.entity_id());
        // The gateway (leaf) presents and verifies against the root.
        chain
            .verify(gateway.entity_id(), root.entity_id(), &reg, 0)
            .expect("fresh gateway chain must verify");
    }

    #[test]
    fn wrong_presenter_is_rejected() {
        let (root, machine, gateway) = root_machine_gateway();
        let chain = DelegationChain::derive_gateway(
            &root,
            &machine,
            gateway.entity_id(),
            Duration::from_secs(3600),
            DEFAULT_DELEGATION_DEPTH,
        )
        .unwrap();
        let reg = RevocationRegistry::new();
        // The machine can't present the gateway's chain — leaf binding fails.
        assert!(chain
            .verify(machine.entity_id(), root.entity_id(), &reg, 0)
            .is_err());
    }

    #[test]
    fn wrong_root_is_rejected() {
        let (root, machine, gateway) = root_machine_gateway();
        let chain = DelegationChain::derive_gateway(
            &root,
            &machine,
            gateway.entity_id(),
            Duration::from_secs(3600),
            DEFAULT_DELEGATION_DEPTH,
        )
        .unwrap();
        let reg = RevocationRegistry::new();
        let stranger = Identity::generate();
        // A chain rooted elsewhere must not anchor at `stranger`.
        assert!(chain
            .verify(gateway.entity_id(), stranger.entity_id(), &reg, 0)
            .is_err());
    }

    #[test]
    fn subagent_extension_attributes_and_verifies() {
        let (root, machine, gateway) = root_machine_gateway();
        let chain = DelegationChain::derive_gateway(
            &root,
            &machine,
            gateway.entity_id(),
            Duration::from_secs(3600),
            DEFAULT_DELEGATION_DEPTH,
        )
        .unwrap();
        let subagent = Identity::generate();
        let sub_chain = chain
            .extend_to_subagent(&gateway, subagent.entity_id())
            .unwrap();
        let reg = RevocationRegistry::new();

        assert_eq!(sub_chain.len(), 3);
        assert_eq!(&sub_chain.leaf(), subagent.entity_id());
        // The subagent presents its own extended chain.
        sub_chain
            .verify(subagent.entity_id(), root.entity_id(), &reg, 0)
            .expect("subagent chain must verify");
        // The original gateway chain is untouched by the extension.
        chain
            .verify(gateway.entity_id(), root.entity_id(), &reg, 0)
            .expect("gateway chain unchanged");
    }

    #[test]
    fn revoking_the_machine_kills_gateway_and_subagents_but_not_a_sibling() {
        let root = Identity::generate();
        let root_seed = root.to_bytes();
        // Two machines under the same root.
        let m1 = Identity::from_seed(derive_child_seed(&root_seed, "machine:host1"));
        let g1 = Identity::from_seed(derive_child_seed(&root_seed, "gateway:host1:hermes"));
        let m2 = Identity::from_seed(derive_child_seed(&root_seed, "machine:host2"));
        let g2 = Identity::from_seed(derive_child_seed(&root_seed, "gateway:host2:hermes"));

        let ttl = Duration::from_secs(3600);
        let c1 = DelegationChain::derive_gateway(
            &root,
            &m1,
            g1.entity_id(),
            ttl,
            DEFAULT_DELEGATION_DEPTH,
        )
        .unwrap();
        let c2 = DelegationChain::derive_gateway(
            &root,
            &m2,
            g2.entity_id(),
            ttl,
            DEFAULT_DELEGATION_DEPTH,
        )
        .unwrap();
        let sub1 = Identity::generate();
        let c1_sub = c1.extend_to_subagent(&g1, sub1.entity_id()).unwrap();

        let reg = RevocationRegistry::new();
        // All live before revocation.
        assert!(c1.verify(g1.entity_id(), root.entity_id(), &reg, 0).is_ok());
        assert!(c1_sub
            .verify(sub1.entity_id(), root.entity_id(), &reg, 0)
            .is_ok());
        assert!(c2.verify(g2.entity_id(), root.entity_id(), &reg, 0).is_ok());

        // Revoke machine 1's gateway delegation: bump M1's floor above the
        // (generation-0) tokens it issued.
        reg.revoke_below(m1.entity_id(), 1);

        // Machine 1's gateway chain — and its subagent — now fail…
        assert!(
            c1.verify(g1.entity_id(), root.entity_id(), &reg, 0)
                .is_err(),
            "revoked gateway chain must fail"
        );
        assert!(
            c1_sub
                .verify(sub1.entity_id(), root.entity_id(), &reg, 0)
                .is_err(),
            "revoking the gateway must kill its subagents"
        );
        // …while machine 2's chain is untouched.
        assert!(
            c2.verify(g2.entity_id(), root.entity_id(), &reg, 0).is_ok(),
            "revoking one machine must not touch another machine's chain"
        );
    }

    #[test]
    fn child_seed_derivation_is_deterministic_and_label_separated() {
        let seed = [7u8; 32];
        assert_eq!(derive_child_seed(&seed, "a"), derive_child_seed(&seed, "a"));
        assert_ne!(derive_child_seed(&seed, "a"), derive_child_seed(&seed, "b"));
        assert_ne!(
            derive_child_seed(&seed, "a"),
            derive_child_seed(&[8u8; 32], "a")
        );
    }

    #[test]
    fn chain_round_trips_through_bytes() {
        let (root, machine, gateway) = root_machine_gateway();
        let chain = DelegationChain::derive_gateway(
            &root,
            &machine,
            gateway.entity_id(),
            Duration::from_secs(3600),
            DEFAULT_DELEGATION_DEPTH,
        )
        .unwrap();
        let bytes = chain.to_bytes();
        let parsed = DelegationChain::from_bytes(&bytes).unwrap();
        assert_eq!(parsed.leaf(), chain.leaf());
        assert_eq!(parsed.root(), chain.root());
        let reg = RevocationRegistry::new();
        parsed
            .verify(gateway.entity_id(), root.entity_id(), &reg, 0)
            .expect("round-tripped chain must verify");
    }

    #[test]
    fn device_chain_derives_and_verifies() {
        // Enrollment: the device generated its own keypair; only its entity id
        // crosses to the root, which signs a single root → device grant.
        let root = Identity::generate();
        let device = Identity::generate();
        let chain = DelegationChain::derive_device(
            &root,
            device.entity_id(),
            Duration::from_secs(3600),
            DEFAULT_DELEGATION_DEPTH,
        )
        .unwrap();
        let reg = RevocationRegistry::new();

        assert_eq!(chain.len(), 1);
        assert_eq!(&chain.root(), root.entity_id());
        assert_eq!(&chain.leaf(), device.entity_id());
        chain
            .verify(device.entity_id(), root.entity_id(), &reg, 0)
            .expect("fresh device chain must verify");
    }

    #[test]
    fn enrolled_device_extends_to_gateway_and_subagent() {
        // The enrolled device (root → device) locally extends to its gateway
        // keeping DELEGATE, and the gateway to a subagent — all from the
        // device's own key, never touching the root again.
        let root = Identity::generate();
        let device = Identity::generate();
        let device_chain = DelegationChain::derive_device(
            &root,
            device.entity_id(),
            Duration::from_secs(3600),
            DEFAULT_DELEGATION_DEPTH,
        )
        .unwrap();

        // The device's gateway is reproducible from the *device's* own seed —
        // no root seed needed, matching the "no extra persistence" property.
        let gateway = Identity::from_seed(derive_child_seed(&device.to_bytes(), "gateway:hermes"));
        let gw_chain = device_chain
            .extend_delegate(&device, gateway.entity_id())
            .unwrap();
        let subagent = Identity::generate();
        let sub_chain = gw_chain
            .extend_to_subagent(&gateway, subagent.entity_id())
            .unwrap();

        let reg = RevocationRegistry::new();
        assert_eq!(gw_chain.len(), 2);
        assert_eq!(sub_chain.len(), 3);
        assert_eq!(&gw_chain.root(), root.entity_id());
        gw_chain
            .verify(gateway.entity_id(), root.entity_id(), &reg, 0)
            .expect("device → gateway chain must verify");
        sub_chain
            .verify(subagent.entity_id(), root.entity_id(), &reg, 0)
            .expect("device → gateway → subagent chain must verify");
        // The gateway kept DELEGATE, so extending to the subagent was possible;
        // the subagent (terminal) dropped it — the round-trip above proves the
        // gateway could delegate.
    }

    #[test]
    fn revoking_a_device_kills_its_gateway_but_not_a_sibling() {
        // Two devices enrolled under one root; each runs a gateway. Revoking
        // one device (bumping *its* floor) kills its gateway subtree while the
        // other device is untouched — the Phase-1 acceptance ("revoke kills its
        // access"), reusing the Phase-3 revocation model unchanged.
        let root = Identity::generate();
        let ttl = Duration::from_secs(3600);

        let d1 = Identity::generate();
        let g1 = Identity::from_seed(derive_child_seed(&d1.to_bytes(), "gateway:hermes"));
        let c1 =
            DelegationChain::derive_device(&root, d1.entity_id(), ttl, DEFAULT_DELEGATION_DEPTH)
                .unwrap()
                .extend_delegate(&d1, g1.entity_id())
                .unwrap();

        let d2 = Identity::generate();
        let g2 = Identity::from_seed(derive_child_seed(&d2.to_bytes(), "gateway:hermes"));
        let c2 =
            DelegationChain::derive_device(&root, d2.entity_id(), ttl, DEFAULT_DELEGATION_DEPTH)
                .unwrap()
                .extend_delegate(&d2, g2.entity_id())
                .unwrap();

        let reg = RevocationRegistry::new();
        assert!(c1.verify(g1.entity_id(), root.entity_id(), &reg, 0).is_ok());
        assert!(c2.verify(g2.entity_id(), root.entity_id(), &reg, 0).is_ok());

        // Revoke device 1: raise d1's floor above the (generation-0) device →
        // gateway link it issued.
        reg.revoke_below(d1.entity_id(), 1);

        assert!(
            c1.verify(g1.entity_id(), root.entity_id(), &reg, 0)
                .is_err(),
            "revoking the device must kill its gateway chain"
        );
        assert!(
            c2.verify(g2.entity_id(), root.entity_id(), &reg, 0).is_ok(),
            "revoking one device must not touch a sibling's chain"
        );
    }
}
