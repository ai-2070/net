//! The 60 Hz two-tab position demo — **the anchor, the page, and the
//! counter**.
//!
//! ```text
//!   cargo run --release --manifest-path \
//!     net/crates/net/examples/browser-demo/host/Cargo.toml \
//!     -- [--check] [--headless] [--seconds N] [--hz N] [--browser-path <exe>]
//! ```
//!
//! One command starts a real native anchor (`net-mesh` with
//! `webrtc`), the real bootstrap listener
//! (`net_sdk::rtc_bootstrap::serve_bootstrap`) over a locally issued
//! CA/leaf certificate, three real `BrowserBootstrapCredential`s, a
//! page server, and a Chromium with **three isolated browsing
//! contexts** — one leaf per context. Two of them are the pair: each
//! page discovers the other through a capability query, reaches a
//! direct session with `connectPeer`/`acceptPeer`, and then puts
//! position updates on a **fire-and-forget stream addressed at the
//! peer** at 60 Hz while this process serves the anchor's own
//! per-pair `forwarded_app_packets` counter to the page for display.
//! The third is the **signalling prober** below: it never joins the
//! pair and never sends a position.
//!
//! # What the demo is actually claiming
//!
//! `forwarded_app_packets` counts packets the anchor FORWARDED for
//! one ordered pair and **excludes `0x0D02` signalling** (see
//! `mesh.rs`, the `inner_sub != SUBPROTOCOL_RTC_SIGNAL` arm). That
//! exclusion is what makes "flat once direct" a claim rather than an
//! artefact: the counter cannot go flat merely because signalling
//! stopped, and it is not the number that moves when a leaf talks to
//! the anchor. So the demo shows it flat *beside* two numbers that
//! keep moving — positions arriving at the other tab, and the
//! announcement tick this anchor keeps resolving for each leaf. Flat,
//! with those two moving, is a direct path and nothing else.
//!
//! # The third context, and why the flat window needs one
//!
//! "Flat" has to be flat *while the anchor is still doing this pair's
//! other business*, and after §9 step 4 the pair itself can supply no
//! signalling at all: the direct install clears the leaf's relay
//! entry for its peer (`leaf/src/wasm.rs`'s `direct_installed` →
//! `clear_peer_relay`), so every `0x0D02` frame A signs for B rides
//! the DataChannel and never reaches this anchor to be counted. Two
//! leaves and a direct pair therefore have exactly ZERO signal
//! transit — which is why a demo with only those two could show the
//! signalling counter moving during SETUP and then display a number
//! that had already stopped moving beside a window it claimed
//! liveness over.
//!
//! So the demo runs a third leaf whose whole job is public
//! signalling. Tab C discovers tab B by a capability tag and calls
//! the public `connectPeer` on it every `PROBE_MS`; nothing in this
//! demo ever arms `acceptPeer` for C, so the attempt is never
//! answered, C↔B stays RELAYED for its whole life, and every offer C
//! signs transits this anchor as `0x0D02` and lands in
//! `note_signal_forwarded`. That is movement on the SIGNALLING path
//! inside the same window the A↔B application pair counter is
//! asserted flat in, and the two can never be each other because the
//! anchor classifies them apart. It is the mechanism the merged
//! Stage 6 runner's part 2 already proves
//! (`tests/rtc_browser/runner/src/stage6.rs`, its `signalling_moved`
//! term), in a demo you can watch.
//!
//! C is handed a TAG and never an id, like every other page here. It
//! also must NOT announce the pair's tag: `discoverPeer` takes the
//! first peer that is not itself, so a third leaf announcing
//! `demo.positions` could be picked as A's peer and the demo would
//! pair the wrong two leaves.
//!
//! # `--check` is the test, and its assertions are made HERE
//!
//! Playwright launches and drives the browser; every verdict is read
//! on the live `MeshNode` in this process and on the pages' own
//! reports. Nothing is asserted from a screenshot and nothing is
//! asserted from the HUD text — the HUD renders the same numbers this
//! process asserts on, which is why the demo and the test cannot
//! drift apart. Each verdict prints one `DEMO PASS <name>` /
//! `DEMO FAIL <name>` line; any failure exits non-zero.
//!
//! # Why a demo has a Rust host at all
//!
//! The counter lives on the anchor, and the anchor is native. A demo
//! that displayed a number the page made up would be a mock; this one
//! displays `anchor.forwarded_app_packets(src, dest)` over HTTP,
//! sampled from the same process that owns the node.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use std::time::{Duration, Instant};

use axum::extract::{Path as AxPath, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::oneshot;

use bytes::Bytes;

use net::adapter::net::behavior::capability::{CapabilityFilter, CapabilityRequirement};
use net::adapter::net::cortex::{
    RpcContext, RpcHandler, RpcHandlerError, RpcResponsePayload, RpcStatus,
};
use net::adapter::net::rtc::{RtcConfig, ENROLL_SERVICE};
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig};
use net_sdk::bootstrap_credential::{BrowserBootstrapCredential, Psk};
use net_sdk::enrollment::InviteToken;
use net_sdk::identity::Identity;
use net_sdk::rtc_bootstrap::{serve_bootstrap, BootstrapConfig, BootstrapTls};

/// The demo's transport trust domain. One value for the anchor and
/// both credentials.
const PSK: [u8; 32] = [0x6Du8; 32];

/// The capability both leaves announce and each discovers the other
/// by. No id, key or SDP is ever passed into a page.
const PEER_TAG: &str = "demo.positions";

/// The position stream's id, pinned on both sides so each leaf's
/// stream and its peer's are the same stream.
///
/// Bit 49 is the leaf's stream discriminator and bit 48 (a channel
/// publication) is deliberately clear, exactly as the Stage 5 ABI
/// witnesses pin theirs.
const POSITION_STREAM_ID: &str = "0x0002000000006011";

/// The demo's target rate. 60 Hz is the point of the demo, not a
/// preference: see `demo_positions_sustain_60_hz_over_the_direct_path`.
const DEFAULT_HZ: u32 = 60;

/// How long `--check` watches the direct phase before reading the
/// counter again.
const DEFAULT_CHECK_SECONDS: u64 = 6;

/// The floor `--check` holds the measured send rate to.
///
/// Not 60: a browser's timer resolution and one `await` per frame
/// cannot produce a mathematically exact 60.000 Hz, and a floor at
/// the target would fail on jitter alone. 57 Hz is 95 % of the
/// target — high enough that a fall back to a 30 Hz-class limit, or
/// to a background-throttled clamp, fails the row, and the MEASURED
/// value is printed either way so the number is never hidden behind
/// the verdict.
const RATE_FLOOR_HZ: f64 = 57.0;

/// The announcement interval, and the freshness bound derived from
/// it.
///
/// Each page re-announces every `ANNOUNCE_MS`, and the anchor
/// THROTTLES how often it ingests one peer's announcement — so the
/// tag it can resolve legitimately lags the page's latest by more
/// than one interval. Three of them is the bound the HUD holds its
/// "the anchor keeps resolving fresh announcements" sentence to.
///
/// It is a FRESHNESS bound on a claim on screen, not a deadline on an
/// assertion: no `--check` verdict reads it, and
/// `demo_announcements_keep_arriving_while_the_counter_is_flat` still
/// asserts a strictly higher resolved tick with no timing term in it
/// at all.
const ANNOUNCE_MS: u64 = 500;
const TICK_FRESH_MS: u64 = 3 * ANNOUNCE_MS;

