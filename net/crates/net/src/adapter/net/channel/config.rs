//! Channel configuration and visibility.
//!
//! Channel policy uses the existing capability system (`CapabilityFilter`)
//! for access rules, combined with L1 permission tokens. This avoids
//! building a separate rule engine.

use super::name::ChannelId;
use crate::adapter::net::behavior::capability::{CapabilityFilter, CapabilitySet};
use crate::adapter::net::identity::{EntityId, TokenCache, TokenScope};
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

    /// Check if a node is authorized to publish on this channel.
    pub fn can_publish(
        &self,
        node_caps: &CapabilitySet,
        entity_id: &EntityId,
        token_cache: &TokenCache,
    ) -> bool {
        // Check capability requirements
        if let Some(ref filter) = self.publish_caps {
            if !filter.matches(node_caps) {
                return false;
            }
        }
        // Check token requirement
        if self.require_token
            && token_cache
                .check(entity_id, TokenScope::PUBLISH, self.channel_id.hash())
                .is_err()
        {
            return false;
        }
        true
    }

    /// Check if a node is authorized to subscribe to this channel.
    pub fn can_subscribe(
        &self,
        node_caps: &CapabilitySet,
        entity_id: &EntityId,
        token_cache: &TokenCache,
    ) -> bool {
        if let Some(ref filter) = self.subscribe_caps {
            if !filter.matches(node_caps) {
                return false;
            }
        }
        if self.require_token
            && token_cache
                .check(entity_id, TokenScope::SUBSCRIBE, self.channel_id.hash())
                .is_err()
        {
            return false;
        }
        true
    }
}

