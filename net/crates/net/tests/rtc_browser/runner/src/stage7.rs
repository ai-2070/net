//! Stage 7 — the networked game store, over a real browser transport.
//!
//! Every store witness that exists today runs in Node against a
//! structural transport double: real codec, real chunker, real
//! assembler, real ledger, and delivery that is a function call. That
//! establishes the store's own rules and nothing about composition,
//! which is exactly what the plan's B2′ gate says — "the same chunker
//! composed with the real stream transport … does **not** establish
//! that chunks ride the shared reliability/reassembly code intact
//! under loss, duplication and reorder."
//!
//! So these five witnesses put the store on the transport the rest of
//! this harness exercises, in two isolated browsing contexts of a real
//! engine. **CI does not run them yet** — they are behind `--stage7`,
//! which neither engine's job passes, and they are in no floor. The
//! earlier version of this header said CI ran them on both engines,
//! which was simply untrue: the harness's own default run excludes
//! them by name.
//!
//! Local status at the time of writing (Chromium, `--stage7`): 1, 2,
//! 4 and 5 PASS; 3 does not, and is left failing rather than
//! softened. The full run is 52 witnesses, 1 failed — this stage no
//! longer disturbs any other, which it did until its two identities
//! were found to be slice 3's (see `SECRET_HOST_ENTITY`).
//!
//! What witness 3 has established, which is more than "it fails":
//! ONE dropped datagram on the host's channel stalls the join past
//! 30 s. Not a pattern of loss — one packet, out of the seven or so
//! the join costs. A store-level `resync` cannot repair that,
//! because a reliable stream with a wire gap holds everything behind
//! it, so recovery has to come from the transport.
//!
//! Four candidate explanations, and where each stands:
//!
//! 1. *The harness's offer budget refuses the attempt.* REFUTED —
//!    raised 200 → 2000 per IP per minute, no change. (The offer
//!    budget stays raised; it is not the cause, and 200 was the
//!    default for a run with far fewer contexts.)
//! 2. *Nothing drives the replica's assembly deadline, so no resync
//!    is ever sent.* TRUE, and fixed (`join.ts` now drives
//!    `replica.tick()`, with its own red/green witness in
//!    `test/store/hosted.test.ts`) — but NOT the cause here: the
//!    browser behaves identically with the clock driven, which is
//!    what point 3 explains.
//! 3. *Retransmission needs traffic.* OPEN. `LeafNode::tick` →
//!    `drive_reliability` is what emits NACKs and due retransmits,
//!    and it runs only from `pump()` — an API call or an inbound
//!    packet. There is no periodic tick in the WASM leaf. An
//!    attempt to test this by keeping the host committing failed to
//!    test anything: with no installed replica there is no audience,
//!    so the commits sent nothing. Needs a driver that does not
//!    depend on the join having succeeded.
//! 4. *The anchor does not forward reliability control packets.*
//!    REFUTED from source — the relay path is header-only routing
//!    and type-agnostic (`mesh.rs`, the `dest_id != local` arm);
//!    only signalling is classified, and only to choose a counter.
//!
//! Both leaves' raw counter ledgers are printed on failure
//! (`Step5::NodeCounters`) because which side is silent about the
//! loss is the diagnosis; at the time of writing neither reports a
//! `stream_failed`, so nothing has given up — it is waiting.
//!
//! 1. a multi-chunk snapshot installs, receiver-observed;
//! 2. it still installs through injected loss **and** reorder;
//! 3. an action crosses, executes once, and its result comes back;
//! 4. a duplicated wire message moves the view exactly once;
//! 5. and store traffic moves the anchor's per-pair forwarding
//!    counter while the pair is relayed.
//!
//! **The sixth property is NOT here**, and its absence is deliberate
//! rather than forgotten: "the same traffic leaves that counter flat
//! once the pair is direct" needs a direct session, a direct session
//! needs `peer_offer`, and that needs the peer's signed announcement.
//! An announcement is FLOODED to the nodes connected when it is made
//! and is never replayed to one that arrives later, so a stage that
//! opens its contexts after Stage 5 and Stage 6 cannot discover
//! anything — measured, not assumed: querying Stage 6's own tag from
//! these tabs returns an empty array too. Stage 6 witnesses 10 and 11
//! already hold the flat-when-direct property on the raw stream
//! surface; holding it on STORE traffic needs this stage to own the
//! pair from the start, which is a change to the harness's topology
//! and not a change to the store.
//!
//! The last two are the plan's "flat for direct, increasing for
//! routed" criterion, measured on STORE traffic rather than on the raw
//! stream surface Stage 6 uses — a store frame is what a game actually
//! sends, and the store's own chunking is what makes it several
//! frames.
//!
//! **What each witness reads.** Never "the step returned ok": the
//! replica's own installed document, digested, compared against the
//! host's. A digest mismatch is a wrong document; an absent digest is
//! a step that did not run. And where a hook is armed, the hook's own
//! count comes back and is asserted non-zero — a loss witness that
//! dropped nothing is a witness about nothing.

