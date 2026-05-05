//! Layer 2: Channels & Authorization for Net.
//!
//! Channels are named, policy-bearing logical endpoints. Access control
//! uses the existing capability system (`CapabilityFilter`) combined with
//! L1 permission tokens. Wire-speed authorization via bloom filter.

mod config;
mod guard;
pub mod membership;
mod name;
mod publisher;
mod roster;

pub use config::{ChannelConfig, ChannelConfigRegistry, Visibility};
pub use guard::{AuthGuard, AuthVerdict};
pub use membership::{
    AckReason, MembershipCodecError, MembershipMsg, SUBPROTOCOL_CHANNEL_MEMBERSHIP,
};
pub use name::{channel_hash, ChannelError, ChannelId, ChannelName, ChannelRegistry};
pub use publisher::{ChannelPublisher, OnFailure, PublishConfig, PublishReport};
pub use roster::{QueueGroupName, SubscriberRoster, SubscriptionMode};
