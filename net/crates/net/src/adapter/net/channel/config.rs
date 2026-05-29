//! Channel configuration and visibility.
//!
//! Channel policy uses the existing capability system (`CapabilityFilter`)
//! for access rules, combined with L1 permission tokens. This avoids
//! building a separate rule engine.

use super::name::{ChannelHash, ChannelId};
use crate::adapter::net::behavior::capability::{CapabilityFilter, CapabilitySet};
use crate::adapter::net::identity::{EntityId, RevocationRegistry, TokenChain, TokenScope};
use dashmap::DashMap;

/// Channel visibility scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Visibility {
    /// Packets never leave the subnet.
    SubnetLocal,
    /// Visible to the parent subnet but not siblings.
    ParentVisible,
    /// Explicitly exported to specific target subnets.
    Exported,
    /// Visible everywhere, no subnet restriction.
    #[default]
    Global,
}

/// Channel configuration with capability-based access control.
///
/// Authorization flow:
/// 1. Node announces capabilities via `CapabilityAd`
/// 2. If `publish_caps` is set, node's `CapabilitySet` must match the filter
/// 3. If `require_token` is true, node must also have a valid `PermissionToken`
/// 4. On success, `(origin_hash, channel_hash)` is inserted into the `AuthGuard`
#[derive(Debug, Clone)]
pub struct ChannelConfig {
    /// Channel identity (name + hash).
    pub channel_id: ChannelId,
    /// Visibility scope for subnet routing.
    pub visibility: Visibility,
    /// Capability requirements for publishing. `None` = any node can publish.
    pub publish_caps: Option<CapabilityFilter>,
    /// Capability requirements for subscribing. `None` = any node can subscribe.
    pub subscribe_caps: Option<CapabilityFilter>,
    /// Whether a valid `PermissionToken` is required (in addition to capabilities).
    pub require_token: bool,
    /// Entities whose signature roots a valid token chain for this
    /// channel — the channel's root(s) of trust.
    ///
    /// When `require_token` is set, a presented [`TokenChain`] is only
    /// honored if its root link (`tokens[0].issuer`) is one of these
    /// entities. This is the anchor the bare-token path lacked: without
    /// it `check`/`can_subscribe` only verified a token was internally
    /// self-consistent (the named issuer signed it), so any peer could
    /// self-issue `issuer = subject = self` and pass. An empty
    /// `token_roots` combined with `require_token = true` **fails
    /// closed** — there is no authority a chain could anchor to, so
    /// nothing is authorized.
    pub token_roots: Vec<EntityId>,
    /// Default priority level for this channel's packets (0 = lowest).
    pub priority: u8,
    /// Default reliability mode for streams on this channel.
    pub reliable: bool,
    /// Optional rate limit in packets per second.
    pub max_rate_pps: Option<u32>,
}

impl ChannelConfig {
    /// Create a new channel config with defaults (open access, global visibility).
    pub fn new(channel_id: ChannelId) -> Self {
        Self {
            channel_id,
            visibility: Visibility::default(),
            publish_caps: None,
            subscribe_caps: None,
            require_token: false,
            token_roots: Vec::new(),
            priority: 0,
            reliable: false,
            max_rate_pps: None,
        }
    }

    /// Set visibility.
    pub fn with_visibility(mut self, visibility: Visibility) -> Self {
        self.visibility = visibility;
        self
    }

    /// Set capability requirements for publishing.
    pub fn with_publish_caps(mut self, filter: CapabilityFilter) -> Self {
        self.publish_caps = Some(filter);
        self
    }

    /// Set capability requirements for subscribing.
    pub fn with_subscribe_caps(mut self, filter: CapabilityFilter) -> Self {
        self.subscribe_caps = Some(filter);
        self
    }

    /// Require a valid permission token.
    pub fn with_require_token(mut self, require: bool) -> Self {
        self.require_token = require;
        self
    }

    /// Require a token chain rooted at one of `roots`. Sets
    /// `require_token = true` and installs the channel's authorizing
    /// root(s). This is the safe way to turn on token enforcement —
    /// `with_require_token(true)` alone (no roots) fails every
    /// authorization closed, since a chain has no authority to anchor
    /// to.
    pub fn with_token_roots(mut self, roots: Vec<EntityId>) -> Self {
        self.require_token = true;
        self.token_roots = roots;
        self
    }

    /// Whether this channel enforces token authorization.
    ///
    /// Enforcement is on when `require_token` is set **or** any
    /// `token_roots` are configured. Coupling the two means a config
    /// that names roots but forgot to flip `require_token` (e.g. built
    /// by struct literal or direct field assignment rather than
    /// [`Self::with_token_roots`]) still enforces, instead of silently
    /// admitting every peer — the fields are both public, so the
    /// invariant can't be guaranteed at construction. All token gates
    /// (subscribe, publish, the periodic sweep, the publish re-check)
    /// consult this rather than `require_token` directly.
    pub fn token_required(&self) -> bool {
        self.require_token || !self.token_roots.is_empty()
    }

    /// Set default priority.
    pub fn with_priority(mut self, priority: u8) -> Self {
        self.priority = priority;
        self
    }

    /// Set default reliability.
    pub fn with_reliable(mut self, reliable: bool) -> Self {
        self.reliable = reliable;
        self
    }

    /// Set rate limit.
    pub fn with_rate_limit(mut self, pps: u32) -> Self {
        self.max_rate_pps = Some(pps);
        self
    }