use net::adapter::net::MeshNode;

use crate::stage5::{Script5, Step5};
use crate::stage6::{discover, leaf_of, routing_id, Leaf};
use crate::{Ledger, StepResult};

/// Entries in the hosted document.
///
/// 700 keys of `entry-<n>` is comfortably past the store's 5934-byte
/// chunk budget, so the snapshot is several chunks and the assembler
/// has real work. A single-chunk snapshot would make witness 1 a test
/// of one `send`.
const ENTRIES: u32 = 700;

/// The transport's per-event ceiling, which the store derives its
/// chunk budget from (`MAX_EVENT_SIZE`).
const MAX_EVENT_BYTES: u32 = 8104;

pub const WITNESSES: [&str; 5] = [
    "stage7_store_snapshot_installs_over_the_real_stream",
    "stage7_store_snapshot_installs_through_injected_loss_and_reorder",
    "stage7_an_action_round_trip_crosses_the_real_transport",
    "stage7_a_duplicated_store_frame_moves_the_view_once",
    "stage7_store_traffic_moves_the_anchors_per_pair_counter_when_routed",
];

/// The tabs this stage drives, on their own isolated contexts.
pub const TABS: [&str; 2] = [TAB_HOST, TAB_PLAYER];

const TAB_HOST: &str = "s7host";
const TAB_PLAYER: &str = "s7player";
const PAGE_HOST: &str = "stage7-host";
const PAGE_PLAYER: &str = "stage7-player";
const CTX_HOST: &str = "stage7-ctx-host";
const CTX_PLAYER: &str = "stage7-ctx-player";

/// One capability tag, so each leaf can discover the other's signed
/// announcement — which is what installs the relayed session the
/// store's `openStream({peer})` needs.
const STORE_TAG: &str = "stage7.store";

// Identities unique to this stage, and the reason that matters more
// than it looks: an entity secret IS the node id. These constants
// were copied from Stage 6 slice 3 (`c6…`/`c7…` and `d6…`/`d7…`), so
// Stage 7's two leaves and slice 3's two leaves were THE SAME TWO
// NODES. Whichever stage ran second announced under an identity the
// anchor already held, and its capability query came back with zero
// peers — the "the leaves did not discover each other" failure that
// looked like a capacity problem for three rounds. Every byte here
// has to stay distinct from `SECRET_*` in `stage6.rs`.
const SECRET_HOST_ENTITY: &str =
    "e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6";
const SECRET_HOST_NOISE: &str =
    "e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7";
const SECRET_PLAYER_ENTITY: &str =
    "f6f6f6f6f6f6f6f6f6f6f6f6f6f6f6f6f6f6f6f6f6f6f6f6f6f6f6f6f6f6f6f6";
const SECRET_PLAYER_NOISE: &str =
    "f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7";

/// What this stage needs from the runner.
pub struct Cx7<'a> {
    pub driver: &'a crate::browser::Driver,
    pub anchor: &'a MeshNode,
    pub credential: String,
    pub bootstrap_url: String,
    pub origin: String,
    pub page_origin: String,
    pub stun: Option<String>,
    pub anchor_rtc_addr: String,
    pub tabs: std::collections::HashMap<String, crate::stage5::Step5Sender>,
}

/// One reading of the anchor's per-pair application forwarding.
///
/// `hp`/`ph` are the pair under test; `signal` is the positive
/// control. A flat pair counter means nothing on a dead anchor, and
/// `signal_forwarded` moving is what says the anchor is still relaying
/// for this pair while their application data has stopped going
/// through it.
#[derive(Debug, Clone, Copy)]
struct Forwarded {
    hp: u64,
    ph: u64,
    signal: u64,
}

impl Forwarded {
    fn read(anchor: &MeshNode, host: &Leaf, player: &Leaf) -> Self {
        Self {
            hp: anchor.forwarded_app_packets(routing_id(host), player.node_id),
            ph: anchor.forwarded_app_packets(routing_id(player), host.node_id),
            signal: anchor.rtc_stats().signal_forwarded(),
        }
    }