/// The capability tab B announces FOR the signalling prober, and the
/// one the prober announces for itself.
///
/// Two tags, and neither of them is `PEER_TAG`. The prober has to be
/// DISCOVERABLE — tab B answers a relayed handshake only from a node
/// whose signed announcement it has verified, and that announcement
/// reaches B through the anchor's flood — while staying invisible to
/// `discoverPeer`, which takes the first peer that is not itself and
/// would otherwise pair A with the prober.
const PROBE_TARGET_TAG: &str = "demo.probe.target";
const PROBER_TAG: &str = "demo.probe.source";

/// How often the prober starts a FRESH public peer attempt.
///
/// Fresh, not repaired: `peer_offer` supersedes its predecessor (one
/// live attempt per peer), and each call signs a NEW offer envelope
/// and hands it to the relayed C↔B session — so this cadence is what
/// puts `0x0D02` frames on the anchor's forwarding path while the
/// pair's application path is idle. The superseded attempt's
/// `connectPeer` resolves as `superseded`, which the page records as
/// the expected outcome rather than as a failure.
const PROBE_MS: u64 = 1000;

/// The floor the flat window holds the prober's PUBLIC offers to.
///
/// One, and deliberately not `window_ms / PROBE_MS`: this floor's
/// only job is "the page really did drive the public API inside this
/// window". The movement claim is carried by the ANCHOR's own
/// counter, which is asserted strictly, and a floor derived from a
/// browser timer running beside two 60 Hz senders would fail the row
/// for scheduler jitter while the number it is about had moved. The
/// measured count is printed either way.
const PROBE_FLOOR: u64 = 1;

/// Every demo witness, in ledger order. CI pins these exactly.
const WITNESSES: [&str; 5] = [
    "demo_the_pair_counter_moves_while_the_anchor_carries_the_pair",
    "demo_the_pair_counter_is_flat_while_the_pair_is_direct",
    "demo_positions_sustain_60_hz_over_the_direct_path",
    "demo_announcements_keep_arriving_while_the_counter_is_flat",
    "demo_public_signalling_moves_the_anchor_signal_counter_in_the_flat_window",
];

// ===================================================================
// the one service the anchor has to serve
// ===================================================================

/// The enrollment provider, because a leaf that is not enrolled
/// cannot announce.
///
/// `connect()` enrolls, and an anchor that serves nothing leaves that
/// call unanswered until the leaf's 30 s deadline — which surfaces as
/// `rpc: the call's deadline elapsed` out of `connect` and looks like
/// a broken transport. It is not optional for the demo either: §12
/// gate 4 refuses a capability announcement from a PROVISIONAL
/// session, so without enrollment neither tab could announce and
/// neither could discover the other. A deployment's anchor serves
/// this; the demo's serves the smallest honest version of it.
struct Enrollment;

#[async_trait::async_trait]
impl RpcHandler for Enrollment {
    async fn call(&self, _ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
        Ok(RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body: admitted_outcome(),
        })
    }
}

/// `JoinOutcome`'s pinned wire form (`sdk/src/enrollment.rs`:
/// `b"NMO1"`, then `0` Admitted / `1` Rejected, then a
/// length-prefixed delegation chain). The core's promotion gate
/// parses exactly this.
fn admitted_outcome() -> Bytes {
    let mut buf = Vec::from(*b"NMO1");
    buf.push(0);
    let chain = b"browser-demo-delegation-chain";
    buf.extend_from_slice(&(chain.len() as u32).to_le_bytes());
    buf.extend_from_slice(chain);
    Bytes::from(buf)
}

// ===================================================================
// verdicts
// ===================================================================

struct Verdict {
    name: &'static str,
    pass: bool,
    detail: String,
}

#[derive(Default)]
struct Ledger(Vec<Verdict>);

impl Ledger {
    fn record(&mut self, name: &'static str, pass: bool, detail: impl Into<String>) {
        let detail = detail.into();
        println!(
            "DEMO {} {name} — {detail}",
            if pass { "PASS" } else { "FAIL" }
        );
        self.0.push(Verdict { name, pass, detail });
    }

    fn failures(&self) -> usize {
        self.0.iter().filter(|v| !v.pass).count()
    }

    fn summary(&self) {
        println!(
            "\n[demo] {} witness(es), {} failed",
            self.0.len(),
            self.failures()
        );
        for v in self.0.iter().filter(|v| !v.pass) {
            println!("[demo] FAILED {} — {}", v.name, v.detail);
        }
        // A roster that silently shrank is the failure this print
        // exists to make visible: the names are const, so a dropped
        // row shows up as a missing line rather than as a smaller
        // green count.
        let missing: Vec<&str> = WITNESSES
            .iter()
            .copied()
            .filter(|name| !self.0.iter().any(|v| v.name == *name))
            .collect();
        if !missing.is_empty() {
            println!("[demo] NOT REACHED: {}", missing.join(", "));
        }
    }
}

// ===================================================================
// what a page reports
// ===================================================================

/// One tab's self-report, posted every `reportMs`.
///
/// Three of the demo's five verdicts need a fact only the page can
/// state — the rate it achieved, whether the frames arrived, and
/// whether the prober really called the public signalling API — so
/// the pages report and this process asserts. Everything about the
/// ANCHOR is read here, on the node.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Report {
    tab: String,
    role: Option<String>,
    phase: String,
    node_id: Option<String>,
    peer_id: Option<String>,
    outcome: Option<String>,
    sent: u64,
    received: u64,
    lost: i64,
    peer_seq: i64,
    send_hz: f64,
    recv_hz: f64,
    avg_send_hz: f64,
    run_ms: f64,
    worst_send_gap_ms: f64,
    gate_wait_ms: f64,
    announce_tick: u64,
    announce_ok: u64,
    announce_failed: u64,
    direct: bool,
    reopened: bool,
    reopen_ms: f64,
    reopen_frames: f64,
    reopen_refusal: Option<String>,
    webgl: bool,
    gl_frames: u64,
    error: Option<String>,
    /// The signalling prober's own counters (tab C only).
    ///
    /// `probe_offers` counts `connectPeer` calls STARTED and
    /// `probe_dialogs` counts the ones that got past `peer_offer` —
    /// which is the boundary that matters, because an outcome
    /// carrying a dialog id is one whose offer envelope was signed
    /// and handed to the relayed session. A call that never reached
    /// that point never put a frame on the anchor's forwarding path,
    /// so it is not counted as one.
    probe_target_found: bool,
    probe_offers: u64,
    probe_dialogs: u64,
    probe_failed: u64,
    probe_last_outcome: Option<String>,
    probe_last_error: Option<String>,
}

// ===================================================================
// the host's shared state
// ===================================================================

struct Shared {
    anchor: Arc<MeshNode>,
    bootstrap_url: String,
    credentials: [String; 3],
    page_dir: PathBuf,
    browser_dist: PathBuf,
    leaf_pkg: PathBuf,
    three_dir: PathBuf,
    hz: u32,
    reports: Mutex<[Report; 3]>,
    /// The pair counter's last observed sum and when it last moved —
    /// so "flat for N seconds" is a fact and not an impression.
    flat: Mutex<Flat>,
    /// The pair counter as it stood the instant BOTH leaves had
    /// reported a node id — captured inside the `/report` handler
    /// that completed that knowledge, not by a poller.
    ///
    /// A 50 ms polling gap was enough to miss it: in one run the
    /// second page's report, its first announcement, the peer's
    /// discovery and the routed Noise handshake all fit inside one
    /// poll interval, and the baseline read 1+1 instead of 0+0 —
    /// which correctly failed the row rather than passing with a
    /// contaminated baseline. Captured here there is no window at
    /// all: a page announces only after its own POST has returned,
    /// so at the moment this runs the second leaf has not announced
    /// and neither leaf can have offered to the other.
    baseline: Mutex<Option<PairView>>,
    logs: AtomicU64,
}

