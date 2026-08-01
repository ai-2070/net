//! Channel configuration and visibility.
//!
//! Channel policy uses the existing capability system (`CapabilityFilter`)
//! for access rules, combined with L1 permission tokens. This avoids
//! building a separate rule engine.

use super::name::{ChannelHash, ChannelId, ChannelName};
use crate::adapter::net::behavior::capability::{CapabilityFilter, CapabilitySet};
use crate::adapter::net::identity::{EntityId, RevocationRegistry, TokenChain, TokenScope};
use dashmap::DashMap;

/// How a channel binds its dynamic name suffix to the subscribing
/// peer's authenticated identity.
///
/// Set on a **prefix**-registered [`ChannelConfig`], this turns a
/// family of dynamically-named channels from "anyone may subscribe to
/// any name under the prefix" into "a peer may subscribe only to the
/// one name that encodes its own identity".
///
/// The motivating case is nRPC's per-caller reply channels
/// (`<service>.replies.<caller_origin>`). Those resolve through a
/// permissive prefix entry, so pre-fix any mesh peer could hold a live
/// subscription to *another* caller's reply channel and receive that
/// caller's response bodies whenever the server's direct route missed
/// and the response fell back to roster fan-out.
///
/// Evaluated against the **pinned** peer identity (the TOFU binding
/// installed from a signature-verified direct capability announcement),
/// never a wire-claimed value. A peer whose identity is not yet pinned
/// is rejected: admitting it would hand an attacker the bypass of
/// simply never announcing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OriginBinding {
    /// The requested name's suffix — everything after the matched
    /// prefix — must equal the subscriber's
    /// [`EntityId::origin_hash`](crate::adapter::net::identity::EntityId::origin_hash)
    /// rendered as exactly 16 lowercase hex digits, which is the
    /// format nRPC uses to build the channel name.
    OriginHashHex16,
}

impl OriginBinding {
    /// The complete subscribe decision for a bound channel family.
    ///
    /// `pinned_origin` is the subscriber's TOFU-pinned
    /// `EntityId::origin_hash()`, or `None` when the publisher has not
    /// pinned that peer yet (no signature-verified direct capability
    /// announcement has arrived from it).
    ///
    /// **`None` rejects.** This is the rule the whole finding turns on:
    /// admitting a peer whose identity we do not know would let an
    /// attacker bypass the binding entirely by simply never announcing,
    /// which is not a fix. It is stated here, as one branch of a pure
    /// function, rather than left implicit at the call site — that
    /// makes it directly testable and hard to "simplify" away.
    ///
    /// The cost is an ordering requirement: a peer must be pinned
    /// before its first subscribe to a bound family. In practice a node
    /// announces as part of coming up, and the publisher pushes/learns
    /// identities at session establishment, so this is satisfied well
    /// before any application traffic; the nRPC client additionally
    /// re-announces and retries on rejection.
    pub fn authorizes(
        self,
        name: &str,
        matched_prefix: Option<&str>,
        pinned_origin: Option<u64>,
    ) -> bool {
        let Some(origin_hash) = pinned_origin else {
            return false;
        };
        self.matches(name, matched_prefix, origin_hash)
    }

    /// Does `name` bind to `origin_hash` under `matched_prefix`?
    ///
    /// `matched_prefix` is `None` when the config was resolved by exact
    /// name rather than through the prefix table. That combination has
    /// no coherent meaning — there is no dynamic suffix to check — so
    /// it fails closed rather than silently admitting.
    pub fn matches(self, name: &str, matched_prefix: Option<&str>, origin_hash: u64) -> bool {
        let Some(prefix) = matched_prefix else {
            return false;
        };
        let Some(suffix) = name.strip_prefix(prefix) else {
            // Defensive: the registry only hands us a prefix it
            // matched, so this is unreachable — but a mismatch must
            // never read as "bound".
            return false;
        };
        match self {
            Self::OriginHashHex16 => suffix == format!("{origin_hash:016x}"),
        }
    }
}

/// Who may join a **queue group** on a channel.
///
/// Queue groups are work distribution: every published event is
/// delivered to exactly one member of each group. So joining a group is
/// not a routing preference, it is a claim on other members' work — an
/// attacker who joins a production group receives a share of its events
/// and, by not processing them, destroys that share. With `L` honest
/// members and `A` attacker identities, attackers collectively take
/// `A/(L+A)` of selections and each identity takes `1/(L+A)`; the
/// attacker scales its share simply by joining under more identities.
///
/// Note this is an **integrity and availability** boundary, not a
/// confidentiality one: a peer that can subscribe at all can already
/// take every event by subscribing in `Broadcast` mode, so joining a
/// group exposes nothing new. What it does is take work away from the
/// members meant to do it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum QueueGroupPolicy {
    /// Any peer that clears the channel's subscribe gate may join any
    /// group. Historical behaviour, and the default so existing
    /// deployments are unaffected.
    #[default]
    Unrestricted,
    /// Refuse queue-group subscriptions entirely — broadcast only.
    Deny,
    /// A peer may join group `G` only by presenting a chain that
    /// authorizes `SUBSCRIBE` on [`queue_group_hash(channel, G)`],
    /// i.e. a grant that names the specific group.
    ///
    /// Under this policy the group grant **is** the subscribe
    /// authority for that request — a worker does not additionally
    /// need a channel-scoped token. It cannot: the `Subscribe` wire
    /// message carries exactly one chain, so requiring both would make
    /// worker subscription unrepresentable. The model an operator gets
    /// is the intended one: channel-scoped tokens for readers,
    /// group-scoped tokens for workers, and a reader's token is
    /// explicitly not a worker grant. Capability filters still apply.
    ///
    /// An allowlist of group *names* would not do: group names are
    /// operational constants, not secrets, so an attacker simply joins
    /// an allowed one. Nor would a generic "may join queue groups"
    /// scope bit — that separates readers from workers but still lets
    /// any worker join any group on the channel. The authority has to
    /// bind the peer to the group.
    ///
    /// [`queue_group_hash(channel, G)`]: super::queue_group_hash
    TokenBound,
}

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
///
/// # Capability filters are advisory, not an access boundary
///
/// `publish_caps` / `subscribe_caps` match against a node's
/// *self-advertised* `CapabilitySet`: a peer declares its own
/// capabilities in its own signed announcement, so any peer can
/// satisfy a cap-filter simply by advertising the required tag
/// (e.g. self-asserting `role:admin`). Treat cap-filters as
/// matchmaking / intent-routing, **not** as a security boundary.
///
/// The actual access boundary is `require_token` + `token_roots`:
/// a root-anchored [`TokenChain`] cannot be forged because each link
/// is signature-verified up to a root the channel explicitly trusts.
/// Any channel that must restrict who can publish or subscribe must
/// use token enforcement; a cap-filter alone restricts nothing.
#[derive(Debug, Clone)]
pub struct ChannelConfig {
    /// Channel identity (name + hash).
    pub channel_id: ChannelId,
    /// Visibility scope for subnet routing.
    pub visibility: Visibility,
    /// Capability requirements for publishing. `None` = any node can
    /// publish. Advisory only — matched against the node's
    /// self-advertised caps; use `require_token` for a real boundary.
    pub publish_caps: Option<CapabilityFilter>,
    /// Capability requirements for subscribing. `None` = any node can
    /// subscribe. Advisory only — matched against the node's
    /// self-advertised caps; use `require_token` for a real boundary.
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
    /// Bind the dynamic name suffix to the subscriber's own pinned
    /// identity. `None` (default) = any peer that clears the other
    /// gates may subscribe to any name this config covers.
    ///
    /// Only meaningful on a prefix-registered config; see
    /// [`OriginBinding`]. Unlike `publish_caps` / `subscribe_caps`,
    /// this **is** an access boundary — it is evaluated against the
    /// TOFU-pinned peer identity, which a peer cannot self-assert.
    pub subscriber_origin_binding: Option<OriginBinding>,
    /// Who may join a queue group on this channel. See
    /// [`QueueGroupPolicy`]; defaults to `Unrestricted`, which is the
    /// historical behaviour.
    pub queue_group_policy: QueueGroupPolicy,
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
            subscriber_origin_binding: None,
            queue_group_policy: QueueGroupPolicy::default(),
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
    ///
    /// Advisory matchmaking, not access control: caps are
    /// self-advertised, so any peer can satisfy the filter by
    /// declaring the tag. Combine with [`Self::with_token_roots`] to
    /// actually restrict publishers.
    pub fn with_publish_caps(mut self, filter: CapabilityFilter) -> Self {
        self.publish_caps = Some(filter);
        self
    }

