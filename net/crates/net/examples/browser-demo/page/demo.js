// The demo page: one leaf per browsing context, positions at 60 Hz
// over a FIRE-AND-FORGET stream addressed at the peer, and the
// ANCHOR's own per-pair forwarding counter on screen while it
// happens.
//
// WHAT THIS PAGE IS ALLOWED TO USE
// --------------------------------
// `@net-mesh/browser` and nothing else. No `node.inner`, no
// `#[wasm_bindgen]` method called directly — the demo is the
// readability test for the public API, so anything it cannot express
// through `connect` / `announce` / `query` / `connectPeer` /
// `acceptPeer` / `openStream` is a finding about the package rather
// than something to reach around. (It found one: before slice 6,
// `openStream` could not address a peer at all.)
//
// THE CLAIM ON SCREEN
// -------------------
// `forwarded_app_packets` is the anchor's count of packets it
// FORWARDED for this pair, and it EXCLUDES `0x0D02` signalling. That
// exclusion is the whole point: "flat once direct" would be
// meaningless if the counter could also go flat because signalling
// stopped, or because nothing was happening at all. So the HUD shows
// three numbers together — the pair counter (flat), the positions
// actually arriving at the other tab (moving), and the announcement
// tick the anchor keeps resolving for this leaf (moving). Flat, with
// the other two moving, is a direct path.

import { connect } from '/browser/index.js';
import * as THREE from '/vendor/three.module.js';

// ---------------------------------------------------------------------
// state
// ---------------------------------------------------------------------

const params = new URLSearchParams(location.search);
const TAB = params.get('tab') === 'b' ? 'b' : 'a';

const state = {
  tab: TAB,
  role: null,
  phase: 'booting',
  nodeId: null,
  peerId: null,
  outcome: null,
  dialog: null,
  /** Positions handed to `stream.send`. */
  sent: 0,
  /** Positions the peer's stream delivered here. */
  received: 0,
  /** Highest sequence seen, so fire-and-forget gaps are visible. */
  peerSeq: -1,
  lost: 0,
  /** Sends in the last second, and over the whole direct phase. */
  sendHz: 0,
  recvHz: 0,
  avgSendHz: 0,
  /** Milliseconds the 60 Hz loop has been running. */
  runMs: 0,
  /** Worst gap between two consecutive sends, ms. */
  worstSendGapMs: 0,
  /** How long the anchor took to admit BOTH leaves, ms. */
  gateWaitMs: 0,
  announceTick: 0,
  announceOk: 0,
  announceFailed: 0,
  /** Whether the leaf says the SESSION with the peer is direct. */
  direct: false,
  /**
   * Whether the routed phase's stream handle was refused after the
   * direct install, what it cost to re-open, and the typed refusal
   * verbatim. `reopenFrames` is that cost in 60 Hz frame slots.
   */
  reopened: false,
  reopenMs: 0,
  reopenFrames: 0,
  reopenRefusal: null,
  streamId: null,
  webgl: false,
  glFrames: 0,
  error: null,
};

globalThis.__demo = { state: () => ({ ...state }) };

const el = (id) => document.getElementById(id);

function fail(where, error) {
  state.error = `${where}: ${error && error.message ? error.message : String(error)}`;
  state.phase = 'failed';
  el('error').textContent = state.error;
  log(`FAILED ${state.error}`);
}

function log(line) {
  fetch('/log', { method: 'POST', body: `[${TAB}] ${line}` }).catch(() => {});
}

// ---------------------------------------------------------------------
// the scene
// ---------------------------------------------------------------------
//
// Two cubes: this tab's, driven by a local Lissajous path, and the
// peer's, driven ONLY by payloads that arrived over the stream. If
// the transport stops, the peer's cube stops — which is why the
// render is worth having next to the counters.

let renderer = null;
let scene = null;
let camera = null;
let mine = null;
let theirs = null;
let trail = null;

