// Stage 6 slice 2 — the page half of the §10 three-part
// direct-path witness.
//
// A separate page from `leaf5.js` on purpose, and the separation is
// not cosmetic: this page's job is to put **application data** on a
// leaf ↔ leaf session and to be able to take the direct path away
// again, which is a different vocabulary from Stage 5's
// call/publish/stream-to-the-anchor page. Every step here is one
// observable of that witness and nothing else.
//
// One tab = one `?tab=<name>` query parameter = one step queue on the
// runner (`GET /harness/step6?tab=<name>`). Results go back on the
// shared `/harness/result` keyed by step id, exactly like the Stage
// 4b and Stage 5 pages, so three tabs are driven independently and
// their results never cross.
//
// WHAT CARRIES THE APPLICATION DATA
// ---------------------------------
// `openStream({ peer })` — the peer-addressed stream. The leaf's
// `route_outbound` decides routed-vs-direct underneath, which is the
// whole point: the page asks for the same stream either way and the
// ANCHOR's per-pair counter is what says which path the bytes took.
//
// A payload is `n:<nonce>`; a receiver that has an `echo` stream
// answers `e:<nonce>` on the same stream id. So "the bytes arrived"
// is asserted at the RECEIVER (its inbox names the nonce) and again
// at the sender (the echo comes back), never inferred from a counter.
// A counter that moves and a payload that arrives are different
// facts and the witness needs both.
//
// WHY THE DATACHANNELS ARE TRACKED
// --------------------------------
// §10 part 3 needs the direct session forced DOWN from the page.
// `RTCPeerConnection.prototype.createDataChannel` is patched once, at
// module load, and every channel this page's leaf creates is recorded
// in creation order. Index 0 is the channel `connect()` built to the
// ANCHOR — it is created before any peer attempt exists — so
// `channels { close: true }` closes every channel above it and leaves
// the anchor path alone. That claim is not taken on trust: the
// witness's own restoration leg only works if the anchor channel
// survived, so a close that took the wrong one fails the row.

import * as browserSdk from '/browser/index.js';

const { connect } = browserSdk;

const logEl = document.getElementById('log');
const TAB = new URLSearchParams(location.search).get('tab') || 'f';
const nodes = new Map();
/// Streams opened by `stream_open`, keyed by the runner's handle.
///
/// Holds the package's `LeafStream` together with what arrived on
/// it, split by prefix: `data` for `n:` payloads, `echoes` for `e:`
/// answers. Both are DRAINED by `stream_inbox`, so each read is
/// "what arrived since the last read" and a witness cannot pass on
/// a payload an earlier phase already accounted for.
const streams = new Map();
/// Every `RTCDataChannel` this page's leaf created, in creation
/// order. See the header: index 0 is the anchor's.
const channels = [];

const enc = new TextEncoder();
const dec = new TextDecoder();