    /// Set capability requirements for subscribing.
    ///
    /// Advisory matchmaking, not access control: caps are
    /// self-advertised, so any peer can satisfy the filter by
    /// declaring the tag. Combine with [`Self::with_token_roots`] to
    /// actually restrict subscribers.
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

    /// Bind this (prefix-registered) channel family's dynamic suffix to
    /// the subscribing peer's own pinned identity — see
    /// [`OriginBinding`].
    ///
    /// Callers subscribing to a bound family must have had their
    /// identity pinned on the publisher first, which happens when their
    /// signature-verified direct capability announcement arrives. A peer
    /// that has not announced is rejected (fail closed).
    pub fn with_subscriber_origin_binding(mut self, binding: OriginBinding) -> Self {
        self.subscriber_origin_binding = Some(binding);
        self
    }

    /// Do this node's advertised capabilities satisfy the channel's
    /// `subscribe_caps` filter?
    ///
    /// Split out of [`Self::can_subscribe`] for the `TokenBound`
    /// queue-group path, which supplies its own token authority (the
    /// group grant) but must still apply the capability filter.
    /// Advisory, like every cap filter — see the type docs.
    pub fn caps_allow_subscribe(&self, node_caps: &CapabilitySet) -> bool {
        match self.subscribe_caps {
            Some(ref filter) => filter.matches(node_caps),
            None => true,
        }
    }

    /// Restrict who may join a queue group on this channel — see
    /// [`QueueGroupPolicy`].
    pub fn with_queue_group_policy(mut self, policy: QueueGroupPolicy) -> Self {
        self.queue_group_policy = policy;
        self
    }

    /// Does `chain` authorize this peer to join queue group `group` on
    /// `channel`?
    ///
    /// Returns `true` when the channel places no restriction. Under
    /// [`QueueGroupPolicy::TokenBound`] the chain must root at one of
    /// this channel's `token_roots`, bind at its leaf to `entity_id`,
    /// and authorize `SUBSCRIBE` on the derived group-grant hash — a
    /// grant naming the specific group, not the channel.
    ///
    /// Fails closed: `Deny` refuses, and `TokenBound` with no chain (or
    /// no roots) refuses.
    pub fn can_join_queue_group(
        &self,
        entity_id: &EntityId,
        channel: &str,
        group: &str,
        chain: Option<&TokenChain>,
        revocation: &RevocationRegistry,
        skew_secs: u64,
    ) -> bool {
        match self.queue_group_policy {
            QueueGroupPolicy::Unrestricted => true,
            QueueGroupPolicy::Deny => false,
            QueueGroupPolicy::TokenBound => {
                if self.token_roots.is_empty() {
                    return false;
                }
                let Some(chain) = chain else {
                    return false;
                };
                chain
                    .verify_authorizes(
                        TokenScope::SUBSCRIBE,
                        super::name::queue_group_hash(channel, group),
                        entity_id,
                        &self.token_roots,
                        revocation,
                        skew_secs,
                    )
                    .is_ok()
            }
        }
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

    /// Check if `entity_id` is authorized to publish on `channel_hash`,
    /// presenting `chain`.
    ///
    /// See [`Self::can_subscribe`] for the chain-verification contract
    /// and for why `channel_hash` is a parameter; this is the
    /// `PUBLISH`-scope counterpart.
    pub fn can_publish(
        &self,
        node_caps: &CapabilitySet,
        entity_id: &EntityId,
        channel_hash: ChannelHash,
        chain: Option<&TokenChain>,
        revocation: &RevocationRegistry,
        skew_secs: u64,
    ) -> bool {
        if let Some(ref filter) = self.publish_caps {
            if !filter.matches(node_caps) {
                return false;
            }
        }
        self.token_gate(
            TokenScope::PUBLISH,
            entity_id,
            channel_hash,
            chain,
            revocation,
            skew_secs,
        )
    }

    /// Check if `entity_id` is authorized to subscribe to
    /// `channel_hash`, presenting `chain`.
    ///
    /// When `require_token` is set, `chain` must be a [`TokenChain`]
    /// that (a) roots at one of [`Self::token_roots`], (b) is bound at
    /// its leaf to `entity_id` (the AEAD-verified presenter), and (c)
    /// authorizes `SUBSCRIBE` on `channel_hash` at every link with no
    /// link revoked. A missing chain, an empty `token_roots`, or a
    /// chain that fails verification all reject — fail closed.
    ///
    /// # Why `channel_hash` is a parameter
    ///
    /// It is the hash of the channel the caller actually asked for, NOT
    /// `self.channel_id.hash()`. Those coincide for an exact-match
    /// config, but a **prefix**-registered config's `channel_id` is a
    /// sentinel that `insert_prefix` itself documents as "not used for
    /// hash lookups" — and verifying against it meant a token minted
    /// for the sentinel authorized *every* channel under the prefix,
    /// silently degrading a per-channel binding to a per-prefix one.
    /// Taking the channel explicitly also removes the standing
    /// temptation to reuse one config across many channels and get a
    /// gate that answers about the wrong one.
    pub fn can_subscribe(
        &self,
        node_caps: &CapabilitySet,
        entity_id: &EntityId,
        channel_hash: ChannelHash,
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
            channel_hash,
            chain,
            revocation,
            skew_secs,
        )
    }

    /// Shared token-chain gate for the publish / subscribe checks.
    /// Returns `true` when token enforcement is off (capability filters
    /// already applied by the caller), else verifies the presented
    /// chain roots at one of `token_roots` and authorizes
    /// `channel_hash`. Fails closed when tokens are required but no
    /// roots are configured or no chain is presented.
    fn token_gate(
        &self,
        action: TokenScope,
        entity_id: &EntityId,
        channel_hash: ChannelHash,
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
                channel_hash,
                entity_id,
                &self.token_roots,
                revocation,
                skew_secs,
            )
            .is_ok()
    }

    /// Re-verify a previously-presented `SUBSCRIBE` chain for
    /// `channel_hash` against the current clock + revocation floors,
    /// anchored to this channel's roots. Shared by the periodic expiry
    /// sweep and the publish-time re-check so the root-anchoring
    /// contract (which roots, which action, which channel hash) lives
    /// in exactly one place instead of being re-threaded at each call
    /// site — where it had already started to diverge (`token_roots`
    /// vs. an `unwrap_or(&[])` fallback).
    ///
    /// `channel_hash` is the requested channel's — see
    /// [`Self::can_subscribe`]. Passing the config's own hash here is
    /// what made prefix-registered channels retain a chain under the
    /// sentinel key that the publish path (keyed on the real channel)
    /// could never find, so every such subscriber was accepted and then
    /// revoked before its first delivery.
    pub fn reverify_subscribe(
        &self,
        chain: &TokenChain,
        entity_id: &EntityId,
        channel_hash: ChannelHash,
        revocation: &RevocationRegistry,
        skew_secs: u64,
    ) -> bool {
        chain
            .verify_authorizes(
                TokenScope::SUBSCRIBE,
                channel_hash,
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
        channel_hash: ChannelHash,
        revocation: &RevocationRegistry,
        skew_secs: u64,
    ) -> bool {
        chain
            .verify_authorizes_presigned(
                TokenScope::SUBSCRIBE,
                channel_hash,
                entity_id,
                &self.token_roots,
                revocation,
                skew_secs,
            )
            .is_ok()
    }
}

/// A channel config plus how it was resolved, from
/// [`ChannelConfigRegistry::resolve_by_name`].
#[derive(Debug, Clone)]
pub struct ResolvedConfig {
    /// The resolved configuration.
    pub config: ChannelConfig,
    /// `Some(prefix)` when resolution fell through to the prefix table,
    /// `None` for an exact-name match. [`OriginBinding`] uses this to
    /// split the requested name into prefix + dynamic suffix.
    pub matched_prefix: Option<String>,
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
    /// Serializes every mutation of `configs` + the two reverse
    /// indices, so the three maps are only ever observed in a
    /// consistent state.
    ///
    /// `configs` and the indices are separate DashMaps, so no per-entry
    /// guard can span them. Under concurrent insert/remove that showed
    /// up as index corruption in both directions: a re-registration
    /// racing a removal could have its fresh index entry deleted (the
    /// channel present in `configs` but invisible to `get(hash)`), and
    /// the repair for THAT could re-add a name a second removal had
    /// just taken out (a phantom name in a bucket, which `get` and
    /// `remove` read as a hash collision and answer `None` to — taking
    /// out the *real* channel's lookup as collateral).
    ///
    /// Writes only. Readers stay lock-free on the DashMaps: they are
    /// the hot path, and a reader that observes a mid-write state
    /// resolves through `configs` and simply misses, which is the
    /// pre-existing behaviour for an unregistered channel. Mutations
    /// are control-plane — registration, `net channel rm` — so
    /// serializing them costs nothing measurable.
    ///
    /// NOT reentrant (`parking_lot::Mutex`). Methods that hold it call
    /// the `_locked` inner helpers, never each other.
    write_lock: parking_lot::Mutex<()>,
}