function buildScene() {
  const canvas = el('scene');
  renderer = new THREE.WebGLRenderer({ canvas, antialias: true });
  renderer.setPixelRatio(Math.min(globalThis.devicePixelRatio || 1, 2));
  scene = new THREE.Scene();
  scene.background = new THREE.Color(0x0b0d12);
  camera = new THREE.PerspectiveCamera(50, 1, 0.1, 100);
  camera.position.set(0, 2.4, 7.5);
  camera.lookAt(0, 0, 0);

  scene.add(new THREE.AmbientLight(0xffffff, 0.45));
  const key = new THREE.DirectionalLight(0xffffff, 1.1);
  key.position.set(3, 5, 4);
  scene.add(key);

  const geometry = new THREE.BoxGeometry(0.9, 0.9, 0.9);
  mine = new THREE.Mesh(geometry, new THREE.MeshStandardMaterial({ color: 0x4f8cff }));
  theirs = new THREE.Mesh(geometry, new THREE.MeshStandardMaterial({ color: 0x46d17a }));
  scene.add(mine, theirs);

  const grid = new THREE.GridHelper(12, 12, 0x1b2030, 0x141824);
  grid.position.y = -2.2;
  scene.add(grid);

  // The peer's path, as it was actually received: a visible gap is a
  // fire-and-forget drop, not a rendering artefact.
  const positions = new Float32Array(3 * 240);
  const geo = new THREE.BufferGeometry();
  geo.setAttribute('position', new THREE.BufferAttribute(positions, 3));
  geo.setDrawRange(0, 0);
  trail = new THREE.Line(geo, new THREE.LineBasicMaterial({ color: 0x2b6b46 }));
  scene.add(trail);

  resize();
  globalThis.addEventListener('resize', resize);
  state.webgl = true;
}

function resize() {
  if (!renderer) return;
  const w = globalThis.innerWidth;
  const h = globalThis.innerHeight;
  renderer.setSize(w, h, false);
  camera.aspect = w / Math.max(h, 1);
  camera.updateProjectionMatrix();
}

let trailCount = 0;

function pushTrail(x, y, z) {
  const attr = trail.geometry.getAttribute('position');
  if (trailCount >= attr.count) {
    // Slide the window by one, so the trail is the last N positions.
    attr.array.copyWithin(0, 3);
    trailCount = attr.count - 1;
  }
  attr.array[trailCount * 3] = x;
  attr.array[trailCount * 3 + 1] = y;
  attr.array[trailCount * 3 + 2] = z;
  trailCount += 1;
  attr.needsUpdate = true;
  trail.geometry.setDrawRange(0, trailCount);
}

function animate() {
  if (renderer) {
    mine.rotation.x += 0.01;
    mine.rotation.y += 0.013;
    theirs.rotation.x -= 0.008;
    theirs.rotation.y += 0.011;
    renderer.render(scene, camera);
    state.glFrames += 1;
  }
  requestAnimationFrame(animate);
}

/** This tab's position at time `t` — a path, so the motion is real. */
function localPosition(t) {
  const phase = TAB === 'a' ? 0 : Math.PI / 2;
  const side = TAB === 'a' ? -1.6 : 1.6;
  return [
    side + Math.sin(t * 0.0011 + phase) * 1.1,
    Math.sin(t * 0.0017 + phase) * 1.3,
    Math.cos(t * 0.0013 + phase) * 1.1,
  ];
}

// ---------------------------------------------------------------------
// the position frame
// ---------------------------------------------------------------------
//
// 24 bytes: a sequence the receiver can see gaps in, the position, and
// the sender's clock so a one-way delay is visible in the log.

const FRAME_BYTES = 24;

function encodeFrame(seq, x, y, z) {
  const buffer = new ArrayBuffer(FRAME_BYTES);
  const view = new DataView(buffer);
  view.setUint32(0, seq, true);
  view.setFloat32(4, x, true);
  view.setFloat32(8, y, true);
  view.setFloat32(12, z, true);
  view.setFloat64(16, performance.now(), true);
  return new Uint8Array(buffer);
}

