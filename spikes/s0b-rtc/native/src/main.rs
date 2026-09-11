//! S0b — RTC loop spike, native anchor side.
//!
//! Throwaway. Three threads, no shared locks around `Rtc`:
//!
//! ```text
//!   http threads            mesh thread                 driver thread
//!   (signalling +           (the "mesh side")           (owns UDP socket
//!    static files)                                       + every Rtc)
//!        |                        |                            |
//!        |--- Cmd (mpsc) ------------------------------------->|
//!        |                        |<-- ingress queue (bounded) -|
//!        |                        |--- per-peer send queue ---->|
//!        |                                (bounded, 256)        |
//! ```
//!
//! The driver thread is the only thing that ever touches an `Rtc`, and
//! it obeys str0m's single-mutation invariant: every mutation is
//! followed by a complete `poll_output` drain to `Output::Timeout`
//! before the next one.

use std::collections::HashMap;
use std::io::{ErrorKind, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::{mpsc, Arc};

use parking_lot::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use str0m::change::{SdpAnswer, SdpOffer, SdpPendingOffer};
use str0m::channel::{ChannelConfig, ChannelId};
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event, Input, Output, Rtc, RtcConfig};

use s0a_wire::crypto::{handshake_prologue, NoiseHandshake, SessionKeys, StaticKeypair};
use s0a_wire::parsed_packet::ParsedPacket;
use s0a_wire::protocol::{EventFrame, PacketFlags};
use s0a_wire::session::NetSession;

// ---------------------------------------------------------------------
// Spike constants
// ---------------------------------------------------------------------

/// Per-peer outbound queue depth — the §2 hard bound (`send_queue_packets`).
const SEND_QUEUE_PACKETS: usize = 256;
/// Bounded ingress queue into the "mesh side".
const INGRESS_QUEUE_PACKETS: usize = 1024;
/// Advisory buffered-amount threshold (§2 `buffered_amount_advisory`).
const BUFFERED_AMOUNT_ADVISORY: usize = 256 * 1024;

/// Node ids used for the Noise prologue binding.
const NATIVE_NODE_ID: u64 = 0x5151_5151_5151_5151;
const BROWSER_NODE_ID: u64 = 0x0B0B_0B0B_0B0B_0B0B;
/// Pre-shared key for NKpsk0 (spike: fixed, published over signalling).
const PSK: [u8; 32] = [0x5b; 32];

/// DataChannel message tags.
const TAG_NOISE_MSG1: u8 = 0x01;
const TAG_NOISE_MSG2: u8 = 0x02;
const TAG_NET_PACKET: u8 = 0x03;
const TAG_PROBE_START: u8 = 0x04;
const TAG_PROBE_FILL: u8 = 0x05;

/// Admission probe shape: how many filler packets the mesh side tries to
/// push while the page's event loop is blocked, and how big each is.
const PROBE_PACKETS: usize = 200_000;
const PROBE_PACKET_BYTES: usize = 8000;
/// How long the mesh side keeps pushing (the page blocks for 3 s).
const PROBE_WINDOW_MS: u64 = 3500;

// ---------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------

/// Handle the mesh side uses to reach one peer's driver-owned channel.
struct PeerHandle {
    /// Bounded per-peer outbound queue (the only cross-thread structure
    /// on the send path). `try_send` on this is the admission boundary.
    out_tx: SyncSender<Vec<u8>>,
    /// Buffered-amount snapshot published by the driver before each
    /// `Channel::write`. Advisory by construction (§2).
    buffered: Arc<AtomicUsize>,
    /// Bumped when the channel closes; queued work carrying an old
    /// generation is discarded.
    generation: Arc<AtomicU64>,
}

#[derive(Default)]
struct ProbeStats {
    /// `try_send` refusals at the admission boundary (queue full).
    admission_refusals: usize,
    /// Packets accepted into the queue.
    accepted: usize,
    /// `Channel::write` returning `Ok(false)` (post-acceptance refusal).
    write_false: usize,
    /// `buffered_amount` at the first `Ok(false)`, if any.
    write_false_at_buffered: Option<usize>,
    /// Max buffered_amount observed by the driver.
    max_buffered: usize,
    /// Advisory reading staleness: |published - actual at next drain|.
    max_staleness: usize,
    /// Sum/count for a mean staleness.
    staleness_sum: u64,
    staleness_n: u64,
    /// Packets still queued when the channel closed.
    dropped_on_close: usize,
    /// Advisory threshold crossings observed by the mesh side.
    advisory_over: usize,
    /// Windows WSAECONNRESET readings swallowed by the driver.
    udp_connreset: usize,
}

