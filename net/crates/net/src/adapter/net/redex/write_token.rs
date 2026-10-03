//! `WriteToken` — typed handle to a specific write on a specific
//! origin's chain, in a specific channel. Returned by event-ingest paths
//! and by the typed adapters' `token(seq)`; consumed by read-your-writes
//! wait primitives.
//!
//! ## Why the channel is part of the token
//!
//! Sequence numbers are per channel (per RedEX file). Two adapters with
//! the same origin over different channels — `cortex/tasks` and
//! `cortex/memories`, say — count independently, so a token carrying only
//! `(origin, seq)` could be satisfied by the *other* channel's fold
//! reaching that number, without the write ever being applied. The token
//! therefore carries the channel's canonical hash
//! ([`ChannelName::hash`](crate::adapter::net::channel::ChannelName::hash)),
//! and every adapter refuses a token for another channel with
//! `WaitForTokenError::WrongChannel`.
//!
//! The substrate uses a 64-bit `origin_hash` throughout (see
//! `identity::entity::EntityKeypair::origin_hash`). An earlier draft
//! of the Dataforts plan speculated a 32-byte origin; the substrate
//! shape wins because every causal-chain primitive already keys on
//! `u64`.
//!
//! ## Threat model
//!
//! `WriteToken` is **plain in-process data**, not a signed
//! capability. The fields are `pub` so the FFI / binding layer can
//! marshal them without going through `serde`; in-process callers
//! can construct any token they want. RYW guarantees are upheld by
//! the *adapter*: `TasksAdapter::wait_for_token` and
//! `MemoriesAdapter::wait_for_token` reject tokens whose
//! `origin_hash` doesn't match the adapter's own bound origin
//! ([`super::cortex::WaitForTokenError::WrongOrigin`]).
//!
//! This means the trust boundary is the adapter handle, not the
//! token. A caller holding a `TasksAdapter` bound to origin X
//! cannot use it to learn anything about origin Y by forging a
//! token — `wait_for_token` will reject and `note_wrong_origin`
//! will trip the metric. Tokens crossing the wire (e.g. encoded
//! into another origin's payload) must be treated as untrusted
//! input; the receiving side validates via the adapter binding
//! before honoring them.

use std::fmt;
use std::str::FromStr;

/// Address of a write — origin (which chain), channel (which RedEX
/// file) and seq (which event in that channel). Sequence numbers are per
/// channel, so all three are its identity. Round-trips through every
/// binding as a typed value.
///
/// Treat tokens as **opaque, in-process data**. They are not
/// signed. The fields are `pub` so FFI / binding layers can
/// marshal them; application code should not synthesise tokens.
/// See module-level docs for the trust model — adapters reject a
/// token from another origin or another channel at `wait_for_token`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct WriteToken {
    /// 64-bit hash of the entity whose chain this write landed on
    /// (`EntityKeypair::origin_hash`).
    pub origin_hash: u64,
    /// Canonical hash of the channel the write was appended to
    /// ([`ChannelName::hash`](crate::adapter::net::channel::ChannelName::hash)).
    /// `seq` is meaningful only within this channel.
    pub channel_hash: u64,
    /// Per-channel monotonic sequence assigned by `RedexFile::append`.
    pub seq: u64,
}

impl WriteToken {
    /// Construct a token from its components. **Not for application
    /// use** — the ingest path and the typed adapters' `token(seq)`
    /// return tokens with the right `(origin_hash, channel_hash, seq)`
    /// already attached. Exposed only so FFI / binding layers can
    /// marshal tokens across the language boundary. Hand-rolled tokens
    /// that don't match an actually-issued write produce waits that hang
    /// until the deadline, or that `WrongOrigin` / `WrongChannel` reject.
    #[doc(hidden)]
    pub const fn new(origin_hash: u64, channel_hash: u64, seq: u64) -> Self {
        Self {
            origin_hash,
            channel_hash,
            seq,
        }
    }
}

/// `<origin_hex>:<channel_hex>:<seq>` — both hashes as 16 lowercase hex
/// characters, the seq in decimal. Chosen for grep-ability against the
/// `causal:<hex>:<seq>` reserved-prefix tag shape. Stable form for
/// log/CLI surfaces; bindings serialise the struct directly instead.
impl fmt::Display for WriteToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:016x}:{:016x}:{}",
            self.origin_hash, self.channel_hash, self.seq
        )
    }
}