function decodeFrame(payload) {
  if (payload.byteLength < FRAME_BYTES) return null;
  const view = new DataView(payload.buffer, payload.byteOffset, payload.byteLength);
  return {
    seq: view.getUint32(0, true),
    x: view.getFloat32(4, true),
    y: view.getFloat32(8, true),
    z: view.getFloat32(12, true),
    t: view.getFloat64(16, true),
  };
}

// ---------------------------------------------------------------------
// ids
// ---------------------------------------------------------------------
//
// A `NodeDescriptor.nodeId` is an exact DECIMAL string — `JSON.parse`
// rounds above 2^53, so the wrapper never hands a page a number. The
// peer methods and `openStream({ peer })` take 16 hex digits. This is
// the one conversion, in one place.

function decimalToHex16(decimal) {
  return BigInt(decimal).toString(16).padStart(16, '0');
}

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

// ---------------------------------------------------------------------
// the run
// ---------------------------------------------------------------------

let cfg = null;
let node = null;
let stream = null;
let running = false;

async function main() {
  try {
    buildScene();
  } catch (error) {
    // A page with no WebGL still exercises the transport, but the
    // demo says so loudly rather than showing a black canvas.
    state.error = `three.js: ${error.message}`;
    el('error').textContent = state.error;
  }
  animate();

  cfg = await (await fetch(`/config?tab=${TAB}`)).json();
  state.role = cfg.role;
  state.streamId = cfg.streamId;
  el('tab').textContent = `${TAB} (${cfg.role})`;
  setInterval(refreshHud, cfg.reportMs);

  state.phase = 'connecting';
  node = await connect({
    credentialB64: cfg.credentialB64,
    bootstrapUrl: cfg.bootstrapUrl,
  });
  state.nodeId = node.nodeIdHex();
  log(`connected: node ${state.nodeId}, anchor ${node.anchorIdHex()}`);

  // Report the node id NOW, before the first announcement.
  //
  // This is what makes the host's baseline provably pre-pair rather
  // than merely early: the host reads the pair counter as soon as it
  // knows both ids, and the peer cannot have offered to this leaf
  // before this leaf announced — which is the next statement. So the
  // ordering is announce-after-report on both sides, and a baseline
  // taken then cannot contain pair traffic. The host asserts it is
  // zero, so a regression in this ordering fails the row instead of
  // quietly weakening it.
  await postReport();

  // Announcements start BEFORE discovery and never stop: they are
  // how the peer learns this leaf's Noise key, and — once the pair is
  // direct — they are the anchor-side proof that this leaf is still
  // talking to the anchor while the pair counter stays flat.
  announceForever();

  state.phase = 'discovering';
  const peerHex = await discoverPeer();
  state.peerId = peerHex;
  log(`discovered peer ${peerHex}`);

  // BOTH leaves must be ADMITTED on the anchor before either offers.
  //
  // Discovery is not reachability. A provisional leaf still floods
  // announcements, so each tab can discover the other while the
  // anchor is still refusing its application transit under §12's
  // admission rule — and the relayed Noise handshake that carries
  // the offer is exactly that transit. `connect()` awaits its own
  // enrollment reply, so this page believes it is enrolled; what it
  // cannot see is whether the ANCHOR has promoted the other tab's
  // session yet. That is what `/gate` answers, read on the anchor.
  //
  // This closes a window rather than widening one: `acceptPeer`'s
  // 5 s offer wait and the leaf's 5 s relayed-handshake deadline are
  // library surface and are not touched.
  state.phase = 'waiting for admission';
  state.gateWaitMs = await waitForAdmission();
  log(`both leaves admitted on the anchor after ${state.gateWaitMs.toFixed(0)} ms`);

  // The attempt is started but NOT awaited: the routed phase only
  // exists between the offer and the direct install, so a page that
  // wants to put application bytes on the anchor's forwarding path
  // has to do it while the attempt is in flight.
  state.phase = cfg.role === 'offerer' ? 'dialling' : 'accepting';
  const attempt = cfg.role === 'offerer' ? node.connectPeer(peerHex) : node.acceptPeer(peerHex);

  await routedBurst(peerHex);

  const outcome = await attempt;
  state.outcome = outcome.type;
  state.dialog = outcome.dialog ?? null;
  log(`attempt outcome: ${JSON.stringify(outcome)}`);
  if (outcome.type !== 'direct') {
    state.phase = 'failed';
    state.error = `the peer attempt ended ${outcome.type}`;
    el('error').textContent = state.error;
    return;
  }
  state.direct = true;
  state.phase = 'direct';

  await openPositionStream(peerHex);
  await pump();
}