struct Shared {
    cmd_tx: mpsc::Sender<Cmd>,
    peers: Mutex<HashMap<String, PeerHandle>>,
    results: Mutex<Vec<String>>,
    probe: Mutex<ProbeStats>,
    notes: Mutex<Vec<String>>,
    done: Mutex<bool>,
}

impl Shared {
    fn note(&self, s: impl Into<String>) {
        let s = s.into();
        println!("[native] {s}");
        self.notes.lock().push(s);
    }
}

// ---------------------------------------------------------------------
// Driver commands
// ---------------------------------------------------------------------

enum Cmd {
    /// Browser offered; native answers (native = ICE controlled / answerer).
    Answer {
        sid: String,
        offer_sdp: String,
        reply: SyncSender<Result<String, String>>,
    },
    /// Native offers (native = ICE controlling / offerer).
    Offer {
        sid: String,
        reply: SyncSender<Result<String, String>>,
    },
    /// Browser's answer to a native offer.
    AcceptAnswer {
        sid: String,
        answer_sdp: String,
        reply: SyncSender<Result<String, String>>,
    },
    /// Trickled remote candidate.
    RemoteCandidate { sid: String, candidate: String },
    /// Tear a session down.
    Close { sid: String },
}

// ---------------------------------------------------------------------
// main
// ---------------------------------------------------------------------

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let web_root = args
        .iter()
        .position(|a| a == "--web-root")
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_else(|| "../web/dist".to_string());
    let http_port: u16 = args
        .iter()
        .position(|a| a == "--http-port")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(8088);

    str0m::crypto::from_feature_flags().install_process_default();

    // The dedicated RTC UDP socket (§6): one socket, owned by the driver.
    let host_ip = select_host_address();
    let socket = UdpSocket::bind(SocketAddr::new(host_ip, 0)).expect("bind RTC udp socket");
    let rtc_addr = socket.local_addr().expect("local addr");

    // The responder's Noise static key. The browser fetches the public
    // half over signalling, exactly as `connect_via` learns it today.
    let responder_static = StaticKeypair::generate();

    let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>();
    let (ingress_tx, ingress_rx) = sync_channel::<(String, u64, Vec<u8>)>(INGRESS_QUEUE_PACKETS);

    let shared = Arc::new(Shared {
        cmd_tx,
        peers: Mutex::new(HashMap::new()),
        results: Mutex::new(Vec::new()),
        probe: Mutex::new(ProbeStats::default()),
        notes: Mutex::new(Vec::new()),
        done: Mutex::new(false),
    });

    // ICE-TCP probe (question 1) runs before anything else so its
    // verdict is in the log even if the browser never shows up.
    let ice_tcp = probe_ice_tcp(rtc_addr);
    shared.note(ice_tcp);

    // Driver thread: owns the socket and every Rtc.
    {
        let shared = shared.clone();
        let socket = socket.try_clone().expect("clone socket");
        thread::spawn(move || driver_loop(shared, socket, rtc_addr, cmd_rx, ingress_tx));
    }

    // Mesh thread: consumes ingress, runs Noise + NetSession, submits
    // outbound packets through the bounded per-peer queue.
    {
        let shared = shared.clone();
        let kp = responder_static.clone();
        thread::spawn(move || mesh_loop(shared, kp, ingress_rx));
    }

    // Signalling + static file server.
    let listener = TcpListener::bind(("127.0.0.1", http_port)).expect("bind http");
    println!(
        "[native] http=http://127.0.0.1:{} rtc_udp={} static_pub={}",
        http_port,
        rtc_addr,
        hex(responder_static.public_key())
    );
    println!("[native] READY");

    let web_root = Arc::new(web_root);
    let static_pub = hex(responder_static.public_key());
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let shared_conn = shared.clone();
        let web_root = web_root.clone();
        let static_pub = static_pub.clone();
        thread::spawn(move || {
            let _ = handle_http(stream, &shared_conn, &web_root, &static_pub);
        });
        if *shared.done.lock() {
            break;
        }
    }
}

/// Pick a non-loopback IPv4 the browser can actually reach. Chrome will
/// not pair with a 127.0.0.1 host candidate from a remote peer.
fn select_host_address() -> IpAddr {
    let probe = UdpSocket::bind("0.0.0.0:0").expect("probe socket");
    // Not a connection: just makes the OS pick the default route's source.
    probe.connect("10.255.255.255:9").ok();
    match probe.local_addr() {
        Ok(a) if !a.ip().is_unspecified() && !a.ip().is_loopback() => a.ip(),
        _ => IpAddr::from([127, 0, 0, 1]),
    }
}

// ---------------------------------------------------------------------
// Question 1: ICE-TCP passive candidates
// ---------------------------------------------------------------------

