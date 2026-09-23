//! The **merged browser runner** — Stage 4b's witnesses and Stage
//! 5's, one command, one ledger, one CI job.
//!
//! ```text
//!   cargo run --release --manifest-path \
//!     net/crates/net/tests/rtc_browser/runner/Cargo.toml \
//!     -- [--engine chromium|firefox|webkit] [--browser-path <exe>] \
//!        [--no-stage5] [--inverse <defect>]
//! ```
//!
//! One command starts a real native anchor (`net-mesh` with
//! `webrtc`), the real bootstrap listener
//! (`net_sdk::rtc_bootstrap::serve_bootstrap`) over a locally issued
//! CA/leaf certificate, a real `BrowserBootstrapCredential`, a real
//! headless browser, and drives every witness through it. Every
//! observable is read **on the anchor** (`RtcStats`,
//! `peer_is_provisional`, `provisional_count`, handler invocation
//! counts, `find_best_node`, `peer_session_id`) and reported as one
//! `RTCB PASS`/`RTCB FAIL` line per witness. Any failure exits
//! non-zero.
//!
//! # Layout
//!
//! ```text
//!   runner/src/main.rs      this file: anchor, listeners, page
//!                           server, the 12 Stage 4b witnesses
//!   runner/src/browser.rs   the Playwright driver handle
//!   runner/src/stage5.rs    the 7 Stage 5 witnesses
//!   runner/src/udp_block.rs the UDP-blocked profile
//!   driver/driver.mjs       Playwright, NDJSON over stdio
//!   page/{index,app}.js     the Stage 4b page (transport only)
//!   page/leaf5.{html,js}    the Stage 5 page (@net-mesh/browser)
//! ```
//!
//! # ONE runner, and why it stayed here
//!
//! Stage 5 owns the browser matrix and had the option of a fresh
//! `tests/browser_e2e/`. It stayed in `tests/rtc_browser/` because
//! every witness in both halves is read on a live `MeshNode` **in
//! this process** — `rtc_stats()`, `peer_session_id`,
//! `provisional_count`, a real `RpcHandler`'s own call log,
//! `find_best_node`. A Node-hosted Playwright runner would have to
//! reach all of that over an IPC bridge it invented, which is a
//! second wire protocol to trust between the assertion and the fact
//! it asserts. So the Rust runner stays the witness authority, and
//! Playwright takes over the one half it is better at: launching and
//! driving browsers (see `browser.rs`). Keeping the directory also
//! keeps the CI cache keys, the artifact paths and `run.sh` working.
//!
//! # What Playwright replaced
//!
//! The bespoke `launch_chromium` — `--headless=new`, a hand-rolled
//! executable search, one tab, Chromium only. Playwright gives the
//! three things Stage 5 needs from a browser and a raw `Command`
//! cannot: **engines** (chromium | firefox | webkit from one code
//! path, each with the right profile and host-obfuscation handling),
//! **tabs** (two pages on one origin, independently addressable —
//! which is what "two tabs share one identity" means), and **tab
//! lifecycle** (CDP `Page.setWebLifecycleState`). The page↔runner
//! step protocol stayed on HTTP: it carries Net packet bytes, it is
//! engine-agnostic, and moving it to CDP would have made Firefox
//! impossible.
//!
//! # The UDP-blocked profile is a FIREWALL RULE, not natsim
//!
//! Stage 5's typed-failure witness needs a network where the
//! anchor's HTTPS bootstrap works and UDP to its `rtc_addr` does
//! not. `udp_block.rs` installs an `nft` (or `iptables`) rule pair
//! scoped to that one address and port, for the duration of that one
//! witness. natsim was rejected: using it would relocate the browser
//! and the page server into a NAT'd namespace, changing the network
//! every other witness measures, and its `--drop-direct` models a
//! dead peer-to-peer path rather than a dead UDP egress. The module
//! doc has the full argument, including the third option that was
//! considered and rejected.
//!
//! # Safari
//!
//! `--engine webkit` drives Playwright's WebKit. It is **best
//! effort, recorded**: Playwright's WebKit is not Safari, its WebRTC
//! stack differs from the shipping one, and it has no
//! mDNS-obfuscation knob. A run that cannot drive it prints why
//! rather than claiming Safari coverage.
//!
//! # TLS
//!
//! The listener serves a leaf issued by a CA this harness generates,
//! and each engine is told about it the narrowest prompt-free way
//! that engine offers:
//!
//! * **Chromium on Linux (the CI gate)** — the NSS database Chromium
//!   reads, `certutil -d sql:$HOME/.pki/nssdb -A`, removed on exit.
//!   No dialog, no OS store.
//! * **Chromium on Windows** — `--ignore-certificate-errors-spki-list`
//!   carrying `base64(SHA-256(SubjectPublicKeyInfo))` of **this run's
//!   leaf key**. That trusts exactly one public key, in one browser
//!   process, for its lifetime. It is NOT
//!   `--ignore-certificate-errors`: verification stays on, every
//!   other certificate is still verified normally, and an impostor
//!   presenting a different key still fails — which is what keeps the
//!   MITM witness meaningful. Nothing is written to the OS trust
//!   store, so no Windows security dialog is ever raised.
//! * **Firefox** — its own NSS database inside the profile
//!   (`cert9.db`), seeded by the driver before launch.
//!
//! **No platform certificate store is ever written, on any platform
//! and under any flag.** Both `certutil -addstore -user Root` and
//! `certutil -delstore -user Root` raise a modal platform security
//! dialog on the desktop of whoever runs the harness, and a test that
//! puts a security prompt in front of a person is not a test anyone
//! can run. The two legs that have no other mechanism say so and
//! refuse: WebKit-on-Windows (no SPKI-pin flag, no profile store) and
//! a Firefox-on-Windows whose `certutil` on PATH is Microsoft's
//! rather than NSS's. The store is only ever READ, by
//! `windows_root_has_harness_ca`, and only to sharpen the pin
//! control's diagnosis — `-store` prints and exits silently.
//!
//! # Sockets, and the other prompt
//!
//! Windows Firewall prompts whenever a binary binds a NON-loopback
//! address, and re-prompts after every rebuild because the path and
//! hash change. So on Windows this harness binds **loopback only** by
//! default: the page server, both bootstrap listeners and every RTC
//! socket sit on `127.0.0.1`, and the routable-interface probe
//! (`lan_ipv4`, which itself binds `0.0.0.0:0`) does not run at all.
//! `--use-routable-interface` opts in and accepts the prompt;
//! `--loopback-only` forces the Windows topology on Linux. With it
//! off, the routable-interface leg is reported UNPROVEN rather than
//! quietly counted.
//!
//! **There is no `--ignore-certificate-errors` anywhere**, and there
//! cannot be: `BootstrapTls` has no self-signed variant, so a harness
//! that skipped verification would be proving something no deployment
//! can rely on. That the SPKI pin is what makes TLS verify — and not
//! some leftover root — is asserted before the first witness: see
//! `tls_pin_control`.
//!
//! # WHAT THE BROWSER DOES AND DOES NOT DO — read this before
//! # believing a witness
//!
//! The browser owns the **transport**, end to end:
//!
//! - it POSTs the credential and a real SDP offer to the real
//!   `POST /rtc/offer`, opens the real `wss /rtc/trickle`, and
//!   exchanges ICE candidates over it;
//! - it runs the NKpsk0 handshake as **initiator** against the key
//!   the *credential* pins, using `net-mesh-wire` compiled to wasm —
//!   the production wire crate, not a spike copy;
//! - it builds and seals every outbound Net packet in wasm
//!   (`PacketBuilder::build_subprotocol`, ChaCha20-Poly1305) and
//!   opens every inbound one.
//!
//! The browser is **not** an nRPC client. That is Stage 5. The
//! *payload bytes* of the enrollment REQUEST, the membership
//! Subscribe, the capability announcement and the routing envelope
//! are constructed **on the native side with the production
//! encoders** (`EventMeta`, `encode_rpc_route`,
//! `RpcRequestPayload`, `channel::membership::encode`,
//! `CapabilityAnnouncement` via the node's own announcement bytes,
//! `RoutingHeader`) and handed to the page over plain HTTP; the page
//! Noise-encrypts them, sends them on the DataChannel, and posts the
//! decrypted reply back. So: **the browser exercises the real
//! transport, not a real nRPC client.** Every §12 gate under test
//! sits above the transport and below nRPC's client half, which is
//! exactly the surface this arrangement exercises honestly.
//!
//! # The page is served over plain `http://localhost`
//!
//! `http://localhost` is a secure context, so `RTCPeerConnection` and
//! wasm work there. Only the *page* rides that sibling server; every
//! **anchor** endpoint the page talks to is the real HTTPS listener.

mod browser;
mod org_stream;
mod stage5;
mod stage6;
mod stage7;
mod udp_block;

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::extract::{Path as AxPath, Query};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot, Mutex};

use net::adapter::net::behavior::capability::CapabilitySet;
use net::adapter::net::channel::membership::{self, MembershipMsg};
use net::adapter::net::channel::{ChannelId, ChannelName, SUBPROTOCOL_CHANNEL_MEMBERSHIP};
use net::adapter::net::cortex::{
    encode_rpc_route, EventMeta, RpcContext, RpcHandler, RpcHandlerError, RpcRequestPayload,
    RpcResponsePayload, RpcStatus, DISPATCH_RPC_REQUEST, DISPATCH_RPC_RESPONSE, EVENT_META_SIZE,
    RPC_FRAME_BODY_OFFSET, RPC_ROUTE_V1_SIZE,
};
use net::adapter::net::rtc::{
    enroll_reply_channel, RtcConfig, ENROLL_SERVICE, MAX_PROVISIONAL_BYTES, MAX_PROVISIONAL_FRAMES,
    PROVISIONAL_TTL,
};
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig, PeerAddr};
use net_sdk::bootstrap_credential::{BrowserBootstrapCredential, Psk};
use net_sdk::enrollment::InviteToken;
use net_sdk::identity::Identity;
use net_sdk::rtc_bootstrap::{serve_bootstrap, BootstrapConfig, BootstrapTls};

use browser::{Driver, Engine, LaunchSpec};
use org_stream::{StepOrgQueue, StepOrgSender};
use stage5::{Bundle, Step5Queue, Step5Sender};
use stage6::{Step6Queue, Step6Sender};

/// The transport trust domain's PSK. One value for the anchor, the
/// impostor and every credential: the MITM witness must fail on the
/// **static key**, not on a trust-domain mismatch that would refuse
/// the credential before a handshake ever ran.
const PSK: [u8; 32] = [0x4Bu8; 32];

/// `SUBPROTOCOL_CAPABILITY_ANN`, re-stated because the constant is
/// behind `behavior::broadcast`.
const SUBPROTOCOL_CAPABILITY_ANN: u16 = 0x0C00;

/// The service a provisional peer may NOT call (§12 gate 5).
const OTHER_SERVICE: &str = "app.orders.place";
/// The service an ENROLLED peer may not invoke without a grant.
const PROTECTED_SERVICE: &str = "org.protected.invoke";
/// `send_transit_probe_for_test`'s stream id, reproduced so the
/// browser's envelope is the same shape the 4a witness sends.
const TRANSIT_STREAM_ID: u64 = 0x0F00;

// ===================================================================
// Verdicts
// ===================================================================

#[derive(Debug)]
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
            "RTCB {} {name} — {detail}",
            if pass { "PASS" } else { "FAIL" }
        );
        self.0.push(Verdict { name, pass, detail });
    }

    fn failed(&self) -> usize {
        self.0.iter().filter(|v| !v.pass).count()
    }
}

// ===================================================================
// nRPC handlers registered on the anchor
// ===================================================================

/// `JoinOutcome`'s pinned wire form (`sdk/src/enrollment.rs`:
/// `b"NMO1"`, then `0` Admitted / `1` Rejected). The core's promotion
/// gate parses this; the SDK pins the same prefix.
fn admitted_outcome() -> Bytes {
    let mut buf = Vec::from(*b"NMO1");
    buf.push(0);
    let chain = b"harness-delegation-chain";
    buf.extend_from_slice(&(chain.len() as u32).to_le_bytes());
    buf.extend_from_slice(chain);
    Bytes::from(buf)
}

/// A request body beginning with this parks the call inside the
/// provider until the harness releases **that exact body** (R7a: an
/// old exchange and its successor's exchange, both in flight at
/// once).
const HOLD_PREFIX: &[u8] = b"hold:";

/// The gate parked enrollment calls wait on, keyed by the request
/// body.
///
/// It replaced a counted semaphore because the replacement
/// discriminator needs TWO enrollment calls carrying the **same call
/// id** parked at the same time, and needs to release exactly one of
/// them: `add_permits(1)` can only say "let one of the parked calls
/// go", which would make the witness depend on the semaphore's
/// wakeup order for the property it is trying to establish. The body
/// is the key because the call id deliberately is not unique here.
#[derive(Default)]
struct HoldGate {
    parked: std::sync::Mutex<Vec<Vec<u8>>>,
    released: std::sync::Mutex<Vec<Vec<u8>>>,
    wake: tokio::sync::Notify,
}

impl HoldGate {
    /// Park this call until its own body is released. The arrival is
    /// recorded first, so the harness can observe both calls sitting
    /// inside the provider before it releases either.
    async fn hold(&self, body: &[u8]) {
        Self::push(&self.parked, body);
        loop {
            // Registered BEFORE the check: `notify_waiters` only
            // wakes waiters that are already registered, so a
            // release landing between the two must not be missed.
            let wake = self.wake.notified();
            if Self::contains(&self.released, body) {
                return;
            }
            wake.await;
        }
    }

    /// Let the call carrying exactly this body finish.
    fn release(&self, body: &[u8]) {
        Self::push(&self.released, body);
        self.wake.notify_waiters();
    }

    /// Is the call carrying this body inside the provider, parked?
    fn is_parked(&self, body: &[u8]) -> bool {
        Self::contains(&self.parked, body)
    }

    fn push(log: &std::sync::Mutex<Vec<Vec<u8>>>, body: &[u8]) {
        log.lock()
            .expect("the hold gate's logs are never poisoned")
            .push(body.to_vec());
    }

    fn contains(log: &std::sync::Mutex<Vec<Vec<u8>>>, body: &[u8]) -> bool {
        log.lock()
            .expect("the hold gate's logs are never poisoned")
            .iter()
            .any(|b| b.as_slice() == body)
    }
}

/// The **real** enrollment provider the anchor serves, with the two
/// seams the browser witnesses read:
///
/// * `delivered` records `(call_id, body)` for every call that
///   actually reached this handler. That is what makes the
///   locally-addressed envelope's outcome an *observed decoded
///   fact* (R7c) instead of "a transit counter that did not move":
///   the runner generates a nonce, the browser seals it into a
///   routed envelope addressed at the anchor itself, and the
///   witness passes only if THIS handler ran with THAT payload.
///   The entry is appended **inside** the handler, before it
///   returns, so it is decoded handler delivery — not a correlated
///   successful enrollment terminal, which is a different
///   observation and is made elsewhere (witness (a) reads the
///   browser-received RESPONSE).
/// * a body starting with [`HOLD_PREFIX`] parks the call on the
///   [`HoldGate`] until the harness releases that body, which is how
///   the replacement witness holds an old enrollment exchange and
///   its successor's exchange open at the same time.
struct Enrollment {
    delivered: Arc<std::sync::Mutex<Vec<(u64, Vec<u8>)>>>,
    gate: Arc<HoldGate>,
}

impl Enrollment {
    /// Did this handler RUN with exactly this `(call_id, body)`?
    fn handler_ran(
        delivered: &std::sync::Mutex<Vec<(u64, Vec<u8>)>>,
        call: u64,
        body: &[u8],
    ) -> bool {
        delivered
            .lock()
            .expect("the delivered-call log is never poisoned")
            .iter()
            .any(|(c, b)| *c == call && b.as_slice() == body)
    }
}

#[async_trait::async_trait]
impl RpcHandler for Enrollment {
    async fn call(&self, ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
        let body = ctx.payload.body.to_vec();
        if body.starts_with(HOLD_PREFIX) {
            self.gate.hold(&body).await;
        }
        self.delivered
            .lock()
            .expect("the delivered-call log is never poisoned")
            .push((ctx.call_id, body));
        Ok(RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body: admitted_outcome(),
        })
    }
}

/// A handler whose **invocation count** is the observable.
struct Counting(Arc<AtomicUsize>);

#[async_trait::async_trait]
impl RpcHandler for Counting {
    async fn call(&self, _ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body: Bytes::from_static(b"served"),
        })
    }
}

// ===================================================================
// The step protocol the page executes
// ===================================================================

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Step {
    /// Offer → trickle → DataChannel → NKpsk0, against `base`.
    Connect {
        id: u64,
        session: String,
        base: String,
        node_id: String,
        psk: String,
        anchor_pub: String,
        anchor_node_id: String,
        credential: String,
        /// `stun:host:port`, or absent for `iceServers: []`.
        stun: Option<String>,
        /// How long to wait for the DataChannel + msg2.
        timeout_ms: u64,
        /// A handshake that must FAIL (the MITM witness).
        expect_failure: bool,
    },
    /// Build one packet in wasm and send it on the DataChannel.
    Send {
        id: u64,
        session: String,
        /// Hex bytes prepended verbatim to the built packet — the
        /// routing envelope header, when there is one.
        prefix: String,
        stream_id: String,
        subprotocol: u16,
        channel_hash: u16,
        origin_hash: String,
        reliable: bool,
        payload: String,
    },
    /// `frames` copies of one frame shape, built and sent inside
    /// the browser. The only way to cross a WHOLE-SESSION bound
    /// (§12 / S0e §2: 256 frames, 256 KiB) without one HTTP round
    /// trip per frame.
    Burst {
        id: u64,
        session: String,
        stream_id: String,
        subprotocol: u16,
        channel_hash: u16,
        origin_hash: String,
        reliable: bool,
        /// How many frames to send.
        frames: u64,
        /// Payload bytes per frame (the packet is slightly larger).
        payload_len: u64,
        /// The byte the payload is filled with.
        fill: u8,
    },
    /// Close one session's DataChannel, trickle socket and
    /// `RTCPeerConnection` from the browser end.
    Close { id: u64, session: String },
    /// Wait until the ANCHOR has torn this session's transport
    /// down, observed in the browser: the DataChannel leaves
    /// `open`, or the `RTCPeerConnection` leaves a live state. The
    /// page never closes the session itself, so this is the far
    /// end's observation that the anchor actually reclaimed the
    /// endpoint — the counter on the anchor records the reclaim
    /// DECISION, this records the transport going away.
    ExpectClosed {
        id: u64,
        session: String,
        timeout_ms: u64,
    },
    /// Wait for one inbound frame on `subprotocol` and return it.
    /// `0` is the event plane; a control subprotocol (membership,
    /// capability announcement) carries its own id. Filtering here
    /// is what keeps a membership Ack from being mistaken for an
    /// nRPC RESPONSE.
    Expect {
        id: u64,
        session: String,
        subprotocol: u16,
        timeout_ms: u64,
    },
    /// `RTCPeerConnection.getStats()` — the selected candidate pair.
    Stats { id: u64, session: String },
    /// Stop polling.
    Done { id: u64 },
}