/** Re-announce on a timer, with a tag that moves every tick. */
function announceForever() {
  const tags = () => [cfg.peerTag, `${cfg.tickTag}.${state.announceTick}`];
  const once = async () => {
    try {
      await node.announce(tags());
      state.announceOk += 1;
      state.announceTick += 1;
    } catch (error) {
      state.announceFailed += 1;
      log(`announce failed: ${error.message}`);
    }
  };
  void once();
  setInterval(() => void once(), cfg.announceMs);
}

/**
 * Query the capability the other tab announces until it is there.
 *
 * Discovery is the only way this page learns the peer: no id, key or
 * SDP is passed in by the host. `connect` gave it a credential; the
 * peer's Noise static comes from the peer's own signed announcement,
 * which is exactly §9 step 1.
 */
async function discoverPeer() {
  const deadline = performance.now() + cfg.discoveryMs;
  while (performance.now() < deadline) {
    const peers = await node.query(cfg.peerTag);
    for (const peer of peers) {
      const hex = decimalToHex16(peer.nodeId);
      if (hex !== state.nodeId) return hex;
    }
    await sleep(200);
  }
  throw new Error(`no peer announced ${cfg.peerTag} within ${cfg.discoveryMs} ms`);
}

/**
 * Wait until the ANCHOR reports both leaves admitted, and return how
 * long that took.
 *
 * The wait is on the anchor's own `peer_is_provisional`, not on this
 * page's belief about itself — a page cannot see the other tab's
 * admission state, and the offer needs BOTH. Returns the measured
 * wait so the demo reports the size of the window it closed instead
 * of assuming it was zero.
 */
async function waitForAdmission() {
  const started = performance.now();
  const deadline = started + cfg.admissionMs;
  for (;;) {
    const gate = await (await fetch('/gate')).json();
    if (gate.ready) return performance.now() - started;
    if (performance.now() >= deadline) {
      throw new Error(
        `the pair was not ready to offer after ${cfg.admissionMs} ms ` +
          `(provisional a=${gate.provisionalA}, b=${gate.provisionalB}; ` +
          `discovered a=${gate.discoveredA}, b=${gate.discoveredB}; ` +
          `provisional=${gate.provisionalCount}, ` +
          `transit refused=${gate.admissionRefusedTransit})`,
      );
    }
    await sleep(100);
  }
}

/**
 * Open the position stream, addressed at the PEER.
 *
 * `peer` is what slice 6 needed and the package did not have: before
 * it, `openStream` could only address the anchor, so a page could
 * establish a direct session and then had nothing to put on it.
 */
async function openStreamToPeer(peerHex) {
  return node.openStream({
    reliability: 'fireAndForget',
    peer: peerHex,
    streamId: cfg.streamId,
    label: `positions-${TAB}`,
  });
}

/**
 * Application data over the ROUTED session, before the direct one
 * exists — the phase that makes the anchor's per-pair counter MOVE.
 *
 * Fire-and-forget on purpose, and short on purpose. These frames are
 * the "moving" half of the demo's claim: a counter that had never
 * moved would prove nothing by being flat. It stops well before the
 * ICE deadline so the direct install is never racing live traffic.
 */
