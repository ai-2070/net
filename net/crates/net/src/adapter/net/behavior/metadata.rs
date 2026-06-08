//! Phase 4C: Node Metadata Surface (NODE-META)
//!
//! This module provides structured node metadata with location awareness,
//! topology hints, and fast indexing for node discovery and routing.

use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Unique node identifier (32 bytes)
pub type NodeId = [u8; 32];

/// Geographic region identifier
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Region {
    /// North America
    NorthAmerica(String),
    /// South America
    SouthAmerica(String),
    /// Europe
    Europe(String),
    /// Asia Pacific
    AsiaPacific(String),
    /// Middle East
    MiddleEast(String),
    /// Africa
    Africa(String),
    /// Custom region
    Custom(String),
}

impl Region {
    /// Get the continent name
    pub fn continent(&self) -> &'static str {
        match self {
            Region::NorthAmerica(_) => "north_america",
            Region::SouthAmerica(_) => "south_america",
            Region::Europe(_) => "europe",
            Region::AsiaPacific(_) => "asia_pacific",
            Region::MiddleEast(_) => "middle_east",
            Region::Africa(_) => "africa",
            Region::Custom(_) => "custom",
        }
    }

    /// Get the zone within the region
    pub fn zone(&self) -> &str {
        match self {
            Region::NorthAmerica(z)
            | Region::SouthAmerica(z)
            | Region::Europe(z)
            | Region::AsiaPacific(z)
            | Region::MiddleEast(z)
            | Region::Africa(z)
            | Region::Custom(z) => z,
        }
    }
}

/// Geographic location information
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LocationInfo {
    /// Geographic region
    pub region: Region,
    /// Availability zone within region
    pub zone: Option<String>,
    /// Latitude (-90 to 90)
    pub latitude: Option<f64>,
    /// Longitude (-180 to 180)
    pub longitude: Option<f64>,
    /// Autonomous System Number
    pub asn: Option<u32>,
    /// ISP or cloud provider name
    pub provider: Option<String>,
    /// Data center identifier
    pub datacenter: Option<String>,
    /// Country code (ISO 3166-1 alpha-2)
    pub country_code: Option<String>,
    /// City name
    pub city: Option<String>,
}

impl LocationInfo {
    /// Create a new location with just region
    pub fn new(region: Region) -> Self {
        Self {
            region,
            zone: None,
            latitude: None,
            longitude: None,
            asn: None,
            provider: None,
            datacenter: None,
            country_code: None,
            city: None,
        }
    }

    /// Set coordinates
    pub fn with_coordinates(mut self, lat: f64, lon: f64) -> Self {
        self.latitude = Some(lat.clamp(-90.0, 90.0));
        self.longitude = Some(lon.clamp(-180.0, 180.0));
        self
    }

    /// Set ASN
    pub fn with_asn(mut self, asn: u32) -> Self {
        self.asn = Some(asn);
        self
    }

    /// Set provider
    pub fn with_provider(mut self, provider: impl Into<String>) -> Self {
        self.provider = Some(provider.into());
        self
    }

    /// Calculate approximate distance to another location in kilometers
    /// Uses Haversine formula
    pub fn distance_to(&self, other: &LocationInfo) -> Option<f64> {
        let (lat1, lon1) = (self.latitude?, self.longitude?);
        let (lat2, lon2) = (other.latitude?, other.longitude?);

        let r = 6371.0; // Earth's radius in km
        let d_lat = (lat2 - lat1).to_radians();
        let d_lon = (lon2 - lon1).to_radians();
        let lat1_rad = lat1.to_radians();
        let lat2_rad = lat2.to_radians();

        let a = (d_lat / 2.0).sin().powi(2)
            + lat1_rad.cos() * lat2_rad.cos() * (d_lon / 2.0).sin().powi(2);
        let c = 2.0 * a.sqrt().asin();

        Some(r * c)
    }

    /// Check if same continent
    pub fn same_continent(&self, other: &LocationInfo) -> bool {
        self.region.continent() == other.region.continent()
    }

    /// Check if same region
    pub fn same_region(&self, other: &LocationInfo) -> bool {
        self.region == other.region
    }
}

/// NAT type for connectivity hints
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum NatType {
    /// No NAT - direct connectivity
    None,
    /// Full cone NAT
    FullCone,
    /// Restricted cone NAT
    RestrictedCone,
    /// Port-restricted cone NAT
    PortRestrictedCone,
    /// Symmetric NAT (hardest to traverse)
    Symmetric,
    /// Unknown NAT type
    Unknown,
}

impl NatType {
    /// NAT traversal difficulty (0 = easiest, 4 = hardest)
    pub fn difficulty(&self) -> u8 {
        match self {
            NatType::None => 0,
            NatType::FullCone => 1,
            NatType::RestrictedCone => 2,
            NatType::PortRestrictedCone => 3,
            NatType::Symmetric => 4,
            NatType::Unknown => 3,
        }
    }

    /// Can establish direct connection with another NAT type
    pub fn can_connect_direct(&self, other: &NatType) -> bool {
        // At least one side should be easy to traverse
        self.difficulty() + other.difficulty() < 5
    }
}

/// Network tier classification
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
pub enum NetworkTier {
    /// Edge device (mobile, IoT)
    Edge = 0,
    /// Consumer connection
    Consumer = 1,
    /// Business/prosumer connection
    Business = 2,
    /// Data center with standard connectivity
    Datacenter = 3,
    /// Premium data center with high bandwidth
    Premium = 4,
    /// Core infrastructure node
    Core = 5,
}

impl NetworkTier {
    /// Get relative priority for routing (higher = prefer)
    pub fn priority(&self) -> u8 {
        *self as u8
    }
}

/// Topology hints for routing optimization
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TopologyHints {
    /// Preferred peer node IDs for routing
    pub preferred_peers: Vec<NodeId>,
    /// Network tier classification
    pub tier: NetworkTier,
    /// Uplink bandwidth in Gbps
    pub uplink_gbps: Option<u32>,
    /// Downlink bandwidth in Gbps
    pub downlink_gbps: Option<u32>,
    /// NAT type
    pub nat_type: NatType,
    /// Whether node can relay traffic
    pub can_relay: bool,
    /// Maximum relay connections
    pub max_relay_connections: Option<u32>,
    /// Known public addresses
    pub public_addresses: Vec<IpAddr>,
    /// STUN-discovered reflexive address
    pub reflexive_address: Option<IpAddr>,
    /// Average latency to known peers (node_id -> latency_ms)
    #[serde(skip)]
    pub peer_latencies: HashMap<NodeId, u32>,
    /// Hop count to well-known nodes
    pub hop_distances: HashMap<String, u8>,
}

impl Default for TopologyHints {
    fn default() -> Self {
        Self {
            preferred_peers: Vec::new(),
            tier: NetworkTier::Consumer,
            uplink_gbps: None,
            downlink_gbps: None,
            nat_type: NatType::Unknown,
            can_relay: false,
            max_relay_connections: None,
            public_addresses: Vec::new(),
            reflexive_address: None,
            peer_latencies: HashMap::new(),
            hop_distances: HashMap::new(),
        }
    }
}

impl TopologyHints {
    /// Create new topology hints
    pub fn new(tier: NetworkTier) -> Self {
        Self {
            tier,
            ..Default::default()
        }
    }

    /// Set bandwidth
    pub fn with_bandwidth(mut self, uplink: u32, downlink: u32) -> Self {
        self.uplink_gbps = Some(uplink);
        self.downlink_gbps = Some(downlink);
        self
    }

    /// Set NAT type
    pub fn with_nat(mut self, nat_type: NatType) -> Self {
        self.nat_type = nat_type;
        self
    }

    /// Enable relay capability
    pub fn with_relay(mut self, max_connections: u32) -> Self {
        self.can_relay = true;
        self.max_relay_connections = Some(max_connections);
        self
    }

    /// Add a preferred peer
    pub fn add_preferred_peer(&mut self, peer: NodeId) {
        if !self.preferred_peers.contains(&peer) {
            self.preferred_peers.push(peer);
        }
    }

    /// Update peer latency
    pub fn update_latency(&mut self, peer: NodeId, latency_ms: u32) {
        self.peer_latencies.insert(peer, latency_ms);
    }

    /// Get average latency to all known peers
    pub fn average_latency(&self) -> Option<f64> {
        if self.peer_latencies.is_empty() {
            return None;
        }
        let sum: u64 = self.peer_latencies.values().map(|&v| v as u64).sum();
        Some(sum as f64 / self.peer_latencies.len() as f64)
    }

    /// Estimate connectivity quality (0.0 - 1.0)
    pub fn connectivity_score(&self) -> f64 {
        let mut score = 0.0;

        // Tier bonus (0-0.3)
        score += (self.tier.priority() as f64) * 0.06;

        // NAT type (0-0.2)
        score += (4 - self.nat_type.difficulty()) as f64 * 0.05;

        // Has public address (0.1)
        if !self.public_addresses.is_empty() {
            score += 0.1;
        }

        // Bandwidth (0-0.2)
        if let Some(uplink) = self.uplink_gbps {
            score += (uplink.min(1000) as f64 / 1000.0) * 0.2;
        }

        // Can relay (0.1)
        if self.can_relay {
            score += 0.1;
        }

        score.min(1.0)
    }
}