#[derive(Debug, Clone, Deserialize, Default)]
struct StepResult {
    #[serde(default)]
    ok: bool,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    open_ms: Option<f64>,
    #[serde(default)]
    frame: Option<String>,
    #[serde(default)]
    stats: Option<serde_json::Value>,
    #[serde(default)]
    info: Option<String>,
    /// The negative `Connect` step could not even reach the
    /// boundary it is supposed to fail at — an unaccepted offer, an
    /// ICE failure, a DataChannel that never opened. A TEST ERROR,
    /// never a pass (R7b).
    #[serde(default)]
    test_error: Option<bool>,
    /// How far the negative `Connect` step got, stage by stage.
    #[serde(default)]
    stage: Option<serde_json::Value>,
    #[serde(default)]
    sent_frames: Option<u64>,
    #[serde(default)]
    sent_bytes: Option<u64>,
    /// The node id the Stage 5 leaf reported for itself (hex).
    #[serde(default)]
    node_id: Option<String>,
    /// The leaf's ORIGIN HASH (hex), which is not its node id.
    ///
    /// Needed by any witness that hands the leaf a natively encoded
    /// event payload: the anchor drops a direct peer's frame whose
    /// packet-header origin and `EventMeta` origin disagree, and the
    /// header's origin is the leaf's, so the payload's must be the
    /// leaf's too.
    #[serde(default)]
    origin_hash: Option<String>,
    /// `leader` | `follower`, when the leaf exposes leadership.
    #[serde(default)]
    role: Option<String>,
    /// §8's lock generation, a decimal STRING (it is a u64 and JSON
    /// numbers are not).
    #[serde(default)]
    generation: Option<String>,
    /// `@net-mesh/browser`'s typed error discriminator.
    #[serde(default)]
    kind: Option<String>,
    /// Verbatim the Rust `LeafError` `Display`.
    #[serde(default)]
    message: Option<String>,
    /// `UdpBlocked`'s evidence object.
    #[serde(default)]
    evidence: Option<serde_json::Value>,
    #[serde(default)]
    elapsed_ms: Option<f64>,
    /// One nRPC reply, hex.
    #[serde(default)]
    reply: Option<String>,
    /// Many nRPC replies, hex, in call order.
    #[serde(default)]
    replies: Option<Vec<String>>,
    /// `query()`'s parsed peer list.
    #[serde(default)]
    peers: Option<serde_json::Value>,
    /// How many outbound datagrams the page's drop hook elided.
    #[serde(default)]
    dropped: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct ResultBody {
    id: u64,
    #[serde(flatten)]
    result: StepResult,
}

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<StepResult>>>>;

#[derive(Clone)]
struct PageState {
    dist: PathBuf,
    page: PathBuf,
    /// `@net-mesh/browser`'s `dist`, served under `/browser/`.
    browser_dist: PathBuf,
    steps: Arc<Mutex<mpsc::Receiver<(Step, oneshot::Sender<StepResult>)>>>,
    /// One Stage 5 queue per tab, so two tabs are driven
    /// independently and their results never cross.
    steps5: Arc<HashMap<String, Step5Queue>>,
    /// One slice 2 queue per tab, on `/harness/step6`. `peer6.js`
    /// speaks its own step vocabulary — application data on a
    /// peer-addressed stream, and closing a DataChannel from the
    /// page — so a Stage 5 step delivered there would be an unknown
    /// kind, and one of its steps delivered to `leaf5.js` likewise.
    steps6: Arc<HashMap<String, Step6Queue>>,
    /// The Stage 4 org tabs' queues (`page/org.js` — its own
    /// vocabulary of the eight org verbs).
    steps_org: Arc<HashMap<String, StepOrgQueue>>,
    /// The revocation facts feed's WS endpoint registry — the
    /// runner's side of the control-plane feed.
    org_feed: Arc<org_stream::OrgControlFeed>,
    pending: Pending,
}

struct Script {
    tx: mpsc::Sender<(Step, oneshot::Sender<StepResult>)>,
    next_id: u64,
}

impl Script {
    async fn run(&mut self, mut step: Step) -> StepResult {
        let id = self.next_id;
        self.next_id += 1;
        match &mut step {
            Step::Connect { id: s, .. }
            | Step::Send { id: s, .. }
            | Step::Burst { id: s, .. }
            | Step::Close { id: s, .. }
            | Step::ExpectClosed { id: s, .. }
            | Step::Expect { id: s, .. }
            | Step::Stats { id: s, .. }
            | Step::Done { id: s } => *s = id,
        }
        let (tx, rx) = oneshot::channel();
        if self.tx.send((step, tx)).await.is_err() {
            return StepResult {
                ok: false,
                error: Some("the page server is gone".into()),
                ..Default::default()
            };
        }
        match tokio::time::timeout(Duration::from_secs(120), rx).await {
            Ok(Ok(r)) => r,
            Ok(Err(_)) => StepResult {
                ok: false,
                error: Some("the step was dropped".into()),
                ..Default::default()
            },
            Err(_) => StepResult {
                ok: false,
                error: Some("the browser did not answer this step in 120 s".into()),
                ..Default::default()
            },
        }
    }
}

/// The fixed half of a `Connect` step for the primary anchor, so the
/// witnesses below read as one line each.
struct Connector {
    base: String,
    credential: String,
    anchor_pub: String,
    psk: String,
    anchor_node: u64,
    stun: Option<String>,
}

impl Connector {
    async fn connect(&self, script: &mut Script, session: &str, node_id: u64) -> StepResult {
        script
            .run(Step::Connect {
                id: 0,
                session: session.to_string(),
                base: self.base.clone(),
                node_id: format!("{node_id:016x}"),
                psk: self.psk.clone(),
                anchor_pub: self.anchor_pub.clone(),
                anchor_node_id: format!("{:016x}", self.anchor_node),
                credential: self.credential.clone(),
                stun: self.stun.clone(),
                timeout_ms: 30_000,
                expect_failure: false,
            })
            .await
    }
}

// ===================================================================
// Page server
// ===================================================================

async fn serve_page(
    state: PageState,
) -> std::io::Result<(SocketAddr, tokio::task::JoinHandle<()>)> {
    let router = Router::new()
        .route("/", get(index))
        .route("/app.js", get(app_js))
        .route("/leaf.js", get(leaf_js))
        .route("/leaf_bg.wasm", get(leaf_wasm))
        .route("/leaf5.html", get(leaf5_html))
        .route("/leaf5.js", get(leaf5_js))
        .route("/peer6.html", get(peer6_html))
        .route("/peer6.js", get(peer6_js))
        .route("/org.html", get(org_html))
        .route("/org.js", get(org_js))
        .route("/browser/{*path}", get(browser_asset))
        .route("/harness/step", get(next_step))
        .route("/harness/step5", get(next_step5))
        .route("/harness/step6", get(next_step6))
        .route("/harness/stepOrg", get(next_step_org))
        .route("/harness/org-control", get(org_stream::org_control_socket))
        .route("/harness/result", post(step_result))
        .route("/harness/log", post(browser_log))
        .with_state(state);
    let listener =
        tokio::net::TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)).await?;
    let addr = listener.local_addr()?;
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    Ok((addr, task))
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

async fn index(State(s): State<PageState>) -> Response {
    file_response(&s.page.join("index.html"), "text/html; charset=utf-8")
}
async fn app_js(State(s): State<PageState>) -> Response {
    file_response(&s.page.join("app.js"), "text/javascript; charset=utf-8")
}
async fn leaf_js(State(s): State<PageState>) -> Response {
    file_response(&s.dist.join("leaf.js"), "text/javascript; charset=utf-8")
}
async fn leaf_wasm(State(s): State<PageState>) -> Response {
    file_response(&s.dist.join("leaf_bg.wasm"), "application/wasm")
}
async fn leaf5_html(State(s): State<PageState>) -> Response {
    file_response(&s.page.join("leaf5.html"), "text/html; charset=utf-8")
}
async fn leaf5_js(State(s): State<PageState>) -> Response {
    file_response(&s.page.join("leaf5.js"), "text/javascript; charset=utf-8")
}
async fn peer6_html(State(s): State<PageState>) -> Response {
    file_response(&s.page.join("peer6.html"), "text/html; charset=utf-8")
}
async fn peer6_js(State(s): State<PageState>) -> Response {
    file_response(&s.page.join("peer6.js"), "text/javascript; charset=utf-8")
}
async fn org_html(State(s): State<PageState>) -> Response {
    file_response(&s.page.join("org.html"), "text/html; charset=utf-8")
}
async fn org_js(State(s): State<PageState>) -> Response {
    file_response(&s.page.join("org.js"), "text/javascript; charset=utf-8")
}

/// The Stage 4 org tabs' queues. `page/org.js` speaks its own step
/// vocabulary (the eight org verbs), so a Stage 5 step delivered
/// there would be an unknown kind and one of its steps delivered to
/// `leaf5.js` likewise.
async fn next_step_org(State(s): State<PageState>, Query(q): Query<TabQuery>) -> Response {
    let idle = || Json(serde_json::json!({ "kind": "idle", "id": 0, "millis": 50 }));
    let Some(queue) = s.steps_org.get(&q.tab) else {
        return idle().into_response();
    };
    let mut rx = queue.lock().await;
    match tokio::time::timeout(Duration::from_secs(25), rx.recv()).await {
        Ok(Some((step, reply))) => {
            let id = step.get("id").and_then(serde_json::Value::as_u64).unwrap_or(0);
            s.pending.lock().await.insert(id, reply);
            Json(step).into_response()
        }
        _ => idle().into_response(),
    }
}

/// Serve `@net-mesh/browser`'s built bundle under one prefix, so the
/// entry's own `new URL('./net_leaf.js', import.meta.url)` resolves
/// without an import map.
async fn browser_asset(State(s): State<PageState>, AxPath(rel): AxPath<String>) -> Response {
    let path = Path::new(&rel);
    if path
        .components()
        .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        return (StatusCode::BAD_REQUEST, "no traversal").into_response();
    }
    let content_type = match path.extension().and_then(|e| e.to_str()) {
        Some("js" | "mjs" | "cjs") => "text/javascript; charset=utf-8",
        Some("wasm") => "application/wasm",
        Some("json" | "map") => "application/json",
        _ => "application/octet-stream",
    };
    file_response(&s.browser_dist.join(path), content_type)
}

async fn next_step(State(s): State<PageState>) -> Response {
    let mut rx = s.steps.lock().await;
    match tokio::time::timeout(Duration::from_secs(25), rx.recv()).await {
        Ok(Some((step, reply))) => {
            let id = match &step {
                Step::Connect { id, .. }
                | Step::Send { id, .. }
                | Step::Burst { id, .. }
                | Step::Close { id, .. }
                | Step::ExpectClosed { id, .. }
                | Step::Expect { id, .. }
                | Step::Stats { id, .. }
                | Step::Done { id } => *id,
            };
            s.pending.lock().await.insert(id, reply);
            Json(step).into_response()
        }
        // No step ready, or the script finished: tell the page to
        // poll again rather than leaving it hanging.
        _ => Json(serde_json::json!({ "kind": "idle", "id": 0, "millis": 50 })).into_response(),
    }
}

/// Which Stage 5 tab is asking.
#[derive(Debug, Deserialize)]
struct TabQuery {
    #[serde(default)]
    tab: String,
}

async fn next_step5(State(s): State<PageState>, Query(q): Query<TabQuery>) -> Response {
    let idle = || Json(serde_json::json!({ "kind": "idle", "id": 0, "millis": 50 }));
    let Some(queue) = s.steps5.get(&q.tab) else {
        return idle().into_response();
    };
    let mut rx = queue.lock().await;
    match tokio::time::timeout(Duration::from_secs(25), rx.recv()).await {
        Ok(Some((step, reply))) => {
            s.pending.lock().await.insert(step.id(), reply);
            Json(step).into_response()
        }
        _ => idle().into_response(),
    }
}

/// The slice 2 queues. Steps are `serde_json::Value`, because the
/// §10 witness's vocabulary belongs to `stage6.rs` and `peer6.js`
/// rather than to the Stage 5 step enum.
async fn next_step6(State(s): State<PageState>, Query(q): Query<TabQuery>) -> Response {
    let idle = || Json(serde_json::json!({ "kind": "idle", "id": 0, "millis": 50 }));
    let Some(queue) = s.steps6.get(&q.tab) else {
        return idle().into_response();
    };
    let mut rx = queue.lock().await;
    match tokio::time::timeout(Duration::from_secs(25), rx.recv()).await {
        Ok(Some((step, reply))) => {
            let id = step
                .get("id")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            s.pending.lock().await.insert(id, reply);
            Json(step).into_response()
        }
        _ => idle().into_response(),
    }
}

async fn step_result(State(s): State<PageState>, Json(body): Json<ResultBody>) -> StatusCode {
    if let Some(tx) = s.pending.lock().await.remove(&body.id) {
        let _ = tx.send(body.result);
    }
    StatusCode::OK
}

async fn browser_log(body: String) -> StatusCode {
    for line in body.lines() {
        println!("[browser] {line}");
    }
    StatusCode::OK
}

// ===================================================================
// Certificates
// ===================================================================

struct Ca {
    ca_pem: String,
    cert_pem_path: PathBuf,
    key_pem_path: PathBuf,
    ca_pem_path: PathBuf,
    /// `base64(SHA-256(SubjectPublicKeyInfo DER))` of the **leaf**
    /// key — exactly the value Chromium's
    /// `--ignore-certificate-errors-spki-list` takes, and exactly one
    /// key wide.
    spki_pin: String,
}

fn issue_localhost_certificate(dir: &Path) -> Result<Ca, String> {
    let mut ca_params =
        rcgen::CertificateParams::new(Vec::<String>::new()).map_err(|e| e.to_string())?;
    ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    ca_params
        .distinguished_name
        .push(rcgen::DnType::CommonName, CA_COMMON_NAME);
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

    // The pin is over the leaf's SubjectPublicKeyInfo, which is what
    // Chromium hashes when it compares against the list — not over
    // the certificate, and not over the CA. A new run mints a new
    // key, so a pin never outlives the process that minted it.
    let spki_pin = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        <sha2::Sha256 as sha2::Digest>::digest(rcgen::PublicKeyData::subject_public_key_info(
            &leaf_key,
        )),
    );

    let ca_pem = ca.pem();
    let cert_pem_path = dir.join("cert.pem");
    let key_pem_path = dir.join("key.pem");
    let ca_pem_path = dir.join("ca.pem");
    std::fs::write(&cert_pem_path, format!("{}{}", leaf.pem(), ca_pem))
        .map_err(|e| e.to_string())?;
    std::fs::write(&key_pem_path, leaf_key.serialize_pem()).map_err(|e| e.to_string())?;
    std::fs::write(&ca_pem_path, &ca_pem).map_err(|e| e.to_string())?;
    Ok(Ca {
        ca_pem,
        cert_pem_path,
        key_pem_path,
        ca_pem_path,
        spki_pin,
    })
}

const CA_COMMON_NAME: &str = "net-mesh stage-4b harness CA";
const NSS_NICKNAME: &str = "net-mesh-stage4b-harness-ca";

/// Whether this process wrote the NSS database Chromium reads on
/// Linux, and so owes a removal on exit.
///
/// There is no Windows counterpart on purpose: **nothing in this
/// harness writes a platform certificate store.** Both `certutil
/// -addstore -user Root` and `certutil -delstore -user Root` raise a
/// modal platform security dialog on the desktop of whoever runs the
/// harness, and a test that puts a security prompt in front of a
/// person is not a test anyone can run. The Windows legs use a
/// mechanism that needs no store: Chromium the one-key SPKI pin,
/// Firefox the NSS database inside its own launch profile.
static NSS_STORE_OWED: AtomicBool = AtomicBool::new(false);

/// How this run makes the browser trust the harness listener.
struct Trust {
    /// What was actually done, printed and carried into the ledger's
    /// preamble.
    how: String,
    /// The leaf SPKI pin, when the pin — rather than a trust store —
    /// is the mechanism. `Some` means `tls_pin_control` must run.
    pin: Option<String>,
}

/// Make **this engine on this platform** trust the harness leaf, the
/// narrowest way that raises no dialog on the user's desktop.
///
/// Nothing here writes a platform trust store, on any platform and
/// under any flag. Nothing about TLS is weakened by that:
/// verification stays on, and only this run's freshly minted leaf key
/// is exempt from the unknown-issuer rejection.
fn establish_trust(ca: &Ca, engine: Engine) -> Result<Trust, String> {
    if !cfg!(windows) {
        // `~/.pki/nssdb` is CHROMIUM's shared NSS store. Firefox does
        // not read it — it reads `cert9.db` inside its own profile,
        // which the driver seeds before launch. Touching Chromium's
        // database for a Firefox run bought nothing and cost the CI
        // job 35 minutes: `certutil -N` against the database the
        // Chromium leg had already created prompted for its password
        // on inherited stdin and blocked until the job timed out.
        return match engine {
            Engine::Chromium | Engine::Webkit => {
                install_nss_store(ca).map(|how| Trust { how, pin: None })
            }
            Engine::Firefox => Ok(Trust {
                how: "Firefox's own NSS database inside the launch profile (cert9.db),                       seeded by the driver; Chromium's ~/.pki/nssdb is not touched"
                    .into(),
                pin: None,
            }),
        };
    }
    match engine {
        Engine::Chromium => Ok(Trust {
            how: format!(
                "--ignore-certificate-errors-spki-list={} — base64(SHA-256(SPKI)) of THIS \
                 run's leaf key, one key wide, one browser process wide. Certificate \
                 verification stays ON (this is not --ignore-certificate-errors) and the OS \
                 trust store is neither read nor written, so no platform security dialog \
                 is raised",
                ca.spki_pin
            ),
            pin: Some(ca.spki_pin.clone()),
        }),
        // Firefox never reads the platform store unless
        // `security.enterprise_roots.enabled` is set, and it does read
        // an NSS database inside its own profile. The driver seeds
        // that before launch and reports which mechanism it got; a
        // run that fell back to `enterprise_roots` is refused at the
        // launch site, because the platform store is empty of this
        // CA and this harness will not put one there.
        Engine::Firefox => Ok(Trust {
            how: "Firefox's own NSS database inside the launch profile (cert9.db), seeded by \
                  the driver; the OS trust store is untouched"
                .into(),
            pin: None,
        }),
        // WebKit on Windows has neither an SPKI-pin flag nor a
        // profile-local trust store: it uses the platform store. This
        // leg is refused rather than run untrusted, and refused
        // rather than prompted at.
        Engine::Webkit => Err(format!(
            "WebKit on Windows trusts only the platform certificate store, and this harness \
             never writes one: `certutil -addstore -user Root` raises an OS security dialog \
             on the desktop of whoever runs the harness, and so does its removal. There is \
             no SPKI-pin flag for WebKit and no profile-local trust store to seed. Run the \
             WebKit leg on Linux, where the NSS database is the prompt-free mechanism. The \
             pin in force for Chromium is {} and is deliberately NOT used to weaken \
             verification for this engine.",
            ca.spki_pin
        )),
    }
}