async function routedBurst(peerHex) {
  const deadline = performance.now() + cfg.routedWaitMs;
  while (performance.now() < deadline) {
    try {
      stream = await openStreamToPeer(peerHex);
    } catch (error) {
      // `session: no session with 0x…` — the routed session is not
      // up yet. Anything else is a real failure.
      if (error.kind !== 'session') throw error;
      await sleep(25);
      continue;
    }
    break;
  }
  if (!stream) {
    log(`no session with the peer inside ${cfg.routedWaitMs} ms; skipping the routed burst`);
    return;
  }
  attachReceiver();
  state.phase = 'routed';
  for (let i = 0; i < cfg.routedBurst; i += 1) {
    const [x, y, z] = localPosition(performance.now());
    try {
      await stream.send(encodeFrame(state.sent, x, y, z));
      state.sent += 1;
    } catch (error) {
      log(`routed send failed: ${error.message}`);
      break;
    }
    await sleep(cfg.routedGapMs);
  }
  log(`routed burst done: ${state.sent} frames`);
}

/**
 * The stream for the direct phase, and what the upgrade costs.
 *
 * `openStream`'s ADDRESSING survives the upgrade — same peer, same
 * stream id, and the leaf's `route_outbound` decides routed vs direct
 * — but the HANDLE does not: `LeafNode::check_handle` fences a
 * `StreamHandle` to the incarnation it was opened on, and §9 step 4
 * installs a REPLACEMENT session, so the routed phase's handle is
 * refused with `stale stream handle: … reopen the stream`. That is
 * the fence working, not a defect.
 *
 * So the interesting number is not *whether* it reopens but what the
 * reopen costs a 60 Hz sender. This sends one frame on the existing
 * handle, times the refusal-plus-reopen if it comes, and records it
 * both in milliseconds and in 60 Hz frame slots — which is the unit a
 * page author actually budgets in.
 */
async function openPositionStream(peerHex) {
  if (stream) {
    const started = performance.now();
    try {
      const [x, y, z] = localPosition(started);
      await stream.send(encodeFrame(state.sent, x, y, z));
      state.sent += 1;
      return;
    } catch (error) {
      state.reopened = true;
      // `.message` is verbatim the Rust `Display` text and already
      // begins with the kind, so the kind is not prefixed again.
      state.reopenRefusal = error.message;
      stream = await openStreamToPeer(peerHex);
      attachReceiver();
      state.reopenMs = performance.now() - started;
      state.reopenFrames = state.reopenMs / (1000 / cfg.hz);
      log(
        `the routed handle was refused after the direct install (${state.reopenRefusal}); ` +
          `re-opened in ${state.reopenMs.toFixed(2)} ms = ${state.reopenFrames.toFixed(2)} ` +
          `frame slots at ${cfg.hz} Hz`,
      );
      return;
    }
  }
  stream = await openStreamToPeer(peerHex);
  attachReceiver();
}

let receiverAttached = false;

function attachReceiver() {
  if (receiverAttached || !stream) return;
  receiverAttached = true;
  stream.onMessage((payload) => {
    const frame = decodeFrame(payload);
    if (!frame) return;
    state.received += 1;
    recvWindow.push(performance.now());
    if (frame.seq > state.peerSeq) {
      state.peerSeq = frame.seq;
      state.lost = state.peerSeq + 1 - state.received;
    }
    if (theirs) {
      theirs.position.set(frame.x, frame.y, frame.z);
      pushTrail(frame.x, frame.y, frame.z);
    }
  });
}

// Rolling one-second windows, so the HUD shows the rate now rather
// than the average since boot.
const sendWindow = [];
const recvWindow = [];

function rate(window, now) {
  while (window.length && now - window[0] > 1000) window.shift();
  return window.length;
}

/**
 * The 60 Hz loop.
 *
 * A self-correcting schedule rather than `setInterval(16)`: the next
 * deadline advances by exactly one period per frame, so a late wake
 * does not push the whole series late. After a stall longer than five
 * periods it resynchronises instead of trying to catch up — a demo
 * that machine-guns 300 frames to make an average look right is
 * lying about its rate.
 */
