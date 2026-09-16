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
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot};

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
pub const WITNESSES: [&str; 15] = [
    "stage6_two_isolated_contexts_reach_a_direct_session",
    "stage6_each_leaf_learns_the_others_keys_from_a_signed_announcement",
    "stage6_the_four_wasm_methods_drive_one_attempt_end_to_end",
    "stage6_peer_handshake_takes_a_peer_id_and_nothing_else",
    "stage6_an_undiscovered_peer_is_refused_before_an_offer_exists",
    "stage6_an_offer_nobody_answers_leaves_the_pair_relayed",
    "stage6_a_second_offer_supersedes_the_live_attempt",
    "stage6_the_ice_attempt_ledger_partitions_on_both_leaves",
    "stage6_a_handshake_the_peer_cannot_answer_is_typed_handshake_failed",
    "stage6_routed_peer_app_data_moves_the_anchors_per_pair_counter",
    "stage6_direct_peer_app_data_leaves_that_counter_flat_while_the_anchor_is_live",
    "stage6_forcing_the_direct_channel_down_moves_the_counter_again",
    "stage6_a_network_change_drives_exactly_one_re_attempt",
    "stage6_rtc_stats_uses_the_native_field_names_and_names_what_it_cannot_measure",
    "stage6_one_node_id_spelling_across_both_signalling_surfaces",
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
    /// The three slice 2 tabs, on their own `/harness/step6` queues
    /// and their own page (`page/peer6.js`). Separate from `tabs`
    /// because the §10 witness is a different page with a different
    /// vocabulary, and because a tab of one page long-polling the
    /// other's queue would resolve steps it cannot execute.
    pub peer_tabs: HashMap<String, Step6Sender>,
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

    // --- slice 2: the §10 three-part direct-path witness -----------
    //
    // Last, and in three fresh contexts of its own: it opens three
    // more identities and needs the pair it tests to be the only
    // direct pair on the anchor, so it runs after every slice 1 row
    // has been recorded and after both slice 1 contexts are closed.
    direct_path_witness(&cx, ledger).await;
    // --- slice 3: the network-change retry trigger, and rtcStats --
    //
    // After slice 2, in two more fresh contexts: it establishes a
    // direct pair of its own, and slice 2 requires the pair it tests
    // to be the only direct one on the anchor.
    retry_witness(&cx, ledger).await;
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

// ===================================================================
// Stage 6 slice 2 — the §10 three-part direct-path witness
// ===================================================================
//
// §10's witness that a pair is off the anchor has THREE parts,
// because a flat counter alone is also what dropped traffic looks
// like:
//
//   1. routed application data A → anchor → B, with the anchor's
//      per-pair application-data counter MOVING;
//   2. the same traffic after the direct session is installed, with
//      that counter FLAT — and signalling, announcements and an
//      unrelated pair still flowing, because a globally flat anchor
//      is not the witness and "flat because nothing happened" is not
//      the claim;
//   3. the inverse leg — the direct DataChannel closed from the page
//      and the pair manually restored to the relay, with the same
//      counter MOVING AGAIN.
//
// The counter is `MeshNode::forwarded_app_packets[(src_routing_id,
// dest)]`, live on the anchor this harness runs. It EXCLUDES
// `0x0D02`, which is exactly what makes part 2 meaningful: the
// signalling that keeps the pair alive cannot be what moves it.
//
// EVERY payload is nonce-correlated at the RECEIVER. The receiver's
// inbox names the nonces it got and echoes each one back on the same
// stream id, so "the bytes arrived" is the receiver's own report and
// the sender's round trip — never an inference from a counter. A
// counter that moves and a payload that arrives are different facts
// and each part asserts both.
//
// THREE contexts, not two. (A, B) is the pair under test; (C, B) is
// an UNRELATED pair whose application data the anchor is forwarding
// during part 2's flat window. Without it, "the counter for this
// pair is flat" cannot be distinguished from "this anchor is not
// forwarding anything any more", which is the reading §10 warns
// about.
//
// Why the pair really is on the relay in parts 1 and 3, rather than
// probably: addressing, not luck. `ensure_relayed_session` sets the
// peer's relay to the anchor and only `direct_installed` clears it,
// so until the page's own `peer_handshake` installs a session, every
// packet for that peer is routed — whatever ICE has managed in the
// meantime. Parts 1 and 3 never call `peer_handshake`.

/// The capability tag the three slice 2 leaves announce and discover
/// each other by.
///
/// Distinct from [`PEER_TAG`] on purpose: a query here must never
/// return a slice 1 tab, whose node is closed by the time this runs.
const P2_TAG: &str = "stage6.direct-path";

/// The stream labels. Both ends of one pair derive the same stream
/// id from the same label (`stream_id_from_label`), so neither side
/// is ever told an id.
///
/// **One label per PAIR, and this is not decoration.** A leaf-opened
/// stream id is derived from the label ALONE — the peer is not in it
/// — and the wasm boundary's `on_message` filter keys on the stream
/// id ALONE. So two peer-addressed streams opened under one label on
/// one leaf share an id, and every payload from either peer is
/// delivered to BOTH consumers. The first run of this witness caught
/// exactly that: B's stream to A reported the unrelated pair's
/// nonces, and B echoed C's payloads to A. Per-pair labels are the
/// page-side fix, and the sharp edge is reported as a Stage 6 gap.
const P2_LABEL_AB: &str = "s6.app.ab";
const P2_LABEL_CB: &str = "s6.app.cb";
/// Part 3's restoration runs on a FRESH id, for a second reason: the
/// send that could not cross the closed channel is still owned by
/// the reliable stream it was sent on, so a retransmit can deliver
/// it once the relay is back — arriving in the middle of the
/// restored phase's payloads. A new id leaves that history behind
/// (the old stream is closed, so late arrivals on it are dropped and
/// counted rather than delivered), and the counter the row asserts
/// on is per-PAIR and does not care which stream moved it.
const P2_LABEL_AB_RESTORED: &str = "s6.app.ab.restored";

/// The three slice 2 tabs. Each has its own `/harness/step6` queue.
pub const PEER_TABS: [&str; 3] = [P2_TAB_A, P2_TAB_B, P2_TAB_C];

const P2_TAB_A: &str = "f";
const P2_TAB_B: &str = "g";
const P2_TAB_C: &str = "h";

const P2_PAGE_A: &str = "peer6-a";
const P2_PAGE_B: &str = "peer6-b";
const P2_PAGE_C: &str = "peer6-c";

const P2_CTX_A: &str = "stage6-p2a";
const P2_CTX_B: &str = "stage6-p2b";
const P2_CTX_C: &str = "stage6-p2c";

/// Three fixed identities, so a failure is reproducible and so no
/// two of the three tabs can accidentally share one.
const P2_A_ENTITY: &str = "c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1";
const P2_A_NOISE: &str = "c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2";
const P2_B_ENTITY: &str = "d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1";
const P2_B_NOISE: &str = "d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2";
const P2_C_ENTITY: &str = "e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1";
const P2_C_NOISE: &str = "e2e2e2e2e2e2e2e2e2e2e2e2e2e2e2e2e2e2e2e2e2e2e2e2e2e2e2e2e2e2e2e2";

/// How long the runner waits for a leaf to hold a session with a peer
/// it can open a stream to. The relayed Noise handshake is one round
/// trip through the anchor; this is the runner's own bound on
/// noticing it landed.
const P2_SESSION_WAIT: Duration = Duration::from_secs(10);

/// How long a receiver's inbox waits for payloads it expects.
const P2_INBOX_MS: u64 = 8_000;

/// How long the "nothing gets through" probe waits with the direct
/// channel closed and the relay not yet restored.
///
/// Short by design and not an assertion about timing: the claim is
/// that nothing arrived AND the anchor forwarded nothing for the
/// pair, and a longer wait would only make a leak arrive later.
const P2_DOWN_MS: u64 = 2_000;

/// One queued slice 2 step and the channel its result comes back on.
///
/// A `Value` rather than an enum: `page/peer6.js` is its own page
/// with its own vocabulary, and `stage5::Step5` is the Stage 5
/// page's. The two never share a queue either — see [`PEER_TABS`].
pub type Step6Item = (Value, oneshot::Sender<StepResult>);
pub type Step6Sender = mpsc::Sender<Step6Item>;
pub type Step6Queue = Arc<tokio::sync::Mutex<mpsc::Receiver<Step6Item>>>;

/// The three slice 2 witness names, in ledger order.
const P2_WITNESSES: [&str; 3] = [WITNESSES[9], WITNESSES[10], WITNESSES[11]];

/// Drives `page/peer6.js`, numbering its steps so they can never
/// collide with the 4b or Stage 5 scripts' ids — every script in this
/// runner posts to one `/harness/result` keyed by step id.
struct Script6 {
    tabs: HashMap<String, Step6Sender>,
    next_id: u64,
}

impl Script6 {
    fn new(tabs: HashMap<String, Step6Sender>, id_base: u64) -> Self {
        Self {
            tabs,
            next_id: id_base,
        }
    }

    async fn run(&mut self, tab: &str, mut step: Value) -> StepResult {
        let id = self.next_id;
        self.next_id += 1;
        step["id"] = json!(id);
        let Some(tx) = self.tabs.get(tab) else {
            return fail6(format!("no slice 2 page tab named {tab}"));
        };
        let (reply_tx, reply_rx) = oneshot::channel();
        if tx.send((step, reply_tx)).await.is_err() {
            return fail6("the page server is gone");
        }
        match tokio::time::timeout(Duration::from_secs(120), reply_rx).await {
            Ok(Ok(r)) => r,
            Ok(Err(_)) => fail6("the step was dropped"),
            Err(_) => fail6("the page did not answer this step in 120 s"),
        }
    }
}

fn fail6(msg: impl Into<String>) -> StepResult {
    StepResult {
        ok: false,
        error: Some(msg.into()),
        ..Default::default()
    }
}

/// The anchor's per-pair application-data counters and its forwarded
/// `0x0D02` count, read together so one sample is one instant.
struct Forwarded {
    /// `(A → B)` application data the anchor forwarded.
    ab: u64,
    /// `(B → A)`, which the echoes ride.
    ba: u64,
    /// `(C → B)` — the UNRELATED pair.
    cb: u64,
    /// `0x0D02` frames the anchor forwarded for any pair it relays.
    signal: u64,
}

impl Forwarded {
    fn read(anchor: &MeshNode, a: &Leaf, b: &Leaf, c: &Leaf) -> Self {
        Self {
            ab: anchor.forwarded_app_packets(routing_id(a), b.node_id),
            ba: anchor.forwarded_app_packets(routing_id(b), a.node_id),
            cb: anchor.forwarded_app_packets(routing_id(c), b.node_id),
            signal: anchor.rtc_stats().signal_forwarded(),
        }
    }
}

/// The `src_id` half of the anchor's key: the low 32 bits of a node
/// id, which is what a routing header carries.
fn routing_id(leaf: &Leaf) -> u32 {
    u32::try_from(leaf.node_id & 0xFFFF_FFFF).expect("the low 32 bits of a u64 fit a u32")
}

/// Mint the nonces one phase sends.
///
/// Unique per run and per phase, and printed in the ledger, so a
/// payload that arrived is correlated to the send that produced it
/// rather than to "three things showed up".
fn nonces(run: u64, phase: &str, count: usize) -> Vec<String> {
    (0..count)
        .map(|i| format!("{phase}-{run:012x}-{i}"))
        .collect()
}

/// A `stats` array of strings — an inbox's nonces, in arrival order.
fn stat_list(result: &StepResult, key: &str) -> Vec<String> {
    result
        .stats
        .as_ref()
        .and_then(|s| s.get(key))
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// The announcement version `tab` holds for `peer_hex`, as the
/// descriptor spells it.
fn version_for(result: &StepResult, peer_hex: &str) -> Option<u64> {
    let peers = result.peers.as_ref()?;
    let wanted = u64::from_str_radix(peer_hex.trim_start_matches("0x"), 16).ok()?;
    for entry in peers.as_array()? {
        let node = entry.get("nodeId").and_then(|v| v.as_str())?;
        if node.parse::<u64>().is_ok_and(|id| id == wanted) {
            return entry
                .get("version")
                .and_then(|v| v.as_str())
                .and_then(|v| v.parse().ok());
        }
    }
    None
}

/// Poll `tab`'s capability query until it holds `peer_hex`'s signed
/// announcement, and return the Noise key it learned from it.
async fn p2_discover(script: &mut Script6, tab: &str, peer_hex: &str) -> Option<String> {
    let deadline = tokio::time::Instant::now() + DISCOVERY_DEADLINE;
    while tokio::time::Instant::now() < deadline {
        let seen = p2_query(script, tab).await;
        if let Some(found) = noise_key_for(&seen, peer_hex) {
            return Some(found);
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    None
}

async fn p2_query(script: &mut Script6, tab: &str) -> StepResult {
    script
        .run(
            tab,
            json!({ "kind": "query", "session": "p2", "capability": P2_TAG }),
        )
        .await
}

/// Open — or REOPEN — a peer-addressed stream under `handle`.
///
/// Retried only while the refusal is "no session with this peer":
/// the relayed Noise handshake is one round trip through the anchor
/// and the answering side opens its stream as soon as that lands.
/// Every other refusal is returned at once, because retrying those
/// would hide them.
async fn p2_open_stream(
    script: &mut Script6,
    tab: &str,
    handle: &str,
    peer_hex: &str,
    label: &str,
    echo: bool,
) -> StepResult {
    let deadline = tokio::time::Instant::now() + P2_SESSION_WAIT;
    loop {
        let opened = script
            .run(
                tab,
                json!({
                    "kind": "stream_open",
                    "session": "p2",
                    "handle": handle,
                    "peer_hex": peer_hex,
                    "label": label,
                    "reliable": true,
                    "echo": echo,
                }),
            )
            .await;
        if opened.ok {
            return opened;
        }
        let no_session = opened
            .message
            .as_deref()
            .is_some_and(|m| m.contains("no session with"));
        if !no_session || tokio::time::Instant::now() >= deadline {
            return opened;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn p2_send(script: &mut Script6, tab: &str, handle: &str, payloads: &[String]) -> StepResult {
    script
        .run(
            tab,
            json!({
                "kind": "stream_send",
                "session": "p2",
                "handle": handle,
                "nonces": payloads,
            }),
        )
        .await
}

async fn p2_inbox(
    script: &mut Script6,
    tab: &str,
    handle: &str,
    data: usize,
    echo: usize,
    timeout_ms: u64,
) -> StepResult {
    script
        .run(
            tab,
            json!({
                "kind": "stream_inbox",
                "session": "p2",
                "handle": handle,
                "expect_data": data,
                "expect_echo": echo,
                "timeout_ms": timeout_ms,
            }),
        )
        .await
}

async fn p2_offer(script: &mut Script6, tab: &str, peer_hex: &str) -> StepResult {
    script
        .run(
            tab,
            json!({ "kind": "offer", "session": "p2", "peer_hex": peer_hex }),
        )
        .await
}

async fn p2_candidate(script: &mut Script6, tab: &str, peer_hex: &str) -> StepResult {
    script
        .run(
            tab,
            json!({ "kind": "candidate", "session": "p2", "peer_hex": peer_hex }),
        )
        .await
}

/// Answer `peer_hex`'s offer until the dialog answered is `dialog`.
///
/// An earlier phase can leave an unanswered offer filed on the
/// answerer, and `peer_accept_offer` legitimately takes the oldest
/// one first — so the stale ones are DRAINED rather than tolerated.
/// Returns the dialog it answered, which the caller compares against
/// the one the offerer minted: the answerer reads it off the signed
/// envelope and is never told it.
async fn p2_answer_matching(
    script: &mut Script6,
    tab: &str,
    peer_hex: &str,
    dialog: &str,
) -> Result<String, String> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let mut last = String::new();
    while tokio::time::Instant::now() < deadline {
        let answered = script
            .run(
                tab,
                json!({ "kind": "answer", "session": "p2", "peer_hex": peer_hex }),
            )
            .await;
        if answered.ok {
            let got = stat_str(&answered, "dialog").unwrap_or_default();
            if got == dialog {
                return Ok(got);
            }
            last = format!("answered a stale dialog {got}, waiting for {dialog}");
        } else {
            last = answered
                .message
                .or(answered.error)
                .unwrap_or_else(|| "peer_accept_offer refused without a reason".to_string());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err(format!(
        "no answer for dialog {dialog} in 20 s; last: {last}"
    ))
}

/// Drive one attempt through the four `#[wasm_bindgen]` methods until
/// BOTH leaves hold a direct session, and return the dialog id.
///
/// The runner holds the sequence — not `connectPeer` — because §10
/// needs to send application data while the pair is still relayed,
/// which means stopping between `peer_offer` and the handshake that
/// replaces the routed session.
async fn p2_drive_direct(script: &mut Script6, a_hex: &str, b_hex: &str) -> Result<String, String> {
    let offered = p2_offer(script, P2_TAB_A, b_hex).await;
    if !offered.ok {
        return Err(format!(
            "peer_offer refused: {:?}",
            offered.message.or(offered.error)
        ));
    }
    let dialog = stat_str(&offered, "dialog").unwrap_or_default();
    p2_answer_matching(script, P2_TAB_B, a_hex, &dialog).await?;

    let mut opened = false;
    for _ in 0..CANDIDATE_POLLS {
        let a = p2_candidate(script, P2_TAB_A, b_hex).await;
        let b = p2_candidate(script, P2_TAB_B, a_hex).await;
        let a_state = stat_str(&a, "state").unwrap_or_default();
        let b_state = stat_str(&b, "state").unwrap_or_default();
        if !a.ok || !b.ok {
            return Err(format!(
                "peer_candidate refused mid-attempt: a={:?} b={:?}",
                a.message.or(a.error),
                b.message.or(b.error)
            ));
        }
        if matches!(a_state.as_str(), "iceTimeout" | "udpBlocked")
            || matches!(b_state.as_str(), "iceTimeout" | "udpBlocked")
        {
            return Err(format!(
                "the attempt ended at its own deadline before the channel opened: \
                 a={a_state} b={b_state}"
            ));
        }
        if a_state == "open" && b_state == "open" {
            opened = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    if !opened {
        return Err("neither side read `open` inside the runner's poll budget".to_string());
    }

    let shook = script
        .run(
            P2_TAB_A,
            json!({ "kind": "handshake", "session": "p2", "peer_hex": b_hex }),
        )
        .await;
    if !shook.ok {
        return Err(format!(
            "peer_handshake refused: {:?}",
            shook.message.or(shook.error)
        ));
    }

    // Both halves, because a direct session on the offerer alone is
    // half a replacement: the answerer installs when message 1
    // arrives on the channel and clears its own relay there.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while tokio::time::Instant::now() < deadline {
        let a = p2_candidate(script, P2_TAB_A, b_hex).await;
        let b = p2_candidate(script, P2_TAB_B, a_hex).await;
        if stat_bool(&a, "direct") && stat_bool(&b, "direct") {
            return Ok(dialog);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Err("the handshake returned and one side still reads direct=false".to_string())
}

/// Record every slice 2 row failed with one reason — a harness fault
/// that happened before any of the three parts could be attempted.
fn p2_fail_all(ledger: &mut Ledger, detail: &str) {
    for name in P2_WITNESSES {
        ledger.record(name, false, detail.to_string());
    }
}

/// Run the §10 three-part witness. Three rows, each failing on its
/// own evidence.
#[expect(clippy::too_many_lines, reason = "one linear witness script")]
async fn direct_path_witness(cx: &Cx6<'_>, ledger: &mut Ledger) {
    let mut script = Script6::new(cx.peer_tabs.clone(), 3_000_000);
    // Per-run, so a nonce in a ledger line belongs to one run and one
    // phase and nothing else.
    let run = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| {
            u64::try_from(d.as_nanos() & 0xFFFF_FFFF_FFFF).unwrap_or(0)
        });

    for (page, tab, ctx) in [
        (P2_PAGE_A, P2_TAB_A, P2_CTX_A),
        (P2_PAGE_B, P2_TAB_B, P2_CTX_B),
        (P2_PAGE_C, P2_TAB_C, P2_CTX_C),
    ] {
        let url = format!("{}/peer6.html?tab={tab}", cx.page_origin);
        if let Err(why) = cx.driver.open_page_in(page, &url, Some(ctx)).await {
            p2_fail_all(
                ledger,
                &format!("the slice 2 browsing context {ctx} would not open: {why}"),
            );
            return;
        }
    }

    let connect = |entity: &str, noise: &str| {
        json!({
            "kind": "connect",
            "session": "p2",
            "credential": cx.credential,
            "bootstrap_url": cx.bootstrap_url,
            "origin": cx.origin,
            "anchor_rtc_addr": cx.anchor_rtc_addr,
            "stun": cx.stun,
            "entity_secret_hex": entity,
            "noise_secret_hex": noise,
        })
    };
    let a_res = script.run(P2_TAB_A, connect(P2_A_ENTITY, P2_A_NOISE)).await;
    let b_res = script.run(P2_TAB_B, connect(P2_B_ENTITY, P2_B_NOISE)).await;
    let c_res = script.run(P2_TAB_C, connect(P2_C_ENTITY, P2_C_NOISE)).await;
    let leaves = match (leaf_of(&a_res), leaf_of(&b_res), leaf_of(&c_res)) {
        (Some(a), Some(b), Some(c))
            if a.node_id != b.node_id && b.node_id != c.node_id && a.node_id != c.node_id =>
        {
            Some((a, b, c))
        }
        (a, b, c) => {
            p2_fail_all(
                ledger,
                &format!(
                    "the three slice 2 leaves did not come up as three distinct nodes: \
                     a={:?} b={:?} c={:?}; errors a={:?} b={:?} c={:?}",
                    a.map(|l| l.node_hex),
                    b.map(|l| l.node_hex),
                    c.map(|l| l.node_hex),
                    a_res.error,
                    b_res.error,
                    c_res.error,
                ),
            );
            None
        }
    };
    let Some((a, b, c)) = leaves else {
        p2_teardown(cx, &mut script).await;
        return;
    };

    for tab in PEER_TABS {
        let announced = script
            .run(
                tab,
                json!({ "kind": "announce", "session": "p2", "capabilities": [P2_TAG] }),
            )
            .await;
        if !announced.ok {
            println!(
                "[stage6] slice 2 leaf {tab} could not announce: {:?}",
                announced.error
            );
        }
    }
    // Each leaf needs the OTHER's signed announcement before it can
    // offer to it or verify an envelope from it: that is where the
    // Noise key comes from, and it is the only place it comes from.
    let discovered = [
        (P2_TAB_A, b.node_hex.as_str(), "a→b"),
        (P2_TAB_B, a.node_hex.as_str(), "b→a"),
        (P2_TAB_C, b.node_hex.as_str(), "c→b"),
        (P2_TAB_B, c.node_hex.as_str(), "b→c"),
        (P2_TAB_C, a.node_hex.as_str(), "c→a"),
    ];
    for (tab, peer, label) in discovered {
        if p2_discover(&mut script, tab, peer).await.is_none() {
            p2_fail_all(
                ledger,
                &format!(
                    "discovery {label} did not land inside {DISCOVERY_DEADLINE:?}: without the \
                     peer's signed announcement there is no Noise key to handshake against, so \
                     no part of §10 can be attempted"
                ),
            );
            p2_teardown(cx, &mut script).await;
            return;
        }
    }

    // ── part 1: routed application data, counter MOVING ────────────
    //
    // `peer_offer` brings the RELAYED session up (§9 step 2) and
    // leaves the peer's relay addressing in place. Candidates are
    // deliberately not serviced and the handshake is not run, so the
    // pair cannot leave the anchor: only `peer_handshake`'s install
    // clears the relay. The answerer answers so nothing stale is
    // left filed on it, and the dialog it reports is the one the
    // offerer minted — read off the signed envelope, never passed.
    let offered = p2_offer(&mut script, P2_TAB_A, &b.node_hex).await;
    let routed_dialog = stat_str(&offered, "dialog").unwrap_or_default();
    let answered = if offered.ok {
        p2_answer_matching(&mut script, P2_TAB_B, &a.node_hex, &routed_dialog).await
    } else {
        Err(format!(
            "peer_offer refused: {:?}",
            offered.message.clone().or(offered.error.clone())
        ))
    };
    let b_stream =
        p2_open_stream(&mut script, P2_TAB_B, "b2a", &a.node_hex, P2_LABEL_AB, true).await;
    let a_stream = p2_open_stream(
        &mut script,
        P2_TAB_A,
        "a2b",
        &b.node_hex,
        P2_LABEL_AB,
        false,
    )
    .await;
    let routed_nonces = nonces(run, "routed", 3);
    let before = Forwarded::read(cx.anchor, &a, &b, &c);
    let routed_sent = p2_send(&mut script, P2_TAB_A, "a2b", &routed_nonces).await;
    let routed_at_b = p2_inbox(&mut script, P2_TAB_B, "b2a", 3, 0, P2_INBOX_MS).await;
    let routed_echo = p2_inbox(&mut script, P2_TAB_A, "a2b", 0, 3, P2_INBOX_MS).await;
    let after = Forwarded::read(cx.anchor, &a, &b, &c);
    let arrived = stat_list(&routed_at_b, "data");
    let echoed = stat_list(&routed_echo, "echoes");
    let routed_ok = offered.ok
        && answered.is_ok()
        && b_stream.ok
        && a_stream.ok
        && stat_u64(&routed_sent, "sent") == 3
        && arrived == routed_nonces
        && echoed == routed_nonces
        && after.ab > before.ab
        && after.ba > before.ba;
    ledger.record(
        P2_WITNESSES[0],
        routed_ok,
        format!(
            "§10 part 1 — ROUTED. Two isolated contexts, a relayed leaf ↔ leaf session \
             (dialog {routed_dialog}, answered as {answered:?} — the answerer read that id off \
             the SIGNED envelope), and one peer-addressed reliable stream each \
             ({:?} / {:?}). A sent {} payloads {routed_nonces:?}; B's inbox reports \
             {arrived:?} and echoed every one back to A, which reports {echoed:?} — the \
             receiver's own account of the bytes, not an inference from a counter. The \
             ANCHOR's per-pair application-data counter moved in both directions: \
             (a→b) {} → {}, (b→a) {} → {}. It EXCLUDES 0x0D02, so the signalling that set \
             this pair up cannot be what moved it. The pair is on the relay by ADDRESSING \
             rather than by luck: `ensure_relayed_session` set it and only the handshake's \
             install clears it, and no handshake has been run. {}",
            stat_str(&a_stream, "stream_id"),
            stat_str(&b_stream, "stream_id"),
            stat_u64(&routed_sent, "sent"),
            before.ab,
            after.ab,
            before.ba,
            after.ba,
            peer_view(cx.anchor, a.node_id, b.node_id),
        ),
    );

    // ── part 2: direct, counter FLAT while the anchor is live ──────
    let direct = p2_drive_direct(&mut script, &a.node_hex, &b.node_hex).await;
    // The replacement fences the handles §9 step 4 replaced the
    // session under: this send is EXPECTED to be refused, and the
    // refusal is the leaf's own words. Reported, never asserted as a
    // pass condition — it is how the row shows that the stream the
    // flat window uses is a NEW one on the direct session.
    let fenced = p2_send(&mut script, P2_TAB_A, "a2b", &nonces(run, "fenced", 1)).await;
    let a_direct_stream = p2_open_stream(
        &mut script,
        P2_TAB_A,
        "a2b",
        &b.node_hex,
        P2_LABEL_AB,
        false,
    )
    .await;
    let b_direct_stream =
        p2_open_stream(&mut script, P2_TAB_B, "b2a", &a.node_hex, P2_LABEL_AB, true).await;
    // Both inboxes are drained before the baseline is taken, so the
    // flat window compares EXACTLY the payloads it sent. The fenced
    // probe above is expected to be refused, but a probe that
    // somehow landed would otherwise show up as an extra nonce and
    // fail this row for a reason that is not its claim.
    let _ = p2_inbox(&mut script, P2_TAB_B, "b2a", 0, 0, 0).await;
    let _ = p2_inbox(&mut script, P2_TAB_A, "a2b", 0, 0, 0).await;

    // The unrelated pair, and the announcement leg: both must be
    // moving INSIDE the window the flat assertion covers, so the
    // baseline is taken first.
    let flat_before = Forwarded::read(cx.anchor, &a, &b, &c);
    let a_version_before = version_for(&p2_query(&mut script, P2_TAB_C).await, &a.node_hex);

    let c_offer = p2_offer(&mut script, P2_TAB_C, &b.node_hex).await;
    let c_dialog = stat_str(&c_offer, "dialog").unwrap_or_default();
    let c_answered = if c_offer.ok {
        p2_answer_matching(&mut script, P2_TAB_B, &c.node_hex, &c_dialog).await
    } else {
        Err(format!(
            "C's peer_offer refused: {:?}",
            c_offer.message.clone().or(c_offer.error.clone())
        ))
    };
    let b2c = p2_open_stream(&mut script, P2_TAB_B, "b2c", &c.node_hex, P2_LABEL_CB, true).await;
    let c2b = p2_open_stream(
        &mut script,
        P2_TAB_C,
        "c2b",
        &b.node_hex,
        P2_LABEL_CB,
        false,
    )
    .await;

    let direct_nonces = nonces(run, "direct", 3);
    let unrelated_nonces = nonces(run, "unrelated", 2);
    let direct_sent = p2_send(&mut script, P2_TAB_A, "a2b", &direct_nonces).await;
    let unrelated_sent = p2_send(&mut script, P2_TAB_C, "c2b", &unrelated_nonces).await;
    let reannounced = script
        .run(
            P2_TAB_A,
            json!({ "kind": "announce", "session": "p2", "capabilities": [P2_TAG] }),
        )
        .await;
    let direct_at_b = p2_inbox(&mut script, P2_TAB_B, "b2a", 3, 0, P2_INBOX_MS).await;
    let direct_echo = p2_inbox(&mut script, P2_TAB_A, "a2b", 0, 3, P2_INBOX_MS).await;
    let unrelated_at_b = p2_inbox(&mut script, P2_TAB_B, "b2c", 2, 0, P2_INBOX_MS).await;
    let unrelated_echo = p2_inbox(&mut script, P2_TAB_C, "c2b", 0, 2, P2_INBOX_MS).await;
    // The announcement leg: A's new version has to reach C THROUGH
    // the anchor's flood, which is a path that cannot ride the
    // direct pair at all.
    let mut a_version_after = None;
    let version_deadline = tokio::time::Instant::now() + DISCOVERY_DEADLINE;
    while tokio::time::Instant::now() < version_deadline {
        let seen = version_for(&p2_query(&mut script, P2_TAB_C).await, &a.node_hex);
        if seen > a_version_before {
            a_version_after = seen;
            break;
        }
        a_version_after = seen;
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let flat_after = Forwarded::read(cx.anchor, &a, &b, &c);

    let direct_arrived = stat_list(&direct_at_b, "data");
    let direct_echoed = stat_list(&direct_echo, "echoes");
    let unrelated_arrived = stat_list(&unrelated_at_b, "data");
    let unrelated_echoed = stat_list(&unrelated_echo, "echoes");
    let flat = flat_after.ab == flat_before.ab && flat_after.ba == flat_before.ba;
    let unrelated_moved = flat_after.cb > flat_before.cb;
    let signalling_moved = flat_after.signal > flat_before.signal;
    let announcements_moved = a_version_after > a_version_before && a_version_before.is_some();
    let direct_ok = direct.is_ok()
        && a_direct_stream.ok
        && b_direct_stream.ok
        && stat_u64(&direct_sent, "sent") == 3
        && direct_arrived == direct_nonces
        && direct_echoed == direct_nonces
        && flat
        // The unrelated pair is part of the CLAIM, not colour: its
        // streams opened, its payloads arrived at B and its echoes
        // came back, so the anchor was forwarding application data
        // for somebody in the same window it forwarded none for the
        // pair under test.
        && c_offer.ok
        && c_answered.is_ok()
        && b2c.ok
        && c2b.ok
        && stat_u64(&unrelated_sent, "sent") == 2
        && unrelated_arrived == unrelated_nonces
        && unrelated_echoed == unrelated_nonces
        && unrelated_moved
        && signalling_moved
        && announcements_moved;
    ledger.record(
        P2_WITNESSES[1],
        direct_ok,
        format!(
            "§10 part 2 — DIRECT, and FLAT WHILE LIVE. The pair reached a direct session \
             ({direct:?}); the streams part 1 used were fenced by that replacement, which \
             the leaf reported as {:?} — §9 step 4 replaces the session, so the flat window \
             below runs on NEW streams ({:?} / {:?}) on the direct one. Inside one window: \
             A sent {direct_nonces:?}, B's inbox reports {direct_arrived:?} and A has the \
             echoes {direct_echoed:?} — traffic genuinely flowing, proved at the receiver. \
             The anchor's per-pair counter for THIS pair did not move: (a→b) {} → {}, \
             (b→a) {} → {}. And the anchor was demonstrably working the whole time: the \
             UNRELATED pair (c→b) moved {} → {} carrying {unrelated_arrived:?} (echoes \
             {unrelated_echoed:?}, dialog {c_dialog}/{c_answered:?}, {} sent); the \
             0x0D02 frames it forwarded moved {} → {}; and A's re-announcement \
             (ok={}) reached C through the anchor's flood as version {a_version_before:?} → \
             {a_version_after:?}. Flat-because-nothing-happened is not the claim and is not \
             what this row asserts.",
            fenced.message.clone().or(fenced.error.clone()),
            stat_str(&a_direct_stream, "stream_id"),
            stat_str(&b_direct_stream, "stream_id"),
            flat_before.ab,
            flat_after.ab,
            flat_before.ba,
            flat_after.ba,
            flat_before.cb,
            flat_after.cb,
            stat_u64(&unrelated_sent, "sent"),
            flat_before.signal,
            flat_after.signal,
            reannounced.ok,
        ),
    );

    // ── part 3: the inverse leg ────────────────────────────────────
    //
    // The DataChannel closed from the PAGE, which is the only honest
    // way to take a direct path away without asking the leaf to
    // pretend: index 0 is the channel `connect()` built to the
    // anchor and is left alone, and that claim is self-checking —
    // the restoration below rides the anchor, so a close that took
    // the wrong channel fails this row.
    let closed = script
        .run(
            P2_TAB_A,
            json!({ "kind": "channels", "session": "p2", "close": true, "keep": 1 }),
        )
        .await;
    let down_nonces = nonces(run, "down", 1);
    let down_before = Forwarded::read(cx.anchor, &a, &b, &c);
    let down_sent = p2_send(&mut script, P2_TAB_A, "a2b", &down_nonces).await;
    let down_at_b = p2_inbox(&mut script, P2_TAB_B, "b2a", 1, 0, P2_DOWN_MS).await;
    let down_after = Forwarded::read(cx.anchor, &a, &b, &c);
    let down_arrived = stat_list(&down_at_b, "data");

    // Manual routed restoration, from the page and through the
    // production surface: a fresh `peer_offer` on a pair that
    // already has a session puts the peer's RELAY addressing back
    // (§9 step 4 read backwards) and the answerer's
    // `peer_accept_offer` does the same on its side. Candidates are
    // not serviced and no handshake is run, so the pair stays on the
    // anchor.
    let restore_offer = p2_offer(&mut script, P2_TAB_A, &b.node_hex).await;
    let restore_dialog = stat_str(&restore_offer, "dialog").unwrap_or_default();
    let restore_answer = if restore_offer.ok {
        p2_answer_matching(&mut script, P2_TAB_B, &a.node_hex, &restore_dialog).await
    } else {
        Err(format!(
            "the restoring peer_offer refused: {:?}",
            restore_offer
                .message
                .clone()
                .or(restore_offer.error.clone())
        ))
    };
    let a_restored = p2_open_stream(
        &mut script,
        P2_TAB_A,
        "a2b",
        &b.node_hex,
        P2_LABEL_AB_RESTORED,
        false,
    )
    .await;
    let b_restored = p2_open_stream(
        &mut script,
        P2_TAB_B,
        "b2a",
        &a.node_hex,
        P2_LABEL_AB_RESTORED,
        true,
    )
    .await;
    let restored_nonces = nonces(run, "restored", 2);
    let restore_before = Forwarded::read(cx.anchor, &a, &b, &c);
    let restore_sent = p2_send(&mut script, P2_TAB_A, "a2b", &restored_nonces).await;
    let restored_at_b = p2_inbox(&mut script, P2_TAB_B, "b2a", 2, 0, P2_INBOX_MS).await;
    let restored_echo = p2_inbox(&mut script, P2_TAB_A, "a2b", 0, 2, P2_INBOX_MS).await;
    let restore_after = Forwarded::read(cx.anchor, &a, &b, &c);
    let restored_arrived = stat_list(&restored_at_b, "data");
    let restored_echoed = stat_list(&restored_echo, "echoes");

    let inverse_ok = closed.ok
        && stat_u64(&closed, "closed") >= 1
        && down_arrived.is_empty()
        && down_after.ab == down_before.ab
        && restore_offer.ok
        && restore_answer.is_ok()
        && a_restored.ok
        && b_restored.ok
        && stat_u64(&restore_sent, "sent") == 2
        && restored_arrived == restored_nonces
        && restored_echoed == restored_nonces
        && restore_after.ab > restore_before.ab
        && restore_after.ba > restore_before.ba;
    ledger.record(
        P2_WITNESSES[2],
        inverse_ok,
        format!(
            "§10 part 3 — THE INVERSE LEG. {} DataChannel(s) closed from A's page, leaving \
             the anchor's alone: {:?} → {:?}. With the direct path down and the relay not \
             yet restored, A's send of {down_nonces:?} ({}) reached nobody — B's inbox \
             reports {down_arrived:?} after {} ms and the anchor forwarded nothing for the \
             pair either ((a→b) {} → {}) — so part 2's traffic really was riding that \
             channel and not some other path. Then the pair was restored to the anchor \
             MANUALLY through the production surface: peer_offer {restore_dialog} + \
             peer_accept_offer {restore_answer:?} put the relay addressing back on both \
             sides, with no candidate serviced and no handshake run. A sent \
             {restored_nonces:?}; B's inbox reports {restored_arrived:?}, A holds the echoes \
             {restored_echoed:?}, and THE SAME counter that stayed flat moved again: \
             (a→b) {} → {}, (b→a) {} → {}. Without this leg part 2 could not tell direct \
             from broken. {}",
            stat_u64(&closed, "closed"),
            closed.stats.as_ref().and_then(|s| s.get("before")),
            closed.stats.as_ref().and_then(|s| s.get("after")),
            down_sent
                .message
                .clone()
                .or(down_sent.error.clone())
                .unwrap_or_else(|| format!(
                    "accepted locally, sent={}",
                    stat_u64(&down_sent, "sent")
                )),
            stat_u64(&down_at_b, "waited_ms"),
            down_before.ab,
            down_after.ab,
            restore_before.ab,
            restore_after.ab,
            restore_before.ba,
            restore_after.ba,
            peer_view(cx.anchor, a.node_id, b.node_id),
        ),
    );

    p2_teardown(cx, &mut script).await;
}

/// Close the three slice 2 nodes, stop their pages' poll loops and
/// release their contexts.
async fn p2_teardown(cx: &Cx6<'_>, script: &mut Script6) {
    for tab in PEER_TABS {
        let _ = script
            .run(tab, json!({ "kind": "close", "session": "p2" }))
            .await;
    }
    for tab in PEER_TABS {
        let _ = script.run(tab, json!({ "kind": "done" })).await;
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    for page in [P2_PAGE_A, P2_PAGE_B, P2_PAGE_C] {
        let _ = cx.driver.close_page(page).await;
    }
    println!(
        "[stage6] {} — the three slice 2 contexts are closed",
        cx.engine.as_str()
    );
}

// ───────────────────── slice 3: the retry trigger ─────────────────────

/// The two slice 3 pages, in two more isolated contexts.
const P3_PAGE_A: &str = "retry6-a";
const P3_PAGE_B: &str = "retry6-b";
const P3_CTX_A: &str = "stage6-r-a";
const P3_CTX_B: &str = "stage6-r-b";
const P3_SECRET_A_ENTITY: &str = "c6c6c6c6c6c6c6c6c6c6c6c6c6c6c6c6c6c6c6c6c6c6c6c6c6c6c6c6c6c6c6c6";
const P3_SECRET_A_NOISE: &str = "c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7";
const P3_SECRET_B_ENTITY: &str = "d6d6d6d6d6d6d6d6d6d6d6d6d6d6d6d6d6d6d6d6d6d6d6d6d6d6d6d6d6d6d6d6";
const P3_SECRET_B_NOISE: &str = "d7d7d7d7d7d7d7d7d7d7d7d7d7d7d7d7d7d7d7d7d7d7d7d7d7d7d7d7d7d7d7d7";

/// How long the OFFERER's browsing context stays offline.
///
/// Long enough for the `offline` event to be observed and for the
/// page to be waiting on `online` when it arrives, and no longer:
/// this is the runner's own staging of a network change, not a
/// timeout anything is asserted against.
const OFFLINE_MS: u64 = 1_200;

const P3_WITNESSES: [&str; 3] = [WITNESSES[12], WITNESSES[13], WITNESSES[14]];

/// Slice 3: the network-change retry trigger, `rtcStats()`, and one
/// node-id spelling.
///
/// # What this row actually stages, and why in that order
///
/// A network change is not the same fact as a broken path, and only
/// one of them may act. So the row separates them:
///
/// 1. a direct pair is established through the page-facing loop;
/// 2. the trigger is armed — it is opt-in, so nothing happened
///    before this;
/// 3. the pair's **transport** is taken away (the page closes the
///    DataChannel it created), leaving the session installed. The
///    report is read here and `started` must be **0**: the path is
///    down and no network change has been observed, so there is
///    nothing the owner is entitled to do;
/// 4. then, and only then, the offerer's browsing context goes
///    offline and comes back. Exactly ONE re-attempt starts, and
///    `ice_attempted` moves by exactly one.
///
/// **`setOffline` is a network change, not a link break.** Chromium
/// emulates it on the URL loader, so an established ICE path over
/// loopback keeps working — which is why step 3 exists rather than
/// hoping the offline window kills the pair. What the toggle really
/// delivers is what the trigger is about: `navigator.onLine` flips
/// and `offline`/`online` fire, in the offerer's context and
/// nowhere else.
///
/// **The side is named.** `setOffline` is per context and the two
/// leaves live in isolated ones, so the row disconnects the
/// OFFERER — the side that owns the repair — and says so. Taking
/// the answerer offline would prove nothing about a re-offer nobody
/// makes.
#[expect(clippy::too_many_lines, reason = "one linear witness script")]
async fn retry_witness(cx: &Cx6<'_>, ledger: &mut Ledger) {
    let mut script = Script5::new(cx.tabs.clone(), 6_000_000);
    let url_a = format!("{}/leaf5.html?tab={TAB_A}", cx.page_origin);
    let url_b = format!("{}/leaf5.html?tab={TAB_B}", cx.page_origin);
    for (page, url, ctx) in [(P3_PAGE_A, &url_a, P3_CTX_A), (P3_PAGE_B, &url_b, P3_CTX_B)] {
        if let Err(why) = cx.driver.open_page_in(page, url, Some(ctx)).await {
            for name in P3_WITNESSES {
                ledger.record(name, false, format!("context {ctx} would not open: {why}"));
            }
            return;
        }
    }

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
        use_session: false,
        capabilities: vec![PEER_TAG.to_string()],
        subscriptions: Vec::new(),
        lock_scope: None,
        expect_failure: false,
    };
    let a = script
        .run(TAB_A, connect(P3_SECRET_A_ENTITY, P3_SECRET_A_NOISE))
        .await;
    let b = script
        .run(TAB_B, connect(P3_SECRET_B_ENTITY, P3_SECRET_B_NOISE))
        .await;
    let (Some(a), Some(b)) = (leaf_of(&a), leaf_of(&b)) else {
        let detail = format!(
            "a slice 3 leaf did not connect: a={:?} b={:?}",
            a.error.unwrap_or_default(),
            b.error.unwrap_or_default()
        );
        for name in P3_WITNESSES {
            ledger.record(name, false, detail.clone());
        }
        return;
    };

    for tab in [TAB_A, TAB_B] {
        let _ = script
            .run(
                tab,
                Step5::Announce {
                    id: 0,
                    session: "peer".to_string(),
                    capabilities: vec![PEER_TAG.to_string()],
                },
            )
            .await;
    }
    let found_b = discover(&mut script, TAB_A, &b.node_hex).await;
    let found_a = discover(&mut script, TAB_B, &a.node_hex).await;
    if found_a.is_none() || found_b.is_none() {
        let detail =
            "the slice 3 leaves did not discover each other inside the announcement deadline"
                .to_string();
        for name in P3_WITNESSES {
            ledger.record(name, false, detail.clone());
        }
        return;
    }

    // --- 1. a direct pair, and the trigger armed -------------------
    let armed = script
        .run(
            TAB_A,
            Step5::PeerArmRetry {
                id: 0,
                session: "peer".to_string(),
            },
        )
        .await;
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
    let established = outcome(&connected).as_deref() == Some("direct")
        && outcome(&accepted).as_deref() == Some("direct");

    // --- 2. rtcStats, on a node that has actually carried traffic --
    let stats = script
        .run(
            TAB_A,
            Step5::PeerRtcStats {
                id: 0,
                session: "peer".to_string(),
            },
        )
        .await;
    let emitted = stat_u64(&stats, "fields");
    let declared = stat_u64(&stats, "not_applicable_fields");
    let native_names = [
        "accepted",
        "written",
        "write_false",
        "retained",
        "discarded_at_close",
        "max_buffered",
        "admission_refused_slots",
        "admission_refused_bytes",
        "admission_refused_advisory",
        "admission_refused_unknown_peer",
        "ingress_delivered",
        "ice_attempted",
        "ice_direct",
        "ice_relayed",
        "ice_failed",
    ];
    let all_native_present = native_names
        .iter()
        .all(|name| counter_string(&stats, "counters", name).is_some());
    // The fields a leaf has no meaning for are ABSENT from the
    // reading and present in `not_applicable` with a reason. A zero
    // would read as "no STUN requests answered", which is an
    // observation this surface is not entitled to make.
    let stun_absent = counter_string(&stats, "counters", "stun_binding_requests").is_none();
    let stun_explained = counter_string(&stats, "not_applicable", "stun_binding_requests")
        .is_some_and(|reason| reason.len() > 20);
    let traffic_observed = counter_of(&stats, "ingress_delivered") > 0
        && counter_of(&stats, "accepted") > 0
        && counter_of(&stats, "written") > 0;
    ledger.record(
        P3_WITNESSES[1],
        stats.ok
            && emitted == 16
            && declared == 24
            && all_native_present
            && stun_absent
            && stun_explained
            && traffic_observed,
        format!(
            "node.rtcStats() on a leaf that has actually carried traffic: {emitted} measured \
             fields, {declared} declared inapplicable. Every one of the 15 native RtcStats \
             names with a leaf meaning is present ({all_native_present}); udp_blocked is the \
             16th and is the one term with NO native counterpart, because native says a node \
             signalling over UDP cannot have UDP blocked and the leaf is the side whose \
             evidence can establish it. accepted={} written={} ingress_delivered={} \
             ice_attempted={} ice_direct={} — measurements, not zeros. \
             stun_binding_requests is ABSENT from the reading ({stun_absent}) and carries a \
             reason instead ({stun_explained}): a field frozen at 0 reads as \"no STUN \
             requests answered\", which is an observation a leaf that serves no STUN is not \
             entitled to make. ice_pending is emitted by neither side — it is the residual \
             ice_attempted - (direct+relayed+failed+udp_blocked) and the reader derives it.",
            counter_of(&stats, "accepted"),
            counter_of(&stats, "written"),
            counter_of(&stats, "ingress_delivered"),
            counter_of(&stats, "ice_attempted"),
            counter_of(&stats, "ice_direct"),
        ),
    );

    // --- 3. take the TRANSPORT away, and observe that nothing acts -
    let cut = script
        .run(
            TAB_A,
            Step5::PeerChannels {
                id: 0,
                session: "peer".to_string(),
                close: true,
                // Index 0 is the anchor's channel: `connect()`
                // created it before any peer attempt existed, and
                // closing it would take away the signalling path the
                // repair needs.
                keep: 1,
            },
        )
        .await;
    let quiet = script
        .run(
            TAB_A,
            Step5::PeerRetryReport {
                id: 0,
                session: "peer".to_string(),
            },
        )
        .await;
    let cut_channels = stat_u64(&cut, "closed");
    let idle_before_the_change = stat_str(&quiet, "started").as_deref() == Some("0");

    // --- 4. the network change ------------------------------------
    //
    // Both steps go IN FLIGHT first: the offerer's page cannot be
    // handed a step while its context's HTTP is down, and the
    // answerer has to be waiting for the re-offer when it arrives.
    let change = script.spawn(
        TAB_A,
        Step5::PeerNetworkChange {
            id: 0,
            session: "peer".to_string(),
            peer_hex: b.node_hex.clone(),
            wait_ms: 25_000,
        },
    );
    let re_accept = script.spawn(
        TAB_B,
        Step5::PeerAccept {
            id: 0,
            session: "peer".to_string(),
            peer_hex: a.node_hex.clone(),
        },
    );
    // **Wait for the page to be LISTENING, do not guess.** The first
    // run of this row read `0 offline event(s)`: the toggle landed
    // 250 ms after the step was posted and the page had not yet
    // long-polled it off the queue, so the listeners went on after
    // the transition they exist to observe. The page publishes
    // `window.__netChangeArmed` the instant they are installed; this
    // polls for it. Not a widened wait — a handshake, and a failure
    // to arm is reported rather than slept through.
    let mut armed_in_page = false;
    for _ in 0..100 {
        if cx
            .driver
            .eval(P3_PAGE_A, "window.__netChangeArmed === true")
            .await
            .ok()
            .and_then(|v| v.as_bool())
            == Some(true)
        {
            armed_in_page = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let took_offline = cx.driver.set_offline(P3_PAGE_A, true).await;
    tokio::time::sleep(Duration::from_millis(OFFLINE_MS)).await;
    let brought_back = cx.driver.set_offline(P3_PAGE_A, false).await;
    let changed = change.await;
    let re_accepted = re_accept.await;

    let online_events = stat_u64(&changed, "online_events");
    let offline_events = stat_u64(&changed, "offline_events");
    let triggers = stat_str(&changed, "triggers")
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);
    let started = stat_str(&changed, "started")
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);
    let coalesced = stat_str(&changed, "coalesced")
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);
    let attempted_before = stat_str(&changed, "attempted_before")
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);
    let attempted_after = counter_of(&changed, "ice_attempted");
    let delta = attempted_after.saturating_sub(attempted_before);
    let restored = stat_bool(&changed, "direct");
    let settled = stat_str(&changed, "settled").unwrap_or_default();

    ledger.record(
        P3_WITNESSES[0],
        established
            && stat_bool(&armed, "armed")
            && cut_channels >= 1
            && idle_before_the_change
            // The page confirmed it is LISTENING before the toggle.
            && armed_in_page
            // The renderer's OWN reading, not the fact that the
            // request was accepted. `setOffline` alone is a no-op
            // for the renderer — it emulates at the URL loader and
            // never notifies the page — so the driver also sends
            // CDP `Network.emulateNetworkConditions`, and this is
            // the assertion that says the page actually believed it.
            && took_offline.as_ref().ok() == Some(&Some(false))
            && brought_back.as_ref().ok() == Some(&Some(true))
            && offline_events >= 1
            && online_events >= 2
            && triggers >= 2
            && started == 1
            && coalesced >= 1
            && delta == 1
            && settled == "direct"
            && restored,
        format!(
            "the OFFERER's browsing context ({P3_CTX_A}, page {P3_PAGE_A}) went offline for \
             {OFFLINE_MS} ms and came back; the answerer stayed online throughout, because a \
             re-offer nobody answers proves nothing. setOffline is per CONTEXT and these two \
             leaves are in isolated ones, so the side is named rather than implied.\n\
             What that toggle IS: Chromium emulates it on the URL LOADER, so it flips \
             navigator.onLine, fires offline/online, and fails the context's HTTP — and an \
             already-established ICE path (here, over loopback) SURVIVES it. It is a network \
             change, not a link break. That is why the interruption below is staged \
             separately and must not be \"simplified\" away on the assumption that going \
             offline killed the transport: it does not, and a row that assumed it would \
             assert nothing.\n\
             Staged in four steps, and the order is the claim: the pair went direct \
             ({established}), the trigger was ARMED ({} — opt-in, so nothing happened \
             before), the pair's TRANSPORT was then taken away ({cut_channels} live \
             DataChannel(s) closed above the anchor's index 0; before={:?} after={:?}), and \
             the report at that moment read started=0 ({idle_before_the_change}). \"The path \
             is down\" is NOT \"the network changed\", and only the second is allowed to \
             act.\n\
             Then the change, with the page CONFIRMED listening first ({armed_in_page} — the \
             first run of this row toggled 250 ms after posting the step, before the page \
             had long-polled it, and installed its listeners after the transition): the \
             renderer's own navigator.onLine read {took_offline:?} then {brought_back:?}, \
             which is the assertion — setOffline ALONE is a no-op for the renderer, so the \
             driver also sends CDP Network.emulateNetworkConditions. \
             {offline_events} offline event(s), {online_events} online \
             event(s) — the second is dispatched by the page for the SAME change, because a \
             browser is under no obligation to fire each observation once and an interface \
             that flaps fires `online` as often as it flaps. {triggers} triggers reached the \
             owner (online={:?}, iceFailed={:?}) and it started {started}. coalesced={coalesced} \
             is the difference it absorbed — reported rather than hidden, because \"the \
             trigger never fired\" and \"it fired and was absorbed\" are different facts and \
             only one is a defect.\n\
             ice_attempted moved {attempted_before} → {attempted_after}, a delta of \
             {delta}. ONE. An attempt is one signalling DIALOG and the anchor bootstrap \
             counts, so the absolute number is not the assertion — the delta across the \
             cycle is. The re-attempt settled as {settled:?} and the pair reads \
             direct={restored} again; the answerer's side reported {:?}. \
             The repair ran through the production steps — offer_peer (peer_offer's own \
             body, so it superseded the stale dialog and counted it), service_peer, \
             run_handshake — under the episode's single absolute deadline, which is also \
             the new dialog's.",
            stat_bool(&armed, "armed"),
            stat_str(&cut, "before"),
            stat_str(&cut, "after"),
            stat_str(&changed, "online"),
            stat_str(&changed, "ice_failed"),
            outcome(&re_accepted),
        ),
    );

    // --- 5. one node-id spelling, both signalling surfaces --------
    let mut spellings: Vec<(String, bool, String)> = Vec::new();
    for candidate in [
        a.node_hex.clone(),
        format!("0x{}", b.node_hex),
        b.node_id.to_string(),
        "0x9".to_string(),
        "nine".to_string(),
    ] {
        let r = script
            .run(
                TAB_A,
                Step5::PeerSignalSpelling {
                    id: 0,
                    session: "peer".to_string(),
                    peer_hex: candidate.clone(),
                },
            )
            .await;
        spellings.push((
            candidate,
            stat_bool(&r, "parsed"),
            stat_str(&r, "detail").unwrap_or_default(),
        ));
    }
    let canonical_accepted = spellings.first().is_some_and(|s| s.1);
    let prefixed_accepted = spellings.get(1).is_some_and(|s| s.1);
    let others_refused = spellings[2..].iter().all(|s| !s.1);
    ledger.record(
        P3_WITNESSES[2],
        canonical_accepted && prefixed_accepted && others_refused,
        format!(
            "`node.signal(peer, …)` with five spellings of a node id: {spellings:?}. The 16 \
             hex digits `node_id_hex()` emits parse, `0x` + 16 digits parse to the same id, \
             and a DECIMAL id, a short hex id and a non-hex string are refused by name. \
             `parsed` is read off the refusal TEXT: \"is not a peer id\" is the parser, and \
             anything else means the id parsed and the call reached the v1 carrier, whose \
             own refusal (R14) is a different fact.\n\
             This closes a PRE-EXISTING defect, not a tightening. `LeafNode::signal` read \
             `parse_u64` — decimal first — so it refused the output of `node_id_hex()`, the \
             leaf's own spelling; the leader-proxied `MeshSession::signal` read bare hex of \
             ANY length, so `\"9\"` and `\"deadbeef\"` named nodes there that nothing else \
             would accept. Two page-facing surfaces, two answers to \"what is a node id\". \
             And underneath the drift, a dead path: the leader re-encodes canonical 16-hex \
             on the way to the surface that wanted decimal, so the proxied `signal` could \
             not work at all. It survived because nothing exercised it. Both surfaces now \
             parse through `parse_peer_id`, the reader the four §9 methods already used \
             (found by PeerLoop's audit, 2026-09-16).",
        ),
    );

    for tab in [TAB_A, TAB_B] {
        let _ = script
            .run(
                tab,
                Step5::Close {
                    id: 0,
                    session: "peer".to_string(),
                },
            )
            .await;
        let _ = script.run(tab, Step5::Done { id: 0 }).await;
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    for page in [P3_PAGE_A, P3_PAGE_B] {
        let _ = cx.driver.close_page(page).await;
    }
    println!(
        "[stage6] {} — the two slice 3 contexts are closed",
        cx.engine.as_str()
    );
}

/// One value out of a named sub-object of a step's `stats`.
///
/// `rtcStats` reports two maps under one `stats` — the measurements
/// and the fields it refuses to measure — and a witness has to be
/// able to assert that a name is in exactly one of them.
fn counter_string(result: &StepResult, group: &str, name: &str) -> Option<String> {
    result
        .stats
        .as_ref()
        .and_then(|s| s.get(group))
        .and_then(|g| g.get(name))
        .and_then(|v| v.as_str())
        .map(str::to_string)
}
