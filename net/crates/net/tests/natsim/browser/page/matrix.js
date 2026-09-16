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
      if (step.stun) opts.iceServers = [{ urls: step.stun }];
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
      while (performance.now() < deadline) {
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
        if (ids.includes(step.expect_peer)) return { ok: true, peers: ids };
        await new Promise((r) => setTimeout(r, 250));
      }
      return {
        ok: false,
        detail:
          `peer ${step.expect_peer} never appeared in a capability query for ` +
          `${step.capability}${last ? ' (last error: ' + last.detail + ')' : ''}`,
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
