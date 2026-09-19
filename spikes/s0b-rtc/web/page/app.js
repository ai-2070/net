// S0b — RTC loop spike, browser driver.
//
// Runs, in order:
//   1. wasm liveness probes (getrandom/wasm_js, web_time clock)
//   2. role=answerer  (browser offers, native answers)  -> b2n + n2b
//   3. role=offerer   (native offers, browser answers)  -> b2n + n2b
//   4. admission probe (event loop blocked while native blasts)
//   5. trickle vs gather-complete open latency, 5 runs each
//   6. RTCPeerConnection in Worker / SharedWorker
//
// Every verdict line is both console.log'd and POSTed to /result so the
// native process prints it on stdout.

import { makeBench } from './bench.js';
import init, {
  LeafEndpoint,
  keygen_probe,
  clock_probe,
  wallclock_probe,
} from './s0b.js';

const TAG_NOISE_MSG1 = 0x01;
const TAG_NOISE_MSG2 = 0x02;
const TAG_NET_PACKET = 0x03;
const TAG_PROBE_START = 0x04;
const TAG_PROBE_FILL = 0x05;

const logEl = document.getElementById('log');

function log(line) {
  console.log(line);
  logEl.textContent += line + '\n';
  return fetch('/result', { method: 'POST', body: line + '\n' }).catch(() => {});
}

async function getConfig() {
  const text = await (await fetch('/config')).text();
  const cfg = {};
  for (const line of text.trim().split('\n')) {
    const [k, v] = line.split('=');
    cfg[k] = v;
  }
  return cfg;
}

function sleepSyncBlockingEventLoop(ms) {
  const end = Date.now() + ms;
  // Deliberate: blocks the event loop so nothing drains the DataChannel.
  while (Date.now() < end) {
    /* spin */
  }
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

function withTimeout(promise, ms, what) {
  return Promise.race([
    promise,
    new Promise((_, rej) => setTimeout(() => rej(new Error('timeout: ' + what)), ms)),
  ]);
}

function waitOpen(dc) {
  if (dc.readyState === 'open') return Promise.resolve();
  return new Promise((res, rej) => {
    dc.onopen = () => res();
    dc.onerror = (e) => rej(new Error('dc error: ' + e.message));
  });
}

/// One message queue per channel so the handshake can await in order.
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
      return withTimeout(
        new Promise((res) => waiters.push(res)),
        timeoutMs,
        what,
      );
    },
    depth: () => q.length,
    drain: () => {
      const n = q.length;
      q.length = 0;
      return n;
    },
  };
}

function tagged(tag, body) {
  const out = new Uint8Array(1 + body.length);
  out[0] = tag;
  out.set(body, 1);
  return out;
}

// ---------------------------------------------------------------------
// Signalling
// ---------------------------------------------------------------------

async function post(path, body) {
  const r = await fetch(path, { method: 'POST', body: body ?? '' });
  const text = await r.text();
  if (!r.ok) throw new Error(path + ' -> ' + r.status + ' ' + text);
  return text;
}

function trickleTo(pc, sid) {
  pc.onicecandidate = (ev) => {
    if (ev.candidate && ev.candidate.candidate) {
      post('/candidate/' + sid, ev.candidate.candidate).catch(() => {});
    }
  };
}

async function gatheringComplete(pc) {
  if (pc.iceGatheringState === 'complete') return;
  await new Promise((res) => {
    const check = () => {
      if (pc.iceGatheringState === 'complete') {
        pc.removeEventListener('icegatheringstatechange', check);
        res();
      }
    };
    pc.addEventListener('icegatheringstatechange', check);
    setTimeout(res, 5000); // gathering can stall; cap it
  });
}