/// Is there a leftover harness root in the current user's store?
///
/// **Read-only and silent.** `certutil -store -user Root <CN>` prints
/// and exits; it raises no dialog, unlike `-addstore`/`-delstore`.
/// Used only to sharpen the pin control's diagnosis when something on
/// the machine already trusts the harness CA — a leftover from an
/// older harness that did write the store.
#[cfg(windows)]
fn windows_root_has_harness_ca() -> bool {
    Command::new("certutil")
        .stdin(Stdio::null())
        .args(["-store", "-user", "Root", CA_COMMON_NAME])
        .output()
        .is_ok_and(|o| o.status.success())
}

#[cfg(not(windows))]
fn windows_root_has_harness_ca() -> bool {
    false
}

/// The CI path: the NSS database Chromium reads on Linux. No dialog,
/// no platform store, removed on exit.
fn install_nss_store(ca: &Ca) -> Result<String, String> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
    let db = format!("{home}/.pki/nssdb");
    let _ = std::fs::create_dir_all(&db);
    // **Only an ABSENT database is initialised.** `certutil -N`
    // against an existing one asks for its current password on
    // stdin, and a CI run's stdin is inherited and never answered:
    // the second engine's install hung for 35 minutes and the job
    // died on its own timeout with no output after "building the
    // wasm leaf". The comment here used to claim an existing
    // database was left alone; the code did not.
    if !std::path::Path::new(&db).join("cert9.db").exists() {
        let _ = Command::new("certutil")
            .stdin(Stdio::null())
            .args(["-N", "--empty-password", "-d", &format!("sql:{db}")])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let out = Command::new("certutil")
        .stdin(Stdio::null())
        .args([
            "-d",
            &format!("sql:{db}"),
            "-A",
            "-t",
            "C,,",
            "-n",
            NSS_NICKNAME,
            "-i",
        ])
        .arg(&ca.ca_pem_path)
        .output();
    match out {
        Ok(o) if o.status.success() => {
            NSS_STORE_OWED.store(true, Ordering::SeqCst);
            Ok(format!(
                "certutil -d sql:{db} -A -t C,, (Chromium's NSS store; removed on exit)"
            ))
        }
        Ok(o) => Err(format!(
            "certutil -A exited {:?}: {}",
            o.status.code(),
            String::from_utf8_lossy(&o.stderr)
        )),
        Err(e) => Err(format!("certutil (libnss3-tools) not runnable: {e}")),
    }
}

