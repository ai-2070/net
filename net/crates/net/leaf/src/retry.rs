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
//! `rtc` or `wasm` cannot be — both are
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

/// The role this leaf installed a peer's CURRENT direct session in.
///
/// Not "has ever been": a pair's repair belongs to one endpoint,
/// and the only endpoint that can be identified without a second
/// negotiation is the one that offered the session both sides are
/// using now. `A→B` then `B→A` moves it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstalledRole {
    /// This leaf sent the offer, so it owns the pair's repair.
    Offerer,
    /// The peer sent the offer, so the peer owns it and this leaf
    /// re-offers nothing.
    Answerer,
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
    /// there is nothing for a re-attempt to restore — or this leaf is
    /// not the endpoint that currently owns the pair's repair (see
    /// [`RetryPolicy::note_installed_role`]).
    NotEligible,
    /// The page has not opted in, so this owner does nothing and the
    /// observation is **dropped**.
    ///
    /// The stated fate of a pre-arm observation, and it is a decision
    /// rather than an omission: it is discarded here, no episode is
    /// opened for it, and [`RetryPolicy::arm`] does not resurrect it.
    /// Retaining it would mean the public report says retry is
    /// unarmed while the owner is holding work it intends to act on
    /// — the same defect in a different costume. A network change
    /// that happened before the page asked for repairs is a network
    /// change the page did not ask to have repaired; the next one
    /// after arming is acted on in full.
    NotArmed,
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
    /// Triggers **dropped** because the page had not opted in.
    ///
    /// Counted, not hidden: the observation is real, and "the
    /// watcher saw an ICE loss and this owner did nothing with it"
    /// is the one fact that tells an unarmed leaf apart from an
    /// engine that never reported a loss at all. `started` stays at
    /// zero for every one of them.
    pub discarded_unarmed: u64,
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
///
/// # What arming is
///
/// The page's `enableRetry()` is the whole authority for this owner
/// acting at all. It is checked HERE rather than at the observation
/// sites because the sites are plural — a window listener and an
/// ICE-state callback, and in a browser neither can be prevented
/// from firing — while the decision is singular. An owner that
/// trusted its sources to be silent until arming would publish
/// `armed:false` and still schedule an offer, which is exactly the
/// gap this gate closes.
///
/// # Who owns a pair's repair
///
/// The endpoint whose CURRENT installed direct session it offered,
/// and only that one. Ownership is set by
/// [`Self::note_installed_role`] on every direct install, in both
/// directions, so `A→B` followed by `B→A` moves it from A to B
/// instead of leaving both ends eligible. A historical record of
/// "was once the offerer" cannot be resolved by per-node
/// coalescing: two endpoints each coalescing correctly for
/// themselves is still two re-offers for one pair.
#[derive(Debug)]
pub struct RetryPolicy {
    /// Peer → the deadline of the episode that currently owns it.
    episodes: HashMap<NodeId, Deadline>,
    ledger: RetryLedger,
    window_ms: u64,
    /// Whether the page opted in. Until it does this owner starts
    /// nothing; see [`RetryDecision::NotArmed`] for the fate of an
    /// observation that arrives first.
    armed: bool,
    /// Peers whose repair this leaf currently owns, because it is
    /// the offerer of their live installed direct session.
    owned: std::collections::HashSet<NodeId>,
}

impl RetryPolicy {
    /// An owner whose episodes — and whose re-attempts — last
    /// `window_ms`.
    pub fn new(window_ms: u64) -> Self {
        Self {
            episodes: HashMap::new(),
            ledger: RetryLedger::default(),
            window_ms,
            armed: false,
            owned: std::collections::HashSet::new(),
        }
    }

    /// The page opted in. Idempotent; `true` when this call is the
    /// one that armed the owner, so a caller can install its window
    /// listeners exactly once.
    pub fn arm(&mut self) -> bool {
        let first = !self.armed;
        self.armed = true;
        first
    }