fn probe_ice_tcp(addr: SocketAddr) -> String {
    use str0m::net::TcpType;
    let built = Candidate::builder()
        .tcp()
        .tcptype(TcpType::Passive)
        .host(addr)
        .build();
    match built {
        Ok(c) => {
            let sdp = c.to_string();
            let mut rtc = RtcConfig::new().build(Instant::now());
            let accepted = rtc.add_local_candidate(c).is_some();
            format!(
                "ICE-TCP: Candidate::builder().tcp().tcptype(Passive) constructed OK -> \"{sdp}\"; \
                 add_local_candidate accepted={accepted}"
            )
        }
        Err(e) => format!("ICE-TCP: candidate construction FAILED: {e}"),
    }
}

// ---------------------------------------------------------------------
// Driver thread
// ---------------------------------------------------------------------

struct Sess {
    rtc: Rtc,
    /// Producer end of the bounded per-peer queue, handed to the mesh
    /// side when the channel opens.
    out_tx: Option<SyncSender<Vec<u8>>>,
    pending: Option<SdpPendingOffer>,
    cid: Option<ChannelId>,
    out_rx: Receiver<Vec<u8>>,
    buffered: Arc<AtomicUsize>,
    generation: Arc<AtomicU64>,
    /// Last buffered-amount value published to the admission side.
    published: usize,
    /// A packet `Channel::write` refused (`Ok(false)`), held for retry.
    /// This is the post-acceptance retention policy §2 leaves to Stage
    /// 3; the spike implements "retain and retry" so a refusal creates
    /// real backpressure instead of a silent drop.
    retry: Option<Vec<u8>>,
    timeout: Instant,
    open: bool,
    closed: bool,
}

fn driver_loop(
    shared: Arc<Shared>,
    socket: UdpSocket,
    rtc_addr: SocketAddr,
    cmd_rx: mpsc::Receiver<Cmd>,
    ingress_tx: SyncSender<(String, u64, Vec<u8>)>,
) {
    let mut sessions: HashMap<String, Sess> = HashMap::new();
    let mut buf = vec![0u8; 2048];

    loop {
        // --- 1. control commands: each is one mutation + full drain ---
        while let Ok(cmd) = cmd_rx.try_recv() {
            handle_cmd(
                &shared,
                &mut sessions,
                rtc_addr,
                &socket,
                &ingress_tx,
                cmd,
            );
        }

        // --- 2. outbound pump: one packet per write + full drain ------
        let sids: Vec<String> = sessions.keys().cloned().collect();
        for sid in &sids {
            loop {
                let Some(s) = sessions.get_mut(sid) else { break };
                if !s.open || s.closed {
                    break;
                }
                let Some(cid) = s.cid else { break };
                let pkt = match s.retry.take() {
                    Some(p) => p,
                    None => match s.out_rx.try_recv() {
                        Ok(p) => p,
                        Err(_) => break,
                    },
                };

                // Admission probe (b): how stale was the reading we
                // published before this write?
                let actual = match s.rtc.channel(cid) {
                    Some(mut ch) => ch.buffered_amount(),
                    None => break,
                };
                {
                    let mut p = shared.probe.lock();
                    let staleness = actual.abs_diff(s.published);
                    if staleness > p.max_staleness {
                        p.max_staleness = staleness;
                    }
                    p.staleness_sum += staleness as u64;
                    p.staleness_n += 1;
                    if actual > p.max_buffered {
                        p.max_buffered = actual;
                    }
                }
                s.buffered.store(actual, Ordering::Relaxed);
                s.published = actual;

                let wrote = match s.rtc.channel(cid) {
                    Some(mut ch) => ch.write(true, &pkt),
                    None => break,
                };
                match wrote {
                    Ok(true) => {}
                    Ok(false) => {
                        {
                            let mut p = shared.probe.lock();
                            p.write_false += 1;
                            if p.write_false_at_buffered.is_none() {
                                p.write_false_at_buffered = Some(actual);
                            }
                        }
                        // Retain and retry: the refused packet stays
                        // owned by the driver and this peer's pump
                        // stops until the next loop iteration.
                        s.retry = Some(pkt);
                        drain(&shared, sid, s, &socket, &ingress_tx);
                        break;
                    }
                    Err(e) => {
                        shared.note(format!("channel.write error on {sid}: {e}"));
                        s.closed = true;
                    }
                }
                drain(&shared, sid, s, &socket, &ingress_tx);
                if s.closed {
                    break;
                }
            }
        }

        // --- 3. timeouts ----------------------------------------------
        let now = Instant::now();
        for sid in &sids {
            let due = sessions.get(sid).map(|s| s.timeout <= now).unwrap_or(false);
            if due {
                let s = sessions.get_mut(sid).unwrap();
                let _ = s.rtc.handle_input(Input::Timeout(now));
                drain(&shared, sid, s, &socket, &ingress_tx);
            }
        }

        // --- 4. reap dead sessions ------------------------------------
        sessions.retain(|sid, s| {
            let alive = s.rtc.is_alive() && !s.closed;
            if !alive {
                s.generation.fetch_add(1, Ordering::SeqCst);
                let leftover = s.out_rx.try_iter().count() + usize::from(s.retry.is_some());
                if leftover > 0 {
                    shared.probe.lock().dropped_on_close += leftover;
                    shared.note(format!(
                        "session {sid} closed with {leftover} packets still queued (discarded)"
                    ));
                }
                shared.peers.lock().remove(sid);
            }
            alive
        });

        // --- 5. socket read, bounded so the pump stays responsive ------
        let now = Instant::now();
        let next = sessions
            .values()
            .map(|s| s.timeout)
            .min()
            .unwrap_or(now + Duration::from_millis(5));
        let mut wait = next.saturating_duration_since(now);
        if wait > Duration::from_millis(5) {
            wait = Duration::from_millis(5);
        }
        if wait.is_zero() {
            wait = Duration::from_micros(500);
        }
        socket.set_read_timeout(Some(wait)).ok();
        match socket.recv_from(&mut buf) {
            Ok((n, source)) => {
                let now = Instant::now();
                let contents: &[u8] = &buf[..n];
                let Ok(contents) = contents.try_into() else {
                    continue;
                };
                let input = Input::Receive(
                    now,
                    Receive {
                        proto: Protocol::Udp,
                        source,
                        destination: rtc_addr,
                        contents,
                    },
                );
                let mut target = None;
                for (sid, s) in sessions.iter() {
                    if s.rtc.accepts(&input) {
                        target = Some(sid.clone());
                        break;
                    }
                }
                if let Some(sid) = target {
                    let s = sessions.get_mut(&sid).unwrap();
                    if let Err(e) = s.rtc.handle_input(input) {
                        shared.note(format!("handle_input error on {sid}: {e}"));
                        s.closed = true;
                    }
                    drain(&shared, &sid, s, &socket, &ingress_tx);
                }
            }
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            // Windows: after a peer goes away, an ICMP port-unreachable
            // makes the *next* `recv_from` on this UDP socket fail with
            // WSAECONNRESET (10054). It says nothing about the socket's
            // health and must not be treated as fatal — a real driver
            // has to swallow it explicitly. (Recorded in the report.)
            Err(e) if e.kind() == ErrorKind::ConnectionReset => {
                shared.probe.lock().udp_connreset += 1;
            }
            Err(e) => {
                shared.note(format!("socket error: {e}"));
            }
        }

        if *shared.done.lock() && sessions.is_empty() {
            return;
        }
    }
}

