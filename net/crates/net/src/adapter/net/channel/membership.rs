//! Channel membership subprotocol — Subscribe / Unsubscribe / Ack.
//!
//! Ships over `SUBPROTOCOL_CHANNEL_MEMBERSHIP` on existing encrypted
//! sessions. Carries the channel name (not just the u16 hash) so that
//! the publisher-side `ChannelConfig::can_subscribe` check can look up
//! the authoritative config by name — hash collisions must never cause
//! a subscribe to land on the wrong channel's ACL.

use bytes::{Buf, BufMut};

use super::name::{ChannelError, ChannelName};

/// Subprotocol ID for channel membership (subscribe / unsubscribe / ack).
pub const SUBPROTOCOL_CHANNEL_MEMBERSHIP: u16 = 0x0A00;

const MSG_SUBSCRIBE: u8 = 0;
const MSG_UNSUBSCRIBE: u8 = 1;
const MSG_ACK: u8 = 2;

const ACK_REASON_OK: u8 = 0;
const ACK_REASON_UNAUTHORIZED: u8 = 1;
const ACK_REASON_UNKNOWN_CHANNEL: u8 = 2;
const ACK_REASON_RATE_LIMITED: u8 = 3;
const ACK_REASON_TOO_MANY_CHANNELS: u8 = 4;

/// Why a `Subscribe` or `Unsubscribe` was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AckReason {
    /// Capability or token check failed.
    Unauthorized,
    /// Channel not registered on the publisher side.
    UnknownChannel,
    /// Membership churn throttled.
    RateLimited,
    /// Per-peer channel cap exceeded.
    TooManyChannels,
}

/// Channel membership wire message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MembershipMsg {
    /// Ask the publisher to add this node to `channel`'s subscriber set.
    Subscribe {
        /// Channel the sender wants to subscribe to.
        channel: ChannelName,
        /// Request correlation nonce — echoed back in `Ack`.
        nonce: u64,
        /// Serialized [`super::super::identity::PermissionToken`]
        /// presented alongside the subscribe request. `None` / empty
        /// when the sender has no token to offer — the publisher's
        /// `authorize_subscribe` decides whether a token is required.
        token: Option<Vec<u8>>,
        /// Subscription mode. `None` → `Broadcast` (every published
        /// event delivered to this subscriber, the historic
        /// pub/sub semantic). `Some(name)` → `QueueGroup(name)`
        /// (work-distribution: every published event delivered to
        /// exactly ONE subscriber in the named group). The
        /// publisher's `authorize_subscribe` is mode-agnostic — the
        /// capability tokens that gate the channel apply equally.
        ///
        /// Wire-compat: encoded as a `u8` length prefix + UTF-8
        /// bytes after the token. Length `0` (or absent trailing
        /// bytes — pre-queue-group senders) means `Broadcast`.
        queue_group: Option<String>,
    },
    /// Ask the publisher to remove this node from `channel`'s subscriber set.
    Unsubscribe {
        /// Channel the sender wants to unsubscribe from.
        channel: ChannelName,
        /// Request correlation nonce — echoed back in `Ack`.
        nonce: u64,
    },
    /// Acknowledgement for a prior Subscribe / Unsubscribe.
    Ack {
        /// Nonce of the request being acknowledged.
        nonce: u64,
        /// Whether the request was accepted.
        accepted: bool,
        /// If rejected, why.
        reason: Option<AckReason>,
    },
}