/// Node operational status
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum NodeStatus {
    /// Fully operational
    Online,
    /// Operational but with reduced capacity
    Degraded,
    /// Draining connections, no new work
    Draining,
    /// Under maintenance, may return
    Maintenance,
    /// Not responding
    Offline,
    /// Starting up
    Starting,
    /// Gracefully shutting down
    ShuttingDown,
}

impl NodeStatus {
    /// Whether node can accept new work
    pub fn accepts_work(&self) -> bool {
        matches!(self, NodeStatus::Online | NodeStatus::Degraded)
    }

    /// Whether node is reachable
    pub fn is_reachable(&self) -> bool {
        !matches!(self, NodeStatus::Offline)
    }

    /// Priority for routing (higher = prefer)
    pub fn routing_priority(&self) -> u8 {
        match self {
            NodeStatus::Online => 5,
            NodeStatus::Degraded => 3,
            NodeStatus::Draining => 1,
            NodeStatus::ShuttingDown => 0,
            NodeStatus::Starting => 2,
            NodeStatus::Maintenance => 0,
            NodeStatus::Offline => 0,
        }
    }
}

/// Maximum allowed length of any single string field in
/// `NodeMetadata` (`name`, `description`, `owner`, individual tag /
/// role / `custom` keys and values). 1 KiB is far past any
/// realistic operator-supplied label and bounds the per-string
/// memory cost.
pub const MAX_METADATA_STRING_LEN: usize = 1024;

/// Maximum number of tags or roles in `NodeMetadata`. 256 is far
/// past real usage (typical deployments use a handful of
/// scope/tier/region tags); without this cap a peer could ship
/// millions and turn one announcement into millions of `by_tag`
/// index entries.
pub const MAX_METADATA_TAGS: usize = 256;

/// Maximum number of entries in the `custom` key-value map. Same
/// pattern as tags — bounds the per-announcement footprint.
pub const MAX_METADATA_CUSTOM_ENTRIES: usize = 256;

/// Maximum number of `preferred_peers` in `TopologyHints`. Each
/// entry is a 32-byte `NodeId`; a 4096-cap bounds the wire/heap
/// cost while staying well above any realistic peering preference
/// list.
pub const MAX_PREFERRED_PEERS: usize = 4096;

/// Maximum number of `hop_distances` entries in `TopologyHints`.
pub const MAX_HOP_DISTANCES: usize = 4096;

/// Maximum number of `public_addresses` (multi-homed nodes
/// typically advertise <16; 256 is generous).
pub const MAX_PUBLIC_ADDRESSES: usize = 256;

/// Complete node metadata
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeMetadata {
    /// Unique node identifier
    pub node_id: NodeId,
    /// Human-readable node name
    pub name: Option<String>,
    /// Node description
    pub description: Option<String>,
    /// Owner identifier (org, user, etc.)
    pub owner: Option<String>,
    /// Geographic location
    pub location: Option<LocationInfo>,
    /// Topology hints
    pub topology: TopologyHints,
    /// Current status
    pub status: NodeStatus,
    /// Custom key-value metadata
    pub custom: HashMap<String, String>,
    /// Metadata version (monotonic)
    pub version: u64,
    /// Last update timestamp (Unix millis)
    pub updated_at: u64,
    /// Creation timestamp (Unix millis)
    pub created_at: u64,
    /// Tags for categorization
    pub tags: HashSet<String>,
    /// Node roles
    pub roles: HashSet<String>,
}

impl NodeMetadata {
    /// Validate that the metadata fits within the per-field
    /// boundedness caps (string lengths, tag/role/custom counts,
    /// preferred-peers / hop-distances / public-addresses sizes).
    ///
    /// Without these caps the deserialize path is unbounded —
    /// every `Vec` / `HashMap` / `String` would accept whatever
    /// the peer shipped, and `MetadataStore::upsert` would
    /// happily index millions of attacker-supplied tags into the
    /// per-tag inverted-index DashMap. `upsert` (and
    /// `update_versioned`) call `validate_bounds` before touching
    /// the indexes; oversized metadata surfaces as
    /// `MetadataError::Invalid(...)`.
    pub fn validate_bounds(&self) -> Result<(), MetadataError> {
        if let Some(name) = &self.name {
            if name.len() > MAX_METADATA_STRING_LEN {
                return Err(MetadataError::Invalid(format!(
                    "name exceeds {} bytes",
                    MAX_METADATA_STRING_LEN
                )));
            }
        }
        if let Some(d) = &self.description {
            if d.len() > MAX_METADATA_STRING_LEN {
                return Err(MetadataError::Invalid(format!(
                    "description exceeds {} bytes",
                    MAX_METADATA_STRING_LEN
                )));
            }
        }
        if let Some(o) = &self.owner {
            if o.len() > MAX_METADATA_STRING_LEN {
                return Err(MetadataError::Invalid(format!(
                    "owner exceeds {} bytes",
                    MAX_METADATA_STRING_LEN
                )));
            }
        }
        if self.tags.len() > MAX_METADATA_TAGS {
            return Err(MetadataError::Invalid(format!(
                "tags exceed {} entries",
                MAX_METADATA_TAGS
            )));
        }
        for tag in &self.tags {
            if tag.len() > MAX_METADATA_STRING_LEN {
                return Err(MetadataError::Invalid(format!(
                    "tag exceeds {} bytes",
                    MAX_METADATA_STRING_LEN
                )));
            }
        }
        if self.roles.len() > MAX_METADATA_TAGS {
            return Err(MetadataError::Invalid(format!(
                "roles exceed {} entries",
                MAX_METADATA_TAGS
            )));
        }
        for role in &self.roles {
            if role.len() > MAX_METADATA_STRING_LEN {
                return Err(MetadataError::Invalid(format!(
                    "role exceeds {} bytes",
                    MAX_METADATA_STRING_LEN
                )));
            }
        }
        if self.custom.len() > MAX_METADATA_CUSTOM_ENTRIES {
            return Err(MetadataError::Invalid(format!(
                "custom map exceeds {} entries",
                MAX_METADATA_CUSTOM_ENTRIES
            )));
        }
        for (k, v) in &self.custom {
            if k.len() > MAX_METADATA_STRING_LEN {
                return Err(MetadataError::Invalid(format!(
                    "custom key exceeds {} bytes",
                    MAX_METADATA_STRING_LEN
                )));
            }
            if v.len() > MAX_METADATA_STRING_LEN {
                return Err(MetadataError::Invalid(format!(
                    "custom value exceeds {} bytes",
                    MAX_METADATA_STRING_LEN
                )));
            }
        }
        if self.topology.preferred_peers.len() > MAX_PREFERRED_PEERS {
            return Err(MetadataError::Invalid(format!(
                "preferred_peers exceed {} entries",
                MAX_PREFERRED_PEERS
            )));
        }
        if self.topology.hop_distances.len() > MAX_HOP_DISTANCES {
            return Err(MetadataError::Invalid(format!(
                "hop_distances exceed {} entries",
                MAX_HOP_DISTANCES
            )));
        }
        // hop_distances keys are unbounded `String`s. Without a
        // per-key length check a peer could ship a single
        // multi-megabyte key inside a perfectly-counted map and
        // smuggle the bound past validate_bounds.
        for k in self.topology.hop_distances.keys() {
            if k.len() > MAX_METADATA_STRING_LEN {
                return Err(MetadataError::Invalid(format!(
                    "hop_distances key exceeds {} bytes",
                    MAX_METADATA_STRING_LEN
                )));
            }
        }
        if self.topology.public_addresses.len() > MAX_PUBLIC_ADDRESSES {
            return Err(MetadataError::Invalid(format!(
                "public_addresses exceed {} entries",
                MAX_PUBLIC_ADDRESSES
            )));
        }
        // Nested LocationInfo strings (`zone`, `provider`,
        // `datacenter`, `country_code`, `city`) MUST be length-
        // checked. The top-level fields and collection counts
        // alone wouldn't catch oversized strings pinned inside
        // `location`.
        if let Some(loc) = &self.location {
            for (label, field) in [
                ("location.zone", &loc.zone),
                ("location.provider", &loc.provider),
                ("location.datacenter", &loc.datacenter),
                ("location.country_code", &loc.country_code),
                ("location.city", &loc.city),
            ] {
                if let Some(v) = field {
                    if v.len() > MAX_METADATA_STRING_LEN {
                        return Err(MetadataError::Invalid(format!(
                            "{label} exceeds {MAX_METADATA_STRING_LEN} bytes",
                        )));
                    }
                }
            }
        }
        Ok(())
    }

