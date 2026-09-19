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
//! So these ten witnesses put the store on the transport the rest of
//! this harness exercises, in isolated browsing contexts of a real
//! engine — eight on one measured pair plus a third participant, and
//! two more on a pair of their own (`refusals`). **CI gates them on
//! CHROMIUM**: that leg passes `--stage7`, its floor is 57 and all
//! ten names are pinned REQUIRED (`ci.yml`).
//!
//! **Firefox RUNS them, recorded, and does not gate them.** They had
//! never executed on that engine at all — this workstation cannot
//! seed a Firefox profile's NSS database (the `certutil` on PATH is
//! Microsoft's, and the only other prompt-free mechanism, an
//! enterprise policy, is not profile-scoped and would defeat the
//! harness's own TLS control) — so a second, `continue-on-error`
//! Firefox step exists to produce that evidence, with its verdicts
//! printed by a step of their own and consulted by no floor.
//!
//! The promotion is one edit each and is deliberately left to a
//! human reading a real run: `--stage7` on the gating Firefox step
//! and its floor from 47 to 55. Gating an unproven engine before one
//! observed run is the mistake this stage already made once, in the
//! other direction.
//!
//! They are still off by DEFAULT, which is what a local run gets
//! without the flag.
//!
//! Local status (Chromium, `--stage7`): **57 witnesses, 0 failed** —
//! all ten below pass, and this stage disturbs no other, which it
//! did until its two identities were found to be Stage 6 slice 3's
//! (see `SECRET_HOST_ENTITY`).
//!
//! This count has been wrong twice, both times because the list
//! below grew and the sentence above it did not. The review caught
//! both. If you add a witness, the number here, the heading below,
//! the floor in `ci.yml` and the pinned names move together.
//!
//! ## How the loss witness came to pass
//!
//! `stage7_store_snapshot_installs_through_injected_loss_and_reorder`
//! failed for as long as the store had two holes, and the failure
//! looked like a transport problem the whole time. The hook that
//! records WHICH datagram it dropped is what ended the guessing: it
//! was 130–200 bytes on channel `net` — the MANIFEST, an order of
//! magnitude too small to be one of the 5934-byte chunks.
//!
//! Two store defects, both found by review probes, both now repaired
//! with their own red/green witnesses in
//! `browser-ts/test/store/hosted.test.ts`:
//!
//! 1. **A lost manifest opened no assembly, so nothing expired.**
//!    `replica.tick()` covered a stalled assembly and could not cover
//!    a transition that never produced one: `ready()` never settled,
//!    nothing was re-sent, and the caller's only signal was silence.
//!    The join now has its own deadline — a bounded re-ask, then a
//!    typed `timeout` — so a caller always gets an answer.
//! 2. **Every host store on a node answered every join it saw.** A
//!    `join` names no handle and `key` is the caller's opaque policy
//!    token, so nothing in the frame said which store it was for; a
//!    store hosted while a sibling was live adopted the sibling's
//!    stream id and was then unreachable by its own replicas. This
//!    page hosts THREE stores, so the re-ask in (1) was landing on a
//!    store that had stolen another's stream — which is why an
//!    earlier attempt at (1) alone appeared to break witnesses 4 and
//!    5, and was reverted with the cause recorded as handle capacity.
//!    That recorded cause was WRONG, and the review said so with the
//!    arithmetic: `MAX_HANDLES` is 256 per owner and those witnesses
//!    use a different owner. Streams are now claimed per transport.
//!
//! Three candidate explanations were refuted along the way and are
//! kept so they are not re-run: the harness's offer budget (raised
//! 200 → 2000 per IP per minute, no change — it stays raised, since
//! 200 was the default for a run with far fewer contexts); the anchor
//! not forwarding reliability control packets (refuted from source —
//! the relay arm is header-only and type-agnostic, `mesh.rs`'s
//! `dest_id != local` case, where the subprotocol is read only to
//! choose a counter); and the WASM leaf's lack of a periodic tick
//! (`LeafNode::tick` → `drive_reliability` runs only from `pump()`,
//! which is TRUE and remains true — it simply was not what stalled
//! this join, since the repaired store recovers with the same leaf).
//!
//! Both leaves' raw counter ledgers still print when the leg fails
//! (`Step5::NodeCounters`), because which side is silent about a loss
//! is the diagnosis.
//!
//! ## The ten witnesses, in the order they run
//!
//! 1. a multi-chunk snapshot installs, receiver-observed;
//! 2. an action crosses, executes once, and its result comes back;
//! 3. the same install, through injected loss **and** reorder;
//! 4. a duplicated wire message moves the view exactly once AND
//!    costs the replica no upstream word;
//! 5. store traffic moves the anchor's per-pair forwarding counter
//!    while the pair is relayed;
//! 6. narrowing one replica's audience withholds from IT and from
//!    nobody else;
//! 7. a reconnect recovers the current view without replaying the
//!    action it already accepted, and a leave frees what the host
//!    was holding;
//! 8. and the SAME traffic leaves the per-pair counter exactly flat
//!    once the pair is direct — the other half of the plan's
//!    criterion, which is a pair of readings and not one.
//!
//! Then, on a pair of contexts of their own, criterion 3's last two
//! clauses (`refusals`):
//!
//! 9. an unauthorized write is refused with a typed `forbidden`
//!    while the same replica is still served; and
//! 10. a handle whose owner was REPLACED is refused `owner-lost`,
//!    and the successor adopts nothing.
//!
//! ## The "unexplained ZERO", answered
//!
//! For two rounds witness 6's untouched replica read ZERO command
//! entries after a host write, where at the change it read four. I
//! reported it rather than asserting it, and the cause was the
//! INSTRUMENT: `Step5::StoreCommit`'s `entries: None` crossed the
//! wire as JSON `null`, `leaf5.js` compared it against `undefined`,
//! and every tick-only commit replaced the host's document with
//! `bulkEntries(null)` — an EMPTY one. So the replica read no
//! command entries because there were no entries of any kind, and
//! every witness that commits a tick had been measuring a delta on a
//! document it had just emptied.
//!
//! Both halves are now `null`-safe (`skip_serializing_if` on the
//! step, `== null` on the page), and the reading is ASSERTED: the
//! untouched replica still holds its four, over a document that
//! still has all its entries. The total is part of the oracle
//! precisely because an emptied document satisfies every reading
//! about the NARROWED replica — which is how this hid.
//!
//! ## Criterion 3's refusal clauses, and why they are elsewhere
//!
//! They were first written onto the measured pair above, where they
//! ran green — but they put two more stores and their joins on those
//! pages, and the §9 promotion witness 8 depends on then stopped
//! completing: the offerer reported `iceTimeout` while the answerer
//! reported `no verified offer from 0x… is waiting`, i.e. the offer
//! never arrived. Moving those stores to the third participant,
//! closing every finished store first, re-announcing and
//! re-discovering before the attempt, and attempting twice all
//! failed to restore it. Three green witnesses would have gone red
//! to buy two, which is a trade this stage does not make.
//!
//! So they now run in `refusals`, on their own pair of contexts with
//! their own session, capability tag and identities, AFTER the
//! measured pair's pages are closed. That is not a workaround for
//! the promotion: no promotion is involved there at all, and the
//! measured pair is already gone. The run that established this
//! reports 57/0 with witness 8 promoting on attempt 1, so the load
//! that broke it is load on the SIGNALLING PAIR's own pages, not
//! store count per se — the isolating experiment that would say
//! which is still not run, and this file does not claim to know.
//!
//! **What witness 10 found, which nothing in process had.** Its
//! first run read `indeterminate` — "the store did not answer before
//! the deadline" — where `closed` was expected. A host that closed
//! took its answer with it: §1.6's rule that a caller learns why
//! rather than inferring it from silence had been applied to lease
//! EXPIRY and not to CLOSURE, so every replica of a closed host
//! waited out its own 20-second deadline and then could only say it
//! did not know. A closing owner now says goodbye to every handle it
//! holds (`owner.farewell`, awaited by `close()` and sent only on
//! streams already open), spelled `owner-lost` rather than `closed`
//! because the two are different events — `closed` is the notice a
//! replica REJOINS on, and rejoining here would have silently
//! attached the caller to a SUCCESSOR's different document under the
//! handle it already had. Five inverses in
//! `browser-ts/test/store/hosted.test.ts` and
//! `test/store/wire.test.ts`; the awaited-ness of the goodbye needed
//! a late-flushing send in the double before it could discriminate
//! at all.
//!
//! 8 is LAST ON THIS PAIR because a promotion replaces the session,
//! and a store that opens a new stream on it afterwards is refused
//! by a fenced stream id. Everything that opens one runs before it —
//! and 9 and 10 run after it on contexts of their own, which is why
//! they are unaffected by that fence.
//!
//! **These numbers are the EXECUTION order**, matching the `// --- N`
//! section comments below, and they are the numbering every sentence
//! in this header uses. The previous header listed loss second and
//! the action third, which is the ARRAY's order — the review caught
//! it, twice.
//!
//! `WITNESSES` is a DIFFERENT order — 0 snapshot, 1 loss, 2 action,
//! 3 duplicate, 4 routed, 5 direct, 6 audience, 7 reconnect, 8
//! unauthorized, 9 replaced — and stays that way because each record
//! names its position by index, so reordering the array would
//! silently retarget records (the same reason Stage 5's list is
//! append-only). An earlier header mixed the two numberings and so
//! named the wrong witness as the red one; the whole value of
//! leaving a witness red is that the record can be trusted, and the
//! review caught this one.
//!
//! **The sixth property is here now**, and what unblocked it was the
//! identity separation: these contexts could not discover each other
//! while Stage 7's two entity secrets were Stage 6 slice 3's, and
//! without discovery there is no `peer_offer` and so no direct pair.
//! Witness 8 — the numbering here is EXECUTION order — promotes the
//! pair through the PUBLIC page loop and then re-reads the same
//! counter the routed witness read.
//!
//! It also found a defect no in-process suite could: a promotion
//! REPLACES the session, and both halves of the store were caching a
//! stream handle opened on its predecessor. The counter went
//! perfectly flat and the replica never saw the commit — flat for
//! the wrong reason, which is exactly what the plan's "a flat
//! counter alone is not success" is about. Both sides now reopen
//! once on a failed send (`host.ts`, `join.ts`), witnessed in
//! `test/store/hosted.test.ts` with a double that stales a handle
//! the way the leaf does.
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
use crate::stage6::{discover, leaf_of, routing_id, stat_bool, Leaf};
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