struct Flat {
    sum: u64,
    since: Instant,
}

type AppState = Arc<Shared>;

impl Shared {
    fn report(&self, tab: usize) -> Report {
        self.reports.lock()[tab].clone()
    }

    /// Both leaf node ids, once both pages have reported one.
    fn pair_ids(&self) -> Option<(u64, u64)> {
        let reports = self.reports.lock();
        let a = parse_hex_id(reports[0].node_id.as_deref()?)?;
        let b = parse_hex_id(reports[1].node_id.as_deref()?)?;
        Some((a, b))
    }

    /// Take the baseline if this is the moment both ids became
    /// known, and never again.
    fn capture_baseline(&self) {
        if self.pair_ids().is_none() {
            return;
        }
        let mut slot = self.baseline.lock();
        if slot.is_none() {
            *slot = Some(self.sample());
        }
    }

    fn baseline(&self) -> Option<PairView> {
        self.baseline.lock().clone()
    }

    /// The anchor's own per-pair application-data counters, A → B and
    /// B → A.
    ///
    /// The source id is the `RoutingHeader`'s `src_id`, which is a
    /// u32 projection of the sender's node id — the same arithmetic
    /// `rtc_signalling.rs` does. The destination is the full u64.
    fn pair_counts(&self, a: u64, b: u64) -> (u64, u64) {
        let a32 = (a & 0xFFFF_FFFF) as u32;
        let b32 = (b & 0xFFFF_FFFF) as u32;
        (
            self.anchor.forwarded_app_packets(a32, b),
            self.anchor.forwarded_app_packets(b32, a),
        )
    }

    /// The admission gate, read on the anchor.
    fn gate(&self) -> Gate {
        let stats = self.anchor.rtc_stats();
        let ids = self.pair_ids();
        let (provisional_a, provisional_b) = match ids {
            Some((a, b)) => (
                self.anchor.peer_is_provisional(a),
                self.anchor.peer_is_provisional(b),
            ),
            None => (true, true),
        };
        // MUTUAL discovery, and why it is the load-bearing half.
        //
        // `query` reads this leaf's own store of signature-verified
        // announcements, so A discovering B proves A holds B's
        // announcement — and says NOTHING about whether B holds A's.
        // A leaf answers a RELAYED handshake only from a node whose
        // signed announcement it has verified (that is slice 1's
        // keys-from-discovery-only enforcement at the session seam),
        // so an offer sent before the peer has verified the offerer
        // is refused, and the offerer sees exactly "discoverable but
        // not reachable through the anchor". The offerer cannot
        // observe the peer's store; the host can, because both pages
        // report the peer they found. So the gate is: both admitted,
        // and each has found the other.
        let reports = self.reports.lock();
        let discovered_a = reports[0].peer_id.is_some();
        let discovered_b = reports[1].peer_id.is_some();
        drop(reports);
        Gate {
            ready: ids.is_some()
                && !provisional_a
                && !provisional_b
                && discovered_a
                && discovered_b,
            have_ids: ids.is_some(),
            discovered_a,
            discovered_b,
            provisional_a,
            provisional_b,
            provisional_count: self.anchor.provisional_count(),
            admission_refused_transit: stats.admission_refused_transit(),
        }
    }

    /// Sample the pair counter and update how long it has been flat.
    fn sample(&self) -> PairView {
        let stats = self.anchor.rtc_stats();
        let (a, b) = match self.pair_ids() {
            Some(ids) => ids,
            None => {
                return PairView {
                    ready: false,
                    ab: 0,
                    ba: 0,
                    flat_ms: 0,
                    signal_forwarded: stats.signal_forwarded(),
                    ingress_delivered: stats.ingress_delivered(),
                    tick_a: None,
                    tick_b: None,
                }
            }
        };
        let (ab, ba) = self.pair_counts(a, b);
        let flat_ms = {
            let mut flat = self.flat.lock();
            if flat.sum != ab + ba {
                flat.sum = ab + ba;
                flat.since = Instant::now();
            }
            flat.since.elapsed().as_millis() as u64
        };
        PairView {
            ready: true,
            ab,
            ba,
            flat_ms,
            signal_forwarded: stats.signal_forwarded(),
            ingress_delivered: stats.ingress_delivered(),
            tick_a: self.resolved_tick(0, a),
            tick_b: self.resolved_tick(1, b),
        }
    }

    /// The highest announcement tick THIS ANCHOR can still resolve to
    /// that leaf.
    ///
    /// Each leaf re-announces `demo.tick.<tab>.<n>` with `n` climbing.
    /// A tick the anchor resolves is an announcement that arrived, was
    /// signature-verified and was folded into the anchor's capability
    /// state — so this number climbing while the pair counter is flat
    /// is the anchor saying "I am still doing this pair's other
    /// business". Probed around the tick the page says it has reached
    /// rather than scanned from zero.
    fn resolved_tick(&self, tab: usize, node_id: u64) -> Option<u64> {
        let reported = self.report(tab).announce_tick;
        // Scanned from the page's own tick down to ZERO, not over a
        // narrow window around it. A six-wide window was enough to
        // fail: the anchor throttles how often it ingests one peer's
        // announcement, so the tag it holds can lag the page's latest
        // by more than that — and the row then read `None` and went
        // red while announcements were demonstrably still arriving.
        // The probe is a local fold lookup and the tick count is
        // small, so scanning the whole range costs nothing and
        // removes a tuned constant from the assertion.
        for k in (0..=reported).rev() {
            let tag = format!("demo.tick.{}.{k}", if tab == 0 { "a" } else { "b" });
            let req = CapabilityRequirement::from_filter(CapabilityFilter::new().require_tag(&tag));
            if self.anchor.find_best_node(&req) == Some(node_id) {
                return Some(k);
            }
        }
        None
    }

