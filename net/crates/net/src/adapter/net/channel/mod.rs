//! Layer 2: Channels & Authorization for Net.
//!
//! Channels are named, policy-bearing logical endpoints. Access control
//! uses the existing capability system (`CapabilityFilter`) combined with
//! L1 permission tokens. Wire-speed authorization via bloom filter.

mod config;
mod guard;
// `membership` (the 0x0A00 codec) and `name` (validation + the
// canonical/wire hashes) moved to `net-mesh-wire` in Stage 5 — plan
// §7 assigns the wire-level subprotocol codecs to that crate, and the
// browser leaf's dispatcher is the second consumer S0a said would
// drive the move. Re-exported under their previous paths, so no
// `use …::channel::membership::…` site changed.
// `membership` stays a public module path (it was `pub mod
// membership`); `name` stays private (it was `mod name`), with only
// its `pub use`d items surfacing — so the export surface is
// unchanged in both directions.
pub use net_wire::channel::membership;
use net_wire::channel::name;
mod publisher;
mod roster;

pub use config::{
    ChannelConfig, ChannelConfigRegistry, OriginBinding, QueueGroupPolicy, ResolvedConfig,
    Visibility,
};
pub use guard::{AclPrincipal, AuthGuard, AuthVerdict};
pub use membership::{
    AckReason, MembershipCodecError, MembershipMsg, SUBPROTOCOL_CHANNEL_MEMBERSHIP,
};
pub use name::{
    channel_hash, queue_group_hash, wire_channel_hash, ChannelError, ChannelHash, ChannelId,
    ChannelName, ChannelRegistry,
};
pub use publisher::{ChannelPublisher, OnFailure, PublishConfig, PublishReport};
pub use roster::{QueueGroupName, SubscriberRoster, SubscriptionMode};