pub const WITNESSES: [&str; 10] = [
    "stage7_store_snapshot_installs_over_the_real_stream",
    "stage7_store_snapshot_installs_through_injected_loss_and_reorder",
    "stage7_an_action_round_trip_crosses_the_real_transport",
    "stage7_a_duplicated_store_frame_moves_the_view_once",
    "stage7_store_traffic_moves_the_anchors_per_pair_counter_when_routed",
    // Appended, never inserted: every record above names its
    // position by index.
    "stage7_store_traffic_leaves_the_counter_flat_once_the_pair_is_direct",
    "stage7_narrowing_an_audience_withholds_only_that_replicas_view",
    "stage7_a_reconnect_recovers_without_replaying_and_a_leave_frees_the_handle",
    // Criterion 3's last two clauses, on their OWN topology — see
    // `refusals` for why they are not on the pair above.
    "stage7_an_unauthorized_write_is_refused_while_the_replica_still_reads",
    "stage7_a_handle_from_a_replaced_owner_is_refused",
];

/// The tabs this stage drives, on their own isolated contexts.
pub const TABS: [&str; 5] = [TAB_HOST, TAB_PLAYER, TAB_SECOND, TAB_OWNER, TAB_CLIENT];

const TAB_HOST: &str = "s7host";
const TAB_PLAYER: &str = "s7player";
/// A SECOND independent participant.
///
/// Not a convenience: the audience witness needs two replicas of ONE
/// store with different audiences, and two replicas on one node
/// cannot have them — both would open the stream the shared label
/// derives, and the second open fails the stream terminally. Two
/// participants is also what the plan's criterion says ("independent
/// browser participants"), so the topology is the honest one rather
/// than a workaround.
const TAB_SECOND: &str = "s7second";
/// The refusal topology's two contexts — its own pair, own session,
/// own capability tag, own identities.
const TAB_OWNER: &str = "s7owner";
const TAB_CLIENT: &str = "s7client";
const PAGE_HOST: &str = "stage7-host";
const PAGE_PLAYER: &str = "stage7-player";
const PAGE_SECOND: &str = "stage7-second";
const CTX_HOST: &str = "stage7-ctx-host";
const CTX_PLAYER: &str = "stage7-ctx-player";
const CTX_SECOND: &str = "stage7-ctx-second";
const PAGE_OWNER: &str = "stage7-owner";
const PAGE_CLIENT: &str = "stage7-client";
const CTX_OWNER: &str = "stage7-ctx-owner";
const CTX_CLIENT: &str = "stage7-ctx-client";

/// One capability tag, so each leaf can discover the other's signed
/// announcement — which is what installs the relayed session the
/// store's `openStream({peer})` needs.
const STORE_TAG: &str = "stage7.store";

/// The refusal topology's own tag, so its two leaves discover each
/// other and nothing else — a shared tag would have the refusal
/// client discovering the measured pair's host as well.
const REFUSAL_TAG: &str = "stage7.refusal";

/// Entries the host projects ONLY to the `command` audience.
///
/// Small and named (`cmd-0`…): the audience witness reads them by
/// count, and a projection that withholds nothing cannot witness an
/// audience change at all.
const COMMAND_ENTRIES: u32 = 4;

// Identities unique to this stage, and the reason that matters more
// than it looks: an entity secret IS the node id. These constants
// were copied from Stage 6 slice 3 (`c6…`/`c7…` and `d6…`/`d7…`), so
// Stage 7's two leaves and slice 3's two leaves were THE SAME TWO
// NODES. Whichever stage ran second announced under an identity the
// anchor already held, and its capability query came back with zero
// peers — the "the leaves did not discover each other" failure that
// looked like a capacity problem for three rounds. Every byte here
// has to stay distinct from `SECRET_*` in `stage6.rs`.
const SECRET_HOST_ENTITY: &str = "e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6";
const SECRET_HOST_NOISE: &str = "e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7";
const SECRET_PLAYER_ENTITY: &str =
    "f6f6f6f6f6f6f6f6f6f6f6f6f6f6f6f6f6f6f6f6f6f6f6f6f6f6f6f6f6f6f6f6";
const SECRET_PLAYER_NOISE: &str =
    "f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7";