    /// Check if `entity_id` is authorized to publish on this channel,
    /// presenting `chain`.
    ///
    /// See [`Self::can_subscribe`] for the chain-verification contract;
    /// this is the `PUBLISH`-scope counterpart.
    pub fn can_publish(
        &self,
        node_caps: &CapabilitySet,
        entity_id: &EntityId,
        chain: Option<&TokenChain>,
        revocation: &RevocationRegistry,
        skew_secs: u64,
    ) -> bool {
        if let Some(ref filter) = self.publish_caps {
            if !filter.matches(node_caps) {
                return false;
            }
        }
        self.token_gate(TokenScope::PUBLISH, entity_id, chain, revocation, skew_secs)
    }

    /// Check if `entity_id` is authorized to subscribe to this channel,
    /// presenting `chain`.
    ///
    /// When `require_token` is set, `chain` must be a [`TokenChain`]
    /// that (a) roots at one of [`Self::token_roots`], (b) is bound at
    /// its leaf to `entity_id` (the AEAD-verified presenter), and (c)
    /// authorizes `SUBSCRIBE` on this channel at every link with no
    /// link revoked. A missing chain, an empty `token_roots`, or a
    /// chain that fails verification all reject — fail closed.
    pub fn can_subscribe(
        &self,
        node_caps: &CapabilitySet,
        entity_id: &EntityId,
        chain: Option<&TokenChain>,
        revocation: &RevocationRegistry,
        skew_secs: u64,
    ) -> bool {
        if let Some(ref filter) = self.subscribe_caps {
            if !filter.matches(node_caps) {
                return false;
            }
        }
        self.token_gate(
            TokenScope::SUBSCRIBE,
            entity_id,
            chain,
            revocation,
            skew_secs,
        )
    }

    /// Shared token-chain gate for the publish / subscribe checks.
    /// Returns `true` when token enforcement is off (capability filters
    /// already applied by the caller), else verifies the presented
    /// chain roots at one of `token_roots`. Fails closed when tokens
    /// are required but no roots are configured or no chain is
    /// presented.
    fn token_gate(
        &self,
        action: TokenScope,
        entity_id: &EntityId,
        chain: Option<&TokenChain>,
        revocation: &RevocationRegistry,
        skew_secs: u64,
    ) -> bool {
        if !self.token_required() {
            return true;
        }
        // No authorizing root → nothing can satisfy the gate. Fail
        // closed rather than (pre-fix) honoring any self-consistent
        // token.
        if self.token_roots.is_empty() {
            return false;
        }
        let Some(chain) = chain else {
            return false;
        };
        chain
            .verify_authorizes(
                action,
                self.channel_id.hash(),
                entity_id,
                &self.token_roots,
                revocation,
                skew_secs,
            )
            .is_ok()
    }

    /// Re-verify a previously-presented `SUBSCRIBE` chain against the
    /// current clock + revocation floors, anchored to this channel's
    /// roots. Shared by the periodic expiry sweep and the publish-time
    /// re-check so the root-anchoring contract (which roots, which
    /// action, which channel hash) lives in exactly one place instead
    /// of being re-threaded at each call site — where it had already
    /// started to diverge (`token_roots` vs. an `unwrap_or(&[])`
    /// fallback).
    pub fn reverify_subscribe(
        &self,
        chain: &TokenChain,
        entity_id: &EntityId,
        revocation: &RevocationRegistry,
        skew_secs: u64,
    ) -> bool {
        chain
            .verify_authorizes(
                TokenScope::SUBSCRIBE,
                self.channel_id.hash(),
                entity_id,
                &self.token_roots,
                revocation,
                skew_secs,
            )
            .is_ok()
    }

    /// Like [`Self::reverify_subscribe`] but skips the per-link ed25519
    /// signature verification — for callers re-checking a chain whose
    /// signatures already verified once (immutable tokens). Time
    /// bounds, revocation, anchoring, and scope are still re-checked.
    /// See [`TokenChain::verify_authorizes_presigned`].
    pub fn reverify_subscribe_presigned(
        &self,
        chain: &TokenChain,
        entity_id: &EntityId,
        revocation: &RevocationRegistry,
        skew_secs: u64,
    ) -> bool {
        chain
            .verify_authorizes_presigned(
                TokenScope::SUBSCRIBE,
                self.channel_id.hash(),
                entity_id,
                &self.token_roots,
                revocation,
                skew_secs,
            )
            .is_ok()
    }
}

/// Registry of channel configurations.
///
/// Keyed by channel name (not hash) to prevent hash collisions from silently
/// overwriting security policies. The canonical [`ChannelHash`] (`u64`) is
/// collision-resistant at realistic scale (~65 K channels), and `by_hash`
/// gives O(1) canonical-hash lookup; `by_wire_hash` resolves the wire
/// `u16` fast-path hint into a list of canonical channels for receive-side
/// dispatch (routine collisions at scale).
///
/// Surface the deny-all misconfiguration loudly at registration time.
///
/// `require_token = true` with no `token_roots` is a valid fail-closed
/// state (nothing is authorized), but it's far more often a mistake —
/// `with_require_token(true)` was called instead of
/// `with_token_roots(...)`. Logging it at insert turns a silent
/// "every publish and subscribe is denied" into an actionable warning.
fn warn_if_fail_closed(config: &ChannelConfig) {
    if config.require_token && config.token_roots.is_empty() {
        tracing::warn!(
            channel = config.channel_id.name().as_str(),
            "channel requires a token but has no token_roots: all publish \
             and subscribe will be denied (fail closed). Use \
             `with_token_roots(...)` to anchor a root of trust."
        );
    }
}

