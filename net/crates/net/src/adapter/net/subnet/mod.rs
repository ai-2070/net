//! Layer 3: Subnets & Hierarchy for Net.
//!
//! Subnets are label-based — nodes belong by identity/capability, not
//! static configuration. The hierarchy is encoded as 4 levels of 8 bits
//! each in the `subnet_id: u32` header field. Gateway nodes enforce
//! visibility policy at subnet boundaries.
//!
//! Everything in this module is **topology, not authority**: subnet
//! assignment classifies where traffic should propagate, and channel
//! visibility is a propagation filter. Access control lives elsewhere —
//! channel tokens for publish/subscribe, provider admission for
//! effects, and (per SUBNET_AUTH_PLAN.md) authority-qualified
//! `SubnetGrant`s for protected transport rights.

mod assignment;
mod error;
mod gateway;
mod id;

pub use assignment::{SubnetPolicy, SubnetRule};
pub use error::SubnetError;
pub use gateway::{DropReason, ForwardDecision, SubnetGateway};
pub use id::{SubnetId, TopologySubnetId};