const SECRET_SECOND_ENTITY: &str =
    "e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4";
const SECRET_SECOND_NOISE: &str =
    "e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5";
const SECRET_OWNER_ENTITY: &str =
    "e2e2e2e2e2e2e2e2e2e2e2e2e2e2e2e2e2e2e2e2e2e2e2e2e2e2e2e2e2e2e2e2";
const SECRET_OWNER_NOISE: &str = "e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3";
const SECRET_CLIENT_ENTITY: &str =
    "f2f2f2f2f2f2f2f2f2f2f2f2f2f2f2f2f2f2f2f2f2f2f2f2f2f2f2f2f2f2f2f2";
const SECRET_CLIENT_NOISE: &str =
    "f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3";

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
/// `hp`/`ph` are the pair under test, either direction.
///
/// No `signal_forwarded` field: it was declared here as a positive
/// control for a FLAT window — "the anchor is still relaying for this
/// pair while their application data has stopped going through it" —
/// and this stage makes no flat claim. Witness 5 asserts the pair
/// counter MOVED, which needs no proof that the anchor is alive: a
/// dead anchor moves nothing. The flat-window claim, and its control,
/// belong to Stage 6's direct-path witnesses. A documented control
/// that no oracle reads is worse than none, because the doc tells the
/// next reader a control is in force.
#[derive(Debug, Clone, Copy)]
struct Forwarded {
    hp: u64,
    ph: u64,
}