fn handle_cmd(
    shared: &Arc<Shared>,
    sessions: &mut HashMap<String, Sess>,
    rtc_addr: SocketAddr,
    socket: &UdpSocket,
    ingress_tx: &SyncSender<(String, u64, Vec<u8>)>,
    cmd: Cmd,
) {
    match cmd {
        Cmd::Answer {
            sid,
            offer_sdp,
            reply,
        } => {
            let offer = match SdpOffer::from_sdp_string(&offer_sdp) {
                Ok(o) => o,
                Err(e) => {
                    let _ = reply.send(Err(format!("bad offer: {e}")));
                    return;
                }
            };
            let mut sess = new_session(rtc_addr);
            match sess.rtc.sdp_api().accept_offer(offer) {
                Ok(answer) => {
                    drain(shared, &sid, &mut sess, socket, ingress_tx);
                    let sdp = answer.to_sdp_string();
                    sessions.insert(sid, sess);
                    let _ = reply.send(Ok(sdp));
                }
                Err(e) => {
                    let _ = reply.send(Err(format!("accept_offer: {e}")));
                }
            }
        }
        Cmd::Offer { sid, reply } => {
            let mut sess = new_session(rtc_addr);
            let mut api = sess.rtc.sdp_api();
            // §3: unordered, zero retransmits, on both sides.
            api.add_channel_with_config(ChannelConfig {
                label: "net".to_string(),
                ordered: false,
                reliability: str0m::channel::Reliability::MaxRetransmits { retransmits: 0 },
                negotiated: None,
                protocol: String::new(),
            });
            match api.apply() {
                Some((offer, pending)) => {
                    sess.pending = Some(pending);
                    drain(shared, &sid, &mut sess, socket, ingress_tx);
                    let sdp = offer.to_sdp_string();
                    sessions.insert(sid, sess);
                    let _ = reply.send(Ok(sdp));
                }
                None => {
                    let _ = reply.send(Err("no changes to apply".into()));
                }
            }
        }
        Cmd::AcceptAnswer {
            sid,
            answer_sdp,
            reply,
        } => {
            let Some(sess) = sessions.get_mut(&sid) else {
                let _ = reply.send(Err("unknown session".into()));
                return;
            };
            let answer = match SdpAnswer::from_sdp_string(&answer_sdp) {
                Ok(a) => a,
                Err(e) => {
                    let _ = reply.send(Err(format!("bad answer: {e}")));
                    return;
                }
            };
            let Some(pending) = sess.pending.take() else {
                let _ = reply.send(Err("no pending offer".into()));
                return;
            };
            match sess.rtc.sdp_api().accept_answer(pending, answer) {
                Ok(()) => {
                    drain(shared, &sid, sess, socket, ingress_tx);
                    let _ = reply.send(Ok(String::new()));
                }
                Err(e) => {
                    let _ = reply.send(Err(format!("accept_answer: {e}")));
                }
            }
        }
        Cmd::RemoteCandidate { sid, candidate } => {
            let Some(sess) = sessions.get_mut(&sid) else {
                return;
            };
            match Candidate::from_sdp_string(&candidate) {
                Ok(c) => {
                    sess.rtc.add_remote_candidate(c);
                    drain(shared, &sid, sess, socket, ingress_tx);
                }
                Err(e) => shared.note(format!("bad remote candidate: {e} ({candidate})")),
            }
        }
        Cmd::Close { sid } => {
            if let Some(sess) = sessions.get_mut(&sid) {
                let _ = sess.rtc.close();
                drain(shared, &sid, sess, socket, ingress_tx);
                sess.closed = true;
            }
        }
    }
}

