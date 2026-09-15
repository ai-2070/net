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
/// Calls issued by `call_begin` and awaited later by `call_await`.
/// Keyed by the runner's handle name; the value is a promise that
/// always fulfils, carrying either the reply or the typed failure, so
/// an unhandled rejection can never escape between the two steps.
const pending = new Map();
/// What an armed re-entrant listener observed, keyed by session name.
///
/// Kept outside `nodes` because the event that fires it is the
/// session's own teardown: the report has to be readable after the
/// node it belonged to is gone.
const reentry = new Map();
/// Streams opened by `stream_open`, keyed by the runner's handle.
///
/// Each entry holds the PACKAGE's `LeafStream` — the object
/// `BrowserNode.openStream` (direct) or `MeshSession.openStream`
/// (leader-proxied) returned — together with what its `onMessage`
/// callback and its `for await` async iterator each saw, in arrival
/// order. Both consumers are attached at open, before a single byte
/// can arrive, so "the callback and the iterator received the same
/// payloads" is a property of the package's fan-out and not of
/// whichever one happened to be registered first.
const streams = new Map();

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

/// The deterministic payload generator the RUNNER also implements,
/// byte for byte. Both sides compute the same bytes from `(seed,
/// len)`, so a multi-megabyte exercise costs no HTTP payload and the
/// comparison is still exact.
function genBytes(seed, len) {
  const out = new Uint8Array(len);
  for (let i = 0; i < len; i += 1) out[i] = (seed + i * 167 + (i >> 8) * 13) & 0xff;
  return out;
}

/// FNV-1a/32 over a payload, identical to the runner's. Reported
/// alongside the length, so a payload that arrived truncated,
/// duplicated, reordered or corrupted is a mismatch rather than a
/// count that happens to agree.
function fnv1a(bytes) {
  let h = 0x811c9dc5;
  for (let i = 0; i < bytes.length; i += 1) {
    h = (h ^ bytes[i]) >>> 0;
    h = Math.imul(h, 0x01000193) >>> 0;
  }
  return h >>> 0;
}

/// One payload, as the runner compares them.
function mark(payload) {
  return { len: payload.length, fnv: fnv1a(payload) };
}

// ---------------------------------------------------------------------
// the drop hook, and the reorder hook beside it
// ---------------------------------------------------------------------
//
// REORDER. The leaf opens its DataChannel ordered and reliable (the
// AEAD replay window refuses packet-level reorder), so SCTP will not
// reorder anything for us — which means the only honest way to
// deliver two Net packets to the peer out of order is to SUBMIT them
// out of order. With `reorderEvery = N` the Nth outbound message is
// held back and sent immediately AFTER the next one, so the receiver
// really does see seq k+1 before seq k. That is a genuine Net-level
// reorder on a transport that cannot produce one by itself, and it
// is engine-agnostic.
//
// A held datagram is never abandoned: `flushHeld()` runs at the end
// of every step that armed the hook, so "reordered" never silently
// becomes "lost".

const loss = { dropEvery: 0, seen: 0, dropped: 0 };
const reorder = { every: 0, seen: 0, swapped: 0, held: null };

(function installDropHook() {
  if (typeof RTCDataChannel === 'undefined') return;
  const original = RTCDataChannel.prototype.send;
  reorder.send = (channel, data) => original.call(channel, data);
  RTCDataChannel.prototype.send = function patched(data) {
    if (loss.dropEvery > 0) {
      loss.seen += 1;
      if (loss.seen % loss.dropEvery === 0) {
        loss.dropped += 1;
        return; // the datagram never leaves the browser
      }
    }
    if (reorder.every > 0) {
      reorder.seen += 1;
      if (reorder.held === null && reorder.seen % reorder.every === 0) {
        // Held back — the NEXT datagram overtakes it.
        reorder.held = { channel: this, data };
        reorder.swapped += 1;
        return;
      }
      if (reorder.held !== null) {
        const held = reorder.held;
        reorder.held = null;
        const out = original.call(this, data); // the later one first
        original.call(held.channel, held.data); // then the overtaken one
        return out;
      }
    }
    return original.call(this, data);
  };
})();