impl Forwarded {
    fn read(anchor: &MeshNode, host: &Leaf, player: &Leaf) -> Self {
        Self {
            hp: anchor.forwarded_app_packets(routing_id(host), player.node_id),
            ph: anchor.forwarded_app_packets(routing_id(player), host.node_id),
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
    Some(result.stats.as_ref()?.get(key)?.as_str()?.to_string())
}

fn why(result: &StepResult) -> String {
    result
        .error
        .clone()
        .unwrap_or_else(|| "no error".to_string())
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

    let tab_second = TAB_SECOND;
    let url_host = format!("{}/leaf5.html?tab={TAB_HOST}", cx.page_origin);
    let url_player = format!("{}/leaf5.html?tab={TAB_PLAYER}", cx.page_origin);
    let url_second = format!("{}/leaf5.html?tab={TAB_SECOND}", cx.page_origin);
    for (page, url, ctx) in [
        (PAGE_HOST, &url_host, CTX_HOST),
        (PAGE_PLAYER, &url_player, CTX_PLAYER),
        (PAGE_SECOND, &url_second, CTX_SECOND),
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
    let second_connected = script
        .run(
            tab_second,
            connect(SECRET_SECOND_ENTITY, SECRET_SECOND_NOISE),
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
    for tab in [tab_host, tab_player, tab_second] {
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
    let second = leaf_of(&second_connected);
    let found_player = discover(&mut script, tab_host, &player.node_hex, &session, STORE_TAG).await;
    let found_host = discover(&mut script, tab_player, &host.node_hex, &session, STORE_TAG).await;
    // The second participant discovers the HOST too — its replica
    // cannot address a store on a peer it has no session with, which
    // is the same precondition the player's join has.
    let found_host_from_second =
        discover(&mut script, tab_second, &host.node_hex, &session, STORE_TAG).await;
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

    // `command_entries` is per store, and only the audience store has
    // any: a host whose document carries entries the joining audience
    // cannot read has a DIFFERENT digest from its replica, which is
    // exactly what witnesses 1 and 2 compare. Giving every store four
    // of them turned both of those green witnesses red — correctly.
    let host_store_with = |handle: &str, command_entries: u32| Step5::StoreHost {
        id: 0,
        session: session.clone(),
        handle: handle.to_string(),
        label: format!("store/stage7/{handle}"),
        store: None,
        entries: ENTRIES,
        command_entries,
        max_event_bytes: MAX_EVENT_BYTES,
        refuse_writes: false,
    };
    let host_store = |handle: &str| host_store_with(handle, 0);
    let join_as = |handle: &str, store: &str, audience: &[&str]| Step5::StoreJoin {
        id: 0,
        session: session.clone(),
        handle: handle.to_string(),
        store: store.to_string(),
        label: format!("store/stage7/{store}"),
        host_hex: host.node_hex.clone(),
        audience: audience.iter().map(|name| (*name).to_string()).collect(),
        key: "harness".to_string(),
        max_event_bytes: MAX_EVENT_BYTES,
        drop_every: 0,
        reorder_every: 0,
        duplicate_every: 0,
        timeout_ms: 30_000,
    };
    let join_store = |handle: &str, drop_every: u32, reorder_every: u32, duplicate_every: u32| {
        Step5::StoreJoin {
            id: 0,
            session: session.clone(),
            handle: handle.to_string(),
            store: handle.to_string(),
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
    // Snapshot chunks the REPLICA received on its own stream, not
    // the page's outbound submission count: a request, an ack or a
    // control packet satisfies an outbound total without a snapshot
    // ever having been cut, so the old reading could pass with one
    // chunk. Counted at the joining page, on the stream that
    // replica opened.
    let chunks = stat_u64(&joined, "snap_chunks").unwrap_or(0);
    let installed = joined.ok
        && host_digest.is_some()
        && host_digest == replica_digest
        && stat_u64(&joined, "entries") == Some(u64::from(ENTRIES))
        // The premise the detail line argues from, asserted rather
        // than merely reported: a single-message join would make this
        // a test of one `send`. It holds by construction of ENTRIES
        // and MAX_EVENT_BYTES today, and construction is exactly what
        // a budget change alters.
        && chunks > 1;
    ledger.record(
        WITNESSES[0],
        installed,
        format!(
            "A STORE SNAPSHOT CROSSES A REAL DATACHANNEL AND INSTALLS. The host holds \
             {ENTRIES} entries; the joining replica's OWN installed document is what is \
             read here, digested and compared with the host's — not the step's ok flag, \
             and not a partial view, because `ready()` resolves only once the manifest, \
             every chunk and the definition's validation have landed. host={host_digest:?} \
             replica={replica_digest:?} entries={:?} snapshot chunks received={chunks} \
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
    // Both hooks are armed on the HOST side, below, because the
    // snapshot is the host's outbound traffic — this comment used to
    // say the joining side, immediately above the step that arms the
    // host, which is the drift a reader trusts and should not have
    // to check. What recovers a gap is the transport's reliability
    // AND the store's own re-ask when nothing noticed the loss
    // (a manifest opens no assembly); the witness does not separate
    // them.
    let hosted_lossy = script.run(tab_host, host_store("lossy")).await;
    // Armed on the HOST, because the snapshot is the host's outbound
    // traffic. Arming the joining page faulted its requests and
    // acknowledgements instead, so a non-zero drop count there said
    // nothing about a chunk — the reviewer's third finding, and the
    // reason this is its own step.
    let armed = script
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
    let faults = script
        .run(tab_host, Step5::StoreFaultsReport { id: 0 })
        .await;
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
        && armed.ok
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
             transport double that cannot lose anything. Here every 7th outbound \
             DataChannel message was DROPPED and every 5th held back so the next \
             overtook it, for the whole join. The hooks' own counts come back and are \
             asserted NON-ZERO — a loss witness that dropped nothing is a witness about \
             nothing: dropped={dropped} reordered={swapped}. The document still installed \
             byte-identical: host={:?} replica={lossy_digest:?}. WHAT RECOVERED IT is \
             the two layers TOGETHER, and this witness does not separate them: the \
             transport repairs a gap it notices (`wire/src/reliability.rs` and \
             `leaf/src/stream.rs`'s reorder buffer), and the STORE asks again for what \
             nothing noticed — a lost manifest opens no assembly, so the joiner's own \
             deadline reissues the join (`replica.ts`, driven by `join.ts`). An earlier \
             version of this line credited the transport alone; a review probe observed \
             the second join directly, so that was a claim this witness could not make. \
             {}",
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
    // THIS replica's own UPSTREAM traffic across the window, counted
    // at the transport it was handed — the page-wide `wire.messages`
    // cannot serve, because three joined stores and their `alive`
    // renewals share it. The publication count alone cannot carry
    // the property either: a
    // re-applied identical delta and a full resync that reinstalls
    // the same document BOTH publish exactly once (`core.ts`'s
    // `#publish` returns early when the next state is the same
    // object). The review's probe A proved that — with every frame
    // duplicated the count stayed 1 while the replica sent a
    // `resync`. What separates "idempotent" from "recovered" is
    // whether the replica had to say anything at all.
    // Both readings must be PRESENT and monotone before their
    // difference means anything: defaulting a missing counter to
    // zero and subtracting with saturation makes "no upstream
    // traffic" the answer to "no instrumentation", which is the
    // failure mode this witness exists to rule out.
    let upstream_before = stat_u64(&before_state, "upstream_frames");
    let upstream_after = stat_u64(&after, "upstream_frames");
    let upstream_read = match (upstream_before, upstream_after) {
        (Some(before), Some(after)) if after >= before => Some(after - before),
        _ => None,
    };
    let replica_sent = upstream_read;
    let once = joined_dup.ok
        && committed.ok
        && after.ok
        && duplicated > 0
        && replica_tick == Some(41)
        && before_tick != 41
        && moves == 1
        && replica_sent == Some(0);
    ledger.record(
        WITNESSES[3],
        once,
        format!(
            "A DUPLICATED FRAME MOVES THE VIEW ONCE AND COSTS NOTHING — counted, not \
             inferred. Every outbound message of the host's commit was sent TWICE \
             (duplicated={duplicated}, asserted non-zero so an inert hook cannot pass \
             this). TWO readings, because neither alone is enough. (a) PUBLICATIONS the \
             replica made: {applications_before} → {applications_after}, so exactly \
             {moves} — the final value cannot carry it, since the commit ASSIGNS tick \
             41 and applying the delta twice ends at 41 too. (b) The replica's own \
             UPSTREAM messages across the same window: {replica_sent:?}, asserted
             `Some(0)` — present, monotone and zero, because a MISSING reading must
             not read as silence. \
             (b) exists because (a) cannot tell IDEMPOTENT from RECOVERED: a resync \
             that reinstalls the same document also publishes once (`core.ts`'s \
             `#publish` returns early on an unchanged state), and the review's probe A \
             demonstrated exactly that — the count stayed 1 while the replica sent a \
             `resync`. A duplicate the leaf absorbed costs the replica no words. Value \
             read back: {replica_tick:?} from {before_tick}. What makes this the right \
             answer is the leaf delivering a retransmitted duplicate once \
             (`leaf/src/stream.rs`). {} {}",
            why(&committed),
            why(&after)
        ),
    );

    // --- 5. routed: the anchor's per-pair counter -------------------
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

    // --- 6. narrowing an audience ----------------------------------
    //
    // BEFORE the direct promotion below, and that ordering is a
    // finding rather than a preference: a promotion REPLACES the
    // session (§9 step 4), and a store that opens a NEW stream on it
    // afterwards was refused `stream … failed terminally
    // (incarnation 3); reconnect or use another stream id`. The
    // reopen repair covers a HELD handle; it does not resurrect an
    // id the session has fenced. So everything that opens a stream
    // runs while the pair is relayed, and the promotion is the last
    // thing this stage does.
    //
    // The plan's criterion has two halves and the second is the one a
    // naive implementation fails: changing an audience must stop
    // irrelevant delivery AND remove what left the view, WITHOUT
    // removing entities another active audience still covers. So two
    // replicas of ONE store join with the same wide audience, and
    // only one of them narrows.
    //
    // The host projects `cmd-*` entries to `command` alone
    // (`leaf5.js`), so "withheld" is observable as entries the
    // replica no longer holds rather than as a flag.
    let _hosted_aud = script
        .run(tab_host, host_store_with("aud", COMMAND_ENTRIES))
        .await;
    let wide = script
        .run(tab_player, join_as("aud-wide", "aud", &["crew", "command"]))
        .await;
    // On the SECOND participant: two replicas of one store on one
    // node would both open the stream their shared label derives.
    let narrowing = script
        .run(
            tab_second,
            join_as("aud-narrow", "aud", &["crew", "command"]),
        )
        .await;
    let wide_before = stat_u64(&wide, "command_entries");
    let narrow_before = stat_u64(&narrowing, "command_entries");
    let narrowed = script
        .run(
            tab_second,
            Step5::StoreAudience {
                id: 0,
                handle: "aud-narrow".to_string(),
                audience: vec!["crew".to_string()],
                settle_ms: 400,
                timeout_ms: 20_000,
            },
        )
        .await;
    // A HOST WRITE AFTER THE CHANGE, which both replicas' windows
    // then answer. Without it "and nobody else" rested on one reading
    // of a replica that might simply have stopped receiving: a review
    // probe blocked delivery to the untouched replica BEFORE the
    // narrowing and the old oracle still passed, because unchanged
    // and deaf look identical. A replica that must OBSERVE something
    // in the same window cannot be deaf.
    // The untouched replica, read AT the change — before the write
    // below — because that is where "and nobody else" is a claim
    // about the narrowing.
    let wide_at_change = script
        .run(
            tab_player,
            Step5::StoreState {
                id: 0,
                handle: "aud-wide".to_string(),
            },
        )
        .await;
    let live_commit = script
        .run(
            tab_host,
            Step5::StoreCommit {
                id: 0,
                handle: "aud".to_string(),
                entries: None,
                tick: Some(23),
                duplicate_every: 0,
                settle_ms: 500,
            },
        )
        .await;
    let wide_after = script
        .run(
            tab_player,
            Step5::StoreState {
                id: 0,
                handle: "aud-wide".to_string(),
            },
        )
        .await;
    let narrow_after_commit = script
        .run(
            tab_second,
            Step5::StoreState {
                id: 0,
                handle: "aud-narrow".to_string(),
            },
        )
        .await;
    let narrow_after = stat_u64(&narrowed, "command_entries");
    let wide_kept = stat_u64(&wide_at_change, "command_entries");
    // The SAME reading AFTER the host's write, and now asserted.
    //
    // It read ZERO for two rounds and I reported it as unexplained.
    // The cause was the instrument: a tick-only `store_commit`
    // crossed the wire with `entries: null`, the page compared it
    // against `undefined`, and the host's document was replaced with
    // an EMPTY one — so the replica read no command entries because
    // there were no entries of any kind. With that repaired the
    // reading means what it says, and this is the conjunct that
    // makes criterion 2's prohibition a claim rather than a
    // sentence: entries another active audience still covers are NOT
    // removed by a sibling's narrowing.
    let wide_after_write = stat_u64(&wide_after, "command_entries");
    let second_ready = second.is_some() && found_host_from_second.is_some();
    let audience_held = second_ready
        && wide.ok
        && narrowing.ok
        && narrowed.ok
        && wide_before == Some(u64::from(COMMAND_ENTRIES))
        && narrow_before == Some(u64::from(COMMAND_ENTRIES))
        && narrow_after == Some(0)
        && wide_kept == Some(u64::from(COMMAND_ENTRIES))
        // BOTH replicas observed the host's write in the same window,
        // so neither reading is a reading of a deaf replica — and the
        // narrowed one STILL holds no command entries, which is
        // criterion 2's "stops irrelevant delivery" half: the
        // withholding survives later traffic rather than being a
        // one-off at the moment of the change.
        && live_commit.ok
        && stat_u64(&wide_after, "tick") == Some(23)
        && stat_u64(&narrow_after_commit, "tick") == Some(23)
        && stat_u64(&narrow_after_commit, "command_entries") == Some(0)
        && wide_after_write == Some(u64::from(COMMAND_ENTRIES))
        // The write preserved the document: a commit that emptied it
        // would satisfy every conjunct above about the NARROWED
        // replica, which is exactly how the wipe hid for two rounds.
        && stat_u64(&wide_after, "entries") == Some(u64::from(ENTRIES + COMMAND_ENTRIES))
        // The narrowed replica keeps everything its remaining
        // audience covers: this is a withholding, not a reset.
        && stat_u64(&narrowed, "entries") == Some(u64::from(ENTRIES));
    ledger.record(
        WITNESSES[6],
        audience_held,
        format!(
            "NARROWING AN AUDIENCE WITHHOLDS FROM THAT REPLICA AND NOBODY ELSE. Two \
             replicas of ONE store, on TWO INDEPENDENT PARTICIPANTS (a third browsing \
             context, connected and discovered={second_ready} — two replicas on ONE \
             node cannot have different audiences here, because both would open the \
             stream their shared label derives), joined with `[crew, command]`, and the host projects \
             `cmd-*` entries to `command` alone — so a withheld entry is an entry the \
             replica NO LONGER HOLDS, not a flag. Both installed {COMMAND_ENTRIES} \
             command entries ({wide_before:?} and {narrow_before:?}). One then asked for \
             `[crew]`: its command entries went to {narrow_after:?} while it KEPT all \
             {ENTRIES} crew entries ({:?}) — a withholding, not a reset. The other \
             replica, re-read after the change, still holds {wide_kept:?}, which is the \
             half that fails when a host projects per STORE instead of per HANDLE. \
             Then the host WROTE (tick 23) and both replicas answered it — wide {:?}, \
             narrowed {:?} — so neither reading above is a reading of a deaf replica, \
             which is how the first version of this witness could have passed with \
             delivery to the untouched one blocked. The narrowed replica is still at \
             {:?} command entries after that write, and the untouched one STILL holds \
             {wide_after_write:?} over a document that still has {:?} entries in \
             total. Those two together are criterion 2's prohibition — a sibling's \
             narrowing removes nothing another active audience covers — and the total \
             is asserted because a commit that EMPTIED the document would satisfy \
             every reading about the narrowed replica. For two rounds one did: a \
             tick-only commit crossed the wire as `entries: null`, the page compared \
             it against `undefined`, and the host's document was replaced with \
             nothing. That instrument defect, not an audience defect, was the \
             \"unexplained ZERO\" — found by review probes on both halves. {} {} {}",
            stat_u64(&narrowed, "entries"),
            stat_u64(&wide_after, "tick"),
            stat_u64(&narrow_after_commit, "tick"),
            stat_u64(&narrow_after_commit, "command_entries"),
            stat_u64(&wide_after, "entries"),
            why(&narrowed),
            why(&wide_after),
            why(&live_commit)
        ),
    );

    // --- 7. reconnect, and leave -----------------------------------
    //
    // Three properties the plan names together, on one handle: a
    // reconnect recovers the CURRENT state, the action it accepted
    // before the reconnect is NOT replayed, and a leave frees what
    // the host was holding for it.
    //
    // Non-duplication is read from the HOST's own document: `bump`
    // ADDS, so a replayed action shows up as a tick that moved twice.
    let acted_before = script
        .run(
            tab_player,
            Step5::StoreAct {
                id: 0,
                handle: "aud-wide".to_string(),
                by: 7,
                settle_ms: 300,
                timeout_ms: 20_000,
            },
        )
        .await;
    let host_after_act = script
        .run(
            tab_host,
            Step5::StoreCounts {
                id: 0,
                handle: "aud".to_string(),
            },
        )
        .await;
    let tick_after_act = stat_u64(&host_after_act, "tick");

    let reconnected = script
        .run(
            tab_player,
            Step5::StoreReconnect {
                id: 0,
                handle: "aud-wide".to_string(),
                settle_ms: 400,
                timeout_ms: 20_000,
            },
        )
        .await;
    let host_after_reconnect = script
        .run(
            tab_host,
            Step5::StoreCounts {
                id: 0,
                handle: "aud".to_string(),
            },
        )
        .await;
    let closed = script
        .run(
            tab_player,
            Step5::StoreClose {
                id: 0,
                handle: "aud-wide".to_string(),
                settle_ms: 600,
            },
        )
        .await;
    // Give the leave time to reach the host, then read what it holds.
    let host_after_leave = script
        .run(
            tab_host,
            Step5::StoreCounts {
                id: 0,
                handle: "aud".to_string(),
            },
        )
        .await;
    // Read AFTER the reconnect, because a resume may install a handle
    // of its own: a count taken before it is not the number a leave
    // subtracts from — the first version read 4 → 5 and called it a
    // leak.
    let handles_before_leave = stat_u64(&host_after_reconnect, "handles");
    let handles_after_leave = stat_u64(&host_after_leave, "handles");
    // A RESUME THAT WAS ANSWERED, not a re-read of what the replica
    // still held. `reconnect()` awaits the SEND and the replica keeps
    // its last snapshot marked STALE, so "tick matches and a digest
    // exists" is satisfied by the view it already had — a review
    // probe silenced the host and that oracle still passed. The
    // discriminator was already in the step's own stats and unread:
    // the phase must be `ready` and `stale` must be false.
    let resumed_phase = reconnected
        .stats
        .as_ref()
        .and_then(|stats| stats.get("status"))
        .and_then(|status| status.get("phase"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    let resumed_stale = reconnected
        .stats
        .as_ref()
        .and_then(|stats| stats.get("status"))
        .and_then(|status| status.get("stale"))
        .and_then(serde_json::Value::as_bool);
    let recovered = stat_u64(&reconnected, "tick") == tick_after_act
        && stat_str(&reconnected, "digest").is_some()
        && resumed_phase.as_deref() == Some("ready")
        && resumed_stale == Some(false);
    let not_replayed = stat_u64(&host_after_reconnect, "tick") == tick_after_act;
    let freed = match (handles_before_leave, handles_after_leave) {
        (Some(before), Some(after)) => before > 0 && after == before - 1,
        _ => false,
    };
    ledger.record(
        WITNESSES[7],
        acted_before.ok && reconnected.ok && closed.ok && recovered && not_replayed && freed,
        format!(
            "A RECONNECT RECOVERS WITHOUT REPLAYING, AND A LEAVE FREES THE HANDLE. The \
             replica's `bump {{by: 7}}` was accepted and moved the host's document to \
             tick {tick_after_act:?}. Then `reconnect()` — §1.6's resume — and the \
             replica's own view came back at that same tick \
             ({:?}, digest {:?}), while the HOST's document did NOT move \
             ({:?}): the accepted action was recovered, not re-executed, which is read \
             from the authority's own state because `bump` ADDS and a replay would show \
             as a second increment. Finally the replica LEFT, and what the host was \
             holding for it went with it: handles {handles_before_leave:?} → \
             {handles_after_leave:?}. That is a CARDINALITY and not an identity: \
             `counts()` reports how many handles the owner holds, not which, so this \
             reading cannot by itself distinguish \"this replica's handle went\" from \
             \"one handle went\" — the claim it carries is exactly the count. It is \
             counted on the HOST because on the replica's side a subscription that \
             outlived its caller reads identically to one that was cleaned up. \
             Resume answered: phase={resumed_phase:?} stale={resumed_stale:?}. {} {} {}",
            stat_u64(&reconnected, "tick"),
            stat_str(&reconnected, "digest"),
            stat_u64(&host_after_reconnect, "tick"),
            why(&acted_before),
            why(&reconnected),
            why(&host_after_leave)
        ),
    );

    // --- 8. direct: the same traffic, and the counter does NOT move -
    //
    // The plan's criterion is a PAIR of readings, not one: flat for
    // direct AND increasing for routed, both with delivery
    // receiver-observed. Witness 5 above holds the routed half on
    // this very pair with this very commit shape, which is what
    // makes the flat reading here mean something — a counter that
    // never moved for any reason would satisfy "flat" and say
    // nothing at all.
    //
    // ROUTING IS READ FROM THE LEAF, not from candidate labels: the
    // attempt's own typed outcome plus `peerAttempt`'s `direct` flag
    // are the §9 promotion's result, and a `host`/`srflx` label on a
    // candidate is not a claim about where packets went.
    //
    // SIGNALLING IS A DIFFERENT COUNTER and is reported, never
    // conflated: `forwarded_app_packets` excludes
    // `SUBPROTOCOL_RTC_SIGNAL` by construction (`mesh.rs`'s transit
    // arm), which is exactly why "flat" can be a claim about
    // application data while the pair is still signalling.
    // The SECOND participant's work is done (witnesses 6 and 7), and
    // it is released before the promotion: a third context is a third
    // `RTCPeerConnection` gathering against the same loopback anchor,
    // and the first version of this section reported `iceTimeout`
    // with it still live. Fewer peers is not a workaround here — the
    // promotion is between the pair this stage measures.
    let _ = script
        .run(
            tab_second,
            Step5::StoreClose {
                id: 0,
                handle: "aud-narrow".to_string(),
                settle_ms: 200,
            },
        )
        .await;
    let _ = cx.driver.close_page(PAGE_SECOND).await;

    // And every store whose witness is finished, on both pages,
    // leaving only the `dup` pair this measurement uses.
    //
    // Not tidiness: the promotion is SIGNALLING, and it has to cross
    // the same anchor these stores are talking through. With six
    // stores live the answerer reported `no verified offer from 0x…
    // is waiting` and the offerer `iceTimeout` — the offer had not
    // arrived by the time `acceptPeer` looked. The pair being
    // measured is quiet by the time it is promoted, which is also
    // the only state in which "flat" means anything.
    for (tab, handle) in [(tab_host, "aud")] {
        let _ = script
            .run(
                tab,
                Step5::StoreClose {
                    id: 0,
                    handle: handle.to_string(),
                    settle_ms: 100,
                },
            )
            .await;
    }

    // NO re-announce and no re-discovery here, and that absence is a
    // measurement rather than an omission. An earlier version did
    // both, on the theory that §9's signalling rides a relay entry
    // learned from an announcement lease minutes old — the review
    // flagged that the mitigation was four changes at once with an
    // unidentified load-bearing component, and it was right to. An
    // isolating run with the re-discovery REMOVED promoted the pair
    // on the FIRST attempt, so the theory was unsupported and the
    // code is deleted rather than kept as a charm.
    //
    // What remains is releasing finished work (the third context and
    // the `aud` store) and a two-attempt ladder, which the last
    // three runs have not needed: each promoted on attempt 1. The
    // ladder stays because `iceTimeout` is a typed disposition §9's
    // own drive loop re-attempts, not because a run here has ever
    // required it.
    // Two ATTEMPTS at most, because `iceTimeout` is a typed
    // disposition and not a failure — §9's own drive loop re-attempts
    // — and because a witness that gave up on the first timeout would
    // report a flat counter it had not earned. What is NOT retried is
    // the assertion: if neither attempt reaches `direct`, the witness
    // says so and fails.
    let mut connected = StepResult::default();
    let mut accepted = StepResult::default();
    let mut attempts = 0_u32;
    for _ in 0..2 {
        attempts += 1;
        let accept = script.spawn(
            tab_player,
            Step5::PeerAccept {
                id: 0,
                session: session.clone(),
                peer_hex: host.node_hex.clone(),
            },
        );
        connected = script
            .run(
                tab_host,
                Step5::PeerConnect {
                    id: 0,
                    session: session.clone(),
                    peer_hex: player.node_hex.clone(),
                },
            )
            .await;
        accepted = accept.await;
        if stat_str(&connected, "outcome").as_deref() == Some("direct") {
            break;
        }
    }
    let host_outcome = stat_str(&connected, "outcome");
    let player_outcome = stat_str(&accepted, "outcome");
    let host_attempt = script
        .run(
            tab_host,
            Step5::PeerAttempt {
                id: 0,
                session: session.clone(),
                peer_hex: player.node_hex.clone(),
            },
        )
        .await;
    let player_attempt = script
        .run(
            tab_player,
            Step5::PeerAttempt {
                id: 0,
                session: session.clone(),
                peer_hex: host.node_hex.clone(),
            },
        )
        .await;
    let both_direct = host_outcome.as_deref() == Some("direct")
        && player_outcome.as_deref() == Some("direct")
        && stat_bool(&host_attempt, "direct")
        && stat_bool(&player_attempt, "direct");

    let direct_before = Forwarded::read(anchor, &host, &player);
    let direct_commit = script
        .run(
            tab_host,
            Step5::StoreCommit {
                id: 0,
                handle: "dup".to_string(),
                entries: None,
                tick: Some(99),
                duplicate_every: 0,
                settle_ms: 500,
            },
        )
        .await;
    let direct_seen = script
        .run(
            tab_player,
            Step5::StoreState {
                id: 0,
                handle: "dup".to_string(),
            },
        )
        .await;
    let direct_after = Forwarded::read(anchor, &host, &player);
    let direct_delivered = stat_u64(&direct_seen, "tick") == Some(99);
    // EXACT equality, both directions, not "did not grow much".
    let stayed_flat = direct_after.hp == direct_before.hp && direct_after.ph == direct_before.ph;
    ledger.record(
        WITNESSES[5],
        both_direct && direct_commit.ok && direct_delivered && stayed_flat,
        format!(
            "STORE TRAFFIC LEAVES THE ANCHOR'S PER-PAIR COUNTER FLAT ONCE THE PAIR IS \
             DIRECT, and the pair's own routed reading above is what makes that a \
             claim. The pair was promoted through the PUBLIC page loop — \
             `connectPeer` on the host reported {host_outcome:?} after \
             {attempts} attempt(s) — printed because a two-attempt promotion must \
             not read like a one-attempt one — and `acceptPeer` on \
             the player {player_outcome:?}, and each leaf's own attempt says its \
             session is direct (host={}, player={}) — so routing is read from the \
             §9 promotion's result, never from a candidate's `host`/`srflx` label. \
             Then the SAME commit shape as witness 5: (host→player, player→host) went \
             from ({}, {}) to ({}, {}) — asserted EXACTLY equal, not merely small — \
             while the replica's own document moved to tick {:?}. Delivery is \
             receiver-observed for the same reason as above: a flat counter with \
             nothing delivered is the trivial way to pass this, and the plan says so. \
             Signalling rides a DIFFERENT counter and is not read here: \
             `forwarded_app_packets` excludes `SUBPROTOCOL_RTC_SIGNAL` in the anchor's \
             transit arm, which is what lets \"flat\" be a statement about application \
             data while the pair keeps signalling. {} {}",
            stat_bool(&host_attempt, "direct"),
            stat_bool(&player_attempt, "direct"),
            direct_before.hp,
            direct_before.ph,
            direct_after.hp,
            direct_after.ph,
            stat_u64(&direct_seen, "tick"),
            why(&direct_commit),
            why(&direct_seen)
        ),
    );

    for (tab, handle) in [(tab_host, "clean"), (tab_host, "lossy"), (tab_host, "dup")] {
        let _ = script
            .run(
                tab,
                Step5::StoreClose {
                    id: 0,
                    handle: handle.to_string(),
                    settle_ms: 0,
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
    let _ = cx.driver.close_page(PAGE_SECOND).await;

    // Criterion 3's last two clauses, on contexts of their own, after
    // the pair above is gone.
    refusals(&cx, ledger).await;
    Ok(())
}

/// Criterion 3's refusal clauses, on a topology of their own.
///
/// **Why a separate topology at all.** These two were first written
/// onto the pair above, and they ran green there — but they put two
/// more stores and two more joins on those pages, and the §9
/// promotion witness 8 depends on then stopped completing (the
/// offerer reported `iceTimeout`, the answerer `no verified offer
/// from 0x… is waiting`). Three green witnesses would have gone red
/// to buy two, which is not a trade this stage makes. So they get
/// their own pair of contexts, their own session, their own
/// capability tag and their own identities, and they run AFTER the
/// measured pair's pages are closed — no promotion is involved here,
/// and nothing above can be disturbed by what happens below.
///
/// **What each one reads.** Never "the step failed": the refusal's
/// typed CODE, plus the authority's own document, plus — for the
/// authorization clause — a later host commit ARRIVING at the same
/// replica. A refusal that came from a dead transport would satisfy
/// "the write did not land" and say nothing about policy, so the
/// replica has to still be served for the claim to be about
/// authorization. Both witnesses carry a positive control in the
/// same run: the replacement clause writes SUCCESSFULLY through the
/// handle first, so "refused" is a statement about the replacement
/// and not about the handle never having worked.
async fn refusals(cx: &Cx7<'_>, ledger: &mut Ledger) {
    let mut script = Script5::new(cx.tabs.clone(), 3_000_000);
    let session = "refuse".to_string();
    let fail = |ledger: &mut Ledger, detail: String| {
        for name in &WITNESSES[8..] {
            ledger.record(name, false, detail.clone());
        }
    };

    for (page, tab, ctx) in [
        (PAGE_OWNER, TAB_OWNER, CTX_OWNER),
        (PAGE_CLIENT, TAB_CLIENT, CTX_CLIENT),
    ] {
        let url = format!("{}/leaf5.html?tab={tab}", cx.page_origin);
        if let Err(reason) = cx.driver.open_page_in(page, &url, Some(ctx)).await {
            fail(
                ledger,
                format!("a Stage 7 refusal context would not open: {reason}"),
            );
            return;
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
        capabilities: vec![REFUSAL_TAG.to_string()],
        subscriptions: Vec::new(),
        lock_scope: None,
        expect_failure: false,
    };
    let owner_connected = script
        .run(TAB_OWNER, connect(SECRET_OWNER_ENTITY, SECRET_OWNER_NOISE))
        .await;
    let client_connected = script
        .run(
            TAB_CLIENT,
            connect(SECRET_CLIENT_ENTITY, SECRET_CLIENT_NOISE),
        )
        .await;
    let (owner, client) = match (leaf_of(&owner_connected), leaf_of(&client_connected)) {
        (Some(o), Some(c)) if o.node_id != c.node_id => (o, c),
        _ => {
            fail(
                ledger,
                format!(
                    "the refusal contexts are not two connected leaves: owner={} client={}",
                    why(&owner_connected),
                    why(&client_connected)
                ),
            );
            return;
        }
    };

    // Two DISTINCT nodes, printed: the refusal clauses are about one
    // node's policy over another's write, and a topology that
    // collapsed to one node would refuse nothing and prove nothing.
    println!(
        "[stage7] refusal topology: owner={} client={}",
        owner.node_hex, client.node_hex
    );

    // Discovery is a precondition of the store, not decoration: a
    // replica cannot `openStream({peer})` without a session, and the
    // relayed one is installed by the discovery path.
    for tab in [TAB_OWNER, TAB_CLIENT] {
        let announced = script
            .run(
                tab,
                Step5::Announce {
                    id: 0,
                    session: session.clone(),
                    capabilities: vec![REFUSAL_TAG.to_string()],
                },
            )
            .await;
        if !announced.ok {
            println!("[stage7] {tab} could not announce: {}", why(&announced));
        }
    }
    let found = discover(
        &mut script,
        TAB_CLIENT,
        &owner.node_hex,
        &session,
        REFUSAL_TAG,
    )
    .await;
    if found.is_none() {
        fail(
            ledger,
            format!(
                "the refusal client never discovered the owner {}, so it has no session \
                 to open a store stream on",
                owner.node_hex
            ),
        );
        return;
    }

    let host_of = |handle: &str, store: Option<&str>, refuse_writes: bool| Step5::StoreHost {
        id: 0,
        session: session.clone(),
        handle: handle.to_string(),
        // The SUCCESSOR answers at its predecessor's label, because
        // the id the leaf derives from it is what a replica's cached
        // stream is addressed to.
        label: format!("store/stage7r/{}", store.unwrap_or(handle)),
        store: store.map(str::to_string),
        entries: ENTRIES,
        command_entries: COMMAND_ENTRIES,
        max_event_bytes: MAX_EVENT_BYTES,
        refuse_writes,
    };
    let join_of = |handle: &str| Step5::StoreJoin {
        id: 0,
        session: session.clone(),
        handle: handle.to_string(),
        store: handle.to_string(),
        label: format!("store/stage7r/{handle}"),
        host_hex: owner.node_hex.clone(),
        audience: vec!["crew".to_string()],
        key: "harness".to_string(),
        max_event_bytes: MAX_EVENT_BYTES,
        drop_every: 0,
        reorder_every: 0,
        duplicate_every: 0,
        timeout_ms: 30_000,
    };

    // --- 9. an unauthorized write is refused, and reads continue ---
    let hosted = script.run(TAB_OWNER, host_of("policy", None, true)).await;
    let joined = script.run(TAB_CLIENT, join_of("policy")).await;
    let refused = script
        .run(
            TAB_CLIENT,
            Step5::StoreAct {
                id: 0,
                handle: "policy".to_string(),
                by: 5,
                settle_ms: 300,
                timeout_ms: 20_000,
            },
        )
        .await;
    let owner_after_refusal = script
        .run(
            TAB_OWNER,
            Step5::StoreCounts {
                id: 0,
                handle: "policy".to_string(),
            },
        )
        .await;
    // The replica is STILL SERVED: a host write after the refusal has
    // to arrive, or "the write did not land" is a statement about a
    // broken transport.
    let after = script
        .run(
            TAB_OWNER,
            Step5::StoreCommit {
                id: 0,
                handle: "policy".to_string(),
                entries: None,
                tick: Some(42),
                duplicate_every: 0,
                settle_ms: 500,
            },
        )
        .await;
    let client_reads = script
        .run(
            TAB_CLIENT,
            Step5::StoreState {
                id: 0,
                handle: "policy".to_string(),
            },
        )
        .await;
    let code = stat_str(&refused, "code");
    let authority_unmoved = stat_u64(&owner_after_refusal, "tick") == Some(0);
    let still_reading = stat_u64(&client_reads, "tick") == Some(42);
    ledger.record(
        WITNESSES[8],
        hosted.ok
            && joined.ok
            && !refused.ok
            && code.as_deref() == Some("forbidden")
            && authority_unmoved
            && after.ok
            && still_reading,
        format!(
            "AN UNAUTHORIZED WRITE IS REFUSED, WITH A TYPED CODE, AND THE REPLICA IS \
             STILL SERVED. The owner's `authorize` permits reads and refuses every \
             write, so the replica JOINED and installed a document — and its \
             `bump {{by: 5}}` came back refused with code {code:?} (asserted equal to \
             `forbidden`, not merely \"it threw\"). The AUTHORITY's own document is \
             where the refusal is confirmed: tick {:?}, asserted 0, so the action did \
             not execute rather than executing and failing to answer. Then the owner \
             committed tick 42 and the SAME replica read {:?} — which is what makes \
             this a claim about authorization: a refusal from a dead transport or an \
             unreachable store satisfies every other reading here. {} {} {}",
            stat_u64(&owner_after_refusal, "tick"),
            stat_u64(&client_reads, "tick"),
            why(&refused),
            why(&after),
            why(&client_reads)
        ),
    );

    // --- 10. a handle from a REPLACED owner is refused --------------
    let hosted_heir = script
        .run(TAB_OWNER, host_of("heirloom", None, false))
        .await;
    let joined_heir = script.run(TAB_CLIENT, join_of("heirloom")).await;
    // THE CONTROL, and it runs first: the very same handle writes
    // successfully while its owner is alive. Without it, "the write
    // was refused after the replacement" is satisfied by a handle
    // that never worked at all.
    let accepted = script
        .run(
            TAB_CLIENT,
            Step5::StoreAct {
                id: 0,
                handle: "heirloom".to_string(),
                by: 3,
                settle_ms: 300,
                timeout_ms: 20_000,
            },
        )
        .await;
    let before_replacement = script
        .run(
            TAB_OWNER,
            Step5::StoreCounts {
                id: 0,
                handle: "heirloom".to_string(),
            },
        )
        .await;
    // The owner goes away, and a SUCCESSOR takes the same store name
    // and the same label — which is the only way the replica's next
    // frame reaches anything at all.
    let retired = script
        .run(
            TAB_OWNER,
            Step5::StoreClose {
                id: 0,
                handle: "heirloom".to_string(),
                settle_ms: 400,
            },
        )
        .await;
    let successor = script
        .run(TAB_OWNER, host_of("heir", Some("heirloom"), false))
        .await;
    let stale_write = script
        .run(
            TAB_CLIENT,
            Step5::StoreAct {
                id: 0,
                handle: "heirloom".to_string(),
                by: 4,
                settle_ms: 400,
                timeout_ms: 20_000,
            },
        )
        .await;
    let successor_state = script
        .run(
            TAB_OWNER,
            Step5::StoreCounts {
                id: 0,
                handle: "heir".to_string(),
            },
        )
        .await;
    let stale_code = stat_str(&stale_write, "code");
    let control_moved = accepted.ok && stat_u64(&before_replacement, "tick") == Some(3);
    let successor_unmoved = stat_u64(&successor_state, "tick") == Some(0);
    let nothing_bound = stat_u64(&successor_state, "handles") == Some(0);
    ledger.record(
        WITNESSES[9],
        hosted_heir.ok
            && joined_heir.ok
            && control_moved
            && retired.ok
            && successor.ok
            && !stale_write.ok
            && stale_code.as_deref() == Some("owner-lost")
            && successor_unmoved
            && nothing_bound,
        format!(
            "A HANDLE FROM A REPLACED OWNER IS REFUSED, AND THE SUCCESSOR ADOPTS \
             NOTHING. First the CONTROL, through the same handle and while its owner \
             was alive: `bump {{by: 3}}` was accepted and moved the authority to tick \
             {:?} (asserted 3) — so what follows is about the REPLACEMENT and not \
             about a handle that never worked. Then the owner closed and a successor \
             took the same store name and the same stream label, and the replica's \
             held handle wrote again: refused with code {stale_code:?} (asserted \
             `owner-lost`). THAT CODE IS THIS RUN'S DOING: the first version asserted \
             `closed` and read `indeterminate` — \"the store did not answer before the \
             deadline\" — because a host that closed took its answer with it, and the \
             replica's only signal was a 20-second silence. §1.6's rule is that a \
             caller learns why rather than inferring it from silence, and it had been \
             applied to lease EXPIRY and not to closure. A closing owner now says \
             goodbye to every handle it holds (`owner.farewell`, awaited by \
             `close()`), spelled `owner-lost` rather than `closed` because the two are \
             different events: `closed` is the expiry notice a replica REJOINS on, and \
             rejoining here would have silently attached this caller to the \
             SUCCESSOR's different document under the handle it already had. The \
             successor's own document is at tick {:?} (asserted 0) and \
             it holds {:?} handles (asserted 0) — it did not inherit the predecessor's \
             binding, which is the half that matters: a successor that ADOPTED the \
             handle would have executed the write and every \"refused\" reading would \
             still be about something else. {} {} {}",
            stat_u64(&before_replacement, "tick"),
            stat_u64(&successor_state, "tick"),
            stat_u64(&successor_state, "handles"),
            why(&accepted),
            why(&stale_write),
            why(&successor_state)
        ),
    );

    for (tab, handle) in [
        (TAB_CLIENT, "policy"),
        (TAB_CLIENT, "heirloom"),
        (TAB_OWNER, "policy"),
        (TAB_OWNER, "heir"),
    ] {
        let _ = script
            .run(
                tab,
                Step5::StoreClose {
                    id: 0,
                    handle: handle.to_string(),
                    settle_ms: 0,
                },
            )
            .await;
    }
    for tab in [TAB_OWNER, TAB_CLIENT] {
        let _ = script
            .run(
                tab,
                Step5::Close {
                    id: 0,
                    session: session.clone(),
                },
            )
            .await;
    }
    let _ = cx.driver.close_page(PAGE_OWNER).await;
    let _ = cx.driver.close_page(PAGE_CLIENT).await;
}
