// Stage 4b Chromium harness — the browser half.
//
// This page owns the TRANSPORT and nothing above it:
//
//   POST https://<anchor>/rtc/offer   (the real Stage 4b listener)
//   wss  https://<anchor>/rtc/trickle (candidates both ways)
//   RTCDataChannel "net", ordered:true, reliable (the leaf's own)
//   NKpsk0 as initiator, against the key the CREDENTIAL pins
//   every packet built and sealed by net-mesh-wire compiled to wasm
//
// The payload bytes it sends are built on the native side with the
// production encoders and handed over by the runner — the browser is
// not an nRPC client (that is Stage 5). See the runner's module doc.

import init, { LeafEndpoint } from './leaf.js';

const logEl = document.getElementById('log');
const sessions = new Map();

function log(line) {
  console.log(line);
  logEl.textContent += line + '\n';
  return fetch('/harness/log', { method: 'POST', body: line + '\n' }).catch(() => {});
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

function withTimeout(promise, ms, what) {
  let t;
  return Promise.race([
    promise.finally(() => clearTimeout(t)),
    new Promise((_, rej) => {
      t = setTimeout(() => rej(new Error('timeout: ' + what)), ms);
    }),
  ]);
}

function hex(bytes) {
  return Array.from(bytes, (b) => b.toString(16).padStart(2, '0')).join('');
}

function unhex(s) {
  const out = new Uint8Array(s.length / 2);
  for (let i = 0; i < out.length; i++) out[i] = parseInt(s.substr(i * 2, 2), 16);
  return out;
}

function concat(a, b) {
  const out = new Uint8Array(a.length + b.length);
  out.set(a, 0);
  out.set(b, a.length);
  return out;
}

/// One in-order queue per DataChannel.
function messageQueue(dc) {
  const q = [];
  const waiters = [];
  dc.binaryType = 'arraybuffer';
  dc.onmessage = (ev) => {
    const buf = new Uint8Array(ev.data);
    if (waiters.length) waiters.shift()(buf);
    else q.push(buf);
  };
  return {
    next(timeoutMs, what) {
      if (q.length) return Promise.resolve(q.shift());
      return withTimeout(new Promise((res) => waiters.push(res)), timeoutMs, what);
    },
  };
}

function waitOpen(dc) {
  if (dc.readyState === 'open') return Promise.resolve();
  return new Promise((res, rej) => {
    dc.onopen = () => res();
    dc.onerror = (e) => rej(new Error('datachannel error: ' + (e.message || 'unknown')));
  });
}

/// One candidate, reduced to the facts the mDNS verdict needs:
/// its TYPE (`host` / `srflx` / `prflx` / `relay`), whether the
/// address is an obfuscated `.local` name, and when it appeared.
///
/// `RTCIceCandidate` exposes `type`/`address`/`port`/`protocol`
/// directly; the SDP line is parsed only for the fields an engine
/// leaves null. Accepts either an `RTCIceCandidate` or a bare
/// candidate line (the anchor's, which arrives as a string).
function describeCandidate(candidate, ms) {
  const line = typeof candidate === 'string' ? candidate : candidate.candidate || '';
  const tok = line.split(' ');
  const typ = tok.indexOf('typ');
  const obj = typeof candidate === 'string' ? {} : candidate;
  const address = obj.address || (typ > 0 ? tok[typ - 2] : null);
  return {
    type: obj.type || (typ > 0 ? tok[typ + 1] : null),
    address,
    port: obj.port || (typ > 0 ? Number(tok[typ - 1]) : null),
    protocol: obj.protocol || tok[2] || null,
    mdns: !!address && address.endsWith('.local'),
    ms,
  };
}

/// The selected candidate pair with both halves resolved, or
/// `null` when `getStats()` reports none.
///
/// ONE implementation, used by the `stats` step and by the
/// snapshot a FAILED attempt takes before it settles its handles.
/// A pair that formed and then lost its session is still evidence
/// about ICE, and discarding it is what made a `noise msg2`
/// timeout read as "NO candidate pair formed" in the mDNS verdict.
async function selectedPair(pc) {
  if (!pc) return null;
  let report;
  try {
    report = await pc.getStats();
  } catch (e) {
    return null;
  }
  const byId = new Map();
  report.forEach((r) => byId.set(r.id, r));
  let pair = null;
  report.forEach((r) => {
    if (r.type === 'candidate-pair' && (r.selected || r.nominated || r.state === 'succeeded')) {
      if (!pair || r.state === 'succeeded') pair = r;
    }
  });
  if (!pair) return null;
  const local = byId.get(pair.localCandidateId) || {};
  const remote = byId.get(pair.remoteCandidateId) || {};
  return {
    state: pair.state,
    nominated: !!pair.nominated,
    local: {
      type: local.candidateType,
      protocol: local.protocol,
      address: local.address,
      port: local.port,
      relatedAddress: local.relatedAddress,
    },
    remote: {
      type: remote.candidateType,
      protocol: remote.protocol,
      address: remote.address,
      port: remote.port,
    },
  };
}

/// Close every handle one attempt owns: the DataChannel, the
/// trickle socket and the `RTCPeerConnection`.
///
/// **R7: a FAILED attempt has to settle its own handles.** A
/// `doConnect` that threw used to leave all three open, so the
/// ANCHOR's attempt stayed alive under its own ICE deadline (90 s
/// in this harness) while the page had already given up after 15.
/// Sampling "the anchor installed nothing" in that window is a
/// reading taken while the attempt is still running and still
/// owns work. A trickle socket that is still LIVE is the handle
/// the listener reads as "the browser went away" when it closes:
/// that ends the bootstrap dialog, removing the dialog table
/// entry, closing the RTC endpoint behind it and releasing the
/// signalling budget. (On a fast local host the upgrade is often
/// already refused by the time the browser asks — the
/// `[trickle …]` lines say which — so the DataChannel and the
/// PeerConnection are the handles that actually still hold
/// anything open, and all three are closed here regardless.)
function settleHandles(label, pc, dc, ws) {
  const state = [
    'dc=' + (dc ? dc.readyState : '<none>'),
    'ws=' + (ws ? ws.readyState : '<none>'),
    'pc=' + (pc ? pc.connectionState : '<none>'),
  ].join(' ');
  log('[page] settling the handles of ' + label + ' (' + state + ')');
  try {
    if (dc) dc.close();
  } catch (e) {
    log('[page] dc.close: ' + e);
  }
  try {
    if (ws) ws.close();
  } catch (e) {
    log('[page] ws.close: ' + e);
  }
  try {
    if (pc) pc.close();
  } catch (e) {
    log('[page] pc.close: ' + e);
  }
}

/// The handles the attempt currently being built owns. `doConnect`
/// publishes each one as it is created and [`connectAttempt`]
/// retires them when the attempt fails, which is the whole point:
/// the failure can arrive at any one of a dozen awaits, and the
/// handles have to be settled at every one of them.
const inflight = { pc: null, dc: null, ws: null };

/// `doConnect`, with a FAILED attempt's handles settled.
///
/// A successful attempt hands its handles to `sessions` and keeps
/// them; nothing here touches those. A failed one owns an accepted
/// offer, an open DataChannel and a live trickle socket on the
/// anchor it reached, and the anchor's attempt lives until its own
/// ICE deadline unless the browser goes away — so the attempt is
/// retired here, at the browser end, before the runner is told the
/// attempt failed.
async function connectAttempt(step, stage) {
  inflight.pc = null;
  inflight.dc = null;
  inflight.ws = null;
  try {
    const r = await doConnect(step, stage);
    inflight.pc = null;
    inflight.dc = null;
    inflight.ws = null;
    return r;
  } catch (e) {
    // **The pair is read BEFORE the handles are settled.** Closing
    // the `RTCPeerConnection` empties `getStats()`, so an attempt
    // that failed downstream of ICE — a lost Noise msg1, say —
    // used to report nothing about the pair it had already formed.
    if (stage) stage.selected = await selectedPair(inflight.pc);
    settleHandles(step.session, inflight.pc, inflight.dc, inflight.ws);
    inflight.pc = null;
    inflight.dc = null;
    inflight.ws = null;
    throw e;
  }
}

// ---------------------------------------------------------------------
// connect: offer -> trickle -> DataChannel -> NKpsk0
// ---------------------------------------------------------------------

/// `stage` is filled in as the attempt advances, so a caller that
/// EXPECTS this to fail can say WHERE it failed instead of treating
/// every exception as the failure it was hoping for. Each field is
/// set only once the step it names has actually happened.
async function doConnect(step, stage) {
  const st = stage || {};
  const t0 = performance.now();
  const iceServers = step.stun ? [{ urls: step.stun }] : [];
  const pc = new RTCPeerConnection({ iceServers });
  // **The channel the leaf ships.** `RtcLeafTransport::create_offer`
  // sets `ordered: true` and leaves the channel RELIABLE, and says
  // why: Net's own reliability rides inside, and reorder at the
  // packet level is what the AEAD replay window refuses. This page
  // used `{ ordered: false, maxRetransmits: 0 }` — a channel that
  // NEVER retransmits — and then sent the single Noise msg1
  // datagram on it. One lost packet on a loaded runner was
  // therefore unrecoverable, and surfaced as `timeout: noise
  // msg2`, i.e. as the anchor's fault. The witness has to model the
  // transport that ships, not a lossier one.
  const dc = pc.createDataChannel('net', { ordered: true });
  const q = messageQueue(dc);
  // R7 settlement: publish the handles as they come into
  // existence, so `connectAttempt` can retire them if this attempt
  // fails at any await below. The step loop runs one step at a
  // time, so there is only ever one attempt being built.
  inflight.pc = pc;
  inflight.dc = dc;

  // The candidate handler is installed BEFORE `setLocalDescription`,
  // which is what starts gathering. Installing it after the offer
  // POST — one HTTP round trip later — silently drops every
  // candidate gathered in that window, and on a fast local host that
  // is ALL of them: the measurement would then only ever observe the
  // peer-reflexive fallback, because nothing was ever signalled.
  // Candidates raised before the dialog id exists are buffered.
  const outbox = [];
  let sendCandidate = (line, mid) => outbox.push([line, mid]);
  // The candidate types this attempt GATHERED, and the ICE state
  // timeline, both recorded with timings. A failed attempt reports
  // them (see `connectAttempt`), so the next occurrence is
  // diagnosable from the log alone: whether the engine produced a
  // reflexive candidate at all on this interface, whether the host
  // candidate was an obfuscated `.local` name, and when the check
  // succeeded relative to the anchor's peer-reflexive learn.
  st.local_candidates = [];
  st.remote_candidates = [];
  st.ice = [];
  const at = () => Math.round(performance.now() - t0);
  pc.onicegatheringstatechange = () =>
    log('[cand ' + step.session + '] gathering=' + pc.iceGatheringState);
  pc.oniceconnectionstatechange = () => {
    st.ice.push({ state: pc.iceConnectionState, ms: at() });
    log('[cand ' + step.session + '] iceConnectionState=' + pc.iceConnectionState);
  };
  pc.onicecandidateerror = (ev) =>
    log(
      '[cand ' + step.session + '] ERROR url=' + ev.url + ' code=' + ev.errorCode + ' ' +
        ev.errorText,
    );
  pc.onicecandidate = (ev) => {
    if (!ev.candidate || !ev.candidate.candidate) {
      log('[cand ' + step.session + '] -> end-of-candidates');
      return;
    }
    st.local_candidates.push(describeCandidate(ev.candidate, at()));
    log('[cand ' + step.session + '] -> ' + ev.candidate.candidate);
    sendCandidate(ev.candidate.candidate, ev.candidate.sdpMid || '0');
  };

  const offer = await pc.createOffer();
  await pc.setLocalDescription(offer);

  const res = await fetch(step.base + '/rtc/offer', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({
      credential: step.credential,
      node_id: '0x' + step.node_id,
      sdp: pc.localDescription.sdp,
    }),
  });
  const text = await res.text();
  st.offer_status = res.status;
  if (!res.ok) throw new Error('POST /rtc/offer -> ' + res.status + ' ' + text);
  st.offer_accepted = true;
  const body = JSON.parse(text);

  await pc.setRemoteDescription({ type: 'answer', sdp: body.sdp });
  if (body.candidate) {
    st.remote_candidates.push(describeCandidate(body.candidate, at()));
    await log('[cand ' + step.session + '] <- anchor (offer body) ' + body.candidate);
    try {
      await pc.addIceCandidate({ candidate: body.candidate, sdpMid: '0', sdpMLineIndex: 0 });
    } catch (e) {
      await log('[page] anchor candidate rejected: ' + e);
    }
  }

  // The trickle socket. Candidates both ways, as the 0x0D02 JSON.
  const wsUrl =
    step.base.replace(/^https:/, 'wss:') +
    '/rtc/trickle?dialog=' +
    body.dialog +
    '&node_id=0x' +
    step.node_id;
  // R1: the attempt token rides the WebSocket SUBPROTOCOL, never
  // the URL — a query string lands in proxy logs and browser
  // history, and this is a bearer credential for a live ICE
  // attempt. The anchor refuses the upgrade without it.
  const ws = new WebSocket(wsUrl, ['net-bootstrap-attempt.' + body.attempt_token]);
  inflight.ws = ws;
  const pendingOut = [];
  let wsOpen = false;
  const frame = (line, mid) =>
    JSON.stringify({ type: 'candidate', dialog: body.dialog, candidate: line, mid });
  ws.onopen = () => {
    wsOpen = true;
    log('[trickle ' + step.session + '] open');
    for (const m of pendingOut.splice(0)) ws.send(m);
  };
  ws.onmessage = async (ev) => {
    try {
      const msg = JSON.parse(ev.data);
      if (msg.type === 'candidate' && msg.candidate) {
        st.remote_candidates.push(describeCandidate(msg.candidate, at()));
        await log('[cand ' + step.session + '] <- anchor ' + msg.candidate);
        await pc.addIceCandidate({
          candidate: msg.candidate,
          sdpMid: msg.mid || '0',
          sdpMLineIndex: 0,
        });
      }
    } catch (e) {
      await log('[page] trickle inbound ignored: ' + e);
    }
  };
  ws.onerror = () => log('[trickle ' + step.session + '] ERROR (readyState ' + ws.readyState + ')');
  // Every close, not only the typed refusals: WHEN this socket goes
  // away decides when the anchor treats the attempt as abandoned,
  // so a witness reading the anchor's attempt state needs it in the
  // log (R7 settlement).
  ws.onclose = (ev) => {
    log(
      '[trickle ' + step.session + '] closed code=' + ev.code + ' clean=' + ev.wasClean +
        (ev.reason ? ' reason=' + ev.reason : ''),
    );
  };
  sendCandidate = (line, mid) => {
    const m = frame(line, mid);
    if (wsOpen) ws.send(m);
    else pendingOut.push(m);
  };
  for (const [line, mid] of outbox.splice(0)) sendCandidate(line, mid);

  await withTimeout(waitOpen(dc), step.timeout_ms, 'datachannel open');
  st.dc_open = true;
  const openMs = performance.now() - t0;
  st.dc_open_ms = Math.round(openMs);

  const ep = new LeafEndpoint(step.psk, step.anchor_pub, step.node_id, step.anchor_node_id);
  st.noise_constructed = true;
  dc.send(ep.msg1_packet());
  st.msg1_sent = true;
  st.msg1_sent_ms = at();
  const pkt = await q.next(step.timeout_ms, 'noise msg2');
  st.msg2_received = true;
  ep.read_msg2_packet(pkt);
  st.msg2_authenticated = true;

  sessions.set(step.session, { pc, dc, q, ep, ws });
  return { ok: true, session_id: ep.session_id(), open_ms: openMs };
}

