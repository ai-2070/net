//! Channels: subscribe and publish, through the production encoders.
//!
//! Nothing here hand-rolls a payload byte. The `0x0A00` membership
//! messages come from
//! [`net_wire::channel::membership`]
//! and the two hashes from
//! [`net_wire::channel::name`] — both moved
//! out of the core into the wire crate in Stage 5 precisely so this
//! module could reach them (plan §7: the wire-level subprotocol
//! codecs belong to the wire crate; S0a deferred the move until "the
//! leaf dispatcher's needs" drove it).
//!
//! What this module *does* own is the three derivations that make a
//! published frame land on the receiver's dispatcher:
//!
//! - [`publish_stream_id`] — `MeshNode::publish_stream_id`, bit 48
//!   packed with the canonical hash;
//! - the `u16` wire `channel_hash` stamp, which is the canonical
//!   hash truncated;
//! - [`reply_channel`] / [`request_channel`] — the deterministic
//!   nRPC channel names.
//!
//! Each is a wire contract, so each has a test that pins it.

use net_wire::channel::membership::{encode, MembershipMsg, SUBPROTOCOL_CHANNEL_MEMBERSHIP};
use net_wire::channel::name::{channel_hash, ChannelHash, ChannelName};

use crate::error::{LeafError, Result};

/// Re-exported so a caller does not need a `net_wire` dependency of
/// its own to name the subprotocol.
pub const SUBPROTOCOL_MEMBERSHIP: u16 = SUBPROTOCOL_CHANNEL_MEMBERSHIP;

/// The bit-48 discriminator on a channel-keyed publisher stream.
///
/// `MeshNode::publish_stream_id` packs the canonical hash under this
/// bit so channel streams cannot alias the low subprotocol range
/// (`0x0400..0x0A00`) that also rides as stream ids.
pub const PUBLISH_STREAM_DISCRIMINATOR: u64 = 0x0001_0000_0000_0000;

/// `MeshNode::publish_stream_id`, which is `pub(super)` in the core.
///
/// A drift here shows up as the receiver never dispatching the frame,
/// so the test below pins the formula rather than trusting it.
#[inline]
pub fn publish_stream_id(canonical: ChannelHash) -> u64 {
    PUBLISH_STREAM_DISCRIMINATOR | canonical
}

/// The `u16` the packet header carries. A fast-path filter hint;
/// ACL and storage decisions key on the canonical `u64`.
#[inline]
pub fn wire_hash(canonical: ChannelHash) -> u16 {
    canonical as u16
}

/// The nRPC request channel for a service: `<service>.requests`.
pub fn request_channel(service: &str) -> Result<ChannelName> {
    ChannelName::new(&format!("{service}.requests"))
        .map_err(|e| LeafError::Wire(format!("request channel for {service:?}: {e}")))
}

/// The nRPC reply channel for a service and this node's origin:
/// `<service>.replies.<origin:016x>`.
///
/// Origin-bound by design: `authorize_subscribe` binds a subscriber
/// to the origin named in the channel, so a peer may subscribe only
/// to the name carrying its own origin.
pub fn reply_channel(service: &str, origin_hash: u64) -> Result<ChannelName> {
    ChannelName::new(&format!("{service}.replies.{origin_hash:016x}"))
        .map_err(|e| LeafError::Wire(format!("reply channel for {service:?}: {e}")))
}

/// A channel this leaf publishes on or subscribes to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Channel {
    name: ChannelName,
    canonical: ChannelHash,
}

impl Channel {
    /// Validate a channel name and cache its canonical hash.
    pub fn new(name: &str) -> Result<Self> {
        let name = ChannelName::new(name)
            .map_err(|e| LeafError::Wire(format!("channel {name:?}: {e}")))?;
        Ok(Self::from_name(name))
    }

    /// Wrap an already-validated name.
    pub fn from_name(name: ChannelName) -> Self {
        let canonical = channel_hash(name.as_str());
        Self { name, canonical }
    }

    /// The validated name.
    #[inline]
    pub fn name(&self) -> &str {
        self.name.as_str()
    }