    /// What the signalling prober waits for, read on the anchor.
    fn probe(&self) -> Probe {
        let reports = self.reports.lock();
        let pair_direct = reports[0].direct && reports[1].direct;
        let prober = reports[2].node_id.as_deref().and_then(parse_hex_id);
        drop(reports);
        // `peer_is_provisional` on the PROBER's own id, which this
        // process learned from the prober's own report — the page is
        // told `start`, never an id.
        let admitted = prober.is_some_and(|id| !self.anchor.peer_is_provisional(id));
        Probe {
            start: pair_direct && admitted,
            pair_direct,
            admitted,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PairView {
    ready: bool,
    ab: u64,
    ba: u64,
    flat_ms: u64,
    signal_forwarded: u64,
    ingress_delivered: u64,
    tick_a: Option<u64>,
    tick_b: Option<u64>,
}

/// Whether the ANCHOR considers both leaves admitted — the gate a
/// page waits on before it offers.
///
/// `connect()` awaits its enrollment reply, so a page believes it is
/// enrolled the moment `connect` returns. The anchor promotes the
/// session on its own side, and until it has, that peer is
/// PROVISIONAL and §12's admission rule refuses its application
/// transit through the anchor. A provisional leaf still floods
/// announcements, so it is DISCOVERABLE while it is not REACHABLE:
/// exactly the state the leaf reports as "did not complete the
/// relayed Noise handshake … it is discoverable but not reachable
/// through the anchor".
///
/// So "connected" and "discovered" are not the precondition for
/// offering; "admitted on the anchor, both sides" is. The page polls
/// this, and the answer is read ON THE ANCHOR rather than inferred
/// on the page.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Gate {
    ready: bool,
    have_ids: bool,
    /// Whether each page has found the other through its own
    /// verified-announcement store.
    discovered_a: bool,
    discovered_b: bool,
    provisional_a: bool,
    provisional_b: bool,
    provisional_count: usize,
    /// The §12 refusal that this race produces: transit refused for a
    /// provisional session. Non-zero means a leaf offered too early.
    admission_refused_transit: u64,
}

/// What the signalling prober is waiting for before it starts
/// offering — two conditions, both answered HERE rather than inferred
/// on the page.
///
/// `pair_direct` is the same fact `--check` opens its flat window on,
/// so gating the prober on it is what puts the prober's signalling
/// INSIDE that window instead of before it. `admitted` is §12's rule
/// applied to the prober itself: the anchor refuses transit for a
/// PROVISIONAL session, so a prober that offered before the anchor
/// had promoted it would produce admission refusals rather than
/// forwarded signalling — and the row would fail for a race instead
/// of for its claim.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Probe {
    start: bool,
    pair_direct: bool,
    admitted: bool,
}

fn parse_hex_id(hex: &str) -> Option<u64> {
    u64::from_str_radix(hex.trim_start_matches("0x"), 16).ok()
}

// ===================================================================
// the page server
// ===================================================================

/// Bind the page server's socket, WITHOUT serving on it yet.
///
/// The order is forced and worth stating: the bootstrap listener's
/// CORS allow-list is the page's ORIGIN, so the page's port has to
/// exist before the listener is configured — and the page's state
/// carries the credentials that listener issues against. Binding
/// first breaks the cycle with one socket and one state, instead of
/// building either of them twice.
async fn bind_page() -> std::io::Result<(tokio::net::TcpListener, SocketAddr)> {
    let listener =
        tokio::net::TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)).await?;
    let addr = listener.local_addr()?;
    Ok((listener, addr))
}

fn serve_page(listener: tokio::net::TcpListener, state: AppState) {
    let router = Router::new()
        .route("/", get(index))
        // Chromium asks for this unprompted, and an unanswered
        // request is a red 404 in the console of a demo people open
        // by hand. `204 No Content` is the honest answer: there is no
        // icon, and that is not an error.
        .route("/favicon.ico", get(|| async { StatusCode::NO_CONTENT }))
        .route("/demo.js", get(demo_js))
        .route("/config", get(config))
        .route("/pair", get(pair))
        .route("/gate", get(gate))
        .route("/probe", get(probe))
        .route("/report", post(report))
        .route("/log", post(page_log))
        .route("/browser/{*path}", get(browser_asset))
        .route("/vendor/{*path}", get(vendor_asset))
        .with_state(state);
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
}

fn file_response(path: &Path, content_type: &str) -> Response {
    match std::fs::read(path) {
        Ok(bytes) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, content_type)],
            bytes,
        )
            .into_response(),
        Err(e) => (StatusCode::NOT_FOUND, format!("{}: {e}", path.display())).into_response(),
    }
}

async fn index(State(s): State<AppState>) -> Response {
    file_response(&s.page_dir.join("index.html"), "text/html; charset=utf-8")
}

async fn demo_js(State(s): State<AppState>) -> Response {
    file_response(
        &s.page_dir.join("demo.js"),
        "text/javascript; charset=utf-8",
    )
}

fn content_type_of(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("js" | "mjs" | "cjs") => "text/javascript; charset=utf-8",
        Some("wasm") => "application/wasm",
        Some("json" | "map") => "application/json",
        _ => "application/octet-stream",
    }
}

fn safe_relative(rel: &str) -> Option<&Path> {
    let path = Path::new(rel);
    path.components()
        .all(|c| matches!(c, std::path::Component::Normal(_)))
        .then_some(path)
}

/// `@net-mesh/browser`'s built bundle under one prefix, so the
/// entry's own `new URL('./net_leaf.js', import.meta.url)` resolves
/// with no import map.
///
/// `dist/` is served first and the leaf's `pkg/` second: `npm run
/// build` copies the wasm-bindgen output next to `dist/index.js`, but
/// a checkout that built the leaf and not the bundle would otherwise
/// 404 on the glue with no hint, and the demo's whole point is being
/// runnable.
async fn browser_asset(State(s): State<AppState>, AxPath(rel): AxPath<String>) -> Response {
    let Some(path) = safe_relative(&rel) else {
        return (StatusCode::BAD_REQUEST, "no traversal").into_response();
    };
    let in_dist = s.browser_dist.join(path);
    let chosen = if in_dist.exists() {
        in_dist
    } else {
        s.leaf_pkg.join(path)
    };
    file_response(&chosen, content_type_of(path))
}

/// three.js, straight out of `node_modules/three/build`.
///
/// The whole build directory rather than the one file: `three.module.js`
/// imports `./three.core.js` beside itself.
async fn vendor_asset(State(s): State<AppState>, AxPath(rel): AxPath<String>) -> Response {
    let Some(path) = safe_relative(&rel) else {
        return (StatusCode::BAD_REQUEST, "no traversal").into_response();
    };
    file_response(&s.three_dir.join(path), content_type_of(path))
}

#[derive(Debug, Deserialize)]
struct TabQuery {
    tab: String,
}

/// Which report slot a tab writes to.
///
/// Slots 0 and 1 are the PAIR and slot 2 is the signalling prober.
/// `pair_ids` reads 0 and 1 only, so the prober's own traffic can
/// never enter the pair counter the demo asserts flatness on.
fn tab_index(tab: &str) -> usize {
    match tab {
        "b" => 1,
        "c" => 2,
        _ => 0,
    }
}

async fn config(State(s): State<AppState>, Query(q): Query<TabQuery>) -> Json<Value> {
    let index = tab_index(&q.tab);
    let tab = ["a", "b", "c"][index];
    // What THIS tab announces, decided here because the trap is here:
    // the prober has to be discoverable without announcing
    // `PEER_TAG`, which `discoverPeer` would happily pair tab A with.
    let base_tags: Vec<&str> = match index {
        0 => vec![PEER_TAG],
        1 => vec![PEER_TAG, PROBE_TARGET_TAG],
        _ => vec![PROBER_TAG],
    };
    Json(json!({
        "tab": tab,
        // Tab A offers, tab B accepts, tab C only ever signals. The
        // first two are the §9 asymmetry; the third is not part of
        // the pair at all.
        "role": match index {
            0 => "offerer",
            1 => "accepter",
            _ => "prober",
        },
        "credentialB64": s.credentials[index],
        "bootstrapUrl": s.bootstrap_url,
        "peerTag": PEER_TAG,
        "baseTags": base_tags,
        "tickTag": format!("demo.tick.{tab}"),
        "streamId": POSITION_STREAM_ID,
        "hz": s.hz,
        "reportMs": 250,
        "announceMs": ANNOUNCE_MS,
        // How stale the anchor-resolved announcement tick may be
        // before the HUD stops claiming the anchor is resolving FRESH
        // announcements. See `TICK_FRESH_MS`: a bound on a sentence,
        // not on an assertion.
        "tickFreshMs": TICK_FRESH_MS,
        "discoveryMs": 30_000,
        // How long a page waits for the anchor to admit BOTH
        // leaves before it offers. Generous: it bounds a wait for
        // something the anchor is doing, and a page that gave up
        // here would offer into a refusal.
        "admissionMs": 30_000,
        // How long a page waits for a session with the peer to exist
        // at all, and the short burst of ROUTED application data it
        // then sends so the anchor's counter has moved before the
        // direct install makes it stop.
        "routedWaitMs": 20_000,
        "routedBurst": 12,
        "routedGapMs": 20,
        // The prober's tag, its offer cadence, and how long it waits
        // for the window to open. The wait is generous for the same
        // reason `admissionMs` is: it bounds a wait for something the
        // PAIR is doing, and `--check` itself allows 150 s for the
        // pair to go direct.
        "probeTargetTag": PROBE_TARGET_TAG,
        "probeMs": PROBE_MS,
        "probeWaitMs": 240_000,
    }))
}

