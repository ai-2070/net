//! The network-change re-attempt owner's **policy** — one owner, one
//! absolute deadline, one re-attempt per network change.
//!
//! Plan §10 / Stage 6 slice 3: `navigator.onLine` / `online` events
//! **and** an ICE `disconnected` → `failed` transition drive a bounded
//! re-attempt through the same production owner, with Stage 4a's
//! `spawn_dialog_completion` semantics on the leaf side — one absolute
//! deadline, retire before install.
//!
//! # Why this is its own module, and native
//!
//! Two reasons, and they are the same reason.
//!
//! The defect this row exists to catch is **a trigger that fires
//! twice on one network change starting two re-attempts.** That is a
//! decision rule, not a browser behaviour: it is about what the leaf
//! does with the second trigger, whichever source produced it. Rules
//! are worth asserting exhaustively, and a rule that lives in
//! [`crate::rtc`] or [`crate::wasm`] cannot be — both are
//! `#![cfg(target_arch = "wasm32")]`, so the only test that could
//! reach it would need a real browser, a real network change and a
//! real ICE agent, and would then assert one path through the rule
//! per run.
//!
//! And the two sources must not be two paths. A browser hands the
//! same network change to a page as several observations — `offline`,
//! then `online`, and separately an ICE agent walking
//! `connected → disconnected → failed` — with no ordering guarantee
//! and no promise that all of them arrive. So every source files a
//! trigger and **this** decides, once, per peer, per episode. Two
//! sources that each decided for themselves would be two owners, and
//! two owners of one retry is the row.
//!
//! The browser witness drives the whole thing end to end against a
//! real `context.setOffline`; the tests here pin the rule.

use std::collections::HashMap;

use net_wire::clock::Instant;

use crate::clock::Deadline;
use crate::control_plane::NodeId;

/// Where one re-attempt trigger came from.
///
/// Carried into the ledger rather than collapsed, because "which
/// observation actually reached us" is the first question a field
/// report raises: an engine that never walks ICE to `failed` and one
/// whose `online` event never fires are both "the retry did not
/// happen", and only the counts tell them apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerSource {
    /// The window's `online` event — `navigator.onLine` going true.
    Online,
    /// An ICE `disconnected` → `failed` transition on a peer's
    /// `RTCPeerConnection`.
    IceFailed,
}

impl TriggerSource {
    /// The stable name the report uses.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Online => "online",
            Self::IceFailed => "iceFailed",
        }
    }
}

/// What the owner does about one trigger.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryDecision {
    /// Start ONE re-attempt for this peer, bounded by this deadline
    /// and nothing else.
    ///
    /// The deadline is the episode's **and** the re-attempt's: the
    /// dialog the re-attempt opens is given exactly this deadline, so
    /// the channel wait, the settle, the Noise wait and the window
    /// that absorbs every further trigger for this network change are
    /// one absolute instant rather than four that nearly agree. That
    /// is Stage 4a's rule (`mesh.rs`: "**One absolute deadline per
    /// attempt** … the channel wait, Noise and the install share it,
    /// so the table can no longer expire an attempt that is already
    /// installing") read onto the leaf.
    Start(Deadline),
    /// A re-attempt for this peer already belongs to this network
    /// change. Nothing starts.
    ///
    /// This is the whole point of the type. Both triggers firing for
    /// one network change lands here on the second, and so does a
    /// trigger that arrives *after* the re-attempt already restored
    /// the session — the episode outlives its own attempt precisely
    /// so that a late observation cannot start a second one.
    Coalesced,
    /// The peer has nothing to repair: no direct session was lost, so
    /// there is nothing for a re-attempt to restore.
    NotEligible,
}

/// The owner's ledger, as `retryReport()` renders it.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RetryLedger {
    /// Triggers filed by the `online` listener.
    pub online: u64,
    /// Triggers filed by the ICE `disconnected` → `failed` watcher.
    pub ice_failed: u64,
    /// Re-attempts this owner STARTED. The number the witness
    /// asserts is one.
    pub started: u64,
    /// Triggers absorbed into an episode that already had its
    /// re-attempt.
    pub coalesced: u64,
    /// Triggers for a peer with nothing to repair.
    pub not_eligible: u64,
}

impl RetryLedger {
    /// Every trigger this owner saw, whatever it decided.
    pub const fn triggers(&self) -> u64 {
        self.online + self.ice_failed
    }
}

