// One side of a NAT conformance row, driven through
// `@net-mesh/browser`.
//
// This page is deliberately thin. It does not build packets, run a
// handshake or speak the bootstrap protocol; it calls the package's
// own surface — `connect`, `announce`, `query`, `connectPeer`,
// `acceptPeer` — and reports what it got. Anything failing here is the
// leaf or the package failing, which is what a conformance row is
// supposed to measure.
//
// The step protocol is the same shape the Stage 4b/5 harness uses: the
// runner owns the sequence, the page long-polls for the next step and
// posts back a result keyed by step id. The runner lives in the
// anchor's namespace and is reached over plain HTTP; this page is
// served over `http://localhost` from inside this browser's own
// namespace, so that fetch is http→http (no mixed content) and the
// page still has a secure context for `RTCPeerConnection`.
//
// BOTH HALVES OF THE DIALOG ARE HERE. The offerer calls
// `connectPeer`; the answerer arms a `signal` listener BEFORE the
// offer is sent and calls `acceptPeer` when it fires. A row that only
// drove the offerer would hang at the answerer's side of §9 step 3 and
// then report an `iceTimeout` that says nothing about NAT.

import * as browserSdk from '/browser/index.js';

const { connect } = browserSdk;

const params = new URLSearchParams(location.search);
const TAB = params.get('tab') || 'a';
const CONTROL = params.get('control');

const logEl = document.getElementById('log');
let node = null;
/// The answerer's armed outcome: a promise that fulfils when
/// `acceptPeer` settles. `answered` keeps a second signal for the same
/// peer from starting a second attempt — which would spend a second
/// `ice_attempted` and break the row's exact count.
let armed = null;
const answered = new Set();
/// This side's own announcement ledger. A query that finds nothing is
/// two different failures depending on whether the ANNOUNCER was
/// still announcing, and the answering side's counts are the only
/// place that distinction exists.
const announces = { ok: 0, failed: 0, lastError: null, timer: null };
/// How often this page re-announces once the `announce` step has run.
/// One second is the same order the reference page uses; the row's
/// discovery budget is twenty, so a lost announcement costs a second
/// rather than the row.
const ANNOUNCE_EVERY_MS = 1000;

/// This side's half of the application exchange.
///
/// `seen` is the nonce the PEER minted, decoded from a frame that
/// arrived here. It is the only field that cannot be produced
/// locally, which is why the row asserts on it rather than on the
/// counts beside it.
const app = {
  stream: null,
  mine: null,
  expect: null,
  seen: null,
  sent: 0,
  received: 0,
  echoes: 0,
  lastError: null,
};

/// How many frames one side sends, and the gap between them.
///
/// `fireAndForget` is the reliability class these rows exercise, so
/// a handful of frames is the honest shape: one frame would make a
/// single drop look like a broken path, and a flood would make the
/// anchor's per-pair counter delta depend on how fast the loop ran.
const APP_FRAMES = 4;
const APP_GAP_MS = 100;

/// The exchange's frame: a fixed prefix and 16 hex nonce digits.
///
/// Deliberately not JSON and deliberately not a bare nonce: the
/// prefix means a frame from anything else on this stream id is
/// ignored rather than decoded into a nonce comparison, and the
/// fixed length means a truncated frame cannot read as a shorter
/// nonce that happens to match.
const APP_PREFIX = 'natsim-app:';

function encodeNonce(nonce) {
  return new TextEncoder().encode(`${APP_PREFIX}${nonce}`);
}

function decodeNonce(payload) {
  let text;
  try {
    text = new TextDecoder().decode(payload);
  } catch (e) {
    return null;
  }
  if (!text.startsWith(APP_PREFIX)) return null;
  const nonce = text.slice(APP_PREFIX.length);
  return /^[0-9a-f]{16}$/.test(nonce) ? nonce : null;
}

/// The PUBLIC peer-addressed stream. `peer` is what the package
/// gained in slice 6 and what a page needs to put application bytes
/// on a direct session at all; nothing here names a key, an SDP or
/// an address.
async function openAppStream(peerHex, streamId) {
  return node.openStream({
    reliability: 'fireAndForget',
    peer: peerHex,
    streamId,
    label: `natsim-app-${TAB}`,
  });
}