/// Remove the NSS entry this process created, and nothing else.
///
/// There is deliberately no platform-store branch: this harness never
/// adds one, so it has nothing to delete — and running `certutil
/// -delstore -user Root` anyway would raise the platform's
/// delete-a-root dialog for a certificate it never added, which is
/// one of the two prompts this policy exists to eliminate.
fn uninstall_nss_store_if_owed() {
    if !NSS_STORE_OWED.swap(false, Ordering::SeqCst) {
        return;
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
    let _ = Command::new("certutil")
        .stdin(Stdio::null())
        .args([
            "-d",
            &format!("sql:{home}/.pki/nssdb"),
            "-D",
            "-n",
            NSS_NICKNAME,
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Prove the SEEDED PROFILE is what makes a Firefox run's TLS
/// verify — the same control as [`tls_pin_control`], for the engine
/// whose mechanism is a certificate database rather than a flag.
async fn tls_trust_control(
    driver: &Driver,
    engine: Engine,
    url: &str,
    executable: Option<&str>,
    ca: &Ca,
) -> Result<String, String> {
    let ca_path = ca.ca_pem_path.to_string_lossy().to_string();
    let unseeded = driver
        .tls_probe(engine, url, None, executable, None, None)
        .await?;
    let seeded = driver
        .tls_probe(
            engine,
            url,
            None,
            executable,
            Some(&ca_path),
            Some(NSS_NICKNAME),
        )
        .await?;
    if unseeded.verified {
        return Err(format!(
            "the Firefox trust control FAILED: a profile with NO seeded CA already trusts \
             {url} ({}). Something on this machine trusts the harness CA, so no TLS \
             observation in this run can be attributed to the seeding.",
            unseeded.detail
        ));
    }
    if !seeded.verified {
        return Err(format!(
            "the Firefox trust control FAILED the other way: even with this run's CA seeded \
             into the profile's cert9.db, Firefox could not load {url} ({}). The seeding is \
             not in effect — check that the `certutil` on PATH is NSS's.",
            seeded.detail
        ));
    }
    Ok(format!(
        "an unseeded Firefox profile is refused ({}) and a seeded one verifies ({}) — the \
         profile's own cert9.db is what makes this run's TLS verify",
        unseeded.detail, seeded.detail
    ))
}

/// Prove the SPKI pin is what makes this run's TLS verify.
///
/// Two throwaway browsers, same engine, same URL, one difference:
///
/// * **without** `--ignore-certificate-errors-spki-list` the
///   navigation MUST be refused — the leaf is issued by a CA nothing
///   trusts;
/// * **with** it the same navigation MUST return an HTTP status.
///
/// Both halves are needed. The second alone would pass on a machine
/// with a leftover harness root in its store — and then every TLS
/// observation in the run would be a statement about that machine
/// instead of about the pin. The first alone would pass if the flag
/// were misspelled or ignored, because then nothing would work at
/// all. Together they pin the mechanism.
///
/// It is a NAVIGATION and not a `fetch` for a measured reason: a
/// cross-origin `fetch(url, {mode: 'no-cors'})` is rejected by
/// Chromium's Opaque Response Blocking whatever TLS did, so a
/// fetch-based control reported "refused" for both halves and
/// discriminated nothing.
async fn tls_pin_control(
    driver: &Driver,
    engine: Engine,
    pin: &str,
    url: &str,
    executable: Option<&str>,
) -> Result<String, String> {
    let unpinned = driver
        .tls_probe(engine, url, None, executable, None, None)
        .await?;
    let pinned = driver
        .tls_probe(engine, url, Some(pin), executable, None, None)
        .await?;
    if unpinned.verified {
        // Read the store — never write it — so the diagnosis can say
        // whether the leftover really is there.
        let leftover = if windows_root_has_harness_ca() {
            format!(
                "and `certutil -store -user Root \"{CA_COMMON_NAME}\"` FINDS one, left by \
                 an older harness that wrote the platform store. Remove it — `certutil \
                 -delstore -user Root \"{CA_COMMON_NAME}\"`, which will raise the \
                 platform's delete-a-root dialog once — and re-run"
            )
        } else {
            format!(
                "though `certutil -store -user Root \"{CA_COMMON_NAME}\"` finds no harness \
                 root, so whatever trusts it is something else on this machine"
            )
        };
        return Err(format!(
            "the SPKI-pin control FAILED: a {} with NO pin already trusts {url} ({}). \
             Something on this machine trusts the harness CA {leftover}: until then no TLS \
             observation in this run can be attributed to the pin.",
            engine.as_str(),
            unpinned.detail
        ));
    }
    if !pinned.verified {
        return Err(format!(
            "the SPKI-pin control FAILED the other way: even WITH \
             --ignore-certificate-errors-spki-list={pin} a {} could not load {url} ({}). \
             The pin is not in effect — a wrong digest, a wrong encoding, or a flag this \
             build ignores — and the harness will not fall back to \
             --ignore-certificate-errors.",
            engine.as_str(),
            pinned.detail
        ));
    }
    Ok(format!(
        "with the pin ABSENT a {} refused {url} ({}), and with the pin PRESENT the same \
         navigation succeeded ({}) — so this run's TLS verifies because of the one pinned \
         leaf key, not because of anything in the machine's trust store, and a certificate \
         carrying any other key is still rejected",
        engine.as_str(),
        unpinned.detail,
        pinned.detail
    ))
}

// ===================================================================
// Build the wasm leaf
// ===================================================================

fn build_leaf(root: &Path) -> Result<PathBuf, String> {
    let leaf = root.join("leaf");
    let dist = leaf.join("dist");
    println!("[harness] building the wasm leaf against net-mesh-wire");
    let status = Command::new("cargo")
        .current_dir(&leaf)
        .args(["build", "--release", "--target", "wasm32-unknown-unknown"])
        .status()
        .map_err(|e| format!("cargo: {e}"))?;
    if !status.success() {
        return Err("the wasm leaf did not build".into());
    }
    let wasm = leaf.join("target/wasm32-unknown-unknown/release/rtc_browser_leaf.wasm");
    let status = Command::new("wasm-bindgen")
        .current_dir(&leaf)
        .args(["--target", "web", "--out-dir"])
        .arg(&dist)
        .args(["--out-name", "leaf"])
        .arg(&wasm)
        .status()
        .map_err(|e| {
            format!("wasm-bindgen: {e} (install with `cargo install wasm-bindgen-cli --version 0.2.128`)")
        })?;
    if !status.success() {
        return Err("wasm-bindgen failed (CLI/crate version mismatch?)".into());
    }
    Ok(dist)
}

// ===================================================================
// Frame payloads — built with the PRODUCTION encoders
// ===================================================================

/// `MeshNode::publish_stream_id`, which is `pub(super)`. The formula
/// is the wire contract for a channel-keyed publisher stream; a drift
/// here would show up as the anchor never dispatching the frame, so
/// the witness fails loudly rather than silently.
fn publish_stream_id(channel: &ChannelId) -> u64 {
    0x0001_0000_0000_0000 | channel.hash()
}

struct RpcFrame {
    stream_id: u64,
    channel_hash_u16: u16,
    payload: Vec<u8>,
}

/// The exact bytes `MeshNode::publish_rpc_request_unsubscribed`
/// publishes: `EventMeta ‖ RpcRouteV1 ‖ RpcRequestPayload`.
fn rpc_request_frame(service: &str, origin_hash: u64, call_id: u64, body: &[u8]) -> RpcFrame {
    let name = ChannelName::new(&format!("{service}.requests")).expect("a valid channel name");
    let channel = ChannelId::new(name);
    let hash = channel.hash();
    let req = RpcRequestPayload {
        service: service.to_string(),
        deadline_ns: 0,
        flags: 0,
        headers: Vec::new(),
        body: Bytes::copy_from_slice(body),
    };
    let meta = EventMeta::new(DISPATCH_RPC_REQUEST, 0, origin_hash, call_id, 0);
    let mut buf = Vec::with_capacity(EVENT_META_SIZE + RPC_ROUTE_V1_SIZE + req.encoded_len());
    buf.extend_from_slice(&meta.to_bytes());
    encode_rpc_route(&mut buf, hash);
    req.encode_into(&mut buf);
    RpcFrame {
        stream_id: publish_stream_id(&channel),
        channel_hash_u16: hash as u16,
        payload: buf,
    }
}

/// A membership Subscribe, through `channel::membership::encode`.
fn subscribe_payload(channel: &str, nonce: u64) -> Vec<u8> {
    membership::encode(&MembershipMsg::Subscribe {
        channel: ChannelName::new(channel).expect("a valid channel name"),
        nonce,
        token: None,
        queue_group: None,
    })
}

/// A routing envelope header addressed at `dest_node_id`, exactly as
/// `send_transit_probe_for_test` builds it.
fn routing_prefix(dest_node_id: u64, src_node_id: u64) -> Vec<u8> {
    net_wire::route_codec::RoutingHeader::new(dest_node_id, src_node_id as u32, 8)
        .to_bytes()
        .to_vec()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .filter_map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

// ===================================================================
// Nodes
// ===================================================================

async fn spawn_node(rtc: Option<RtcConfig>) -> Arc<MeshNode> {
    spawn_node_identity(rtc).await.0
}

/// [`spawn_node`], keeping the signing keypair: the org stage's
/// native callers sign admission proofs with it (`OrgProofIntent`).
async fn spawn_node_identity(rtc: Option<RtcConfig>) -> (Arc<MeshNode>, Arc<EntityKeypair>) {
    let mut cfg = MeshNodeConfig::new("127.0.0.1:0".parse().expect("addr"), PSK);
    cfg.rtc = rtc;
    let key = Arc::new(EntityKeypair::generate());
    let node = Arc::new(
        MeshNode::new((*key).clone(), cfg)
            .await
            .expect("MeshNode::new"),
    );
    node.start();
    (node, key)
}

fn anchor_rtc(bind: SocketAddr) -> RtcConfig {
    RtcConfig {
        serve_bootstrap: true,
        serve_stun: true,
        // **This must be strictly larger than the page's per-step
        // budget (30 s).** R4-A made the attempt deadline ONE
        // absolute budget covering DataChannel open + Noise + the
        // fenced install, and it starts when the offer is accepted —
        // the same instant the page starts its own wait. With both
        // set to 30 s, a slow ICE on a loaded runner eats the
        // anchor's Noise budget, the completion owner correctly
        // retires the attempt, and the page reports
        // `timeout: noise msg2` — a symptom that cannot be told
        // apart from an anchor that never answered. That is the CI
        // failure in run 34737303... at head f44088908 (2 of 9
        // witnesses).
        //
        // 90 s keeps the anchor's budget three times the page's, so
        // a `noise msg2` timeout means what it says.
        ice_deadline: Duration::from_secs(90),
        ..RtcConfig::new().with_bind_addr(bind)
    }
}

/// The host's routable IPv4, discovered without sending anything: a
/// connected UDP socket only installs a route.
fn lan_ipv4() -> Option<Ipv4Addr> {
    let sock = UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("8.8.8.8:80").ok()?;
    match sock.local_addr().ok()?.ip() {
        IpAddr::V4(v4) if !v4.is_loopback() && !v4.is_unspecified() => Some(v4),
        _ => None,
    }
}

// ===================================================================
// main
// ===================================================================

#[tokio::main(flavor = "multi_thread", worker_threads = 8)]
async fn main() {
    // The anchor's own account of what it discarded. Every
    // "dropped", "refused" and "malformed" decision in the core is a
    // `tracing` event, and without a subscriber a witness whose frame
    // the anchor threw away can only report the absence of an effect.
    // WARN by default — quiet on a green run — and `RUST_LOG`
    // overrides it for a diagnosis session
    // (`RUST_LOG=net_mesh=trace`).
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();
    let mut browser_path = None;
    let mut engine = Engine::Chromium;
    let mut engine_arg_bad: Option<String> = None;
    let mut stage5 = true;
    let mut stage7 = false;
    // Run ONLY the Stage 4 org witness stage (37 witnesses),
    // skipping the 4b/5/6/7 halves (each still named `RTCB
    // EXCLUDED`, so the ledger is never silently short).
    let mut org_only = false;
    let mut inverse = String::new();
    // **Off by default on Windows.** Binding the host's routable
    // IPv4 — which the anchor's and the impostor's RTC sockets do
    // when a routable interface is used — makes the Windows Firewall
    // raise its "allow this app to accept connections" prompt, and
    // it re-raises it after every rebuild because the binary's path
    // and hash change. So the default topology here is loopback-only
    // and the routable-interface leg is reported as UNPROVEN rather
    // than bought with a security prompt on someone's desktop. On
    // Linux, where no such prompt exists, it stays on.
    let mut routable = !cfg!(windows);
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            // `--chrome` is the Stage 4b spelling, kept working.
            "--chrome" | "--browser-path" => browser_path = args.next(),
            "--engine" => {
                let raw = args.next().unwrap_or_default();
                match Engine::parse(&raw) {
                    Some(e) => engine = e,
                    None => engine_arg_bad = Some(raw),
                }
            }
            // Run ONLY the Stage 4b half. Every excluded Stage 5
            // witness is still named on stdout as `RTCB EXCLUDED`,
            // so the ledger is never quietly short and the CI floor
            // (which counts `RTCB PASS`) still fails.
            "--no-stage5" => stage5 = false,
            // Opt IN to the Stage 7 store witnesses. Off by default
            // so a bare local run keeps the 47-witness surface, and
            // ON in CI's Chromium leg, where all five are pinned at
            // floor 55. The Firefox leg leaves them off: they have
            // never been run on that engine, and a flag whose
            // witnesses are unproven there does not belong in its
            // gate. They were behind this flag for a different
            // reason once — the browser ↔ browser join was not
            // answered — and that reason is gone.
            "--stage7" => stage7 = true,
            // Run ONLY the Stage 4 org witness stage.
            "--org-only" => org_only = true,
            // Opt in to the routable-interface topology on Windows,
            // accepting the Windows Firewall prompt the non-loopback
            // binds raise. Needed for the mDNS
            // routable-interface verdict, and for the Firefox MITM
            // leg, which forms no ICE pair between an obfuscated
            // `.local` host candidate and a `127.0.0.1` remote.
            "--use-routable-interface" => routable = true,
            // …and the way to force loopback-only on Linux, so the
            // Windows topology can be reproduced on a CI host.
            "--loopback-only" => routable = false,
            // Deliberate defect injection, so a witness can be shown
            // to be capable of failing:
            //   mitm-pins-the-live-key  — the browser pins the
            //     IMPOSTOR's own static key instead of the
            //     credential's, so the MITM handshake succeeds and
            //     the MITM witness must FAIL.
            //   mitm-offer-refused      — the MITM step presents a
            //     corrupted credential, so the offer is REFUSED. The
            //     pinned-key boundary is never reached, and the
            //     witness must FAIL as a TEST ERROR — this is the
            //     shape the old `catch { ok: true }` passed (R7b).
            //   protected-denial-uncorrelated — the protected
            //     REQUEST is sent under a different call id than the
            //     verdict correlates against: the denial still
            //     arrives and the handler still never runs, so only
            //     the CALL CORRELATION fails (R7d).
            //   mdns-off-from-the-start — the measurement launch
            //     disables the engine's mDNS obfuscation. Pairs
            //     still form, so the diagnostic sweep still passes,
            //     but `mdns_on_pair_formed` must FAIL.
            //   skip-enrollment-request — the enrollment REQUEST is
            //     never sent, so nothing may be promoted and the
            //     enrollment witness must FAIL.
            "--inverse" => inverse = args.next().unwrap_or_default(),
            _ => {}
        }
    }
    if let Some(raw) = engine_arg_bad {
        println!("RTCB FAIL harness — unknown --engine {raw:?}; use chromium, firefox or webkit");
        std::process::exit(2);
    }
    if !inverse.is_empty() {
        println!("[harness] INVERSE MODE: {inverse}");
    }
    println!(
        "[harness] engine: {} ({})",
        engine.as_str(),
        if engine.is_gate() {
            "gate"
        } else {
            "BEST EFFORT — recorded, never a silent skip"
        }
    );

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("runner has a parent")
        .to_path_buf();
    let work = root.join("work");
    let _ = std::fs::create_dir_all(&work);

    let mut ledger = Ledger::default();
    // BOXED: `run` is a multi-thousand-line async fn and its state
    // machine (with every awaited stage inlined) is far past the
    // default 1 MB thread stack in debug builds — the future itself
    // goes on the heap, which is what a stack overflow here means.
    let outcome = Box::pin(run(
        &root,
        &work,
        engine,
        browser_path.clone(),
        stage5,
        stage7,
        org_only,
        &inverse,
        routable,
        &mut ledger,
    ))
    .await;
    let code = match outcome {
        Ok(()) => {
            println!();
            println!("=== merged browser-harness ledger (Stage 4b + Stage 5) ===");
            for v in &ledger.0 {
                println!("  {:<6} {}", if v.pass { "PASS" } else { "FAIL" }, v.name);
                if !v.pass {
                    println!("         {}", v.detail);
                }
            }
            let failed = ledger.failed();
            println!("{} witness(es), {} failed", ledger.0.len(), failed);
            if !engine.is_gate() {
                println!(
                    "[harness] NOTE: engine {} is the recorded best-effort leg; its \
                     verdicts are evidence, and the gate is chromium + firefox.",
                    engine.as_str()
                );
            }
            i32::from(failed > 0)
        }
        Err(e) => {
            println!("RTCB FAIL harness — {e}");
            println!();
            println!("=== merged browser-harness ledger (incomplete) ===");
            for v in &ledger.0 {
                println!("  {:<6} {}", if v.pass { "PASS" } else { "FAIL" }, v.name);
                if !v.pass {
                    println!("         {}", v.detail);
                }
            }
            1
        }
    };
    uninstall_nss_store_if_owed();
    std::process::exit(code);
}

// ===================================================================
// The mDNS probe's transport evidence
// ===================================================================

/// What the ANCHOR saw of one attempt's pair, sampled while the
/// attempt was still running.
///
/// The anchor's `selected_pair` is addressed through the peer's
/// mesh endpoint, and a failed attempt settles its handles before
/// the step returns — so an after-the-fact read reports nothing
/// whether the anchor had answered this peer for ten seconds or had
/// never heard from it. Those are opposite diagnoses.
///
/// **What this does and does not establish.** The mesh endpoint
/// exists once the Noise handshake completes, so a readable pair
/// says the handshake completed and names the address str0m
/// transmits to plus `learned` — `signalled` or `peer-reflexive` —
/// which is the substantive mDNS fact. It does NOT timestamp the
/// instant str0m learned that address; the instant the check
/// succeeded is the browser's `connected` timing, and for an
/// obfuscated `.local` candidate with nothing but the anchor's own
/// host address signalled, a check that succeeded at all can only
/// have succeeded peer-reflexively.
#[derive(Default, Clone)]
struct AnchorWatch {
    /// ms from the start of the attempt to the anchor holding a
    /// mesh endpoint for this peer — i.e. to the handshake landing.
    endpoint_ms: Option<u128>,
    /// ms to the pair becoming readable, sampled. An upper bound on
    /// the handshake, not the address learn.
    learned_ms: Option<u128>,
    /// `local=… remote=… learned=signalled|peer-reflexive`.
    pair: Option<String>,
}

impl AnchorWatch {
    fn describe(&self) -> String {
        let endpoint = match self.endpoint_ms {
            Some(ms) => format!("mesh peer @{ms}ms"),
            None => "never installed a mesh peer for this node".into(),
        };
        match (&self.pair, self.learned_ms) {
            (Some(pair), Some(ms)) => format!("{pair}, readable by @{ms}ms ({endpoint})"),
            // Said explicitly, because the tempting misreading is
            // "the anchor never learned the peer-reflexive address".
            _ => format!(
                "pair NOT readable — {endpoint} — so the handshake did not complete; \
                 this says nothing about whether the pair formed, which the browser \
                 half above answers"
            ),
        }
    }
}

/// Sample the anchor's half of one attempt, every 20 ms, until
/// `stop`.
///
/// `learned` is str0m's own account of where the remote address came
/// from: `peer-reflexive` means it was discovered from the peer's
/// inbound binding request and the signalling contributed nothing,
/// which is exactly the mechanism a browser hiding its host IPs
/// behind `<uuid>.local` depends on.
async fn watch_anchor_pair(node: Arc<MeshNode>, peer: u64, stop: Arc<AtomicBool>) -> AnchorWatch {
    let t0 = std::time::Instant::now();
    let mut watch = AnchorWatch::default();
    while !stop.load(Ordering::Relaxed) {
        if let Some(PeerAddr::Rtc(id)) = node.peer_endpoint(peer) {
            if watch.endpoint_ms.is_none() {
                watch.endpoint_ms = Some(t0.elapsed().as_millis());
            }
            if let Some(driver) = node.rtc_driver() {
                if let Some((local, remote, learned)) = driver.selected_pair(id).await {
                    watch.learned_ms = Some(t0.elapsed().as_millis());
                    watch.pair = Some(format!("local={local} remote={remote} learned={learned}"));
                    return watch;
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    watch
}

/// The candidate TYPES one side produced, in the order they
/// appeared: `host mdns <uuid>.local:51829 @2ms`.
///
/// This is what tells a reflexive candidate that never existed
/// apart from a pair that formed and then lost its session — the
/// two mechanisms an mDNS-on failure can have, and a bare verdict
/// cannot distinguish them.
fn candidate_types(list: Option<&serde_json::Value>) -> String {
    let Some(items) = list.and_then(serde_json::Value::as_array) else {
        return "<not recorded>".into();
    };
    if items.is_empty() {
        return "NONE gathered".into();
    }
    items
        .iter()
        .map(|c| {
            let text = |k: &str| c.get(k).and_then(serde_json::Value::as_str).unwrap_or("?");
            let num = |k: &str| c.get(k).and_then(serde_json::Value::as_u64);
            format!(
                "{} {}{}:{} @{}ms",
                text("type"),
                if c.get("mdns") == Some(&serde_json::Value::Bool(true)) {
                    "mdns "
                } else {
                    ""
                },
                text("address"),
                num("port").map_or_else(|| "?".into(), |p| p.to_string()),
                num("ms").unwrap_or_default(),
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// `checking@7ms connected@11ms disconnected@12748ms` — when the
/// connectivity check actually succeeded, and whether the pair then
/// went away.
fn ice_timeline(list: Option<&serde_json::Value>) -> String {
    let Some(items) = list.and_then(serde_json::Value::as_array) else {
        return "<not recorded>".into();
    };
    if items.is_empty() {
        return "no state change".into();
    }
    items
        .iter()
        .map(|s| {
            format!(
                "{}@{}ms",
                s.get("state")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("?"),
                s.get("ms")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or_default(),
            )
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// One attempt's transport evidence, on one line: the candidate
/// types each side produced, the ICE state timeline, when the
/// DataChannel opened and msg1 left, the pair `getStats()`
/// selected, and the anchor's half.
///
/// Recorded for a FAILED attempt as well as a passing one, which is
/// the point: the next occurrence has to be diagnosable from the
/// log without a re-run. Reading it, `ice connected@Nms` with only
/// an obfuscated `.local` candidate gathered is the peer-reflexive
/// learn — nothing else could have made that check succeed — and a
/// failure after it is a failure downstream of ICE.
fn attempt_evidence(stage: Option<&serde_json::Value>, watch: &AnchorWatch) -> String {
    let field = |k: &str| stage.and_then(|s| s.get(k));
    let mut out = format!(
        "browser gathered [{}]; anchor signalled [{}]; ice {}",
        candidate_types(field("local_candidates")),
        candidate_types(field("remote_candidates")),
        ice_timeline(field("ice")),
    );
    match field("dc_open_ms").and_then(serde_json::Value::as_u64) {
        Some(ms) => out.push_str(&format!("; datachannel open @{ms}ms")),
        None => out.push_str("; datachannel NEVER opened"),
    }
    if let Some(ms) = field("msg1_sent_ms").and_then(serde_json::Value::as_u64) {
        out.push_str(&format!("; noise msg1 sent @{ms}ms"));
    }
    // Only a FAILED attempt snapshots `getStats()` itself — a
    // passing one is read by the `Stats` step, which the verdict
    // already quotes. Printing "<none>" for the passing case would
    // be an absence the page never reported.
    if let Some(selected) = field("selected").filter(|v| !v.is_null()) {
        out.push_str(&format!("; browser selected at failure {selected}"));
    }
    out.push_str(&format!("; anchor {}", watch.describe()));
    out
}

#[expect(clippy::too_many_lines, reason = "one linear harness script")]
async fn run(
    root: &Path,
    work: &Path,
    engine: Engine,
    browser_path: Option<String>,
    stage5: bool,
    // Opt-in: the Stage 7 store witnesses (see `--stage7`).
    stage7: bool,
    // Run ONLY the Stage 4 org witness stage (see `--org-only`).
    org_only: bool,
    inverse: &str,
    routable: bool,
    ledger: &mut Ledger,
) -> Result<(), String> {
    // --- 0. tools ---------------------------------------------------
    let driver = Driver::spawn(&root.join("driver")).await?;
    driver.ensure_engine(engine).await;
    let dist = build_leaf(root)?;

    // --- 1. certificate --------------------------------------------
    let ca = issue_localhost_certificate(work)?;
    let trust = establish_trust(&ca, engine).map_err(|why| {
        format!(
            "this engine could not be made to trust the harness leaf: {why}. This harness \
             will not fall back to --ignore-certificate-errors."
        )
    })?;
    println!("[harness] TLS trust: {}", trust.how);
    println!(
        "[harness] CA fingerprint source: {} ({} bytes); leaf SPKI pin {}",
        ca.ca_pem_path.display(),
        ca.ca_pem.len(),
        ca.spki_pin
    );

    // --- 2. nodes ---------------------------------------------------
    // **The only non-loopback bind in the harness, and it is opt-in
    // on Windows.** `lan_ipv4()` itself binds `0.0.0.0:0`, and the
    // anchor's and impostor's RTC sockets then bind the routable
    // address — three binds the Windows Firewall prompts about, once
    // per rebuild. When the flag is off the probe does not run at
    // all, so `lan` is `None` and every socket in this process is on
    // `127.0.0.1`: the topology and the verdicts that read `lan`
    // agree, rather than a verdict claiming an interface leg the
    // sockets never used.
    let lan = if routable { lan_ipv4() } else { None };
    let anchor_bind: SocketAddr = match lan {
        Some(v4) => SocketAddr::new(IpAddr::V4(v4), 0),
        None => "127.0.0.1:0".parse().expect("addr"),
    };
    if lan.is_none() {
        println!(
            "[harness] topology: LOOPBACK ONLY — every socket this process binds is on \
             127.0.0.1, so no firewall prompt is raised{}. The routable-interface leg is \
             therefore UNPROVEN on this host: `mdns_on_pair_formed` is measured on the \
             loopback pair only, and the MITM impostor shares loopback with the anchor \
             (on Firefox that pair may not form, because an obfuscated `.local` host \
             candidate and a 127.0.0.1 remote do not pair). Pass \
             --use-routable-interface to exercise it and accept the prompt.",
            if cfg!(windows) {
                " (the Windows default)"
            } else {
                " (--loopback-only)"
            }
        );
    }
    let (anchor, anchor_key) = spawn_node_identity(Some(anchor_rtc(anchor_bind))).await;
    let loop_anchor = spawn_node(Some(anchor_rtc("127.0.0.1:0".parse().expect("addr")))).await;
    // The impostor sits on the SAME interface as the real anchor.
    // Its whole point is a different Noise static key under the same
    // credential; its address is irrelevant to that. It used to bind
    // loopback, which Chromium reached through a peer-reflexive
    // candidate — but Firefox forms no pair at all between an
    // obfuscated `.local` host candidate and a `127.0.0.1` remote,
    // so on Firefox the MITM witness failed as a TEST ERROR ("the
    // DataChannel never opened"), proving nothing about the pinned
    // key. Same interface, same property, reachable on every engine.
    let impostor = spawn_node(Some(anchor_rtc(anchor_bind))).await;
    let third_party = spawn_node(None).await;
    // The identity whose *signed* capability announcement witness (b)
    // replays: the announcement's `node_id` has to be the id the
    // browser claims, or the anchor would drop it for a reason other
    // than §12 gate 4.
    let ident = spawn_node(None).await;
    ident
        .announce_capabilities(CapabilitySet::new())
        .await
        .map_err(|e| format!("announce: {e}"))?;
    let announcement = ident
        .announcement_bytes_for_send_for_test()
        .ok_or_else(|| "the identity node produced no announcement bytes".to_string())?;

    let anchor_rtc_addr = anchor
        .rtc_driver()
        .ok_or_else(|| "the anchor has no rtc driver".to_string())?
        .local_addr();
    let loop_rtc_addr = loop_anchor
        .rtc_driver()
        .ok_or_else(|| "the loopback anchor has no rtc driver".to_string())?
        .local_addr();
    println!("[harness] anchor rtc socket: {anchor_rtc_addr} (stun responder on)");
    println!("[harness] loopback anchor rtc socket: {loop_rtc_addr}");

    // --- 3. services on the anchor ---------------------------------
    let enroll_delivered: Arc<std::sync::Mutex<Vec<(u64, Vec<u8>)>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let enroll_gate = Arc::new(HoldGate::default());
    let _enroll = anchor
        .serve_rpc(
            ENROLL_SERVICE,
            Arc::new(Enrollment {
                delivered: Arc::clone(&enroll_delivered),
                gate: Arc::clone(&enroll_gate),
            }),
        )
        .map_err(|e| format!("serve {ENROLL_SERVICE}: {e}"))?;
    let other_calls = Arc::new(AtomicUsize::new(0));
    let _other = anchor
        .serve_rpc(OTHER_SERVICE, Arc::new(Counting(Arc::clone(&other_calls))))
        .map_err(|e| format!("serve {OTHER_SERVICE}: {e}"))?;

    // A real protected provider, so "enrolled is not authorized" is
    // observed as a handler that never ran — not as an absent
    // service.
    let protected_calls = Arc::new(AtomicUsize::new(0));
    let authority_dir = work.join("authority");
    let _ = std::fs::remove_dir_all(&authority_dir);
    let _protected = {
        use net::adapter::net::behavior::org::{OrgKeypair, OrgMembershipCert};
        use net::adapter::net::behavior::org_admission::OrgAdmission;
        use net::adapter::net::behavior::org_authority::NodeAuthority;
        let org = OrgKeypair::generate();
        let entity = anchor.entity_id().clone();
        let cert = OrgMembershipCert::try_issue(&org, entity.clone(), 1, 3600)
            .map_err(|e| format!("cert: {e:?}"))?;
        let authority = NodeAuthority::adopt(&authority_dir, cert, &entity, 0, None)
            .map_err(|e| format!("adopt: {e:?}"))?;
        anchor
            .install_node_authority(Arc::new(authority))
            .map_err(|e| format!("install authority: {e:?}"))?;
        anchor
            .serve_rpc_protected(
                PROTECTED_SERVICE,
                Arc::new(Counting(Arc::clone(&protected_calls))),
                OrgAdmission::OwnerDelegated,
                Arc::new(|_| true),
            )
            .map_err(|e| format!("serve {PROTECTED_SERVICE}: {e}"))?
    };

    // --- 4. the page server (its origin is the CORS allow-list) ----
    let (step_tx, step_rx) = mpsc::channel(4);
    let bundle = Bundle::locate(root);
    let mut step5_tx: HashMap<String, Step5Sender> = HashMap::new();
    let mut step5_rx: HashMap<String, Step5Queue> = HashMap::new();
    // Stage 6's two tabs have their own queues: they live in
    // separate browsing contexts and must not consume the Stage 5
    // tabs' steps, and two pages long-polling one queue would
    // resolve each other's.
    for tab in stage5::TABS
        .iter()
        .chain(stage6::TABS.iter())
        .chain(stage7::TABS.iter())
    {
        let (tx, rx) = mpsc::channel(4);
        step5_tx.insert((*tab).to_string(), tx);
        step5_rx.insert((*tab).to_string(), Arc::new(Mutex::new(rx)));
    }
    // The slice 2 tabs are a third script on a third route: their
    // page is `peer6.js` and its steps are not Stage 5 steps.
    let mut step6_tx: HashMap<String, Step6Sender> = HashMap::new();
    let mut step6_rx: HashMap<String, Step6Queue> = HashMap::new();
    for tab in stage6::PEER_TABS {
        let (tx, rx) = mpsc::channel(4);
        step6_tx.insert(tab.to_string(), tx);
        step6_rx.insert(tab.to_string(), Arc::new(Mutex::new(rx)));
    }
    // The Stage 4 org tabs are a fourth script on a fourth route:
    // their page is `org.js` and their steps are the eight org verbs.
    let mut step_org_tx: HashMap<String, StepOrgSender> = HashMap::new();
    let mut step_org_rx: HashMap<String, StepOrgQueue> = HashMap::new();
    for tab in org_stream::TABS {
        let (tx, rx) = mpsc::channel(4);
        step_org_tx.insert(tab.to_string(), tx);
        step_org_rx.insert(tab.to_string(), Arc::new(Mutex::new(rx)));
    }
    // The revocation facts feed's WS endpoint (the runner side of the
    // control-plane feed).
    let org_feed = Arc::new(org_stream::OrgControlFeed::default());
    let page_state = PageState {
        dist,
        page: root.join("page"),
        browser_dist: bundle.dist.clone(),
        steps: Arc::new(Mutex::new(step_rx)),
        steps5: Arc::new(step5_rx),
        steps6: Arc::new(step6_rx),
        steps_org: Arc::new(step_org_rx),
        org_feed: Arc::clone(&org_feed),
        pending: Arc::new(Mutex::new(HashMap::new())),
    };
    let (page_addr, _page_task) = serve_page(page_state)
        .await
        .map_err(|e| format!("page server: {e}"))?;
    let page_url = format!("http://localhost:{}/", page_addr.port());
    let origin = format!("http://localhost:{}", page_addr.port());
    println!("[harness] page: {page_url}");

    // --- 5. the bootstrap listeners --------------------------------
    //
    // R3: one issuer for the whole harness; every listener is
    // configured with its public half and every credential below is
    // signed with it.
    let issuer = Identity::generate();
    let tls = BootstrapTls::Operator {
        cert_pem: ca.cert_pem_path.clone(),
        key_pem: ca.key_pem_path.clone(),
    };
    let mut cfg = BootstrapConfig::new(
        "127.0.0.1:0".parse().expect("addr"),
        Psk::new(PSK),
        issuer.entity_id().clone(),
        tls.clone(),
        origin.clone(),
    );
    // Every context in this harness is one IP, and the halves add up:
    // Stage 4b, Stage 5's tabs, Stage 6's three slices and — with
    // `--stage7` — two more leaves. At 200 the last slice started
    // being refused, which surfaces as "the leaves did not discover
    // each other" three witnesses later rather than as a rate-limit
    // error, so the number is raised well past what the harness can
    // reach. Nothing here tests the limiter; the witnesses that care
    // about admission test §12, which is a different gate.
    cfg.offers_per_ip_per_minute = 2_000;
    let anchor_listener = serve_bootstrap(Arc::clone(&anchor), cfg.clone())
        .await
        .map_err(|e| format!("anchor listener: {e}"))?;
    let loop_listener = serve_bootstrap(Arc::clone(&loop_anchor), cfg.clone())
        .await
        .map_err(|e| format!("loopback listener: {e}"))?;
    let impostor_listener = serve_bootstrap(Arc::clone(&impostor), cfg)
        .await
        .map_err(|e| format!("impostor listener: {e}"))?;
    let anchor_base = format!("https://localhost:{}", anchor_listener.local_addr().port());
    let loop_base = format!("https://localhost:{}", loop_listener.local_addr().port());
    let impostor_base = format!(
        "https://localhost:{}",
        impostor_listener.local_addr().port()
    );
    println!("[harness] anchor   {anchor_base}");
    println!("[harness] impostor {impostor_base} (fresh Noise keypair)");

    // --- 6. the credential -----------------------------------------
    let root_entity = Identity::generate().entity_id().clone();
    let credential = |anchor_pub: [u8; 32], url: &str| {
        let invite = InviteToken::mint(&root_entity, url, Duration::from_secs(600));
        BrowserBootstrapCredential::mint(
            &issuer,
            invite,
            anchor_pub,
            Psk::new(PSK),
            url,
            Duration::from_secs(86_400),
        )
        .encode()
    };
    // ONE credential, pinning the REAL anchor's static key. The MITM
    // step presents this same credential to the impostor.
    let anchor_cred = credential(*anchor.public_key(), &anchor_base);
    let loop_cred = credential(*loop_anchor.public_key(), &loop_base);
    let anchor_pub_hex = hex(anchor.public_key());
    let psk_hex = hex(&PSK);

    // --- 7. the browser --------------------------------------------
    let profile = work.join("browser-profile");
    // `mdns-off-from-the-start` is the inverse for the mDNS split:
    // the diagnostic sweep still records pairs (so the old
    // measurement verdict is still satisfied), but nothing ran with
    // the engine's obfuscation ON, so `mdns_on_pair_formed` must
    // FAIL.
    let measure_with_mdns_off = inverse == "mdns-off-from-the-start";
    // The pin is only trustworthy if it is what makes TLS verify.
    // ASSERT that, before a single witness runs: a browser without
    // the flag must fail the handshake against this very listener,
    // and the same browser with it must succeed. A leftover root in
    // the OS store would make the first control succeed — and would
    // silently turn every TLS observation below into a statement
    // about the machine rather than about the harness.
    if let Some(pin) = trust.pin.as_deref() {
        let control = tls_pin_control(
            &driver,
            engine,
            pin,
            &format!("{anchor_base}/rtc/anchor"),
            browser_path.as_deref(),
        )
        .await?;
        println!("[harness] TLS pin control: {control}");
    } else if engine == Engine::Firefox {
        // Firefox's mechanism is a seeded PROFILE, so its control has
        // the same shape as Chromium's with a different difference:
        // two throwaway profiles, one seeded with this run's CA and
        // one not. Without the seeding the navigation must be
        // refused; with it, it must complete. Without both halves a
        // Firefox run's TLS observations would be statements about
        // whatever else the machine trusts.
        let control = tls_trust_control(
            &driver,
            engine,
            &format!("{anchor_base}/rtc/anchor"),
            browser_path.as_deref(),
            &ca,
        )
        .await?;
        println!("[harness] TLS trust control: {control}");
    } else {
        println!(
            "[harness] TLS pin control: not applicable — this run's mechanism is a trust \
             store ({}), so a browser without the pin is SUPPOSED to trust the listener \
             and the control could not discriminate",
            trust.how
        );
    }

    let launch = LaunchSpec {
        engine,
        profile: profile.clone(),
        ca_pem: ca.ca_pem_path.clone(),
        ca_nickname: NSS_NICKNAME.to_string(),
        disable_mdns: measure_with_mdns_off,
        executable: browser_path.clone(),
        spki_pin: trust.pin.clone(),
        // Never at launch: the UDP-blocked profile is one witness's
        // subject, and it installs and removes it itself.
        webrtc_udp_off: false,
    };
    let launched = driver
        .launch(&launch)
        .await
        .map_err(|e| format!("{}: {e}", engine.as_str()))?;
    // Firefox on Windows reads its own NSS database, and the driver
    // seeds it — unless the `certutil` it found was Microsoft's, in
    // which case it falls back to `security.enterprise_roots.enabled`
    // and reads the platform store. This harness never writes that
    // store, so there is nothing there to read and the leg is
    // refused rather than run against a listener it cannot verify.
    if launched.trust.contains("enterprise_roots") {
        return Err(format!(
            "{} fell back to the platform root store for trust ({}), and this harness never \
             writes the harness CA there — `certutil -addstore -user Root` raises a modal \
             security dialog on the desktop of whoever runs it, and so does its removal. \
             Install NSS's certutil (libnss3-tools / the `nss-tools` package) so the \
             profile's cert9.db can be seeded, and re-run. Refusing to continue against a \
             listener this browser cannot verify.",
            engine.as_str(),
            launched.trust
        ));
    }
    driver
        .open_page("main", &page_url)
        .await
        .map_err(|e| format!("opening the Stage 4b tab: {e}"))?;

    // Preflight, so "17 witnesses failed" is never the first thing
    // that tells you the engine has no WebRTC at all. Playwright's
    // WebKit for Windows is built WITHOUT `RTCPeerConnection`, and
    // every witness below it then fails with the same sentence.
    // This is a banner, not a verdict: adding a witness here would
    // move the floor the CI job pins.
    match driver
        .eval("main", "typeof RTCPeerConnection")
        .await
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
    {
        Some(kind) if kind == "function" => {
            println!("[harness] preflight: RTCPeerConnection is available");
        }
        other => {
            println!(
                "[harness] PREFLIGHT FAILURE: this {} build has NO RTCPeerConnection \
                 (typeof = {:?}). Every transport witness below will fail for that one \
                 reason. {}",
                engine.as_str(),
                other.unwrap_or_else(|| "unreadable".into()),
                if engine.is_gate() {
                    "This engine is a GATE, so that is a hard failure."
                } else {
                    "This engine is the recorded best-effort leg; the finding is the result."
                }
            );
        }
    }
    // Every probe below is attributed to this flag: a pair that
    // formed with the obfuscation DISABLED can never satisfy the
    // mDNS-on verdict.
    let mdns_on_for_measurement = !measure_with_mdns_off;

    // ================================================================
    // STAGE 4 — the org-scoped streaming witness stage.
    //
    // With `--org-only` it runs HERE and returns; in the full ledger
    // it runs after the 4b/5/6/7 halves (the tail below). Every
    // other half's witnesses are named `RTCB EXCLUDED` under
    // `--org-only`, so the ledger is never silently short.
    // ================================================================
    let cx_org = org_stream::CxOrg {
        driver: &driver,
        engine,
        anchor: &anchor,
        anchor_key: &anchor_key,
        credential: anchor_cred.clone(),
        bootstrap_base: anchor_base.clone(),
        origin: origin.clone(),
        page_origin: origin.clone(),
        anchor_rtc_addr: anchor_rtc_addr.to_string(),
        stun: Some(format!("stun:{anchor_rtc_addr}")),
        feed: Arc::clone(&org_feed),
        work: work.to_path_buf(),
        tabs: step_org_tx.clone(),
    };
    if org_only {
        Box::pin(org_stream::run(cx_org, ledger)).await?;
        let excluded: [&[&str]; 4] = [
            &[
                "enrollment_exchange_promotes_this_session",
                "enrolled_without_authority_is_still_denied",
                "provisional_announcement_is_refused",
                "provisional_subscribe_to_an_unrelated_channel_is_refused",
                "provisional_call_to_another_service_is_refused",
                "provisional_transit_is_refused",
                "local_envelope_accepted_redirected_denied",
                "mitm_anchor_fails_the_handshake_and_installs_nothing",
                "browser_enrollment_survives_replacement",
                "browser_session_bounds_are_enforced",
                "mdns_candidate_pair_measured",
                "mdns_on_pair_formed",
            ],
            &stage5::WITNESSES,
            &stage6::WITNESSES,
            &stage7::WITNESSES,
        ];
        for name in excluded.into_iter().flatten() {
            println!("RTCB EXCLUDED {name} — --org-only was passed");
        }
        // The same teardown the tail performs: the early return must
        // not leave the driver process or the listeners behind.
        driver.quit().await;
        anchor_listener.shutdown().await;
        loop_listener.shutdown().await;
        impostor_listener.shutdown().await;
        let _ = std::fs::remove_dir_all(&authority_dir);
        return Ok(());
    }
    println!(
        "[harness] {} {} launched; host-address obfuscation {}; CA trust: {}",
        engine.as_str(),
        launched.version,
        if launched.mdns_obfuscation {
            "ON"
        } else {
            "OFF"
        },
        launched.trust,
    );

    let mut script = Script {
        tx: step_tx,
        next_id: 1,
    };

    // ================================================================
    // Slice 3 — the mDNS measurement
    //
    // Two separate things live here, and they are now two separate
    // verdicts:
    //
    //   `mdns_candidate_pair_measured` — the DIAGNOSTIC sweep. It
    //     records what each `(interface, iceServers)` configuration
    //     did, including "NO PAIR", because the sweep's value is the
    //     comparison. It cannot, and must not, stand in for pair
    //     formation.
    //   `mdns_on_pair_formed` — the REQUIRED verdict: a pair that
    //     actually formed with the engine's mDNS obfuscation ON,
    //     naming the interface and BOTH halves of the selected pair
    //     (the browser's `getStats()` and the anchor's
    //     `selected_pair`). Previously a nonempty diagnostic log
    //     passed while every mDNS-on attempt had failed and the
    //     §12 witnesses ran with the obfuscation disabled.
    // ================================================================
    let mut mdns_lines: Vec<String> = Vec::new();
    let mut working_stun: Option<Option<String>> = None;

    /// One probe's pair evidence, kept structured so the required
    /// verdict reads facts rather than re-parsing a log line.
    struct PairEvidence {
        label: &'static str,
        /// The interface the ANCHOR is bound to for this probe.
        interface: String,
        /// Was the engine's mDNS obfuscation ON for this attempt?
        mdns_on: bool,
        /// Did the browser reach a Noise session at all?
        session: bool,
        /// The browser's selected candidate pair, if it reported one.
        browser_pair: Option<serde_json::Value>,
        /// The anchor's own transmit destination for this peer.
        anchor_pair: Option<String>,
        open_ms: f64,
        /// Everything the attempt's transport left behind, pass or
        /// fail: candidate types per side, the ICE timeline, the
        /// selected pair, and when the anchor learned where to
        /// answer. The verdict carries this instead of a bare
        /// "no pair formed".
        transport: String,
    }
    let mut pair_evidence: Vec<PairEvidence> = Vec::new();

    struct Probe {
        label: &'static str,
        base: String,
        cred: String,
        pub_hex: String,
        anchor_node: u64,
        stun: Option<String>,
        node_id: u64,
        /// The anchor-side interface this probe exercises.
        interface: String,
    }
    // Ordering is deliberate: the STUN variant runs FIRST on
    // loopback and SECOND on the interface. If a failure tracked
    // position rather than configuration, the two would fail
    // together on the same position — they do not, so a failure is
    // the `iceServers` configuration and not "the second session
    // on this anchor".
    let loop_iface = format!("loopback 127.0.0.1 (anchor rtc {loop_rtc_addr})");
    let lan_iface = match lan {
        Some(v4) => format!("{v4} (anchor rtc {anchor_rtc_addr})"),
        None => "<none>".to_string(),
    };
    let mut probes = vec![
        Probe {
            label: "loopback/anchor-stun",
            base: loop_base.clone(),
            cred: loop_cred.clone(),
            pub_hex: hex(loop_anchor.public_key()),
            anchor_node: loop_anchor.node_id(),
            stun: Some(format!("stun:{loop_rtc_addr}")),
            node_id: 0xB0B0_0102,
            interface: loop_iface.clone(),
        },
        Probe {
            label: "loopback/no-stun",
            base: loop_base.clone(),
            cred: loop_cred.clone(),
            pub_hex: hex(loop_anchor.public_key()),
            anchor_node: loop_anchor.node_id(),
            stun: None,
            node_id: 0xB0B0_0101,
            interface: loop_iface.clone(),
        },
    ];
    if lan.is_some() {
        probes.push(Probe {
            label: "interface/no-stun",
            base: anchor_base.clone(),
            cred: anchor_cred.clone(),
            pub_hex: anchor_pub_hex.clone(),
            anchor_node: anchor.node_id(),
            stun: None,
            node_id: 0xB0B0_0103,
            interface: lan_iface.clone(),
        });
        probes.push(Probe {
            label: "interface/anchor-stun",
            base: anchor_base.clone(),
            cred: anchor_cred.clone(),
            pub_hex: anchor_pub_hex.clone(),
            anchor_node: anchor.node_id(),
            stun: Some(format!("stun:{anchor_rtc_addr}")),
            node_id: 0xB0B0_0104,
            interface: lan_iface.clone(),
        });
    } else {
        mdns_lines
            .push("interface: SKIPPED — this host exposed no routable non-loopback IPv4".into());
    }

    for probe in &probes {
        let node = if probe.base == loop_base {
            &loop_anchor
        } else {
            &anchor
        };
        // The anchor's half is sampled WHILE the attempt runs. A
        // failed attempt has closed its `RTCPeerConnection` and the
        // anchor's session is gone by the time the step returns, so
        // an after-the-fact read cannot tell "the anchor learned the
        // peer and answered" from "the anchor never heard from it".
        let stop = Arc::new(AtomicBool::new(false));
        let watching = tokio::spawn(watch_anchor_pair(
            Arc::clone(node),
            probe.node_id,
            Arc::clone(&stop),
        ));
        let r = script
            .run(Step::Connect {
                id: 0,
                session: format!("mdns-{}", probe.label),
                base: probe.base.clone(),
                node_id: format!("{:016x}", probe.node_id),
                psk: psk_hex.clone(),
                anchor_pub: probe.pub_hex.clone(),
                anchor_node_id: format!("{:016x}", probe.anchor_node),
                credential: probe.cred.clone(),
                stun: probe.stun.clone(),
                timeout_ms: 35_000,
                expect_failure: false,
            })
            .await;
        stop.store(true, Ordering::Relaxed);
        let watch = watching.await.unwrap_or_default();
        let transport = attempt_evidence(r.stage.as_ref(), &watch);
        if !r.ok {
            mdns_lines.push(format!(
                "{}: NO PAIR — {}; {transport}",
                probe.label,
                r.error.clone().unwrap_or_else(|| "unknown".into())
            ));
            pair_evidence.push(PairEvidence {
                label: probe.label,
                interface: probe.interface.clone(),
                mdns_on: mdns_on_for_measurement,
                session: false,
                // The pair the attempt DID form, snapshotted by the
                // page before it settled its handles. It cannot
                // satisfy the verdict — that needs `session` — and
                // it is the difference between a mechanism that
                // never reached the interface and one that failed
                // after it did.
                browser_pair: r
                    .stage
                    .as_ref()
                    .and_then(|s| s.get("selected"))
                    .filter(|v| v.get("state").is_some())
                    .cloned(),
                anchor_pair: watch.pair.clone(),
                open_ms: f64::NAN,
                transport,
            });
            continue;
        }
        if working_stun.is_none() {
            working_stun = Some(probe.stun.clone());
        }
        let stats = script
            .run(Step::Stats {
                id: 0,
                session: format!("mdns-{}", probe.label),
            })
            .await;
        // `{"selected":null}` is the page saying "no pair in
        // getStats"; only a pair with a `state` is evidence.
        let browser_selected = stats
            .stats
            .as_ref()
            .filter(|v| v.get("state").is_some())
            .cloned();
        let browser_pair = stats
            .stats
            .as_ref()
            .map(|v| v.to_string())
            .unwrap_or_else(|| "<no getStats>".into());
        // The anchor-side half. str0m 0.23.1 exposes no candidate
        // TYPE; `selected_pair` reports the address traffic is going
        // to and whether that address was ever signalled.
        let anchor_selected = match node.peer_endpoint(probe.node_id) {
            Some(PeerAddr::Rtc(id)) => node
                .rtc_driver()
                .expect("rtc driver")
                .selected_pair(id)
                .await
                .map(|(local, remote, learned)| {
                    format!("local={local} remote={remote} learned={learned}")
                }),
            _ => None,
        };
        let anchor_pair = anchor_selected
            .clone()
            .unwrap_or_else(|| "<no transmit destination recorded yet>".into());
        mdns_lines.push(format!(
            "{}: PAIR FORMED in {:.0} ms; browser getStats {browser_pair}; anchor \
             {anchor_pair}; {transport}",
            probe.label,
            r.open_ms.unwrap_or(f64::NAN)
        ));
        pair_evidence.push(PairEvidence {
            label: probe.label,
            interface: probe.interface.clone(),
            mdns_on: mdns_on_for_measurement,
            session: true,
            browser_pair: browser_selected,
            anchor_pair: anchor_selected,
            open_ms: r.open_ms.unwrap_or(f64::NAN),
            transport,
        });
    }

    // If nothing formed with mDNS on, relaunch the browser with the
    // obfuscation off so the §12 witnesses can still run, and SAY SO.
    let mut mdns_needed_disabling = false;
    if working_stun.is_none() {
        mdns_needed_disabling = true;
        mdns_lines.push(format!(
            "NO configuration formed a pair with {}'s host-address obfuscation ON; \
             relaunching with it DISABLED so the §12 witnesses can run. The plan's \
             answer (c) — an mDNS client on the anchor — is therefore REQUIRED on \
             this host.",
            engine.as_str()
        ));
        let _ = driver.close_page("main").await;
        let _ = driver.shutdown_browser().await;
        let relaunched = driver
            .launch(&LaunchSpec {
                disable_mdns: true,
                ..launch.clone()
            })
            .await
            .map_err(|e| format!("{} relaunch: {e}", engine.as_str()))?;
        println!(
            "[harness] relaunched {} {} with host-address obfuscation OFF",
            engine.as_str(),
            relaunched.version
        );
        driver
            .open_page("main", &page_url)
            .await
            .map_err(|e| format!("reopening the Stage 4b tab: {e}"))?;
        working_stun = Some(Some(format!("stun:{anchor_rtc_addr}")));
    }
    let stun = working_stun.clone().unwrap_or(None);

    for line in &mdns_lines {
        println!("[mdns] {line}");
    }
    // The DIAGNOSTIC sweep. "NO PAIR" rows are part of its value,
    // so it passes on a nonempty comparison — and for exactly that
    // reason it can never be the pair-formation evidence.
    ledger.record(
        "mdns_candidate_pair_measured",
        !mdns_lines.is_empty(),
        format!(
            "DIAGNOSTIC sweep (not pair evidence): {}",
            mdns_lines.join(" | ")
        ),
    );

    // The REQUIRED verdict: a candidate pair that formed while
    // the engine's mDNS obfuscation was ON, with both halves of the
    // pair named. When the host has a routable interface, that
    // interface is where it has to be proven — a loopback pair says
    // nothing about a browser reaching an anchor across a LAN.
    {
        let want_interface = lan.is_some();
        let qualifies = |e: &&PairEvidence| {
            e.mdns_on
                && e.session
                && e.browser_pair.is_some()
                && e.anchor_pair.is_some()
                && (!want_interface || e.label.starts_with("interface/"))
        };
        let formed = pair_evidence.iter().find(qualifies);
        let detail = match formed {
            Some(e) => format!(
                "interface={} pair=browser{} anchor[{}] — probe {} formed in {:.0} ms with \
                 the engine's host-address obfuscation ON (no opt-out flag or pref passed); {}",
                e.interface,
                e.browser_pair
                    .as_ref()
                    .map(std::string::ToString::to_string)
                    .unwrap_or_default(),
                e.anchor_pair.clone().unwrap_or_default(),
                e.label,
                e.open_ms,
                e.transport,
            ),
            // The gate is unchanged — a pair, a session, both
            // halves named, on the routable interface. What changed
            // is that the failure now SAYS which mechanism it was.
            // It used to assert one ("an mDNS client on the anchor
            // is required"), and that is only one of the two
            // mechanisms. The discriminator is the browser's ice
            // timeline against what it gathered.
            None => format!(
                "no probe formed a pair that carried a session with the engine's \
                 host-address obfuscation ON{}. The diagnostic sweep above CANNOT satisfy \
                 this verdict. Evidence per probe — the discriminator is `ice` against \
                 `browser gathered`: a `connected@Nms` reached with nothing but an \
                 obfuscated `.local` candidate IS the peer-reflexive learn, so a failure \
                 after that instant is downstream of ICE and not the anchor's mDNS \
                 support; no `connected` at all, or no candidate but `.local` with no \
                 pair, is the anchor-side mDNS question: [{}]",
                if want_interface {
                    format!(" on the routable interface {lan_iface}")
                } else {
                    String::new()
                },
                pair_evidence
                    .iter()
                    .map(|e| format!(
                        "{}: mdns_on={} session={} browser_pair={} anchor_pair={} — {}",
                        e.label,
                        e.mdns_on,
                        e.session,
                        e.browser_pair.is_some(),
                        e.anchor_pair.is_some(),
                        e.transport,
                    ))
                    .collect::<Vec<_>>()
                    .join(" | ")
            ),
        };
        ledger.record("mdns_on_pair_formed", formed.is_some(), detail);
    }

    // ================================================================
    // The §12 witnesses
    // ================================================================
    let conn = Connector {
        base: anchor_base.clone(),
        credential: anchor_cred.clone(),
        anchor_pub: anchor_pub_hex.clone(),
        psk: psk_hex.clone(),
        anchor_node: anchor.node_id(),
        stun: stun.clone(),
    };

    // ---- (a) the permitted enrollment exchange --------------------
    let enroll_node: u64 = 0xB0B0_0001;
    let enroll_origin: u64 = 0xE1E1_0000_0000_0001;
    {
        let r = conn.connect(&mut script, "enroll", enroll_node).await;
        if !r.ok {
            ledger.record(
                "enrollment_exchange_promotes_this_session",
                false,
                format!(
                    "the browser could not establish a session: {}",
                    r.error.unwrap_or_default()
                ),
            );
        } else {
            let provisional_at_install = anchor.peer_is_provisional(enroll_node);
            let session_at_install = anchor.peer_session_id(enroll_node);
            let promoted_before = anchor.rtc_stats().admission_promoted();
            // Both halves derive the session id from the handshake;
            // if the browser's differs, the two are not the same
            // session and every observable below is about something
            // else.
            let same_session = r.session_id.as_deref()
                == session_at_install.map(|s| format!("{s:016x}")).as_deref();

            // 1. subscribe to our own enrollment reply channel — the
            //    one subscription §12 permits.
            let reply_channel = enroll_reply_channel(enroll_origin);
            let sub = subscribe_payload(&reply_channel, 0x5B5B_0001);
            let s = script
                .run(Step::Send {
                    id: 0,
                    session: "enroll".into(),
                    prefix: String::new(),
                    stream_id: format!("{:x}", SUBPROTOCOL_CHANNEL_MEMBERSHIP as u64),
                    subprotocol: SUBPROTOCOL_CHANNEL_MEMBERSHIP,
                    channel_hash: 0,
                    origin_hash: format!("{enroll_origin:016x}"),
                    reliable: false,
                    payload: hex(&sub),
                })
                .await;
            if !s.ok {
                return Err(format!("subscribe send failed: {:?}", s.error));
            }

            // The publisher's membership Ack, so "the subscription
            // was accepted" is an observed fact rather than an
            // assumption the missing reply would silently hide.
            let ack = script
                .run(Step::Expect {
                    id: 0,
                    session: "enroll".into(),
                    subprotocol: SUBPROTOCOL_CHANNEL_MEMBERSHIP,
                    timeout_ms: 10_000,
                })
                .await;
            let subscribe_accepted = ack
                .frame
                .as_deref()
                .map(unhex)
                .and_then(|f| membership::decode(&f).ok())
                .is_some_and(|m| matches!(m, MembershipMsg::Ack { accepted: true, .. }));

            // 2. the enrollment REQUEST.
            let frame = rpc_request_frame(ENROLL_SERVICE, enroll_origin, 0xC0FF_EE01, b"join");
            if inverse == "skip-enrollment-request" {
                println!("[harness] INVERSE: NOT sending the enrollment REQUEST");
            } else {
                let s = script
                    .run(Step::Send {
                        id: 0,
                        session: "enroll".into(),
                        prefix: String::new(),
                        stream_id: format!("{:x}", frame.stream_id),
                        subprotocol: 0,
                        channel_hash: frame.channel_hash_u16,
                        origin_hash: format!("{enroll_origin:016x}"),
                        reliable: true,
                        payload: hex(&frame.payload),
                    })
                    .await;
                if !s.ok {
                    return Err(format!("enrollment REQUEST send failed: {:?}", s.error));
                }
            }

            // 3. the RESPONSE, decrypted in the browser and handed
            //    back for decoding here. Event plane only — the
            //    membership Ack rides its own subprotocol and must
            //    not be mistaken for the reply.
            let reply = script
                .run(Step::Expect {
                    id: 0,
                    session: "enroll".into(),
                    subprotocol: 0,
                    timeout_ms: 20_000,
                })
                .await;
            let reply_ok = reply
                .frame
                .as_deref()
                .map(unhex)
                .map(|f| response_carries_admitted_outcome(&f))
                .unwrap_or(false);

            // `peer_is_provisional` going false is NOT by itself
            // promotion: a reclaimed session is also "not
            // provisional", because it is not there at all. The
            // counter is the transition; the session-id equality
            // below is what says it happened to THIS incarnation.
            let mut promoted = false;
            for _ in 0..100 {
                if anchor.rtc_stats().admission_promoted() > promoted_before
                    && !anchor.peer_is_provisional(enroll_node)
                {
                    promoted = true;
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            let session_after = anchor.peer_session_id(enroll_node);
            let promoted_delta = anchor.rtc_stats().admission_promoted() - promoted_before;
            let pass = provisional_at_install
                && same_session
                && subscribe_accepted
                && reply_ok
                && promoted
                && promoted_delta == 1
                && session_at_install.is_some()
                && session_at_install == session_after;
            ledger.record(
                "enrollment_exchange_promotes_this_session",
                pass,
                format!(
                    "installed provisional={provisional_at_install}; browser and anchor \
                     agree on the session id={same_session}; reply-channel Subscribe \
                     acked accepted={subscribe_accepted}; the BROWSER decrypted an \
                     Admitted JoinOutcome={reply_ok} ({}); peer_is_provisional now {}; \
                     admission_promoted +{promoted_delta}; session {:?} -> {:?}",
                    reply
                        .error
                        .clone()
                        .unwrap_or_else(|| "reply received".into()),
                    anchor.peer_is_provisional(enroll_node),
                    session_at_install,
                    session_after
                ),
            );

            // ---- the sixth §12 item: enrolled is not authorized ----
            //
            // R7d. This used to ignore the Send result and check
            // only "the handler ran zero times", which is equally
            // true of a frame that never arrived, a session that
            // was reclaimed, or a service that was never
            // registered. The evidence is now **call-correlated at
            // the completed boundary**, and the correlation used is
            // the TYPED ERROR THE BROWSER RECEIVES BACK: an
            // `RpcStatus::AdmissionDenied` (0x0009) whose
            // `EventMeta` call id is the call id this witness sent,
            // decrypted in the browser and decoded here.
            //
            // Why that and not a refusal counter: `RtcStats` has no
            // per-call-id refusal counter (and adding one to the
            // core is out of this repair's scope), so a counter
            // could only say "some call was refused in this
            // window" — which a concurrent witness could satisfy.
            // The typed terminal is call-correlated BY
            // CONSTRUCTION: it carries the call id, it can only
            // exist if the REQUEST reached the protected service's
            // admission gate, and it is unicast to the
            // AEAD-authenticated session that sent it. Zero handler
            // executions stays as the second half — the denial has
            // to be a refusal, not a reply from a handler that ran.
            let before = protected_calls.load(Ordering::SeqCst);
            let protected_call: u64 = 0xC0FF_EE02;
            // The inverse for R7d: send the REQUEST under a
            // DIFFERENT call id than the verdict correlates
            // against. The denial still arrives, the handler still
            // never runs — and the verdict must go RED, because it
            // is the correlation that is being asserted.
            let sent_call = if inverse == "protected-denial-uncorrelated" {
                println!(
                    "[harness] INVERSE: sending the protected REQUEST as call \
                     0xC0FFEE99 while the verdict correlates 0x{protected_call:X}"
                );
                0xC0FF_EE99
            } else {
                protected_call
            };
            let frame = rpc_request_frame(PROTECTED_SERVICE, enroll_origin, sent_call, b"{}");
            let sent = script
                .run(Step::Send {
                    id: 0,
                    session: "enroll".into(),
                    prefix: String::new(),
                    stream_id: format!("{:x}", frame.stream_id),
                    subprotocol: 0,
                    channel_hash: frame.channel_hash_u16,
                    origin_hash: format!("{enroll_origin:016x}"),
                    reliable: true,
                    payload: hex(&frame.payload),
                })
                .await;
            // The terminal the browser gets back for that call.
            let terminal = script
                .run(Step::Expect {
                    id: 0,
                    session: "enroll".into(),
                    subprotocol: 0,
                    timeout_ms: 15_000,
                })
                .await;
            let observed = terminal
                .frame
                .as_deref()
                .map(unhex)
                .and_then(|f| response_call_and_status(&f));
            let denied_this_call = observed == Some((protected_call, RpcStatus::AdmissionDenied));
            let after = protected_calls.load(Ordering::SeqCst);
            // The caller must still BE there and be enrolled: a
            // session that was reclaimed also never invokes the
            // handler, and would make this pass for the wrong
            // reason.
            let still_live = anchor.peer_session_id(enroll_node) == session_at_install;
            ledger.record(
                "enrolled_without_authority_is_still_denied",
                promoted && still_live && sent.ok && denied_this_call && after == before,
                format!(
                    "the caller is ENROLLED and still the same live session \
                     (promoted={promoted}, session unchanged={still_live}, \
                     peer_is_provisional={}); the REQUEST was sent={} ({}); the \
                     browser received back {:?} and the witness correlates \
                     (call=0x{protected_call:X}, AdmissionDenied)={denied_this_call} \
                     ({}); the registered protected handler ran {} time(s)",
                    anchor.peer_is_provisional(enroll_node),
                    sent.ok,
                    sent.error.clone().unwrap_or_else(|| "no error".into()),
                    observed,
                    terminal
                        .error
                        .clone()
                        .unwrap_or_else(|| "terminal received".into()),
                    after - before
                ),
            );
        }
    }

    // ---- (b) a provisional capability announcement ----------------
    {
        let node_id = ident.node_id();
        let r = conn.connect(&mut script, "announce", node_id).await;
        let before = anchor.rtc_stats().admission_refused_announce();
        if !r.ok {
            ledger.record(
                "provisional_announcement_is_refused",
                false,
                format!("no session: {}", r.error.unwrap_or_default()),
            );
        } else {
            let _ = script
                .run(Step::Send {
                    id: 0,
                    session: "announce".into(),
                    prefix: String::new(),
                    stream_id: format!("{:x}", SUBPROTOCOL_CAPABILITY_ANN as u64),
                    subprotocol: SUBPROTOCOL_CAPABILITY_ANN,
                    channel_hash: 0,
                    origin_hash: format!("{:016x}", ident.origin_hash()),
                    reliable: false,
                    payload: hex(&announcement),
                })
                .await;
            let hit = wait_for(
                || anchor.rtc_stats().admission_refused_announce() > before,
                Duration::from_secs(10),
            )
            .await;
            ledger.record(
                "provisional_announcement_is_refused",
                hit && anchor.peer_is_provisional(node_id),
                format!(
                    "admission_refused_announce {} -> {}",
                    before,
                    anchor.rtc_stats().admission_refused_announce()
                ),
            );
        }
    }

    // ---- (c) a provisional Subscribe to an unrelated channel ------
    {
        let node_id: u64 = 0xB0B0_0003;
        let r = conn.connect(&mut script, "subscribe", node_id).await;
        let before = anchor.rtc_stats().admission_refused_subscribe();
        if !r.ok {
            ledger.record(
                "provisional_subscribe_to_an_unrelated_channel_is_refused",
                false,
                format!("no session: {}", r.error.unwrap_or_default()),
            );
        } else {
            let sub = subscribe_payload("app.events.orders", 0x5B5B_0003);
            let _ = script
                .run(Step::Send {
                    id: 0,
                    session: "subscribe".into(),
                    prefix: String::new(),
                    stream_id: format!("{:x}", SUBPROTOCOL_CHANNEL_MEMBERSHIP as u64),
                    subprotocol: SUBPROTOCOL_CHANNEL_MEMBERSHIP,
                    channel_hash: 0,
                    origin_hash: format!("{node_id:016x}"),
                    reliable: false,
                    payload: hex(&sub),
                })
                .await;
            let hit = wait_for(
                || anchor.rtc_stats().admission_refused_subscribe() > before,
                Duration::from_secs(10),
            )
            .await;
            ledger.record(
                "provisional_subscribe_to_an_unrelated_channel_is_refused",
                hit,
                format!(
                    "admission_refused_subscribe {} -> {}",
                    before,
                    anchor.rtc_stats().admission_refused_subscribe()
                ),
            );
        }
    }

    // ---- (d) a provisional call to another service ----------------
    {
        let node_id: u64 = 0xB0B0_0004;
        let origin: u64 = 0xE1E1_0000_0000_0004;
        let r = conn.connect(&mut script, "other-service", node_id).await;
        let before = anchor.rtc_stats().admission_refused_deliver();
        let calls_before = other_calls.load(Ordering::SeqCst);
        if !r.ok {
            ledger.record(
                "provisional_call_to_another_service_is_refused",
                false,
                format!("no session: {}", r.error.unwrap_or_default()),
            );
        } else {
            let frame = rpc_request_frame(OTHER_SERVICE, origin, 0xC0FF_EE04, b"{}");
            let _ = script
                .run(Step::Send {
                    id: 0,
                    session: "other-service".into(),
                    prefix: String::new(),
                    stream_id: format!("{:x}", frame.stream_id),
                    subprotocol: 0,
                    channel_hash: frame.channel_hash_u16,
                    origin_hash: format!("{origin:016x}"),
                    reliable: true,
                    payload: hex(&frame.payload),
                })
                .await;
            let hit = wait_for(
                || anchor.rtc_stats().admission_refused_deliver() > before,
                Duration::from_secs(10),
            )
            .await;
            let calls_after = other_calls.load(Ordering::SeqCst);
            ledger.record(
                "provisional_call_to_another_service_is_refused",
                hit && calls_after == calls_before,
                format!(
                    "admission_refused_deliver {} -> {}; the registered {OTHER_SERVICE} \
                     handler ran {} time(s)",
                    before,
                    anchor.rtc_stats().admission_refused_deliver(),
                    calls_after - calls_before
                ),
            );
        }
    }

    // ---- (e) + (f) transit, local delivery, redirection -----------
    //
    // R7c. The locally-addressed half used to send `b"transit"` and
    // read an UNCHANGED transit-refusal counter as "delivered
    // locally" — which is equally true of a frame that was refused
    // at the *delivery* gate, dropped, or never sent at all (the
    // Send result was discarded too). Both halves are now positive
    // facts:
    //
    //   * the REDIRECTED envelope is refused as transit, counted;
    //   * the LOCALLY-ADDRESSED envelope carries a correlated
    //     enrollment REQUEST — the one action §12 permits a
    //     provisional session — whose nonce the runner generated,
    //     and the witness passes only when the anchor's REAL
    //     registered enrollment handler RAN with THAT call id and
    //     THAT body. What that establishes is **decoded handler
    //     delivery**: the envelope was decrypted, routed locally,
    //     decoded and dispatched, and the marker is appended
    //     inside the handler before it returns. It is deliberately
    //     NOT "a completed successful enrollment" — no correlated
    //     successful terminal is observed here (witness (a) is
    //     where a browser-received RESPONSE is read). The fact is
    //     decoded on the anchor, not inferred from an absence.
    //
    // The redirected envelope goes first, while the session is
    // still provisional: the local one ends with this session's
    // enrollment call delivered.
    {
        let node_id: u64 = 0xB0B0_0005;
        let origin: u64 = 0xE1E1_0000_0000_0005;
        let r = conn.connect(&mut script, "transit", node_id).await;
        if !r.ok {
            ledger.record(
                "provisional_transit_is_refused",
                false,
                format!("no session: {}", r.error.unwrap_or_default()),
            );
            ledger.record(
                "local_envelope_accepted_redirected_denied",
                false,
                "no session",
            );
        } else {
            // (e) — the envelope redirected at a third party.
            let before_transit = anchor.rtc_stats().admission_refused_transit();
            let redirected = script
                .run(Step::Send {
                    id: 0,
                    session: "transit".into(),
                    prefix: hex(&routing_prefix(third_party.node_id(), node_id)),
                    stream_id: format!("{TRANSIT_STREAM_ID:x}"),
                    subprotocol: 0,
                    channel_hash: 0,
                    origin_hash: format!("{origin:016x}"),
                    reliable: false,
                    payload: hex(b"transit"),
                })
                .await;
            let refused = redirected.ok
                && wait_for(
                    || anchor.rtc_stats().admission_refused_transit() > before_transit,
                    Duration::from_secs(10),
                )
                .await;
            ledger.record(
                "provisional_transit_is_refused",
                refused,
                format!(
                    "the envelope was sent={} ({}); admission_refused_transit {} -> {} \
                     for a third-party dest_id",
                    redirected.ok,
                    redirected
                        .error
                        .clone()
                        .unwrap_or_else(|| "no error".into()),
                    before_transit,
                    anchor.rtc_stats().admission_refused_transit()
                ),
            );

            // (f) — the locally-addressed envelope. Its reply
            // channel subscription first: the same single
            // subscription §12 permits, and the same shape witness
            // (a) uses, so the enrollment call inside the envelope
            // is the permitted action and nothing here widens the
            // allow-list.
            let sub = subscribe_payload(&enroll_reply_channel(origin), 0x5B5B_0005);
            let subscribed = script
                .run(Step::Send {
                    id: 0,
                    session: "transit".into(),
                    prefix: String::new(),
                    stream_id: format!("{:x}", SUBPROTOCOL_CHANNEL_MEMBERSHIP as u64),
                    subprotocol: SUBPROTOCOL_CHANNEL_MEMBERSHIP,
                    channel_hash: 0,
                    origin_hash: format!("{origin:016x}"),
                    reliable: false,
                    payload: hex(&sub),
                })
                .await;
            // The nonce is generated HERE, so the handler seeing it
            // cannot be explained by anything but this envelope
            // having been decrypted, routed locally, decoded and
            // dispatched.
            let nonce: u64 = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0x5EED_5EED_5EED_5EED);
            let mut local_body = Vec::from(*b"local-envelope:");
            local_body.extend_from_slice(format!("{nonce:016x}").as_bytes());
            let local_call: u64 = 0xC0FF_EE05;
            let frame = rpc_request_frame(ENROLL_SERVICE, origin, local_call, &local_body);
            let before_local = anchor.rtc_stats().admission_refused_transit();
            let local = script
                .run(Step::Send {
                    id: 0,
                    session: "transit".into(),
                    // The envelope's dest IS the anchor: local
                    // delivery, never transit.
                    prefix: hex(&routing_prefix(anchor.node_id(), node_id)),
                    stream_id: format!("{:x}", frame.stream_id),
                    subprotocol: 0,
                    channel_hash: frame.channel_hash_u16,
                    origin_hash: format!("{origin:016x}"),
                    reliable: true,
                    payload: hex(&frame.payload),
                })
                .await;
            let delivered = local.ok
                && wait_for(
                    || Enrollment::handler_ran(&enroll_delivered, local_call, &local_body),
                    Duration::from_secs(20),
                )
                .await;
            let after_local = anchor.rtc_stats().admission_refused_transit();
            ledger.record(
                "local_envelope_accepted_redirected_denied",
                subscribed.ok && local.ok && delivered && after_local == before_local && refused,
                format!(
                    "reply-channel Subscribe sent={}; the locally-addressed ROUTED \
                     envelope was sent={} ({}); DECODED HANDLER DELIVERY: the anchor's \
                     registered {ENROLL_SERVICE} handler RAN for call 0x{local_call:X} \
                     carrying the runner's nonce {nonce:016x}={delivered} (the marker is \
                     appended inside the handler before it returns, so this is delivery \
                     of a decoded call — NOT an observed successful enrollment \
                     terminal); transit refusals {before_local} -> \
                     {after_local} (a local envelope is not transit); redirected \
                     envelope refused={refused}",
                    subscribed.ok,
                    local.ok,
                    local.error.clone().unwrap_or_else(|| "no error".into()),
                ),
            );
        }
    }

    // ================================================================
    // Slice 5 — the pinned-key (MITM) witness
    //
    // R7b. The page no longer reports "failed as required" for any
    // exception: it asserts, in order, that the offer was ACCEPTED,
    // the DataChannel OPENED, Noise was constructed and msg1 was
    // SENT, and only then that the attempt died at the PINNED-KEY
    // boundary. Anything short of that arrives here as
    // `test_error`, and is recorded as a distinct FAIL — an HTTP or
    // ICE failure is a broken witness, not a refused MITM.
    //
    // The success control is the same code path against the REAL
    // anchor with the SAME credential: if that cannot reach a
    // session, the negative result above is not evidence about the
    // pinned key.
    //
    // **R7, the settlement gap.** "The impostor installed nothing"
    // used to be a peer-count sample taken 1.5 s after the page
    // gave up. The page gives up after 15 s; the impostor's own
    // `ice_deadline` is 90 s, and a failed `doConnect` used to
    // leave its DataChannel, trickle socket and PeerConnection
    // open — so the attempt the impostor ACCEPTED was still alive,
    // still owned by its completion task, when the counts were
    // read. A zero there is not evidence of anything.
    //
    // So the exact attempt's terminal owner boundary is required
    // FIRST, and it is established without sampling a race:
    //
    //   * OWNERSHIP is monotonic. The impostor admitted this
    //     attempt's signalling (`signal_delivered`, a counter that
    //     only goes up) and the page reports its offer ACCEPTED
    //     (HTTP 200) with the DataChannel OPEN — which only happens
    //     because the impostor answered the offer, and answering is
    //     the one path that registers the attempt's dialog under
    //     this claimed node id.
    //   * SETTLEMENT is the terminal state of that registration:
    //     this claimed node's open-attempt count back at zero.
    //     Given the ownership fact above the registration
    //     definitely happened, so zero here means it was RETIRED,
    //     rather than never having existed. Zero is reached only
    //     through the one release path — the accepted-attempt row
    //     dropped and the signalling reservation its ingress took
    //     released — whichever terminal owner drives it: the
    //     trickle socket closing (`end_bootstrap_dialog`), the
    //     attempt's own completion, or its expiry. This witness
    //     asserts the boundary, not which owner reached it; the
    //     browser's `[trickle …]` log lines say which one did on
    //     any given run.
    //
    // The page also settles its own handles when the attempt
    // fails, so an abandoned attempt is not left for the anchor to
    // discover at its 90 s deadline. The peak below is reported as
    // the attempt's observed liveness window, NOT as the premise:
    // the retirement can land within milliseconds of the answer,
    // and a sampler that misses that window must not turn a
    // correctly settled attempt into a failure.
    // ================================================================
    {
        let node_id: u64 = 0xB0B0_0006;
        let peers_before = impostor.peer_count();
        let prov_before = impostor.provisional_count();
        let dialogs_before = impostor.open_signal_dialogs(node_id);
        let signals_before = impostor.rtc_stats().signal_delivered();
        // Diagnostic only: how long this attempt's dialog was
        // observably open on the impostor.
        let dialog_peak = Arc::new(AtomicUsize::new(0));
        let watcher = {
            let impostor = Arc::clone(&impostor);
            let peak = Arc::clone(&dialog_peak);
            tokio::spawn(async move {
                loop {
                    peak.fetch_max(impostor.open_signal_dialogs(node_id), Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
        };
        // The SAME credential — so the impostor accepts it, answers
        // the offer, and the DataChannel opens. The browser pins the
        // REAL anchor's static key, which the impostor does not hold.
        let r = script
            .run(Step::Connect {
                id: 0,
                session: "mitm".into(),
                base: impostor_base.clone(),
                node_id: format!("{node_id:016x}"),
                psk: psk_hex.clone(),
                anchor_pub: if inverse == "mitm-pins-the-live-key" {
                    // The defect the credential exists to prevent:
                    // pinning whatever key the reached anchor holds.
                    hex(impostor.public_key())
                } else {
                    anchor_pub_hex.clone()
                },
                anchor_node_id: format!("{:016x}", impostor.node_id()),
                credential: if inverse == "mitm-offer-refused" {
                    // The inverse that proves the catch-all is gone:
                    // a credential the listener must refuse, so the
                    // offer is never accepted. The old code reported
                    // that as a PASS.
                    println!(
                        "[harness] INVERSE: presenting a corrupted credential to the impostor"
                    );
                    let mut bad = anchor_cred.clone();
                    bad.push('x');
                    bad
                } else {
                    anchor_cred.clone()
                },
                stun: stun.clone(),
                timeout_ms: 15_000,
                expect_failure: true,
            })
            .await;
        watcher.abort();
        let dialog_peak = dialog_peak.load(Ordering::SeqCst);
        let signals_delta = impostor.rtc_stats().signal_delivered() - signals_before;
        // The impostor really did accept and own an attempt for this
        // claimed node: it admitted the attempt's signalling, and
        // the page's own stages say the offer was accepted and the
        // DataChannel opened — which is the impostor having answered
        // that offer, the one path that registers the dialog.
        let stage_bool = |field: &str| {
            r.stage
                .as_ref()
                .and_then(|s| s.get(field))
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
        };
        let owned_an_attempt =
            signals_delta > 0 && stage_bool("offer_accepted") && stage_bool("dc_open");
        // The terminal owner boundary of THAT attempt, not a sleep:
        // its registration is back to zero, which it reaches only by
        // being retired.
        let settled = wait_for(
            || impostor.open_signal_dialogs(node_id) == 0,
            Duration::from_secs(30),
        )
        .await;
        let dialogs_after = impostor.open_signal_dialogs(node_id);
        let peers_after = impostor.peer_count();
        let prov_after = impostor.provisional_count();
        // `expect_failure` inverts `ok`: `ok` means "reached the
        // pinned-key boundary and failed there".
        let failed_at_pinned_key = r.ok;
        let test_error = r.test_error.unwrap_or(false);

        // The correct-key success control: the real anchor, the same
        // credential, the same `doConnect` path.
        let control_node: u64 = 0xB0B0_0016;
        let control = conn
            .connect(&mut script, "mitm-control", control_node)
            .await;
        let control_session_on_anchor = anchor.peer_session_id(control_node);
        let control_ok = control.ok
            && control.session_id.is_some()
            && control_session_on_anchor.map(|s| format!("{s:016x}")) == control.session_id;

        ledger.record(
            "mitm_anchor_fails_the_handshake_and_installs_nothing",
            failed_at_pinned_key
                && !test_error
                && control_ok
                && owned_an_attempt
                && settled
                && peers_after == peers_before
                && prov_after == prov_before,
            format!(
                "impostor attempt: failed AT THE PINNED-KEY BOUNDARY={failed_at_pinned_key}, \
                 test_error={test_error} — {} [stages {}]; the impostor ACCEPTED AND OWNED \
                 this attempt={owned_an_attempt} (it admitted its signalling, \
                 signal_delivered +{signals_delta}, and answered the offer, which is what \
                 registers the dialog for claimed node 0x{node_id:X}; that dialog was \
                 observed open with a peak of {dialog_peak} while the attempt ran); THAT \
                 attempt has now SETTLED={settled} — this claimed node's open-attempt \
                 registration is back to {dialogs_after} from {dialogs_before}, which it \
                 reaches only by being retired: its accepted-attempt row dropped and the \
                 signalling reservation its ingress took released (whichever terminal \
                 owner drove it — see the browser's [trickle mitm] lines); ONLY THEN \
                 sampled: impostor peer_count \
                 {peers_before} -> {peers_after}, provisional {prov_before} -> \
                 {prov_after}; correct-key control against the real anchor reached a \
                 session={control_ok} (browser {:?}, anchor {:?})",
                r.info
                    .clone()
                    .or_else(|| r.error.clone())
                    .unwrap_or_else(|| "no detail".into()),
                r.stage
                    .as_ref()
                    .map(std::string::ToString::to_string)
                    .unwrap_or_else(|| "<none>".into()),
                control.session_id,
                control_session_on_anchor,
            ),
        );
    }

    // ================================================================
    // R7a — the two browser schedules the original brief named and
    // Stage 4b never drove from a browser.
    // ================================================================

    // ---- a COLLIDING-CALL replacement promotes nothing ------------
    //
    // The 4a R2-A property, driven from Chromium, with the
    // discriminator the first version of this witness lacked (R7).
    //
    // The historical defect was a completion that consumed an
    // enrollment reservation by `(node, call)`, scanning across
    // session ids: call ids are sender-chosen, so a browser that
    // reconnected and reused one had its SUCCESSOR's reservation
    // spent — and its successor promoted — by the obsolete
    // incarnation's completion. The repair keys the reservation on
    // the composite `(node, session, call)` and re-verifies the
    // session inside `promote_admission`.
    //
    // A schedule whose two calls carry DIFFERENT call ids cannot
    // tell the repair from the defect: with no successor
    // reservation under the old call id, both a captured-incarnation
    // lookup and a current-session lookup find nothing and record an
    // orphan. So this schedule collides them deliberately — same
    // node id, same origin, same reply channel, and the SAME call id
    // on both sides:
    //
    //   1. the old exchange is parked inside the anchor's real
    //      provider and its browser session is then replaced;
    //   2. the successor's own exchange — same call id — is parked
    //      too, so BOTH handlers sit in the provider at once, and
    //      the successor's reservation `(node, new_session, call)`
    //      is positively armed (`kyra_has_enrollment_reservation`)
    //      rather than inferred;
    //   3. ONLY the old result is released. A completion that looks
    //      up the incarnation it CAPTURED finds nothing — that
    //      reservation was retired with its peer — and is counted
    //      orphaned. A completion that looks up the CURRENT session
    //      finds the successor's live reservation under the same
    //      call id and promotes it. The verdict requires the first
    //      AND requires the successor to still hold its own
    //      reservation and still be provisional afterwards;
    //   4. then the successor's OWN result is released and promotes
    //      it — the positive control, without which this would also
    //      pass on an anchor that can never promote anything.
    //
    // The source inverse that turns this RED: pass the CURRENT
    // session instead of the captured `receiving_session_id` to
    // `promote_on_enrollment_response` in `mesh_rpc.rs`'s
    // `Some(true)` arm.
    {
        let node_id: u64 = 0xB0B0_0007;
        let origin: u64 = 0xE1E1_0000_0000_0007;
        let reply_channel = enroll_reply_channel(origin);
        // ONE call id, both incarnations. The reservations differ in
        // the session id and in nothing else.
        let call: u64 = 0xC0FF_EE07;
        let old_body = Vec::from(*b"hold:the-old-exchange");
        let new_body = Vec::from(*b"hold:the-successors-own-exchange");

        let old = conn.connect(&mut script, "replace-old", node_id).await;
        if !old.ok {
            ledger.record(
                "browser_enrollment_survives_replacement",
                false,
                format!(
                    "the first browser session never established: {}",
                    old.error.unwrap_or_default()
                ),
            );
        } else {
            let old_session = anchor.peer_session_id(node_id);
            let sub = subscribe_payload(&reply_channel, 0x5B5B_0007);
            let subscribed = script
                .run(Step::Send {
                    id: 0,
                    session: "replace-old".into(),
                    prefix: String::new(),
                    stream_id: format!("{:x}", SUBPROTOCOL_CHANNEL_MEMBERSHIP as u64),
                    subprotocol: SUBPROTOCOL_CHANNEL_MEMBERSHIP,
                    channel_hash: 0,
                    origin_hash: format!("{origin:016x}"),
                    reliable: false,
                    payload: hex(&sub),
                })
                .await;
            let frame = rpc_request_frame(ENROLL_SERVICE, origin, call, &old_body);
            let held_sent = script
                .run(Step::Send {
                    id: 0,
                    session: "replace-old".into(),
                    prefix: String::new(),
                    stream_id: format!("{:x}", frame.stream_id),
                    subprotocol: 0,
                    channel_hash: frame.channel_hash_u16,
                    origin_hash: format!("{origin:016x}"),
                    reliable: true,
                    payload: hex(&frame.payload),
                })
                .await;
            // The call is INSIDE the provider, parked, with its
            // promotion reservation armed against this incarnation.
            let old_parked = held_sent.ok
                && wait_for(|| enroll_gate.is_parked(&old_body), Duration::from_secs(20)).await;
            let old_reserved = old_session
                .is_some_and(|s| anchor.kyra_has_enrollment_reservation(node_id, s, call));

            // The browser session goes away with the call still in
            // flight.
            let closed = script
                .run(Step::Close {
                    id: 0,
                    session: "replace-old".into(),
                })
                .await;
            let evicted = wait_for(
                || anchor.peer_session_id(node_id).is_none(),
                Duration::from_secs(25),
            )
            .await;
            // Recorded because it is WHY the correct lookup finds
            // nothing below: the evicted peer's reservations are
            // retired with it, so the old completion's own key is
            // gone while the successor's is live.
            let old_reservation_retired = old_session
                .is_some_and(|s| !anchor.kyra_has_enrollment_reservation(node_id, s, call));

            // The SUCCESSOR: a second browser session claiming the
            // same node id, and — as a reconnecting browser would —
            // the same origin and reply channel.
            let new = conn.connect(&mut script, "replace-new", node_id).await;
            // The browser's handshake finishing is not the anchor
            // having INSTALLED the peer: msg2 leaves the anchor
            // before it publishes the session, so reading the table
            // the instant `connect` returns is a race. Every other
            // capture site in this harness follows a step the anchor
            // already answered; this one follows an eviction, so it
            // waits for a session id that is present AND is not the
            // one that was just evicted. (Observed on the Linux CI
            // runner: `new_session` read `None` here and the verdict
            // failed with nothing else wrong.)
            let _installed = wait_for(
                || {
                    let now = anchor.peer_session_id(node_id);
                    now.is_some() && now != old_session
                },
                Duration::from_secs(25),
            )
            .await;
            let new_session = anchor.peer_session_id(node_id);
            let sub = subscribe_payload(&reply_channel, 0x5B5B_0008);
            let new_subscribed = script
                .run(Step::Send {
                    id: 0,
                    session: "replace-new".into(),
                    prefix: String::new(),
                    stream_id: format!("{:x}", SUBPROTOCOL_CHANNEL_MEMBERSHIP as u64),
                    subprotocol: SUBPROTOCOL_CHANNEL_MEMBERSHIP,
                    channel_hash: 0,
                    origin_hash: format!("{origin:016x}"),
                    reliable: false,
                    payload: hex(&sub),
                })
                .await;
            // The successor's OWN exchange, carrying the SAME call
            // id, parked in the provider alongside the old one.
            let frame = rpc_request_frame(ENROLL_SERVICE, origin, call, &new_body);
            let own_sent = script
                .run(Step::Send {
                    id: 0,
                    session: "replace-new".into(),
                    prefix: String::new(),
                    stream_id: format!("{:x}", frame.stream_id),
                    subprotocol: 0,
                    channel_hash: frame.channel_hash_u16,
                    origin_hash: format!("{origin:016x}"),
                    reliable: true,
                    payload: hex(&frame.payload),
                })
                .await;
            let new_parked = own_sent.ok
                && wait_for(|| enroll_gate.is_parked(&new_body), Duration::from_secs(20)).await;
            // The discriminator's premise, positively armed: a LIVE
            // reservation under the same `(node, call)` that differs
            // from the old one only in the session id.
            let successor_armed = new_session
                .is_some_and(|s| anchor.kyra_has_enrollment_reservation(node_id, s, call));
            // Both handlers are parked at the same time, and neither
            // has returned: nothing has been released yet.
            let both_parked = old_parked
                && new_parked
                && !Enrollment::handler_ran(&enroll_delivered, call, &old_body)
                && !Enrollment::handler_ran(&enroll_delivered, call, &new_body);

            // Release the OLD result, and only that one.
            let promoted_before = anchor.rtc_stats().admission_promoted();
            let orphaned_before = anchor.rtc_stats().admission_promotion_orphaned();
            enroll_gate.release(&old_body);
            let old_delivered = wait_for(
                || Enrollment::handler_ran(&enroll_delivered, call, &old_body),
                Duration::from_secs(20),
            )
            .await;
            let orphaned = wait_for(
                || anchor.rtc_stats().admission_promotion_orphaned() > orphaned_before,
                Duration::from_secs(20),
            )
            .await;
            tokio::time::sleep(Duration::from_millis(1500)).await;
            let promoted_delta = anchor.rtc_stats().admission_promoted() - promoted_before;
            // The successor still OWNS its call: the obsolete
            // completion spent none of its reservation.
            let successor_keeps_ownership = new_session
                .is_some_and(|s| anchor.kyra_has_enrollment_reservation(node_id, s, call));
            let successor_untouched = anchor.peer_is_provisional(node_id)
                && anchor.peer_session_id(node_id) == new_session
                && new_session.is_some()
                && new_session != old_session
                && !Enrollment::handler_ran(&enroll_delivered, call, &new_body);

            // Now the successor's own result: the same call id, its
            // own reservation, and it promotes.
            enroll_gate.release(&new_body);
            let own_promoted = wait_for(
                || {
                    Enrollment::handler_ran(&enroll_delivered, call, &new_body)
                        && anchor.rtc_stats().admission_promoted() > promoted_before
                        && !anchor.peer_is_provisional(node_id)
                        && anchor.peer_session_id(node_id) == new_session
                },
                Duration::from_secs(25),
            )
            .await;
            let own_reservation_spent = new_session
                .is_some_and(|s| !anchor.kyra_has_enrollment_reservation(node_id, s, call));

            ledger.record(
                "browser_enrollment_survives_replacement",
                subscribed.ok
                    && old_parked
                    && old_reserved
                    && closed.ok
                    && evicted
                    && new.ok
                    && new_subscribed.ok
                    && new_parked
                    && successor_armed
                    && both_parked
                    && old_delivered
                    && orphaned
                    && promoted_delta == 0
                    && successor_keeps_ownership
                    && successor_untouched
                    && own_promoted
                    && own_reservation_spent,
                format!(
                    "ONE call id 0x{call:X} on both sides. Old session {old_session:?}: \
                     parked in the provider={old_parked}, its reservation armed={old_reserved}; \
                     browser closed it={} and the anchor evicted the peer={evicted}, \
                     retiring that reservation={old_reservation_retired}. Successor session \
                     {new_session:?} installed={} (browser said {:?}, subscribed={}); its OWN \
                     call — same id — parked={new_parked} with its reservation \
                     (node, successor session, 0x{call:X}) ARMED={successor_armed}, both \
                     handlers parked and neither returned={both_parked}. Released the OLD \
                     result ONLY: it reached the handler={old_delivered} and was counted \
                     ORPHANED={orphaned} (admission_promotion_orphaned {orphaned_before} -> \
                     {}); admission_promoted +{promoted_delta}; the successor still HOLDS its \
                     reservation={successor_keeps_ownership} and is untouched and still \
                     provisional={successor_untouched} (provisional={}). Then released the \
                     successor's own result: it promoted={own_promoted} and its reservation \
                     was spent={own_reservation_spent}",
                    closed.ok,
                    new.ok,
                    new.error.as_deref().unwrap_or("ok"),
                    new_subscribed.ok,
                    anchor.rtc_stats().admission_promotion_orphaned(),
                    anchor.peer_is_provisional(node_id),
                ),
            );
        }
    }

    // ---- the per-session §12 bounds close a browser session -------
    //
    // The bounds that landed in 4a (`MAX_PROVISIONAL_FRAMES`,
    // `MAX_PROVISIONAL_BYTES`, `MAX_PROVISIONAL_STREAMS`), NOT the
    // owner-pending §12.5 policies. A burst under every bound leaves
    // the session installed; crossing the whole-session BYTE bound
    // ends it.
    //
    // **What this witness does and does not establish (R7).** It
    // observes exactly three things: the control burst leaving the
    // session installed, the anchor REMOVING this session's peer
    // entry after the byte bound is crossed (and `admission_reclaimed`
    // — the reclaim decision's own counter — moving with it), and the
    // BROWSER then seeing this session's transport go down: its
    // DataChannel leaving `open`, or its `RTCPeerConnection` leaving
    // a live state because ICE consent to the anchor's endpoint
    // lapsed. The page never closes this session itself, so that is
    // the far end's evidence that the close reached the TRANSPORT
    // and not only the peer table. It does NOT establish the other
    // §12 bounds (only the byte bound is crossed here, deliberately,
    // with the frame count kept under its own bound), and it does
    // not read the provisional-endpoint projection for this exact
    // endpoint: no accessor exposes one endpoint's membership, and
    // the aggregate count moves for unrelated sessions' expiry too,
    // so it would not be evidence about this one.
    {
        let node_id: u64 = 0xB0B0_0009;
        let origin: u64 = 0xE1E1_0000_0000_0009;
        let r = conn.connect(&mut script, "bounds", node_id).await;
        if !r.ok {
            ledger.record(
                "browser_session_bounds_are_enforced",
                false,
                format!("no session: {}", r.error.unwrap_or_default()),
            );
        } else {
            let installed = std::time::Instant::now();
            let session = anchor.peer_session_id(node_id);
            // Well under 256 frames, 256 KiB and the 64 KiB tracked
            // stream bytes: the session must survive this, or the
            // close below would prove nothing about a bound.
            let control = script
                .run(Step::Burst {
                    id: 0,
                    session: "bounds".into(),
                    stream_id: format!("{TRANSIT_STREAM_ID:x}"),
                    subprotocol: 0,
                    channel_hash: 0,
                    origin_hash: format!("{origin:016x}"),
                    reliable: false,
                    frames: 4,
                    payload_len: 7_900,
                    fill: 0xA5,
                })
                .await;
            tokio::time::sleep(Duration::from_millis(1200)).await;
            let alive_after_control = anchor.peer_session_id(node_id) == session;
            let reclaimed_before = anchor.rtc_stats().admission_reclaimed();

            // Now cross `MAX_PROVISIONAL_BYTES`.
            let over = script
                .run(Step::Burst {
                    id: 0,
                    session: "bounds".into(),
                    stream_id: format!("{TRANSIT_STREAM_ID:x}"),
                    subprotocol: 0,
                    channel_hash: 0,
                    origin_hash: format!("{origin:016x}"),
                    reliable: false,
                    frames: 48,
                    payload_len: 7_900,
                    fill: 0xA5,
                })
                .await;
            let sent_bytes = control.sent_bytes.unwrap_or(0) + over.sent_bytes.unwrap_or(0);
            let sent_frames = control.sent_frames.unwrap_or(0) + over.sent_frames.unwrap_or(0);
            let closed = wait_for(
                || anchor.peer_session_id(node_id) != session,
                Duration::from_secs(20),
            )
            .await;
            let reclaimed = anchor.rtc_stats().admission_reclaimed() > reclaimed_before;
            // The transport teardown, observed at the far end: the
            // page never closes this session itself, so its
            // DataChannel leaving `open` — or its peer connection
            // leaving a live state, ICE consent to the anchor's
            // endpoint having lapsed — is the anchor's close having
            // reached the transport and not only the peer table.
            let torn_down = script
                .run(Step::ExpectClosed {
                    id: 0,
                    session: "bounds".into(),
                    timeout_ms: 20_000,
                })
                .await;
            let elapsed = installed.elapsed();
            // The whole schedule finishes well inside
            // `PROVISIONAL_TTL`, so expiry cannot be the reason the
            // session went away.
            let before_ttl = elapsed < PROVISIONAL_TTL;
            ledger.record(
                "browser_session_bounds_are_enforced",
                alive_after_control
                    && closed
                    && reclaimed
                    && torn_down.ok
                    && before_ttl
                    && sent_bytes > MAX_PROVISIONAL_BYTES
                    && sent_frames < u64::from(MAX_PROVISIONAL_FRAMES),
                format!(
                    "session {session:?}: {} control frame(s) ({} bytes) left it \
                     installed={alive_after_control}; the browser then sent {} frame(s) \
                     for {sent_bytes} bytes total — over MAX_PROVISIONAL_BYTES \
                     ({MAX_PROVISIONAL_BYTES}) and under MAX_PROVISIONAL_FRAMES \
                     ({MAX_PROVISIONAL_FRAMES}), so the BYTE bound is the one crossed; \
                     the anchor REMOVED this session's peer entry={closed} (now {:?}) and \
                     counted the reclaim decision, admission_reclaimed {reclaimed_before} \
                     -> {} ({reclaimed}); the BROWSER then observed this session's \
                     TRANSPORT go down={} — it never closes this session itself, so this \
                     is the anchor's close reaching the transport rather than only the \
                     peer table ({}); it is NOT a read of the provisional-endpoint \
                     projection for this endpoint; \
                     {:.1} s after install, inside PROVISIONAL_TTL={before_ttl} ({})",
                    control.sent_frames.unwrap_or(0),
                    control.sent_bytes.unwrap_or(0),
                    sent_frames,
                    anchor.peer_session_id(node_id),
                    anchor.rtc_stats().admission_reclaimed(),
                    torn_down.ok,
                    torn_down
                        .info
                        .clone()
                        .or_else(|| torn_down.error.clone())
                        .unwrap_or_else(|| "no detail".into()),
                    elapsed.as_secs_f64(),
                    over.info.clone().unwrap_or_default(),
                ),
            );
        }
    }

    // --- done -------------------------------------------------------
    let _ = script.run(Step::Done { id: 0 }).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let _ = driver.close_page("main").await;

    // ================================================================
    // Stage 7 — the store, on the transport
    //
    // Two isolated contexts, the same anchor, the same ledger, and
    // FIRST — which is the whole finding of the previous round.
    //
    // An announcement is flooded to the nodes connected when it is
    // made and is never replayed to one that arrives later, and the
    // store cannot address a peer it has not discovered:
    // `openStream({peer})` needs a session, and the relayed one is
    // installed by the discovery path. Running last, this stage
    // discovered nothing — including, as the control, Stage 6's own
    // tag. So it opens its pair before anyone else announces, and
    // closes its contexts before Stage 5 begins, leaving the topology
    // every other witness was recorded on unchanged.
    // ================================================================
    if stage5 && stage7 {
        let cx7 = stage7::Cx7 {
            driver: &driver,
            anchor: &anchor,
            credential: anchor_cred.clone(),
            bootstrap_url: anchor_base.clone(),
            origin: origin.clone(),
            page_origin: origin.clone(),
            stun: stun.clone(),
            anchor_rtc_addr: anchor_rtc_addr.to_string(),
            tabs: step5_tx.clone(),
        };
        stage7::run(cx7, ledger).await?;
    } else {
        for name in stage7::WITNESSES {
            println!(
                "RTCB SKIPPED {name} — pass --stage7 to run the store witnesses. CI's \
                 Chromium leg does (floor 55, all eight pinned); the Firefox leg does \
                 not, because they have never been run on that engine"
            );
        }
    }

    // ================================================================
    // Stage 5 — the leaf crate and the TypeScript wrapper
    //
    // Same browser, same anchor, same ledger. The bundle is a hard
    // dependency: absent, every witness below is recorded FAIL with
    // the paths and the build commands. `--no-stage5` excludes the
    // half explicitly and still NAMES every excluded witness, so a
    // short ledger is never silent.
    // ================================================================
    if stage5 {
        let cx = stage5::Cx {
            driver: &driver,
            engine,
            anchor: &anchor,
            anchor_rtc_addr,
            credential: anchor_cred.clone(),
            bootstrap_url: anchor_base.clone(),
            origin: origin.clone(),
            page_origin: origin.clone(),
            stun: stun.clone(),
            bundle,
            tabs: step5_tx.clone(),
            launch: launch.clone(),
        };
        stage5::run(cx, ledger).await?;
    } else {
        println!(
            "[harness] --no-stage5: the Stage 5 half was EXCLUDED by the command line. \
             The witnesses below were not run; the CI floor counts `RTCB PASS` lines and \
             will therefore fail, which is the point."
        );
        for name in stage5::WITNESSES {
            println!("RTCB EXCLUDED {name} — --no-stage5 was passed");
        }
    }

    // ================================================================
    // Stage 6 slice 1 — browser ↔ browser direct, from a page
    //
    // Two ISOLATED browsing contexts on the same browser and the same
    // anchor. Runs after Stage 5 deliberately: it opens two more
    // contexts and leaves the launch context's pages alone, so every
    // Stage 5 witness has already been recorded on exactly the
    // topology it passes on today.
    //
    // Gated on the same `--no-stage5` flag, because it needs the same
    // bundle: excluding one half and running the other would report a
    // Stage 6 result against an untested leaf.
    // ================================================================
    if stage5 {
        let cx6 = stage6::Cx6 {
            driver: &driver,
            engine,
            anchor: &anchor,
            credential: anchor_cred.clone(),
            bootstrap_url: anchor_base.clone(),
            origin: origin.clone(),
            page_origin: origin.clone(),
            stun: stun.clone(),
            tabs: step5_tx,
            peer_tabs: step6_tx,
            anchor_rtc_addr: anchor_rtc_addr.to_string(),
        };
        stage6::run(cx6, ledger).await?;
    } else {
        for name in stage6::WITNESSES {
            println!("RTCB EXCLUDED {name} — --no-stage5 was passed");
        }
    }

    // ================================================================
    // STAGE 4 — the org-scoped streaming witnesses. The full-ledger
    // tail (`--org-only` ran them earlier and returned): same anchor,
    // same ledger, its own tabs and step vocabulary.
    // ================================================================
    Box::pin(org_stream::run(cx_org, ledger)).await?;

    driver.quit().await;
    anchor_listener.shutdown().await;
    loop_listener.shutdown().await;
    impostor_listener.shutdown().await;
    let _ = std::fs::remove_dir_all(&authority_dir);
    if mdns_needed_disabling {
        println!(
            "[mdns] NOTE: the §12 witnesses above ran with the engine's mDNS \
             obfuscation DISABLED, because no pair formed with it on."
        );
    }
    Ok(())
}

/// Does this decrypted event-plane frame carry an nRPC RESPONSE whose
/// body is an **Admitted** `JoinOutcome`?
fn response_carries_admitted_outcome(frame: &[u8]) -> bool {
    let Some(meta) = EventMeta::from_bytes(frame) else {
        return false;
    };
    if meta.dispatch != DISPATCH_RPC_RESPONSE {
        return false;
    }
    let Some(body) = frame.get(RPC_FRAME_BODY_OFFSET..) else {
        return false;
    };
    let Ok(resp) = RpcResponsePayload::decode(Bytes::copy_from_slice(body)) else {
        return false;
    };
    resp.body.as_ref() == admitted_outcome().as_ref()
}

/// The `(call_id, status)` of a decrypted event-plane nRPC RESPONSE
/// — the typed terminal a caller gets back, including a refusal
/// (`RpcStatus::AdmissionDenied`) emitted instead of dispatching a
/// handler. The call id rides `EventMeta::seq_or_ts`, which is where
/// the client's publish site puts it, so a terminal can be matched
/// to the exact call that produced it (R7d).
fn response_call_and_status(frame: &[u8]) -> Option<(u64, RpcStatus)> {
    let meta = EventMeta::from_bytes(frame)?;
    if meta.dispatch != DISPATCH_RPC_RESPONSE {
        return None;
    }
    let body = frame.get(RPC_FRAME_BODY_OFFSET..)?;
    let resp = RpcResponsePayload::decode(Bytes::copy_from_slice(body)).ok()?;
    Some((meta.seq_or_ts, resp.status))
}

async fn wait_for<F: Fn() -> bool>(predicate: F, within: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + within;
    while tokio::time::Instant::now() < deadline {
        if predicate() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    predicate()
}