/// Browser offers, native answers. Native is ICE controlled.
async function connectAsOfferer(sid, { trickle = true } = {}) {
  const pc = new RTCPeerConnection({ iceServers: [] });
  const dc = pc.createDataChannel('net', { ordered: false, maxRetransmits: 0 });
  const offer = await pc.createOffer();
  const tOfferCreated = performance.now();
  await pc.setLocalDescription(offer);
  if (trickle) {
    trickleTo(pc, sid);
    const answer = await post('/offer/' + sid, pc.localDescription.sdp);
    await pc.setRemoteDescription({ type: 'answer', sdp: answer });
  } else {
    await gatheringComplete(pc);
    const answer = await post('/offer/' + sid, pc.localDescription.sdp);
    await pc.setRemoteDescription({ type: 'answer', sdp: answer });
  }
  return { pc, dc, tOfferCreated };
}

/// Native offers, browser answers. Native is ICE controlling.
async function connectAsAnswerer(sid) {
  const pc = new RTCPeerConnection({ iceServers: [] });
  const dcPromise = new Promise((res) => {
    pc.ondatachannel = (ev) => res(ev.channel);
  });
  const offerSdp = await post('/create-offer/' + sid);
  const tOfferCreated = performance.now();
  await pc.setRemoteDescription({ type: 'offer', sdp: offerSdp });
  trickleTo(pc, sid);
  const answer = await pc.createAnswer();
  await pc.setLocalDescription(answer);
  await post('/answer/' + sid, pc.localDescription.sdp);
  const dc = await withTimeout(dcPromise, 15000, 'ondatachannel');
  return { pc, dc, tOfferCreated };
}

// ---------------------------------------------------------------------
// Handshake + round-trip over the channel
// ---------------------------------------------------------------------

async function roundTrip(role, dc, q, ep) {
  const payload = new TextEncoder().encode(
    'S0B reliable-stream payload from the browser leaf, role=' + role,
  );
  dc.send(tagged(TAG_NET_PACKET, ep.build_packet(payload)));

  let reply = await q.next(15000, 'net reply');
  while (reply[0] === TAG_PROBE_FILL) reply = await q.next(15000, 'net reply');
  if (reply[0] !== TAG_NET_PACKET) throw new Error('expected packet, got tag ' + reply[0]);
  const plain = ep.open_packet(reply.subarray(1));

  const expected = new TextEncoder().encode('echo:' + new TextDecoder().decode(payload));
  if (plain.length !== expected.length) {
    throw new Error('echo length mismatch ' + plain.length + ' != ' + expected.length);
  }
  for (let i = 0; i < plain.length; i++) {
    if (plain[i] !== expected[i]) throw new Error('echo mismatch at ' + i);
  }

  // b2n is proven by the native side having decrypted our packet well
  // enough to echo its plaintext back; n2b by us decrypting the reply.
  await log(`S0B OK role=${role} dir=b2n payload=${payload.length} bytes`);
  await log(`S0B OK role=${role} dir=n2b payload=${plain.length} bytes`);
  return ep;
}

/// Connect, open the channel, run NKpsk0 — no verdict logging, no
/// round-trip. Shared by the S0b scenarios and the S0c bench.
async function establish(cfg, role, { quiet = false } = {}) {
  const sid = role + '-' + Math.random().toString(16).slice(2, 8);
  const t0 = performance.now();
  const { pc, dc } =
    role === 'answerer' ? await connectAsOfferer(sid) : await connectAsAnswerer(sid);
  const q = messageQueue(dc);
  await withTimeout(waitOpen(dc), 20000, 'datachannel open (' + role + ')');
  if (!quiet) {
    await log(
      `S0B INFO role=${role} channel open after ${Math.round(performance.now() - t0)} ms ` +
        `(ordered=${dc.ordered} maxRetransmits=${dc.maxRetransmits})`,
    );
  }
  const ep = new LeafEndpoint(cfg.psk, cfg.static, cfg.browser_id, cfg.native_id);
  dc.send(tagged(TAG_NOISE_MSG1, ep.msg1()));
  const m2 = await q.next(15000, 'noise msg2');
  if (m2[0] !== TAG_NOISE_MSG2) throw new Error('expected msg2, got tag ' + m2[0]);
  ep.read_msg2(m2.subarray(1));
  return { sid, pc, dc, q, ep, openMs: performance.now() - t0 };
}