fn new_session(rtc_addr: SocketAddr) -> Sess {
    let mut rtc = RtcConfig::new().build(Instant::now());
    if let Ok(c) = Candidate::host(rtc_addr, "udp") {
        rtc.add_local_candidate(c);
    }
    let (out_tx, out_rx) = sync_channel::<Vec<u8>>(SEND_QUEUE_PACKETS);
    // `out_tx` is parked in the session until the channel opens, at
    // which point it is published to the mesh side.
    Sess {
        rtc,
        out_tx: Some(out_tx),
        pending: None,
        cid: None,
        out_rx,
        buffered: Arc::new(AtomicUsize::new(0)),
        generation: Arc::new(AtomicU64::new(0)),
        published: 0,
        retry: None,
        timeout: Instant::now(),
        open: false,
        closed: false,
    }
}

/// str0m's single-mutation invariant: drain to `Output::Timeout` after
/// every mutation, before the next one.
fn drain(
    shared: &Arc<Shared>,
    sid: &str,
    s: &mut Sess,
    socket: &UdpSocket,
    ingress_tx: &SyncSender<(String, u64, Vec<u8>)>,
) {
    loop {
        match s.rtc.poll_output() {
            Ok(Output::Timeout(t)) => {
                s.timeout = t;
                return;
            }
            Ok(Output::Transmit(t)) => {
                let _ = socket.send_to(&t.contents, t.destination);
            }
            Ok(Output::Event(e)) => match e {
                Event::ChannelOpen(cid, label) => {
                    s.cid = Some(cid);
                    s.open = true;
                    if let Some(tx) = s.out_tx.take() {
                        shared.peers.lock().insert(
                            sid.to_string(),
                            PeerHandle {
                                out_tx: tx,
                                buffered: s.buffered.clone(),
                                generation: s.generation.clone(),
                            },
                        );
                    }
                    shared.note(format!("session {sid}: channel open (label={label})"));
                }
                Event::ChannelData(d) => {
                    let gen = s.generation.load(Ordering::Relaxed);
                    if ingress_tx
                        .try_send((sid.to_string(), gen, d.data.clone()))
                        .is_err()
                    {
                        shared.note(format!("session {sid}: ingress queue full, packet dropped"));
                    }
                }
                Event::ChannelClose(_) => {
                    s.generation.fetch_add(1, Ordering::SeqCst);
                    s.open = false;
                    shared.note(format!("session {sid}: channel closed"));
                }
                Event::Connected => shared.note(format!("session {sid}: ICE+DTLS connected")),
                Event::Closed => {
                    s.closed = true;
                }
                _ => {}
            },
            Err(e) => {
                shared.note(format!("poll_output error on {sid}: {e}"));
                s.closed = true;
                return;
            }
        }
    }
}

// ---------------------------------------------------------------------
// Mesh thread — the "mesh side" of the seam
// ---------------------------------------------------------------------

struct PeerWire {
    handshake: Option<NoiseHandshake>,
    session: Option<NetSession>,
    keys: Option<SessionKeys>,
    stream_id: u64,
}