/// Consulted at subscription/channel-creation time (slow path).
/// The fast path uses the `AuthGuard` bloom filter.
pub struct ChannelConfigRegistry {
    /// Primary storage: name → config (collision-safe)
    configs: DashMap<String, ChannelConfig>,
    /// Reverse index: canonical hash → names (collision-resistant at u32).
    by_hash: DashMap<ChannelHash, Vec<String>>,
    /// Wire-hash reverse index: u16 wire-hash → names (routine collisions).
    /// Used by receive-side dispatch to disambiguate the `NetHeader`
    /// fast-path hint into canonical channels.
    by_wire_hash: DashMap<u16, Vec<String>>,
    /// Prefix registry: prefix → config. Consulted by
    /// `get_by_name` when no exact match exists; the first prefix
    /// that the queried name starts with wins. Used by nRPC's
    /// SDK glue to register `<service>.replies.` once and admit
    /// every `<service>.replies.<caller_origin>` subscribe that
    /// follows.
    ///
    /// Prefix lookups are O(num_prefixes) — a small constant in
    /// practice (one prefix per nRPC service). The exact-match
    /// hot path is unaffected.
    prefix_configs: DashMap<String, ChannelConfig>,
}

impl ChannelConfigRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            configs: DashMap::new(),
            by_hash: DashMap::new(),
            by_wire_hash: DashMap::new(),
            prefix_configs: DashMap::new(),
        }
    }

    /// Register a prefix-matched channel configuration. Any
    /// channel name starting with `prefix` that has no exact-match
    /// entry will resolve to `config` via [`Self::get_by_name`].
    ///
    /// **Use sparingly.** Prefix lookups bypass the `by_hash`
    /// fast path and walk the prefix list on the slow path; one
    /// prefix per service is fine, hundreds is not. nRPC uses
    /// this for its dynamic per-caller reply channels
    /// (`<service>.replies.<caller_origin>`) — one prefix per
    /// `serve_rpc` registration.
    ///
    /// `config.channel_id` should carry the prefix as a sentinel
    /// name (e.g. `<svc>.replies.`); it isn't used for hash
    /// lookups, so the channel-name validation rules don't apply
    /// strictly. Prefix entries are collision-safe with respect
    /// to each other (DashMap on the prefix string). When multiple
    /// prefixes match a queried name, [`Self::get_by_name`] returns
    /// the LONGEST one — so a more specific entry safely overrides
    /// a more general one. Resolution is deterministic across
    /// processes (the longest-length tiebreaker can never tie since
    /// DashMap deduplicates keys).
    pub fn insert_prefix(&self, prefix: impl Into<String>, config: ChannelConfig) {
        warn_if_fail_closed(&config);
        self.prefix_configs.insert(prefix.into(), config);
    }

    /// Remove a prefix-matched config. Returns the removed config
    /// if it existed.
    pub fn remove_prefix(&self, prefix: &str) -> Option<ChannelConfig> {
        self.prefix_configs.remove(prefix).map(|(_, v)| v)
    }

    /// Register a channel configuration.
    pub fn insert(&self, config: ChannelConfig) {
        warn_if_fail_closed(&config);
        let name = config.channel_id.name().to_string();
        let hash = config.channel_id.hash();
        let wire_hash = config.channel_id.wire_hash();
        self.configs.insert(name.clone(), config);
        self.by_hash.entry(hash).or_default().push(name.clone());
        self.by_wire_hash.entry(wire_hash).or_default().push(name);
    }

    /// Look up a channel config by canonical [`ChannelHash`] (`u64`).
    ///
    /// Returns `None` if the hash is unknown **or** if multiple channels
    /// share the same canonical hash (rare at u64 — ~65 K channels before
    /// birthday-collision threshold). Callers that need collision-safe
    /// lookups should use [`Self::get_by_name`] with the full channel name.
    ///
    /// Returning `None` on collision forces callers to fall back to safe
    /// defaults rather than silently applying the wrong channel's policy.
    pub fn get(
        &self,
        channel_hash: ChannelHash,
    ) -> Option<dashmap::mapref::one::Ref<'_, String, ChannelConfig>> {
        let names = self.by_hash.get(&channel_hash)?;
        // Refuse to return an arbitrary config when hashes collide.
        if names.len() != 1 {
            return None;
        }
        let name = names.first()?;
        self.configs.get(name)
    }

    /// Look up a channel config by the wire `u16` fast-path hint.
    ///
    /// Returns `None` if the wire bucket is empty **or** if multiple
    /// channels share the same `u16` bucket (routine at scale).
    /// On wire-bucket collision, receive-side dispatch must fall through
    /// to a name-aware path; the wire hash is only a fast-path hint.
    pub fn get_by_wire_hash(
        &self,
        wire_hash: u16,
    ) -> Option<dashmap::mapref::one::Ref<'_, String, ChannelConfig>> {
        let names = self.by_wire_hash.get(&wire_hash)?;
        if names.len() != 1 {
            return None;
        }
        let name = names.first()?;
        self.configs.get(name)
    }

    /// Look up a channel config by exact name (collision-safe).
    ///
    /// Falls back to the prefix registry if no exact match exists.
    /// Resolution is **longest-prefix-match** (the standard semantic
    /// for prefix tables): if both `foo.` and `foo.bar.` are
    /// registered and the queried name is `foo.bar.baz`, the
    /// `foo.bar.` config wins because it's the more specific match.
    /// Length ties are impossible (DashMap deduplicates keys), so
    /// resolution is fully deterministic across processes.
    ///
    /// Used by nRPC's dynamic reply channels — one
    /// `<service>.replies.` prefix admits every per-caller
    /// `<service>.replies.<caller_origin>` subscribe.
    pub fn get_by_name(
        &self,
        name: &str,
    ) -> Option<dashmap::mapref::one::Ref<'_, String, ChannelConfig>> {
        if let Some(exact) = self.configs.get(name) {
            return Some(exact);
        }
        // Slow path: walk the prefix table. Cheap in the typical
        // case (zero or one prefix entries); the fast path is
        // unaffected. Picks the LONGEST matching prefix so a more
        // specific entry overrides a more general one — and so
        // resolution is deterministic across runs (the previous
        // "first match wins" was DashMap-shard-order dependent and
        // would silently flip across builds).
        let mut best_len = 0usize;
        let mut best_key: Option<String> = None;
        for entry in self.prefix_configs.iter() {
            let prefix = entry.key();
            if name.starts_with(prefix) && prefix.len() >= best_len {
                best_len = prefix.len();
                best_key = Some(prefix.clone());
            }
        }
        self.prefix_configs.get(&best_key?)
    }

    /// Remove a channel config by canonical [`ChannelHash`].
    ///
    /// Returns `None` if the hash is unknown **or** if multiple channels
    /// share the same canonical hash — mirroring the collision-safe
    /// semantics of `get()`. Removing an arbitrary config on collision
    /// would silently delete the wrong channel's policy (e.g. dropping a
    /// `SubnetLocal` entry and leaving a `Global` sibling in place).
    ///
    /// Callers that need to remove a specific channel should use
    /// [`remove_by_name`](Self::remove_by_name).
    pub fn remove(&self, channel_hash: ChannelHash) -> Option<ChannelConfig> {
        let name = {
            let names = self.by_hash.get(&channel_hash)?;
            if names.len() != 1 {
                return None;
            }
            names.first()?.clone()
        };
        self.remove_by_name(&name)
    }

    /// Remove a channel config by exact name (collision-safe).
    ///
    /// Returns the removed config if it existed.
    pub fn remove_by_name(&self, name: &str) -> Option<ChannelConfig> {
        let (_, removed) = self.configs.remove(name)?;
        let hash = removed.channel_id.hash();
        let wire_hash = removed.channel_id.wire_hash();
        if let Some(mut hash_names) = self.by_hash.get_mut(&hash) {
            hash_names.retain(|n| n != name);
        }
        if let Some(mut wire_names) = self.by_wire_hash.get_mut(&wire_hash) {
            wire_names.retain(|n| n != name);
        }
        Some(removed)
    }

    /// Number of registered channels.
    pub fn len(&self) -> usize {
        self.configs.len()
    }

    /// Check if empty.
    pub fn is_empty(&self) -> bool {
        self.configs.is_empty()
    }

    /// Snapshot every registered channel as `(name, config)` pairs,
    /// sorted by name for stable operator-tool output. Walks the
    /// exact-match table only — prefix entries are excluded
    /// because their `channel_id.name()` is a sentinel rather
    /// than a routable channel.
    ///
    /// O(N) clone — N is the registry size (typically tens to a
    /// few hundred). Suitable for `net channel ls` / Deck-panel
    /// renders, not for hot-path use.
    pub fn snapshot(&self) -> Vec<(String, ChannelConfig)> {
        let mut out: Vec<(String, ChannelConfig)> = self
            .configs
            .iter()
            .map(|e| (e.key().clone(), e.value().clone()))
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// Same as [`Self::snapshot`] but for prefix entries — emits
    /// `(prefix, config)` pairs for every prefix registered via
    /// [`Self::insert_prefix`]. Sorted by prefix for stable
    /// output.
    pub fn snapshot_prefixes(&self) -> Vec<(String, ChannelConfig)> {
        let mut out: Vec<(String, ChannelConfig)> = self
            .prefix_configs
            .iter()
            .map(|e| (e.key().clone(), e.value().clone()))
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// Get the priority for a channel (0 if not configured).
    #[inline]
    pub fn priority(&self, channel_hash: ChannelHash) -> u8 {
        self.get(channel_hash).map(|c| c.priority).unwrap_or(0)
    }
}

impl Default for ChannelConfigRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for ChannelConfigRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChannelConfigRegistry")
            .field("channels", &self.configs.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::behavior::capability::{GpuInfo, GpuVendor, HardwareCapabilities};
    use crate::adapter::net::identity::{EntityKeypair, PermissionToken};

    fn make_caps(gpu: bool) -> CapabilitySet {
        if gpu {
            let gpu_info = GpuInfo {
                vendor: GpuVendor::Nvidia,
                model: "test".to_string(),
                vram_gb: 8,
                compute_units: 0,
                tensor_cores: 0,
                fp16_tflops_x10: 0,
            };
            CapabilitySet::new().with_hardware(HardwareCapabilities::new().with_gpu(gpu_info))
        } else {
            CapabilitySet::new()
        }
    }

    /// One-link chain wrapping a token directly issued by `issuer` to
    /// `subject`.
    fn direct_chain(
        issuer: &EntityKeypair,
        subject: &EntityKeypair,
        scope: TokenScope,
        channel_hash: ChannelHash,
    ) -> TokenChain {
        TokenChain::single(PermissionToken::issue(
            issuer,
            subject.entity_id().clone(),
            scope,
            channel_hash,
            3600,
            0,
        ))
    }

    #[test]
    fn test_open_channel() {
        let id = ChannelId::parse("sensors/lidar").unwrap();
        let config = ChannelConfig::new(id);
        let caps = make_caps(false);
        let entity = EntityKeypair::generate();
        let rev = RevocationRegistry::new();

        assert!(config.can_publish(&caps, entity.entity_id(), None, &rev, 0));
        assert!(config.can_subscribe(&caps, entity.entity_id(), None, &rev, 0));
    }

    #[test]
    fn test_capability_restricted_channel() {
        let id = ChannelId::parse("compute/gpu-tasks").unwrap();
        let config =
            ChannelConfig::new(id).with_publish_caps(CapabilityFilter::new().require_gpu());

        let entity = EntityKeypair::generate();
        let rev = RevocationRegistry::new();

        let no_gpu = make_caps(false);
        assert!(!config.can_publish(&no_gpu, entity.entity_id(), None, &rev, 0));

        let with_gpu = make_caps(true);
        assert!(config.can_publish(&with_gpu, entity.entity_id(), None, &rev, 0));
    }

    /// The C1 fix: a `require_token` channel anchored to an owner must
    /// reject a self-issued token and accept an owner-issued one.
    #[test]
    fn token_channel_rejects_self_issued_accepts_owner_issued() {
        let id = ChannelId::parse("control/estop").unwrap();
        let owner = EntityKeypair::generate();
        let subject = EntityKeypair::generate();
        let config =
            ChannelConfig::new(id.clone()).with_token_roots(vec![owner.entity_id().clone()]);
        let caps = make_caps(false);
        let rev = RevocationRegistry::new();

        // No chain -> denied.
        assert!(!config.can_publish(&caps, subject.entity_id(), None, &rev, 0));

        // Self-issued (issuer == subject, NOT the channel owner) ->
        // denied. Pre-fix this was the privilege-escalation hole:
        // `verify()` + `TokenCache::check` accepted any self-consistent
        // token regardless of issuer.
        let self_chain = direct_chain(&subject, &subject, TokenScope::PUBLISH, id.hash());
        assert!(
            !config.can_publish(&caps, subject.entity_id(), Some(&self_chain), &rev, 0),
            "self-issued token must be rejected: its issuer is not a channel root"
        );

        // Owner-issued -> allowed.
        let owner_chain = direct_chain(&owner, &subject, TokenScope::PUBLISH, id.hash());
        assert!(config.can_publish(&caps, subject.entity_id(), Some(&owner_chain), &rev, 0));
    }

    /// `with_require_token(true)` without any roots fails closed — there
    /// is no authority a chain could anchor to.
    #[test]
    fn require_token_with_no_roots_fails_closed() {
        let id = ChannelId::parse("control/locked").unwrap();
        let config = ChannelConfig::new(id.clone()).with_require_token(true);
        let caps = make_caps(false);
        let rev = RevocationRegistry::new();
        let anyone = EntityKeypair::generate();

        // Even an otherwise-well-formed token can't anchor to nothing.
        let chain = direct_chain(&anyone, &anyone, TokenScope::SUBSCRIBE, id.hash());
        assert!(!config.can_subscribe(&caps, anyone.entity_id(), Some(&chain), &rev, 0));
        assert!(!config.can_subscribe(&caps, anyone.entity_id(), None, &rev, 0));
    }

    /// A config that names roots but never set the `require_token`
    /// flag (e.g. built field-by-field rather than via
    /// `with_token_roots`) must still enforce. Pre-fix the gate keyed
    /// only off `require_token`, so this drifted-open config silently
    /// admitted every peer.
    #[test]
    fn roots_without_require_token_flag_still_enforces() {
        let id = ChannelId::parse("control/estop").unwrap();
        let owner = EntityKeypair::generate();
        let subject = EntityKeypair::generate();
        let caps = make_caps(false);
        let rev = RevocationRegistry::new();

        let mut config = ChannelConfig::new(id.clone());
        config.token_roots = vec![owner.entity_id().clone()];
        // Deliberately leave `require_token` false — the two fields are
        // both public and can drift out of sync.
        assert!(!config.require_token);
        assert!(
            config.token_required(),
            "named roots must imply enforcement"
        );

        // No chain -> denied (would have been silently admitted pre-fix).
        assert!(!config.can_subscribe(&caps, subject.entity_id(), None, &rev, 0));
        // Owner-issued chain -> allowed.
        let owner_chain = direct_chain(&owner, &subject, TokenScope::SUBSCRIBE, id.hash());
        assert!(config.can_subscribe(&caps, subject.entity_id(), Some(&owner_chain), &rev, 0));
    }

    /// The chain's leaf must be bound to the presenting entity — a peer
    /// can't replay a chain minted for someone else.
    #[test]
    fn leaf_subject_must_match_presenter() {
        let id = ChannelId::parse("control/estop").unwrap();
        let owner = EntityKeypair::generate();
        let intended = EntityKeypair::generate();
        let attacker = EntityKeypair::generate();
        let config =
            ChannelConfig::new(id.clone()).with_token_roots(vec![owner.entity_id().clone()]);
        let caps = make_caps(false);
        let rev = RevocationRegistry::new();

        // Owner issued this to `intended`; `attacker` presents it.
        let chain = direct_chain(&owner, &intended, TokenScope::SUBSCRIBE, id.hash());
        assert!(!config.can_subscribe(&caps, attacker.entity_id(), Some(&chain), &rev, 0));
        // The intended subject is accepted.
        assert!(config.can_subscribe(&caps, intended.entity_id(), Some(&chain), &rev, 0));
    }

    /// A valid owner → intermediate → leaf delegation chain is accepted;
    /// scope narrows correctly down the chain.
    #[test]
    fn delegation_chain_accepted() {
        let id = ChannelId::parse("fleet/telemetry").unwrap();
        let owner = EntityKeypair::generate();
        let mid = EntityKeypair::generate();
        let leaf = EntityKeypair::generate();
        let config =
            ChannelConfig::new(id.clone()).with_token_roots(vec![owner.entity_id().clone()]);
        let caps = make_caps(false);
        let rev = RevocationRegistry::new();

        // Owner grants `mid` SUBSCRIBE + DELEGATE, depth 2.
        let root = PermissionToken::issue(
            &owner,
            mid.entity_id().clone(),
            TokenScope::SUBSCRIBE.union(TokenScope::DELEGATE),
            id.hash(),
            3600,
            2,
        );
        // `mid` delegates SUBSCRIBE to `leaf` (drops DELEGATE).
        let child = root
            .delegate(&mid, leaf.entity_id().clone(), TokenScope::SUBSCRIBE)
            .expect("delegation should succeed");
        let chain = TokenChain {
            tokens: vec![root, child],
        };
        assert!(config.can_subscribe(&caps, leaf.entity_id(), Some(&chain), &rev, 0));
    }

    /// A chain whose links don't connect (`child.issuer != parent.subject`)
    /// is rejected — no splicing an unrelated token onto a real root.
    #[test]
    fn delegation_broken_continuity_rejected() {
        let id = ChannelId::parse("fleet/telemetry").unwrap();
        let owner = EntityKeypair::generate();
        let mid = EntityKeypair::generate();
        let rogue = EntityKeypair::generate();
        let leaf = EntityKeypair::generate();
        let config =
            ChannelConfig::new(id.clone()).with_token_roots(vec![owner.entity_id().clone()]);
        let caps = make_caps(false);
        let rev = RevocationRegistry::new();

        // Real owner→mid root link.
        let root = PermissionToken::issue(
            &owner,
            mid.entity_id().clone(),
            TokenScope::SUBSCRIBE.union(TokenScope::DELEGATE),
            id.hash(),
            3600,
            2,
        );
        // Spliced second link issued by `rogue` (NOT `mid`), so
        // child.issuer (rogue) != root.subject (mid).
        let spliced = PermissionToken::issue(
            &rogue,
            leaf.entity_id().clone(),
            TokenScope::SUBSCRIBE,
            id.hash(),
            3600,
            0,
        );
        let chain = TokenChain {
            tokens: vec![root, spliced],
        };
        assert!(!config.can_subscribe(&caps, leaf.entity_id(), Some(&chain), &rev, 0));
    }

    /// A delegated child can't authorize a scope its parent lacked —
    /// chain authority is the intersection of all links.
    #[test]
    fn delegation_cannot_broaden_scope() {
        let id = ChannelId::parse("fleet/telemetry").unwrap();
        let owner = EntityKeypair::generate();
        let mid = EntityKeypair::generate();
        let leaf = EntityKeypair::generate();
        let config =
            ChannelConfig::new(id.clone()).with_token_roots(vec![owner.entity_id().clone()]);
        let caps = make_caps(false);
        let rev = RevocationRegistry::new();

        // Owner grants `mid` only SUBSCRIBE + DELEGATE — no PUBLISH.
        let root = PermissionToken::issue(
            &owner,
            mid.entity_id().clone(),
            TokenScope::SUBSCRIBE.union(TokenScope::DELEGATE),
            id.hash(),
            3600,
            2,
        );
        // `mid` forges a child claiming PUBLISH (which it never held).
        // `delegate` would intersect it away, so mint the child by hand
        // to simulate a malicious intermediate.
        let forged_child = PermissionToken::issue(
            &mid,
            leaf.entity_id().clone(),
            TokenScope::PUBLISH,
            id.hash(),
            3600,
            0,
        );
        let chain = TokenChain {
            tokens: vec![root, forged_child],
        };
        // The root link doesn't authorize PUBLISH, so the chain can't.
        assert!(!config.can_publish(&caps, leaf.entity_id(), Some(&chain), &rev, 0));
    }

    /// The H1 fix: revoking the root issuer invalidates the whole chain,
    /// including offline-delegated descendants, because the root grant
    /// is itself a verified link.
    #[test]
    fn root_revocation_kills_delegated_chain() {
        let id = ChannelId::parse("fleet/telemetry").unwrap();
        let owner = EntityKeypair::generate();
        let mid = EntityKeypair::generate();
        let leaf = EntityKeypair::generate();
        let config =
            ChannelConfig::new(id.clone()).with_token_roots(vec![owner.entity_id().clone()]);
        let caps = make_caps(false);
        let rev = RevocationRegistry::new();

        let root = PermissionToken::issue(
            &owner,
            mid.entity_id().clone(),
            TokenScope::SUBSCRIBE.union(TokenScope::DELEGATE),
            id.hash(),
            3600,
            2,
        );
        let child = root
            .delegate(&mid, leaf.entity_id().clone(), TokenScope::SUBSCRIBE)
            .expect("delegation should succeed");
        let chain = TokenChain {
            tokens: vec![root, child],
        };

        // Accepted before revocation.
        assert!(config.can_subscribe(&caps, leaf.entity_id(), Some(&chain), &rev, 0));

        // Owner bumps its revocation floor above the chain's generation
        // (0). The root link falls below the floor → whole chain dies,
        // even though the delegated child's issuer is `mid`, not `owner`.
        rev.revoke_below(owner.entity_id(), 1);
        assert!(
            !config.can_subscribe(&caps, leaf.entity_id(), Some(&chain), &rev, 0),
            "revoking the root must kill the delegated descendant"
        );
    }

    #[test]
    fn test_caps_and_token_combined() {
        let id = ChannelId::parse("compute/secure").unwrap();
        let owner = EntityKeypair::generate();
        let subject = EntityKeypair::generate();
        let config = ChannelConfig::new(id.clone())
            .with_publish_caps(CapabilityFilter::new().require_gpu())
            .with_token_roots(vec![owner.entity_id().clone()]);
        let rev = RevocationRegistry::new();

        let owner_chain = direct_chain(&owner, &subject, TokenScope::PUBLISH, id.hash());

        // Has GPU but no token -> denied.
        let with_gpu = make_caps(true);
        assert!(!config.can_publish(&with_gpu, subject.entity_id(), None, &rev, 0));

        // Has token but no GPU -> denied.
        let no_gpu = make_caps(false);
        assert!(!config.can_publish(&no_gpu, subject.entity_id(), Some(&owner_chain), &rev, 0));

        // Has both -> allowed.
        assert!(config.can_publish(&with_gpu, subject.entity_id(), Some(&owner_chain), &rev, 0));
    }

    #[test]
    fn test_config_registry() {
        let reg = ChannelConfigRegistry::new();
        let id = ChannelId::parse("sensors/lidar").unwrap();
        let config = ChannelConfig::new(id.clone()).with_priority(5);

        reg.insert(config);
        assert_eq!(reg.len(), 1);
        assert_eq!(reg.priority(id.hash()), 5);

        let retrieved = reg.get(id.hash()).unwrap();
        assert_eq!(retrieved.priority, 5);
    }

    #[test]
    fn test_visibility_default() {
        let id = ChannelId::parse("test").unwrap();
        let config = ChannelConfig::new(id);
        assert_eq!(config.visibility, Visibility::Global);
    }

    #[test]
    fn snapshot_returns_sorted_exact_matches_excludes_prefixes() {
        // Pin the operator-tool surface: `snapshot` yields every
        // exact-match channel in lex order, and `snapshot_prefixes`
        // is a sibling for the prefix table — exact-matches and
        // prefixes don't mix.
        let reg = ChannelConfigRegistry::new();
        let zeta = ChannelConfig::new(ChannelId::parse("zeta/c").unwrap())
            .with_visibility(Visibility::SubnetLocal);
        let alpha = ChannelConfig::new(ChannelId::parse("alpha/a").unwrap())
            .with_visibility(Visibility::Global);
        let middle = ChannelConfig::new(ChannelId::parse("middle/b").unwrap());
        reg.insert(zeta);
        reg.insert(alpha);
        reg.insert(middle);
        reg.insert_prefix(
            "rpc.replies.",
            ChannelConfig::new(ChannelId::parse("rpc.replies.").unwrap()),
        );

        let snap = reg.snapshot();
        let names: Vec<&str> = snap.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["alpha/a", "middle/b", "zeta/c"]);
        // Prefix entries excluded from `snapshot`.
        assert!(!names.contains(&"rpc.replies."));
        // Per-entry visibility round-trips.
        let alpha_cfg = snap.iter().find(|(n, _)| n == "alpha/a").unwrap();
        assert_eq!(alpha_cfg.1.visibility, Visibility::Global);
        let zeta_cfg = snap.iter().find(|(n, _)| n == "zeta/c").unwrap();
        assert_eq!(zeta_cfg.1.visibility, Visibility::SubnetLocal);

        let prefixes = reg.snapshot_prefixes();
        let prefix_names: Vec<&str> = prefixes.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(prefix_names, vec!["rpc.replies."]);
    }

    #[test]
    fn test_regression_config_registry_hash_collision_no_overwrite() {
        // Regression: ChannelConfigRegistry used u16 hash as the key,
        // so two channels with the same hash silently overwrote each
        // other's configs — including visibility and security policies.
        // With only 65536 hashes, the birthday paradox makes collisions
        // likely at ~300 channels.
        //
        // Fix: keyed by channel name with a hash→names reverse index.
        let reg = ChannelConfigRegistry::new();

        let id1 = ChannelId::parse("channel/alpha").unwrap();
        let id2 = ChannelId::parse("channel/beta").unwrap();

        let config1 = ChannelConfig::new(id1.clone()).with_priority(1);
        let config2 = ChannelConfig::new(id2.clone()).with_priority(2);

        reg.insert(config1);
        reg.insert(config2);

        // Both configs should be present regardless of hash collision
        assert_eq!(reg.len(), 2, "both channels should exist in registry");

        // Each should retain its own priority
        let c1 = reg.get_by_name("channel/alpha").unwrap();
        assert_eq!(c1.priority, 1, "channel/alpha priority should be 1");
        let c2 = reg.get_by_name("channel/beta").unwrap();
        assert_eq!(c2.priority, 2, "channel/beta priority should be 2");
    }

    #[test]
    fn test_regression_config_registry_get_returns_none_on_collision() {
        // Regression: get() returned an arbitrary config when multiple
        // channels shared the same u16 hash. A SubnetLocal channel
        // colliding with a Global channel could silently receive the
        // wrong visibility policy, leaking traffic across subnet
        // boundaries.
        //
        // Fix: get() returns None when the hash maps to more than one
        // channel name. Callers fall back to safe defaults or use
        // get_by_name() for collision-safe lookups.
        use crate::adapter::net::channel::name::wire_channel_hash;

        // Find two valid channel names that produce the same wire `u16`
        // hash. With 65 536 possible values, birthday paradox gives a
        // collision within ~300 names on average. (Canonical `u32`
        // collisions are rare enough — ~65 K names — that exercising
        // them in tests would be slow; the wire-hash bucket is the
        // observable collision surface here.)
        let mut seen = std::collections::HashMap::<u16, String>::new();
        let (name1, name2) = loop {
            let name = format!("ch-{}", seen.len());
            let wire = wire_channel_hash(&name);
            if let Some(existing) = seen.get(&wire) {
                break (existing.clone(), name);
            }
            seen.insert(wire, name);
        };

        let reg = ChannelConfigRegistry::new();
        let id1 = ChannelId::parse(&name1).unwrap();
        let id2 = ChannelId::parse(&name2).unwrap();
        assert_eq!(
            id1.wire_hash(),
            id2.wire_hash(),
            "precondition: wire hashes must collide"
        );

        // Insert a SubnetLocal channel and a Global channel that collide
        let config1 = ChannelConfig::new(id1.clone()).with_visibility(Visibility::SubnetLocal);
        let config2 = ChannelConfig::new(id2.clone()).with_visibility(Visibility::Global);
        reg.insert(config1);
        reg.insert(config2);

        // get_by_wire_hash() must return None — not an arbitrary
        // config — on a wire-bucket collision.
        assert!(
            reg.get_by_wire_hash(id1.wire_hash()).is_none(),
            "get_by_wire_hash() must return None when wire hashes collide between channels"
        );

        // The canonical-hash path stays unaffected: each name has a
        // distinct canonical [`ChannelHash`] (collision-resistant at
        // u32), so `get(canonical)` resolves uniquely.
        assert_eq!(
            reg.get(id1.hash()).unwrap().visibility,
            Visibility::SubnetLocal
        );
        assert_eq!(reg.get(id2.hash()).unwrap().visibility, Visibility::Global);

        // get_by_name() must still work for each channel individually
        let c1 = reg.get_by_name(&name1).unwrap();
        assert_eq!(c1.visibility, Visibility::SubnetLocal);
        let c2 = reg.get_by_name(&name2).unwrap();
        assert_eq!(c2.visibility, Visibility::Global);
    }

    #[test]
    fn test_regression_remove_by_wire_hash_safe_on_wire_collision() {
        // Regression: the wire-keyed remove path used to silently
        // delete the first name bucketed under a colliding `u16` wire
        // hash, swapping policies between unrelated channels. With
        // the substrate-wide widening to canonical [`ChannelHash`]
        // (`u32`), the primary `remove(hash)` keys on the canonical
        // value (unique per name); the wire-bucket collision space
        // is exercised below via two names that share a `u16` bucket
        // and asserts each name is independently addressable through
        // both `remove(canonical)` and `remove_by_name`.
        use crate::adapter::net::channel::name::wire_channel_hash;

        let mut seen = std::collections::HashMap::<u16, String>::new();
        let (name1, name2) = loop {
            let name = format!("rm-{}", seen.len());
            let wire = wire_channel_hash(&name);
            if let Some(existing) = seen.get(&wire) {
                break (existing.clone(), name);
            }
            seen.insert(wire, name);
        };

        let reg = ChannelConfigRegistry::new();
        let id1 = ChannelId::parse(&name1).unwrap();
        let id2 = ChannelId::parse(&name2).unwrap();
        assert_eq!(
            id1.wire_hash(),
            id2.wire_hash(),
            "precondition: wire hashes must collide"
        );

        reg.insert(ChannelConfig::new(id1.clone()).with_visibility(Visibility::SubnetLocal));
        reg.insert(ChannelConfig::new(id2.clone()).with_visibility(Visibility::Global));

        // Canonical `remove(hash)` keys on the u32 canonical hash,
        // which is unique per name, so each config is removable
        // individually even under a wire-bucket collision.
        let removed1 = reg.remove(id1.hash()).expect("remove canonical1");
        assert_eq!(removed1.visibility, Visibility::SubnetLocal);
        assert_eq!(reg.len(), 1, "the other config must still be present");
        assert_eq!(
            reg.get_by_name(&name2).unwrap().visibility,
            Visibility::Global,
            "name2 must be untouched by the canonical remove of name1"
        );

        // `remove_by_name` is the explicit-collision-safe path used
        // by callers that already hold the name string; it must
        // continue to work alongside the canonical-hash path.
        let removed2 = reg.remove_by_name(&name2).unwrap();
        assert_eq!(removed2.visibility, Visibility::Global);
        assert_eq!(reg.len(), 0);
    }

    #[test]
    fn prefix_resolution_picks_longest_match_deterministically() {
        // Regression: prior `get_by_name` used DashMap iteration
        // order to pick "first matching prefix wins", which is shard-
        // order dependent and non-deterministic across processes.
        // With both `foo.` and `foo.bar.` registered against
        // `foo.bar.baz`, the longer (more specific) prefix must win.
        let reg = ChannelConfigRegistry::new();
        reg.insert_prefix(
            "foo.",
            ChannelConfig::new(ChannelId::parse("foo.sentinel").unwrap()).with_priority(1),
        );
        reg.insert_prefix(
            "foo.bar.",
            ChannelConfig::new(ChannelId::parse("foo.bar.sentinel").unwrap()).with_priority(2),
        );
        reg.insert_prefix(
            "foo.bar.baz.",
            ChannelConfig::new(ChannelId::parse("foo.bar.baz.sentinel").unwrap()).with_priority(3),
        );

        // Most-specific match wins regardless of insertion order.
        let c = reg.get_by_name("foo.bar.baz.qux").unwrap();
        assert_eq!(c.priority, 3, "longest matching prefix must win");

        // Slightly shorter target — `foo.bar.baz.` no longer matches
        // (target doesn't start with the trailing dot), so `foo.bar.`
        // wins.
        let c = reg.get_by_name("foo.bar.something").unwrap();
        assert_eq!(c.priority, 2);

        // Shortest matching prefix wins when no others apply.
        let c = reg.get_by_name("foo.something").unwrap();
        assert_eq!(c.priority, 1);

        // No match.
        assert!(reg.get_by_name("other.thing").is_none());

        // Run the lookup many times; result must be stable.
        for _ in 0..100 {
            assert_eq!(reg.get_by_name("foo.bar.baz.x").unwrap().priority, 3);
        }
    }

    #[test]
    fn test_remove_by_hash_works_when_unique() {
        // Baseline: `remove(hash)` still works for the common non-collision
        // case — only refuses when ambiguous.
        let reg = ChannelConfigRegistry::new();
        let id = ChannelId::parse("sensors/only").unwrap();
        let hash = id.hash();
        reg.insert(ChannelConfig::new(id).with_priority(7));

        let removed = reg.remove(hash).unwrap();
        assert_eq!(removed.priority, 7);
        assert_eq!(reg.len(), 0);
        assert!(reg.get(hash).is_none());
    }
}