/// Release a datagram the reorder hook is still holding. Called at
/// the end of every step that armed it, so the last message of a
/// batch can never be silently dropped by the swap.
function flushHeld() {
  if (reorder.held !== null && typeof reorder.send === 'function') {
    const held = reorder.held;
    reorder.held = null;
    reorder.send(held.channel, held.data);
  }
  reorder.held = null;
}

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

    // A call that is issued now and awaited later, so the runner can
    // act on the tab — stand the leader down, for instance — while
    // the call is genuinely outstanding.
    case 'call_begin': {
      const node = nodes.get(step.session);
      if (!node) return { ok: false, error: 'no such session ' + step.session };
      const promise = node
        .call(step.service, unhex(step.payload), step.timeout_ms)
        .then((reply) => ({ settled: 'resolved', reply: hex(new Uint8Array(reply)) }))
        .catch((e) => ({ settled: 'rejected', failure: typedFailure(e) }));
      pending.set(step.handle, promise);
      return { ok: true, info: 'issued, not awaited' };
    }

    case 'call_await': {
      const promise = pending.get(step.handle);
      if (!promise) return { ok: false, error: 'no pending call ' + step.handle };
      pending.delete(step.handle);
      // Raced against a local timer: a promise that never settles is
      // a REPORTABLE outcome ("never settled"), not a hung step.
      const HUNG = Symbol('hung');
      const out = await Promise.race([promise, sleep(step.timeout_ms).then(() => HUNG)]);
      if (out === HUNG) {
        return { ok: false, error: 'the pending call never settled', info: 'never settled' };
      }
      if (out.settled === 'resolved') {
        return { ok: true, reply: out.reply, info: 'resolved' };
      }
      return { ...out.failure, info: 'rejected' };
    }

    // `calls` round trips on one session, so a multi-call assertion
    // costs one HTTP round trip rather than one per call.
    //
    // Two modes, two different properties. Sequential awaits each
    // reply before issuing the next, which is what makes the
    // anchor's arrival ORDER meaningful. Concurrent issues every
    // call first and awaits them together, so N requests are
    // outstanding at once and each reply has to reach its own
    // caller; `replies[i]` stays the reply to `payloads[i]` by
    // INDEX, so a reply delivered to the wrong pending call shows
    // up as a mismatch rather than being masked by arrival order.
    case 'call_many': {
      const node = nodes.get(step.session);
      if (!node) return { ok: false, error: 'no such session ' + step.session };
      if (step.concurrent) {
        const settled = await Promise.allSettled(
          step.payloads.map((payload) =>
            node.call(step.service, unhex(payload), step.timeout_ms),
          ),
        );
        const replies = [];
        let firstFailure = null;
        for (let i = 0; i < settled.length; i += 1) {
          const outcome = settled[i];
          if (outcome.status === 'fulfilled') {
            replies.push(hex(new Uint8Array(outcome.value)));
          } else {
            replies.push('');
            if (firstFailure === null) firstFailure = { index: i, reason: outcome.reason };
          }
        }
        if (firstFailure !== null) {
          const out = typedFailure(firstFailure.reason);
          out.replies = replies;
          out.info = 'concurrent call ' + firstFailure.index + ' of ' + settled.length + ' failed';
          return out;
        }
        return { ok: true, replies };
      }
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

    // -----------------------------------------------------------------
    // The REAL stream ABI: one stream from the built package, held
    // open across steps, with both of its consumers attached.
    //
    // `await node.openStream(...)` covers both surfaces without a
    // branch: `BrowserNode.openStream` is synchronous and
    // `MeshSession.openStream` is a promise, and awaiting a
    // non-promise is a no-op. Which one this is comes back in the
    // result as `proxied`, read from the object the package
    // actually returned rather than from what the runner asked for.
    // -----------------------------------------------------------------
    case 'stream_open': {
      const node = nodes.get(step.session);
      if (!node) return { ok: false, error: 'no such session ' + step.session };
      const opts = {
        reliability: step.reliable ? 'reliable' : 'fireAndForget',
        reliable: step.reliable,
      };
      if (step.label) opts.label = step.label;
      if (step.stream_id) opts.streamId = step.stream_id;
      if (step.channel_hash !== null && step.channel_hash !== undefined) {
        opts.channelHash = step.channel_hash;
      }
      // What the BUILT wasm makes of this exact options object,
      // read through the same parser `open_stream` uses. Reached
      // off the generated node's own constructor, so it is the
      // artifact the page loaded and not a second copy: a page that
      // has no direct wasm node (a follower) reports `null` here and
      // the runner gates that leg on the opened stream's own
      // answers instead.
      let effective = null;
      const generated = node.inner && node.inner.constructor;
      if (generated && typeof generated.effective_stream_options === 'function') {
        try {
          effective = JSON.parse(generated.effective_stream_options(opts));
        } catch (e) {
          effective = { error: (e && (e.message || String(e))) || 'unknown' };
        }
      }
      let stream;
      const openPromise = (() => {
        try {
          return node.openStream(opts);
        } catch (e) {
          return Promise.reject(e);
        }
      })();
      const proxied = typeof openPromise?.then === 'function';
      try {
        stream = await openPromise;
      } catch (e) {
        return typedFailure(e);
      }
      const state = { stream, node, callback: [], iterator: [], iteratorEnded: false };
      // Callback first, iterator second, both before any byte can
      // arrive: `LeafStream` hands buffered payloads to whichever
      // consumer registers first, so registering both up front is
      // what makes the two lists comparable.
      state.cancel = stream.onMessage((payload) => state.callback.push(mark(payload)));
      state.draining = (async () => {
        try {
          for await (const payload of stream) state.iterator.push(mark(payload));
        } catch (e) {
          state.iteratorError = (e && (e.message || String(e))) || 'unknown';
        }
        state.iteratorEnded = true;
      })();
      streams.set(step.handle, state);
      return {
        ok: true,
        stats: {
          stream_id: stream.streamId,
          reliability: stream.reliability,
          proxied,
          effective_options: effective,
        },
      };
    }

    // Send on a stream `stream_open` already opened, optionally with
    // the drop and reorder hooks armed. Payloads are either the
    // runner's verbatim frames (`frames`) or the shared deterministic
    // generator (`seed`/`size`/`count`), which is what keeps a
    // multi-megabyte exercise off the HTTP step channel.
    case 'stream_write': {
      const state = streams.get(step.handle);
      if (!state) return { ok: false, error: 'no such open stream ' + step.handle };
      loss.dropEvery = step.drop_every || 0;
      loss.seen = 0;
      loss.dropped = 0;
      reorder.every = step.reorder_every || 0;
      reorder.seen = 0;
      reorder.swapped = 0;
      reorder.held = null;
      const sent = [];
      const disarm = () => {
        flushHeld();
        loss.dropEvery = 0;
        reorder.every = 0;
      };
      try {
        if (step.frames && step.frames.length) {
          for (const frame of step.frames) {
            const payload = unhex(frame);
            await state.stream.send(payload);
            sent.push(mark(payload));
          }
        } else {
          for (let i = 0; i < step.count; i += 1) {
            const payload = genBytes(step.seed + i, step.size);
            await state.stream.send(payload);
            sent.push(mark(payload));
          }
        }
      } catch (e) {
        const dropped = loss.dropped;
        const swapped = reorder.swapped;
        disarm();
        const out = typedFailure(e);
        out.stats = { sent, dropped, reordered: swapped, failed_at: sent.length };
        return out;
      }
      const stats = {
        sent,
        dropped: loss.dropped,
        reordered: reorder.swapped,
        outbound_messages: Math.max(loss.seen, reorder.seen),
      };
      disarm();
      return { ok: true, stats };
    }

    // What the two consumers of an open stream have received so far.
    // Waits for `expect` payloads on the CALLBACK and then reports
    // both lists; a short list is the result, never a hang.
    case 'stream_inbox': {
      const state = streams.get(step.handle);
      if (!state) return { ok: false, error: 'no such open stream ' + step.handle };
      const deadline = performance.now() + (step.timeout_ms || 15000);
      while (
        performance.now() < deadline &&
        (state.callback.length < step.expect || state.iterator.length < step.expect)
      ) {
        await sleep(25);
      }
      // The leaf's own counters, sampled at the same instant, as the
      // RAW `counters_json()` string. A short list is then
      // attributable: a `stream_failed` event or an
      // `unknown_subprotocol` / `duplicate_sequence` / `no_session`
      // drop each say a different thing, and the nested `drops`
      // object is exactly what the flattened `counters()` record
      // loses.
      let counters = null;
      try {
        counters =
          state.node.inner && typeof state.node.inner.counters_json === 'function'
            ? state.node.inner.counters_json()
            : JSON.stringify(state.node.counters());
      } catch (e) {
        counters = 'ERROR ' + ((e && (e.message || String(e))) || 'unknown');
      }
      return {
        ok: true,
        stats: {
          callback: state.callback,
          iterator: state.iterator,
          iterator_ended: state.iteratorEnded,
          iterator_error: state.iteratorError || null,
          stream_id: state.stream.streamId,
          reliability: state.stream.reliability,
          counters,
        },
      };
    }

    // One `call()` whose request AND reply are large enough to
    // fragment, through the package's public API in both
    // directions. The bodies never cross the step channel: both
    // sides build them from `(seed, size)` and compare length + FNV.
    case 'call_sized': {
      const node = nodes.get(step.session);
      if (!node) return { ok: false, error: 'no such session ' + step.session };
      const body = genBytes(step.seed, step.size);
      const started = performance.now();
      try {
        const reply = new Uint8Array(await node.call(step.service, body, step.timeout_ms));
        return {
          ok: true,
          stats: {
            request: mark(body),
            reply: mark(reply),
            elapsed_ms: performance.now() - started,
          },
        };
      } catch (e) {
        const out = typedFailure(e);
        out.stats = { request: mark(body), elapsed_ms: performance.now() - started };
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

    // A listener on a DIRECT node that calls straight back into it.
    //
    // Both re-entrant calls are SYNCHRONOUS on the direct surface —
    // `counters()` reads the node, `openStream()` mutates it — so
    // each one lands inside whatever borrow the callback was invoked
    // under. A page doing this is ordinary ("log the final counters
    // when the session drops"), and the documented echo shape
    // (`onMessage(p => stream.send(p))`) is the same schedule. If the
    // events are dispatched while the node is borrowed, the first of
    // these traps the wasm module instead of answering.
    case 'arm_reentry': {
      const node = nodes.get(step.session);
      if (!node) return { ok: false, error: 'no such session ' + step.session };
      // Registered on the GENERATED wasm node, not on the wrapper's
      // event hub. Two reasons, both about testing the real thing:
      // the wrapper detaches its own listeners before it calls
      // `inner.close()`, so a hub listener never sees the teardown
      // event at all; and the callback path under test *is* the raw
      // `on_event`/`on_message` one — it is what the wrapper feeds
      // from and what `LeafStream.onMessage` registers.
      const wasmNode = node.inner && typeof node.inner.on_event === 'function' ? node.inner : null;
      if (!wasmNode) {
        return {
          ok: false,
          error: 'the generated wasm node is not reachable through the package object',
        };
      }
      const state = {
        fired: 0,
        counters_keys: 0,
        counters_error: null,
        stream_outcome: null,
        trapped: null,
      };
      reentry.set(step.session, state);
      wasmNode.on_event(() => {
        state.fired += 1;
        // One shot: the report is about the first re-entry, and a
        // second listener call must not overwrite what it saw.
        if (state.fired > 1) return;
        try {
          const counters = node.counters();
          state.counters_keys = Object.keys(counters || {}).length;
        } catch (e) {
          state.counters_error = (e && (e.message || String(e))) || 'unknown';
          state.trapped = 'counters';
          return;
        }
        try {
          node.openStream({ reliability: 'fireAndForget', reliable: false });
          state.stream_outcome = 'opened';
        } catch (e) {
          // A typed refusal is a correct answer here — the node is
          // closed by the time its teardown event is delivered. A
          // RefCell trap is not, and carries no `kind`.
          state.stream_outcome = (e && e.kind) || 'trapped';
          if (!(e && e.kind)) state.trapped = 'openStream';
        }
      });
      return { ok: true };
    }

    case 'reentry_report': {
      const state = reentry.get(step.session);
      if (!state) return { ok: false, error: 'nothing armed on ' + step.session };
      return { ok: true, info: JSON.stringify(state) };
    }

    case 'close': {
      const node = nodes.get(step.session);
      if (!node) return { ok: false, error: 'no such session ' + step.session };
      // Deliberately still addressable. A closed session that
      // refuses is the observation a retirement witness needs, and a
      // later `connect` under the same name replaces the entry
      // anyway.
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