    /// Create new node metadata
    pub fn new(node_id: NodeId) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        Self {
            node_id,
            name: None,
            description: None,
            owner: None,
            location: None,
            topology: TopologyHints::default(),
            status: NodeStatus::Starting,
            custom: HashMap::new(),
            version: 1,
            updated_at: now,
            created_at: now,
            tags: HashSet::new(),
            roles: HashSet::new(),
        }
    }

    /// Set name
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Set owner
    pub fn with_owner(mut self, owner: impl Into<String>) -> Self {
        self.owner = Some(owner.into());
        self
    }

    /// Set location
    pub fn with_location(mut self, location: LocationInfo) -> Self {
        self.location = Some(location);
        self
    }

    /// Set topology
    pub fn with_topology(mut self, topology: TopologyHints) -> Self {
        self.topology = topology;
        self
    }

    /// Set status
    pub fn with_status(mut self, status: NodeStatus) -> Self {
        self.status = status;
        self
    }

    /// Add tag
    pub fn with_tag(mut self, tag: impl Into<String>) -> Self {
        self.tags.insert(tag.into());
        self
    }

    /// Add role
    pub fn with_role(mut self, role: impl Into<String>) -> Self {
        self.roles.insert(role.into());
        self
    }

    /// Add custom metadata
    pub fn with_custom(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.custom.insert(key.into(), value.into());
        self
    }

    /// Update and increment version
    pub fn touch(&mut self) {
        self.version += 1;
        self.updated_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
    }

    /// Get age since last update
    pub fn age(&self) -> Duration {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        Duration::from_millis(now.saturating_sub(self.updated_at))
    }

    /// Check if metadata is stale
    pub fn is_stale(&self, max_age: Duration) -> bool {
        self.age() > max_age
    }

    /// Calculate routing score for this node
    pub fn routing_score(&self) -> f64 {
        let mut score = 0.0;

        // Status priority (0-0.5)
        score += (self.status.routing_priority() as f64) * 0.1;

        // Topology score (0-0.3)
        score += self.topology.connectivity_score() * 0.3;

        // Tier bonus (0-0.2)
        score += (self.topology.tier.priority() as f64) * 0.04;

        score.min(1.0)
    }
}

/// Query filter for metadata store
#[derive(Debug, Clone, Default)]
pub struct MetadataQuery {
    /// Filter by status
    pub status: Option<NodeStatus>,
    /// Filter by statuses (any match)
    pub statuses: Option<Vec<NodeStatus>>,
    /// Filter by region continent
    pub continent: Option<String>,
    /// Filter by region
    pub region: Option<Region>,
    /// Filter by minimum tier
    pub min_tier: Option<NetworkTier>,
    /// Filter by tag (all must match)
    pub tags: Option<Vec<String>>,
    /// Filter by role (all must match)
    pub roles: Option<Vec<String>>,
    /// Filter by owner
    pub owner: Option<String>,
    /// Maximum age since last update
    pub max_age: Option<Duration>,
    /// Must accept new work
    pub accepts_work: Option<bool>,
    /// Must be able to relay
    pub can_relay: Option<bool>,
    /// Maximum results
    pub limit: Option<usize>,
}

impl MetadataQuery {
    /// Create empty query
    pub fn new() -> Self {
        Self::default()
    }

    /// Filter by status
    pub fn with_status(mut self, status: NodeStatus) -> Self {
        self.status = Some(status);
        self
    }

    /// Filter by multiple statuses
    pub fn with_statuses(mut self, statuses: Vec<NodeStatus>) -> Self {
        self.statuses = Some(statuses);
        self
    }

    /// Filter by continent
    pub fn with_continent(mut self, continent: impl Into<String>) -> Self {
        self.continent = Some(continent.into());
        self
    }

    /// Filter by region
    pub fn with_region(mut self, region: Region) -> Self {
        self.region = Some(region);
        self
    }

    /// Filter by minimum tier
    pub fn with_min_tier(mut self, tier: NetworkTier) -> Self {
        self.min_tier = Some(tier);
        self
    }

    /// Filter by tags
    pub fn with_tags(mut self, tags: Vec<String>) -> Self {
        self.tags = Some(tags);
        self
    }

    /// Filter by roles
    pub fn with_roles(mut self, roles: Vec<String>) -> Self {
        self.roles = Some(roles);
        self
    }

    /// Filter by owner
    pub fn with_owner(mut self, owner: impl Into<String>) -> Self {
        self.owner = Some(owner.into());
        self
    }

    /// Filter by max age
    pub fn with_max_age(mut self, max_age: Duration) -> Self {
        self.max_age = Some(max_age);
        self
    }

    /// Filter nodes that accept work
    pub fn accepting_work(mut self) -> Self {
        self.accepts_work = Some(true);
        self
    }

    /// Filter nodes that can relay
    pub fn can_relay(mut self) -> Self {
        self.can_relay = Some(true);
        self
    }

    /// Limit results
    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = Some(limit);
        self
    }

    /// Check if metadata matches query
    pub fn matches(&self, meta: &NodeMetadata) -> bool {
        // Status filter
        if let Some(status) = self.status {
            if meta.status != status {
                return false;
            }
        }

        // Multiple statuses
        if let Some(ref statuses) = self.statuses {
            if !statuses.contains(&meta.status) {
                return false;
            }
        }

        // Continent filter
        if let Some(ref continent) = self.continent {
            if let Some(ref loc) = meta.location {
                if loc.region.continent() != continent {
                    return false;
                }
            } else {
                return false;
            }
        }

        // Region filter
        if let Some(ref region) = self.region {
            if let Some(ref loc) = meta.location {
                if &loc.region != region {
                    return false;
                }
            } else {
                return false;
            }
        }

        // Tier filter
        if let Some(min_tier) = self.min_tier {
            if meta.topology.tier < min_tier {
                return false;
            }
        }

        // Tags filter (all must match)
        if let Some(ref tags) = self.tags {
            for tag in tags {
                if !meta.tags.contains(tag) {
                    return false;
                }
            }
        }

        // Roles filter (all must match)
        if let Some(ref roles) = self.roles {
            for role in roles {
                if !meta.roles.contains(role) {
                    return false;
                }
            }
        }

        // Owner filter
        if let Some(ref owner) = self.owner {
            if meta.owner.as_ref() != Some(owner) {
                return false;
            }
        }

        // Age filter
        if let Some(max_age) = self.max_age {
            if meta.is_stale(max_age) {
                return false;
            }
        }

        // Accepts work filter
        if let Some(accepts) = self.accepts_work {
            if meta.status.accepts_work() != accepts {
                return false;
            }
        }

        // Can relay filter
        if let Some(can_relay) = self.can_relay {
            if meta.topology.can_relay != can_relay {
                return false;
            }
        }

        true
    }
}

/// Metadata store errors
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetadataError {
    /// Node not found
    NotFound(NodeId),
    /// Version conflict
    VersionConflict {
        /// Expected metadata version
        expected: u64,
        /// Actual metadata version
        actual: u64,
    },
    /// Invalid metadata
    Invalid(String),
    /// Store capacity exceeded
    CapacityExceeded,
}

impl std::fmt::Display for MetadataError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MetadataError::NotFound(_) => write!(f, "Node not found"),
            MetadataError::VersionConflict { expected, actual } => {
                write!(f, "Version conflict: expected {}, got {}", expected, actual)
            }
            MetadataError::Invalid(msg) => write!(f, "Invalid metadata: {}", msg),
            MetadataError::CapacityExceeded => write!(f, "Store capacity exceeded"),
        }
    }
}

impl std::error::Error for MetadataError {}

/// Statistics for MetadataStore
#[derive(Debug, Clone, Default)]
pub struct MetadataStoreStats {
    /// Total nodes stored
    pub total_nodes: usize,
    /// Nodes by status
    pub by_status: HashMap<NodeStatus, usize>,
    /// Nodes by tier
    pub by_tier: HashMap<NetworkTier, usize>,
    /// Nodes by continent
    pub by_continent: HashMap<String, usize>,
    /// Total queries performed
    pub queries: u64,
    /// Total updates performed
    pub updates: u64,
}

/// High-performance metadata store with indexes
pub struct MetadataStore {
    /// Primary storage
    nodes: DashMap<NodeId, Arc<NodeMetadata>>,
    /// Index by status
    by_status: DashMap<NodeStatus, HashSet<NodeId>>,
    /// Index by tier
    by_tier: DashMap<NetworkTier, HashSet<NodeId>>,
    /// Index by continent
    by_continent: DashMap<String, HashSet<NodeId>>,
    /// Index by tag
    by_tag: DashMap<String, HashSet<NodeId>>,
    /// Index by role
    by_role: DashMap<String, HashSet<NodeId>>,
    /// Index by owner
    by_owner: DashMap<String, HashSet<NodeId>>,
    /// Query counter
    query_count: AtomicU64,
    /// Update counter
    update_count: AtomicU64,
    /// O(1) live node count. `DashMap::len()` walks every shard (~1us); the
    /// capacity gate in `upsert` and `len()`/`stats()` read this instead.
    /// Maintained exactly on the Vacant-insert / remove paths. See
    /// docs/misc/PERF_AUDIT_2026_06_08_BENCHMARK_WINS.md §2/§4.
    node_count: AtomicUsize,
    /// Maximum capacity
    max_capacity: Option<usize>,
}

