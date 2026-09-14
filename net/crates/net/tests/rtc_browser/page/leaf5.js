// Stage 5 harness page — the browser half, driven through
// `@net-mesh/browser`.
//
// The difference from `app.js` (the Stage 4b page) is the whole point
// of Stage 5: this page does NOT speak the bootstrap protocol, build
// packets, or run the handshake. It calls `connect()` and then uses
// the SDK surface — `call`, `openStream`, `publish`, `announce`,
// `query`. Everything below that is the leaf crate's job, and a
// witness here failing is the leaf failing.
//
// One tab = one `?tab=<name>` query parameter = one step queue on the
// runner (`GET /harness/step5?tab=<name>`). Results go back on the
// shared `/harness/result` keyed by step id, so two tabs can be
// driven independently and their results never cross.
//
// THE DROP HOOK
// -------------
// `RTCDataChannel.prototype.send` is patched, once, at module load.
// With `dropEvery = N` the Nth, 2Nth, … outbound DataChannel message
// is counted and NOT sent. That is transport-level loss injection:
// engine-agnostic, invisible to the leaf, and identical from the
// anchor's point of view to a datagram that was lost on the wire. It
// exists because Stage 4b has no drop hook — the brief said it did;
// the only "drop" in the 4b runner is a `close` step that tears a
// session down, which is a different thing.

import * as browserSdk from '/browser/index.js';

const { connect, probeStunBinding } = browserSdk;

/// §8's session entry point. Taken from the package's named export
/// when it has one and from the `NetMesh` global otherwise, so a
/// re-export landing later does not need a harness change. Absent
/// entirely → the step fails LOUDLY naming it, never silently.
function openSessionFn() {
  if (typeof browserSdk.openSession === 'function') return browserSdk.openSession;
  const ns = globalThis.NetMesh;
  if (ns && typeof ns.openSession === 'function') return ns.openSession.bind(ns);
  return null;
}

/// §8's leadership view, from whichever surface the object has.
/// `generation` is a decimal STRING because it is a u64.
function roleOf(node) {
  const out = {};
  if (typeof node.role === 'function') out.role = node.role();
  else if (typeof node.isLeader === 'function') out.role = node.isLeader() ? 'leader' : 'follower';
  if (typeof node.generation === 'function') out.generation = String(node.generation());
  return out;
}

/// This node's ORIGIN HASH, which is not its node id.
///
/// An event payload the runner encoded natively must carry this node's
/// origin in its `EventMeta`, because the anchor drops a direct peer's
/// frame whose packet-header origin and payload origin disagree
/// ("a direct peer must not run the fold under a forged payload
/// origin"). The header's origin is the leaf's; so the payload's must
/// be too, and the runner can only know it by being told.
/// Absent on a surface that does not expose it → `null`, and the
/// runner fails the witness naming it rather than sending a frame it
/// knows will be dropped.
function originOf(node) {
  return typeof node.originHashHex === 'function' ? node.originHashHex() : null;
}

const logEl = document.getElementById('log');
const TAB = new URLSearchParams(location.search).get('tab') || 'a';
const nodes = new Map();

