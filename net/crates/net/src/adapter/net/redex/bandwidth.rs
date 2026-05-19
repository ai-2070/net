//! Per-stream bandwidth class for the v0.3 Phase D replication
//! admission gate + send-queue priority + anti-starvation hatch.
//!
//! The class rides the [`SyncRequest`](super::replication::SyncRequest)
//! wire frame so receivers can hint sender-side priority on a
//! per-request basis. The [`ReplicationConfig`](super::replication_config::ReplicationConfig)
//! carries a per-channel default that requests inherit when their
//! wire-encoded class is absent (legacy peers) or when the caller
//! didn't explicitly override.
//!
//! This is the canonical home of the type. The blob layer's
//! `dataforts::blob::bandwidth` re-exports it alongside the
//! `dataforts:blob-bandwidth-class-supported` capability tag +
//! the [`BandwidthClassSupportProbe`](super::super::dataforts::blob::bandwidth::BandwidthClassSupportProbe)
//! downgrade hook.

/// Per-stream bandwidth class hint. Drives the v0.3 Phase D
/// admission gate (`BandwidthBudget::try_consume_with_class`) +
/// the anti-starvation hatch.
///
/// `Foreground` is the default — interactive workloads, normal
/// RPC responses, anything a person is waiting on.
///
/// `Background` is admitted only when the bucket has at least
/// `(1 - background_fraction) × capacity` available. The
/// anti-starvation hatch one-shot-bypasses the gate when
/// `Background` has been denied for > 60 s.
///
/// `Realtime` bypasses the rate-limit failure path entirely
/// (still subject to disk-pressure circuit-breakers). Reserved
/// for control-plane traffic and operator-triggered repair
/// sweeps.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum BandwidthClass {
    /// Default class — interactive / user-driven work.
    #[default]
    Foreground,
    /// TB-scale background work — backfills, migrations,
    /// cold-blob warming. Bounded by `background_fraction`.
    Background,
    /// Operator-pinned. Bypasses per-class rate budget.
    Realtime,
}

impl BandwidthClass {
    /// Wire-encoded discriminant. Pinned for backward compat:
    /// new variants must take new discriminant values; never
    /// re-purpose an existing value. Wrapping a legacy peer's
    /// missing-class trailing byte defaults to `Foreground` via
    /// [`Self::from_wire_or_default`].
    pub const FOREGROUND_WIRE: u8 = 0;
    /// See [`Self::FOREGROUND_WIRE`].
    pub const BACKGROUND_WIRE: u8 = 1;
    /// See [`Self::FOREGROUND_WIRE`].
    pub const REALTIME_WIRE: u8 = 2;

    /// Encode to the 1-byte wire form. The replication layer
    /// appends this to [`SyncRequest`](super::replication::SyncRequest)
    /// frames as a trailing byte; legacy 55-byte frames omit it
    /// entirely and are read back via [`Self::from_wire_or_default`].
    pub fn as_u8(self) -> u8 {
        match self {
            Self::Foreground => Self::FOREGROUND_WIRE,
            Self::Background => Self::BACKGROUND_WIRE,
            Self::Realtime => Self::REALTIME_WIRE,
        }
    }

    /// Decode from the 1-byte wire form. Unknown discriminants
    /// (a forward-compat scenario where a future variant lands
    /// before this reader knows about it) decode as `Foreground`
    /// — conservative degrade that keeps unknown-class requests
    /// admitted under the most permissive gate rather than
    /// silently dropped.
    pub fn from_wire_or_default(byte: u8) -> Self {
        match byte {
            Self::BACKGROUND_WIRE => Self::Background,
            Self::REALTIME_WIRE => Self::Realtime,
            // FOREGROUND_WIRE + any unknown future variant.
            _ => Self::Foreground,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_foreground() {
        assert_eq!(BandwidthClass::default(), BandwidthClass::Foreground);
    }

    #[test]
    fn wire_round_trip_for_every_variant() {
        for c in [
            BandwidthClass::Foreground,
            BandwidthClass::Background,
            BandwidthClass::Realtime,
        ] {
            assert_eq!(BandwidthClass::from_wire_or_default(c.as_u8()), c);
        }
    }

    #[test]
    fn unknown_wire_discriminant_decodes_as_foreground() {
        // A future variant with discriminant 7 should decode as
        // Foreground on this reader — conservative degrade.
        assert_eq!(
            BandwidthClass::from_wire_or_default(7),
            BandwidthClass::Foreground
        );
        assert_eq!(
            BandwidthClass::from_wire_or_default(255),
            BandwidthClass::Foreground
        );
    }
}
