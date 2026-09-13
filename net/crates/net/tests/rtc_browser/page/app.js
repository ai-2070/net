// Stage 4b Chromium harness — the browser half.
//
// This page owns the TRANSPORT and nothing above it:
//
//   POST https://<anchor>/rtc/offer   (the real Stage 4b listener)
//   wss  https://<anchor>/rtc/trickle (candidates both ways)
//   RTCDataChannel "net", ordered:false, maxRetransmits:0
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

// ---------------------------------------------------------------------
// connect: offer -> trickle -> DataChannel -> NKpsk0
// ---------------------------------------------------------------------

async function doConnect(step) {
  const t0 = performance.now();
  const iceServers = step.stun ? [{ urls: step.stun }] : [];
  const pc = new RTCPeerConnection({ iceServers });
  const dc = pc.createDataChannel('net', { ordered: false, maxRetransmits: 0 });
  const q = messageQueue(dc);

  // The candidate handler is installed BEFORE `setLocalDescription`,
  // which is what starts gathering. Installing it after the offer
  // POST — one HTTP round trip later — silently drops every
  // candidate gathered in that window, and on a fast local host that
  // is ALL of them: the measurement would then only ever observe the
  // peer-reflexive fallback, because nothing was ever signalled.
  // Candidates raised before the dialog id exists are buffered.
  const outbox = [];
  let sendCandidate = (line, mid) => outbox.push([line, mid]);
  pc.onicegatheringstatechange = () =>
    log('[cand ' + step.session + '] gathering=' + pc.iceGatheringState);
  pc.oniceconnectionstatechange = () =>
    log('[cand ' + step.session + '] iceConnectionState=' + pc.iceConnectionState);
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
  if (!res.ok) throw new Error('POST /rtc/offer -> ' + res.status + ' ' + text);
  const body = JSON.parse(text);

  await pc.setRemoteDescription({ type: 'answer', sdp: body.sdp });
  if (body.candidate) {
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
  const ws = new WebSocket(wsUrl);
  const pendingOut = [];
  let wsOpen = false;
  const frame = (line, mid) =>
    JSON.stringify({ type: 'candidate', dialog: body.dialog, candidate: line, mid });
  ws.onopen = () => {
    wsOpen = true;
    for (const m of pendingOut.splice(0)) ws.send(m);
  };
  ws.onmessage = async (ev) => {
    try {
      const msg = JSON.parse(ev.data);
      if (msg.type === 'candidate' && msg.candidate) {
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
  ws.onclose = (ev) => {
    if (ev.code >= 4400) log('[page] trickle closed with typed code ' + ev.code);
  };
  sendCandidate = (line, mid) => {
    const m = frame(line, mid);
    if (wsOpen) ws.send(m);
    else pendingOut.push(m);
  };
  for (const [line, mid] of outbox.splice(0)) sendCandidate(line, mid);

  await withTimeout(waitOpen(dc), step.timeout_ms, 'datachannel open');
  const openMs = performance.now() - t0;

  const ep = new LeafEndpoint(step.psk, step.anchor_pub, step.node_id, step.anchor_node_id);
  dc.send(ep.msg1_packet());
  const pkt = await q.next(step.timeout_ms, 'noise msg2');
  ep.read_msg2_packet(pkt);

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
        // The MITM witness: the handshake MUST fail. `ok: true` here
        // means "failed as required"; the runner inverts nothing.
        try {
          await doConnect(step);
          return {
            ok: false,
            error: 'the handshake SUCCEEDED against an anchor that does not hold the pinned key',
            info: 'a session was installed — this is the MITM the credential is supposed to stop',
          };
        } catch (e) {
          return { ok: true, info: 'noise/transport failure: ' + (e.message || String(e)) };
        }
      }
      return await doConnect(step);
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
      s.dc.send(out);
      return { ok: true, info: 'sent ' + out.length + ' bytes' };
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
      const report = await s.pc.getStats();
      const byId = new Map();
      report.forEach((r) => byId.set(r.id, r));
      let pair = null;
      report.forEach((r) => {
        if (r.type === 'candidate-pair' && (r.selected || r.nominated || r.state === 'succeeded')) {
          if (!pair || r.state === 'succeeded') pair = r;
        }
      });
      if (!pair) return { ok: true, stats: { selected: null } };
      const local = byId.get(pair.localCandidateId) || {};
      const remote = byId.get(pair.remoteCandidateId) || {};
      return {
        ok: true,
        stats: {
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
        },
      };
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