fn mesh_loop(
    shared: Arc<Shared>,
    responder_static: StaticKeypair,
    ingress_rx: Receiver<(String, u64, Vec<u8>)>,
) {
    let mut peers: HashMap<String, PeerWire> = HashMap::new();
    let prologue = handshake_prologue(BROWSER_NODE_ID, NATIVE_NODE_ID);
    let peer_addr: SocketAddr = "127.0.0.1:1".parse().unwrap();

    while let Ok((sid, gen, msg)) = ingress_rx.recv() {
        if msg.is_empty() {
            continue;
        }
        let tag = msg[0];
        let body = &msg[1..];
        let entry = peers.entry(sid.clone()).or_insert_with(|| PeerWire {
            handshake: None,
            session: None,
            keys: None,
            stream_id: 0x00AA_00BB_00CC_00DD,
        });

        match tag {
            TAG_NOISE_MSG1 => {
                let mut hs = match NoiseHandshake::responder_with_prologue(
                    &PSK,
                    &responder_static,
                    &prologue,
                ) {
                    Ok(h) => h,
                    Err(e) => {
                        shared.note(format!("responder init failed: {e}"));
                        continue;
                    }
                };
                if let Err(e) = hs.read_message(body) {
                    shared.note(format!("read msg1 failed: {e}"));
                    continue;
                }
                let msg2 = match hs.write_message(&[]) {
                    Ok(m) => m,
                    Err(e) => {
                        shared.note(format!("write msg2 failed: {e}"));
                        continue;
                    }
                };
                let keys = match hs.into_session_keys() {
                    Ok(k) => k,
                    Err(e) => {
                        shared.note(format!("into_session_keys failed: {e}"));
                        continue;
                    }
                };
                entry.session = Some(NetSession::new(keys.clone(), peer_addr, 2, false));
                entry.keys = Some(keys);
                entry.handshake = None;
                let mut out = Vec::with_capacity(1 + msg2.len());
                out.push(TAG_NOISE_MSG2);
                out.extend_from_slice(&msg2);
                submit(&shared, &sid, gen, out);
                shared.note(format!("session {sid}: NKpsk0 complete (responder)"));
            }
            TAG_NET_PACKET => {
                let Some(session) = entry.session.as_ref() else {
                    shared.note(format!("session {sid}: packet before handshake"));
                    continue;
                };
                let Some(payload) = decrypt_packet(session, body) else {
                    shared.note(format!("session {sid}: packet failed to decrypt"));
                    continue;
                };
                // Echo the plaintext back inside a fresh Net packet, so
                // the browser can prove BOTH directions.
                let mut echo = Vec::with_capacity(payload.len() + 8);
                echo.extend_from_slice(b"echo:");
                echo.extend_from_slice(&payload);
                let pkt = build_packet(session, entry.stream_id, &echo);
                let mut out = Vec::with_capacity(1 + pkt.len());
                out.push(TAG_NET_PACKET);
                out.extend_from_slice(&pkt);
                submit(&shared, &sid, gen, out);
            }
            TAG_PROBE_START => {
                let Some(session) = entry.session.as_ref() else {
                    continue;
                };
                shared.note(format!("session {sid}: admission probe start"));
                let filler = vec![0xABu8; PROBE_PACKET_BYTES];
                // Keep pushing for the whole window the page's event
                // loop is blocked, retrying refusals, so the numbers
                // describe a saturated channel rather than the first
                // instant of one.
                let deadline = Instant::now() + Duration::from_millis(PROBE_WINDOW_MS);
                let mut accepted = 0usize;
                while Instant::now() < deadline && accepted < PROBE_PACKETS {
                    let pkt = build_packet(session, entry.stream_id ^ 1, &filler);
                    let mut out = Vec::with_capacity(1 + pkt.len());
                    out.push(TAG_PROBE_FILL);
                    out.extend_from_slice(&pkt);
                    if submit(&shared, &sid, gen, out) {
                        accepted += 1;
                    } else {
                        thread::sleep(Duration::from_millis(1));
                    }
                }
                let p = shared.probe.lock();
                println!(
                    "[native] probe: accepted={} admission_refusals={} write_false={} max_buffered={}",
                    p.accepted, p.admission_refusals, p.write_false, p.max_buffered
                );
            }
            _ => {}
        }
    }
}

/// The §2 submission boundary: a synchronous, total, non-blocking
/// admission decision. Returns false when the packet was refused
/// (`WouldBlock` == `Backpressure`).
fn submit(shared: &Arc<Shared>, sid: &str, gen: u64, pkt: Vec<u8>) -> bool {
    let peers = shared.peers.lock();
    let Some(h) = peers.get(sid) else {
        return false;
    };
    if h.generation.load(Ordering::SeqCst) != gen {
        return false;
    }
    // Advisory buffered-amount reading is an INPUT to admission (§2).
    let advisory = h.buffered.load(Ordering::Relaxed);
    if advisory > BUFFERED_AMOUNT_ADVISORY {
        shared.probe.lock().advisory_over += 1;
    }
    match h.out_tx.try_send(pkt) {
        Ok(()) => {
            shared.probe.lock().accepted += 1;
            true
        }
        Err(TrySendError::Full(_)) => {
            shared.probe.lock().admission_refusals += 1;
            false
        }
        Err(TrySendError::Disconnected(_)) => false,
    }
}