async function pump() {
  running = true;
  const period = 1000 / cfg.hz;
  const started = performance.now();
  // The routed burst's frames and the one probe frame that found the
  // stale handle are NOT part of this measurement: counting them
  // against the loop's own elapsed time reported a rate ~5 % above
  // the target, which is exactly the kind of flattering arithmetic
  // this row exists to refuse.
  const base = state.sent;
  let next = started;
  let last = started;
  while (running) {
    const now = performance.now();
    if (now + 0.4 < next) {
      await sleep(Math.max(0, next - now - 0.4));
      continue;
    }
    next += period;
    if (next < now - period * 5) next = now + period;

    const [x, y, z] = localPosition(now);
    if (mine) mine.position.set(x, y, z);
    try {
      await stream.send(encodeFrame(state.sent, x, y, z));
    } catch (error) {
      fail('position send', error);
      return;
    }
    state.sent += 1;
    sendWindow.push(now);
    const gap = now - last;
    if (state.sent - base > 2 && gap > state.worstSendGapMs) state.worstSendGapMs = gap;
    last = now;
    state.runMs = now - started;
    state.sendHz = rate(sendWindow, now);
    state.recvHz = rate(recvWindow, now);
    // N frames span N-1 intervals, so the divisor is the elapsed
    // time from the FIRST of them, not from the loop's entry.
    const pumped = state.sent - base;
    state.avgSendHz = pumped > 1 ? ((pumped - 1) / (now - started)) * 1000 : 0;
  }
}

// ---------------------------------------------------------------------
// the HUD, and the report the host asserts on
// ---------------------------------------------------------------------

async function refreshHud() {
  el('phase').textContent = state.phase;
  el('me').textContent = state.nodeId ?? '—';
  el('peer').textContent = state.peerId ?? '—';
  el('frames').textContent = `${state.sent} / ${state.received}${
    state.lost > 0 ? ` (${state.lost} dropped)` : ''
  }`;
  el('rate').textContent = `${state.sendHz} Hz / ${state.recvHz} Hz`;
  el('direct').textContent = state.direct
    ? 'DIRECT — leaf ↔ leaf, no anchor in the path'
    : `routed through the anchor${state.outcome ? ` (${state.outcome})` : ''}`;
  el('direct').className = state.direct ? 'flat' : 'moving';
  el('tick').textContent = `${state.announceTick} sent`;
  el('gl').textContent = state.webgl ? `${state.glFrames}` : 'no WebGL context';

  let pair = null;
  try {
    pair = await (await fetch('/pair')).json();
  } catch {
    /* the host is gone; the HUD keeps the last values */
  }
  if (pair && pair.ready) {
    el('pair').textContent = `${pair.ab} → · ← ${pair.ba}`;
    el('pair').className = `big ${pair.flatMs > 1500 ? 'flat' : 'moving'}`;
    el('flat').textContent =
      pair.flatMs > 0 ? `${(pair.flatMs / 1000).toFixed(1)} s` : 'moving now';
    el('signal').textContent = `${pair.signalForwarded} (excluded from the counter above)`;
    el('tick').textContent = `${pair.tickA ?? '—'} / ${pair.tickB ?? '—'} (this leaf sent ${state.announceTick})`;
    // The on-screen sentence is held to the same rule as the host's
    // verdict strings: it may only claim what the numbers beside it
    // establish. "Flat" alone is also what a dropped stream looks
    // like, so the flat claim is gated on positions STILL ARRIVING
    // and on a fresh announcement tick — and when either stops, the
    // HUD says so instead of narrating the happy path.
    const arriving = state.recvHz > 0;
    el('note').textContent = !state.direct
      ? 'The pair counter MOVES while the anchor carries this pair.'
      : arriving && pair.flatMs > 1500
        ? 'The pair counter is FLAT while positions keep arriving and the anchor keeps ' +
          'resolving fresh announcements: the bytes are going leaf → leaf.'
        : arriving
          ? 'Direct, and the counter has just moved — read it again in a second.'
          : 'Direct, but NOTHING is arriving: a flat counter proves nothing here.';
  }

  await postReport();
}

/** The state the host reads its two page-side facts from. */
async function postReport() {
  await fetch('/report', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(state),
  }).catch(() => {});
}

main().catch((error) => fail('demo', error));