    /// Application data the anchor carried for this pair, either way.
    fn pair(self) -> u64 {
        self.hp + self.ph
    }
}

/// A store statistic the page reported.
fn stat_u64(result: &StepResult, key: &str) -> Option<u64> {
    result
        .stats
        .as_ref()?
        .get(key)
        .and_then(serde_json::Value::as_u64)
}

fn stat_str(result: &StepResult, key: &str) -> Option<String> {
    Some(
        result
            .stats
            .as_ref()?
            .get(key)?
            .as_str()?
            .to_string(),
    )
}

fn why(result: &StepResult) -> String {
    result.error.clone().unwrap_or_else(|| "no error".to_string())
}

/// Run every Stage 7 witness.
///
/// Two isolated contexts of its own, connected to the same anchor, so
/// the store's frames ride the relayed pair the anchor serves. The
/// pair is NOT taken direct here — see the module header for why the
/// flat-when-direct property is Stage 6's and not this stage's.
#[expect(clippy::too_many_lines, reason = "one linear witness script")]
pub async fn run(cx: Cx7<'_>, ledger: &mut Ledger) -> Result<(), String> {
    let mut script = Script5::new(cx.tabs.clone(), 3_000_000);
    let session = "store".to_string();
    let tab_host = TAB_HOST;
    let tab_player = TAB_PLAYER;
    let anchor = cx.anchor;

    let url_host = format!("{}/leaf5.html?tab={TAB_HOST}", cx.page_origin);
    let url_player = format!("{}/leaf5.html?tab={TAB_PLAYER}", cx.page_origin);
    for (page, url, ctx) in [
        (PAGE_HOST, &url_host, CTX_HOST),
        (PAGE_PLAYER, &url_player, CTX_PLAYER),
    ] {
        if let Err(reason) = cx.driver.open_page_in(page, url, Some(ctx)).await {
            let detail = format!("a Stage 7 browsing context would not open: {reason}");
            for name in WITNESSES {
                ledger.record(name, false, detail.clone());
            }
            return Ok(());
        }
    }

    let connect = |entity: &str, noise: &str| Step5::Connect {
        id: 0,
        session: session.clone(),
        credential: cx.credential.clone(),
        bootstrap_url: cx.bootstrap_url.clone(),
        origin: cx.origin.clone(),
        anchor_rtc_addr: cx.anchor_rtc_addr.clone(),
        stun: cx.stun.clone(),
        entity_secret_hex: Some(entity.to_string()),
        noise_secret_hex: Some(noise.to_string()),
        use_session: false,
        capabilities: vec![STORE_TAG.to_string()],
        subscriptions: Vec::new(),
        lock_scope: None,
        expect_failure: false,
    };

    let host_connected = script
        .run(tab_host, connect(SECRET_HOST_ENTITY, SECRET_HOST_NOISE))
        .await;
    let player_connected = script
        .run(
            tab_player,
            connect(SECRET_PLAYER_ENTITY, SECRET_PLAYER_NOISE),
        )
        .await;
    let (host, player) = match (leaf_of(&host_connected), leaf_of(&player_connected)) {
        (Some(h), Some(p)) if h.node_id != p.node_id => (h, p),
        (Some(h), Some(_)) => {
            let detail = format!(
                "both Stage 7 contexts connected as the SAME node {} — nothing below \
                 would be two peers",
                h.node_hex
            );
            for name in WITNESSES {
                ledger.record(name, false, detail.clone());
            }
            return Ok(());
        }
        _ => {
            let detail = format!(
                "a leaf did not connect: host={} player={}",
                why(&host_connected),
                why(&player_connected)
            );
            for name in WITNESSES {
                ledger.record(name, false, detail.clone());
            }
            return Ok(());
        }
    };

    // Announce and discover, because the store CANNOT address a peer
    // without them — and that is the correction to the previous
    // version of this comment, which asserted the opposite. What the
    // repaired `joinStore` reports is the reason:
    //
    //   the transport could not carry a store frame:
    //   session: no session with 0x<host>
    //
    // `openStream({peer})` needs a session with that peer, and a
    // relayed one is installed by the discovery path
    // (`ensure_relayed_session`). So discovery is a precondition of
    // the store, not an incidental step — and the earlier "nothing
    // here needs either" was an assumption that the transport double
    // could not contradict.
    for tab in [tab_host, tab_player] {
        let announced = script
            .run(
                tab,
                Step5::Announce {
                    id: 0,
                    session: session.clone(),
                    capabilities: vec![STORE_TAG.to_string()],
                },
            )
            .await;
        if !announced.ok {
            println!("[stage7] {tab} could not announce: {}", why(&announced));
        }
    }
    let found_player = discover(&mut script, tab_host, &player.node_hex, &session, STORE_TAG).await;
    let found_host = discover(&mut script, tab_player, &host.node_hex, &session, STORE_TAG).await;
    if found_player.is_none() || found_host.is_none() {
        let detail = format!(
            "the leaves did not discover each other, so neither has a session with the \
             other and `openStream({{peer}})` cannot be issued: host's view of the \
             player = {found_player:?}, player's view of the host = {found_host:?}"
        );
        for name in WITNESSES {
            ledger.record(name, false, detail.clone());
        }
        return Ok(());
    }

    let host_store = |handle: &str| Step5::StoreHost {
        id: 0,
        session: session.clone(),
        handle: handle.to_string(),
        label: format!("store/stage7/{handle}"),
        entries: ENTRIES,
        max_event_bytes: MAX_EVENT_BYTES,
    };
    let join_store = |handle: &str, drop_every: u32, reorder_every: u32, duplicate_every: u32| {
        Step5::StoreJoin {
            id: 0,
            session: session.clone(),
            handle: handle.to_string(),
            label: format!("store/stage7/{handle}"),
            host_hex: host.node_hex.clone(),
            audience: vec!["crew".to_string()],
            key: "harness".to_string(),
            max_event_bytes: MAX_EVENT_BYTES,
            drop_every,
            reorder_every,
            duplicate_every,
            timeout_ms: 30_000,
        }
    };

    // --- 1. a multi-chunk snapshot installs ------------------------
    let hosted = script.run(tab_host, host_store("clean")).await;
    let joined = script.run(tab_player, join_store("clean", 0, 0, 0)).await;
    let host_digest = stat_str(&hosted, "digest");
    let replica_digest = stat_str(&joined, "digest");
    let chunks = stat_u64(&joined, "wire_messages").unwrap_or(0);
    let installed = joined.ok
        && host_digest.is_some()
        && host_digest == replica_digest
        && stat_u64(&joined, "entries") == Some(u64::from(ENTRIES));
    ledger.record(
        WITNESSES[0],
        installed,
        format!(
            "A STORE SNAPSHOT CROSSES A REAL DATACHANNEL AND INSTALLS. The host holds \
             {ENTRIES} entries; the joining replica's OWN installed document is what is \
             read here, digested and compared with the host's — not the step's ok flag, \
             and not a partial view, because `ready()` resolves only once the manifest, \
             every chunk and the definition's validation have landed. host={host_digest:?} \
             replica={replica_digest:?} entries={:?} wire messages this join cost={chunks} \
             (a single-chunk snapshot would make this a test of one `send`; the store \
             chunks at 5934 payload bytes). host node={} player node={}. {}",
            stat_u64(&joined, "entries"),
            host.node_hex,
            player.node_hex,
            why(&joined)
        ),
    );

    // --- 2. an action round trip -----------------------------------
    //
    // The correlated half of the protocol: a request that must be
    // answered, executed by the host's handler inside one
    // transaction, with the result coming back over the same
    // transport. §1.10's ledger is what makes it exactly once; the
    // transport is what makes it arrive at all.
    let acted = script
        .run(
            tab_player,
            Step5::StoreAct {
                id: 0,
                handle: "clean".to_string(),
                by: 5,
                settle_ms: 400,
                timeout_ms: 20_000,
            },
        )
        .await;
    let acted_again = script
        .run(
            tab_player,
            Step5::StoreState {
                id: 0,
                handle: "clean".to_string(),
            },
        )
        .await;
    let action_tick = acted
        .stats
        .as_ref()
        .and_then(|stats| stats.get("result"))
        .and_then(|result| result.get("tick"))
        .and_then(serde_json::Value::as_i64);
    let view_tick = stat_u64(&acted_again, "tick");
    ledger.record(
        WITNESSES[2],
        acted.ok && acted_again.ok && action_tick == Some(5) && view_tick == Some(5),
        format!(
            "AN ACTION CROSSES THE REAL TRANSPORT AND COMES BACK. The replica called              `bump {{by: 5}}`: the host's policy admitted it, its handler ran inside one              synchronous transaction, and the RESULT rode back correlated to the              request's own `q` — result tick={action_tick:?}. Then the delta the same              commit produced moved the replica's view to tick={view_tick:?}, which is              the second half of the contract: the `res` answers the request and the              delta moves the view, and a caller needs both. {} {}",
            why(&acted),
            why(&acted_again)
        ),
    );

    // --- 3. the same install, through loss and reorder -------------
    //
    // Both hooks armed on the JOINING side, so the caller's own
    // requests and the acknowledgements the reliable stream needs are
    // what get dropped and swapped. The store has no retransmission
    // of its own: if this passes, the transport's reliability is what
    // carried it.
    let hosted_lossy = script.run(tab_host, host_store("lossy")).await;
    // Armed on the HOST, because the snapshot is the host's outbound
    // traffic. Arming the joining page faulted its requests and
    // acknowledgements instead, so a non-zero drop count there said
    // nothing about a chunk — the reviewer's third finding, and the
    // reason this is its own step.
    let _ = script
        .run(
            tab_host,
            Step5::StoreFaults {
                id: 0,
                // Gentler than the first attempt: these hooks fall on
                // EVERY store this page is serving, not only the one
                // being joined, and one in three took the whole page
                // past what the 30 s join deadline could recover.
                drop_every: 7,
                reorder_every: 5,
                duplicate_every: 0,
            },
        )
        .await;
    let joined_lossy = script.run(tab_player, join_store("lossy", 0, 0, 0)).await;
    let faults = script.run(tab_host, Step5::StoreFaultsReport { id: 0 }).await;
    if !joined_lossy.ok {
        // The failing leg's own evidence, both sides. Printed only
        // when it fails, because on a pass it is noise, and printed
        // at all because the next person to pick this up should not
        // have to re-instrument it.
        let host_counters = script
            .run(
                tab_host,
                Step5::NodeCounters {
                    id: 0,
                    session: session.clone(),
                },
            )
            .await;
        let player_counters = script
            .run(
                tab_player,
                Step5::NodeCounters {
                    id: 0,
                    session: session.clone(),
                },
            )
            .await;
        println!(
            "[stage7] lossy join ok=false err={:?} host faults={:?}",
            joined_lossy.error, faults.stats
        );
        println!("[stage7] lossy host counters {:?}", host_counters.stats);
        println!("[stage7] lossy player counters {:?}", player_counters.stats);
    }
    let dropped = stat_u64(&faults, "dropped").unwrap_or(0);
    let swapped = stat_u64(&faults, "swapped").unwrap_or(0);
    let lossy_digest = stat_str(&joined_lossy, "digest");
    let survived = joined_lossy.ok
        && lossy_digest.is_some()
        && lossy_digest == stat_str(&hosted_lossy, "digest")
        && dropped > 0
        && swapped > 0;
    ledger.record(
        WITNESSES[1],
        survived,
        format!(
            "THE SNAPSHOT SURVIVES LOSS AND REORDER — the B2′ composition, and the \
             reason the gate exists: the store's chunker has been exercised against a \
             transport double that cannot lose anything. Here every 3rd outbound \
             DataChannel message was DROPPED and every 4th held back so the next \
             overtook it, for the whole join. The hooks' own counts come back and are \
             asserted NON-ZERO — a loss witness that dropped nothing is a witness about \
             nothing: dropped={dropped} reordered={swapped}. The document still installed \
             byte-identical: host={:?} replica={lossy_digest:?}. The store retransmits \
             nothing itself, so what recovered this is `wire/src/reliability.rs` plus \
             `leaf/src/stream.rs`'s reorder buffer. {}",
            stat_str(&hosted_lossy, "digest"),
            why(&joined_lossy)
        ),
    );

    // --- 4. a duplicated wire message ------------------------------
    //
    // Neither hook above can produce a duplicate. This one sends the
    // Nth message twice, which is what a retransmit looks like to the
    // receiver — and the view must move ONCE.
    let _hosted_dup = script.run(tab_host, host_store("dup")).await;
    let joined_dup = script.run(tab_player, join_store("dup", 0, 0, 0)).await;
    let before_tick = stat_u64(&joined_dup, "tick").unwrap_or(0);
    // The joined replica's publication count BEFORE the commit, so
    // the witness can say the view moved ONCE rather than that it
    // ended at 41. An absolute assignment converges to 41 whether it
    // is applied once, twice, or recovered to — which is precisely
    // what the reviewer's second finding says the old oracle could
    // not tell apart.
    let before_state = script
        .run(
            tab_player,
            Step5::StoreState {
                id: 0,
                handle: "dup".to_string(),
            },
        )
        .await;
    let applications_before = stat_u64(&before_state, "applications").unwrap_or(0);
    let committed = script
        .run(
            tab_host,
            Step5::StoreCommit {
                id: 0,
                handle: "dup".to_string(),
                entries: None,
                tick: Some(41),
                duplicate_every: 1,
                settle_ms: 400,
            },
        )
        .await;
    let after = script
        .run(
            tab_player,
            Step5::StoreState {
                id: 0,
                handle: "dup".to_string(),
            },
        )
        .await;
    let duplicated = stat_u64(&committed, "duplicated").unwrap_or(0);
    let replica_tick = stat_u64(&after, "tick");
    let applications_after = stat_u64(&after, "applications").unwrap_or(0);
    let moves = applications_after.saturating_sub(applications_before);
    let once = joined_dup.ok
        && committed.ok
        && after.ok
        && duplicated > 0
        && replica_tick == Some(41)
        && before_tick != 41
        && moves == 1;
    ledger.record(
        WITNESSES[3],
        once,
        format!(
            "A DUPLICATED FRAME MOVES THE VIEW ONCE — counted, not inferred. Every \
             outbound message of the host's commit was sent TWICE (duplicated={duplicated}, \
             asserted non-zero so an inert hook cannot pass this). The oracle is the \
             number of PUBLICATIONS the replica made: {applications_before} → \
             {applications_after}, so exactly {moves}. The final value cannot carry this \
             property — the commit ASSIGNS tick 41, so applying the delta twice, or \
             recovering to the same document, both end at 41 as well, and the earlier \
             version of this witness could not tell any of the three apart. Value read \
             back: {replica_tick:?} from {before_tick}. What makes one publication the \
             right answer is two things composed: the leaf delivers a retransmitted \
             duplicate once (`leaf/src/stream.rs`), and the assembler treats a \
             byte-identical chunk as idempotent. {} {}",
            why(&committed),
            why(&after)
        ),
    );

    // --- 4/5. routed, then direct ----------------------------------
    //
    // The pair is relayed by the anchor until something installs a
    // direct path, so store traffic now is routed by construction.
    let routed_before = Forwarded::read(anchor, &host, &player);
    let routed_commit = script
        .run(
            tab_host,
            Step5::StoreCommit {
                id: 0,
                handle: "dup".to_string(),
                entries: None,
                tick: Some(77),
                duplicate_every: 0,
                settle_ms: 500,
            },
        )
        .await;
    let routed_seen = script
        .run(
            tab_player,
            Step5::StoreState {
                id: 0,
                handle: "dup".to_string(),
            },
        )
        .await;
    let routed_after = Forwarded::read(anchor, &host, &player);
    let routed_delivered = stat_u64(&routed_seen, "tick") == Some(77);
    let routed_moved = routed_after.pair() > routed_before.pair();
    ledger.record(
        WITNESSES[4],
        routed_commit.ok && routed_delivered && routed_moved,
        format!(
            "STORE TRAFFIC IS RELAYED WHILE THE PAIR IS ROUTED, and the anchor's \
             PER-PAIR counter says so: (host→player, player→host) went from \
             ({}, {}) to ({}, {}). Delivery is RECEIVER-OBSERVED — the replica's own \
             document reads tick 77 — because a counter alone cannot tell delivery from \
             forwarding, and the plan is explicit that a flat counter is not success. \
             The key is `forwarded_app_packets(src_id, dest_id)` on the anchor's own \
             `MeshNode`, incremented only in the transit arm and only for non-signalling \
             subprotocols. {}",
            routed_before.hp,
            routed_before.ph,
            routed_after.hp,
            routed_after.ph,
            why(&routed_commit)
        ),
    );


    for (tab, handle) in [(tab_host, "clean"), (tab_host, "lossy"), (tab_host, "dup")] {
        let _ = script
            .run(
                tab,
                Step5::StoreClose {
                    id: 0,
                    handle: handle.to_string(),
                },
            )
            .await;
    }
    let _ = script
        .run(
            tab_player,
            Step5::Close {
                id: 0,
                session: session.clone(),
            },
        )
        .await;
    let _ = script
        .run(
            tab_host,
            Step5::Close {
                id: 0,
                session: session.clone(),
            },
        )
        .await;
    let _ = cx.driver.close_page(PAGE_HOST).await;
    let _ = cx.driver.close_page(PAGE_PLAYER).await;
    Ok(())
}