impl MetadataStore {
    /// Create new metadata store
    pub fn new() -> Self {
        Self {
            nodes: DashMap::new(),
            by_status: DashMap::new(),
            by_tier: DashMap::new(),
            by_continent: DashMap::new(),
            by_tag: DashMap::new(),
            by_role: DashMap::new(),
            by_owner: DashMap::new(),
            query_count: AtomicU64::new(0),
            update_count: AtomicU64::new(0),
            node_count: AtomicUsize::new(0),
            max_capacity: None,
        }
    }

    /// Create store with capacity limit
    pub fn with_capacity(max_capacity: usize) -> Self {
        let mut store = Self::new();
        store.max_capacity = Some(max_capacity);
        store
    }

    /// Insert or update node metadata
    ///
    /// The entire (read-old, remove-from-indexes,
    /// add-to-indexes, insert) sequence runs inside
    /// `DashMap::entry`'s shard write lock, serializing all
    /// concurrent upserts on the same node_id. Splitting this into
    /// a 5-step sequence without an overarching lock — (1) capacity
    /// check, (2) `nodes.get(&id)`, (3) `remove_from_indexes(&old)`,
    /// (4) `add_to_indexes(&new)`, (5) `nodes.insert` — would let
    /// two concurrent upserts on the same node both observe the
    /// same `old` at step 2, both remove its index entries at step
    /// 3 (second a no-op), and both add to indexes at step 4 into
    /// DIFFERENT buckets if the metadata differed. Whichever
    /// `nodes.insert` landed second would win, but the loser's
    /// index entries would never be removed, producing permanent
    /// index drift (queries return the node under the wrong
    /// filter; stats over-count).
    pub fn upsert(&self, metadata: NodeMetadata) -> Result<(), MetadataError> {
        // Bound peer-supplied metadata before touching the
        // indexes — without this, one peer could ship a single
        // announcement carrying millions of unique tags and turn
        // it into millions of `by_tag` DashMap entries. Validation
        // runs first so we don't even pay the index-clear cost on
        // a bad input.
        metadata.validate_bounds()?;

        let node_id = metadata.node_id;

        // Capacity check BEFORE entering the entry guard —
        // `self.nodes.len()` walks all shards and would deadlock
        // if called while we hold a write guard on one of them.
        // The soft-cap race window (a concurrent upsert lands
        // between this check and the entry-acquire below) is
        // acceptable: the cap is best-effort, mirroring the
        // pattern used by `TokenCache::insert_unchecked` and
        // `ContextStore::create_context`.
        if let Some(max) = self.max_capacity {
            if !self.nodes.contains_key(&node_id)
                && self.node_count.load(Ordering::Relaxed) >= max
            {
                return Err(MetadataError::CapacityExceeded);
            }
        }

        // Take the per-shard write lock on the node_id entry
        // FIRST. Holding it serializes all concurrent upserts on
        // this id, so the (remove_from_indexes, add_to_indexes,
        // insert) sequence is observed by other upserts as a
        // single atomic transition. A read-old outside any lock
        // would let two threads both observe the same `old`, both
        // `remove_from_indexes(&old)`, both `add_to_indexes` into
        // different buckets, and the loser's entries would leak
        // into permanent index drift.
        //
        // `add_to_indexes` / `remove_from_indexes` write to
        // OTHER DashMap instances (`by_status`, `by_tag`, etc.).
        // The lock-order convention is: hold `nodes` entry FIRST,
        // then write the index DashMaps. As long as no other
        // operation locks an index DashMap and then reaches into
        // `nodes`, we're deadlock-free. The `query` path takes
        // index DashMap snapshots first, then reads `nodes`
        // afterwards — that order is the inverse of ours, but
        // each step's lock is released before the next is taken,
        // so there's no held-lock chain to deadlock.
        use dashmap::mapref::entry::Entry;
        match self.nodes.entry(node_id) {
            Entry::Vacant(slot) => {
                self.add_to_indexes(&metadata);
                slot.insert(Arc::new(metadata));
                // Genuine insert (the only branch that grows `nodes`).
                self.node_count.fetch_add(1, Ordering::Relaxed);
            }
            Entry::Occupied(mut slot) => {
                // Read the old metadata WHILE holding the entry
                // lock — this is the critical change vs. pre-fix,
                // where the read-old happened before the lock and
                // could be invalidated by a concurrent upsert.
                let old = slot.get().clone();
                self.remove_from_indexes(&old);
                self.add_to_indexes(&metadata);
                slot.insert(Arc::new(metadata));
            }
        }
        self.update_count.fetch_add(1, Ordering::Relaxed);

        Ok(())
    }

    /// Update with version check (optimistic locking)
    pub fn update_versioned(
        &self,
        metadata: NodeMetadata,
        expected_version: u64,
    ) -> Result<(), MetadataError> {
        let node_id = metadata.node_id;

        // Check version
        if let Some(existing) = self.nodes.get(&node_id) {
            if existing.version != expected_version {
                return Err(MetadataError::VersionConflict {
                    expected: expected_version,
                    actual: existing.version,
                });
            }
        }

        self.upsert(metadata)
    }

    /// Get node metadata
    pub fn get(&self, node_id: &NodeId) -> Option<Arc<NodeMetadata>> {
        self.nodes.get(node_id).map(|r| Arc::clone(&r))
    }

    /// Remove node
    pub fn remove(&self, node_id: &NodeId) -> Option<Arc<NodeMetadata>> {
        if let Some((_, meta)) = self.nodes.remove(node_id) {
            self.remove_from_indexes(&meta);
            self.node_count.fetch_sub(1, Ordering::Relaxed);
            Some(meta)
        } else {
            None
        }
    }

    /// Query nodes
    pub fn query(&self, query: &MetadataQuery) -> Vec<Arc<NodeMetadata>> {
        self.query_count.fetch_add(1, Ordering::Relaxed);

        // Use indexes for initial filtering if possible
        let candidates: Vec<NodeId> = if let Some(status) = query.status {
            self.by_status
                .get(&status)
                .map(|s| s.iter().copied().collect())
                .unwrap_or_default()
        } else if let Some(ref continent) = query.continent {
            self.by_continent
                .get(continent)
                .map(|s| s.iter().copied().collect())
                .unwrap_or_default()
        } else if let Some(min_tier) = query.min_tier {
            // Collect all nodes at or above min_tier
            let mut nodes = HashSet::new();
            for tier in [
                NetworkTier::Edge,
                NetworkTier::Consumer,
                NetworkTier::Business,
                NetworkTier::Datacenter,
                NetworkTier::Premium,
                NetworkTier::Core,
            ] {
                if tier >= min_tier {
                    if let Some(tier_nodes) = self.by_tier.get(&tier) {
                        nodes.extend(tier_nodes.iter().copied());
                    }
                }
            }
            nodes.into_iter().collect()
        } else {
            // Full scan
            self.nodes.iter().map(|r| *r.key()).collect()
        };

        // Filter and collect results
        let mut results: Vec<Arc<NodeMetadata>> = candidates
            .into_iter()
            .filter_map(|id| self.nodes.get(&id).map(|r| Arc::clone(&r)))
            .filter(|meta| query.matches(meta))
            .collect();

        // Apply limit
        if let Some(limit) = query.limit {
            results.truncate(limit);
        }

        results
    }

    /// Find nodes near a location
    pub fn find_nearby(
        &self,
        location: &LocationInfo,
        max_distance_km: f64,
        limit: usize,
    ) -> Vec<(Arc<NodeMetadata>, f64)> {
        self.query_count.fetch_add(1, Ordering::Relaxed);

        let mut results: Vec<(Arc<NodeMetadata>, f64)> = self
            .nodes
            .iter()
            .filter_map(|r| {
                let meta = Arc::clone(r.value());
                meta.location
                    .as_ref()
                    .and_then(|loc| location.distance_to(loc))
                    .filter(|&d| d <= max_distance_km)
                    .map(|d| (meta, d))
            })
            .collect();

        // `partial_cmp(...).unwrap_or(Equal)` on NaN produces a
        // non-total order — `sort_by` would permute arbitrarily
        // and `truncate(limit)` would then drop random items.
        // `LocationInfo::distance_to` computes `(...).asin()` for
        // near-antipodal points where FP rounding can push the
        // asin argument > 1.0 → NaN. `total_cmp` on a
        // NaN-sentinel score (`f64::INFINITY` so NaN distances
        // sink to the end) gives a deterministic total order.
        results.sort_by(|a, b| {
            let a_dist = if a.1.is_nan() { f64::INFINITY } else { a.1 };
            let b_dist = if b.1.is_nan() { f64::INFINITY } else { b.1 };
            a_dist.total_cmp(&b_dist)
        });
        results.truncate(limit);

        results
    }