async function log(line) {
  logEl.textContent += line + '\n';
  try {
    await fetch(`${CONTROL}/matrix/log`, { method: 'POST', body: `[${TAB}] ${line}` });
  } catch (e) {
    // The control plane going away is the runner's problem to report;
    // a page that threw here would lose the step result it is mid-way
    // through producing.
  }
}

/// Every leaf counter, values left as DECIMAL STRINGS.
///
/// `counters_json()` is preferred over the wrapper's `counters()`
/// because it is the raw `u64` dump: a JS number would round any
/// counter above 2^53, and the identity this row asserts is exact
/// arithmetic.
function counters() {
  if (!node) return {};
  try {
    const raw =
      node.inner && typeof node.inner.counters_json === 'function'
        ? node.inner.counters_json()
        : null;
    if (raw) return JSON.parse(raw);
    return node.counters();
  } catch (e) {
    return { error: (e && (e.message || String(e))) || 'unknown' };
  }
}

/// A `LeafEvent`'s `from` is an exact DECIMAL mesh id; every peer-id
/// argument is 16 lowercase hex digits. `BigInt` because a u64 node id
/// does not survive `Number`.
function toPeerHex(decimal) {
  return BigInt(decimal).toString(16).padStart(16, '0');
}

/// Whether a peer entry names `expectHex`, in EITHER encoding the
/// leaf surface uses.
///
/// `connect` hands back `node.nodeId` as 16 lowercase hex digits and
/// every peer-id ARGUMENT is hex, but a capability query's `nodeId`
/// is the exact DECIMAL u64 — the same asymmetry `toPeerHex` already
/// exists for on a `signal` event's `from`. Comparing the two
/// verbatim is why run 35055657044's Firefox row reported that B
/// "never appeared" in eighty queries whose very first answer was
/// `nodeId: "9684434118755589433"` — which IS `0x866605854b199d39`,
/// the peer it was looking for. Both readings are tried and neither
/// is guessed at: a decimal that converts to the expected hex is the
/// same node, and so is a string already equal to it.
function namesPeer(entry, expectHex) {
  const raw = typeof entry === 'string' ? entry : entry && (entry.nodeId || entry.node_id);
  if (raw === null || raw === undefined) return false;
  const s = String(raw);
  if (s.toLowerCase() === expectHex.toLowerCase()) return true;
  if (!/^[0-9]+$/.test(s)) return false;
  try {
    return toPeerHex(s) === expectHex.toLowerCase();
  } catch (e) {
    return false;
  }
}

function safeJson(v) {
  try {
    return JSON.stringify(v);
  } catch (e) {
    return String(v);
  }
}

/// A rejection from `@net-mesh/browser`. `PeerConnectOutcome` is a
/// typed RESULT, so a rejection is a NON-disposition by contract (a
/// closed node, a peer that answered `Reject`, a malformed id) and is
/// reported as itself rather than folded into a disposition.
function typed(e) {
  return {
    page_type: (e && (e.type || e.kind)) || 'threw',
    detail: (e && (e.message || String(e))) || 'unknown',
  };
}

/// The engine's own account of every `RTCPeerConnection` the leaf
/// built, installed by the inline script in `matrix.html` before this
/// module could run. It rides the result of every step that turns on
/// ICE, in both directions: a row that fails needs it to name the
/// cause, and a row that passes needs it to show the path was the one
/// the NAT flavor predicts rather than an accident.
async function iceReport() {
  const api = window.__natsimIce;
  if (!api) return { unavailable: 'the ICE observer did not install' };
  try {
    return await api.report();
  } catch (e) {
    return { error: (e && (e.message || String(e))) || 'unknown' };
  }
}