/// Error returned by the membership codec.
#[derive(Debug, thiserror::Error)]
pub enum MembershipCodecError {
    /// Unknown or reserved message-type byte.
    #[error("unknown membership message type: {0}")]
    UnknownType(u8),
    /// Buffer ended mid-field.
    #[error("truncated membership message: {0}")]
    Truncated(&'static str),
    /// Channel name failed validation.
    #[error("channel name: {0}")]
    Name(#[from] ChannelError),
    /// Length prefix exceeds the remaining buffer.
    #[error("length {0} exceeds remaining {1}")]
    Overflow(usize, usize),
    /// Length prefix exceeds the declared max.
    #[error("channel name length {0} exceeds limit {1}")]
    NameTooLong(usize, usize),
}

/// Maximum channel-name length accepted by the decoder, in bytes.
/// Matches `name::MAX_NAME_LEN`; duplicated here to keep the wire check local.
const MAX_CHANNEL_NAME_LEN: usize = 255;

/// Maximum queue-group name length, in bytes. Bounded by the u8
/// length prefix in the wire format. Long names work but bloat
/// every Subscribe; recommend keeping group names short and
/// human-readable.
const MAX_QUEUE_GROUP_NAME_LEN: usize = 255;

/// Encode a membership message to bytes.
pub fn encode(msg: &MembershipMsg) -> Vec<u8> {
    let mut buf = Vec::with_capacity(64);
    match msg {
        MembershipMsg::Subscribe {
            channel,
            nonce,
            token,
            queue_group,
        } => {
            buf.put_u8(MSG_SUBSCRIBE);
            buf.put_u64_le(*nonce);
            let name = channel.as_str().as_bytes();
            buf.put_u8(name.len() as u8);
            buf.extend_from_slice(name);
            // Token payload: u16_le length + bytes. Zero length when
            // unset — decoder treats absent trailing bytes as no
            // token, for forward-compat with a potential pre-E-1
            // sender (none exist in practice but the cost is ~nil).
            let token_bytes: &[u8] = token.as_deref().unwrap_or(&[]);
            buf.put_u16_le(token_bytes.len() as u16);
            buf.extend_from_slice(token_bytes);
            // Queue-group payload: u8 length + UTF-8 bytes. Zero
            // length means `Broadcast`. Pre-queue-group senders that
            // stop after the token are decoded as Broadcast (zero
            // remaining → None) for forward compat. Names are bound
            // by `MAX_QUEUE_GROUP_NAME_LEN` to keep the prefix u8.
            let qg_bytes: &[u8] = queue_group.as_deref().map(|s| s.as_bytes()).unwrap_or(&[]);
            buf.put_u8(qg_bytes.len() as u8);
            buf.extend_from_slice(qg_bytes);
        }
        MembershipMsg::Unsubscribe { channel, nonce } => {
            buf.put_u8(MSG_UNSUBSCRIBE);
            buf.put_u64_le(*nonce);
            let name = channel.as_str().as_bytes();
            buf.put_u8(name.len() as u8);
            buf.extend_from_slice(name);
        }
        MembershipMsg::Ack {
            nonce,
            accepted,
            reason,
        } => {
            buf.put_u8(MSG_ACK);
            buf.put_u64_le(*nonce);
            buf.put_u8(u8::from(*accepted));
            buf.put_u8(match reason {
                None => ACK_REASON_OK,
                Some(AckReason::Unauthorized) => ACK_REASON_UNAUTHORIZED,
                Some(AckReason::UnknownChannel) => ACK_REASON_UNKNOWN_CHANNEL,
                Some(AckReason::RateLimited) => ACK_REASON_RATE_LIMITED,
                Some(AckReason::TooManyChannels) => ACK_REASON_TOO_MANY_CHANNELS,
            });
        }
    }
    buf
}

/// Decode a membership message from bytes.
pub fn decode(data: &[u8]) -> Result<MembershipMsg, MembershipCodecError> {
    if data.is_empty() {
        return Err(MembershipCodecError::Truncated("empty"));
    }
    let mut cur = std::io::Cursor::new(data);
    let tag = cur.get_u8();
    match tag {
        MSG_SUBSCRIBE | MSG_UNSUBSCRIBE => {
            if cur.remaining() < 9 {
                return Err(MembershipCodecError::Truncated("subscribe header"));
            }
            let nonce = cur.get_u64_le();
            let name_len = cur.get_u8() as usize;
            if name_len == 0 {
                return Err(MembershipCodecError::Truncated("empty channel name"));
            }
            if name_len > MAX_CHANNEL_NAME_LEN {
                return Err(MembershipCodecError::NameTooLong(
                    name_len,
                    MAX_CHANNEL_NAME_LEN,
                ));
            }
            if cur.remaining() < name_len {
                return Err(MembershipCodecError::Overflow(name_len, cur.remaining()));
            }
            let start = cur.position() as usize;
            let end = start + name_len;
            let name_bytes = &data[start..end];
            let name_str = std::str::from_utf8(name_bytes)
                .map_err(|_| MembershipCodecError::Truncated("non-utf8 channel name"))?;
            let channel = ChannelName::new(name_str)?;
            if tag == MSG_SUBSCRIBE {
                // Advance past the name we just read.
                cur.set_position(end as u64);
                // Token: u16_le length + bytes. Zero length ⇒ absent.
                // Legacy pre-E-1 payloads that stop exactly after the
                // name (zero trailing bytes) are treated as "no token"
                // for forward-compat. Exactly one trailing byte is
                // neither — it means a malformed sender wrote half
                // the length prefix, and the older
                // `cur.remaining() < 2` check silently accepted it as
                // "no token," hiding the bug from callers. Reject so
                // truncation surfaces as an error.
                let token = match cur.remaining() {
                    0 => None,
                    1 => {
                        return Err(MembershipCodecError::Truncated(
                            "subscribe token length prefix",
                        ));
                    }
                    _ => {
                        let token_len = cur.get_u16_le() as usize;
                        if token_len == 0 {
                            None
                        } else if cur.remaining() < token_len {
                            return Err(MembershipCodecError::Overflow(token_len, cur.remaining()));
                        } else {
                            let tstart = cur.position() as usize;
                            let tend = tstart + token_len;
                            // Advance cur past the token bytes so
                            // the trailing-byte check below operates
                            // against the actual unconsumed remainder
                            // rather than seeing the token as
                            // "trailing".
                            cur.set_position(tend as u64);
                            Some(data[tstart..tend].to_vec())
                        }
                    }
                };
                // Queue-group: u8 length + UTF-8 bytes. Forward-
                // compat with pre-queue-group senders: zero
                // remaining bytes after the token decodes as
                // `Broadcast` (queue_group = None). A non-zero
                // length but a malformed (non-UTF-8) name surfaces
                // as a decode error rather than a silent acceptance.
                let queue_group = match cur.remaining() {
                    0 => None,
                    _ => {
                        let qg_len = cur.get_u8() as usize;
                        if qg_len == 0 {
                            None
                        } else if qg_len > MAX_QUEUE_GROUP_NAME_LEN {
                            return Err(MembershipCodecError::NameTooLong(
                                qg_len,
                                MAX_QUEUE_GROUP_NAME_LEN,
                            ));
                        } else if cur.remaining() < qg_len {
                            return Err(MembershipCodecError::Overflow(qg_len, cur.remaining()));
                        } else {
                            let qstart = cur.position() as usize;
                            let qend = qstart + qg_len;
                            cur.set_position(qend as u64);
                            let s = std::str::from_utf8(&data[qstart..qend]).map_err(|_| {
                                MembershipCodecError::Truncated(
                                    "non-utf8 subscribe queue-group name",
                                )
                            })?;
                            Some(s.to_string())
                        }
                    }
                };
                // Strict-trailer rejection after the queue-group
                // bytes (the new outermost optional field). Pre-
                // queue-group, this guarded against arbitrary
                // garbage after the token; the guard moves outward
                // by one field.
                if cur.remaining() != 0 {
                    return Err(MembershipCodecError::Truncated(
                        "trailing bytes after subscribe queue-group",
                    ));
                }
                Ok(MembershipMsg::Subscribe {
                    channel,
                    nonce,
                    token,
                    queue_group,
                })
            } else {
                // Advance cur past the channel name we read by
                // direct slice (the SUBSCRIBE branch above does
                // this via `cur.set_position(end as u64)`; we
                // mirror that here so the trailing-byte check
                // below is meaningful).
                cur.set_position(end as u64);
                // Pre-fix UNSUBSCRIBE returned Ok without
                // checking that the buffer was fully consumed. A
                // malformed peer could append arbitrary bytes
                // after a valid Unsubscribe and the decoder
                // accepted it, hiding upstream framer bugs.
                if cur.remaining() != 0 {
                    return Err(MembershipCodecError::Truncated(
                        "trailing bytes after unsubscribe",
                    ));
                }
                Ok(MembershipMsg::Unsubscribe { channel, nonce })
            }
        }
        MSG_ACK => {
            if cur.remaining() < 10 {
                return Err(MembershipCodecError::Truncated("ack"));
            }
            let nonce = cur.get_u64_le();
            // Strict boolean: reject any byte other than 0 or 1 instead of
            // treating every non-zero value as "accepted". Prevents a
            // malformed sender from making an otherwise-unknown reason
            // code silently imply acceptance.
            let accepted = match cur.get_u8() {
                0 => false,
                1 => true,
                other => return Err(MembershipCodecError::UnknownType(other)),
            };
            let reason_byte = cur.get_u8();
            let reason = match reason_byte {
                ACK_REASON_OK => None,
                ACK_REASON_UNAUTHORIZED => Some(AckReason::Unauthorized),
                ACK_REASON_UNKNOWN_CHANNEL => Some(AckReason::UnknownChannel),
                ACK_REASON_RATE_LIMITED => Some(AckReason::RateLimited),
                ACK_REASON_TOO_MANY_CHANNELS => Some(AckReason::TooManyChannels),
                other => return Err(MembershipCodecError::UnknownType(other)),
            };
            // Same strict-trailer rejection on the ACK
            // path.
            if cur.remaining() != 0 {
                return Err(MembershipCodecError::Truncated("trailing bytes after ack"));
            }
            Ok(MembershipMsg::Ack {
                nonce,
                accepted,
                reason,
            })
        }
        other => Err(MembershipCodecError::UnknownType(other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ch(name: &str) -> ChannelName {
        ChannelName::new(name).unwrap()
    }

    #[test]
    fn test_roundtrip_subscribe_no_token() {
        let msg = MembershipMsg::Subscribe {
            channel: ch("sensors/lidar"),
            nonce: 0xDEAD_BEEF_CAFE_F00D,
            token: None,
            queue_group: None,
        };
        let bytes = encode(&msg);
        let decoded = decode(&bytes).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn test_roundtrip_subscribe_with_token() {
        // Arbitrary token bytes — codec doesn't validate internal
        // structure. Validation is the job of `PermissionToken`.
        let token_bytes = vec![0xABu8; 64];
        let msg = MembershipMsg::Subscribe {
            channel: ch("sensors/lidar"),
            nonce: 0xCAFE,
            token: Some(token_bytes),
            queue_group: None,
        };
        let bytes = encode(&msg);
        let decoded = decode(&bytes).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn test_legacy_subscribe_no_trailing_token_len_decodes_as_none() {
        // Forge a pre-E-1 payload (no u16 token_len trailer).
        use bytes::BufMut;
        let mut buf = Vec::new();
        buf.put_u8(MSG_SUBSCRIBE);
        buf.put_u64_le(42);
        let name = b"lab/x";
        buf.put_u8(name.len() as u8);
        buf.extend_from_slice(name);
        // NO token_len field — stops right after the name.
        let decoded = decode(&buf).unwrap();
        assert_eq!(
            decoded,
            MembershipMsg::Subscribe {
                channel: ch("lab/x"),
                nonce: 42,
                token: None,
                queue_group: None,
            }
        );
    }

    #[test]
    fn test_regression_subscribe_one_byte_token_len_rejected() {
        // Regression for a cubic-flagged P2: a Subscribe payload with
        // exactly one trailing byte after the name used to be silently
        // accepted as "no token" because the decoder guarded on
        // `remaining() < 2`. A half-written `u16_le` length prefix is
        // a truncation, not a legacy payload — it must error.
        use bytes::BufMut;
        let mut buf = Vec::new();
        buf.put_u8(MSG_SUBSCRIBE);
        buf.put_u64_le(42);
        let name = b"lab/x";
        buf.put_u8(name.len() as u8);
        buf.extend_from_slice(name);
        // Exactly ONE trailing byte — half of a u16_le length prefix.
        buf.push(0x05);
        let err = decode(&buf).unwrap_err();
        assert!(
            matches!(err, MembershipCodecError::Truncated(_)),
            "expected Truncated, got {err:?}",
        );
    }

    #[test]
    fn test_roundtrip_unsubscribe() {
        let msg = MembershipMsg::Unsubscribe {
            channel: ch("control/estop"),
            nonce: 42,
        };
        let bytes = encode(&msg);
        let decoded = decode(&bytes).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn test_roundtrip_ack_accepted() {
        let msg = MembershipMsg::Ack {
            nonce: 7,
            accepted: true,
            reason: None,
        };
        let bytes = encode(&msg);
        let decoded = decode(&bytes).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn test_roundtrip_ack_rejected() {
        let reasons = [
            AckReason::Unauthorized,
            AckReason::UnknownChannel,
            AckReason::RateLimited,
            AckReason::TooManyChannels,
        ];
        for r in reasons {
            let msg = MembershipMsg::Ack {
                nonce: 99,
                accepted: false,
                reason: Some(r),
            };
            let bytes = encode(&msg);
            let decoded = decode(&bytes).unwrap();
            assert_eq!(decoded, msg);
        }
    }

    #[test]
    fn test_decode_empty_fails() {
        assert!(matches!(
            decode(&[]),
            Err(MembershipCodecError::Truncated(_))
        ));
    }

    #[test]
    fn test_decode_unknown_tag() {
        assert!(matches!(
            decode(&[0xFF]),
            Err(MembershipCodecError::UnknownType(0xFF))
        ));
    }

    #[test]
    fn test_decode_truncated_subscribe() {
        // Tag + partial nonce only.
        assert!(matches!(
            decode(&[MSG_SUBSCRIBE, 0, 0, 0]),
            Err(MembershipCodecError::Truncated(_))
        ));
    }

    #[test]
    fn test_decode_zero_name_len_rejected() {
        let mut buf = vec![MSG_SUBSCRIBE];
        buf.extend_from_slice(&0u64.to_le_bytes());
        buf.push(0); // name_len = 0
        assert!(matches!(
            decode(&buf),
            Err(MembershipCodecError::Truncated(_))
        ));
    }

    #[test]
    fn test_decode_overflow_name_len() {
        let mut buf = vec![MSG_SUBSCRIBE];
        buf.extend_from_slice(&0u64.to_le_bytes());
        buf.push(10); // claims 10 bytes but we only have 3
        buf.extend_from_slice(b"abc");
        assert!(matches!(
            decode(&buf),
            Err(MembershipCodecError::Overflow(10, 3))
        ));
    }

    #[test]
    fn test_decode_ack_strict_boolean_rejects_non_01() {
        // Valid ack with accepted=true (0x01), reason=OK — sanity check.
        let mut buf = vec![MSG_ACK];
        buf.extend_from_slice(&7u64.to_le_bytes());
        buf.push(1);
        buf.push(ACK_REASON_OK);
        assert!(decode(&buf).is_ok());

        // Same message but accepted=0xFF — must be rejected, not treated
        // as `true`.
        let mut buf = vec![MSG_ACK];
        buf.extend_from_slice(&7u64.to_le_bytes());
        buf.push(0xFF);
        buf.push(ACK_REASON_OK);
        assert!(matches!(
            decode(&buf),
            Err(MembershipCodecError::UnknownType(0xFF))
        ));
    }

    #[test]
    fn test_decode_invalid_channel_name() {
        let mut buf = vec![MSG_SUBSCRIBE];
        buf.extend_from_slice(&0u64.to_le_bytes());
        // name contains '//' which fails validation
        let name = b"a//b";
        buf.push(name.len() as u8);
        buf.extend_from_slice(name);
        assert!(matches!(decode(&buf), Err(MembershipCodecError::Name(_))));
    }

    /// Trailing bytes after a valid UNSUBSCRIBE must be
    /// rejected. Pre-fix the decoder returned Ok without checking
    /// `cur.remaining() == 0`, so a malformed peer could append
    /// garbage that hid upstream framer bugs.
    #[test]
    fn unsubscribe_with_trailing_bytes_is_rejected() {
        let msg = MembershipMsg::Unsubscribe {
            channel: ch("control/estop"),
            nonce: 42,
        };
        let mut bytes = encode(&msg);
        bytes.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
        let err = decode(&bytes).unwrap_err();
        assert!(
            matches!(err, MembershipCodecError::Truncated(s) if s.contains("unsubscribe")),
            "expected trailing-after-unsubscribe error, got {:?}",
            err
        );
    }

    /// Same strict-trailer rejection on the ACK path.
    #[test]
    fn ack_with_trailing_bytes_is_rejected() {
        let msg = MembershipMsg::Ack {
            nonce: 42,
            accepted: true,
            reason: None,
        };
        let mut bytes = encode(&msg);
        bytes.push(0xAA);
        let err = decode(&bytes).unwrap_err();
        assert!(
            matches!(err, MembershipCodecError::Truncated(s) if s.contains("ack")),
            "expected trailing-after-ack error, got {:?}",
            err
        );
    }

    /// SUBSCRIBE-with-token must reject trailing bytes
    /// after the token. Pre-fix this was the SUBSCRIBE path's
    /// equivalent gap.
    #[test]
    fn subscribe_with_token_then_trailing_bytes_is_rejected() {
        let msg = MembershipMsg::Subscribe {
            channel: ch("sensors/lidar"),
            nonce: 0xCAFE,
            token: Some(vec![0xAB; 32]),
            queue_group: None,
        };
        let mut bytes = encode(&msg);
        // Append two arbitrary bytes that aren't a valid
        // queue_group prefix-length+payload (the first is read as
        // qg_len=0xDE which then demands 0xDE bytes that aren't
        // there).
        bytes.extend_from_slice(&[0xDE, 0xAD]);
        let err = decode(&bytes).unwrap_err();
        // After the queue-group field landed on the wire, trailing
        // bytes after a valid token are interpreted as the start
        // of the queue-group field. The first byte (`0xDE`) is
        // read as `qg_len = 222`; the second byte is then short of
        // the demanded payload, so the decoder errors with
        // `Overflow`. Pre-queue-group, the same bytes were
        // rejected as `Truncated("trailing bytes after subscribe
        // token")`. Either error proves the load-bearing property:
        // arbitrary garbage past a valid Subscribe is NOT silently
        // accepted.
        assert!(
            matches!(
                err,
                MembershipCodecError::Truncated(_) | MembershipCodecError::Overflow(_, _)
            ),
            "expected trailing-after-subscribe rejection (Truncated or Overflow), got {:?}",
            err
        );
    }

    // ====================================================================
    // SUBSCRIBE queue-group field — wire-format extension.
    //
    // The codec's forward-compat property: pre-queue-group senders
    // (no trailing bytes after the token) decode as Broadcast
    // (queue_group = None). New senders that include the field
    // round-trip cleanly.
    // ====================================================================

    /// SUBSCRIBE with a queue group set round-trips through
    /// encode/decode unchanged.
    #[test]
    fn subscribe_queue_group_roundtrip() {
        let msg = MembershipMsg::Subscribe {
            channel: ch("svc/req"),
            nonce: 7,
            token: None,
            queue_group: Some("workers".to_string()),
        };
        let bytes = encode(&msg);
        let decoded = decode(&bytes).unwrap();
        assert_eq!(decoded, msg);
    }

    /// SUBSCRIBE with both a token AND a queue group round-trips.
    /// Pin the field ordering (token then queue group) so a
    /// future re-ordering tickles the test.
    #[test]
    fn subscribe_token_and_queue_group_roundtrip() {
        let msg = MembershipMsg::Subscribe {
            channel: ch("svc/req"),
            nonce: 11,
            token: Some(vec![0xAB; 80]),
            queue_group: Some("workers-pool-a".to_string()),
        };
        let bytes = encode(&msg);
        let decoded = decode(&bytes).unwrap();
        assert_eq!(decoded, msg);
    }

    /// SUBSCRIBE with `queue_group: None` is byte-equivalent to a
    /// pre-queue-group SUBSCRIBE that stops right after the token.
    /// Both encode to (token+0x00) trailing the channel name; the
    /// decoder treats `qg_len = 0` and "no remaining bytes" as
    /// the same Broadcast-default outcome.
    #[test]
    fn subscribe_pre_queue_group_payload_decodes_as_broadcast() {
        // Forge a wire payload that stops after the token (no qg
        // byte) — this is what a pre-queue-group sender produces.
        use bytes::BufMut;
        let mut buf = Vec::new();
        buf.put_u8(MSG_SUBSCRIBE);
        buf.put_u64_le(99);
        let name = b"sensors/lidar";
        buf.put_u8(name.len() as u8);
        buf.extend_from_slice(name);
        // Token of length 0.
        buf.put_u16_le(0);
        // No qg byte at all — matches a pre-queue-group sender.
        let decoded = decode(&buf).unwrap();
        assert_eq!(
            decoded,
            MembershipMsg::Subscribe {
                channel: ch("sensors/lidar"),
                nonce: 99,
                token: None,
                queue_group: None,
            },
            "pre-queue-group payload (no trailing bytes after token) \
             must decode as Broadcast",
        );
    }

    /// A queue-group name that exceeds `MAX_QUEUE_GROUP_NAME_LEN`
    /// in the wire-format invariant `qg_len <= u8::MAX` is
    /// structurally impossible (the prefix can't encode a longer
    /// length). But a malformed sender that writes
    /// `qg_len > remaining` must surface as `Overflow`. Pin so a
    /// future change can't silently accept short reads.
    #[test]
    fn subscribe_queue_group_overflow_is_rejected() {
        use bytes::BufMut;
        let mut buf = Vec::new();
        buf.put_u8(MSG_SUBSCRIBE);
        buf.put_u64_le(1);
        let name = b"svc/req";
        buf.put_u8(name.len() as u8);
        buf.extend_from_slice(name);
        buf.put_u16_le(0); // no token
                           // Claim a 200-byte queue-group name but only provide 5.
        buf.put_u8(200);
        buf.extend_from_slice(b"short");
        let err = decode(&buf).unwrap_err();
        assert!(
            matches!(err, MembershipCodecError::Overflow(claimed, remaining) if claimed == 200 && remaining == 5),
            "expected Overflow(200, 5), got {:?}",
            err,
        );
    }

    /// Non-UTF-8 bytes in the queue-group payload are rejected.
    #[test]
    fn subscribe_queue_group_non_utf8_is_rejected() {
        use bytes::BufMut;
        let mut buf = Vec::new();
        buf.put_u8(MSG_SUBSCRIBE);
        buf.put_u64_le(1);
        let name = b"svc/req";
        buf.put_u8(name.len() as u8);
        buf.extend_from_slice(name);
        buf.put_u16_le(0);
        buf.put_u8(2);
        buf.extend_from_slice(&[0xFF, 0xFE]); // not valid UTF-8 in this position
        let err = decode(&buf).unwrap_err();
        assert!(
            matches!(err, MembershipCodecError::Truncated(s) if s.contains("non-utf8")),
            "expected non-utf8 rejection, got {:?}",
            err,
        );
    }
}