    /// Find best nodes for routing
    pub fn find_best_for_routing(&self, limit: usize) -> Vec<Arc<NodeMetadata>> {
        self.query_count.fetch_add(1, Ordering::Relaxed);

        let mut results: Vec<(Arc<NodeMetadata>, f64)> = self
            .nodes
            .iter()
            .filter(|r| r.value().status.accepts_work())
            .map(|r| {
                let meta = Arc::clone(r.value());
                let score = meta.routing_score();
                (meta, score)
            })
            .collect();

        // Same hazard as `find_nearby` above. Sort descending
        // with `total_cmp`; NaN scores sink to the end (treated
        // as `f64::NEG_INFINITY` for descending).
        results.sort_by(|a, b| {
            let a_score = if a.1.is_nan() { f64::NEG_INFINITY } else { a.1 };
            let b_score = if b.1.is_nan() { f64::NEG_INFINITY } else { b.1 };
            b_score.total_cmp(&a_score)
        });
        results.truncate(limit);

        results.into_iter().map(|(m, _)| m).collect()
    }

    /// Find relay nodes
    pub fn find_relays(&self) -> Vec<Arc<NodeMetadata>> {
        self.query(&MetadataQuery::new().can_relay().accepting_work())
    }

    /// Get statistics
    pub fn stats(&self) -> MetadataStoreStats {
        // Histograms come straight from the inverted indexes, which
        // add_to_indexes/remove_from_indexes keep in sync. This is
        // O(distinct keys) — a handful of statuses/tiers/continents —
        // instead of an O(nodes) full scan with a String allocation per
        // node. `remove_from_indexes` can leave an empty set behind, so
        // skip zero-count buckets to match the old scan's "absent key"
        // output (a status with no nodes was simply never in the map).
        let by_status: HashMap<NodeStatus, usize> = self
            .by_status
            .iter()
            .filter(|e| !e.value().is_empty())
            .map(|e| (*e.key(), e.value().len()))
            .collect();
        let by_tier: HashMap<NetworkTier, usize> = self
            .by_tier
            .iter()
            .filter(|e| !e.value().is_empty())
            .map(|e| (*e.key(), e.value().len()))
            .collect();
        let by_continent: HashMap<String, usize> = self
            .by_continent
            .iter()
            .filter(|e| !e.value().is_empty())
            .map(|e| (e.key().clone(), e.value().len()))
            .collect();

        MetadataStoreStats {
            total_nodes: self.node_count.load(Ordering::Relaxed),
            by_status,
            by_tier,
            by_continent,
            queries: self.query_count.load(Ordering::Relaxed),
            updates: self.update_count.load(Ordering::Relaxed),
        }
    }

    /// Number of nodes
    pub fn len(&self) -> usize {
        self.node_count.load(Ordering::Relaxed)
    }

    /// Check if empty
    pub fn is_empty(&self) -> bool {
        self.node_count.load(Ordering::Relaxed) == 0
    }

    /// Clear all nodes
    ///
    /// Drains `nodes` FIRST and routes every drained metadata
    /// through `remove_from_indexes` — making the intermediate
    /// state consistent (nodes exist alongside their indexes
    /// throughout the drain). A naive `nodes.clear()` followed by
    /// six index `clear()`s in sequence would let a concurrent
    /// `upsert` landing between any two of those clears observe
    /// `nodes.get(&id) → None` (skipping `remove_from_indexes`),
    /// then `add_to_indexes` (writing into the SAME index maps
    /// `clear` is about to wipe), then `nodes.insert(...)` — the
    /// final state would be a node in `nodes` with NO index
    /// entries, invisible to every indexed query and only
    /// retrievable via the full-scan branch.
    ///
    /// With the drain-first ordering, any concurrent `upsert`
    /// landing during the drain either races BEFORE this function
    /// reads its key (the upsert wins; we drain its entry
    /// afterward) or AFTER (the upsert observes a cleared `nodes`
    /// and proceeds normally — no index drift, since
    /// `remove_from_indexes` only touches keys that exist in
    /// `nodes`). The final `clear`s on the index maps catch any
    /// residual entries the per-key path missed
    /// (defense-in-depth; should be no-ops on the happy path).
    pub fn clear(&self) {
        // `dashmap::DashMap` doesn't have a `drain()` that takes
        // ownership of every entry; use a remove-on-iter pattern.
        // Collect keys first, then remove and route through
        // `remove_from_indexes`. Holding the iter guard across
        // `remove` would deadlock — the keys vec is the
        // intermediate.
        let keys: Vec<NodeId> = self.nodes.iter().map(|r| *r.key()).collect();
        for key in keys {
            if let Some((_, meta)) = self.nodes.remove(&key) {
                self.remove_from_indexes(&meta);
                // Per-remove decrement (not store(0)) so a concurrent upsert
                // landing during the drain keeps its own +1 — the count stays
                // exact w.r.t. `nodes` rather than racing to a wrong 0.
                self.node_count.fetch_sub(1, Ordering::Relaxed);
            }
        }
        // Defense-in-depth: clear any residual entries that may
        // have leaked in via a concurrent upsert that landed
        // after our key-collection iterator finished but before
        // this point. These should be no-ops on the happy path.
        self.by_status.clear();
        self.by_tier.clear();
        self.by_continent.clear();
        self.by_tag.clear();
        self.by_role.clear();
        self.by_owner.clear();
    }

    // Private helper to add node to indexes
    fn add_to_indexes(&self, meta: &NodeMetadata) {
        let node_id = meta.node_id;

        // Status index
        self.by_status
            .entry(meta.status)
            .or_default()
            .insert(node_id);

        // Tier index
        self.by_tier
            .entry(meta.topology.tier)
            .or_default()
            .insert(node_id);

        // Continent index
        if let Some(ref loc) = meta.location {
            self.by_continent
                .entry(loc.region.continent().to_string())
                .or_default()
                .insert(node_id);
        }

        // Tag index
        for tag in &meta.tags {
            self.by_tag.entry(tag.clone()).or_default().insert(node_id);
        }

        // Role index
        for role in &meta.roles {
            self.by_role
                .entry(role.clone())
                .or_default()
                .insert(node_id);
        }

        // Owner index
        if let Some(ref owner) = meta.owner {
            self.by_owner
                .entry(owner.clone())
                .or_default()
                .insert(node_id);
        }
    }

    // Private helper to remove node from indexes
    fn remove_from_indexes(&self, meta: &NodeMetadata) {
        let node_id = meta.node_id;

        // Status index
        if let Some(mut set) = self.by_status.get_mut(&meta.status) {
            set.remove(&node_id);
        }

        // Tier index
        if let Some(mut set) = self.by_tier.get_mut(&meta.topology.tier) {
            set.remove(&node_id);
        }

        // Continent index
        if let Some(ref loc) = meta.location {
            if let Some(mut set) = self.by_continent.get_mut(loc.region.continent()) {
                set.remove(&node_id);
            }
        }

        // Tag index
        for tag in &meta.tags {
            if let Some(mut set) = self.by_tag.get_mut(tag) {
                set.remove(&node_id);
            }
        }

        // Role index
        for role in &meta.roles {
            if let Some(mut set) = self.by_role.get_mut(role) {
                set.remove(&node_id);
            }
        }

        // Owner index
        if let Some(ref owner) = meta.owner {
            if let Some(mut set) = self.by_owner.get_mut(owner) {
                set.remove(&node_id);
            }
        }
    }
}