    /// The canonical `u64` hash — the ACL/storage key.
    #[inline]
    pub fn canonical(&self) -> ChannelHash {
        self.canonical
    }

    /// The `u16` wire hint stamped on the packet header.
    #[inline]
    pub fn wire_hash(&self) -> u16 {
        wire_hash(self.canonical)
    }

    /// The stream id a publish on this channel rides.
    #[inline]
    pub fn publish_stream_id(&self) -> u64 {
        publish_stream_id(self.canonical)
    }

    /// The `0x0A00` Subscribe payload for this channel, through the
    /// production encoder.
    ///
    /// `nonce` correlates the Ack: the receiver echoes it, which is
    /// how a leaf knows *which* subscribe was admitted or refused.
    pub fn subscribe_payload(&self, nonce: u64) -> Vec<u8> {
        encode(&MembershipMsg::Subscribe {
            channel: self.name.clone(),
            nonce,
            token: None,
            queue_group: None,
        })
    }

    /// The `0x0A00` Unsubscribe payload.
    pub fn unsubscribe_payload(&self, nonce: u64) -> Vec<u8> {
        encode(&MembershipMsg::Unsubscribe {
            channel: self.name.clone(),
            nonce,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use net_wire::channel::membership::decode;

    /// `publish_stream_id` is a wire contract with a native node.
    /// Pinned against the formula, and against the property that
    /// makes the formula necessary.
    #[test]
    fn the_publish_stream_id_is_the_bit_48_packing_of_the_canonical_hash() {
        let channel = Channel::new("sensors/lidar/front").expect("valid");
        let canonical = channel_hash("sensors/lidar/front");
        assert_eq!(channel.canonical(), canonical);
        assert_eq!(
            channel.publish_stream_id(),
            0x0001_0000_0000_0000 | canonical
        );
        assert_eq!(
            channel.wire_hash(),
            canonical as u16,
            "the header hint is the canonical hash truncated, not a second hash"
        );
        assert!(
            channel.publish_stream_id() > 0x0A00,
            "a publish stream must not alias the subprotocol id range"
        );
    }

    #[test]
    fn the_nrpc_channel_names_are_the_deterministic_ones() {
        assert_eq!(
            request_channel("net.mesh.enroll").expect("valid").as_str(),
            "net.mesh.enroll.requests"
        );
        assert_eq!(
            reply_channel("net.mesh.enroll", 0x0102_0304_0506_0708)
                .expect("valid")
                .as_str(),
            "net.mesh.enroll.replies.0102030405060708",
            "the origin is 16 lowercase hex digits, zero-padded — the \
             core's authorize_subscribe binds the subscriber to it"
        );
    }

    /// The point of the move: a leaf's Subscribe decodes with the
    /// same codec the anchor runs.
    #[test]
    fn a_subscribe_payload_round_trips_through_the_production_codec() {
        let channel = Channel::new("net.mesh.enroll.replies.0000000000000001").expect("valid");
        let payload = channel.subscribe_payload(0xFEED);
        match decode(&payload).expect("the production decoder must accept it") {
            MembershipMsg::Subscribe {
                channel: name,
                nonce,
                token,
                queue_group,
            } => {
                assert_eq!(name.as_str(), channel.name());
                assert_eq!(nonce, 0xFEED, "the nonce is what correlates the Ack");
                assert!(token.is_none(), "a leaf presents no channel token");
                assert!(queue_group.is_none(), "a leaf joins no queue group");
            }
            other => panic!("expected a Subscribe, got {other:?}"),
        }

        let payload = channel.unsubscribe_payload(1);
        assert!(matches!(
            decode(&payload).expect("decodes"),
            MembershipMsg::Unsubscribe { nonce: 1, .. }
        ));
    }

    #[test]
    fn an_invalid_channel_name_is_refused_at_construction() {
        for bad in ["", "/leading", "trailing/", "double//slash", "has spaces"] {
            assert!(
                Channel::new(bad).is_err(),
                "{bad:?} must not become a channel"
            );
        }
    }
}
