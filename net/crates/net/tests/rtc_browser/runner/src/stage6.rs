//! Stage 6 slice 1 — **browser ↔ browser direct, from a page.**
//!
//! Plan §9: browser ↔ browser is the §5 sequence with the mesh as the
//! signalling network. Two isolated browsing contexts, two leaf
//! identities, one anchor, and nothing passed between the pages by the
//! harness except a node id.
//!
//! # What makes this different from the Stage 5 two-tab witness
//!
//! Stage 5's two tabs shared one `BrowserContext` and one identity on
//! purpose — the property under test was leader election. Here the two
//! tabs are in **separate browsing contexts**, which is what makes the
//! isolation real: separate storage partitions, so two independent
//! identities at rest, and separate Web Lock namespaces, so two
//! independent leader elections rather than one contended lock. ICE
//! between them is therefore genuinely cross-context, with real host
//! and peer-reflexive candidates.
//!
//! # The carrier, and why it is not the control plane
//!
//! Signalling rides the **routed leaf ↔ leaf session** through the
//! anchor's blind forwarding — `0x0D02` frames on the data path, which
//! is plan §9 step 3 verbatim and the same framing the native
//! `send_rtc_signal` uses. `ControlPlane::signal` is NOT used and
//! `AnchorControlPlane` still refuses it: Stage 5's R14 stands, and its
//! witness (`the_anchor_control_plane_refuses_to_carry_a_signalling_
//! envelope`) is unchanged and still green.
//!
//! The anchor never decrypts what it forwards — the inner packet is
//! sealed to the A↔B session it does not hold — and `0x0D02` is
//! excluded from its per-pair application-data counter. The carrier is
//! blind, and the D1 signature is what makes that blindness safe
//! rather than merely polite.
//!
//! # Where the keys come from
//!
//! From discovery, and only from discovery. A page names a peer; it
//! never passes an SDP blob, a candidate line or a key, because a page
//! that can supply a Noise key can supply *any* key. Two of the
//! witnesses below exist purely to hold that line: one reads the
//! declared arity of the four `#[wasm_bindgen]` methods off the live
//! boundary, and one drives an undiscovered peer and observes the
//! refusal happen before an offer exists.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::MeshNode;

use crate::browser::{Driver, Engine};
use crate::stage5::{Script5, Step5, Step5Sender};
use crate::{Ledger, StepResult};

/// The capability tag both leaves announce and each discovers the
/// other by.
const PEER_TAG: &str = "stage6.peer";

/// The two Stage 6 tabs, each in its own browsing context.
///
/// Their own step queues: a page long-polling a queue another page
/// also polls would resolve the other's steps.
pub const TABS: [&str; 2] = [TAB_A, TAB_B];

const TAB_A: &str = "c";
const TAB_B: &str = "d";
/// The two pages, one per context.
const PAGE_A: &str = "peer6-a";
const PAGE_B: &str = "peer6-b";

/// The isolated browsing contexts the two tabs live in. Named, so the
/// driver creates one per name rather than sharing the launch context
/// every Stage 4b and Stage 5 page uses.
const CTX_A: &str = "stage6-a";
const CTX_B: &str = "stage6-b";

/// The two leaf identities, fixed so a failure is reproducible and so
/// the two tabs cannot accidentally share one.
const SECRET_A_ENTITY: &str = "a6a6a6a6a6a6a6a6a6a6a6a6a6a6a6a6a6a6a6a6a6a6a6a6a6a6a6a6a6a6a6a6";
const SECRET_A_NOISE: &str = "a7a7a7a7a7a7a7a7a7a7a7a7a7a7a7a7a7a7a7a7a7a7a7a7a7a7a7a7a7a7a7a7";
const SECRET_B_ENTITY: &str = "b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6";
const SECRET_B_NOISE: &str = "b7b7b7b7b7b7b7b7b7b7b7b7b7b7b7b7b7b7b7b7b7b7b7b7b7b7b7b7b7b7b7b7";

/// How long the runner waits for one leaf to discover the other
/// through the anchor's announcement flood.
const DISCOVERY_DEADLINE: Duration = Duration::from_secs(20);

/// How many `peer_candidate` services the fine-grained witness runs
/// before it gives up on the channel.
///
/// The leaf owns the real ICE deadline; this is only the runner's
/// bound on its own polling, generous enough that the leaf's deadline
/// is always the one that fires.
const CANDIDATE_POLLS: usize = 400;

/// Every Stage 6 witness name, in ledger order. CI pins these
/// exactly; the list lives here so a rename is one edit and a drop
/// cannot be done quietly.
pub const WITNESSES: [&str; 9] = [
    "stage6_two_isolated_contexts_reach_a_direct_session",
    "stage6_each_leaf_learns_the_others_keys_from_a_signed_announcement",
    "stage6_the_four_wasm_methods_drive_one_attempt_end_to_end",
    "stage6_peer_handshake_takes_a_peer_id_and_nothing_else",
    "stage6_an_undiscovered_peer_is_refused_before_an_offer_exists",
    "stage6_an_offer_nobody_answers_leaves_the_pair_relayed",
    "stage6_a_second_offer_supersedes_the_live_attempt",
    "stage6_the_ice_attempt_ledger_partitions_on_both_leaves",
    "stage6_a_handshake_the_peer_cannot_answer_is_typed_handshake_failed",
];