function log(line) {
  const text = '[' + TAB + '] ' + line;
  console.log(text);
  logEl.textContent += text + '\n';
  return fetch('/harness/log', { method: 'POST', body: text + '\n' }).catch(() => {});
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

function hex(bytes) {
  return Array.from(bytes, (b) => b.toString(16).padStart(2, '0')).join('');
}

function unhex(s) {
  const out = new Uint8Array(s.length / 2);
  for (let i = 0; i < out.length; i++) out[i] = parseInt(s.substr(i * 2, 2), 16);
  return out;
}

// ---------------------------------------------------------------------
// the drop hook
// ---------------------------------------------------------------------

const loss = { dropEvery: 0, seen: 0, dropped: 0 };

(function installDropHook() {
  if (typeof RTCDataChannel === 'undefined') return;
  const original = RTCDataChannel.prototype.send;
  RTCDataChannel.prototype.send = function patched(data) {
    if (loss.dropEvery > 0) {
      loss.seen += 1;
      if (loss.seen % loss.dropEvery === 0) {
        loss.dropped += 1;
        return; // the datagram never leaves the browser
      }
    }
    return original.call(this, data);
  };
})();

// ---------------------------------------------------------------------
// error typing
// ---------------------------------------------------------------------
//
// `@net-mesh/browser` rejects with an error carrying `.kind` (flat
// kebab, one per Rust `LeafError` variant) and `.message` (verbatim
// the Rust `Display`). Both are reported: the runner asserts on the
// kind AND on the message prefix, so a wrapper that invented a kind
// or a Display that drifted are separately visible.
function typedFailure(e) {
  const out = {
    ok: false,
    error: (e && (e.message || String(e))) || 'unknown',
    kind: (e && e.kind) || null,
    message: (e && e.message) || null,
  };
  const evidence = e && e.failure && e.failure.evidence;
  if (evidence) out.evidence = evidence;
  return out;
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
        // The STUN probe that distinguishes `UdpBlocked` from
        // `IceTimeout` targets the anchor's published `rtc_addr`.
        // The leaf reads it from `GET /rtc/anchor`; passing it too
        // means the narrow type does not depend on that fetch
        // having landed before ICE gave up.
        anchorRtcAddr: step.anchor_rtc_addr,
      };
      // §8's IndexedDB identity does not exist yet, so the runner
      // injects one custodial keypair for the whole run and both
      // tabs present the same node id. Same API shape as
      // `MeshNodeConfig::entity_keypair`.
      if (step.entity_secret_hex) opts.entitySecretHex = step.entity_secret_hex;
      if (step.noise_secret_hex) opts.noiseSecretHex = step.noise_secret_hex;
      if (step.stun) opts.iceServers = [{ urls: step.stun }];
      // §8: `openSession` runs the Web Lock election and returns a
      // session that carries the same call/subscribe/publish/
      // announce/query/openStream/close surface, so every step
      // below works unchanged against either shape.
      if (step.use_session) {
        opts.capabilities = step.capabilities || [];
        opts.subscriptions = step.subscriptions || [];
        if (step.lock_scope) opts.lockScope = step.lock_scope;
      }
      const started = performance.now();
      let node;
      try {
        if (step.use_session) {
          const open = openSessionFn();
          if (!open) {
            return {
              ok: false,
              error:
                'openSession is not reachable — neither a named export of ' +
                '@net-mesh/browser nor globalThis.NetMesh.openSession',
            };
          }
          node = await open(opts);
        } else {
          node = await connect(opts);
        }
      } catch (e) {
        const out = typedFailure(e);
        out.elapsed_ms = performance.now() - started;
        // A negative step reaching its boundary is the RESULT, not
        // an error: `ok` stays false and the runner decides. A
        // POSITIVE step that failed is reported the same way and
        // the runner fails the witness.
        return out;
      }
      const elapsed = performance.now() - started;
      if (step.expect_failure) {
        // R7b: the boundary was not reached. A test error, never a
        // pass — the runner must be able to tell "connect refused
        // for the right reason" from "connect succeeded".
        nodes.set(step.session, node);
        return {
          ok: false,
          test_error: true,
          error: 'connect() SUCCEEDED where the step required a typed failure',
          node_id: node.nodeIdHex(),
          elapsed_ms: elapsed,
        };
      }
      nodes.set(step.session, node);
      const events = [];
      node.onEvent((ev) => {
        events.push(typeof ev === 'string' ? ev : ev.type || JSON.stringify(ev));
        if (events.length > 64) events.shift();
      });
      await log(
        'opened as ' +
          node.nodeIdHex() +
          ' in ' +
          elapsed.toFixed(0) +
          ' ms' +
          (typeof node.role === 'function' ? ' role=' + node.role() : ''),
      );
      return {
        ok: true,
        node_id: node.nodeIdHex(),
        origin_hash: originOf(node),
        elapsed_ms: elapsed,
        ...roleOf(node),
      };
    }

    case 'info': {
      const node = nodes.get(step.session);
      if (!node) return { ok: false, error: 'no such session ' + step.session };
      return {
        ok: true,
        node_id: node.nodeIdHex(),
        origin_hash: originOf(node),
        ...roleOf(node),
      };
    }

    case 'call': {
      const node = nodes.get(step.session);
      if (!node) return { ok: false, error: 'no such session ' + step.session };
      try {
        const reply = await node.call(step.service, unhex(step.payload), step.timeout_ms);
        return { ok: true, reply: hex(new Uint8Array(reply)) };
      } catch (e) {
        return typedFailure(e);
      }
    }

    // `calls` sequential round trips on one session, so a 64-call
    // ordering assertion costs one HTTP round trip rather than 64.
    case 'call_many': {
      const node = nodes.get(step.session);
      if (!node) return { ok: false, error: 'no such session ' + step.session };
      const replies = [];
      for (const payload of step.payloads) {
        try {
          const reply = await node.call(step.service, unhex(payload), step.timeout_ms);
          replies.push(hex(new Uint8Array(reply)));
        } catch (e) {
          const out = typedFailure(e);
          out.replies = replies;
          out.info = 'failed on call ' + replies.length;
          return out;
        }
      }
      return { ok: true, replies };
    }

    case 'stream_send': {
      const node = nodes.get(step.session);
      if (!node) return { ok: false, error: 'no such session ' + step.session };
      const opts = {
        reliability: step.reliable ? 'reliable' : 'fireAndForget',
        reliable: step.reliable,
      };
      // The publish contract's ids, so the anchor DISPATCHES these
      // events to its real handler instead of merely receiving
      // them. Without them a fire-and-forget stream is observable
      // only as a counter, which is not evidence.
      if (step.stream_id) opts.streamId = step.stream_id;
      if (step.channel_hash !== null && step.channel_hash !== undefined) {
        opts.channelHash = step.channel_hash;
      }
      loss.dropEvery = step.drop_every || 0;
      loss.seen = 0;
      loss.dropped = 0;
      try {
        const stream = node.openStream(opts);
        let sent = 0;
        for (const payload of step.payloads) {
          await stream.send(unhex(payload));
          sent += 1;
        }
        if (typeof stream.flush === 'function') await stream.flush();
        const dropped = loss.dropped;
        loss.dropEvery = 0;
        return { ok: true, sent_frames: sent, dropped, info: 'drop hook saw ' + loss.seen + ' outbound messages' };
      } catch (e) {
        loss.dropEvery = 0;
        const out = typedFailure(e);
        out.dropped = loss.dropped;
        return out;
      }
    }

    case 'announce': {
      const node = nodes.get(step.session);
      if (!node) return { ok: false, error: 'no such session ' + step.session };
      try {
        await node.announce(step.capabilities);
        return { ok: true };
      } catch (e) {
        return typedFailure(e);
      }
    }

    // The falsifier for the `UdpBlocked` typing: does the anchor's
    // published `rtc_addr` answer an UNAUTHENTICATED STUN binding
    // when UDP is open? `reflexive` or `stunError` means the probe
    // can tell blocked from timed-out; `unanswered` against a
    // healthy anchor would make every ICE timeout look like blocked
    // UDP, and the runner fails the witness on exactly that.
    case 'stun_probe': {
      if (typeof probeStunBinding !== 'function') {
        return { ok: false, error: 'probeStunBinding is not exported by @net-mesh/browser' };
      }
      try {
        const outcome = await probeStunBinding(step.addr);
        const type = (outcome && (outcome.type || outcome.kind)) || String(outcome);
        await log('stun probe ' + step.addr + ' -> ' + type);
        return { ok: true, info: type };
      } catch (e) {
        return typedFailure(e);
      }
    }

    case 'query': {
      const node = nodes.get(step.session);
      if (!node) return { ok: false, error: 'no such session ' + step.session };
      try {
        const peers = await node.query(step.capability);
        return { ok: true, peers: typeof peers === 'string' ? JSON.parse(peers) : peers };
      } catch (e) {
        return typedFailure(e);
      }
    }

    case 'close': {
      const node = nodes.get(step.session);
      if (!node) return { ok: false, error: 'no such session ' + step.session };
      nodes.delete(step.session);
      try {
        node.close();
      } catch (e) {
        return typedFailure(e);
      }
      return { ok: true };
    }

    case 'idle':
      await sleep(step.millis || 50);
      return { ok: true };

    case 'done':
      return { ok: true };

    default:
      return { ok: false, error: 'unknown stage 5 step kind ' + step.kind };
  }
}

// ---------------------------------------------------------------------
// main loop
// ---------------------------------------------------------------------

async function main() {
  await log('stage 5 page up; @net-mesh/browser loaded');
  for (;;) {
    let step;
    try {
      const res = await fetch('/harness/step5?tab=' + encodeURIComponent(TAB));
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