    /// Whether the page has opted in — the value the public report
    /// publishes, read from the owner that enforces it rather than
    /// from a second flag that could disagree with it.
    pub const fn is_armed(&self) -> bool {
        self.armed
    }

    /// Record the role this leaf installed `peer`'s direct session
    /// in, which is what decides who repairs the pair.
    ///
    /// Called on **every** direct install, including the ones that
    /// take this leaf out of the owning role: an answerer install
    /// gives ownership up. That is the whole mechanism — a set that
    /// is only ever inserted into records history, and history is
    /// not a role.
    ///
    /// A caller that cannot attribute a role passes
    /// [`InstalledRole::Answerer`]: an endpoint unable to show it is
    /// the current offerer must not initiate repair, because the
    /// failure mode of guessing yes is two endpoints offering.
    pub fn note_installed_role(&mut self, peer: NodeId, role: InstalledRole) {
        match role {
            InstalledRole::Offerer => {
                self.owned.insert(peer);
            }
            InstalledRole::Answerer => {
                self.owned.remove(&peer);
            }
        }
    }

    /// Give up ownership of `peer`'s repair and close its episode.
    ///
    /// For a pair that is terminal for a reason other than a role
    /// change — supersession, an explicit close, the peer going
    /// away. An episode is a coalescing window and owns no
    /// resource, but leaving one open would absorb the first
    /// trigger of a genuinely new attempt against the same peer.
    pub fn forget(&mut self, peer: NodeId) {
        self.owned.remove(&peer);
        self.episodes.remove(&peer);
    }

    /// [`Self::forget`] for every peer: the node is going away, so
    /// no episode and no ownership outlives it.
    pub fn forget_all(&mut self) {
        self.owned.clear();
        self.episodes.clear();
    }

    /// Whether this leaf currently owns `peer`'s repair.
    pub fn owns(&self, peer: NodeId) -> bool {
        self.owned.contains(&peer)
    }

    /// The peers this leaf currently owns the repair for — the fan-out
    /// an `online` event applies to, since that event names no peer.
    pub fn owned_peers(&self) -> Vec<NodeId> {
        self.owned.iter().copied().collect()
    }

    /// How many pairs this leaf owns the repair for, for the report.
    pub fn owned_count(&self) -> usize {
        self.owned.len()
    }