/// Errors returned by [`WriteToken::from_str`].
#[derive(Debug, PartialEq, Eq)]
pub enum WriteTokenParseError {
    /// Input was not exactly three `:`-separated parts.
    MissingSeparator,
    /// Origin portion was not 16 hex characters.
    BadOrigin,
    /// Channel portion was not 16 hex characters.
    BadChannel,
    /// Seq portion did not parse as a decimal `u64`.
    BadSeq,
}

impl fmt::Display for WriteTokenParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingSeparator => f.write_str("expected `<origin_hex>:<channel_hex>:<seq>`"),
            Self::BadOrigin => f.write_str("origin must be 16 hex chars"),
            Self::BadChannel => f.write_str("channel must be 16 hex chars"),
            Self::BadSeq => f.write_str("seq must be a decimal u64"),
        }
    }
}

impl std::error::Error for WriteTokenParseError {}

impl FromStr for WriteToken {
    type Err = WriteTokenParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut parts = s.split(':');
        let (Some(origin_str), Some(channel_str), Some(seq_str), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(WriteTokenParseError::MissingSeparator);
        };
        let origin_hash = parse_hex16(origin_str).ok_or(WriteTokenParseError::BadOrigin)?;
        let channel_hash = parse_hex16(channel_str).ok_or(WriteTokenParseError::BadChannel)?;
        let seq: u64 = seq_str.parse().map_err(|_| WriteTokenParseError::BadSeq)?;
        Ok(Self::new(origin_hash, channel_hash, seq))
    }
}

/// Exactly 16 hex digits (no sign, no prefix) as a `u64`.
fn parse_hex16(s: &str) -> Option<u64> {
    if s.len() != 16 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u64::from_str_radix(s, 16).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_pads_both_hashes_to_16_hex() {
        let token = WriteToken::new(0xDEAD_BEEF, 0xC0FFEE, 42);
        assert_eq!(token.to_string(), "00000000deadbeef:0000000000c0ffee:42");
    }

    #[test]
    fn display_round_trips_via_from_str() {
        let token = WriteToken::new(0x0123_4567_89AB_CDEF, 0xFEDC_BA98_7654_3210, 12345);
        let parsed: WriteToken = token.to_string().parse().unwrap();
        assert_eq!(parsed, token);
    }

    #[test]
    fn from_str_requires_exactly_three_parts() {
        for s in [
            "deadbeef",
            // The old two-part form is refused, not read with a zero channel.
            "00000000deadbeef:1",
            "00000000deadbeef:0000000000000001:1:9",
        ] {
            assert_eq!(
                s.parse::<WriteToken>(),
                Err(WriteTokenParseError::MissingSeparator),
                "{s}"
            );
        }
    }

    #[test]
    fn from_str_rejects_bad_origin() {
        for s in [
            "deadbeef:0000000000000001:1",
            "zzzzzzzzzzzzzzzz:0000000000000001:1",
        ] {
            assert_eq!(
                s.parse::<WriteToken>(),
                Err(WriteTokenParseError::BadOrigin),
                "{s}"
            );
        }
    }

    #[test]
    fn from_str_rejects_bad_channel() {
        for s in [
            "0000000000000001:01:1",
            "0000000000000001:+000000000000001:1",
            "0000000000000001:zzzzzzzzzzzzzzzz:1",
        ] {
            assert_eq!(
                s.parse::<WriteToken>(),
                Err(WriteTokenParseError::BadChannel),
                "{s}"
            );
        }
    }

    #[test]
    fn from_str_rejects_bad_seq() {
        assert_eq!(
            "0000000000000001:0000000000000002:-1".parse::<WriteToken>(),
            Err(WriteTokenParseError::BadSeq)
        );
    }

    #[test]
    fn equality_is_componentwise() {
        let a = WriteToken::new(1, 7, 2);
        assert_eq!(a, WriteToken::new(1, 7, 2));
        assert_ne!(a, WriteToken::new(1, 7, 3));
        assert_ne!(a, WriteToken::new(2, 7, 2));
        assert_ne!(
            a,
            WriteToken::new(1, 8, 2),
            "the channel is part of the identity"
        );
    }

    #[test]
    fn token_is_copy() {
        fn assert_copy<T: Copy>() {}
        assert_copy::<WriteToken>();
    }
}