async fn pair(State(s): State<AppState>) -> Json<PairView> {
    Json(s.sample())
}

async fn gate(State(s): State<AppState>) -> Json<Gate> {
    Json(s.gate())
}

async fn probe(State(s): State<AppState>) -> Json<Probe> {
    Json(s.probe())
}

async fn report(State(s): State<AppState>, Json(body): Json<Report>) -> StatusCode {
    let index = tab_index(&body.tab);
    s.reports.lock()[index] = body;
    // Before the response returns, so the page that just told us its
    // node id has not yet announced. See `Shared::baseline`.
    s.capture_baseline();
    StatusCode::NO_CONTENT
}

async fn page_log(State(s): State<AppState>, body: String) -> StatusCode {
    s.logs.fetch_add(1, Ordering::Relaxed);
    println!("[page] {body}");
    StatusCode::NO_CONTENT
}

// ===================================================================
// certificates
// ===================================================================

struct Ca {
    cert_pem_path: PathBuf,
    key_pem_path: PathBuf,
    /// `base64(SHA-256(SubjectPublicKeyInfo DER))` of the **leaf**
    /// key — exactly what Chromium's
    /// `--ignore-certificate-errors-spki-list` takes, one key wide.
    spki_pin: String,
}

/// Mint a CA and a `localhost` leaf for this run.
///
/// The same mechanism the browser harness uses, and for the same
/// reason: `BootstrapTls` has no self-signed variant, so a demo that
/// skipped verification would be demonstrating something no
/// deployment can rely on. Nothing is written to any platform trust
/// store — the browser is told about exactly this leaf's public key
/// — so running the demo by hand never raises a security dialog.
fn issue_localhost_certificate(dir: &Path) -> Result<Ca, String> {
    let mut ca_params =
        rcgen::CertificateParams::new(Vec::<String>::new()).map_err(|e| e.to_string())?;
    ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    ca_params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "net-mesh browser-demo CA");
    let ca_key = rcgen::KeyPair::generate().map_err(|e| e.to_string())?;
    let ca = ca_params
        .clone()
        .self_signed(&ca_key)
        .map_err(|e| e.to_string())?;
    let issuer = rcgen::Issuer::new(ca_params, ca_key);

    let mut leaf_params =
        rcgen::CertificateParams::new(vec!["localhost".to_string()]).map_err(|e| e.to_string())?;
    leaf_params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "localhost");
    let leaf_key = rcgen::KeyPair::generate().map_err(|e| e.to_string())?;
    let leaf = leaf_params
        .signed_by(&leaf_key, &issuer)
        .map_err(|e| e.to_string())?;

    let spki_pin = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        <sha2::Sha256 as sha2::Digest>::digest(rcgen::PublicKeyData::subject_public_key_info(
            &leaf_key,
        )),
    );

    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let cert_pem_path = dir.join("cert.pem");
    let key_pem_path = dir.join("key.pem");
    std::fs::write(&cert_pem_path, format!("{}{}", leaf.pem(), ca.pem()))
        .map_err(|e| e.to_string())?;
    std::fs::write(&key_pem_path, leaf_key.serialize_pem()).map_err(|e| e.to_string())?;
    Ok(Ca {
        cert_pem_path,
        key_pem_path,
        spki_pin,
    })
}

// ===================================================================
// the Playwright driver handle
// ===================================================================

struct Driver {
    _child: Child,
    stdin: tokio::sync::Mutex<ChildStdin>,
    pending: Arc<tokio::sync::Mutex<std::collections::HashMap<u64, oneshot::Sender<Value>>>>,
    next_id: AtomicU64,
}

impl Driver {
    async fn spawn(dir: &Path) -> Result<Self, String> {
        let mut child = Command::new("node")
            .current_dir(dir)
            .arg("driver.mjs")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("node: {e} (the demo needs Node >= 20 on PATH)"))?;

        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");

        let pending: Arc<
            tokio::sync::Mutex<std::collections::HashMap<u64, oneshot::Sender<Value>>>,
        > = Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));
        let reader = Arc::clone(&pending);
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(value) = serde_json::from_str::<Value>(&line) else {
                    println!("[driver] unparseable reply: {line}");
                    continue;
                };
                let id = value.get("id").and_then(Value::as_u64).unwrap_or(0);
                if id == 0 {
                    println!("[driver] {line}");
                    continue;
                }
                if let Some(tx) = reader.lock().await.remove(&id) {
                    let _ = tx.send(value);
                }
            }
        });
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                println!("[driver] {line}");
            }
        });

        Ok(Self {
            _child: child,
            stdin: tokio::sync::Mutex::new(stdin),
            pending,
            next_id: AtomicU64::new(1),
        })
    }

    async fn request(&self, op: &str, mut body: Value) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        body["id"] = json!(id);
        body["op"] = json!(op);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        {
            let mut w = self.stdin.lock().await;
            w.write_all(format!("{body}\n").as_bytes())
                .await
                .map_err(|e| format!("driver stdin: {e}"))?;
            w.flush().await.map_err(|e| format!("driver flush: {e}"))?;
        }
        let reply = match tokio::time::timeout(Duration::from_secs(180), rx).await {
            Ok(Ok(v)) => v,
            Ok(Err(_)) => return Err(format!("the driver dropped the `{op}` request")),
            Err(_) => return Err(format!("the driver did not answer `{op}` in 180 s")),
        };
        if reply.get("ok").and_then(Value::as_bool) == Some(true) {
            Ok(reply)
        } else {
            Err(reply
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("the driver refused without a reason")
                .to_string())
        }
    }
}

/// Make sure Playwright's browser registry has Chromium. A no-op once
/// it is cached; on a clean checkout it is what makes the demo's one
/// command work without a separate install step.
async fn ensure_chromium(dir: &Path) -> Result<(), String> {
    let cli = dir.join("node_modules/playwright-core/cli.js");
    if !cli.exists() {
        return Err(format!(
            "{} is missing — run `npm install` in {}",
            cli.display(),
            dir.display()
        ));
    }
    let out = Command::new("node")
        .current_dir(dir)
        .arg(&cli)
        .args(["install", "chromium"])
        .output()
        .await
        .map_err(|e| format!("node {}: {e}", cli.display()))?;
    if !out.status.success() {
        println!(
            "[demo] playwright install chromium said: {}",
            String::from_utf8_lossy(&out.stderr)
                .lines()
                .last()
                .unwrap_or("")
        );
    }
    Ok(())
}

// ===================================================================
// nodes
// ===================================================================