/// One owner of the network-change re-attempt, for every peer.
///
/// # What an episode is
///
/// One network change, for one peer: it opens on the first trigger
/// that finds something to repair, and it lasts exactly as long as
/// the re-attempt it started — the same absolute deadline. Every
/// further trigger inside it is absorbed.
///
/// The window is the attempt's own deadline and not a tunable of its
/// own, deliberately. A shorter one would reopen while the
/// re-attempt was still gathering and a second `online` would start
/// a rival attempt against the same peer; a longer one would refuse
/// to act on a genuinely new network change that arrived after this
/// one had already failed. "Until this attempt is over" is the
/// honest span, and it is one number.
#[derive(Debug)]
pub struct RetryPolicy {
    /// Peer → the deadline of the episode that currently owns it.
    episodes: HashMap<NodeId, Deadline>,
    ledger: RetryLedger,
    window_ms: u64,
}

impl RetryPolicy {
    /// An owner whose episodes — and whose re-attempts — last
    /// `window_ms`.
    pub fn new(window_ms: u64) -> Self {
        Self {
            episodes: HashMap::new(),
            ledger: RetryLedger::default(),
            window_ms,
        }
    }

    /// File one trigger and say what happens.
    ///
    /// `interrupted` is the caller's reading of the only thing that
    /// makes a re-attempt meaningful: this leaf took a direct session
    /// with `peer` as the offerer, and does not have one now. It is
    /// the caller's because only [`crate::wasm`] can see a session;
    /// it is a parameter rather than a closure so this rule stays
    /// assertable.
    ///
    /// The episode is checked **before** eligibility, so a trigger
    /// that arrives after the re-attempt already restored the pair
    /// reads as `Coalesced` — "this network change is accounted
    /// for" — rather than as `NotEligible`, which would say
    /// something quite different about it.
    pub fn note(
        &mut self,
        peer: NodeId,
        source: TriggerSource,
        now: Instant,
        interrupted: bool,
    ) -> RetryDecision {
        match source {
            TriggerSource::Online => self.ledger.online += 1,
            TriggerSource::IceFailed => self.ledger.ice_failed += 1,
        }
        // An episode is over when its attempt's deadline has passed.
        // Pruned here rather than swept: this is the only reader, and
        // a sweep would be a second place that decides when an
        // episode ends.
        self.episodes
            .retain(|_, deadline| !deadline.expired_at(now));
        if self.episodes.contains_key(&peer) {
            self.ledger.coalesced += 1;
            return RetryDecision::Coalesced;
        }
        if !interrupted {
            self.ledger.not_eligible += 1;
            return RetryDecision::NotEligible;
        }
        let deadline = Deadline::in_ms(self.window_ms);
        self.episodes.insert(peer, deadline);
        self.ledger.started += 1;
        RetryDecision::Start(deadline)
    }

    /// The ledger, for the report.
    pub const fn ledger(&self) -> RetryLedger {
        self.ledger
    }

    /// Milliseconds left on `peer`'s episode; `0` when none is open.
    pub fn episode_remaining_ms(&self, peer: NodeId, now: Instant) -> u64 {
        self.episodes
            .get(&peer)
            .map_or(0, |deadline| deadline.remaining_ms_at(now))
    }

    /// How many episodes are open as of `now`.
    pub fn open_episodes(&self, now: Instant) -> usize {
        self.episodes
            .values()
            .filter(|deadline| !deadline.expired_at(now))
            .count()
    }
}

/// The ICE connection states this leaf reads, as the web platform
/// spells them.
///
/// A plain enum rather than `web_sys::RtcIceConnectionState` so
/// [`IceWatch`] is assertable on the host; [`crate::rtc`] maps the
/// engine's value onto it at the one place it reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IceLinkState {
    /// No gathering has started.
    New,
    /// Connectivity checks are running.
    Checking,
    /// A usable pair was nominated.
    Connected,
    /// Every component is connected and checking has stopped.
    Completed,
    /// The agent stopped receiving on a pair that was working. Not
    /// terminal: it recovers to `connected` or gives up.
    Disconnected,
    /// The agent gave up.
    Failed,
    /// This side closed the connection.
    Closed,
}

