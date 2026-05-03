//! Daemon subscription ledger — tracks what a daemon has subscribed
//! to so channel bindings can be replayed on the migration target.
//!
//! See [`DAEMON_CHANNEL_REBIND_PLAN.md`](../../../../docs/DAEMON_CHANNEL_REBIND_PLAN.md)
//! for the end-to-end design. This module owns the in-memory ledger
//! (on [`DaemonHost`](super::DaemonHost)) plus the wire
//! serialization that rides inside `StateSnapshot::bindings_bytes`
//! during migration. The actual re-bind / teardown flow is driven
//! by the migration handlers (Stages 3 + 4).

use bytes::{Buf, BufMut};

use crate::adapter::net::channel::ChannelName;

/// One subscription a daemon holds on a specific publisher for a
/// specific channel. The `publisher` is a `node_id`; the `token`
/// (if present) is the serialized
/// [`PermissionToken`](crate::adapter::net::identity::PermissionToken)
/// bytes the daemon presented when it subscribed — stored as raw
/// bytes (not a typed token) so the ledger can round-trip through a
/// SDK version mismatch without re-verifying signatures source-side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubscriptionBinding {
    /// `node_id` of the publisher this subscription targets.
    pub publisher: u64,
    /// Canonical channel name. Validated via [`ChannelName`] so an
    /// attacker-crafted snapshot can't smuggle a path-traversal
    /// segment through the bindings list and escape storage sandboxes
    /// on the target.
    pub channel: ChannelName,
    /// Serialized `PermissionToken`, if the subscribe carried one.
    /// `None` on open (unauthenticated) channels.
    pub token_bytes: Option<Vec<u8>>,
}

/// Per-daemon subscription ledger — the full set the daemon held at
/// snapshot time. Written to `StateSnapshot::bindings_bytes` by
/// [`DaemonHost::take_snapshot`](super::DaemonHost::take_snapshot)
/// and replayed by the migration target handler during Restore.
///
/// `Default` = "no subscriptions," which is the correct value for
/// both (a) stateless daemons that never subscribed, and (b) v0
/// snapshots decoded by a v1 reader.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DaemonBindings {
    /// Subscriptions held at snapshot time, in insertion order.
    /// Order is advisory — re-bind replays them in this order but
    /// the migration target has to tolerate out-of-order acks from
    /// concurrent publishers, so ordering is a best-effort nicety,
    /// not a correctness property.
    pub subscriptions: Vec<SubscriptionBinding>,
}

impl DaemonBindings {
    /// Returns `true` iff the ledger is empty. Used by the snapshot
    /// writer to avoid serializing an empty `Vec` into the wire
    /// trailer — saves 4 bytes per snapshot on the common (stateless
    /// compute daemon) case.
    pub fn is_empty(&self) -> bool {
        self.subscriptions.is_empty()
    }