/// Registry of channel configurations.
///
/// Keyed by channel name (not hash) to prevent u16 hash collisions
/// from silently overwriting security policies. With only 65536
/// possible hash values, the birthday paradox makes collisions likely
/// at ~300 channels.
///
/// Consulted at subscription/channel-creation time (slow path).
/// The fast path uses the `AuthGuard` bloom filter.
pub struct ChannelConfigRegistry {
    /// Primary storage: name → config (collision-safe)
    configs: DashMap<String, ChannelConfig>,
    /// Reverse index: hash → names (for hash-based lookups)
    by_hash: DashMap<u16, Vec<String>>,
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
        self.prefix_configs.insert(prefix.into(), config);
    }

    /// Remove a prefix-matched config. Returns the removed config
    /// if it existed.
    pub fn remove_prefix(&self, prefix: &str) -> Option<ChannelConfig> {
        self.prefix_configs.remove(prefix).map(|(_, v)| v)
    }

    /// Register a channel configuration.
    pub fn insert(&self, config: ChannelConfig) {
        let name = config.channel_id.name().to_string();
        let hash = config.channel_id.hash();
        self.configs.insert(name.clone(), config);
        self.by_hash.entry(hash).or_default().push(name);
    }

    /// Look up a channel config by hash.
    ///
    /// Returns `None` if the hash is unknown **or** if multiple channels
    /// share the same hash (collision). Callers that need collision-safe
    /// lookups should use `get_by_name()` with the full channel name.
    ///
    /// With only 65,536 possible u16 hash values, collisions become likely
    /// at ~300 channels (birthday paradox). Returning `None` on collision
    /// forces callers to fall back to safe defaults rather than silently
    /// applying the wrong channel's security policy.
    pub fn get(
        &self,
        channel_hash: u16,
    ) -> Option<dashmap::mapref::one::Ref<'_, String, ChannelConfig>> {
        let names = self.by_hash.get(&channel_hash)?;
        // Refuse to return an arbitrary config when hashes collide.
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

    /// Remove a channel config by hash.
    ///
    /// Returns `None` if the hash is unknown **or** if multiple channels
    /// share the same hash — mirroring the collision-safe semantics of
    /// `get()`. Removing an arbitrary config on collision would silently
    /// delete the wrong channel's policy (e.g. dropping a `SubnetLocal`
    /// entry and leaving a `Global` sibling in place).
    ///
    /// Callers that need to remove a specific channel should use
    /// [`remove_by_name`](Self::remove_by_name).
    pub fn remove(&self, channel_hash: u16) -> Option<ChannelConfig> {
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
        if let Some(mut hash_names) = self.by_hash.get_mut(&hash) {
            hash_names.retain(|n| n != name);
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

    /// Get the priority for a channel (0 if not configured).
    #[inline]
    pub fn priority(&self, channel_hash: u16) -> u8 {
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

    #[test]
    fn test_open_channel() {
        let id = ChannelId::parse("sensors/lidar").unwrap();
        let config = ChannelConfig::new(id);
        let caps = make_caps(false);
        let entity = EntityKeypair::generate();
        let cache = TokenCache::new();

        assert!(config.can_publish(&caps, entity.entity_id(), &cache));
        assert!(config.can_subscribe(&caps, entity.entity_id(), &cache));
    }

    #[test]
    fn test_capability_restricted_channel() {
        let id = ChannelId::parse("compute/gpu-tasks").unwrap();
        let config =
            ChannelConfig::new(id).with_publish_caps(CapabilityFilter::new().require_gpu());

        let entity = EntityKeypair::generate();
        let cache = TokenCache::new();

        let no_gpu = make_caps(false);
        assert!(!config.can_publish(&no_gpu, entity.entity_id(), &cache));

        let with_gpu = make_caps(true);
        assert!(config.can_publish(&with_gpu, entity.entity_id(), &cache));
    }

    #[test]
    fn test_token_required_channel() {
        let id = ChannelId::parse("control/estop").unwrap();
        let config = ChannelConfig::new(id.clone()).with_require_token(true);
        let caps = make_caps(false);
        let issuer = EntityKeypair::generate();
        let subject = EntityKeypair::generate();
        let cache = TokenCache::new();

        // No token -> denied
        assert!(!config.can_publish(&caps, subject.entity_id(), &cache));

        // Issue a publish token for this channel
        let token = PermissionToken::issue(
            &issuer,
            subject.entity_id().clone(),
            TokenScope::PUBLISH,
            id.hash(),
            3600,
            0,
        );
        let _ = cache.insert(token);

        // With token -> allowed
        assert!(config.can_publish(&caps, subject.entity_id(), &cache));
    }

    #[test]
    fn test_caps_and_token_combined() {
        let id = ChannelId::parse("compute/secure").unwrap();
        let config = ChannelConfig::new(id.clone())
            .with_publish_caps(CapabilityFilter::new().require_gpu())
            .with_require_token(true);

        let issuer = EntityKeypair::generate();
        let subject = EntityKeypair::generate();
        let cache = TokenCache::new();

        // Has GPU but no token -> denied
        let with_gpu = make_caps(true);
        assert!(!config.can_publish(&with_gpu, subject.entity_id(), &cache));

        // Has token but no GPU -> denied
        let token = PermissionToken::issue(
            &issuer,
            subject.entity_id().clone(),
            TokenScope::PUBLISH,
            id.hash(),
            3600,
            0,
        );
        let _ = cache.insert(token);
        let no_gpu = make_caps(false);
        assert!(!config.can_publish(&no_gpu, subject.entity_id(), &cache));

        // Has both -> allowed
        assert!(config.can_publish(&with_gpu, subject.entity_id(), &cache));
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
        use crate::adapter::net::channel::name::channel_hash;

        // Find two valid channel names that produce the same u16 hash.
        // With 65536 possible values, birthday paradox gives a collision
        // within ~300 names on average.
        let mut seen = std::collections::HashMap::<u16, String>::new();
        let (name1, name2) = loop {
            let name = format!("ch-{}", seen.len());
            let hash = channel_hash(&name);
            if let Some(existing) = seen.get(&hash) {
                break (existing.clone(), name);
            }
            seen.insert(hash, name);
        };

        let reg = ChannelConfigRegistry::new();
        let id1 = ChannelId::parse(&name1).unwrap();
        let id2 = ChannelId::parse(&name2).unwrap();
        assert_eq!(id1.hash(), id2.hash(), "precondition: hashes must collide");

        // Insert a SubnetLocal channel and a Global channel that collide
        let config1 = ChannelConfig::new(id1.clone()).with_visibility(Visibility::SubnetLocal);
        let config2 = ChannelConfig::new(id2.clone()).with_visibility(Visibility::Global);
        reg.insert(config1);
        reg.insert(config2);

        // get() by hash must return None — not an arbitrary config
        assert!(
            reg.get(id1.hash()).is_none(),
            "get() must return None when hash collides between channels"
        );

        // get_by_name() must still work for each channel individually
        let c1 = reg.get_by_name(&name1).unwrap();
        assert_eq!(c1.visibility, Visibility::SubnetLocal);
        let c2 = reg.get_by_name(&name2).unwrap();
        assert_eq!(c2.visibility, Visibility::Global);
    }

    #[test]
    fn test_regression_remove_by_hash_returns_none_on_collision() {
        // Regression: `remove(hash)` removed the *first* name bucketed under
        // a colliding hash, silently deleting the wrong channel's config.
        // A `SubnetLocal` entry could disappear while an unrelated `Global`
        // sibling survived under the same hash — exactly the kind of silent
        // policy swap that the `get()` fix was meant to prevent.
        //
        // Fix: `remove(hash)` now returns None on collision and leaves both
        // configs in place. Callers that want to remove a specific entry
        // must use `remove_by_name`.
        use crate::adapter::net::channel::name::channel_hash;

        let mut seen = std::collections::HashMap::<u16, String>::new();
        let (name1, name2) = loop {
            let name = format!("rm-{}", seen.len());
            let hash = channel_hash(&name);
            if let Some(existing) = seen.get(&hash) {
                break (existing.clone(), name);
            }
            seen.insert(hash, name);
        };

        let reg = ChannelConfigRegistry::new();
        let id1 = ChannelId::parse(&name1).unwrap();
        let id2 = ChannelId::parse(&name2).unwrap();
        let shared_hash = id1.hash();
        assert_eq!(shared_hash, id2.hash(), "precondition: hashes must collide");

        reg.insert(ChannelConfig::new(id1.clone()).with_visibility(Visibility::SubnetLocal));
        reg.insert(ChannelConfig::new(id2.clone()).with_visibility(Visibility::Global));

        // remove(hash) refuses on collision — both configs still present.
        assert!(
            reg.remove(shared_hash).is_none(),
            "remove(hash) must refuse on collision"
        );
        assert_eq!(
            reg.len(),
            2,
            "both configs must remain after refused remove"
        );
        assert_eq!(
            reg.get_by_name(&name1).unwrap().visibility,
            Visibility::SubnetLocal
        );
        assert_eq!(
            reg.get_by_name(&name2).unwrap().visibility,
            Visibility::Global
        );

        // remove_by_name works and does not disturb its colliding sibling.
        let removed = reg.remove_by_name(&name1).unwrap();
        assert_eq!(removed.visibility, Visibility::SubnetLocal);
        assert!(reg.get_by_name(&name1).is_none(), "name1 should be gone");
        assert_eq!(
            reg.get_by_name(&name2).unwrap().visibility,
            Visibility::Global,
            "name2 must be untouched"
        );

        // With the collision resolved, remove(hash) works again.
        let removed2 = reg.remove(shared_hash).unwrap();
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