async fn spawn_anchor() -> Arc<MeshNode> {
    let mut cfg = MeshNodeConfig::new("127.0.0.1:0".parse().expect("addr"), PSK);
    cfg.rtc = Some(RtcConfig {
        serve_bootstrap: true,
        serve_stun: true,
        // Three times the page's own per-attempt patience, the same
        // ratio the browser harness runs: an anchor whose budget
        // expires first turns a slow ICE into a symptom that cannot
        // be told apart from an anchor that never answered.
        ice_deadline: Duration::from_secs(90),
        ..RtcConfig::new().with_bind_addr("127.0.0.1:0".parse().expect("addr"))
    });
    let node = Arc::new(
        MeshNode::new(EntityKeypair::generate(), cfg)
            .await
            .expect("MeshNode::new"),
    );
    node.start_arc();
    node
}

// ===================================================================
// assets
// ===================================================================

struct Assets {
    browser_dist: PathBuf,
    leaf_pkg: PathBuf,
    three_dir: PathBuf,
}

/// Everything the page needs, or the exact command that builds it.
///
/// A demo whose failure mode is a blank page and a 404 in a console
/// nobody opened is documentation with extra steps, so this refuses
/// to start and names the build instead.
fn locate_assets(demo_root: &Path, net_root: &Path) -> Result<Assets, String> {
    let browser_dist = net_root.join("browser-ts/dist");
    let leaf_pkg = net_root.join("leaf/pkg");
    let three_dir = demo_root.join("node_modules/three/build");
    let mut missing: Vec<String> = Vec::new();
    for p in [
        browser_dist.join("index.js"),
        leaf_pkg.join("net_leaf.js"),
        leaf_pkg.join("net_leaf_bg.wasm"),
        three_dir.join("three.module.js"),
        demo_root.join("node_modules/playwright-core/package.json"),
    ] {
        if !p.exists() {
            missing.push(p.display().to_string());
        }
    }
    if !missing.is_empty() {
        return Err(format!(
            "the demo's assets are missing: {}.\nBuild them with the documented one command:\n  \
             net/crates/net/examples/browser-demo/run.sh        (or run.ps1 on Windows)\nor by \
             hand:\n  (1) cd net/crates/net/leaf && cargo build --release --target \
             wasm32-unknown-unknown && wasm-bindgen --target web --out-dir pkg \
             target/wasm32-unknown-unknown/release/net_leaf.wasm\n  (2) cd \
             net/crates/net/browser-ts && npm install && npm run build\n  (3) cd \
             net/crates/net/examples/browser-demo && npm install",
            missing.join(", ")
        ));
    }
    Ok(Assets {
        browser_dist,
        leaf_pkg,
        three_dir,
    })
}

// ===================================================================
// main
// ===================================================================

struct Args {
    check: bool,
    headless: bool,
    seconds: u64,
    hz: u32,
    browser_path: Option<String>,
}

fn parse_args() -> Args {
    let mut args = Args {
        check: false,
        headless: false,
        seconds: DEFAULT_CHECK_SECONDS,
        hz: DEFAULT_HZ,
        browser_path: None,
    };
    let mut argv = std::env::args().skip(1);
    while let Some(arg) = argv.next() {
        match arg.as_str() {
            // `--check` implies headless: it is the test.
            "--check" => {
                args.check = true;
                args.headless = true;
            }
            "--headless" => args.headless = true,
            "--headed" => args.headless = false,
            "--seconds" => {
                args.seconds = argv
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(DEFAULT_CHECK_SECONDS)
            }
            "--hz" => {
                args.hz = argv
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(DEFAULT_HZ)
            }
            "--browser-path" => args.browser_path = argv.next(),
            other => {
                eprintln!("unknown argument {other}");
                std::process::exit(2);
            }
        }
    }
    args
}

#[tokio::main(flavor = "multi_thread", worker_threads = 8)]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();

    let args = parse_args();
    let demo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("host/..")
        .to_path_buf();
    let net_root = demo_root
        .parent()
        .and_then(Path::parent)
        .expect("examples/browser-demo/../..")
        .to_path_buf();

    let mut ledger = Ledger::default();
    match run(&args, &demo_root, &net_root, &mut ledger).await {
        Ok(()) => {}
        Err(why) => {
            println!("[demo] FAILED TO RUN: {why}");
            ledger.summary();
            std::process::exit(1);
        }
    }
    if args.check {
        ledger.summary();
        if ledger.failures() > 0 || ledger.0.len() < WITNESSES.len() {
            std::process::exit(1);
        }
    }
}

async fn run(
    args: &Args,
    demo_root: &Path,
    net_root: &Path,
    ledger: &mut Ledger,
) -> Result<(), String> {
    let assets = locate_assets(demo_root, net_root)?;
    let work = demo_root.join("work");
    let ca = issue_localhost_certificate(&work)?;

    let anchor = spawn_anchor().await;
    let rtc_addr = anchor
        .rtc_driver()
        .ok_or_else(|| "the anchor has no rtc driver".to_string())?
        .local_addr();
    println!("[demo] anchor {:#018x} on {rtc_addr}", anchor.node_id());

    // Held for the process's lifetime: dropping the guard
    // unregisters the provider, and then `connect()` would hang on
    // its enrollment call again.
    let _enrollment = anchor
        .serve_rpc(ENROLL_SERVICE, Arc::new(Enrollment))
        .map_err(|e| format!("serve {ENROLL_SERVICE}: {e}"))?;

    let (page_listener, page_addr) = bind_page().await.map_err(|e| format!("page server: {e}"))?;
    let origin = format!("http://localhost:{}", page_addr.port());

    let issuer = Identity::generate();
    let mut cfg = BootstrapConfig::new(
        "127.0.0.1:0".parse().expect("addr"),
        Psk::new(PSK),
        issuer.entity_id().clone(),
        BootstrapTls::Operator {
            cert_pem: ca.cert_pem_path.clone(),
            key_pem: ca.key_pem_path.clone(),
        },
        origin.clone(),
    );
    cfg.offers_per_ip_per_minute = 200;
    let listener = serve_bootstrap(Arc::clone(&anchor), cfg)
        .await
        .map_err(|e| format!("bootstrap listener: {e}"))?;
    let bootstrap_url = format!("https://localhost:{}", listener.local_addr().port());

    // One credential per tab, and three separate invites because
    // three leaves enroll: a single-use invite redeemed twice is the
    // `identity: … replay` refusal, and a demo that hit it would look
    // like a broken anchor.
    let root_entity = Identity::generate().entity_id().clone();
    let credential = || {
        let invite = InviteToken::mint(&root_entity, &bootstrap_url, Duration::from_secs(600));
        BrowserBootstrapCredential::mint(
            &issuer,
            invite,
            *anchor.public_key(),
            Psk::new(PSK),
            &bootstrap_url,
            Duration::from_secs(86_400),
        )
        .encode()
    };

    let state: AppState = Arc::new(Shared {
        anchor: Arc::clone(&anchor),
        bootstrap_url: bootstrap_url.clone(),
        credentials: [credential(), credential(), credential()],
        page_dir: demo_root.join("page"),
        browser_dist: assets.browser_dist,
        leaf_pkg: assets.leaf_pkg,
        three_dir: assets.three_dir,
        hz: args.hz,
        reports: Mutex::new([Report::default(), Report::default(), Report::default()]),
        flat: Mutex::new(Flat {
            sum: 0,
            since: Instant::now(),
        }),
        baseline: Mutex::new(None),
        logs: AtomicU64::new(0),
    });
    serve_page(page_listener, Arc::clone(&state));
    println!("[demo] bootstrap {bootstrap_url}");
    println!("[demo] page http://localhost:{}/", page_addr.port());

    ensure_chromium(demo_root).await?;
    let driver = Driver::spawn(demo_root).await?;
    let launched = driver
        .request(
            "launch",
            json!({
                "headless": args.headless,
                "spkiPin": ca.spki_pin,
                "executablePath": args.browser_path,
            }),
        )
        .await?;
    println!(
        "[demo] chromium {} ({})",
        launched
            .get("version")
            .and_then(Value::as_str)
            .unwrap_or("?"),
        if args.headless { "headless" } else { "headed" }
    );

    // Three contexts, opened in this order: the pair first, then the
    // signalling prober. The prober holds itself behind `/probe`
    // until the pair is direct anyway, so the order is only about
    // which windows appear first in a headed run.
    for (name, context, tab) in [
        ("a", "demo-a", "a"),
        ("b", "demo-b", "b"),
        ("c", "demo-c", "c"),
    ] {
        driver
            .request(
                "open",
                json!({
                    "name": name,
                    "context": context,
                    "url": format!("http://localhost:{}/?tab={tab}", page_addr.port()),
                }),
            )
            .await
            .map_err(|e| format!("open tab {name}: {e}"))?;
    }

    if args.check {
        check(&state, &driver, args, ledger).await?;
        let _ = driver.request("shutdown", json!({})).await;
        return Ok(());
    }

    // By hand: keep serving until Ctrl-C, printing the same numbers
    // the HUD shows so the terminal tells the story too.
    println!(
        "[demo] three tabs are open — the pair (a, b) and the signalling prober (c). Ctrl-C to \
         stop.\n[demo] watching the anchor's per-pair forwarded_app_packets — it moves while the \
         anchor carries the pair and goes FLAT once the pair is direct, while the prober keeps \
         the anchor's SIGNALLING counter moving beside it."
    );
    let watcher = tokio::spawn({
        let state = Arc::clone(&state);
        async move {
            loop {
                tokio::time::sleep(Duration::from_secs(2)).await;
                let view = state.sample();
                let a = state.report(0);
                let b = state.report(1);
                let c = state.report(2);
                if !view.ready {
                    println!("[demo] a: {} · b: {} · c: {}", a.phase, b.phase, c.phase);
                    continue;
                }
                println!(
                    "[demo] pair a→b {} b→a {} (flat {:.1} s) · a {} {:.0} Hz sent {} recv {} · \
                     b {} {:.0} Hz sent {} recv {} · c {} {} offers/{} signed · signalling {} · \
                     ticks {:?}/{:?}",
                    view.ab,
                    view.ba,
                    view.flat_ms as f64 / 1000.0,
                    a.phase,
                    a.send_hz,
                    a.sent,
                    a.received,
                    b.phase,
                    b.send_hz,
                    b.sent,
                    b.received,
                    c.phase,
                    c.probe_offers,
                    c.probe_dialogs,
                    view.signal_forwarded,
                    view.tick_a,
                    view.tick_b
                );
            }
        }
    });
    let _ = tokio::signal::ctrl_c().await;
    watcher.abort();
    let _ = driver.request("shutdown", json!({})).await;
    println!("[demo] stopped.");
    Ok(())
}