impl Default for MetadataStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_node_id(n: u8) -> NodeId {
        let mut id = [0u8; 32];
        id[0] = n;
        id
    }

    #[test]
    fn test_location_distance() {
        // New York
        let ny = LocationInfo::new(Region::NorthAmerica("us-east".into()))
            .with_coordinates(40.7128, -74.0060);

        // Los Angeles
        let la = LocationInfo::new(Region::NorthAmerica("us-west".into()))
            .with_coordinates(34.0522, -118.2437);

        // London
        let london =
            LocationInfo::new(Region::Europe("uk".into())).with_coordinates(51.5074, -0.1278);

        let ny_la = ny.distance_to(&la).unwrap();
        assert!(ny_la > 3900.0 && ny_la < 4000.0, "NY-LA: {}", ny_la);

        let ny_london = ny.distance_to(&london).unwrap();
        assert!(
            ny_london > 5500.0 && ny_london < 5600.0,
            "NY-London: {}",
            ny_london
        );

        assert!(ny.same_continent(&la));
        assert!(!ny.same_continent(&london));
    }

    #[test]
    fn test_nat_connectivity() {
        assert!(NatType::None.can_connect_direct(&NatType::Symmetric));
        assert!(NatType::FullCone.can_connect_direct(&NatType::RestrictedCone));
        assert!(!NatType::Symmetric.can_connect_direct(&NatType::Symmetric));
    }

    #[test]
    fn test_topology_score() {
        let basic = TopologyHints::new(NetworkTier::Consumer);
        let premium = TopologyHints::new(NetworkTier::Premium)
            .with_bandwidth(1000, 1000)
            .with_nat(NatType::None)
            .with_relay(100);

        assert!(premium.connectivity_score() > basic.connectivity_score());
    }

    #[test]
    fn test_node_metadata() {
        let node = NodeMetadata::new(make_node_id(1))
            .with_name("test-node")
            .with_owner("test-org")
            .with_tag("gpu")
            .with_role("worker")
            .with_status(NodeStatus::Online);

        assert_eq!(node.name, Some("test-node".into()));
        assert!(node.tags.contains("gpu"));
        assert!(node.roles.contains("worker"));
        assert!(node.status.accepts_work());
    }

    #[test]
    fn test_metadata_store_basic() {
        let store = MetadataStore::new();

        let node1 = NodeMetadata::new(make_node_id(1))
            .with_name("node1")
            .with_status(NodeStatus::Online);

        let node2 = NodeMetadata::new(make_node_id(2))
            .with_name("node2")
            .with_status(NodeStatus::Degraded);

        store.upsert(node1).unwrap();
        store.upsert(node2).unwrap();

        assert_eq!(store.len(), 2);

        let retrieved = store.get(&make_node_id(1)).unwrap();
        assert_eq!(retrieved.name, Some("node1".into()));

        store.remove(&make_node_id(1));
        assert_eq!(store.len(), 1);
        assert!(store.get(&make_node_id(1)).is_none());
    }

    #[test]
    fn test_metadata_query() {
        let store = MetadataStore::new();

        // Add nodes with different properties
        for i in 0..10 {
            let status = if i < 5 {
                NodeStatus::Online
            } else {
                NodeStatus::Degraded
            };
            let tier = if i < 3 {
                NetworkTier::Core
            } else {
                NetworkTier::Consumer
            };

            let mut node = NodeMetadata::new(make_node_id(i))
                .with_status(status)
                .with_topology(TopologyHints::new(tier));

            if i % 2 == 0 {
                node.tags.insert("even".into());
            }

            store.upsert(node).unwrap();
        }

        // Query by status
        let online = store.query(&MetadataQuery::new().with_status(NodeStatus::Online));
        assert_eq!(online.len(), 5);

        // Query by tier
        let core = store.query(&MetadataQuery::new().with_min_tier(NetworkTier::Core));
        assert_eq!(core.len(), 3);

        // Query accepting work
        let working = store.query(&MetadataQuery::new().accepting_work());
        assert_eq!(working.len(), 10); // Both Online and Degraded accept work

        // Query with limit
        let limited = store.query(&MetadataQuery::new().with_limit(3));
        assert_eq!(limited.len(), 3);
    }

    #[test]
    fn test_find_nearby() {
        let store = MetadataStore::new();

        // Add nodes at different locations
        let locations = [
            (40.7128, -74.0060),  // NY
            (34.0522, -118.2437), // LA
            (51.5074, -0.1278),   // London
        ];

        for (i, (lat, lon)) in locations.iter().enumerate() {
            let node = NodeMetadata::new(make_node_id(i as u8))
                .with_location(
                    LocationInfo::new(Region::NorthAmerica("test".into()))
                        .with_coordinates(*lat, *lon),
                )
                .with_status(NodeStatus::Online);
            store.upsert(node).unwrap();
        }

        // Find nodes near NY
        let ny = LocationInfo::new(Region::NorthAmerica("test".into()))
            .with_coordinates(40.7128, -74.0060);

        let nearby = store.find_nearby(&ny, 100.0, 10);
        assert_eq!(nearby.len(), 1); // Only NY itself is within 100km

        let nearby = store.find_nearby(&ny, 5000.0, 10);
        assert_eq!(nearby.len(), 2); // NY and LA within 5000km

        let nearby = store.find_nearby(&ny, 10000.0, 10);
        assert_eq!(nearby.len(), 3); // All within 10000km
    }

    #[test]
    fn test_find_relays() {
        let store = MetadataStore::new();

        let relay_node = NodeMetadata::new(make_node_id(1))
            .with_topology(TopologyHints::new(NetworkTier::Datacenter).with_relay(100))
            .with_status(NodeStatus::Online);

        let normal_node = NodeMetadata::new(make_node_id(2))
            .with_topology(TopologyHints::new(NetworkTier::Consumer))
            .with_status(NodeStatus::Online);

        store.upsert(relay_node).unwrap();
        store.upsert(normal_node).unwrap();

        let relays = store.find_relays();
        assert_eq!(relays.len(), 1);
    }

    #[test]
    fn test_version_conflict() {
        let store = MetadataStore::new();

        let node = NodeMetadata::new(make_node_id(1));
        store.upsert(node.clone()).unwrap();

        // Try to update with wrong version
        let result = store.update_versioned(node.clone(), 999);
        assert!(matches!(result, Err(MetadataError::VersionConflict { .. })));

        // Update with correct version
        let result = store.update_versioned(node, 1);
        assert!(result.is_ok());
    }

    #[test]
    fn test_capacity_limit() {
        let store = MetadataStore::with_capacity(2);

        store.upsert(NodeMetadata::new(make_node_id(1))).unwrap();
        store.upsert(NodeMetadata::new(make_node_id(2))).unwrap();

        let result = store.upsert(NodeMetadata::new(make_node_id(3)));
        assert!(matches!(result, Err(MetadataError::CapacityExceeded)));

        // Can still update existing
        store.upsert(NodeMetadata::new(make_node_id(1))).unwrap();
    }

    /// CR-30: pin the invariant that every `Arc<NodeMetadata>`
    /// returned from a read path (`get`, `query`, `find_nearby`,
    /// `best_for_routing`) satisfies [`NodeMetadata::validate_bounds`].
    /// Pre-CR-30 the bounds check ran only on `upsert` /
    /// `update_versioned`; if a future refactor adds a write path
    /// that bypasses both (e.g. snapshot restore that deserializes
    /// raw `NodeMetadata` and inserts it into `nodes` directly), an
    /// over-bounded entry could leak into reads. This test pins
    /// the read-side contract so a future maintainer either
    /// honours it on every new write path OR has to update the
    /// test.
    #[test]
    fn cr30_read_path_invariant_every_returned_node_passes_validate_bounds() {
        let store = MetadataStore::new();
        let mut node = NodeMetadata::new(make_node_id(1));
        node.tags.insert("training".into());
        store.upsert(node).unwrap();

        // Read via `get`: the returned Arc must validate cleanly.
        let got = store.get(&make_node_id(1)).expect("inserted node");
        got.validate_bounds().expect(
            "CR-30: every node returned from MetadataStore::get MUST satisfy \
             validate_bounds. If this fires, a write path is bypassing \
             upsert's bound check.",
        );

        // Read via `query`: same invariant.
        let q = MetadataQuery::new();
        for entry in store.query(&q) {
            entry.validate_bounds().expect(
                "CR-30: every node returned from MetadataStore::query MUST \
                 satisfy validate_bounds.",
            );
        }
    }

    /// CR-18: pin the soft-cap race window. The capacity check sits
    /// outside the `DashMap::entry` write lock (cannot move it inside
    /// without `nodes.len()` self-deadlocking), so two concurrent
    /// upserts of distinct `node_id`s can both pass the
    /// `nodes.len() >= max` check and both insert past the cap. This
    /// is documented as acceptable behavior — `max_capacity` is a
    /// best-effort target, not a hard cap. The test intentionally
    /// exercises the race and pins that the `len()` may transiently
    /// exceed `max` so a future "fix" that turns this into a hard
    /// cap also has to update this test.
    ///
    /// (Mirrors the pattern in `TokenCache::insert_unchecked` and
    /// `ContextStore::create_context`.)
    #[test]
    fn cr18_capacity_check_is_a_soft_cap_under_concurrent_upserts() {
        use std::sync::Arc;
        use std::thread;

        const MAX: usize = 4;
        const N_THREADS: usize = 16;

        let store = Arc::new(MetadataStore::with_capacity(MAX));
        let barrier = Arc::new(std::sync::Barrier::new(N_THREADS));

        let mut handles = Vec::with_capacity(N_THREADS);
        for t in 0..N_THREADS {
            let store = Arc::clone(&store);
            let barrier = Arc::clone(&barrier);
            handles.push(thread::spawn(move || {
                barrier.wait();
                let id = make_node_id(t as u8 + 1);
                store.upsert(NodeMetadata::new(id))
            }));
        }
        let mut accepted = 0usize;
        let mut rejected = 0usize;
        for h in handles {
            match h.join().unwrap() {
                Ok(()) => accepted += 1,
                Err(MetadataError::CapacityExceeded) => rejected += 1,
                Err(other) => panic!("unexpected upsert error: {other:?}"),
            }
        }

        // CR-18: pin that at LEAST `MAX` upserts succeeded. The
        // total `accepted` may be > MAX under heavy concurrency
        // because the soft-cap check loses the race against
        // multiple concurrent inserters of distinct ids — that's
        // the documented behavior. Pre-CR-18 the docs didn't
        // call this out at the public-API level, so a caller
        // reading `with_capacity(N)` might assume a hard ceiling.
        assert!(
            accepted >= MAX,
            "at least the cap's worth must succeed; got {accepted}"
        );
        assert!(
            accepted + rejected == N_THREADS,
            "every upsert must surface either Ok or CapacityExceeded"
        );
        // The store SIZE may transiently equal `accepted`, which
        // can exceed MAX. Pin THAT property so the soft-cap
        // semantic is documented in code, not just docs.
        assert!(
            store.nodes.len() <= accepted,
            "store size must not exceed the count of successful upserts"
        );
        // If accepted > MAX, the soft-cap was crossed — that's
        // the documented limitation. Just confirm nothing's torn.
    }

    // ========================================================================
    // NodeMetadata bounds must be enforced before indexing
    // ========================================================================

    /// Oversized tag set is rejected by `upsert` before touching the
    /// index DashMaps. Pre-fix a peer could ship a single
    /// announcement with millions of unique tags and turn it into
    /// millions of `by_tag` entries.
    #[test]
    fn upsert_rejects_oversized_tags() {
        let store = MetadataStore::new();
        let mut node = NodeMetadata::new(make_node_id(1));
        for i in 0..(MAX_METADATA_TAGS + 1) {
            node.tags.insert(format!("t{}", i));
        }
        let result = store.upsert(node);
        assert!(
            matches!(result, Err(MetadataError::Invalid(_))),
            "oversized tags must surface as MetadataError::Invalid, got {:?}",
            result,
        );
    }

    /// Oversized custom-map is rejected.
    #[test]
    fn upsert_rejects_oversized_custom_map() {
        let store = MetadataStore::new();
        let mut node = NodeMetadata::new(make_node_id(2));
        for i in 0..(MAX_METADATA_CUSTOM_ENTRIES + 1) {
            node.custom.insert(format!("k{}", i), "v".to_string());
        }
        assert!(matches!(store.upsert(node), Err(MetadataError::Invalid(_))));
    }

    /// A single string field over the per-string cap is rejected.
    #[test]
    fn upsert_rejects_oversized_string_fields() {
        let store = MetadataStore::new();
        let huge = "x".repeat(MAX_METADATA_STRING_LEN + 1);
        let node = NodeMetadata::new(make_node_id(3)).with_name(huge);
        assert!(matches!(store.upsert(node), Err(MetadataError::Invalid(_))));
    }

    /// Metadata at exactly the boundaries is accepted — pins the
    /// `<=` semantics so a future tightening to strict `<` doesn't
    /// silently break legitimate-but-large announcements.
    #[test]
    fn upsert_accepts_metadata_at_exact_boundaries() {
        let store = MetadataStore::new();
        let mut node = NodeMetadata::new(make_node_id(4));
        for i in 0..MAX_METADATA_TAGS {
            node.tags.insert(format!("t{}", i));
        }
        let name_at_cap = "x".repeat(MAX_METADATA_STRING_LEN);
        node = node.with_name(name_at_cap);
        store
            .upsert(node)
            .expect("metadata at the exact boundaries must be accepted");
    }

    // ========================================================================
    // upsert must serialize concurrent writers on the same node_id
    // ========================================================================

    /// Concurrent `upsert`s on the same `node_id` with DIFFERENT
    /// metadata must leave the inverted indexes consistent —
    /// exactly one (status, tag) pairing per node, matching the
    /// final stored metadata. Pre-fix the read-old happened
    /// outside any lock, so two threads could observe the same
    /// `old`, both `remove_from_indexes(old)`, both
    /// `add_to_indexes(new_a)` / `add_to_indexes(new_b)` into
    /// different buckets, and the loser's index entries leaked
    /// permanently.
    #[test]
    fn upsert_serializes_concurrent_writes_on_same_node_id() {
        use std::sync::Arc;
        use std::thread;

        let store = Arc::new(MetadataStore::new());
        let node_id = make_node_id(42);

        // Seed the store so both threads see an existing entry to
        // remove from indexes — the original race vector.
        let seed = NodeMetadata::new(node_id)
            .with_status(NodeStatus::Online)
            .with_tag("seed")
            .with_topology(TopologyHints::new(NetworkTier::Consumer));
        store.upsert(seed).unwrap();

        // Two threads upsert different metadata on the same id.
        // Each contends with the other for the node's shard write
        // guard.
        let n_iters = 50;
        let store_a = store.clone();
        let store_b = store.clone();
        let h_a = thread::spawn(move || {
            for i in 0..n_iters {
                let node = NodeMetadata::new(node_id)
                    .with_status(NodeStatus::Online)
                    .with_tag(format!("a-{}", i))
                    .with_topology(TopologyHints::new(NetworkTier::Premium));
                store_a.upsert(node).unwrap();
            }
        });
        let h_b = thread::spawn(move || {
            for i in 0..n_iters {
                let node = NodeMetadata::new(node_id)
                    .with_status(NodeStatus::Degraded)
                    .with_tag(format!("b-{}", i))
                    .with_topology(TopologyHints::new(NetworkTier::Datacenter));
                store_b.upsert(node).unwrap();
            }
        });
        h_a.join().unwrap();
        h_b.join().unwrap();

        // Final metadata: one of the two threads' last write
        // landed last. Whatever it is, the inverted indexes must
        // reflect ONLY that final metadata — no leftover
        // status/tag entries from the other thread.
        let final_meta = store.get(&node_id).expect("node must still exist");
        let final_status = final_meta.status;
        let final_tags: std::collections::HashSet<&str> =
            final_meta.tags.iter().map(|s| s.as_str()).collect();

        // No status bucket OTHER than the final one may contain
        // this node_id.
        for status in [
            NodeStatus::Online,
            NodeStatus::Offline,
            NodeStatus::Degraded,
            NodeStatus::Starting,
            NodeStatus::Maintenance,
        ] {
            let bucket_has_node = store
                .by_status
                .get(&status)
                .map(|s| s.contains(&node_id))
                .unwrap_or(false);
            if status == final_status {
                assert!(
                    bucket_has_node,
                    "final status {:?} bucket must contain the node",
                    status
                );
            } else {
                assert!(
                    !bucket_has_node,
                    "stale status {:?} bucket must NOT contain the node",
                    status
                );
            }
        }

        // No tag bucket OTHER than the final tags may contain
        // this node_id. Walk every tag bucket the threads might
        // have touched.
        for i in 0..n_iters {
            for prefix in ["a-", "b-"] {
                let tag = format!("{}{}", prefix, i);
                if final_tags.contains(tag.as_str()) {
                    continue;
                }
                let bucket_has_node = store
                    .by_tag
                    .get(&tag)
                    .map(|s| s.contains(&node_id))
                    .unwrap_or(false);
                assert!(
                    !bucket_has_node,
                    "stale tag '{}' bucket must NOT contain the node",
                    tag
                );
            }
        }
        // The seed tag must also be gone.
        let seed_bucket_has_node = store
            .by_tag
            .get("seed")
            .map(|s| s.contains(&node_id))
            .unwrap_or(false);
        assert!(
            !seed_bucket_has_node,
            "the original seed tag must have been removed"
        );
    }

    #[test]
    fn test_stats() {
        let store = MetadataStore::new();

        for i in 0..5 {
            let node = NodeMetadata::new(make_node_id(i))
                .with_status(if i < 3 {
                    NodeStatus::Online
                } else {
                    NodeStatus::Offline
                })
                .with_topology(TopologyHints::new(NetworkTier::Consumer));
            store.upsert(node).unwrap();
        }

        // Perform some queries
        store.query(&MetadataQuery::new());
        store.query(&MetadataQuery::new());

        let stats = store.stats();
        assert_eq!(stats.total_nodes, 5);
        assert_eq!(stats.by_status.get(&NodeStatus::Online), Some(&3));
        assert_eq!(stats.by_status.get(&NodeStatus::Offline), Some(&2));
        assert_eq!(stats.queries, 2);
        assert_eq!(stats.updates, 5);
    }

    /// `stats()` is now computed from the inverted indexes and
    /// `total_nodes` / `len()` from an O(1) counter. Verify both stay
    /// exact across upsert, status change, remove (which can leave an
    /// empty index bucket — that bucket must NOT surface as a 0 count),
    /// and clear.
    #[test]
    fn stats_and_len_track_inserts_removes_and_status_changes() {
        let store = MetadataStore::new();
        for i in 0..4u8 {
            store
                .upsert(
                    NodeMetadata::new(make_node_id(i)).with_status(NodeStatus::Online),
                )
                .unwrap();
        }
        assert_eq!(store.len(), 4);
        assert_eq!(store.stats().total_nodes, 4);
        assert_eq!(store.stats().by_status.get(&NodeStatus::Online), Some(&4));

        // Re-upsert (same id, new status) must not grow len, and must move
        // the node between status buckets.
        store
            .upsert(NodeMetadata::new(make_node_id(0)).with_status(NodeStatus::Offline))
            .unwrap();
        assert_eq!(store.len(), 4, "re-upsert must not change node count");
        let s = store.stats();
        assert_eq!(s.by_status.get(&NodeStatus::Online), Some(&3));
        assert_eq!(s.by_status.get(&NodeStatus::Offline), Some(&1));

        // Remove the only Offline node — its bucket empties and must drop
        // out of the histogram entirely (not report 0).
        store.remove(&make_node_id(0));
        assert_eq!(store.len(), 3);
        let s = store.stats();
        assert_eq!(s.total_nodes, 3);
        assert_eq!(
            s.by_status.get(&NodeStatus::Offline),
            None,
            "emptied status bucket must not appear in stats"
        );

        store.clear();
        assert_eq!(store.len(), 0);
        assert!(store.is_empty());
        assert_eq!(store.stats().total_nodes, 0);
        assert!(store.stats().by_status.is_empty());
    }

    // ========================================================================
    // Cubic-ai P2: validate_bounds must walk nested structs, not just
    // top-level fields and collection counts
    // ========================================================================

    /// `validate_bounds` rejects metadata whose nested
    /// `LocationInfo::provider` exceeds `MAX_METADATA_STRING_LEN`.
    /// Pre-fix only top-level strings (`name`, `description`,
    /// `owner`) and collection counts were checked, so a peer could
    /// stuff arbitrary multi-megabyte data into `location.provider`
    /// (or any other LocationInfo string) and slip past every guard.
    #[test]
    fn validate_bounds_rejects_oversized_location_string() {
        let mut node = NodeMetadata::new(make_node_id(1));
        node.location = Some(LocationInfo {
            region: Region::NorthAmerica("us-east".into()),
            zone: None,
            latitude: None,
            longitude: None,
            asn: None,
            provider: Some("p".repeat(MAX_METADATA_STRING_LEN + 1)),
            datacenter: None,
            country_code: None,
            city: None,
        });

        match node.validate_bounds() {
            Err(MetadataError::Invalid(msg)) => {
                assert!(
                    msg.contains("location.provider"),
                    "rejection must name the offending field; got: {msg}",
                );
            }
            other => panic!("expected Invalid for oversized location.provider, got {other:?}"),
        }
    }

    /// `validate_bounds` rejects metadata whose `hop_distances` contains
    /// a key longer than `MAX_METADATA_STRING_LEN`. Pre-fix only the
    /// map cardinality was capped — a single oversized key inside an
    /// otherwise small map smuggled the check.
    #[test]
    fn validate_bounds_rejects_oversized_hop_distances_key() {
        let mut topo = TopologyHints::new(NetworkTier::Consumer);
        topo.hop_distances
            .insert("k".repeat(MAX_METADATA_STRING_LEN + 1), 3);
        let node = NodeMetadata::new(make_node_id(1)).with_topology(topo);

        match node.validate_bounds() {
            Err(MetadataError::Invalid(msg)) => {
                assert!(
                    msg.contains("hop_distances key"),
                    "rejection must name the offending field; got: {msg}",
                );
            }
            other => {
                panic!("expected Invalid for oversized hop_distances key, got {other:?}")
            }
        }
    }

    /// Belt-and-braces happy path: a complete metadata bundle with
    /// every nested field at exactly `MAX_METADATA_STRING_LEN` must
    /// pass — the new checks are bound-strict (`>`, not `>=`), so a
    /// future tightening that flipped the comparator would lock out
    /// legitimate boundary callers and this test would catch it.
    #[test]
    fn validate_bounds_accepts_at_boundary_lengths() {
        let mut node = NodeMetadata::new(make_node_id(1));
        node.location = Some(LocationInfo {
            region: Region::Europe("uk".into()),
            zone: Some("z".repeat(MAX_METADATA_STRING_LEN)),
            latitude: None,
            longitude: None,
            asn: None,
            provider: Some("p".repeat(MAX_METADATA_STRING_LEN)),
            datacenter: Some("d".repeat(MAX_METADATA_STRING_LEN)),
            country_code: Some("c".repeat(MAX_METADATA_STRING_LEN)),
            city: Some("y".repeat(MAX_METADATA_STRING_LEN)),
        });
        let mut topo = TopologyHints::new(NetworkTier::Consumer);
        topo.hop_distances
            .insert("k".repeat(MAX_METADATA_STRING_LEN), 1);
        node.topology = topo;

        node.validate_bounds()
            .expect("at-boundary nested strings must validate");
    }

    // ---------- Pure-function coverage ----------
    //
    // Existing tests construct nodes and run queries but never
    // call these accessors. Each is a tiny pure function; the
    // risk if they regress is silent (e.g., a routing scheduler
    // suddenly preferring Draining over Online).

    #[test]
    fn region_zone_returns_inner_zone_for_every_variant() {
        assert_eq!(Region::NorthAmerica("us-east".into()).zone(), "us-east");
        assert_eq!(Region::SouthAmerica("br-sp".into()).zone(), "br-sp");
        assert_eq!(Region::Europe("eu-west".into()).zone(), "eu-west");
        assert_eq!(Region::AsiaPacific("ap-1".into()).zone(), "ap-1");
        assert_eq!(Region::MiddleEast("me-1".into()).zone(), "me-1");
        assert_eq!(Region::Africa("af-1".into()).zone(), "af-1");
        assert_eq!(Region::Custom("custom-z".into()).zone(), "custom-z");
    }

    #[test]
    fn node_status_routing_priority_orders_variants_correctly() {
        // Online must outrank everything; Offline/ShuttingDown/
        // Maintenance must be the lowest tier. A regression that
        // swaps any pair here would silently mis-route traffic.
        assert!(NodeStatus::Online.routing_priority() > NodeStatus::Degraded.routing_priority());
        assert!(NodeStatus::Degraded.routing_priority() > NodeStatus::Starting.routing_priority());
        assert!(NodeStatus::Starting.routing_priority() > NodeStatus::Draining.routing_priority());
        assert_eq!(NodeStatus::Offline.routing_priority(), 0);
        assert_eq!(NodeStatus::ShuttingDown.routing_priority(), 0);
        assert_eq!(NodeStatus::Maintenance.routing_priority(), 0);
        assert_eq!(NodeStatus::Online.routing_priority(), 5);
    }

    #[test]
    fn average_latency_handles_empty_and_populated() {
        let mut topo = TopologyHints::new(NetworkTier::Consumer);
        assert!(topo.average_latency().is_none(), "empty must be None");

        topo.update_latency(make_node_id(1), 100);
        topo.update_latency(make_node_id(2), 200);
        topo.update_latency(make_node_id(3), 300);
        assert_eq!(topo.average_latency(), Some(200.0));
    }

    #[test]
    fn touch_advances_version_and_updated_at() {
        let mut node = NodeMetadata::new(make_node_id(1));
        let v0 = node.version;
        let t0 = node.updated_at;
        // Tiny sleep so wall-clock ms tick — `updated_at` is ms-resolution.
        std::thread::sleep(std::time::Duration::from_millis(5));
        node.touch();
        assert_eq!(node.version, v0 + 1);
        assert!(
            node.updated_at >= t0,
            "updated_at must not go backward (got {} from {t0})",
            node.updated_at,
        );
    }

    #[test]
    fn is_stale_compares_age_against_max_age() {
        let mut node = NodeMetadata::new(make_node_id(1));
        // Force updated_at into the past so age() reports a known
        // gap regardless of wall-clock drift on the test host.
        node.updated_at = node.updated_at.saturating_sub(60_000);
        assert!(node.is_stale(Duration::from_secs(10)));
        assert!(!node.is_stale(Duration::from_secs(3600)));
    }

    // ---------- validate_bounds error branches ----------

    #[test]
    fn validate_bounds_rejects_oversized_owner() {
        let mut node = NodeMetadata::new(make_node_id(1));
        node.owner = Some("o".repeat(MAX_METADATA_STRING_LEN + 1));
        let err = node.validate_bounds().unwrap_err();
        let msg = format!("{}", err);
        assert!(msg.contains("owner"), "error must name 'owner': {msg}");
    }

    #[test]
    fn validate_bounds_rejects_too_many_tags() {
        let mut node = NodeMetadata::new(make_node_id(1));
        for i in 0..MAX_METADATA_TAGS + 1 {
            node.tags.insert(format!("t{i}"));
        }
        assert!(matches!(
            node.validate_bounds(),
            Err(MetadataError::Invalid(_))
        ));
    }

    #[test]
    fn validate_bounds_rejects_oversized_role() {
        let mut node = NodeMetadata::new(make_node_id(1));
        node.roles.insert("r".repeat(MAX_METADATA_STRING_LEN + 1));
        let err = node.validate_bounds().unwrap_err();
        assert!(format!("{}", err).contains("role"));
    }

    #[test]
    fn validate_bounds_rejects_oversized_custom_map_entries() {
        let mut node = NodeMetadata::new(make_node_id(1));

        // Oversized key.
        node.custom
            .insert("k".repeat(MAX_METADATA_STRING_LEN + 1), "v".into());
        let err = node.validate_bounds().unwrap_err();
        assert!(format!("{}", err).contains("custom key"));

        // Oversized value.
        node.custom.clear();
        node.custom
            .insert("k".into(), "v".repeat(MAX_METADATA_STRING_LEN + 1));
        let err = node.validate_bounds().unwrap_err();
        assert!(format!("{}", err).contains("custom value"));
    }

    #[test]
    fn validate_bounds_rejects_too_many_custom_entries() {
        let mut node = NodeMetadata::new(make_node_id(1));
        for i in 0..MAX_METADATA_CUSTOM_ENTRIES + 1 {
            node.custom.insert(format!("k{i}"), "v".into());
        }
        assert!(matches!(
            node.validate_bounds(),
            Err(MetadataError::Invalid(_))
        ));
    }
}