async function scenario(cfg, role) {
  const s = await establish(cfg, role);
  await roundTrip(role, s.dc, s.q, s.ep);
  return s;
}

// ---------------------------------------------------------------------
// Admission probe
// ---------------------------------------------------------------------

async function admissionProbe(cfg) {
  const s = await scenario(cfg, 'answerer');
  await log('S0B INFO admission probe: blocking the event loop for 3000 ms');
  s.dc.send(new Uint8Array([TAG_PROBE_START]));
  sleepSyncBlockingEventLoop(3000);
  // Short settle so the queued onmessage events actually dispatch and
  // can be counted; the native side keeps pushing for 3500 ms, so the
  // close below still lands mid-backlog — that is question (c).
  await sleep(300);
  const queued = s.q.drain();
  const stats = await (await fetch('/probe-stats')).text();
  const flat = stats.trim().split('\n').join(' ');
  await log(`S0B PROBE browser_received_while_blocked=${queued} ${flat}`);
  await post('/close/' + s.sid);
  s.pc.close();
  await sleep(1500);
  const after = await (await fetch('/probe-stats')).text();
  await log(`S0B PROBE after_close ${after.trim().split('\n').join(' ')}`);
}

// ---------------------------------------------------------------------
// Trickle vs gather-complete
// ---------------------------------------------------------------------

async function openLatency(trickle) {
  const sid = (trickle ? 'trickle-' : 'gathered-') + Math.random().toString(16).slice(2, 8);
  const { pc, dc, tOfferCreated } = await connectAsOfferer(sid, { trickle });
  await withTimeout(waitOpen(dc), 20000, 'open latency');
  const ms = performance.now() - tOfferCreated;
  await post('/close/' + sid);
  pc.close();
  return ms;
}

function stats(xs) {
  const s = [...xs].sort((a, b) => a - b);
  const med = s.length % 2 ? s[(s.length - 1) / 2] : (s[s.length / 2 - 1] + s[s.length / 2]) / 2;
  return { min: s[0], med, max: s[s.length - 1] };
}

async function timings() {
  for (const trickle of [true, false]) {
    const runs = [];
    for (let i = 0; i < 5; i++) {
      try {
        runs.push(await openLatency(trickle));
      } catch (e) {
        await log(`S0B TIMING ${trickle ? 'trickle' : 'gathered'} run ${i} FAILED: ${e.message}`);
      }
      await sleep(200);
    }
    if (runs.length) {
      const st = stats(runs);
      await log(
        `S0B TIMING mode=${trickle ? 'trickle' : 'gather-complete'} n=${runs.length} ` +
          `min=${st.min.toFixed(1)}ms median=${st.med.toFixed(1)}ms max=${st.max.toFixed(1)}ms ` +
          `raw=[${runs.map((r) => r.toFixed(1)).join(',')}]`,
      );
    }
  }
}

// ---------------------------------------------------------------------
// RTCPeerConnection in workers
// ---------------------------------------------------------------------

const WORKER_SRC = `
try {
  const pc = new RTCPeerConnection();
  const dc = pc.createDataChannel('x');
  pc.close();
  REPLY('ok: RTCPeerConnection constructed, createDataChannel ok');
} catch (e) {
  REPLY('err: ' + (e && e.name) + ': ' + (e && e.message));
}
`;