    /// Serialize to length-prefixed opaque bytes for storage in
    /// [`StateSnapshot::bindings_bytes`](super::super::state::snapshot::StateSnapshot::bindings_bytes).
    ///
    /// # Wire format
    ///
    /// ```text
    /// subscription_count: 4 bytes (u32 le)
    /// for each subscription:
    ///   publisher:         8 bytes (u64 le)
    ///   channel_len:       2 bytes (u16 le, max MAX_NAME_LEN = 255)
    ///   channel:           channel_len bytes (UTF-8, validated)
    ///   token_flag:        1 byte  (0 = none, 1 = present)
    ///   [token_len:        2 bytes (u16 le)]  (if token_flag == 1)
    ///   [token:            token_len bytes]   (if token_flag == 1)
    /// ```
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        let count = u32::try_from(self.subscriptions.len())
            .expect("subscription ledger exceeds 4 billion entries");
        buf.put_u32_le(count);
        for sub in &self.subscriptions {
            buf.put_u64_le(sub.publisher);
            let name = sub.channel.as_str().as_bytes();
            let name_len =
                u16::try_from(name.len()).expect("channel name validation already caps at u16");
            buf.put_u16_le(name_len);
            buf.extend_from_slice(name);
            match &sub.token_bytes {
                None => buf.put_u8(0),
                Some(tok) => {
                    buf.put_u8(1);
                    let tok_len =
                        u16::try_from(tok.len()).expect("token bytes exceed u16::MAX — caller bug");
                    buf.put_u16_le(tok_len);
                    buf.extend_from_slice(tok);
                }
            }
        }
        buf
    }

    /// Deserialize from bytes produced by [`Self::to_bytes`]. An
    /// empty slice decodes to an empty ledger (v0 snapshot
    /// compatibility). Truncation, invalid channel names, and
    /// unknown `token_flag` values all reject — this is attacker-
    /// controlled input on the target side.
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.is_empty() {
            return Some(Self::default());
        }
        let mut cursor = data;
        if cursor.remaining() < 4 {
            return None;
        }
        let count = cursor.get_u32_le() as usize;
        // Bound the pre-allocation against remaining bytes. Each
        // subscription needs at minimum 11 bytes on the wire
        // (publisher u64 + name_len u16 + zero name bytes +
        // token_flag u8). A peer claiming `count = u32::MAX`
        // would otherwise force a ~96 GiB `Vec::with_capacity`
        // allocation before any per-entry validation runs —
        // attacker-controlled DoS on the migration target. Reject
        // counts that obviously cannot be satisfied by the
        // remaining bytes.
        const MIN_BINDING_SIZE: usize = 11;
        if count > cursor.remaining() / MIN_BINDING_SIZE {
            return None;
        }
        let mut subscriptions = Vec::with_capacity(count);
        for _ in 0..count {
            if cursor.remaining() < 8 + 2 {
                return None;
            }
            let publisher = cursor.get_u64_le();
            let name_len = cursor.get_u16_le() as usize;
            if cursor.remaining() < name_len + 1 {
                return None;
            }
            let name_bytes = &cursor[..name_len];
            let name_str = std::str::from_utf8(name_bytes).ok()?;
            let channel = ChannelName::new(name_str).ok()?;
            cursor = &cursor[name_len..];
            let token_bytes = match cursor.get_u8() {
                0 => None,
                1 => {
                    if cursor.remaining() < 2 {
                        return None;
                    }
                    let tok_len = cursor.get_u16_le() as usize;
                    if cursor.remaining() < tok_len {
                        return None;
                    }
                    let bytes = cursor[..tok_len].to_vec();
                    cursor = &cursor[tok_len..];
                    Some(bytes)
                }
                _ => return None,
            };
            subscriptions.push(SubscriptionBinding {
                publisher,
                channel,
                token_bytes,
            });
        }
        // Strict length match: trailing bytes indicate a framing
        // bug or attack. Better to fail at parse than silently
        // swallow data the caller expects to consume next.
        if !cursor.is_empty() {
            return None;
        }
        Some(Self { subscriptions })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ch(s: &str) -> ChannelName {
        ChannelName::new(s).unwrap()
    }

    #[test]
    fn default_is_empty() {
        let b = DaemonBindings::default();
        assert!(b.is_empty());
        assert_eq!(b.to_bytes(), vec![0, 0, 0, 0]);
    }

    #[test]
    fn roundtrip_multi_binding() {
        let b = DaemonBindings {
            subscriptions: vec![
                SubscriptionBinding {
                    publisher: 0xAAAA_BBBB_CCCC_DDDD,
                    channel: ch("sensors/lidar"),
                    token_bytes: None,
                },
                SubscriptionBinding {
                    publisher: 0x1111_2222_3333_4444,
                    channel: ch("control/estop"),
                    token_bytes: Some(vec![0xDE, 0xAD, 0xBE, 0xEF]),
                },
            ],
        };
        let bytes = b.to_bytes();
        let decoded = DaemonBindings::from_bytes(&bytes).expect("roundtrip");
        assert_eq!(decoded, b);
    }

    #[test]
    fn empty_bytes_decode_as_empty_ledger() {
        let b = DaemonBindings::from_bytes(&[]).expect("empty = no-op decode");
        assert!(b.is_empty());
    }

    #[test]
    fn rejects_trailing_garbage() {
        let b = DaemonBindings {
            subscriptions: vec![SubscriptionBinding {
                publisher: 1,
                channel: ch("t"),
                token_bytes: None,
            }],
        };
        let mut bytes = b.to_bytes();
        bytes.push(0xFF);
        assert!(DaemonBindings::from_bytes(&bytes).is_none());
    }

    #[test]
    fn rejects_invalid_channel_name() {
        // Construct a payload with a channel name containing `..`,
        // which `ChannelName::new` rejects as a path-traversal
        // sentinel. The ledger decoder must surface this as a hard
        // rejection — attacker-controlled names never reach
        // downstream storage code.
        let mut buf = Vec::new();
        buf.put_u32_le(1);
        buf.put_u64_le(0);
        let name = b"../etc";
        buf.put_u16_le(name.len() as u16);
        buf.extend_from_slice(name);
        buf.put_u8(0);
        assert!(DaemonBindings::from_bytes(&buf).is_none());
    }

    #[test]
    fn rejects_truncated_header() {
        assert!(DaemonBindings::from_bytes(&[0, 0]).is_none());
    }

    #[test]
    fn rejects_unknown_token_flag() {
        let mut buf = Vec::new();
        buf.put_u32_le(1);
        buf.put_u64_le(0);
        let name = b"x";
        buf.put_u16_le(name.len() as u16);
        buf.extend_from_slice(name);
        buf.put_u8(0x7F); // invalid flag
        assert!(DaemonBindings::from_bytes(&buf).is_none());
    }

    /// Pin: an attacker who declares a `count` larger than the
    /// remaining bytes can support is rejected immediately —
    /// before `Vec::with_capacity(count)` can pre-allocate
    /// gigabytes of memory. Pre-fix `count = u32::MAX` would
    /// force a ~96 GiB allocation on the migration target on
    /// any peer-supplied snapshot, before any per-entry
    /// validation ran.
    #[test]
    fn rejects_count_exceeding_remaining_bytes() {
        let mut buf = Vec::new();
        // Declare a huge count but provide no entries.
        buf.put_u32_le(u32::MAX);
        // No further bytes — clearly cannot satisfy u32::MAX × 11
        // bytes of binding data.
        assert!(DaemonBindings::from_bytes(&buf).is_none());

        // Even count = 1_000_000 with only a few hundred bytes is
        // rejected, well before the per-entry parse loop.
        let mut buf = Vec::new();
        buf.put_u32_le(1_000_000);
        buf.extend_from_slice(&[0u8; 256]);
        assert!(DaemonBindings::from_bytes(&buf).is_none());

        // Boundary: a `count` consistent with the remaining bytes
        // is admitted. 1 binding requires at minimum 11 bytes
        // (8 publisher + 2 name_len + 0 name + 1 flag).
        let mut buf = Vec::new();
        buf.put_u32_le(1);
        buf.put_u64_le(0);
        buf.put_u16_le(0); // empty name — caught by ChannelName::new later
        buf.put_u8(0);
        // ChannelName::new("") rejects, so the parse fails on the
        // *name* check, not the count check — that's correct, the
        // pre-allocation gate let us through to the real
        // validation.
        let _ = DaemonBindings::from_bytes(&buf);
    }
}