impl ChannelConfigRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            configs: DashMap::new(),
            by_hash: DashMap::new(),
            by_wire_hash: DashMap::new(),
            prefix_configs: DashMap::new(),
            write_lock: parking_lot::Mutex::new(()),
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

    /// Register a prefix-matched config **only if that prefix has no
    /// entry yet**. Returns `true` if this call installed the config,
    /// `false` if an entry already existed (which is left untouched).
    ///
    /// The prefix counterpart of [`Self::insert_if_absent`], and the
    /// operation auto-registration must use so it cannot silently
    /// discard an operator's ACL. See that method for the rationale.
    pub fn insert_prefix_if_absent(
        &self,
        prefix: impl Into<String>,
        config: ChannelConfig,
    ) -> bool {
        match self.prefix_configs.entry(prefix.into()) {
            dashmap::mapref::entry::Entry::Occupied(_) => false,
            dashmap::mapref::entry::Entry::Vacant(slot) => {
                warn_if_fail_closed(&config);
                slot.insert(config);
                true
            }
        }
    }

    /// Remove a prefix-matched config. Returns the removed config
    /// if it existed.
    pub fn remove_prefix(&self, prefix: &str) -> Option<ChannelConfig> {
        self.prefix_configs.remove(prefix).map(|(_, v)| v)
    }

    /// Install the standard channel policy for an RPC-style service:
    /// the exact `<service>.requests` channel, and the
    /// `<service>.replies.` prefix bound to each caller's own origin.
    ///
    /// **Install-if-absent, never replace** (H2) — an ACL the operator
    /// registered before serving survives untouched. **Origin-bound
    /// reply prefix** (H3) — a peer may subscribe only to the one reply
    /// channel that encodes its own pinned identity, not to another
    /// caller's.
    ///
    /// Lives on the registry, not on an SDK type, because there are
    /// several serve paths and they do not share a receiver: the SDK's
    /// `Mesh::serve_rpc*`, the `aggregator` module, and the org facade's
    /// `serve_org_bytes_node` (which holds an `Arc<MeshNode>` for the
    /// language bindings). What they DO share is this registry.
    ///
    /// That sharing is the whole point. This policy has now drifted
    /// twice, both times the same way — a serve path carrying its own
    /// copy of the registration and not receiving a later fix. The
    /// aggregator kept a replacing insert and never gained the origin
    /// binding, so aggregator reply channels stayed world-subscribable
    /// after H2 and H3 were fixed for `serve_rpc`; the org path did the
    /// same and was still doing it after the aggregator was folded in.
    /// A copy per receiver type is not a shared implementation. One
    /// implementation on the object all of them already hold is.
    ///
    /// A caller with no registry (possible via the bare `MeshNode::new`
    /// path) simply has no channel ACLs in play and does not call this.
    ///
    /// **All or nothing**, and validated against the names callers will
    /// actually use.
    ///
    /// Three names are in play and none of them is the same length:
    ///
    /// | name | suffix bytes |
    /// |---|---|
    /// | `<service>.requests` | 9 |
    /// | `<service>.replies.prefix` (sentinel) | 15 |
    /// | `<service>.replies.<16 hex>` (real) | 25 |
    ///
    /// So there are two bands near the channel-name length limit where a
    /// naive implementation half-succeeds, and they fail differently:
    ///
    /// - Request fits, sentinel does not. Installing the half that fits
    ///   leaves the request channel looking deliberately configured
    ///   while replies fall through to the unregistered-channel policy,
    ///   unbound — the H3 posture, reached by accident.
    /// - Both fit, but no REAL reply channel does. Everything looks
    ///   installed, and then no caller can ever name a reply channel
    ///   that validates, so calls hang until they time out. Checking the
    ///   sentinel does not catch this: it is 10 bytes shorter than the
    ///   thing it stands for.
    ///
    /// Hence the concrete probe below. The sentinel is still what gets
    /// STORED — it must stay unroutable — but what gets VALIDATED is a
    /// real per-caller name.
    pub fn install_rpc_service_defaults(&self, service: &str) {
        let Ok(req_channel) = ChannelName::new(&format!("{service}.requests")) else {
            return;
        };
        // Probe, not a channel: every origin hash renders as exactly 16
        // hex digits, so any value answers "does a per-caller reply
        // channel fit under this service name?" for all of them.
        if ChannelName::new(&format!("{service}.replies.{:016x}", 0u64)).is_err() {
            return;
        }
        // The sentinel name is never routed — it exists so the prefix
        // entry has a `ChannelId` to carry. Token gates on a prefix
        // entry evaluate against the requested CONCRETE channel (M1),
        // not this. Kept deliberately unroutable rather than reusing the
        // probe: `<service>.replies.0000000000000000` is a name a real
        // caller could hold, and a sentinel should not collide with one.
        let Ok(sentinel) = ChannelName::new(&format!("{service}.replies.prefix")) else {
            return;
        };

        // Return values ignored on purpose: "already registered" is the
        // operator-configured case, which is exactly what this protects.
        let _ = self.insert_if_absent(ChannelConfig::new(ChannelId::new(req_channel)));
        let cfg = ChannelConfig::new(ChannelId::new(sentinel))
            .with_subscriber_origin_binding(OriginBinding::OriginHashHex16);
        let _ = self.insert_prefix_if_absent(format!("{service}.replies."), cfg);
    }

    /// Register a channel configuration, **replacing** any existing
    /// entry for the same canonical name.
    ///
    /// Callers that must not clobber an existing policy — notably
    /// anything auto-registering a default on behalf of a subsystem —
    /// want [`Self::insert_if_absent`] instead.
    pub fn insert(&self, config: ChannelConfig) {
        warn_if_fail_closed(&config);
        let name = config.channel_id.name().to_string();
        let hash = config.channel_id.hash();
        let wire_hash = config.channel_id.wire_hash();
        let _w = self.write_lock.lock();
        self.configs.insert(name.clone(), config);
        self.index_name(hash, wire_hash, name);
    }

    /// Register a channel configuration **only if that canonical name
    /// has no entry yet**. Returns `true` if this call installed the
    /// config, `false` if an entry already existed (which is left
    /// untouched).
    ///
    /// This exists because [`Self::insert`] replaces, and a subsystem
    /// that auto-registers a permissive default for a channel it owns
    /// (nRPC's `<service>.requests` / `<service>.replies.`) would
    /// otherwise silently destroy an ACL the operator installed first
    /// — with no error and no log, leaving a posture identical to the
    /// default. Auto-registration must be "install a default if the
    /// operator expressed no opinion," which is exactly this
    /// operation.
    ///
    /// Atomic against concurrent callers: exactly one of N racing
    /// callers observes `true`, and its index update is not visible
    /// before its `configs` entry.
    pub fn insert_if_absent(&self, config: ChannelConfig) -> bool {
        let name = config.channel_id.name().to_string();
        let hash = config.channel_id.hash();
        let wire_hash = config.channel_id.wire_hash();
        let _w = self.write_lock.lock();
        let installed = match self.configs.entry(name.clone()) {
            dashmap::mapref::entry::Entry::Occupied(_) => false,
            dashmap::mapref::entry::Entry::Vacant(slot) => {
                warn_if_fail_closed(&config);
                slot.insert(config);
                true
            }
        };
        if installed {
            self.index_name(hash, wire_hash, name);
        }
        installed
    }

    /// Add `name` to the canonical- and wire-hash reverse indices,
    /// skipping a name already present in either bucket.
    ///
    /// The de-dup is load-bearing, not hygiene. [`Self::get`] and
    /// [`Self::remove`] treat a bucket holding more than one name as a
    /// hash collision and return `None` to avoid applying the wrong
    /// channel's policy. Pre-fix, `insert` pushed unconditionally, so
    /// re-registering the *same* channel (which the SDK documents as
    /// idempotent, and which `serve_rpc` does on every call) grew the
    /// bucket to `[name, name]` and made `get(hash)` start returning
    /// `None` for a channel that plainly exists — a self-inflicted
    /// collision that disabled canonical-hash lookup for that channel.
    fn index_name(&self, hash: ChannelHash, wire_hash: u16, name: String) {
        let mut by_hash = self.by_hash.entry(hash).or_default();
        if !by_hash.iter().any(|n| n == &name) {
            by_hash.push(name.clone());
        }
        drop(by_hash);
        let mut by_wire = self.by_wire_hash.entry(wire_hash).or_default();
        if !by_wire.iter().any(|n| n == &name) {
            by_wire.push(name);
        }
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
        self.prefix_configs
            .get(&self.longest_matching_prefix(name)?)
    }

    /// The longest registered prefix that `name` starts with, if any.
    ///
    /// Single source of truth for prefix resolution, shared by
    /// [`Self::get_by_name`] and [`Self::resolve_by_name`]. Those two
    /// answer the same authorization question and previously each
    /// carried their own copy of this loop — two places to keep the
    /// longest-match rule correct, on the path that decides which ACL
    /// applies.
    ///
    /// Longest match means a more specific entry overrides a more
    /// general one, and makes resolution deterministic across runs: an
    /// earlier "first match wins" was DashMap-shard-order dependent and
    /// could silently flip between builds.
    fn longest_matching_prefix(&self, name: &str) -> Option<String> {
        let mut best_len = 0usize;
        let mut best_key: Option<String> = None;
        for entry in self.prefix_configs.iter() {
            let prefix = entry.key();
            if name.starts_with(prefix) && prefix.len() >= best_len {
                best_len = prefix.len();
                best_key = Some(prefix.clone());
            }
        }
        best_key
    }

    /// Resolve `name` the same way [`Self::get_by_name`] does, but also
    /// report **which prefix matched** when resolution came from the
    /// prefix table.
    ///
    /// [`OriginBinding`] needs that prefix to locate the dynamic suffix
    /// inside the requested name; `get_by_name` alone discards it, and
    /// re-deriving it at the call site would duplicate the
    /// longest-match rule (and drift from it). Returns owned values
    /// because every caller on the authorization path clones the config
    /// immediately anyway, to drop the registry guard before doing
    /// signature work.
    pub fn resolve_by_name(&self, name: &str) -> Option<ResolvedConfig> {
        if let Some(exact) = self.configs.get(name) {
            return Some(ResolvedConfig {
                config: exact.clone(),
                matched_prefix: None,
            });
        }
        let key = self.longest_matching_prefix(name)?;
        let config = self.prefix_configs.get(&key)?.clone();
        Some(ResolvedConfig {
            config,
            matched_prefix: Some(key),
        })
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
        // One critical section covering the index read AND the removal
        // it selects, so the name cannot be replaced by a different
        // channel in between and get removed in its place.
        let _w = self.write_lock.lock();
        let name = {
            let names = self.by_hash.get(&channel_hash)?;
            if names.len() != 1 {
                return None;
            }
            names.first()?.clone()
        };
        self.remove_by_name_locked(&name)
    }

    /// Remove a channel config by exact name (collision-safe).
    ///
    /// Returns the removed config if it existed.
    pub fn remove_by_name(&self, name: &str) -> Option<ChannelConfig> {
        let _w = self.write_lock.lock();
        self.remove_by_name_locked(name)
    }

    /// [`Self::remove_by_name`] for callers already holding
    /// `write_lock`. Split out because `parking_lot::Mutex` is not
    /// reentrant and [`Self::remove`] must hold the lock across its
    /// index lookup.
    ///
    /// Under the lock, `configs.remove` and the index cleanup are one
    /// step, which is what makes the pair sound. Previously they were
    /// not, and the repair each defect needed reintroduced the other:
    ///
    /// - Without a repair pass, a re-registration landing between the
    ///   `configs.remove` and the `retain` had its fresh index entry
    ///   deleted — the channel present in `configs`, invisible to
    ///   `get(hash)`.
    /// - With one (`if configs.contains_key(name) { index_name(..) }`),
    ///   a second removal completing between that test and the re-index
    ///   put a name back into the bucket with nothing behind it. `get`
    ///   and `remove` treat a bucket holding more than one name as a
    ///   hash collision and answer `None`, so a phantom entry disables
    ///   lookup for whatever real channel shares the bucket.
    ///
    /// Serializing removes the interleaving both were patching around,
    /// so neither the repair nor its own failure mode remains.
    fn remove_by_name_locked(&self, name: &str) -> Option<ChannelConfig> {
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
    use crate::adapter::net::channel::{channel_hash, queue_group_hash, ChannelName};
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

        assert!(config.can_publish(
            &caps,
            entity.entity_id(),
            config.channel_id.hash(),
            None,
            &rev,
            0
        ));
        assert!(config.can_subscribe(
            &caps,
            entity.entity_id(),
            config.channel_id.hash(),
            None,
            &rev,
            0
        ));
    }

    #[test]
    fn test_capability_restricted_channel() {
        let id = ChannelId::parse("compute/gpu-tasks").unwrap();
        let config =
            ChannelConfig::new(id).with_publish_caps(CapabilityFilter::new().require_gpu());

        let entity = EntityKeypair::generate();
        let rev = RevocationRegistry::new();

        let no_gpu = make_caps(false);
        assert!(!config.can_publish(
            &no_gpu,
            entity.entity_id(),
            config.channel_id.hash(),
            None,
            &rev,
            0
        ));

        let with_gpu = make_caps(true);
        assert!(config.can_publish(
            &with_gpu,
            entity.entity_id(),
            config.channel_id.hash(),
            None,
            &rev,
            0
        ));
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
        assert!(!config.can_publish(
            &caps,
            subject.entity_id(),
            config.channel_id.hash(),
            None,
            &rev,
            0
        ));

        // Self-issued (issuer == subject, NOT the channel owner) ->
        // denied. Pre-fix this was the privilege-escalation hole:
        // `verify()` + `TokenCache::check` accepted any self-consistent
        // token regardless of issuer.
        let self_chain = direct_chain(&subject, &subject, TokenScope::PUBLISH, id.hash());
        assert!(
            !config.can_publish(
                &caps,
                subject.entity_id(),
                config.channel_id.hash(),
                Some(&self_chain),
                &rev,
                0
            ),
            "self-issued token must be rejected: its issuer is not a channel root"
        );

        // Owner-issued -> allowed.
        let owner_chain = direct_chain(&owner, &subject, TokenScope::PUBLISH, id.hash());
        assert!(config.can_publish(
            &caps,
            subject.entity_id(),
            config.channel_id.hash(),
            Some(&owner_chain),
            &rev,
            0
        ));
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
        assert!(!config.can_subscribe(
            &caps,
            anyone.entity_id(),
            config.channel_id.hash(),
            Some(&chain),
            &rev,
            0
        ));
        assert!(!config.can_subscribe(
            &caps,
            anyone.entity_id(),
            config.channel_id.hash(),
            None,
            &rev,
            0
        ));
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
        assert!(!config.can_subscribe(
            &caps,
            subject.entity_id(),
            config.channel_id.hash(),
            None,
            &rev,
            0
        ));
        // Owner-issued chain -> allowed.
        let owner_chain = direct_chain(&owner, &subject, TokenScope::SUBSCRIBE, id.hash());
        assert!(config.can_subscribe(
            &caps,
            subject.entity_id(),
            config.channel_id.hash(),
            Some(&owner_chain),
            &rev,
            0
        ));
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
        assert!(!config.can_subscribe(
            &caps,
            attacker.entity_id(),
            config.channel_id.hash(),
            Some(&chain),
            &rev,
            0
        ));
        // The intended subject is accepted.
        assert!(config.can_subscribe(
            &caps,
            intended.entity_id(),
            config.channel_id.hash(),
            Some(&chain),
            &rev,
            0
        ));
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
        assert!(config.can_subscribe(
            &caps,
            leaf.entity_id(),
            config.channel_id.hash(),
            Some(&chain),
            &rev,
            0
        ));
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
        assert!(!config.can_subscribe(
            &caps,
            leaf.entity_id(),
            config.channel_id.hash(),
            Some(&chain),
            &rev,
            0
        ));
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
        assert!(!config.can_publish(
            &caps,
            leaf.entity_id(),
            config.channel_id.hash(),
            Some(&chain),
            &rev,
            0
        ));
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
        assert!(config.can_subscribe(
            &caps,
            leaf.entity_id(),
            config.channel_id.hash(),
            Some(&chain),
            &rev,
            0
        ));

        // Owner bumps its revocation floor above the chain's generation
        // (0). The root link falls below the floor → whole chain dies,
        // even though the delegated child's issuer is `mid`, not `owner`.
        rev.revoke_below(owner.entity_id(), 1);
        assert!(
            !config.can_subscribe(
                &caps,
                leaf.entity_id(),
                config.channel_id.hash(),
                Some(&chain),
                &rev,
                0
            ),
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
        assert!(!config.can_publish(
            &with_gpu,
            subject.entity_id(),
            config.channel_id.hash(),
            None,
            &rev,
            0
        ));

        // Has token but no GPU -> denied.
        let no_gpu = make_caps(false);
        assert!(!config.can_publish(
            &no_gpu,
            subject.entity_id(),
            config.channel_id.hash(),
            Some(&owner_chain),
            &rev,
            0
        ));

        // Has both -> allowed.
        assert!(config.can_publish(
            &with_gpu,
            subject.entity_id(),
            config.channel_id.hash(),
            Some(&owner_chain),
            &rev,
            0
        ));
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

    // ---- Review follow-ups: registry index consistency ----

    /// A remove that interleaves with a re-registration must not leave
    /// the NEW config stranded — present in `configs` but invisible to
    /// the reverse indices that `get(hash)` / `get_by_wire_hash`
    /// resolve through.
    ///
    /// Sequenced deterministically rather than raced, so it pins the
    /// outcome rather than an interleaving: remove-then-reregister and
    /// reregister-then-remove must both leave the index agreeing with
    /// `configs`. (The interleaving itself can no longer occur —
    /// `remove_by_name_locked` holds the registry write lock across both
    /// steps — but the property is what callers depend on, and it should
    /// keep being asserted independently of how it is achieved.)
    #[test]
    fn remove_racing_reregistration_leaves_the_index_consistent() {
        let reg = ChannelConfigRegistry::new();
        let id = ChannelId::parse("svc.requests").unwrap();
        let hash = id.hash();
        let wire = id.wire_hash();

        reg.insert(ChannelConfig::new(id.clone()).with_priority(1));
        // Re-register, then remove: the remove's cleanup targets a name
        // that is legitimately present again.
        reg.insert(ChannelConfig::new(id.clone()).with_priority(2));
        reg.remove_by_name("svc.requests");
        assert!(
            reg.get(hash).is_none(),
            "after a completed remove the channel is gone"
        );

        // Now the interleaved shape: removed, then re-registered.
        reg.insert(ChannelConfig::new(id).with_priority(3));
        assert_eq!(
            reg.get(hash).map(|c| c.priority),
            Some(3),
            "a channel re-registered after removal must be reachable by \
             canonical hash"
        );
        assert_eq!(
            reg.get_by_wire_hash(wire).map(|c| c.priority),
            Some(3),
            "…and by wire hash"
        );
    }

    /// The concurrent form of the same property. Whatever the
    /// interleaving, the registry must not end with a config that
    /// `get_by_name` finds but `get(hash)` cannot.
    #[test]
    fn concurrent_remove_and_reregister_never_strands_the_index() {
        use std::sync::Arc as StdArc;

        for _ in 0..64 {
            let reg = StdArc::new(ChannelConfigRegistry::new());
            let id = ChannelId::parse("svc.requests").unwrap();
            let hash = id.hash();
            reg.insert(ChannelConfig::new(id.clone()));

            std::thread::scope(|s| {
                let r1 = reg.clone();
                s.spawn(move || {
                    r1.remove_by_name("svc.requests");
                });
                let r2 = reg.clone();
                let id2 = id.clone();
                s.spawn(move || {
                    r2.insert(ChannelConfig::new(id2).with_priority(9));
                });
            });

            // The invariant: `configs` and the reverse index agree.
            if reg.get_by_name("svc.requests").is_some() {
                assert!(
                    reg.get(hash).is_some(),
                    "config present by name but unreachable by canonical \
                     hash — the reverse index was stranded"
                );
            }
        }
    }

    /// Both resolution entry points must agree, since they answer the
    /// same authorization question. Pinned because they used to carry
    /// separate copies of the longest-match loop.
    #[test]
    fn get_by_name_and_resolve_by_name_agree_on_prefix_resolution() {
        let reg = ChannelConfigRegistry::new();
        reg.insert_prefix(
            "svc.",
            ChannelConfig::new(ChannelId::parse("svc.general").unwrap()).with_priority(1),
        );
        reg.insert_prefix(
            "svc.replies.",
            ChannelConfig::new(ChannelId::parse("svc.replies.prefix").unwrap()).with_priority(2),
        );
        reg.insert(ChannelConfig::new(ChannelId::parse("svc.exact").unwrap()).with_priority(3));

        for name in ["svc.replies.aa", "svc.other", "svc.exact", "nomatch"] {
            let via_get = reg.get_by_name(name).map(|c| c.priority);
            let via_resolve = reg.resolve_by_name(name).map(|r| r.config.priority);
            assert_eq!(
                via_get, via_resolve,
                "get_by_name and resolve_by_name disagreed for {name:?}"
            );
        }

        // And the longest prefix wins, not merely any match.
        assert_eq!(
            reg.resolve_by_name("svc.replies.aa")
                .unwrap()
                .matched_prefix
                .as_deref(),
            Some("svc.replies.")
        );
    }

    // ---- M2 (2026-07-31 audit): queue-group membership authority ----

    /// Default is unchanged: any subscriber may join any group.
    #[test]
    fn queue_group_unrestricted_by_default() {
        let peer = EntityKeypair::generate();
        let rev = RevocationRegistry::new();
        let config = ChannelConfig::new(ChannelId::parse("work/queue").unwrap());
        assert_eq!(config.queue_group_policy, QueueGroupPolicy::Unrestricted);
        assert!(config.can_join_queue_group(
            peer.entity_id(),
            "work/queue",
            "workers",
            None,
            &rev,
            0
        ));
    }

    /// `Deny` refuses every group, chain or not.
    #[test]
    fn queue_group_deny_refuses_even_with_a_valid_chain() {
        let owner = EntityKeypair::generate();
        let peer = EntityKeypair::generate();
        let rev = RevocationRegistry::new();
        let config = ChannelConfig::new(ChannelId::parse("work/queue").unwrap())
            .with_token_roots(vec![owner.entity_id().clone()])
            .with_queue_group_policy(QueueGroupPolicy::Deny);

        let chain = direct_chain(
            &owner,
            &peer,
            TokenScope::SUBSCRIBE,
            queue_group_hash("work/queue", "workers"),
        );
        assert!(!config.can_join_queue_group(
            peer.entity_id(),
            "work/queue",
            "workers",
            Some(&chain),
            &rev,
            0
        ));
    }

    /// The core M2 property: a grant for one group must not admit the
    /// holder to a DIFFERENT group. An allowlist of group names could
    /// not express this — names are operational constants, not secrets.
    #[test]
    fn queue_group_grant_binds_to_one_specific_group() {
        let owner = EntityKeypair::generate();
        let worker = EntityKeypair::generate();
        let rev = RevocationRegistry::new();
        let channel = "work/queue";
        let config = ChannelConfig::new(ChannelId::parse(channel).unwrap())
            .with_token_roots(vec![owner.entity_id().clone()])
            .with_queue_group_policy(QueueGroupPolicy::TokenBound);

        let chain = direct_chain(
            &owner,
            &worker,
            TokenScope::SUBSCRIBE,
            queue_group_hash(channel, "batch"),
        );

        assert!(
            config.can_join_queue_group(
                worker.entity_id(),
                channel,
                "batch",
                Some(&chain),
                &rev,
                0
            ),
            "the granted group must be joinable"
        );
        assert!(
            !config.can_join_queue_group(
                worker.entity_id(),
                channel,
                "realtime",
                Some(&chain),
                &rev,
                0
            ),
            "a grant for one group must not admit the holder to another — \
             that is the work-stealing this policy exists to stop"
        );
    }

    /// A plain channel-scoped SUBSCRIBE token is NOT a worker grant.
    /// Otherwise every legitimate subscriber would silently keep the
    /// ability to join any group and the policy would be a no-op.
    #[test]
    fn channel_subscribe_token_is_not_a_queue_group_grant() {
        let owner = EntityKeypair::generate();
        let reader = EntityKeypair::generate();
        let rev = RevocationRegistry::new();
        let channel = "work/queue";
        let id = ChannelId::parse(channel).unwrap();
        let config = ChannelConfig::new(id.clone())
            .with_token_roots(vec![owner.entity_id().clone()])
            .with_queue_group_policy(QueueGroupPolicy::TokenBound);

        // Scoped to the CHANNEL, which is what an ordinary
        // read-only subscriber (e.g. an auditor) would hold.
        let chain = direct_chain(&owner, &reader, TokenScope::SUBSCRIBE, id.hash());

        assert!(
            config.can_subscribe(
                &make_caps(false),
                reader.entity_id(),
                id.hash(),
                Some(&chain),
                &rev,
                0
            ),
            "precondition: it is a valid subscribe credential"
        );
        assert!(
            !config.can_join_queue_group(
                reader.entity_id(),
                channel,
                "workers",
                Some(&chain),
                &rev,
                0
            ),
            "a read-only subscriber must not be able to steal worker traffic"
        );
    }

    /// TokenBound fails closed with no chain and with no roots.
    #[test]
    fn queue_group_token_bound_fails_closed() {
        let owner = EntityKeypair::generate();
        let peer = EntityKeypair::generate();
        let rev = RevocationRegistry::new();
        let channel = "work/queue";

        let rooted = ChannelConfig::new(ChannelId::parse(channel).unwrap())
            .with_token_roots(vec![owner.entity_id().clone()])
            .with_queue_group_policy(QueueGroupPolicy::TokenBound);
        assert!(
            !rooted.can_join_queue_group(peer.entity_id(), channel, "w", None, &rev, 0),
            "no chain presented → refuse"
        );

        let rootless = ChannelConfig::new(ChannelId::parse(channel).unwrap())
            .with_queue_group_policy(QueueGroupPolicy::TokenBound);
        let chain = direct_chain(
            &owner,
            &peer,
            TokenScope::SUBSCRIBE,
            queue_group_hash(channel, "w"),
        );
        assert!(
            !rootless.can_join_queue_group(peer.entity_id(), channel, "w", Some(&chain), &rev, 0),
            "no roots to anchor against → refuse"
        );
    }

    /// The `#` separator keeps group grants and channel grants in
    /// disjoint hash spaces: `#` is outside the channel-name charset,
    /// so no legitimate channel name can ever hash to a group grant.
    #[test]
    fn queue_group_hash_cannot_collide_with_a_channel_name() {
        let h = queue_group_hash("work/queue", "workers");
        // The only string that would produce it is not a legal name.
        assert!(ChannelName::new("work/queue#workers").is_err());
        assert_ne!(h, channel_hash("work/queue"));
        assert_ne!(h, channel_hash("work/queueworkers"));
        // Distinct groups on one channel are distinct grants.
        assert_ne!(h, queue_group_hash("work/queue", "other"));
        // Same group name on distinct channels are distinct grants.
        assert_ne!(h, queue_group_hash("work/other", "workers"));
    }

    // ---- M1 (2026-07-31 audit): gates key on the REQUESTED channel ----

    /// A token minted for one channel under a prefix must not authorize
    /// a sibling under the same prefix.
    ///
    /// Pre-fix the gate verified against `self.channel_id.hash()`, and
    /// for a prefix-registered config that is a sentinel standing for
    /// the whole family — so one token minted for the sentinel
    /// authorized every channel beneath it, silently degrading a
    /// per-channel binding to a per-prefix one.
    #[test]
    fn prefix_config_gate_binds_to_the_requested_channel_not_the_sentinel() {
        let owner = EntityKeypair::generate();
        let subject = EntityKeypair::generate();
        let caps = make_caps(false);
        let rev = RevocationRegistry::new();

        let sentinel = ChannelId::parse("svc.replies.prefix").unwrap();
        let config =
            ChannelConfig::new(sentinel.clone()).with_token_roots(vec![owner.entity_id().clone()]);

        let mine = channel_hash("svc.replies.aaaa");
        let theirs = channel_hash("svc.replies.bbbb");

        // A token for MY channel authorizes my channel...
        let chain = direct_chain(&owner, &subject, TokenScope::SUBSCRIBE, mine);
        assert!(config.can_subscribe(&caps, subject.entity_id(), mine, Some(&chain), &rev, 0));
        // ...and not a sibling under the same prefix.
        assert!(
            !config.can_subscribe(&caps, subject.entity_id(), theirs, Some(&chain), &rev, 0),
            "a token for one channel must not authorize a sibling under the \
             same prefix"
        );

        // A token minted for the SENTINEL authorizes nothing real —
        // that was the per-prefix skeleton key.
        let sentinel_chain = direct_chain(&owner, &subject, TokenScope::SUBSCRIBE, sentinel.hash());
        assert!(
            !config.can_subscribe(
                &caps,
                subject.entity_id(),
                mine,
                Some(&sentinel_chain),
                &rev,
                0
            ),
            "a sentinel-scoped token must not authorize a real channel"
        );
    }

    /// The publish counterpart, and the reason `set_publish_chain` was
    /// unreachable for token-gated prefix channels: it stores under the
    /// real channel hash while the gate asked about the sentinel.
    #[test]
    fn prefix_config_publish_gate_binds_to_the_requested_channel() {
        let owner = EntityKeypair::generate();
        let subject = EntityKeypair::generate();
        let caps = make_caps(false);
        let rev = RevocationRegistry::new();

        let sentinel = ChannelId::parse("svc.requests.prefix").unwrap();
        let config = ChannelConfig::new(sentinel).with_token_roots(vec![owner.entity_id().clone()]);

        let real = channel_hash("svc.requests.aaaa");
        let chain = direct_chain(&owner, &subject, TokenScope::PUBLISH, real);

        assert!(config.can_publish(&caps, subject.entity_id(), real, Some(&chain), &rev, 0));
        assert!(
            !config.can_publish(
                &caps,
                subject.entity_id(),
                channel_hash("svc.requests.bbbb"),
                Some(&chain),
                &rev,
                0
            ),
            "a publish token for one channel must not authorize a sibling"
        );
    }

    /// `reverify_subscribe*` must ask about the same channel the
    /// subscribe gate did, or the publish-time re-check and the sweep
    /// disagree with the decision that admitted the peer.
    #[test]
    fn reverify_paths_bind_to_the_requested_channel() {
        let owner = EntityKeypair::generate();
        let subject = EntityKeypair::generate();
        let rev = RevocationRegistry::new();

        let sentinel = ChannelId::parse("svc.replies.prefix").unwrap();
        let config = ChannelConfig::new(sentinel).with_token_roots(vec![owner.entity_id().clone()]);

        let mine = channel_hash("svc.replies.aaaa");
        let chain = direct_chain(&owner, &subject, TokenScope::SUBSCRIBE, mine);

        for reverify in [
            ChannelConfig::reverify_subscribe as fn(&_, &_, &_, u64, &_, u64) -> bool,
            ChannelConfig::reverify_subscribe_presigned,
        ] {
            assert!(reverify(
                &config,
                &chain,
                subject.entity_id(),
                mine,
                &rev,
                0
            ));
            assert!(
                !reverify(
                    &config,
                    &chain,
                    subject.entity_id(),
                    channel_hash("svc.replies.bbbb"),
                    &rev,
                    0
                ),
                "re-verify must reject a chain that does not authorize the \
                 channel being published to"
            );
        }
    }

    // ---- H3 (2026-07-31 audit): origin-bound channel families ----

    const OB: OriginBinding = OriginBinding::OriginHashHex16;
    const OB_PREFIX: &str = "svc.replies.";

    /// The rule the whole finding turns on: a peer whose identity is
    /// not pinned is REJECTED. Admitting it would let an attacker
    /// bypass the binding by simply never announcing.
    #[test]
    fn origin_binding_rejects_unpinned_peer() {
        let name = format!("{OB_PREFIX}{:016x}", 0xABCD_1234_5678_9ABCu64);
        assert!(
            !OB.authorizes(&name, Some(OB_PREFIX), None),
            "an unpinned peer must never be authorized, even for a \
             well-formed name"
        );
    }

    #[test]
    fn origin_binding_admits_matching_origin() {
        let origin = 0xABCD_1234_5678_9ABCu64;
        let name = format!("{OB_PREFIX}{origin:016x}");
        assert!(OB.authorizes(&name, Some(OB_PREFIX), Some(origin)));
    }

    /// The attack: a pinned peer asking for a name that encodes some
    /// OTHER peer's origin.
    #[test]
    fn origin_binding_rejects_other_peers_origin() {
        let victim = 0xABCD_1234_5678_9ABCu64;
        let attacker = 0x0011_2233_4455_6677u64;
        let name = format!("{OB_PREFIX}{victim:016x}");
        assert!(
            !OB.authorizes(&name, Some(OB_PREFIX), Some(attacker)),
            "a peer must not claim a channel naming another peer's origin"
        );
    }

    /// A binding on an exact-match config has no dynamic suffix to
    /// check, so it fails closed rather than admitting.
    #[test]
    fn origin_binding_without_a_matched_prefix_fails_closed() {
        let origin = 0xABCD_1234_5678_9ABCu64;
        let name = format!("{OB_PREFIX}{origin:016x}");
        assert!(!OB.authorizes(&name, None, Some(origin)));
    }

    /// Formatting is exact: no truncation, no case folding, no
    /// suffix-prefix matching.
    #[test]
    fn origin_binding_requires_exact_16_hex_suffix() {
        let origin = 0x0000_0000_0000_00ABu64;
        for bad in [
            "ab",                // unpadded
            "AB",                // uppercase (also unpadded)
            "00000000000000ab0", // trailing garbage
            "00000000000000a",   // short
            "00000000000000AB",  // uppercase, padded
        ] {
            let name = format!("{OB_PREFIX}{bad}");
            assert!(
                !OB.authorizes(&name, Some(OB_PREFIX), Some(origin)),
                "suffix {bad:?} must not authorize origin {origin:#x}"
            );
        }
        // The canonical rendering does authorize.
        let good = format!("{OB_PREFIX}{origin:016x}");
        assert!(OB.authorizes(&good, Some(OB_PREFIX), Some(origin)));
    }

    /// A config carrying no binding is unaffected — the gate is opt-in.
    #[test]
    fn config_without_binding_is_unconstrained() {
        let cfg = ChannelConfig::new(ChannelId::parse("svc.replies.prefix").unwrap());
        assert!(cfg.subscriber_origin_binding.is_none());
    }

    /// `resolve_by_name` must report the prefix it matched, or the
    /// binding has no way to locate the dynamic suffix.
    #[test]
    fn resolve_by_name_reports_the_matched_prefix() {
        let reg = ChannelConfigRegistry::new();
        let sentinel = ChannelId::parse("svc.replies.prefix").unwrap();
        reg.insert_prefix(
            OB_PREFIX,
            ChannelConfig::new(sentinel).with_subscriber_origin_binding(OB),
        );

        let resolved = reg
            .resolve_by_name("svc.replies.00112233445566aa")
            .expect("prefix must resolve");
        assert_eq!(resolved.matched_prefix.as_deref(), Some(OB_PREFIX));
        assert_eq!(resolved.config.subscriber_origin_binding, Some(OB));

        // An exact-match resolution reports no prefix.
        let exact = ChannelId::parse("plain.channel").unwrap();
        reg.insert(ChannelConfig::new(exact));
        let resolved = reg.resolve_by_name("plain.channel").expect("exact");
        assert!(resolved.matched_prefix.is_none());
    }

    // ---- R9: the one shared RPC service-channel registration ----

    /// The H2 + H3 content of `install_rpc_service_defaults`, asserted
    /// behaviourally rather than by scanning for method names.
    ///
    /// This is the policy every serve path now shares — `serve_rpc*`,
    /// the aggregator, and the org facade. It has drifted twice, each
    /// time because a serve path carried its own copy and a later fix
    /// landed on only one of them, so it is worth pinning what the
    /// policy DOES and not just where it lives.
    #[test]
    fn rpc_service_defaults_are_install_if_absent_and_origin_bound() {
        let reg = ChannelConfigRegistry::new();
        let root = EntityKeypair::generate();

        // H2: an operator's strict ACL, registered before serving.
        reg.insert(
            ChannelConfig::new(ChannelId::parse("svc.requests").unwrap())
                .with_token_roots(vec![root.entity_id().clone()]),
        );
        reg.insert_prefix(
            "svc.replies.",
            ChannelConfig::new(ChannelId::parse("svc.replies.prefix").unwrap())
                .with_token_roots(vec![root.entity_id().clone()]),
        );

        reg.install_rpc_service_defaults("svc");

        assert!(
            reg.get_by_name("svc.requests").unwrap().token_required(),
            "H2: serving must not replace an ACL the operator registered \
             first — a replacing insert destroys it silently, leaving a \
             posture identical to the default"
        );
        assert!(
            reg.get_by_name("svc.replies.abcdef0123456789")
                .unwrap()
                .token_required(),
            "H2 applies to the reply PREFIX too"
        );

        // …and on a clean registry it installs both, with the binding.
        let fresh = ChannelConfigRegistry::new();
        fresh.install_rpc_service_defaults("svc");

        assert!(
            fresh.get_by_name("svc.requests").is_some(),
            "the request channel must be installed when unclaimed"
        );
        let replies = fresh
            .get_by_name("svc.replies.abcdef0123456789")
            .expect("the reply prefix must admit a per-caller channel");
        assert_eq!(
            replies.subscriber_origin_binding,
            Some(OriginBinding::OriginHashHex16),
            "H3: the reply prefix must be origin-bound. Unbound, any mesh peer \
             can hold a live subscription to another caller's reply channel and \
             receive that caller's response bodies whenever the server's direct \
             route misses and the response falls back to roster fan-out."
        );
    }

    /// A service name that cannot form a valid channel name installs
    /// NOTHING — not a half-configured pair where the requests channel
    /// exists and the reply prefix does not.
    ///
    /// The LENGTH cases are the ones that matter and the ones an
    /// invalid-character name does not reach. There are TWO of them,
    /// because the three names involved are three different lengths —
    /// `.requests` is 9 bytes, the `.replies.prefix` sentinel is 15, and
    /// a real `.replies.<16 hex>` is 25:
    ///
    /// - request fits, sentinel does not;
    /// - both fit, but no real per-caller reply channel does.
    ///
    /// The second is the one a sentinel-based check misses, and it fails
    /// worse than a visible refusal: everything looks installed, and
    /// then every call hangs until it times out because no caller can
    /// name a reply channel that validates.
    ///
    /// A character-invalid name fails all three and would pass this test
    /// against an implementation that got either band wrong, which is
    /// why the bands are enumerated explicitly with preconditions.
    #[test]
    fn rpc_service_defaults_install_nothing_for_an_unrepresentable_name() {
        use super::super::name::MAX_NAME_LEN;

        // Band 1: request fits, sentinel does not.
        let no_sentinel = "s".repeat(MAX_NAME_LEN - ".requests".len());
        assert!(ChannelName::new(&format!("{no_sentinel}.requests")).is_ok());
        assert!(
            ChannelName::new(&format!("{no_sentinel}.replies.prefix")).is_err(),
            "precondition: band 1 must have an unrepresentable sentinel"
        );

        // Band 2: request AND sentinel fit; a real reply channel does not.
        let no_real_reply = "s".repeat(MAX_NAME_LEN - ".replies.prefix".len());
        assert!(ChannelName::new(&format!("{no_real_reply}.requests")).is_ok());
        assert!(
            ChannelName::new(&format!("{no_real_reply}.replies.prefix")).is_ok(),
            "precondition: band 2's sentinel must VALIDATE — that is what \
             makes checking the sentinel insufficient"
        );
        assert!(
            ChannelName::new(&format!("{no_real_reply}.replies.{:016x}", 0u64)).is_err(),
            "precondition: …while no real per-caller reply channel fits"
        );

        for (band, service) in [
            ("no sentinel", no_sentinel.as_str()),
            ("no real reply channel", no_real_reply.as_str()),
            ("invalid characters", "bad name#with/invalid chars"),
        ] {
            let reg = ChannelConfigRegistry::new();
            reg.install_rpc_service_defaults(service);
            assert_eq!(
                reg.len(),
                0,
                "[{band}] installed a request channel the reply side cannot \
                 match (service len {})",
                service.len()
            );
            assert_eq!(
                reg.snapshot_prefixes().len(),
                0,
                "[{band}] installed a reply prefix no caller can ever use \
                 (service len {})",
                service.len()
            );
        }
    }

    /// The largest service name that IS fully usable must still install.
    ///
    /// Paired with the refusal test above so the boundary is pinned from
    /// both sides — a validator that is too strict silently stops
    /// configuring services that work, which no test asserting "installs
    /// nothing" would ever catch.
    #[test]
    fn rpc_service_defaults_install_at_the_longest_usable_service_name() {
        use super::super::name::MAX_NAME_LEN;

        let longest = "s".repeat(MAX_NAME_LEN - ".replies.0123456789abcdef".len());
        let reg = ChannelConfigRegistry::new();
        reg.install_rpc_service_defaults(&longest);

        assert!(
            reg.get_by_name(&format!("{longest}.requests")).is_some(),
            "the longest fully-usable service name must still get its request \
             channel"
        );
        let reply = format!("{longest}.replies.{:016x}", u64::MAX);
        assert_eq!(
            ChannelName::new(&reply).map(|_| ()),
            Ok(()),
            "precondition: this is the longest name where a real reply \
             channel still fits"
        );
        assert_eq!(
            reg.get_by_name(&reply)
                .expect("the reply prefix must resolve it")
                .subscriber_origin_binding,
            Some(OriginBinding::OriginHashHex16)
        );
    }

    // ---- H2 (2026-07-31 audit): install-if-absent must not clobber ----

    /// `insert_if_absent` installs into an empty slot and reports it.
    #[test]
    fn insert_if_absent_installs_when_vacant() {
        let reg = ChannelConfigRegistry::new();
        let id = ChannelId::parse("svc.requests").unwrap();
        assert!(reg.insert_if_absent(ChannelConfig::new(id.clone()).with_priority(4)));
        assert_eq!(reg.get_by_name("svc.requests").unwrap().priority, 4);
        assert_eq!(reg.get(id.hash()).unwrap().priority, 4);
    }

    /// The H2 regression at the registry level: an operator's strict
    /// config must survive a later auto-registered permissive default.
    /// Pre-fix `insert` replaced unconditionally and the ACL vanished.
    #[test]
    fn insert_if_absent_preserves_existing_strict_config() {
        let reg = ChannelConfigRegistry::new();
        let id = ChannelId::parse("svc.requests").unwrap();
        let root = EntityKeypair::generate();
        reg.insert(ChannelConfig::new(id.clone()).with_token_roots(vec![root.entity_id().clone()]));

        // Auto-registration's permissive default arrives second.
        let installed = reg.insert_if_absent(ChannelConfig::new(id.clone()));

        assert!(!installed, "must report that it did not install");
        let cfg = reg.get_by_name("svc.requests").unwrap();
        assert!(
            cfg.token_required(),
            "operator's token gate must survive auto-registration"
        );
        assert_eq!(cfg.token_roots.len(), 1);
    }

    /// Same guarantee on the prefix table, which is where nRPC's
    /// reply-channel ACL would live.
    #[test]
    fn insert_prefix_if_absent_preserves_existing_strict_prefix() {
        let reg = ChannelConfigRegistry::new();
        let sentinel = ChannelId::parse("svc.replies.prefix").unwrap();
        let root = EntityKeypair::generate();
        reg.insert_prefix(
            "svc.replies.",
            ChannelConfig::new(sentinel.clone()).with_token_roots(vec![root.entity_id().clone()]),
        );

        let installed = reg.insert_prefix_if_absent("svc.replies.", ChannelConfig::new(sentinel));

        assert!(!installed);
        let cfg = reg.get_by_name("svc.replies.abcdef0123456789").unwrap();
        assert!(
            cfg.token_required(),
            "operator's reply-prefix gate must survive auto-registration"
        );
    }

    #[test]
    fn insert_prefix_if_absent_installs_when_vacant() {
        let reg = ChannelConfigRegistry::new();
        let sentinel = ChannelId::parse("svc.replies.prefix").unwrap();
        assert!(reg.insert_prefix_if_absent(
            "svc.replies.",
            ChannelConfig::new(sentinel).with_priority(2)
        ));
        assert_eq!(reg.get_by_name("svc.replies.deadbeef").unwrap().priority, 2);
    }

    /// Re-registering the same channel must not corrupt the canonical-
    /// hash index. Pre-fix `insert` pushed the name unconditionally, so
    /// the second registration grew the bucket to `[name, name]`,
    /// `get()` read that as a hash collision, and canonical-hash lookup
    /// started returning `None` for a channel that plainly exists.
    #[test]
    fn repeated_insert_does_not_self_collide_the_hash_index() {
        let reg = ChannelConfigRegistry::new();
        let id = ChannelId::parse("svc.requests").unwrap();
        let hash = id.hash();

        reg.insert(ChannelConfig::new(id.clone()).with_priority(1));
        reg.insert(ChannelConfig::new(id.clone()).with_priority(2));
        reg.insert(ChannelConfig::new(id).with_priority(3));

        assert_eq!(reg.len(), 1, "one channel, not three");
        let cfg = reg
            .get(hash)
            .expect("canonical-hash lookup must survive re-registration");
        assert_eq!(cfg.priority, 3, "latest config wins");
    }

    /// The mixed path auto-registration actually takes: a replacing
    /// `insert` followed by install-if-absent attempts.
    #[test]
    fn insert_then_if_absent_leaves_hash_index_unambiguous() {
        let reg = ChannelConfigRegistry::new();
        let id = ChannelId::parse("svc.requests").unwrap();
        let hash = id.hash();

        reg.insert(ChannelConfig::new(id.clone()).with_priority(9));
        assert!(!reg.insert_if_absent(ChannelConfig::new(id.clone())));
        assert!(!reg.insert_if_absent(ChannelConfig::new(id)));

        assert_eq!(
            reg.get(hash).expect("must stay resolvable").priority,
            9,
            "the operator's config must remain, and the index unambiguous"
        );
    }

    /// Exactly one of N concurrent `insert_if_absent` callers may win,
    /// and the reverse index must not end up ambiguous afterwards.
    #[test]
    fn concurrent_insert_if_absent_elects_exactly_one_winner() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc as StdArc;

        let reg = StdArc::new(ChannelConfigRegistry::new());
        let wins = StdArc::new(AtomicUsize::new(0));
        let id = ChannelId::parse("svc.requests").unwrap();

        std::thread::scope(|s| {
            for i in 0..8 {
                let reg = reg.clone();
                let wins = wins.clone();
                let id = id.clone();
                s.spawn(move || {
                    if reg.insert_if_absent(ChannelConfig::new(id).with_priority(i)) {
                        wins.fetch_add(1, Ordering::Relaxed);
                    }
                });
            }
        });

        assert_eq!(wins.load(Ordering::Relaxed), 1, "exactly one installer");
        assert_eq!(reg.len(), 1);
        assert!(
            reg.get(id.hash()).is_some(),
            "hash index must stay unambiguous under concurrent installs"
        );
    }

    /// The reverse indices must agree with `configs` after arbitrary
    /// concurrent registration and removal — no name indexed that
    /// `configs` does not hold, and none held that is not indexed.
    ///
    /// Both directions matter and they used to fail in turn:
    ///
    /// - Un-indexed-but-present: a re-registration landing between
    ///   `remove`'s `configs.remove` and its `retain` had its fresh
    ///   index entry deleted. The channel is registered and `get_by_name`
    ///   finds it, but `get(hash)` — the path publish and subscribe
    ///   authorization take — answers `None`, so its ACL stops being
    ///   enforced.
    /// - Indexed-but-absent: the repair pass for the above re-added a
    ///   name a second concurrent removal had just taken out. `get` and
    ///   `remove` read a bucket of more than one name as a hash
    ///   collision and refuse it, so a phantom name disables lookup for
    ///   whatever real channel shares the bucket.
    ///
    /// Interleaving-dependent, so a green run is evidence rather than
    /// proof. It fails reliably against the unsynchronized version
    /// (typically within a few hundred iterations), which is what makes
    /// it worth keeping: the assertion states the invariant exactly, and
    /// a future change that drops the serialization has a real chance of
    /// being caught here.
    #[test]
    fn concurrent_registration_and_removal_keep_the_indices_consistent() {
        use std::sync::Arc as StdArc;

        // Distinct names, so threads contend on the registry rather
        // than on one key — the churn that produced both defects.
        let names: Vec<String> = (0..4).map(|i| format!("svc.chan{i}")).collect();

        for _round in 0..200 {
            let reg = StdArc::new(ChannelConfigRegistry::new());
            std::thread::scope(|s| {
                for name in &names {
                    for _ in 0..2 {
                        let reg = reg.clone();
                        let id = ChannelId::parse(name).unwrap();
                        s.spawn(move || {
                            reg.insert(ChannelConfig::new(id.clone()));
                            reg.remove_by_name(id.name().as_str());
                            reg.insert(ChannelConfig::new(id));
                        });
                    }
                }
                for name in &names {
                    let reg = reg.clone();
                    let name = name.clone();
                    s.spawn(move || {
                        reg.remove_by_name(&name);
                    });
                }
            });

            for name in &names {
                let present = reg.get_by_name(name).is_some();
                // Straight at `by_hash`: the invariant is about the
                // index itself, and the collision-safe public accessors
                // hide exactly the corruption being asserted on.
                let hash = ChannelId::parse(name).unwrap().hash();
                let indexed = reg
                    .by_hash
                    .get(&hash)
                    .is_some_and(|names| names.iter().any(|n| n == name));
                assert_eq!(
                    present, indexed,
                    "index and `configs` disagree about {name:?}: present={present}, \
                     indexed={indexed}. Registered-but-unindexed silently stops \
                     enforcing that channel's ACL on the `get(hash)` path; \
                     indexed-but-absent poisons the bucket for every channel \
                     sharing it."
                );
            }
        }
    }
}