// ---------------------------------------------------------------------
// steps
// ---------------------------------------------------------------------

async function execute(step) {
  switch (step.kind) {
    case 'connect': {
      if (step.expect_failure) {
        // ---------------------------------------------------------
        // The pinned-key (MITM) witness.
        //
        // This used to be `try { connect() } catch { ok: true }`,
        // which passed on ANY exception: a refused offer, a
        // certificate problem, an ICE failure, a DataChannel that
        // never opened — none of which say anything about the
        // credential's pinned static key. It asserts, IN ORDER:
        //
        //   1. the offer was ACCEPTED (HTTP 200),
        //   2. the DataChannel OPENED,
        //   3. Noise was constructed and msg1 was SENT,
        //   4. the attempt then died AT THE PINNED-KEY BOUNDARY:
        //      either an msg2 that does not authenticate under the
        //      pinned static key, or no authenticated msg2 at all
        //      after msg1 was sent.
        //
        // Anything short of stage 3 is a TEST ERROR (`test_error`),
        // which the runner reports as a distinct FAIL — never as a
        // refused MITM. `ok: true` here means "failed at the pinned
        // key"; the runner inverts nothing.
        const stage = {
          offer_status: 0,
          offer_accepted: false,
          dc_open: false,
          noise_constructed: false,
          msg1_sent: false,
          msg2_received: false,
          msg2_authenticated: false,
          failure: null,
        };
        try {
          await connectAttempt(step, stage);
        } catch (e) {
          stage.failure = e.message || String(e);
        }
        const f = stage.failure || '<no error>';
        if (!stage.offer_accepted) {
          return {
            ok: false,
            test_error: true,
            stage,
            error:
              'TEST ERROR: the offer was never accepted (HTTP ' + stage.offer_status + '): ' + f +
              ' — the pinned-key witness requires an ACCEPTED offer; a refused offer is not a refused MITM',
          };
        }
        if (!stage.dc_open) {
          return {
            ok: false,
            test_error: true,
            stage,
            error:
              'TEST ERROR: the DataChannel never opened (ICE/transport): ' + f +
              ' — an ICE failure proves nothing about the credential-pinned key',
          };
        }
        if (!stage.noise_constructed) {
          return {
            ok: false,
            test_error: true,
            stage,
            error: 'TEST ERROR: the Noise initiator could not be constructed: ' + f,
          };
        }
        if (!stage.msg1_sent) {
          return {
            ok: false,
            test_error: true,
            stage,
            error: 'TEST ERROR: Noise msg1 was never sent: ' + f,
          };
        }
        if (stage.msg2_authenticated) {
          return {
            ok: false,
            stage,
            error: 'the handshake SUCCEEDED against an anchor that does not hold the pinned key',
            info: 'a session was installed — this is the MITM the credential is supposed to stop',
          };
        }
        const timedOutAfterMsg1 = !!stage.failure && stage.failure.indexOf('timeout: noise msg2') === 0;
        if (!stage.msg2_received && !timedOutAfterMsg1) {
          return {
            ok: false,
            test_error: true,
            stage,
            error:
              'TEST ERROR: after msg1 the attempt died for a reason that is not the pinned-key ' +
              'boundary (no msg2, no msg2 timeout): ' + f,
          };
        }
        return {
          ok: true,
          stage,
          info: stage.msg2_received
            ? 'offer accepted (' + stage.offer_status + '), DataChannel open, msg1 sent, and msg2 ' +
              'FAILED to authenticate under the credential-pinned static key: ' + f
            : 'offer accepted (' + stage.offer_status + '), DataChannel open, msg1 sent, and no ' +
              'authenticating msg2 ever arrived: ' + f,
        };
      }
      // A connect reports its stage whether it succeeded or not.
      // The exception alone says only where the wait expired; the
      // stage says what the engine gathered, when the check
      // succeeded, and which pair carried it — the difference
      // between "no pair formed" and "a pair formed and something
      // after it went wrong". On the passing path the same record
      // is what the mDNS verdict quotes as its own baseline.
      const stage = {};
      try {
        const r = await connectAttempt(step, stage);
        r.stage = stage;
        return r;
      } catch (e) {
        stage.failure = e.message || String(e);
        return { ok: false, error: stage.failure, stage };
      }
    }

    case 'send': {
      const s = sessions.get(step.session);
      if (!s) return { ok: false, error: 'no session ' + step.session };
      const pkt = s.ep.build_frame(
        step.stream_id,
        step.subprotocol,
        step.channel_hash,
        step.origin_hash,
        step.reliable,
        unhex(step.payload),
      );
      const out = step.prefix ? concat(unhex(step.prefix), pkt) : pkt;
      // A send on a channel that is not open is a FAILED send, and
      // the runner asserts on this: "the frame was sent" is a
      // premise of every delivery verdict, never an assumption.
      if (s.dc.readyState !== 'open') {
        return { ok: false, error: 'the DataChannel is ' + s.dc.readyState + ', not open' };
      }
      try {
        s.dc.send(out);
      } catch (e) {
        return { ok: false, error: 'DataChannel.send: ' + (e.message || String(e)) };
      }
      return { ok: true, sent_frames: 1, sent_bytes: out.length, info: 'sent ' + out.length + ' bytes' };
    }

    /// N frames of the same shape, built and sent from the browser
    /// so a whole-session bound can be crossed without N HTTP round
    /// trips. Reports exactly how many frames and bytes left the
    /// page, and why it stopped early if it did.
    case 'burst': {
      const s = sessions.get(step.session);
      if (!s) return { ok: false, error: 'no session ' + step.session };
      const payload = new Uint8Array(step.payload_len);
      payload.fill(step.fill & 0xff);
      let frames = 0;
      let bytes = 0;
      let stopped = null;
      for (let i = 0; i < step.frames; i++) {
        if (s.dc.readyState !== 'open') {
          stopped = 'the DataChannel went ' + s.dc.readyState + ' after ' + frames + ' frames';
          break;
        }
        let pkt;
        try {
          pkt = s.ep.build_frame(
            step.stream_id,
            step.subprotocol,
            step.channel_hash,
            step.origin_hash,
            step.reliable,
            payload,
          );
        } catch (e) {
          stopped = 'build_frame: ' + (e.message || String(e));
          break;
        }
        try {
          s.dc.send(pkt);
        } catch (e) {
          stopped = 'DataChannel.send: ' + (e.message || String(e));
          break;
        }
        frames++;
        bytes += pkt.length;
        if (s.dc.bufferedAmount > 1 << 20) await sleep(20);
      }
      return {
        ok: frames > 0,
        sent_frames: frames,
        sent_bytes: bytes,
        error: frames > 0 ? undefined : 'not one frame was sent: ' + stopped,
        info:
          'sent ' + frames + ' frame(s), ' + bytes + ' bytes' +
          (stopped ? '; stopped early: ' + stopped : ''),
      };
    }

    /// Drop one session from the browser end: the DataChannel, the
    /// trickle socket and the PeerConnection. The anchor must
    /// observe the close and evict the peer.
    case 'close': {
      const s = sessions.get(step.session);
      if (!s) return { ok: false, error: 'no session ' + step.session };
      settleHandles(step.session, s.pc, s.dc, s.ws);
      sessions.delete(step.session);
      return { ok: true, info: 'closed the browser end of ' + step.session };
    }

    /// Wait until this session's transport goes DOWN, observed from
    /// the far end: the DataChannel leaves `open`, or the
    /// `RTCPeerConnection` leaves a live state — which on this host
    /// is ICE consent to the anchor's endpoint lapsing, because the
    /// endpoint behind it stopped answering.
    ///
    /// This is the peer-side observation that the anchor's close
    /// reached the TRANSPORT, which no counter on the anchor
    /// establishes: `admission_reclaimed` says the reclaim DECISION
    /// was taken. The page never closes this session itself, so
    /// whichever of the two states moves, the anchor moved it.
    case 'expect_closed': {
      const s = sessions.get(step.session);
      if (!s) return { ok: false, error: 'no session ' + step.session };
      const t0 = performance.now();
      const deadline = Date.now() + step.timeout_ms;
      const down = () =>
        s.dc.readyState !== 'open' ||
        s.pc.connectionState === 'disconnected' ||
        s.pc.connectionState === 'failed' ||
        s.pc.connectionState === 'closed';
      while (Date.now() < deadline && !down()) await sleep(50);
      const torn = down();
      const state =
        'datachannel=' + s.dc.readyState + ', pc=' + s.pc.connectionState +
        ', ice=' + s.pc.iceConnectionState;
      return {
        ok: torn,
        open_ms: performance.now() - t0,
        info: state + ' after ' + Math.round(performance.now() - t0) + ' ms',
        error: torn
          ? undefined
          : 'the transport of ' + step.session + ' is still up (' + state +
            '): the anchor did not tear it down',
      };
    }

    case 'expect': {
      const s = sessions.get(step.session);
      if (!s) return { ok: false, error: 'no session ' + step.session };
      const deadline = Date.now() + step.timeout_ms;
      while (Date.now() < deadline) {
        let pkt;
        try {
          pkt = await s.q.next(Math.max(200, deadline - Date.now()), 'inbound packet');
        } catch (e) {
          return { ok: false, error: e.message || String(e) };
        }
        const head = JSON.parse(s.ep.peek(pkt));
        if (head.handshake) continue;
        // A membership Ack and an nRPC RESPONSE both arrive on this
        // channel; only the one the runner asked for counts.
        if (head.subprotocol !== step.subprotocol) {
          await log('[page] skipping an inbound subprotocol ' + head.subprotocol + ' frame');
          continue;
        }
        let frame;
        try {
          frame = s.ep.open_packet(pkt);
        } catch (e) {
          await log('[page] undecryptable inbound packet: ' + e);
          continue;
        }
        if (frame.length === 0) continue;
        return { ok: true, frame: hex(frame), info: 'subprotocol ' + head.subprotocol };
      }
      return { ok: false, error: 'no inbound frame within the deadline' };
    }

    case 'stats': {
      const s = sessions.get(step.session);
      if (!s) return { ok: false, error: 'no session ' + step.session };
      const pair = await selectedPair(s.pc);
      if (!pair) return { ok: true, stats: { selected: null } };
      return { ok: true, stats: pair };
    }

    case 'idle':
      await sleep(step.millis || 50);
      return { ok: true };

    case 'done':
      return { ok: true };

    default:
      return { ok: false, error: 'unknown step kind ' + step.kind };
  }
}

// ---------------------------------------------------------------------
// main loop
// ---------------------------------------------------------------------

async function main() {
  await init();
  await log('[page] wasm leaf (net-mesh-wire) initialised');
  for (;;) {
    let step;
    try {
      const res = await fetch('/harness/step');
      step = await res.json();
    } catch (e) {
      await sleep(200);
      continue;
    }
    if (step.kind === 'done') {
      await fetch('/harness/result', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ id: step.id, ok: true }),
      }).catch(() => {});
      await log('[page] done');
      return;
    }
    if (step.kind === 'idle' && step.id === 0) {
      await sleep(step.millis || 50);
      continue;
    }
    let result;
    try {
      result = await execute(step);
    } catch (e) {
      result = { ok: false, error: e.message || String(e) };
    }
    result.id = step.id;
    await fetch('/harness/result', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(result),
    }).catch(() => {});
  }
}

main().catch(async (e) => {
  await log('[page] FATAL ' + (e.message || String(e)));
});