// ===================================================================
// --check: the counter's shape, asserted
// ===================================================================

async fn check(
    state: &AppState,
    driver: &Driver,
    args: &Args,
    ledger: &mut Ledger,
) -> Result<(), String> {
    // t0 — the baseline, taken the instant BOTH leaves have reported
    // a node id and before either has announced.
    //
    // It was first taken when both pages reported a PEER id, and that
    // was wrong: discovery, the offer and most of the routed burst
    // all fit inside one 250 ms report interval, so the baseline
    // already contained the traffic the first verdict is supposed to
    // measure and the row failed with 11+12 → 11+12. The page now
    // posts its node id before its first `announce`, and a peer
    // cannot offer to a leaf whose announcement it has not verified
    // — so at this instant no packet can have been forwarded for
    // this pair. The verdict asserts that the baseline IS zero, which
    // turns that ordering argument into something the run checks
    // rather than something a reader has to trust.
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut t0 = None;
    while Instant::now() < deadline {
        // The baseline itself was taken in the `/report` handler; this
        // only waits for it to exist.
        if let Some(view) = state.baseline() {
            t0 = Some(view);
            break;
        }
        let (a, b) = (state.report(0), state.report(1));
        if let Some(error) = a.error.or(b.error) {
            return Err(format!("a page failed before it had a node id: {error}"));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let t0 = t0.ok_or_else(|| {
        format!(
            "the two tabs did not both connect in 60 s (a: {}, b: {})",
            state.report(0).phase,
            state.report(1).phase
        )
    })?;
    println!(
        "[demo] t0 (both leaves connected, neither has announced): pair a→b {} b→a {}",
        t0.ab, t0.ba
    );

    // t1 — both leaves report a DIRECT session.
    let deadline = Instant::now() + Duration::from_secs(150);
    let mut direct = false;
    while Instant::now() < deadline {
        let a = state.report(0);
        let b = state.report(1);
        if a.direct && b.direct {
            direct = true;
            break;
        }
        if let Some(error) = a.error.clone().or(b.error.clone()) {
            return Err(format!("a page failed before the direct session: {error}"));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let a1 = state.report(0);
    let b1 = state.report(1);
    if !direct {
        return Err(format!(
            "the pair never reached a direct session (a: phase {} outcome {:?}, b: phase {} \
             outcome {:?})",
            a1.phase, a1.outcome, b1.phase, b1.outcome
        ));
    }
    let t1 = state.sample();
    println!(
        "[demo] t1 (direct on both leaves): pair a→b {} b→a {}; routed frames a {} b {}",
        t1.ab, t1.ba, a1.sent, b1.sent
    );
    // The prober's baseline, read at the SAME instant as `t1`: the
    // window this row measures has to be the window the flat row
    // measures, not one that merely overlaps it.
    let c1 = state.report(2);

    ledger.record(
        WITNESSES[0],
        t0.ab + t0.ba == 0 && t1.ab + t1.ba > 0,
        format!(
            "the anchor's per-pair application-data counter went {}+{} → {}+{} while it carried \
             the pair — the routed Noise handshake plus {}+{} routed position frames on the \
             peer-addressed fire-and-forget stream. The baseline was read before either leaf \
             announced, so it must be 0 and was {}. `0x0D02` signalling is EXCLUDED from this \
             counter and moved on its own, {} → {}. Neither leaf offered until the ANCHOR \
             reported both admitted, which took {:.0}/{:.0} ms; {} admission transit refusal(s) \
             recorded, which is what offering into a provisional peer produces",
            t0.ab,
            t0.ba,
            t1.ab,
            t1.ba,
            a1.sent,
            b1.sent,
            t0.ab + t0.ba,
            t0.signal_forwarded,
            t1.signal_forwarded,
            a1.gate_wait_ms,
            b1.gate_wait_ms,
            state.gate().admission_refused_transit
        ),
    );

    // The flat window. Long enough that at 60 Hz thousands of
    // position frames cross the direct path inside it.
    tokio::time::sleep(Duration::from_secs(args.seconds)).await;
    let t2 = state.sample();
    let a2 = state.report(0);
    let b2 = state.report(1);
    let c2 = state.report(2);
    let window_ms = args.seconds * 1000;
    let expected = (args.hz as u64 * args.seconds) / 2;

    let sent_a = a2.sent.saturating_sub(a1.sent);
    let sent_b = b2.sent.saturating_sub(b1.sent);
    let recv_a = a2.received.saturating_sub(a1.received);
    let recv_b = b2.received.saturating_sub(b1.received);

    let flat = t2.ab == t1.ab && t2.ba == t1.ba;
    let arriving = recv_a >= expected && recv_b >= expected;
    let still_direct = a2.direct && b2.direct;
    ledger.record(
        WITNESSES[1],
        flat && arriving && still_direct,
        format!(
            "over {window_ms} ms of direct traffic the pair counter stayed {}+{} (t1 {}+{}), \
             while {sent_a}/{sent_b} positions were sent and {recv_a}/{recv_b} arrived at the \
             other tab (floor {expected} each) and both leaves still reported direct \
             (a={} b={}); flat for {} ms",
            t2.ab, t2.ba, t1.ab, t1.ba, a2.direct, b2.direct, t2.flat_ms
        ),
    );

    // The rate. Reported whatever it is: a demo that quietly ran at
    // 30 Hz and called it 60 would be the exact failure this row
    // exists to catch.
    let rate_ok = a2.avg_send_hz >= RATE_FLOOR_HZ && b2.avg_send_hz >= RATE_FLOOR_HZ;
    ledger.record(
        WITNESSES[2],
        rate_ok,
        format!(
            "measured send rate a {:.2} Hz / b {:.2} Hz over {:.1}/{:.1} s (target {} Hz, floor \
             {RATE_FLOOR_HZ} Hz); instantaneous a {:.0} Hz b {:.0} Hz; receive rate a {:.0} Hz b \
             {:.0} Hz; worst single-frame gap a {:.1} ms b {:.1} ms; dropped a {} b {}; three.js \
             frames a {} b {}. The routed handle was refused by the incarnation fence at the \
             direct install (a {} b {}) and re-opening cost a {:.2} ms / {:.2} frame slots, b \
             {:.2} ms / {:.2} frame slots — a {:?}",
            a2.avg_send_hz,
            b2.avg_send_hz,
            a2.run_ms / 1000.0,
            b2.run_ms / 1000.0,
            args.hz,
            a2.send_hz,
            b2.send_hz,
            a2.recv_hz,
            b2.recv_hz,
            a2.worst_send_gap_ms,
            b2.worst_send_gap_ms,
            a2.lost,
            b2.lost,
            a2.gl_frames,
            b2.gl_frames,
            a2.reopened,
            b2.reopened,
            a2.reopen_ms,
            a2.reopen_frames,
            b2.reopen_ms,
            b2.reopen_frames,
            a2.reopen_refusal
        ),
    );

    // Announcements. The anchor resolving a HIGHER tick tag for each
    // leaf than it could at t1 is that leaf's announcement arriving,
    // verifying and folding — while the pair counter did not move.
    let climbed_a = matches!((t2.tick_a, t1.tick_a), (Some(now), Some(then)) if now > then);
    let climbed_b = matches!((t2.tick_b, t1.tick_b), (Some(now), Some(then)) if now > then);
    ledger.record(
        WITNESSES[3],
        climbed_a && climbed_b && flat,
        format!(
            "the announcement tick this anchor resolves went a {:?} → {:?} and b {:?} → {:?} \
             across the SAME window in which the pair counter stayed {}+{} (pages announced {} \
             and {} times, {} + {} failed); over that window the anchor's RTC ingress delivered \
             {} → {} packets",
            t1.tick_a,
            t2.tick_a,
            t1.tick_b,
            t2.tick_b,
            t2.ab,
            t2.ba,
            a2.announce_tick,
            b2.announce_tick,
            a2.announce_failed,
            b2.announce_failed,
            t1.ingress_delivered,
            t2.ingress_delivered
        ),
    );

    // Public signalling, in the SAME window, on the anchor's OTHER
    // counter.
    //
    // `flat` and `still_direct` are REUSED here rather than
    // recomputed, so this row and the flat row provably speak about
    // one window: the flatness term in this conjunction is literally
    // the flatness term in that one.
    //
    // The prober is the only leaf that can move this counter once the
    // pair is direct, and that is the reason it exists rather than an
    // implementation detail. §9 step 4 clears the leaf's relay entry
    // for its peer at the direct install, so every `0x0D02` frame A
    // signs for B rides the DataChannel and this anchor never sees
    // it; C↔B is still RELAYED, because nothing in this demo ever
    // answers C, so every offer C signs transits here and is counted.
    let probe_offers = c2.probe_offers.saturating_sub(c1.probe_offers);
    let probe_dialogs = c2.probe_dialogs.saturating_sub(c1.probe_dialogs);
    let probe_failures = c2.probe_failed.saturating_sub(c1.probe_failed);
    let signal_delta = t2.signal_forwarded.saturating_sub(t1.signal_forwarded);
    let signalling_moved = t2.signal_forwarded > t1.signal_forwarded;
    let prober_drove = probe_dialogs >= PROBE_FLOOR;
    // Four terms, and the prober's FAILURE count is deliberately not
    // one of them. It is printed, in full, with the page's own words
    // — but a relayed Noise handshake that timed out once and
    // succeeded on the next tick does not contradict the claim this
    // row makes, and a row that went red for it would be asserting
    // something it does not say. The claim is movement, its
    // attribution, and the identical window.
    ledger.record(
        WITNESSES[4],
        signalling_moved && prober_drove && flat && still_direct,
        format!(
            "the ANCHOR's own signalling counter went {} → {} (+{signal_delta}) across the SAME \
             {window_ms} ms window in which the per-pair application-data counter stayed {}+{} \
             (t1 {}+{}) and both leaves still reported direct (a={} b={}). The movement is the \
             PROBER's, and it can only be: tab C found tab B by the `{PROBE_TARGET_TAG}` tag \
             (found={}) and called the public `connectPeer` {probe_offers} time(s) inside the \
             window, {probe_dialogs} of which got past `peer_offer` and therefore signed an offer \
             envelope onto the RELAYED C↔B session (floor {PROBE_FLOOR}); {probe_failures} \
             failed, last outcome {:?}, last error {:?}, page error {:?}. A and B contribute \
             nothing to this counter any more: §9 step 4 clears the leaf's relay entry for its \
             peer at the direct install (`leaf/src/wasm.rs`'s `direct_installed` → \
             `clear_peer_relay`), so a frame either of them signs for the other rides the \
             DataChannel and never reaches this anchor to be counted. And the two counters can \
             never be each other's movement: the anchor EXCLUDES `0x0D02` from \
             `forwarded_app_packets` and counts it in `note_signal_forwarded` instead (`mesh.rs`, \
             the `inner_sub != SUBPROTOCOL_RTC_SIGNAL` arm), so signalling cannot inflate the \
             pair counter and the pair counter cannot go flat because signalling went quiet",
            t1.signal_forwarded,
            t2.signal_forwarded,
            t2.ab,
            t2.ba,
            t1.ab,
            t1.ba,
            a2.direct,
            b2.direct,
            c2.probe_target_found,
            c2.probe_last_outcome,
            c2.probe_last_error,
            c2.error
        ),
    );

    // The page's own view, read through Playwright rather than
    // through the report channel — so a report the page posted and a
    // page that is genuinely still running cannot be confused.
    for name in ["a", "b", "c"] {
        let live = driver.request("state", json!({ "name": name })).await?;
        let phase = live
            .get("state")
            .and_then(|s| s.get("phase"))
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let webgl = live
            .get("state")
            .and_then(|s| s.get("webgl"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let gl_frames = live
            .get("state")
            .and_then(|s| s.get("glFrames"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        println!(
            "[demo] tab {name} live: phase {phase}, webgl {webgl}, three.js frames {gl_frames}"
        );
    }

    Ok(())
}
