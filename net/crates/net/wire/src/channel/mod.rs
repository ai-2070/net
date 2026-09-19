//! Channel naming and the channel-membership subprotocol codec.
//!
//! Moved out of `net::adapter::net::channel` in Stage 5 of
//! `docs/internal/plans/BROWSER_NATIVE_WEBRTC_TRANSPORT_PLAN.md`. §7
//! lists "the wire-level subprotocol codecs" among what this crate
//! owns; S0a deferred the move because the codecs were not a compile
//! dependency of the wire modules, and named the trigger: "their move
//! is driven by the leaf dispatcher's needs". The leaf dispatcher
//! needs them now — a browser leaf that subscribes to a channel must
//! encode a `0x0A00` Subscribe, and it cannot link the core.
//!
//! What travels: the **name** (validation, the canonical `u64` and
//! wire `u16` hashes, `ChannelId`, `ChannelRegistry`) and the
//! **membership codec** (`Subscribe` / `Unsubscribe` / `Ack`). What
//! stays in the core: config, ACL/`AuthGuard`, the publisher and the
//! subscriber roster — policy, not wire format.
//!
//! The core re-exports both modules under their previous paths, so no
//! `use net::adapter::net::channel::…` site changed.

pub mod membership;
pub mod name;