async function workerChecks() {
  // Dedicated worker
  try {
    const src = WORKER_SRC.replace(/REPLY\(/g, 'postMessage(');
    const w = new Worker(URL.createObjectURL(new Blob([src], { type: 'text/javascript' })));
    const msg = await withTimeout(
      new Promise((res) => {
        w.onmessage = (e) => res(e.data);
        w.onerror = (e) => res('err(onerror): ' + e.message);
      }),
      5000,
      'worker',
    );
    await log(`S0B WORKER dedicated: ${msg}`);
    w.terminate();
  } catch (e) {
    await log(`S0B WORKER dedicated: FAILED ${e.message}`);
  }

  // Shared worker
  try {
    const src =
      'onconnect = (ev) => { const p = ev.ports[0];' +
      WORKER_SRC.replace(/REPLY\(/g, 'p.postMessage(') +
      '};';
    const sw = new SharedWorker(URL.createObjectURL(new Blob([src], { type: 'text/javascript' })));
    sw.port.start();
    const msg = await withTimeout(
      new Promise((res) => {
        sw.port.onmessage = (e) => res(e.data);
        sw.onerror = (e) => res('err(onerror): ' + e.message);
      }),
      5000,
      'sharedworker',
    );
    await log(`S0B WORKER shared: ${msg}`);
  } catch (e) {
    await log(`S0B WORKER shared: FAILED ${e.message}`);
  }
}

// ---------------------------------------------------------------------
// S0c hook — defined, deliberately NOT measured here
// ---------------------------------------------------------------------

/// Open a session and send `n` Net packets of `size` bytes at `rateHz`
/// over the DataChannel, returning per-packet timings in milliseconds.
///
/// This is the hook S0c (double-AEAD cost) will drive; S0b defines it
/// and takes no measurement. `build` is the Net-layer
/// ChaCha20-Poly1305 seal in wasm, `send` is the DataChannel handoff
/// (DTLS is the second AEAD, inside Chromium).
///
///   await window.s0cHook({ n: 600, size: 1024, rateHz: 60 });
window.s0cHook = async function s0cHook({ n = 100, size = 1024, rateHz = 60 } = {}) {
  const cfg = await getConfig();
  const s = await scenario(cfg, 'answerer');
  const payload = new Uint8Array(size).fill(0x5a);
  const build = [];
  const send = [];
  const periodMs = 1000 / rateHz;
  const t0 = performance.now();
  for (let i = 0; i < n; i++) {
    const target = t0 + i * periodMs;
    const now = performance.now();
    if (target > now) await sleep(target - now);
    const a0 = performance.now();
    const pkt = s.ep.build_packet(payload);
    const a1 = performance.now();
    s.dc.send(tagged(TAG_NET_PACKET, pkt));
    const a2 = performance.now();
    build.push(a1 - a0);
    send.push(a2 - a1);
  }
  const wall = performance.now() - t0;
  await post('/close/' + s.sid);
  s.pc.close();
  return { n, size, rateHz, wallMs: wall, build, send };
};

// ---------------------------------------------------------------------
// main
// ---------------------------------------------------------------------

async function main() {
  const cfg = await getConfig();
  await init();
  await log(`S0B INFO userAgent=${navigator.userAgent}`);

  // These three calls are the first execution anywhere of the S0a wasm
  // build: getrandom/wasm_js (keygen), snow's default resolver, and
  // web_time (monotonic + wall clock).
  const pub = keygen_probe();
  await log(
    `S0B INFO wasm probes: keygen_pub_len=${pub.length} first_byte=${pub[0]} ` +
      `clock_delta_ms=${clock_probe().toFixed(3)} wallclock_ms=${wallclock_probe().toFixed(0)}`,
  );

  const answerer = await scenario(cfg, 'answerer');
  await post('/close/' + answerer.sid);
  answerer.pc.close();

  const offerer = await scenario(cfg, 'offerer');
  await post('/close/' + offerer.sid);
  offerer.pc.close();

  await admissionProbe(cfg);
  await timings();
  await workerChecks();
}

/// S0c: `?bench=1` runs the double-AEAD measurement instead of the S0b
/// scenario sequence. Same page, same wasm, same driver.
async function benchMain() {
  await init();
  const bench = makeBench({ log, post, sleep, tagged, establish, getConfig });
  await bench.run();
}

const isBench = new URLSearchParams(location.search).has('bench');

(isBench ? benchMain() : main())
  .then(async () => {
    await log(isBench ? 'S0C COMPLETE' : 'S0B COMPLETE');
    await post('/done');
  })
  .catch(async (e) => {
    await log((isBench ? 'S0C FAILED ' : 'S0B FAILED ') + (e && e.stack ? e.stack : e));
    await post('/done');
  });
