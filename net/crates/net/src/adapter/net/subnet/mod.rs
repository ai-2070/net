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

pub mod admission;
mod assignment;
pub mod auth;
mod error;
mod gateway;
mod id;
pub mod route_hop;

pub use admission::{SubnetChallengeStore, SubnetContextStore};
pub use assignment::{SubnetPolicy, SubnetRule};
pub use auth::{
    build_gateway_context_set, compile_gateway_context, ExpectedBinding, ForwardDenial,
    SubnetAuthError, SubnetAuthPresentation, SubnetAuthorityConfig, SubnetBoundarySet,
    SubnetCredentialSet, SubnetFloorRegistry, SubnetGrant, SubnetIssuerGrant, SubnetRef,
    SubnetRevocationFloor, SubnetRights, VerifiedGatewayContext, VerifiedGatewayContextSet,
    VerifiedSubnetAuthority, VerifiedSubnetContext, MAX_GATEWAY_CONTEXTS_PER_AUTHORITY,
};
pub use error::SubnetError;
pub use gateway::{DropReason, ForwardDecision, SubnetGateway};
pub use id::{SubnetId, TopologySubnetId};