/// The `disconnected` → `failed` transition, and nothing else.
///
/// # Why the transition and not the state
///
/// `failed` on its own is not a network change. An attempt whose ICE
/// never connected ends in `failed` too, and that attempt already
/// has an owner — its own deadline, which settles `ice_relayed` or
/// `udp_blocked` on the evidence. Re-attempting on any `failed`
/// would double every timed-out attempt and make the §10 partition
/// drift for a reason nobody could see from the counters.
///
/// The transition the plan names is what a *lost* connection looks
/// like: ICE was up, the browser noticed the path stop answering
/// (`disconnected`), and then gave up on it (`failed`). A
/// `disconnected` that recovers goes back to `connected` and is not
/// a trigger — which is the case this type exists to get right, and
/// the one a state-only reading gets wrong.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct IceWatch {
    disconnected: bool,
}

impl IceWatch {
    /// Observe one state change; `true` means "this is a trigger".
    pub fn observe(&mut self, state: IceLinkState) -> bool {
        match state {
            IceLinkState::Disconnected => {
                self.disconnected = true;
                false
            }
            // Recovery. The path came back, so the `disconnected`
            // that preceded it was not a loss and must not arm a
            // later `failed` — an agent that flaps
            // `disconnected → connected → … → failed` at the end of
            // its own deadline would otherwise fire a trigger that
            // belongs to the attempt's deadline, not to a network
            // change.
            IceLinkState::Connected | IceLinkState::Completed => {
                self.disconnected = false;
                false
            }
            // Fires once: the flag is taken, so a second `failed`
            // for the same loss is not a second trigger.
            IceLinkState::Failed => core::mem::take(&mut self.disconnected),
            // `closed` is this side letting go — a `close()` we
            // called — and `new`/`checking` are an attempt starting.
            // Neither is a loss.
            IceLinkState::New | IceLinkState::Checking | IceLinkState::Closed => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock;

    const PEER: NodeId = 0x00AA;
    const OTHER: NodeId = 0x00BB;
    const WINDOW_MS: u64 = 10_000;

    /// **The row.** One network change reaches the leaf as two
    /// observations — the `online` event and the ICE transition — and
    /// starts exactly ONE re-attempt.
    #[test]
    fn both_triggers_for_one_network_change_start_one_re_attempt() {
        let mut policy = RetryPolicy::new(WINDOW_MS);
        let now = clock::now();
        let first = policy.note(PEER, TriggerSource::IceFailed, now, true);
        let second = policy.note(PEER, TriggerSource::Online, now, true);
        // A third, because a browser is under no obligation to fire
        // each observation once: `online` after a flapping interface
        // arrives as often as the interface flaps.
        let third = policy.note(PEER, TriggerSource::Online, now, true);

        assert!(matches!(first, RetryDecision::Start(_)), "{first:?}");
        assert_eq!(second, RetryDecision::Coalesced);
        assert_eq!(third, RetryDecision::Coalesced);
        let ledger = policy.ledger();
        assert_eq!(ledger.started, 1, "one network change, one re-attempt");
        assert_eq!(ledger.coalesced, 2);
        assert_eq!(ledger.triggers(), 3);
    }

    /// The deadline the decision carries is the episode's own, so the
    /// attempt and the window that absorbs further triggers are ONE
    /// absolute instant.
    #[test]
    fn the_episode_and_its_re_attempt_share_one_absolute_deadline() {
        let mut policy = RetryPolicy::new(WINDOW_MS);
        let now = clock::now();
        let RetryDecision::Start(deadline) = policy.note(PEER, TriggerSource::Online, now, true)
        else {
            panic!("the first trigger must start a re-attempt");
        };
        let remaining = policy.episode_remaining_ms(PEER, now);
        assert_eq!(
            deadline.remaining_ms_at(now),
            remaining,
            "the attempt's deadline and the episode's window are not the same instant"
        );
        assert!(remaining > 0 && remaining <= WINDOW_MS, "{remaining}");
    }

    /// A trigger that arrives once the re-attempt has already
    /// restored the session is absorbed, not acted on. This is the
    /// late-`online` case: the episode outlives its attempt on
    /// purpose.
    #[test]
    fn a_trigger_after_the_session_is_back_starts_nothing() {
        let mut policy = RetryPolicy::new(WINDOW_MS);
        let now = clock::now();
        assert!(matches!(
            policy.note(PEER, TriggerSource::IceFailed, now, true),
            RetryDecision::Start(_)
        ));
        // Restored: `interrupted` is now false, and the episode is
        // still open.
        assert_eq!(
            policy.note(PEER, TriggerSource::Online, now, false),
            RetryDecision::Coalesced
        );
        assert_eq!(policy.ledger().started, 1);
    }

    /// A peer with nothing to repair is not re-attempted, and is not
    /// charged an episode either — so the next genuine loss is acted
    /// on immediately rather than waiting out a window it never
    /// used.
    #[test]
    fn a_peer_with_a_live_direct_session_is_not_re_attempted() {
        let mut policy = RetryPolicy::new(WINDOW_MS);
        let now = clock::now();
        assert_eq!(
            policy.note(PEER, TriggerSource::Online, now, false),
            RetryDecision::NotEligible
        );
        assert_eq!(policy.open_episodes(now), 0);
        assert!(matches!(
            policy.note(PEER, TriggerSource::Online, now, true),
            RetryDecision::Start(_)
        ));
        let ledger = policy.ledger();
        assert_eq!((ledger.started, ledger.not_eligible), (1, 1));
    }

    /// One network change, two peers: each gets its own episode. The
    /// bound is per peer, because the thing being repaired is a pair.
    #[test]
    fn one_online_event_re_attempts_each_interrupted_peer_once() {
        let mut policy = RetryPolicy::new(WINDOW_MS);
        let now = clock::now();
        assert!(matches!(
            policy.note(PEER, TriggerSource::Online, now, true),
            RetryDecision::Start(_)
        ));
        assert!(matches!(
            policy.note(OTHER, TriggerSource::Online, now, true),
            RetryDecision::Start(_)
        ));
        assert_eq!(
            policy.note(PEER, TriggerSource::Online, now, true),
            RetryDecision::Coalesced
        );
        assert_eq!(policy.ledger().started, 2);
        assert_eq!(policy.open_episodes(now), 2);
    }

    /// A genuinely NEW network change, after the previous episode's
    /// deadline passed, is acted on. Bounded is not once-ever.
    #[test]
    fn a_later_network_change_gets_its_own_re_attempt() {
        // A zero-length window: the episode is over the instant the
        // clock moves past the reading that opened it, which is what
        // "its attempt's deadline has passed" means at the limit.
        let mut policy = RetryPolicy::new(0);
        assert!(matches!(
            policy.note(PEER, TriggerSource::Online, clock::now(), true),
            RetryDecision::Start(_)
        ));
        assert!(matches!(
            policy.note(PEER, TriggerSource::Online, clock::now(), true),
            RetryDecision::Start(_)
        ));
        assert_eq!(policy.ledger().started, 2);
        assert_eq!(policy.ledger().coalesced, 0);
    }

    /// Only `disconnected` → `failed` is a trigger.
    #[test]
    fn the_ice_watch_fires_on_the_transition_and_not_on_the_state() {
        // The loss.
        let mut watch = IceWatch::default();
        assert!(!watch.observe(IceLinkState::Checking));
        assert!(!watch.observe(IceLinkState::Connected));
        assert!(!watch.observe(IceLinkState::Disconnected));
        assert!(watch.observe(IceLinkState::Failed), "the transition");
        // Once. A second `failed` for the same loss is not a second
        // network change.
        assert!(!watch.observe(IceLinkState::Failed));

        // An attempt that never connected. Its own deadline owns it
        // and settles the §10 term; re-attempting here would double
        // every timed-out attempt.
        let mut never_up = IceWatch::default();
        assert!(!never_up.observe(IceLinkState::New));
        assert!(!never_up.observe(IceLinkState::Checking));
        assert!(!never_up.observe(IceLinkState::Failed), "not a loss");

        // A recovery. The path came back, so the `disconnected` is
        // spent and a much later `failed` is not this network
        // change.
        let mut recovered = IceWatch::default();
        assert!(!recovered.observe(IceLinkState::Connected));
        assert!(!recovered.observe(IceLinkState::Disconnected));
        assert!(!recovered.observe(IceLinkState::Connected));
        assert!(!recovered.observe(IceLinkState::Failed), "recovered first");

        // Our own close.
        let mut closed = IceWatch::default();
        assert!(!closed.observe(IceLinkState::Connected));
        assert!(!closed.observe(IceLinkState::Closed));
        assert!(!closed.observe(IceLinkState::Failed));
    }

    /// `completed` is a healthy state too — an agent that nominated a
    /// pair and stopped checking reports it instead of `connected`,
    /// and reading it as anything but recovery would arm a trigger on
    /// every successful connection.
    #[test]
    fn completed_counts_as_recovery() {
        let mut watch = IceWatch::default();
        assert!(!watch.observe(IceLinkState::Disconnected));
        assert!(!watch.observe(IceLinkState::Completed));
        assert!(!watch.observe(IceLinkState::Failed));
    }
}