/// Everything the Stage 6 witnesses need from the runner.
///
/// Narrower than `stage5::Cx` on purpose: no `LaunchSpec` (nothing
/// here relaunches the browser) and no `Bundle` (Stage 5 has already
/// failed every one of its witnesses by the time this runs if the
/// bundle is absent).
pub struct Cx6<'a> {
    pub driver: &'a Driver,
    pub engine: Engine,
    pub anchor: &'a Arc<MeshNode>,
    pub credential: String,
    pub bootstrap_url: String,
    pub origin: String,
    /// `http://localhost:PORT` — the page server's origin.
    pub page_origin: String,
    pub stun: Option<String>,
    /// The anchor's published STUN/RTC socket. The page passes it to
    /// `connect` and it is the ONLY address the UDP-blocked evidence
    /// is allowed to probe.
    pub anchor_rtc_addr: String,
    pub tabs: HashMap<String, Step5Sender>,
}

/// One leaf's identity as the page reports it after `connect`.
struct Leaf {
    node_hex: String,
    node_id: u64,
}

/// Run every Stage 6 witness.
///
/// Returns `Err` only for a harness fault — a browsing context that
/// would not open. A witness that failed is recorded and the run
/// continues, exactly like the 4b and Stage 5 halves.
#[expect(clippy::too_many_lines, reason = "one linear witness script")]
pub async fn run(cx: Cx6<'_>, ledger: &mut Ledger) -> Result<(), String> {
    let mut script = Script5::new(cx.tabs.clone(), 2_000_000);

    // --- two isolated contexts --------------------------------------
    //
    // Opened here rather than at launch so the ledger can name the
    // failure: a driver without the context op refuses by name, and a
    // silent fallback to the launch context would let every witness
    // below claim an isolation it did not get.
    let url_a = format!("{}/leaf5.html?tab={TAB_A}", cx.page_origin);
    let url_b = format!("{}/leaf5.html?tab={TAB_B}", cx.page_origin);
    if let Err(why) = cx
        .driver
        .open_page_in(PAGE_A, &url_a, Some(CTX_A))
        .await
        .and(Ok(()))
    {
        for name in WITNESSES {
            ledger.record(
                name,
                false,
                format!("the first isolated browsing context would not open: {why}"),
            );
        }
        return Ok(());
    }
    if let Err(why) = cx.driver.open_page_in(PAGE_B, &url_b, Some(CTX_B)).await {
        for name in WITNESSES {
            ledger.record(
                name,
                false,
                format!("the second isolated browsing context would not open: {why}"),
            );
        }
        return Ok(());
    }

    // --- both leaves connect, announce, and discover each other ----
    let connect = |entity: &str, noise: &str| Step5::Connect {
        id: 0,
        session: "peer".to_string(),
        credential: cx.credential.clone(),
        bootstrap_url: cx.bootstrap_url.clone(),
        origin: cx.origin.clone(),
        anchor_rtc_addr: cx.anchor_rtc_addr.clone(),
        stun: cx.stun.clone(),
        entity_secret_hex: Some(entity.to_string()),
        noise_secret_hex: Some(noise.to_string()),
        // A direct `connect`, not `openSession`: the contexts are
        // already isolated, so there is no contention for a leader to
        // resolve, and a leader election here would be a second
        // mechanism between the page and the property under test.
        use_session: false,
        capabilities: vec![PEER_TAG.to_string()],
        subscriptions: Vec::new(),
        lock_scope: None,
        expect_failure: false,
    };

    let a = script
        .run(TAB_A, connect(SECRET_A_ENTITY, SECRET_A_NOISE))
        .await;
    let b = script
        .run(TAB_B, connect(SECRET_B_ENTITY, SECRET_B_NOISE))
        .await;
    let leaves = match (leaf_of(&a), leaf_of(&b)) {
        (Some(a), Some(b)) if a.node_id != b.node_id => Some((a, b)),
        (Some(a), Some(b)) => {
            let detail = format!(
                "both isolated contexts connected as the SAME node {} — the identities did \
                 not separate, so nothing below would be a browser ↔ browser test",
                a.node_hex
            );
            let _ = b;
            for name in WITNESSES {
                ledger.record(name, false, detail.clone());
            }
            return Ok(());
        }
        _ => {
            let detail = format!(
                "a leaf did not connect: a={:?} b={:?}",
                a.error.clone().unwrap_or_default(),
                b.error.clone().unwrap_or_default()
            );
            for name in WITNESSES {
                ledger.record(name, false, detail.clone());
            }
            return Ok(());
        }
    };
    let (a, b) = leaves.expect("checked above");

    // Announce under one tag, then wait for each side to hold the
    // OTHER's announcement. This is §5 Layer 1 and it is a
    // precondition of everything else: without it there is no Noise
    // key to handshake against and no authority to verify an envelope
    // with.
    for (tab, who) in [(TAB_A, "a"), (TAB_B, "b")] {
        let announced = script
            .run(
                tab,
                Step5::Announce {
                    id: 0,
                    session: "peer".to_string(),
                    capabilities: vec![PEER_TAG.to_string()],
                },
            )
            .await;
        if !announced.ok {
            println!(
                "[stage6] leaf {who} could not announce: {}",
                announced.error.clone().unwrap_or_default()
            );
        }
    }

    let found_b = discover(&mut script, TAB_A, &b.node_hex).await;
    let found_a = discover(&mut script, TAB_B, &a.node_hex).await;
    ledger.record(
        WITNESSES[1],
        found_b.is_some() && found_a.is_some(),
        format!(
            "each leaf holds the other's SIGNED announcement, verified by itself: \
             a's view of b = {found_b:?}, b's view of a = {found_a:?}. The anchor floods \
             what it verified and holds no key that could forge one; the noise_pubkey in \
             each view is the only key either handshake below authenticates against, and \
             it never crossed the page boundary. a={} b={}",
            a.node_hex, b.node_hex
        ),
    );

    // --- 1. the page-facing loop, both halves ----------------------
    //
    // B's `acceptPeer` is started FIRST and left in flight: it blocks
    // until an offer arrives, and an offerer whose peer is not
    // answering would otherwise burn its whole ICE deadline before
    // the answerer was even asked.
    let accept = script.spawn(
        TAB_B,
        Step5::PeerAccept {
            id: 0,
            session: "peer".to_string(),
            peer_hex: a.node_hex.clone(),
        },
    );
    let connected = script
        .run(
            TAB_A,
            Step5::PeerConnect {
                id: 0,
                session: "peer".to_string(),
                peer_hex: b.node_hex.clone(),
            },
        )
        .await;
    let accepted = accept.await;

    let offerer = outcome(&connected);
    let answerer = outcome(&accepted);
    let a_direct = attempt(&mut script, TAB_A, &b.node_hex).await;
    let b_direct = attempt(&mut script, TAB_B, &a.node_hex).await;
    let both_direct = offerer.as_deref() == Some("direct") && answerer.as_deref() == Some("direct");
    let sessions_installed = stat_bool(&a_direct, "direct") && stat_bool(&b_direct, "direct");
    let candidates_flowed =
        stat_u64(&a_direct, "sent") + stat_u64(&b_direct, "sent") > 0 || sessions_installed;

    ledger.record(
        WITNESSES[0],
        both_direct && sessions_installed,
        format!(
            "connectPeer/acceptPeer across two ISOLATED browsing contexts: offerer={offerer:?} \
             answerer={answerer:?}; a's attempt reads direct={} state={:?}, b's reads \
             direct={} state={:?}. `direct` is the SESSION (installed and off the relay), \
             `state` is ICE — different facts, both asserted. Real cross-context ICE: \
             candidates were exchanged as signed envelopes over the relayed session \
             ({candidates_flowed}). Keys came from the signed announcements only. {}",
            stat_bool(&a_direct, "direct"),
            stat_str(&a_direct, "state"),
            stat_bool(&b_direct, "direct"),
            stat_str(&b_direct, "state"),
            peer_view(cx.anchor, a.node_id, b.node_id),
        ),
    );

    // --- 2. the four wasm methods, one at a time -------------------
    //
    // The same attempt, driven through the raw `#[wasm_bindgen]`
    // surface instead of the wrapper's loop, on a SECOND dialog. Both
    // families have to work: the four methods are what Stage 6 adds,
    // and `connectPeer` is only a loop over them.
    let raw = drive_raw(&mut script, &a, &b).await;
    ledger.record(
        WITNESSES[2],
        raw.installed,
        format!(
            "peer_offer → peer_accept_offer → peer_candidate* → peer_handshake, driven one \
             call at a time: offer dialog={:?}, answer dialog={:?} (the answerer reads the \
             dialog off the SIGNED envelope, it is not passed to it), {} candidate services, \
             channel open after {:?}, handshake {}. {}",
            raw.offer_dialog,
            raw.answer_dialog,
            raw.polls,
            raw.opened_after,
            if raw.installed {
                "installed the direct session"
            } else {
                "did NOT install"
            },
            raw.detail,
        ),
    );

    // --- 3. the arity of the boundary ------------------------------
    let arity = script
        .run(
            TAB_A,
            Step5::PeerArity {
                id: 0,
                session: "peer".to_string(),
            },
        )
        .await;
    let handshake_arity = stat_u64(&arity, "peer_handshake");
    let arity_ok = arity.ok
        && handshake_arity == 1
        && stat_u64(&arity, "peer_offer") == 1
        && stat_u64(&arity, "peer_accept_offer") == 1
        && stat_u64(&arity, "peer_candidate") == 1
        && stat_bool(&arity, "all_functions");
    ledger.record(
        WITNESSES[3],
        arity_ok,
        format!(
            "read off the live boundary: peer_handshake.length={handshake_arity}, \
             peer_offer={}, peer_accept_offer={}, peer_candidate={}. One parameter each, and \
             for peer_handshake that parameter is a peer id — so there is no argument through \
             which a page could pass a Noise key, which is the security property of the \
             slice rather than a convention about it. A page that can supply a key can \
             supply ANY key; these take a peer id and nothing else, and the key comes from \
             the peer's signature-verified announcement.",
            stat_u64(&arity, "peer_offer"),
            stat_u64(&arity, "peer_accept_offer"),
            stat_u64(&arity, "peer_candidate"),
        ),
    );

    // --- 4. an undiscovered peer -----------------------------------
    //
    // A node id that is well-formed and that nobody announced. The
    // refusal must happen before an offer is created — which is also
    // why it counts no attempt: an offer that never existed is not
    // one.
    let before = counters(&mut script, TAB_A).await;
    let stranger = script
        .run(
            TAB_A,
            Step5::PeerConnect {
                id: 0,
                session: "peer".to_string(),
                peer_hex: "00000000deadbeef".to_string(),
            },
        )
        .await;
    let after = counters(&mut script, TAB_A).await;
    let no_announcement = outcome(&stranger).as_deref() == Some("noAnnouncement");
    let unattempted = counter_of(&before, "ice_attempted") == counter_of(&after, "ice_attempted");
    ledger.record(
        WITNESSES[4],
        no_announcement && unattempted,
        format!(
            "connectPeer on a node nobody announced: outcome={:?}, detail={:?}. \
             ice_attempted did not move ({} → {}), which is the ordering claim: the refusal \
             happens before an offer is created, so a peer that cannot be discovered costs \
             nothing and is not charged against the denominator of ice_direct/ice_attempted.",
            outcome(&stranger),
            stat_str(&stranger, "detail"),
            counter_of(&before, "ice_attempted"),
            counter_of(&after, "ice_attempted"),
        ),
    );

    // --- 5. an offer nobody answers --------------------------------
    //
    // A offers to B and B is never asked to answer, so ICE has
    // nothing to connect to. The attempt must end at its own
    // deadline, typed, with the pair still relayed — §9 step 6's
    // "the routed session is simply never replaced".
    let before = counters(&mut script, TAB_A).await;
    let unanswered = script
        .run(
            TAB_A,
            Step5::PeerConnect {
                id: 0,
                session: "peer".to_string(),
                peer_hex: b.node_hex.clone(),
            },
        )
        .await;
    let after = counters(&mut script, TAB_A).await;
    let kind = outcome(&unanswered);
    let relayed_moved = counter_of(&after, "ice_relayed") > counter_of(&before, "ice_relayed");
    let blocked_moved = counter_of(&after, "udp_blocked") > counter_of(&before, "udp_blocked");
    let timed_out = matches!(kind.as_deref(), Some("iceTimeout" | "udpBlocked"));
    ledger.record(
        WITNESSES[5],
        timed_out && (relayed_moved ^ blocked_moved),
        format!(
            "a third offer to the same peer, with nobody answering it: outcome={kind:?}. \
             ice_relayed {} → {}, udp_blocked {} → {} — exactly one moved, which is the \
             partition: the deadline's evidence decides which, and udp_blocked is claimed \
             ONLY where UdpBlockedEvidence was actually established. The pair was left \
             relayed rather than failed: a relayed session is not a failure, it is §9 step \
             6's disposition.",
            counter_of(&before, "ice_relayed"),
            counter_of(&after, "ice_relayed"),
            counter_of(&before, "udp_blocked"),
            counter_of(&after, "udp_blocked"),
        ),
    );

    // --- 6. supersession ------------------------------------------
    //
    // Two overlapping attempts with the same peer. The first must be
    // told, typed, that a newer one replaced it — and the one it
    // lost to must be counted, because an attempt that vanished from
    // the ledger is how the partition drifts.
    let before = counters(&mut script, TAB_A).await;
    let first = script
        .run(
            TAB_A,
            Step5::PeerOffer {
                id: 0,
                session: "peer".to_string(),
                peer_hex: b.node_hex.clone(),
            },
        )
        .await;
    let second = script
        .run(
            TAB_A,
            Step5::PeerOffer {
                id: 0,
                session: "peer".to_string(),
                peer_hex: b.node_hex.clone(),
            },
        )
        .await;
    let poll = script
        .run(
            TAB_A,
            Step5::PeerCandidate {
                id: 0,
                session: "peer".to_string(),
                peer_hex: b.node_hex.clone(),
            },
        )
        .await;
    let after = counters(&mut script, TAB_A).await;
    let first_dialog = stat_str(&first, "dialog").unwrap_or_default();
    let second_dialog = stat_str(&second, "dialog").unwrap_or_default();
    let live_dialog = stat_str(&poll, "dialog").unwrap_or_default();
    let superseded = !first_dialog.is_empty()
        && first_dialog != second_dialog
        && live_dialog == second_dialog
        && counter_of(&after, "ice_failed") > counter_of(&before, "ice_failed")
        && counter_of(&after, "ice_attempted") >= counter_of(&before, "ice_attempted") + 2;
    ledger.record(
        WITNESSES[6],
        superseded,
        format!(
            "two overlapping offers to one peer: dialogs {first_dialog} then \
             {second_dialog}; the live attempt is now {live_dialog}. A caller holding \
             {first_dialog} reads a dialog that is not its own, which is what \
             `connectPeer` types as `superseded` — it cannot be reported on the attempt \
             itself, because the attempt it would report about is gone. \
             ice_attempted {} → {} (both offers counted), ice_failed {} → {} (the retired \
             attempt was counted, not forgotten).",
            counter_of(&before, "ice_attempted"),
            counter_of(&after, "ice_attempted"),
            counter_of(&before, "ice_failed"),
            counter_of(&after, "ice_failed"),
        ),
    );

    // --- 7. the ledger partitions ----------------------------------
    //
    // The identity the conformance matrix asserts on every row, read
    // on both leaves at the end of the run. `pending` is the residual
    // and must be zero: an in-flight attempt is a legitimate
    // off-by-one and this is where it would otherwise look like a
    // defect.
    //
    // The attempt the supersede witness left live has to REACH a
    // terminal term first. Not a widened wait: `pending` is exactly
    // the count of attempts still in flight, and reading the ledger
    // with one of them outstanding would report a legitimate
    // off-by-one as a broken partition. So the runner drives that
    // attempt to its own deadline and then reads.
    for _ in 0..CANDIDATE_POLLS {
        let poll = script
            .run(
                TAB_A,
                Step5::PeerCandidate {
                    id: 0,
                    session: "peer".to_string(),
                    peer_hex: b.node_hex.clone(),
                },
            )
            .await;
        let state = stat_str(&poll, "state").unwrap_or_default();
        if !poll.ok || matches!(state.as_str(), "iceTimeout" | "udpBlocked") {
            break;
        }
        if state == "open" {
            // The channel came up, so this attempt is not going to
            // end at a deadline: finish it the way the surface says
            // an open channel is finished.
            let _ = script
                .run(
                    TAB_A,
                    Step5::PeerHandshake {
                        id: 0,
                        session: "peer".to_string(),
                        peer_hex: b.node_hex.clone(),
                    },
                )
                .await;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let ledger_a = counters(&mut script, TAB_A).await;
    let ledger_b = counters(&mut script, TAB_B).await;
    let part_a = partition(&ledger_a);
    let part_b = partition(&ledger_b);
    ledger.record(
        WITNESSES[7],
        part_a.holds() && part_b.holds(),
        format!(
            "leaf a: {part_a}; leaf b: {part_b}. An attempt is one signalling DIALOG, and \
             the leaf's own bootstrap dialog with the anchor is one of them — so the \
             denominator is direct-path attempts and NOT pairs that got off the anchor. \
             Every term is non-zero-checked through ice_attempted, and pending is the \
             residual rather than an assumption about quiescence.",
        ),
    );

    // --- 8. a handshake the peer cannot answer ---------------------
    //
    // The fourth typed disposition, and the last one without a
    // witness. It is the only one that cannot be reached through
    // `connectPeer` from the outside: `connectPeer` handshakes the
    // instant its own poll reads `open`, so a peer that has to be
    // gone by then cannot be taken away in time — the offerer would
    // win the race about half the time and the witness would be a
    // coin. So the runner drives the four methods to an open
    // channel itself, closes the answerer's node while nothing is
    // in flight, and then asks for `connectPeer`'s last step
    // through `handshakePeer` — the same call, the same classifier,
    // a failure the runner placed rather than hoped for.
    //
    // What fails is a REAL handshake, and the refusal is the leaf's
    // own observation rather than a string the harness made up:
    // closing the answerer's node closes its `RTCPeerConnection`,
    // the offerer's DataChannel to it leaves `open`, and
    // `peer_handshake` will not begin NKpsk0 on a channel that is
    // not open — sending message 1 over the relay instead would
    // install the "direct" session through the anchor, which is the
    // one thing §9 step 4 exists to stop doing. So the channel
    // opened, the session did not install, and that pair of facts
    // is exactly what `handshakeFailed` means.
    let before = counters(&mut script, TAB_A).await;
    let orphan = drive_to_open(&mut script, &a.node_hex, &b.node_hex).await;
    let closed_b = script
        .run(
            TAB_B,
            Step5::Close {
                id: 0,
                session: "peer".to_string(),
            },
        )
        .await;
    let vanished = match &orphan {
        Ok(dialog) => {
            script
                .run(
                    TAB_A,
                    Step5::PeerHandshakeTyped {
                        id: 0,
                        session: "peer".to_string(),
                        peer_hex: b.node_hex.clone(),
                        dialog: dialog.clone(),
                    },
                )
                .await
        }
        Err(_) => StepResult::default(),
    };
    let after = counters(&mut script, TAB_A).await;
    let a_view = attempt(&mut script, TAB_A, &b.node_hex).await;
    let typed = outcome(&vanished);
    let no_session = !stat_bool(&a_view, "direct");
    let uncounted_direct = counter_of(&after, "ice_direct") == counter_of(&before, "ice_direct");
    ledger.record(
        WITNESSES[8],
        orphan.is_ok()
            && closed_b.ok
            && typed.as_deref() == Some("handshakeFailed")
            && no_session
            && uncounted_direct,
        format!(
            "the four methods drove dialog {:?} to an open DataChannel, the answerer's node \
             was then CLOSED, and the offerer's last step was asked for through \
             `handshakePeer` — `connectPeer`'s own call and `connectPeer`'s own classifier: \
             outcome={typed:?}, detail={:?}. The detail is the leaf's, not the harness's. \
             The offerer's session with the peer is still not direct ({}), and ice_direct \
             did not move ({} → {}): a channel that opened and a session that did not \
             install is what `handshakeFailed` means, and it is the one typed failure \
             `connectPeer` cannot be raced into from outside — it decides to handshake the \
             instant its own poll reads `open`, so a peer that has to be gone by then \
             cannot be taken away in time.",
            orphan.as_ref().map_err(String::as_str),
            stat_str(&vanished, "detail"),
            stat_bool(&a_view, "direct"),
            counter_of(&before, "ice_direct"),
            counter_of(&after, "ice_direct"),
        ),
    );

    let _ = script
        .run(
            TAB_A,
            Step5::Close {
                id: 0,
                session: "peer".to_string(),
            },
        )
        .await;
    let _ = script
        .run(
            TAB_B,
            Step5::Close {
                id: 0,
                session: "peer".to_string(),
            },
        )
        .await;
    let _ = script.run(TAB_A, Step5::Done { id: 0 }).await;
    let _ = script.run(TAB_B, Step5::Done { id: 0 }).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let _ = cx.driver.close_page(PAGE_A).await;
    let _ = cx.driver.close_page(PAGE_B).await;
    println!("[stage6] {} — both contexts closed", cx.engine.as_str());
    Ok(())
}

/// What one fine-grained drive produced.
struct RawDrive {
    offer_dialog: Option<String>,
    answer_dialog: Option<String>,
    polls: usize,
    opened_after: Option<usize>,
    installed: bool,
    detail: String,
}

/// Drive one attempt through the four `#[wasm_bindgen]` methods, one
/// call at a time.
///
/// Deliberately not a loop inside the page: the runner holds the
/// sequence so a witness can observe — and, in other witnesses,
/// intervene — between any two steps. That is the reason the
/// fine-grained family exists beside `connectPeer` at all.
async fn drive_raw(script: &mut Script5, a: &Leaf, b: &Leaf) -> RawDrive {
    let offer = script
        .run(
            TAB_A,
            Step5::PeerOffer {
                id: 0,
                session: "peer".to_string(),
                peer_hex: b.node_hex.clone(),
            },
        )
        .await;
    let offer_dialog = stat_str(&offer, "dialog");
    if !offer.ok {
        return RawDrive {
            offer_dialog,
            answer_dialog: None,
            polls: 0,
            opened_after: None,
            installed: false,
            detail: format!("peer_offer refused: {:?}", offer.error),
        };
    }

    // The offer is IN FLIGHT: it crossed A → anchor → B as a
    // `0x0D02` frame and B files it when it next pumps. So the
    // answerer is asked until the envelope is there, not once and
    // then judged — a message in flight is not a refusal, and the
    // bound below is the runner's own patience, never a widened
    // assertion.
    let mut answer = StepResult::default();
    for _ in 0..40 {
        answer = script
            .run(
                TAB_B,
                Step5::PeerAcceptOffer {
                    id: 0,
                    session: "peer".to_string(),
                    peer_hex: a.node_hex.clone(),
                },
            )
            .await;
        if answer.ok {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let answer_dialog = stat_str(&answer, "dialog");
    if !answer.ok {
        return RawDrive {
            offer_dialog,
            answer_dialog,
            polls: 0,
            opened_after: None,
            installed: false,
            detail: format!("peer_accept_offer refused: {:?}", answer.error),
        };
    }

    // Both sides service their own attempt: candidates only leave a
    // leaf when something asks it to trickle, and a one-sided pump is
    // a half-gathered attempt that times out for no reason a reader
    // would guess.
    let mut polls = 0;
    let mut opened_after = None;
    for round in 1..=CANDIDATE_POLLS {
        let from_a = script
            .run(
                TAB_A,
                Step5::PeerCandidate {
                    id: 0,
                    session: "peer".to_string(),
                    peer_hex: b.node_hex.clone(),
                },
            )
            .await;
        let from_b = script
            .run(
                TAB_B,
                Step5::PeerCandidate {
                    id: 0,
                    session: "peer".to_string(),
                    peer_hex: a.node_hex.clone(),
                },
            )
            .await;
        polls = round;
        let a_state = stat_str(&from_a, "state").unwrap_or_default();
        let b_state = stat_str(&from_b, "state").unwrap_or_default();
        if a_state == "open" && b_state == "open" {
            opened_after = Some(round);
            break;
        }
        if matches!(a_state.as_str(), "iceTimeout" | "udpBlocked") {
            return RawDrive {
                offer_dialog,
                answer_dialog,
                polls,
                opened_after: None,
                installed: false,
                detail: format!(
                    "the offerer's attempt ended as {a_state} before the channel opened"
                ),
            };
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let handshake = script
        .run(
            TAB_A,
            Step5::PeerHandshake {
                id: 0,
                session: "peer".to_string(),
                peer_hex: b.node_hex.clone(),
            },
        )
        .await;
    let a_view = attempt(script, TAB_A, &b.node_hex).await;
    let b_view = attempt(script, TAB_B, &a.node_hex).await;
    let installed = handshake.ok && stat_bool(&a_view, "direct") && stat_bool(&b_view, "direct");
    RawDrive {
        offer_dialog,
        answer_dialog,
        polls,
        opened_after,
        installed,
        detail: format!(
            "handshake ok={} error={:?}; a.direct={} b.direct={} — the answerer never ran a \
             handshake, its half installed from the inbound message, which is §9 step 4's \
             \"Noise in the offerer's role\"",
            handshake.ok,
            handshake.error,
            stat_bool(&a_view, "direct"),
            stat_bool(&b_view, "direct"),
        ),
    }
}

/// Drive a fresh attempt through `peer_offer`, `peer_accept_offer`
/// and `peer_candidate` until BOTH sides read `open`, and return the
/// offerer's dialog id.
///
/// The offerer's handshake is deliberately NOT run: this exists for
/// the witness that has to do something to the answerer while the
/// channel is open and no handshake is in flight, and running the
/// last step here would be exactly the race that witness exists to
/// avoid. Errors are `Err(reason)` rather than a panic, because a
/// witness records a failure and the run continues.
async fn drive_to_open(script: &mut Script5, a_hex: &str, b_hex: &str) -> Result<String, String> {
    let offer = script
        .run(
            TAB_A,
            Step5::PeerOffer {
                id: 0,
                session: "peer".to_string(),
                peer_hex: b_hex.to_string(),
            },
        )
        .await;
    if !offer.ok {
        return Err(format!("peer_offer refused: {:?}", offer.error));
    }
    let dialog = stat_str(&offer, "dialog").ok_or("peer_offer reported no dialog")?;

    // **The answerer may be holding an older offer.** Two earlier
    // witnesses offer to this peer and never have it answer — that
    // is the whole point of "an offer nobody answers" and of the
    // supersession pair — and each of those envelopes was verified
    // and filed on the answerer. `peer_accept_offer` answers the
    // offer it holds, so the first call here can legitimately
    // answer one of THOSE: the answer then names a dialog the
    // offerer no longer owns, the offerer drops it (correctly —
    // `file_signal` refuses an envelope for a dialog it is not
    // driving), and both sides gather until their deadlines.
    //
    // So the answerer is asked until the dialog it answers is the
    // one this function offered. Each call consumes exactly one
    // filed offer, so the stale ones drain and the fresh one is
    // reached; the count is reported, because "how many offers was
    // this peer sitting on" is a fact about the preceding witnesses
    // and not noise.
    let mut answer = StepResult::default();
    let mut stale = 0usize;
    for _ in 0..40 {
        answer = script
            .run(
                TAB_B,
                Step5::PeerAcceptOffer {
                    id: 0,
                    session: "peer".to_string(),
                    peer_hex: a_hex.to_string(),
                },
            )
            .await;
        if answer.ok && stat_str(&answer, "dialog").as_deref() == Some(dialog.as_str()) {
            break;
        }
        if answer.ok {
            stale += 1;
            continue;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    if !answer.ok {
        return Err(format!("peer_accept_offer refused: {:?}", answer.error));
    }
    let answered = stat_str(&answer, "dialog").unwrap_or_default();
    if answered != dialog {
        return Err(format!(
            "the answerer kept answering other dialogs: offered {dialog}, answered \
             {answered} after draining {stale}"
        ));
    }

    for _ in 1..=CANDIDATE_POLLS {
        let from_a = attempt(script, TAB_A, b_hex).await;
        let from_b = attempt(script, TAB_B, a_hex).await;
        let a_state = stat_str(&from_a, "state").unwrap_or_default();
        let b_state = stat_str(&from_b, "state").unwrap_or_default();
        if a_state == "open" && b_state == "open" {
            return Ok(dialog);
        }
        if matches!(a_state.as_str(), "iceTimeout" | "udpBlocked") {
            return Err(format!("the offerer's attempt ended as {a_state}"));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Err("the channel never opened inside the runner's own bound".to_string())
}

/// Poll `tab`'s capability query until it holds `peer_hex`, and return
/// the Noise key it learned.
async fn discover(script: &mut Script5, tab: &str, peer_hex: &str) -> Option<String> {
    let deadline = tokio::time::Instant::now() + DISCOVERY_DEADLINE;
    while tokio::time::Instant::now() < deadline {
        let seen = script
            .run(
                tab,
                Step5::Query {
                    id: 0,
                    session: "peer".to_string(),
                    capability: PEER_TAG.to_string(),
                },
            )
            .await;
        if let Some(found) = noise_key_for(&seen, peer_hex) {
            return Some(found);
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    None
}

/// The `noisePubkey` a query result carries for `peer_hex`.
///
/// Read out of the descriptor rather than asserted from the harness's
/// own knowledge: the point of the witness is that the key reached the
/// leaf through a signature it verified, so the observable has to be
/// what the LEAF holds. The keys are camelCase because the wrapper's
/// `parseDescriptors` hands the page a `NodeDescriptor`, and `nodeId`
/// is an exact DECIMAL string — `JSON.parse` rounds above 2^53, so a
/// numeric node id would name the wrong node.
fn noise_key_for(result: &StepResult, peer_hex: &str) -> Option<String> {
    let peers = result.peers.as_ref()?;
    let wanted = u64::from_str_radix(peer_hex.trim_start_matches("0x"), 16).ok()?;
    for entry in peers.as_array()? {
        let Some(node) = entry.get("nodeId").and_then(|v| v.as_str()) else {
            continue;
        };
        let matches = node.parse::<u64>().is_ok_and(|id| id == wanted)
            || u64::from_str_radix(node.trim_start_matches("0x"), 16).is_ok_and(|id| id == wanted);
        if matches {
            return entry
                .get("noisePubkey")
                .and_then(|v| v.as_str())
                .map(str::to_string);
        }
    }
    None
}

/// `{ ok, node_id }` from a `Connect` result.
fn leaf_of(result: &StepResult) -> Option<Leaf> {
    if !result.ok {
        return None;
    }
    let hex = result.node_id.clone()?;
    let id = u64::from_str_radix(hex.trim_start_matches("0x"), 16).ok()?;
    Some(Leaf {
        node_hex: hex,
        node_id: id,
    })
}

/// The `outcome` a `peer_connect` / `peer_accept` step reported.
fn outcome(result: &StepResult) -> Option<String> {
    stat_str(result, "outcome")
}

/// One service of a live attempt, as the page reports it.
async fn attempt(script: &mut Script5, tab: &str, peer_hex: &str) -> StepResult {
    script
        .run(
            tab,
            Step5::PeerAttempt {
                id: 0,
                session: "peer".to_string(),
                peer_hex: peer_hex.to_string(),
            },
        )
        .await
}

/// The whole attempt ledger, with no attempt required.
async fn counters(script: &mut Script5, tab: &str) -> StepResult {
    script
        .run(
            tab,
            Step5::PeerCounters {
                id: 0,
                session: "peer".to_string(),
            },
        )
        .await
}

/// One counter out of a step's `stats.counters` object.
///
/// Every value there is a decimal STRING, because `JSON.parse` rounds
/// integers above 2^53 and a rounded counter is a wrong counter that
/// looks right.
fn counter_of(result: &StepResult, name: &str) -> u64 {
    result
        .stats
        .as_ref()
        .and_then(|s| s.get("counters"))
        .and_then(|c| c.get(name))
        .and_then(|v| v.as_str())
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

/// The §10 attempt partition, as read off one leaf.
struct Partition {
    attempted: u64,
    direct: u64,
    relayed: u64,
    failed: u64,
    blocked: u64,
}

impl Partition {
    /// `direct + relayed + failed + udp_blocked == attempted`, with a
    /// zero residual and a non-zero denominator.
    ///
    /// All three together, deliberately: `0 == 0` satisfies the
    /// identity and proves nothing, and a non-zero residual is an
    /// attempt still in flight rather than a broken partition — which
    /// is a different bug report.
    fn holds(&self) -> bool {
        self.attempted > 0
            && self.direct + self.relayed + self.failed + self.blocked == self.attempted
    }

    fn pending(&self) -> i64 {
        i64::try_from(self.attempted).unwrap_or(i64::MAX)
            - i64::try_from(self.direct + self.relayed + self.failed + self.blocked)
                .unwrap_or(i64::MAX)
    }
}

impl std::fmt::Display for Partition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "attempted={} direct={} relayed={} failed={} udp_blocked={} pending={}",
            self.attempted,
            self.direct,
            self.relayed,
            self.failed,
            self.blocked,
            self.pending()
        )
    }
}

fn partition(result: &StepResult) -> Partition {
    Partition {
        attempted: counter_of(result, "ice_attempted"),
        direct: counter_of(result, "ice_direct"),
        relayed: counter_of(result, "ice_relayed"),
        failed: counter_of(result, "ice_failed"),
        blocked: counter_of(result, "udp_blocked"),
    }
}

/// The anchor's own view of the pair, appended to the headline
/// witness so a failure is not left as "the page said no".
fn peer_view(anchor: &MeshNode, a: u64, b: u64) -> String {
    format!(
        "anchor: a session={:?} provisional={}, b session={:?} provisional={}",
        anchor.peer_session_id(a),
        anchor.peer_is_provisional(a),
        anchor.peer_session_id(b),
        anchor.peer_is_provisional(b),
    )
}

fn stat_str(result: &StepResult, key: &str) -> Option<String> {
    result
        .stats
        .as_ref()
        .and_then(|s| s.get(key))
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

fn stat_u64(result: &StepResult, key: &str) -> u64 {
    result
        .stats
        .as_ref()
        .and_then(|s| s.get(key))
        .and_then(|v| {
            v.as_u64()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        })
        .unwrap_or(0)
}

fn stat_bool(result: &StepResult, key: &str) -> bool {
    result
        .stats
        .as_ref()
        .and_then(|s| s.get(key))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}