async function execute(step) {
  switch (step.kind) {
    case 'connect': {
      const opts = {
        credentialB64: step.credential,
        bootstrapUrl: step.bootstrap_url,
        origin: step.origin,
        anchorRtcAddr: step.anchor_rtc_addr,
        entitySecretHex: step.entity_secret_hex,
        noiseSecretHex: step.noise_secret_hex,
      };
      // The anchor's own STUN responder is this row's only reflexive
      // candidate source, and it is what makes the NAT flavor
      // observable: a server-reflexive candidate IS the gateway's
      // mapping. Without it the two browsers would have nothing but
      // host candidates on two different private subnets.
      // NOTHING is configured here on purpose. Stage 6 §6.12.2: the
      // leaf defaults its own `iceServers` to the STUN endpoint the
      // anchor ANNOUNCES, read from `GET /rtc/anchor`. A page that
      // named one here would be the harness picking a configuration
      // that works instead of exercising the product's — Kyra's
      // acceptance item 2 — and naming the anchor's `rtc_addr` is now
      // a typed refusal before ICE rather than a 60-second timeout.
      try {
        node = await connect(opts);
      } catch (e) {
        return { ok: false, ...typed(e), rtc: await iceReport() };
      }
      return { ok: true, node_id: node.nodeId, counters: counters(), rtc: await iceReport() };
    }

    case 'announce': {
      if (!node) return { ok: false, detail: 'announce before connect' };
      try {
        await node.announce(step.capabilities);
      } catch (e) {
        return { ok: false, ...typed(e) };
      }
      announces.ok += 1;
      // KEEP announcing. A leaf's announcement is a periodic fact,
      // not a one-shot registration, and `@net-mesh/browser`'s own
      // reference page (`examples/browser-demo`) re-announces on a
      // timer for exactly that reason. Announcing once and then
      // querying for twenty seconds is what run 35054553211's
      // Firefox row did: both tabs connected, enrolled and announced
      // successfully, and A's query for B still never resolved. This
      // is not a retry bolted onto a deadline — the deadline is
      // unchanged; it is the page behaving like the leaf it is
      // standing in for.
      if (!announces.timer) {
        const capabilities = step.capabilities;
        announces.timer = setInterval(() => {
          node.announce(capabilities).then(
            () => {
              announces.ok += 1;
            },
            (e) => {
              announces.failed += 1;
              announces.lastError = (e && (e.message || String(e))) || 'unknown';
            },
          );
        }, ANNOUNCE_EVERY_MS);
      }
      return { ok: true };
    }

    case 'query': {
      // §9 step 1: A learns B's entity and Noise keys from B's SIGNED
      // announcement, by capability query, before any offer. This is
      // also why `connectPeer` takes a peer id and nothing else — a
      // page that could supply a Noise key could supply any key.
      //
      // The bounded retry here is not a widened wait around a flake:
      // announcement propagation is a step OF the sequence, and the
      // row's own failure mode for "it never arrived" is the typed
      // `noAnnouncement` this step would otherwise hide.
      if (!node) return { ok: false, detail: 'query before connect' };
      const deadline = performance.now() + (step.timeout_ms || 20000);
      let seen = [];
      let last = null;
      let attempts = 0;
      while (performance.now() < deadline) {
        attempts += 1;
        try {
          const peers = await node.query(step.capability);
          seen = typeof peers === 'string' ? JSON.parse(peers) : peers || [];
        } catch (e) {
          last = typed(e);
          seen = [];
        }
        const ids = seen.map((p) =>
          typeof p === 'string' ? p : p && (p.nodeId || p.node_id),
        );
        if (seen.some((p) => namesPeer(p, step.expect_peer))) {
          return { ok: true, peers: ids };
        }
        await new Promise((r) => setTimeout(r, 250));
      }
      // Everything the next reader needs without another cycle: what
      // the query DID return, how many times it was asked, whether
      // this side's own announcements are still landing, and the last
      // typed error if any call rejected.
      return {
        ok: false,
        detail:
          `peer ${step.expect_peer} never appeared in ${attempts} capability queries for ` +
          `${step.capability} over ${step.timeout_ms || 20000} ms ` +
          `(this side announced ok=${announces.ok} failed=${announces.failed}` +
          `${announces.lastError ? ' lastAnnounceError=' + announces.lastError : ''}` +
          `; last query returned ${safeJson(seen)})` +
          `${last ? ' (last query error: ' + last.detail + ')' : ''}`,
        peers: seen,
      };
    }

    case 'arm_accept': {
      // Armed BEFORE the offerer's step runs. `acceptPeer` answers
      // the VERIFIED offer envelope the leaf already holds, so it can
      // only be called once that envelope has arrived — which is what
      // the `signal` event announces.
      if (!node) return { ok: false, detail: 'arm_accept before connect' };
      if (armed) return { ok: true, already: true };
      let settle;
      armed = new Promise((r) => {
        settle = r;
      });
      const off = node.on('signal', (ev) => {
        let peerHex;
        try {
          peerHex = toPeerHex(ev.from);
        } catch (e) {
          log(`signal with unparseable from=${ev && ev.from}`);
          return;
        }
        if (answered.has(peerHex)) return;
        answered.add(peerHex);
        off();
        log(`signal from ${peerHex}, answering`);
        node.acceptPeer(peerHex).then(
          (r) => settle({ page_type: (r && r.type) || 'undefined', detail: safeJson(r) }),
          (e) => settle(typed(e)),
        );
      });
      return { ok: true };
    }

    case 'accept_result': {
      if (!armed) return { ok: false, detail: 'accept_result before arm_accept' };
      const timeout = new Promise((r) => setTimeout(() => r(null), step.timeout_ms || 60000));
      const out = await Promise.race([armed, timeout]);
      if (!out) {
        return {
          ok: false,
          detail:
            'no `signal` event from the peer arrived, so acceptPeer never ran — the offer ' +
            'never reached this leaf, which is a routing or forwarding failure and not an ICE one',
          counters: counters(),
          rtc: await iceReport(),
        };
      }
      return { ok: true, ...out, counters: counters(), rtc: await iceReport() };
    }

    case 'connect_peer': {
      if (!node) return { ok: false, detail: 'connect_peer before connect' };
      if (typeof node.connectPeer !== 'function') {
        return {
          ok: false,
          detail:
            'connectPeer is not a method on BrowserNode — the page-facing ' +
            'browser-to-browser surface is not present in this bundle',
        };
      }
      const started = performance.now();
      let out;
      try {
        const r = await node.connectPeer(step.peer);
        // The DISCRIMINANT, verbatim: `PeerConnectOutcome.type`. The
        // runner maps it to a disposition; a type that is neither
        // `direct` nor `iceTimeout` fails the row naming what it
        // actually got instead of being coerced into one of them.
        out = { ok: true, page_type: (r && r.type) || 'undefined', detail: safeJson(r) };
      } catch (e) {
        // `ok` stays true: a rejection is a reportable non-disposition,
        // not a failure of the step machinery.
        out = { ok: true, ...typed(e) };
      }
      out.elapsed_ms = performance.now() - started;
      out.counters = counters();
      out.rtc = await iceReport();
      return out;
    }

    // --- the application exchange -------------------------------
    //
    // A row's three original witnesses are all statements about a
    // DIALOG: the typed outcome, the ICE ledgers, the gateways'
    // conntrack. None of them observes an application payload, and
    // on a relayed row the disposition does not even claim one —
    // `iceTimeout` means the routed session was kept, not that a
    // byte crossed it.
    //
    // These two steps put nonce-correlated bytes on the public
    // peer-addressed stream surface in BOTH directions. The nonces
    // come from the runner: a page that minted its own could report
    // a value it had also sent, and "B decoded exactly what A sent"
    // would then be satisfiable by one side on its own.

    case 'app_arm': {
      // The ANSWERER's half, armed before the offerer sends
      // anything. `fireAndForget` keeps nothing for a receiver that
      // is not attached yet, so arming after the first frame would
      // lose it — the same ordering `arm_accept` needs and for the
      // same reason.
      if (!node) return { ok: false, detail: 'app_arm before connect' };
      if (app.stream) return { ok: true, already: true };
      try {
        app.stream = await openAppStream(step.peer, step.stream_id);
      } catch (e) {
        return { ok: false, ...typed(e) };
      }
      app.expect = step.expect_nonce;
      app.mine = step.nonce;
      app.stream.onMessage((payload) => {
        const seen = decodeNonce(payload);
        if (!seen) return;
        app.received += 1;
        if (seen !== app.expect) return;
        app.seen = seen;
        // THE ECHO. Sent from inside the receive callback so the
        // answer is caused by the arrival rather than by a timer
        // that might have fired either way — the reverse direction
        // has to be an answer to this exact frame.
        if (app.echoes < APP_FRAMES) {
          app.echoes += 1;
          app.stream.send(encodeNonce(app.mine)).then(
            () => {
              app.sent += 1;
            },
            (e) => {
              app.lastError = (e && (e.message || String(e))) || 'unknown';
            },
          );
        }
      });
      return { ok: true };
    }

    case 'app_send': {
      // The OFFERER's half: send this side's nonce until the peer's
      // comes back. A bounded loop rather than one shot because the
      // stream is `fireAndForget` — the reliability class the demo
      // and these rows both use — so a lost frame is a normal event
      // and not a verdict. It cannot rescue a wrong nonce: the
      // assertion is exact equality and is made in `rows.rs`.
      if (!node) return { ok: false, detail: 'app_send before connect' };
      if (!app.stream) {
        try {
          app.stream = await openAppStream(step.peer, step.stream_id);
        } catch (e) {
          return { ok: false, ...typed(e) };
        }
      }
      app.expect = step.expect_nonce;
      app.mine = step.nonce;
      app.stream.onMessage((payload) => {
        const seen = decodeNonce(payload);
        if (!seen) return;
        app.received += 1;
        if (seen === app.expect) app.seen = seen;
      });
      const deadline = performance.now() + (step.timeout_ms || 20000);
      while (performance.now() < deadline && !app.seen) {
        try {
          await app.stream.send(encodeNonce(app.mine));
          app.sent += 1;
        } catch (e) {
          app.lastError = (e && (e.message || String(e))) || 'unknown';
          break;
        }
        await new Promise((r) => setTimeout(r, APP_GAP_MS));
      }
      // `ok` regardless of whether the nonce came back: a missing
      // echo is a FINDING about the path, and the row fails on the
      // reported nonce in `rows.rs` rather than here, where the
      // message would carry none of the counters.
      return {
        ok: true,
        seen_nonce: app.seen || '',
        app_sent: app.sent,
        app_received: app.received,
        detail: app.lastError ? `last app stream error: ${app.lastError}` : '',
        counters: counters(),
      };
    }

    case 'app_result': {
      if (!app.stream) return { ok: false, detail: 'app_result before app_arm' };
      // Wait for the echo this side owes, bounded: the arrival that
      // triggers it and the send it causes are both asynchronous, so
      // the runner can ask before the callback has finished.
      const deadline = performance.now() + (step.timeout_ms || 5000);
      while (performance.now() < deadline && (!app.seen || app.sent === 0)) {
        await new Promise((r) => setTimeout(r, 50));
      }
      return {
        ok: true,
        seen_nonce: app.seen || '',
        app_sent: app.sent,
        app_received: app.received,
        detail: app.lastError ? `last app stream error: ${app.lastError}` : '',
        counters: counters(),
      };
    }

    case 'counters':
      return { ok: true, counters: counters() };

    case 'done':
      return { ok: true };

    default:
      return { ok: false, detail: `unknown step kind: ${step.kind}` };
  }
}