    /// File one trigger and say what happens.
    ///
    /// `interrupted` is the caller's reading of the only thing that
    /// makes a re-attempt meaningful: the direct session this leaf
    /// holds with `peer` is not usable right now. It is the
    /// caller's because only `wasm` can see a session; it is a
    /// parameter rather than a closure so this rule stays
    /// assertable. **Who may act on it is not the caller's**: that
    /// is arming plus the current installed role, and both are
    /// enforced here.
    ///
    /// Order of disposition, and each step is a different fact:
    ///
    /// 1. **Unarmed** — the page never asked. Dropped
    ///    ([`RetryDecision::NotArmed`]) before any episode exists,
    ///    so nothing this owner holds can later be mistaken for
    ///    work in flight.
    /// 2. **Episode open** — this network change is already
    ///    accounted for. `Coalesced`, which is what a trigger
    ///    arriving after the re-attempt already restored the pair
    ///    must read as rather than `NotEligible`; the two say quite
    ///    different things.
    /// 3. **Not ours, or nothing to repair** — `NotEligible`. An
    ///    answerer install has given the pair up, and the endpoint
    ///    that offered the live session is the one that re-offers.
    pub fn note(
        &mut self,
        peer: NodeId,
        source: TriggerSource,
        now: Instant,
        interrupted: bool,
    ) -> RetryDecision {
        // The observation is counted whatever happens to it: the
        // counters are what distinguish "the watcher saw nothing"
        // from "the watcher saw a loss and this owner dropped it".
        match source {
            TriggerSource::Online => self.ledger.online += 1,
            TriggerSource::IceFailed => self.ledger.ice_failed += 1,
        }
        if !self.armed {
            self.ledger.discarded_unarmed += 1;
            return RetryDecision::NotArmed;
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
        if !self.owns(peer) || !interrupted {
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
/// [`IceWatch`] is assertable on the host; `rtc` maps the
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

    /// A policy the page has opted into, owning the repair for
    /// `peers` because it offered their live direct sessions.
    ///
    /// Both facts are now preconditions of this owner acting at
    /// all, so the rows below that are about coalescing say so
    /// once here instead of re-arguing it each time.
    fn owner(window_ms: u64, peers: &[NodeId]) -> RetryPolicy {
        let mut policy = RetryPolicy::new(window_ms);
        policy.arm();
        for peer in peers {
            policy.note_installed_role(*peer, InstalledRole::Offerer);
        }
        policy
    }

    /// **The row.** One network change reaches the leaf as two
    /// observations — the `online` event and the ICE transition — and
    /// starts exactly ONE re-attempt.
    #[test]
    fn both_triggers_for_one_network_change_start_one_re_attempt() {
        let mut policy = owner(WINDOW_MS, &[PEER]);
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
        let mut policy = owner(WINDOW_MS, &[PEER]);
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
        let mut policy = owner(WINDOW_MS, &[PEER]);
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
        let mut policy = owner(WINDOW_MS, &[PEER]);
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
        let mut policy = owner(WINDOW_MS, &[PEER, OTHER]);
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
        let mut policy = owner(0, &[PEER]);
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

    /// **S6-05, the opt-in.** An ICE failure observed before the
    /// page called `enableRetry()` starts nothing, and its fate is
    /// stated: it is DROPPED. Arming afterwards replays nothing —
    /// and then the next observation is acted on in full.
    ///
    /// The defect this refuses: the watcher is installed
    /// unconditionally, so a leaf whose public report says
    /// `armed:false` could still schedule an offer.
    #[test]
    fn an_unarmed_ice_failure_is_dropped_and_arming_replays_nothing() {
        let mut policy = RetryPolicy::new(WINDOW_MS);
        // Ownership is not the missing piece here: this leaf really
        // does own the pair, and the trigger is a real loss.
        policy.note_installed_role(PEER, InstalledRole::Offerer);
        let now = clock::now();
        assert!(!policy.is_armed(), "the page has not opted in");
        assert_eq!(
            policy.note(PEER, TriggerSource::IceFailed, now, true),
            RetryDecision::NotArmed
        );
        let ledger = policy.ledger();
        assert_eq!(
            (ledger.started, ledger.discarded_unarmed, ledger.ice_failed),
            (0, 1, 1),
            "the observation is counted and dropped; nothing is started"
        );
        assert_eq!(
            policy.open_episodes(now),
            0,
            "a dropped observation opens no episode, so the owner holds no work \
             its own report would deny"
        );
        assert_eq!(policy.episode_remaining_ms(PEER, now), 0);

        assert!(policy.arm(), "the call that arms says so, once");
        assert!(!policy.arm(), "and arming twice is not a second arming");
        assert_eq!(
            policy.ledger().started,
            0,
            "arming is not a replay: the pre-arm observation stays dropped"
        );

        let after = policy.note(PEER, TriggerSource::IceFailed, clock::now(), true);
        assert!(
            matches!(after, RetryDecision::Start(_)),
            "the first observation AFTER arming is acted on in full: {after:?}"
        );
        assert_eq!(policy.ledger().started, 1);
    }

    /// **S6-05, ownership.** Armed and genuinely interrupted is not
    /// enough: a leaf that answered the pair's offer re-offers
    /// nothing, because the endpoint that offered the live session
    /// is the one that repairs it.
    #[test]
    fn an_armed_trigger_for_a_pair_this_leaf_answered_starts_nothing() {
        let mut policy = RetryPolicy::new(WINDOW_MS);
        policy.arm();
        policy.note_installed_role(PEER, InstalledRole::Answerer);
        let now = clock::now();
        assert!(!policy.owns(PEER));
        assert_eq!(
            policy.note(PEER, TriggerSource::IceFailed, now, true),
            RetryDecision::NotEligible
        );
        assert_eq!(policy.ledger().started, 0);
        assert!(
            policy.owned_peers().is_empty(),
            "and the `online` fan-out, which names no peer, reaches nothing"
        );
    }

    /// **S6-05, role reversal — the branch with teeth.** `A→B`
    /// direct followed by `B→A` direct must leave exactly ONE
    /// endpoint eligible to initiate repair.
    ///
    /// Asserted across BOTH endpoints' owners, because per-node
    /// coalescing cannot resolve two owners of one pair: each would
    /// coalesce correctly for itself and the pair would still get
    /// two re-offers. A historical "was once the offerer" set
    /// records A for ever and fails this row at `a.ledger().started`.
    #[test]
    fn role_reversal_moves_repair_ownership_and_leaves_one_owner() {
        const A: NodeId = 0x00AA;
        const B: NodeId = 0x00BB;

        // Two leaves, two owners. A peer id names the OTHER end, so
        // `a` speaks about `B` and `b` speaks about `A`.
        let mut a = RetryPolicy::new(WINDOW_MS);
        let mut b = RetryPolicy::new(WINDOW_MS);
        a.arm();
        b.arm();

        // A→B: A offered, B answered.
        a.note_installed_role(B, InstalledRole::Offerer);
        b.note_installed_role(A, InstalledRole::Answerer);
        assert!(a.owns(B), "the offerer owns the repair");
        assert!(!b.owns(A), "the answerer owns nothing");

        // B→A: the pair takes a direct session the other way round.
        // Both ends install again, each in its new role.
        b.note_installed_role(A, InstalledRole::Offerer);
        a.note_installed_role(B, InstalledRole::Answerer);

        let now = clock::now();
        let at_a = a.note(B, TriggerSource::IceFailed, now, true);
        let at_b = b.note(A, TriggerSource::IceFailed, now, true);

        assert_eq!(
            at_a,
            RetryDecision::NotEligible,
            "A offered a session that is no longer the installed one; it must not \
             initiate repair for a pair B now owns"
        );
        assert!(
            matches!(at_b, RetryDecision::Start(_)),
            "B offered the live session, so B repairs it: {at_b:?}"
        );
        assert_eq!(
            a.ledger().started + b.ledger().started,
            1,
            "ONE re-offer for one pair, counted over both endpoints"
        );
        assert_eq!(
            a.owned_count() + b.owned_count(),
            1,
            "and exactly one endpoint is eligible at all"
        );
    }

    /// Terminal retirement of a pair: ownership and the coalescing
    /// window both go, so a genuinely new attempt's first trigger
    /// is not absorbed by the dead episode.
    #[test]
    fn forgetting_a_pair_drops_its_ownership_and_its_episode() {
        let mut policy = owner(WINDOW_MS, &[PEER, OTHER]);
        let now = clock::now();
        assert!(matches!(
            policy.note(PEER, TriggerSource::IceFailed, now, true),
            RetryDecision::Start(_)
        ));
        assert_eq!(policy.open_episodes(now), 1);

        policy.forget(PEER);
        assert!(!policy.owns(PEER));
        assert_eq!(policy.open_episodes(now), 0);
        assert_eq!(
            policy.note(PEER, TriggerSource::IceFailed, now, true),
            RetryDecision::NotEligible,
            "a forgotten pair is not repaired, and is not coalesced either"
        );

        assert!(policy.owns(OTHER), "its sibling is untouched");
        policy.forget_all();
        assert_eq!(policy.owned_count(), 0);
        assert_eq!(policy.open_episodes(now), 0);
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