fn build_packet(session: &NetSession, stream_id: u64, payload: &[u8]) -> Vec<u8> {
    let seq = session.get_or_create_stream(stream_id).next_tx_seq();
    let events = [bytes::Bytes::copy_from_slice(payload)];
    let mut builder = session.thread_local_pool().get();
    builder
        .build(stream_id, seq, &events, PacketFlags::RELIABLE)
        .to_vec()
}

fn decrypt_packet(session: &NetSession, raw: &[u8]) -> Option<Vec<u8>> {
    let src: SocketAddr = "127.0.0.1:1".parse().ok()?;
    let parsed = ParsedPacket::parse(bytes::Bytes::copy_from_slice(raw), src)?;
    let aad = parsed.header.aad();
    let counter = u64::from_le_bytes(parsed.header.nonce[4..12].try_into().ok()?);
    let rx = session.rx_cipher();
    let plain = rx.decrypt_to_bytes(counter, &aad, parsed.payload.clone()).ok()?;
    if !rx.try_admit_rx_counter(counter) {
        return None;
    }
    let mut frames = EventFrame::read_events(plain, parsed.header.event_count);
    if frames.is_empty() {
        return None;
    }
    Some(frames.remove(0).to_vec())
}

// ---------------------------------------------------------------------
// Minimal HTTP: text bodies only, no JSON anywhere
// ---------------------------------------------------------------------