async function main() {
  if (!CONTROL) {
    await log('FATAL no ?control= base url');
    return;
  }
  await log('page up');
  for (;;) {
    let step;
    try {
      const res = await fetch(`${CONTROL}/matrix/step?tab=${encodeURIComponent(TAB)}`);
      if (res.status === 204) continue; // long-poll window elapsed
      step = await res.json();
    } catch (e) {
      await log(`step fetch failed: ${(e && e.message) || e}`);
      await new Promise((r) => setTimeout(r, 250));
      continue;
    }
    await log(`step ${step.id} ${step.kind}`);
    let result;
    try {
      result = await execute(step);
    } catch (e) {
      result = { ok: false, detail: `step threw: ${(e && e.message) || e}` };
    }
    // Posted as text/plain, deliberately: an `application/json`
    // content type makes this a non-simple cross-origin request and
    // requires a CORS preflight, which is machinery this harness has
    // no reason to own.
    await fetch(`${CONTROL}/matrix/result`, {
      method: 'POST',
      body: JSON.stringify({ id: step.id, tab: TAB, ...result }),
    });
    if (step.kind === 'done') {
      await log('done');
      return;
    }
  }
}

main().catch(async (e) => {
  await log(`FATAL ${(e && (e.message || String(e))) || 'unknown'}`);
});
