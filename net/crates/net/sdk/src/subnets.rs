//! Subnet assignment — hierarchical 4-level grouping for routing and
//! visibility.
//!
//! `SubnetId` is a bit-packed `u32` encoding up to four levels of
//! nesting. `SubnetPolicy` maps capability tags onto a subnet id so
//! assignment can derive from what a node declares it is.
//! `SubnetGateway` decides at forwarding time whether a packet
//! addressed to one subnet should be forwarded to another.
//!
//! # Policy construction
//!
//! ```
//! use net_sdk::subnets::{SubnetId, SubnetPolicy, SubnetRule};
//!
//! // Policy: put nodes tagged `region:eu` into subnet level 0 bucket 1.
//! let policy = SubnetPolicy::new().add_rule(
//!     SubnetRule::new("region:", 0).map("eu", 1).map("us", 2),
//! );
//! let _ = policy;
//!
//! // Global subnet = "no restriction".
//! assert!(SubnetId::GLOBAL.is_global());
//! ```
//!
//! # Binding a subnet to a mesh (`--features net`)
//!
//! ```
//! # #[cfg(feature = "net")]
//! # async fn doc() -> net_sdk::error::Result<()> {
//! use std::sync::Arc;
//! use net_sdk::capabilities::CapabilitySet;
//! use net_sdk::mesh::MeshBuilder;
//! use net_sdk::subnets::{SubnetId, SubnetPolicy, SubnetRule};
//!
//! let policy = Arc::new(SubnetPolicy::new().add_rule(
//!     SubnetRule::new("region:", 0).map("eu", 1).map("us", 2),
//! ));
//!
//! let node = MeshBuilder::new("127.0.0.1:0", &[0x42u8; 32])?
//!     .subnet(SubnetId::new(&[1, 0, 0]))
//!     .subnet_policy(policy)
//!     .build()
//!     .await?;
//!
//! // The derived subnet of a peer comes from applying the policy to
//! // their announced `CapabilitySet`; our own subnet here is
//! // `[1, 0, 0]`. Self-announced caps must round-trip through the
//! // policy to the same value for consistent fan-out.
//! node.announce_capabilities(CapabilitySet::new().add_tag("region:eu"))
//!     .await?;
//!
//! node.shutdown().await?;
//! # Ok(())
//! # }
//! ```
//!
//! Today visibility enforcement covers `Visibility::SubnetLocal` and
//! `Visibility::ParentVisible`. `Visibility::Exported` and multi-hop
//! gateway routing are follow-ups; see
//! [`docs/internal/plans/SUBNET_ENFORCEMENT_PLAN.md`](../../../../../docs/internal/plans/SUBNET_ENFORCEMENT_PLAN.md).

pub use net::adapter::net::subnet::{
    DropReason, ForwardDecision, SubnetGateway, SubnetId, SubnetPolicy, SubnetRule,
};

// `Visibility` isn't strictly in the subnet module — it lives on
// channel config — but it pairs with subnets semantically. Re-export
// here so security-surface users get it from one place.
pub use net::adapter::net::Visibility;