fn handle_http(
    mut stream: TcpStream,
    shared: &Arc<Shared>,
    web_root: &str,
    static_pub: &str,
) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    let mut raw = Vec::new();
    let mut chunk = [0u8; 4096];
    let (head_end, mut body) = loop {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            return Ok(());
        }
        raw.extend_from_slice(&chunk[..n]);
        if let Some(i) = find(&raw, b"\r\n\r\n") {
            break (i + 4, raw[i + 4..].to_vec());
        }
        if raw.len() > 1 << 20 {
            return Ok(());
        }
    };
    let head = String::from_utf8_lossy(&raw[..head_end]).to_string();
    let mut lines = head.lines();
    let req = lines.next().unwrap_or("");
    let mut parts = req.split_whitespace();
    let method = parts.next().unwrap_or("");
    let path = parts.next().unwrap_or("/");
    let content_len: usize = head
        .lines()
        .find(|l| l.to_ascii_lowercase().starts_with("content-length:"))
        .and_then(|l| l.split(':').nth(1))
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0);
    while body.len() < content_len {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    let body = String::from_utf8_lossy(&body[..content_len.min(body.len())]).to_string();

    let (status, ctype, out): (&str, &str, Vec<u8>) = match (method, path) {
        ("GET", "/config") => (
            "200 OK",
            "text/plain",
            format!(
                "psk={}\nstatic={}\nnative_id={:016x}\nbrowser_id={:016x}\n",
                hex(&PSK),
                static_pub,
                NATIVE_NODE_ID,
                BROWSER_NODE_ID
            )
            .into_bytes(),
        ),
        ("POST", p) if p.starts_with("/offer/") => {
            let sid = p.trim_start_matches("/offer/").to_string();
            let (tx, rx) = sync_channel(1);
            let _ = shared.cmd_tx.send(Cmd::Answer {
                sid,
                offer_sdp: body,
                reply: tx,
            });
            match rx.recv_timeout(Duration::from_secs(10)) {
                Ok(Ok(sdp)) => ("200 OK", "text/plain", sdp.into_bytes()),
                Ok(Err(e)) => ("500 Internal Server Error", "text/plain", e.into_bytes()),
                Err(e) => (
                    "504 Gateway Timeout",
                    "text/plain",
                    format!("{e}").into_bytes(),
                ),
            }
        }
        ("POST", p) if p.starts_with("/create-offer/") => {
            let sid = p.trim_start_matches("/create-offer/").to_string();
            let (tx, rx) = sync_channel(1);
            let _ = shared.cmd_tx.send(Cmd::Offer { sid, reply: tx });
            match rx.recv_timeout(Duration::from_secs(10)) {
                Ok(Ok(sdp)) => ("200 OK", "text/plain", sdp.into_bytes()),
                Ok(Err(e)) => ("500 Internal Server Error", "text/plain", e.into_bytes()),
                Err(e) => (
                    "504 Gateway Timeout",
                    "text/plain",
                    format!("{e}").into_bytes(),
                ),
            }
        }
        ("POST", p) if p.starts_with("/answer/") => {
            let sid = p.trim_start_matches("/answer/").to_string();
            let (tx, rx) = sync_channel(1);
            let _ = shared.cmd_tx.send(Cmd::AcceptAnswer {
                sid,
                answer_sdp: body,
                reply: tx,
            });
            match rx.recv_timeout(Duration::from_secs(10)) {
                Ok(Ok(_)) => ("200 OK", "text/plain", b"ok".to_vec()),
                Ok(Err(e)) => ("500 Internal Server Error", "text/plain", e.into_bytes()),
                Err(e) => (
                    "504 Gateway Timeout",
                    "text/plain",
                    format!("{e}").into_bytes(),
                ),
            }
        }
        ("POST", p) if p.starts_with("/candidate/") => {
            let sid = p.trim_start_matches("/candidate/").to_string();
            let _ = shared.cmd_tx.send(Cmd::RemoteCandidate {
                sid,
                candidate: body.trim().to_string(),
            });
            ("200 OK", "text/plain", b"ok".to_vec())
        }
        ("POST", p) if p.starts_with("/close/") => {
            let sid = p.trim_start_matches("/close/").to_string();
            let _ = shared.cmd_tx.send(Cmd::Close { sid });
            ("200 OK", "text/plain", b"ok".to_vec())
        }
        ("POST", "/result") => {
            for line in body.lines() {
                if line.trim().is_empty() {
                    continue;
                }
                println!("[browser] {}", line.trim());
                shared.results.lock().push(line.trim().to_string());
            }
            ("200 OK", "text/plain", b"ok".to_vec())
        }
        ("GET", "/probe-stats") => {
            let p = shared.probe.lock();
            let mean = if p.staleness_n > 0 {
                p.staleness_sum / p.staleness_n
            } else {
                0
            };
            (
                "200 OK",
                "text/plain",
                format!(
                    "accepted={}\nadmission_refusals={}\nwrite_false={}\nwrite_false_at_buffered={}\n\
                     max_buffered={}\nmax_staleness={}\nmean_staleness={}\nadvisory_over={}\n\
                     dropped_on_close={}\nsend_queue_packets={}\nbuffered_amount_advisory={}\n\
                     udp_connreset={}\n",
                    p.accepted,
                    p.admission_refusals,
                    p.write_false,
                    p.write_false_at_buffered
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "none".into()),
                    p.max_buffered,
                    p.max_staleness,
                    mean,
                    p.advisory_over,
                    p.dropped_on_close,
                    SEND_QUEUE_PACKETS,
                    BUFFERED_AMOUNT_ADVISORY,
                    p.udp_connreset,
                )
                .into_bytes(),
            )
        }
        ("POST", "/done") => {
            *shared.done.lock() = true;
            let results = shared.results.lock().clone();
            println!("[native] DONE with {} result lines", results.len());
            let shared2 = shared.clone();
            thread::spawn(move || {
                thread::sleep(Duration::from_millis(300));
                let p = shared2.probe.lock();
                let mean = if p.staleness_n > 0 {
                    p.staleness_sum / p.staleness_n
                } else {
                    0
                };
                println!(
                    "[native] PROBE accepted={} admission_refusals={} write_false={} \
                     write_false_at_buffered={:?} max_buffered={} max_staleness={} \
                     mean_staleness={} advisory_over={} dropped_on_close={}",
                    p.accepted,
                    p.admission_refusals,
                    p.write_false,
                    p.write_false_at_buffered,
                    p.max_buffered,
                    p.max_staleness,
                    mean,
                    p.advisory_over,
                    p.dropped_on_close
                );
                println!("[native] EXIT");
                std::process::exit(0);
            });
            ("200 OK", "text/plain", b"ok".to_vec())
        }
        ("GET", p) => {
            let rel = if p == "/" { "/index.html" } else { p };
            let rel = rel.split('?').next().unwrap_or(rel);
            let full = format!("{web_root}{rel}");
            match std::fs::read(&full) {
                Ok(data) => ("200 OK", content_type(rel), data),
                Err(_) => ("404 Not Found", "text/plain", b"not found".to_vec()),
            }
        }
        _ => ("404 Not Found", "text/plain", b"not found".to_vec()),
    };

    let header = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\n\
         Access-Control-Allow-Origin: *\r\nConnection: close\r\n\r\n",
        out.len()
    );
    stream.write_all(header.as_bytes())?;
    stream.write_all(&out)?;
    stream.flush()
}

fn content_type(path: &str) -> &'static str {
    if path.ends_with(".html") {
        "text/html"
    } else if path.ends_with(".js") {
        "text/javascript"
    } else if path.ends_with(".wasm") {
        "application/wasm"
    } else {
        "application/octet-stream"
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