async function log(line) {
  const text = `[${TAB}] ${line}`;
  if (logEl) logEl.textContent += text + '\n';
  await fetch('/harness/log', { method: 'POST', body: text }).catch(() => {});
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

(function trackDataChannels() {
  const proto = RTCPeerConnection.prototype;
  const create = proto.createDataChannel;
  proto.createDataChannel = function patched(...args) {
    const channel = create.apply(this, args);
    channels.push({ index: channels.length, label: channel.label, channel });
    return channel;
  };
})();

/// The generated wasm node behind the `BrowserNode` wrapper.
///
/// Reached the one way `leaf5.js` reaches it — `.inner` — because the
/// four `#[wasm_bindgen]` peer methods are what this witness drives:
/// it has to be able to stop between `peer_offer` and `peer_candidate`
/// (to send routed application data while the pair is still relayed)
/// and between `peer_candidate` and `peer_handshake`. `connectPeer`
/// is a loop over them and cannot be interrupted in the middle.
///
/// Keyed on `peer_offer` rather than on `inner` merely existing, so a
/// build whose peer methods did not land fails BY NAME instead of
/// throwing `undefined is not a function` from inside a step.
function wasmNodeOf(node) {
  return node && node.inner && typeof node.inner.peer_offer === 'function' ? node.inner : null;
}

const NO_WASM_NODE = 'the generated wasm node with the peer methods is not reachable';

/// `@net-mesh/browser` rejects with an error carrying `.kind` (flat
/// kebab, one per Rust `LeafError` variant) and `.message` (verbatim
/// the Rust `Display`). Both are reported: a refusal this witness
/// asserts on is the LEAF's own words, not a string this page made
/// up.
function typedFailure(e) {
  return {
    ok: false,
    kind: (e && e.kind) || null,
    message: (e && e.message) || String(e),
    error: (e && e.message) || String(e),
  };
}

/// `query`'s descriptors, verbatim in the spelling
/// `@net-mesh/browser`'s `parseDescriptors` hands the page —
/// `nodeId` an exact DECIMAL string, because `JSON.parse` rounds
/// above 2^53 and a rounded node id names the wrong node. The
/// runner reads them with the same reader the slice 1 rows use.
function descriptorsOf(list) {
  return list.map((d) => ({
    nodeId: d.nodeId,
    entityId: d.entityId,
    noisePubkey: d.noisePubkey,
    version: d.version,
    capabilities: d.capabilities,
  }));
}

function channelRows() {
  return channels.map((c) => ({
    index: c.index,
    label: c.label,
    state: c.channel.readyState,
  }));
}

// ---------------------------------------------------------------------
// steps
// ---------------------------------------------------------------------

async function execute(step) {
  switch (step.kind) {
    case 'connect': {
      const opts = {
        credentialB64: step.credential,
        bootstrapUrl: step.bootstrap_url,
        origin: step.origin,
        anchorRtcAddr: step.anchor_rtc_addr,
      };
      // §8's IndexedDB identity exists, but a custodial keypair per
      // tab is what makes a failure reproducible and what keeps
      // three tabs from ever sharing one node id.
      if (step.entity_secret_hex) opts.entitySecretHex = step.entity_secret_hex;
      if (step.noise_secret_hex) opts.noiseSecretHex = step.noise_secret_hex;
      if (step.stun) opts.iceServers = [{ urls: step.stun }];
      let node;
      try {
        // `connect`, never `openSession`: a leader-proxied
        // `openStream` REFUSES a peer address (the Stage 5 leader
        // request protocol carries no peer), so the session surface
        // cannot express this witness at all.
        node = await connect(opts);
      } catch (e) {
        return typedFailure(e);
      }
      nodes.set(step.session, node);
      await log('opened as ' + node.nodeIdHex());
      return { ok: true, node_id: node.nodeIdHex() };
    }

    case 'announce': {
      const node = nodes.get(step.session);
      if (!node) return { ok: false, error: 'no such session ' + step.session };
      try {
        await node.announce(step.capabilities || []);
        return { ok: true };
      } catch (e) {
        return typedFailure(e);
      }
    }

    case 'query': {
      const node = nodes.get(step.session);
      if (!node) return { ok: false, error: 'no such session ' + step.session };
      try {
        const found = await node.query(step.capability);
        return { ok: true, peers: descriptorsOf(found) };
      } catch (e) {
        return typedFailure(e);
      }
    }

    // ---- the four `#[wasm_bindgen]` methods, one call each --------

    case 'offer': {
      const node = nodes.get(step.session);
      if (!node) return { ok: false, error: 'no such session ' + step.session };
      const wasm = wasmNodeOf(node);
      if (!wasm) return { ok: false, error: NO_WASM_NODE };
      try {
        return { ok: true, stats: { dialog: await wasm.peer_offer(step.peer_hex) } };
      } catch (e) {
        return typedFailure(e);
      }
    }

    case 'answer': {
      const node = nodes.get(step.session);
      if (!node) return { ok: false, error: 'no such session ' + step.session };
      const wasm = wasmNodeOf(node);
      if (!wasm) return { ok: false, error: NO_WASM_NODE };
      try {
        return { ok: true, stats: { dialog: await wasm.peer_accept_offer(step.peer_hex) } };
      } catch (e) {
        return typedFailure(e);
      }
    }

    case 'candidate': {
      const node = nodes.get(step.session);
      if (!node) return { ok: false, error: 'no such session ' + step.session };
      const wasm = wasmNodeOf(node);
      if (!wasm) return { ok: false, error: NO_WASM_NODE };
      try {
        const raw = await wasm.peer_candidate(step.peer_hex);
        return { ok: true, stats: JSON.parse(raw), info: raw };
      } catch (e) {
        return typedFailure(e);
      }
    }

    case 'handshake': {
      const node = nodes.get(step.session);
      if (!node) return { ok: false, error: 'no such session ' + step.session };
      const wasm = wasmNodeOf(node);
      if (!wasm) return { ok: false, error: NO_WASM_NODE };
      try {
        await wasm.peer_handshake(step.peer_hex);
        return { ok: true };
      } catch (e) {
        return typedFailure(e);
      }
    }

    // ---- the application data itself ------------------------------

    // Open (or REOPEN) a peer-addressed stream under `handle`.
    //
    // A reopen closes the previous `LeafStream` first, and that is
    // load-bearing twice over. The package registers an
    // `on_message` filter per stream object, so a second object on
    // the same id would deliver every payload twice — and an `echo`
    // stream would answer twice. And §9 step 4 REPLACES the
    // session, which fences the old handle by incarnation, so after
    // a pair goes direct a reopen is the only thing that can send
    // at all. The runner asks for it explicitly rather than this
    // page retrying underneath: the refusal on the stale handle is
    // evidence the witness reports.
    case 'stream_open': {
      const node = nodes.get(step.session);
      if (!node) return { ok: false, error: 'no such session ' + step.session };
      const previous = streams.get(step.handle);
      if (previous) {
        try {
          previous.stream.close();
        } catch (e) {
          await log('closing the previous stream on ' + step.handle + ' threw: ' + e);
        }
      }
      let stream;
      try {
        stream = await node.openStream({
          reliability: step.reliable ? 'reliable' : 'fireAndForget',
          label: step.label,
          peer: step.peer_hex,
        });
      } catch (e) {
        return typedFailure(e);
      }
      const state = previous || {
        data: [],
        echoes: [],
        other: [],
        echoFailures: [],
        opens: 0,
      };
      state.stream = stream;
      state.echo = !!step.echo;
      state.opens += 1;
      streams.set(step.handle, state);
      stream.onMessage((payload) => {
        const text = dec.decode(payload);
        if (text.startsWith('n:')) {
          const nonce = text.slice(2);
          state.data.push(nonce);
          if (state.echo) {
            // Answered on the stream that is CURRENT, not the one
            // captured when this listener was registered: a reopen
            // replaces the handle and an echo on the fenced one
            // would be refused.
            Promise.resolve(state.stream.send(enc.encode('e:' + nonce))).catch((e) => {
              state.echoFailures.push((e && e.message) || String(e));
            });
          }
        } else if (text.startsWith('e:')) {
          state.echoes.push(text.slice(2));
        } else {
          state.other.push(text);
        }
      });
      return {
        ok: true,
        stats: {
          stream_id: stream.streamId,
          reliability: stream.reliability,
          opens: state.opens,
          echo: state.echo,
        },
      };
    }

    case 'stream_send': {
      const state = streams.get(step.handle);
      if (!state) return { ok: false, error: 'no such open stream ' + step.handle };
      let sent = 0;
      for (const nonce of step.nonces || []) {
        try {
          await state.stream.send(enc.encode('n:' + nonce));
        } catch (e) {
          // Reported, never retried: a refused send is the fact a
          // witness asserts on (a fenced handle, a channel that is
          // gone), and a retry here would erase it.
          const out = typedFailure(e);
          out.stats = { sent, attempted: (step.nonces || []).length };
          return out;
        }
        sent += 1;
      }
      return { ok: true, stats: { sent } };
    }

    // Drain what arrived since the last read, waiting up to
    // `timeout_ms` for `expect_data` payloads and `expect_echo`
    // echoes. Always `ok`: the counts are the observable and the
    // runner decides. A witness that needs "nothing arrived" asks
    // for one and gets zero.
    case 'stream_inbox': {
      const state = streams.get(step.handle);
      if (!state) return { ok: false, error: 'no such open stream ' + step.handle };
      const wantData = step.expect_data || 0;
      const wantEcho = step.expect_echo || 0;
      const started = performance.now();
      const deadline = started + (step.timeout_ms || 5000);
      while (
        (state.data.length < wantData || state.echoes.length < wantEcho) &&
        performance.now() < deadline
      ) {
        await sleep(25);
      }
      const data = state.data.splice(0, state.data.length);
      const echoes = state.echoes.splice(0, state.echoes.length);
      const other = state.other.splice(0, state.other.length);
      const echoFailures = state.echoFailures.splice(0, state.echoFailures.length);
      return {
        ok: true,
        stats: {
          data,
          echoes,
          other,
          echo_failures: echoFailures,
          waited_ms: Math.round(performance.now() - started),
          stream_id: state.stream.streamId,
        },
      };
    }

    // Force the direct path down, or just report it. `keep` is how
    // many leading channels are left alone; 1 by default, which is
    // the anchor's.
    case 'channels': {
      const before = channelRows();
      let closed = 0;
      if (step.close) {
        const keep = step.keep === undefined ? 1 : step.keep;
        for (const entry of channels) {
          if (entry.index < keep) continue;
          const state = entry.channel.readyState;
          if (state === 'open' || state === 'connecting') {
            entry.channel.close();
            closed += 1;
          }
        }
      }
      // One turn, so `readyState` has settled from `closing`.
      await sleep(50);
      return { ok: true, stats: { closed, before, after: channelRows() } };
    }

    case 'counters': {
      const node = nodes.get(step.session);
      if (!node) return { ok: false, error: 'no such session ' + step.session };
      return { ok: true, stats: { counters: node.counters() } };
    }

    case 'close': {
      const node = nodes.get(step.session);
      if (!node) return { ok: false, error: 'no such session ' + step.session };
      node.close();
      return { ok: true };
    }

    case 'idle':
      await sleep(step.millis || 50);
      return { ok: true };

    case 'done':
      return { ok: true };

    default:
      return { ok: false, error: 'unknown stage 6 step kind ' + step.kind };
  }
}

// ---------------------------------------------------------------------
// main loop
// ---------------------------------------------------------------------

async function main() {
  await log('stage 6 witness page up; @net-mesh/browser loaded');
  for (;;) {
    let step;
    try {
      const res = await fetch('/harness/step6?tab=' + encodeURIComponent(TAB));
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
      await log('done');
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
      result = { ok: false, error: (e && (e.message || String(e))) || 'unknown' };
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
  await log('FATAL ' + (e && (e.message || String(e))));
});
