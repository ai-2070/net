// Stage 4 (S4Browser) — the org-scoped streaming matrix's browser side.
//
// One tab = one `?tab=<name>` query parameter = one step queue on the
// runner (`GET /harness/stepOrg?tab=<name>`). Results go back on the
// shared `/harness/result` keyed by step id, exactly like the Stage
// 4b/5/6 pages, so every tab is driven independently and their
// results never cross.
//
// This page drives ONLY the org verbs of the browser SDK surface
// (`callOrg` / `callOrgStreaming` / `callOrgClientStream` /
// `callOrgDuplex` / `serveOrg` / `serveOrgStreaming` /
// `serveOrgClientStream` / `serveOrgDuplex`, and the same eight on a
// leader `MeshSession`). Every step fails LOUDLY, by verb name, when a
// verb the runner asked for is absent from the built SDK: a silent
// `undefined is not a function` would read as a protocol failure
// rather than as "lane D's surface has not landed".
//
// The five verified attribution fields (`OrgCaller`: entity,
// actingOrg, providerOrg, provider, capability) are recorded per
// handler invocation EXACTLY as delivered — the witness assertions
// live in the runner and compare against exact identities.

import * as browserSdk from '/browser/index.js';

const logEl = document.getElementById('log');
const TAB = new URLSearchParams(location.search).get('tab') || 'o1';

// THE FEED POINTER (the `window.__netChangeArmed` precedent: a page
// published hook the leaf reads). `anchor_control_plane.rs::bind`
// picks this up and opens the revocation feed's control socket at
// it; without it the feed is "no feed" and every cert verifies
// against implicit floor 0.
globalThis.__netOrgControl =
  location.origin.replace(/^http/, 'ws') + '/harness/org-control?node=' + TAB;

const nodes = new Map(); // session handle -> BrowserNode | MeshSession
const surfaceKinds = new Map(); // session handle -> 'node' | 'session'
const serves = new Map(); // serve handle -> serve state
const calls = new Map(); // call handle -> in-flight call state

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function log(text) {
  if (logEl) logEl.textContent += text + '\n';
  await fetch('/harness/log', { method: 'POST', body: text }).catch(() => {});
}

const hex = (bytes) =>
  [...new Uint8Array(bytes)].map((b) => b.toString(16).padStart(2, '0')).join('');

function unhex(s) {
  const out = new Uint8Array((s || '').length / 2);
  for (let i = 0; i < out.length; i++) out[i] = parseInt(s.substr(i * 2, 2), 16);
  return out;
}

// THE TYPED FAILURE SHAPE. A refusal this stage asserts on is the
// SDK's own words: `kind` is the LeafError taxonomy discriminator,
// `coarse` is OrgAdmissionDeniedError's frozen coarse reason
// ('denied' | 'not-supported' | 'unavailable'), `error_class` is the
// thrown class name, and `message` is verbatim.
function typedFailure(e) {
  return {
    ok: false,
    kind: (e && e.kind) || null,
    message: (e && e.message) || String(e),
    // `coarse` and `error_class` ride `stats`, which is the
    // StepResult field that carries structured detail.
    stats: {
      coarse: (e && e.coarse) || null,
      error_class: (e && e.constructor && e.constructor.name) || null,
    },
  };
}

function wasmNodeOf(node) {
  // The generated wasm node, reached through the wrapper — same seam
  // leaf5.js/peer6.js use, keyed on a wasm-only method so a build
  // without it fails BY NAME.
  const inner = node && node.inner;
  if (inner && typeof inner.peer_offer === 'function') return inner;
  return null;
}

// Every verb name of the org surface, checked up front so a missing
// one names itself.
const ORG_VERBS = [
  'callOrg',
  'callOrgStreaming',
  'callOrgClientStream',
  'callOrgDuplex',
  'serveOrg',
  'serveOrgStreaming',
  'serveOrgClientStream',
  'serveOrgDuplex',
];

function requireVerb(surface, verb) {
  if (!surface || typeof surface[verb] !== 'function') {
    throw new Error(
      `the built SDK surface exposes no ${verb}() — the org verbs of the ` +
        `browser SDK have not landed in @net-mesh/browser (lane D)`,
    );
  }
  return surface[verb].bind(surface);
}

function credsOf(raw) {
  // The wire bytes are hex at the step boundary and Uint8Array at the
  // verb boundary; the ids are hex strings, verbatim.
  const credentials = {
    membership: unhex(raw.membership_hex),
    dispatcher: unhex(raw.dispatcher_hex),
    actingOrg: raw.acting_org,
    providerOwnerOrg: raw.provider_owner_org,
    provider: raw.provider,
  };
  if (raw.capability_grant_hex) {
    credentials.capabilityGrant = unhex(raw.capability_grant_hex);
  }
  if (raw.proof_ttl_secs != null) credentials.proofTtlSecs = raw.proof_ttl_secs;
  return credentials;
}

function optsOf(raw) {
  const opts = { credentials: credsOf(raw.creds) };
  if (raw.deadline_ms != null) opts.deadlineMs = raw.deadline_ms;
  if (raw.stream_window_initial != null) opts.streamWindowInitial = raw.stream_window_initial;
  if (raw.request_window_initial != null) opts.requestWindowInitial = raw.request_window_initial;
  return opts;
}

function callerFacts(caller) {
  // EXACTLY the five verified attribution fields, plus the same-org
  // projection. Nothing here is caller-claimed page-side; these are
  // what the leaf's admission gate verified and handed up.
  return {
    entity: caller.entity,
    acting_org: caller.actingOrg,
    provider_org: caller.providerOrg,
    provider: caller.provider,
    capability: caller.capability,
    is_same_org: !!caller.isSameOrg,
  };
}

function surfaceOf(handle) {
  const surface = nodes.get(handle);
  if (!surface) throw new Error('no such session ' + handle);
  return surface;
}

// ───────────────────────── serve registration ─────────────────────────
//
// Each shape's handler records: the exact request payload bytes, the
// five attribution fields of every invocation, per-send resolution
// times (the backpressure observable), and the retirement observable
// (`retired` resolving BEFORE handler completion). `hold` parks the
// handler between `pre` and `post` phases so the runner can raise
// floors mid-stream.

function serveState(handle) {
  const previous = serves.get(handle);
  if (previous) {
    try {
      previous.close();
    } catch (e) {
      /* a serve that would not close is reported by the next step */
    }
    serves.delete(handle);
  }
  const state = {
    calls: [],
    close: null,
    holds: [],
    release: () => {},
  };
  serves.set(handle, state);
  return state;
}

function recordInvocation(state, phase, caller, requestHex) {
  const facts = callerFacts(caller);
  const entry = {
    phase,
    payload: requestHex,
    entity: facts.entity,
    acting_org: facts.acting_org,
    provider_org: facts.provider_org,
    provider: facts.provider,
    capability: facts.capability,
    is_same_org: facts.is_same_org,
    at: performance.now(),
    items: [],
    send_log: [],
    retired_at: null,
    completed_at: null,
    result_hex: null,
  };
  state.calls.push(entry);
  return entry;
}

function trackedSink(entry, sink) {
  return {
    async send(chunk) {
      const sentAt = performance.now();
      const index = entry.items.length;
      entry.items.push(hex(chunk));
      await sink.send(chunk);
      entry.send_log.push({ i: index, sent_at: sentAt, resolved_at: performance.now() });
    },
    async close() {
      await sink.close();
    },
    get retired() {
      return sink.retired.then((reason) => {
        entry.retired_at = performance.now();
        return reason;
      });
    },
  };
}

async function execute(step) {
  switch (step.kind) {
    case 'connect': {
      const opts = {
        credentialB64: step.credential,
        bootstrapUrl: step.bootstrap_url,
        origin: step.origin,
        // The failure-typing probe subject (leaf5.js's exact shape):
        // without it an ICE timeout can never be narrowed to
        // udp-blocked.
        anchorRtcAddr: step.anchor_rtc_addr,
      };
      if (step.entity_secret_hex) opts.entitySecretHex = step.entity_secret_hex;
      if (step.noise_secret_hex) opts.noiseSecretHex = step.noise_secret_hex;
      // ABSENCE, NOT EMPTINESS: when the runner measured no working
      // stun config it passes no `stun`, and iceServers must then be
      // ABSENT so the anchor-announced default applies. A present-but-
      // empty `[{urls: []}]` defeats that substitution (leaf5.js's
      // exact rule — this page's earlier `else` branch did exactly the
      // defeating and ICE never paired).
      if (step.stun) opts.iceServers = [{ urls: step.stun }];
      if (step.lock_scope && typeof browserSdk.openSession === 'function') {
        const session = await browserSdk.openSession({
          ...opts,
          lockScope: step.lock_scope,
          capabilities: step.capabilities || [],
          subscriptions: step.subscriptions || [],
        });
        nodes.set(step.session, session);
        surfaceKinds.set(step.session, 'session');
        await log(`opened session ${step.session} role=${session.role && session.role()}`);
        return {
          ok: true,
          node_id: session.nodeIdHex ? session.nodeIdHex() : null,
          role: session.role ? session.role() : null,
          generation: session.generation ? session.generation() : null,
        };
      }
      const node = await browserSdk.connect(opts);
      nodes.set(step.session, node);
      surfaceKinds.set(step.session, 'node');
      await log(`opened as ${node.nodeIdHex()}`);
      return { ok: true, node_id: node.nodeIdHex(), origin_hash: node.originHashHex() };
    }

    case 'info': {
      const surface = surfaceOf(step.session);
      return {
        ok: true,
        node_id: surface.nodeIdHex ? surface.nodeIdHex() : null,
        origin_hash: surface.originHashHex ? surface.originHashHex() : null,
        role: surface.role ? surface.role() : null,
        generation: surface.generation ? surface.generation() : null,
        stats: {
          verbs: ORG_VERBS.filter((v) => typeof surface[v] === 'function'),
          missing_verbs: ORG_VERBS.filter((v) => typeof surface[v] !== 'function'),
        },
      };
    }

    case 'announce': {
      const surface = surfaceOf(step.session);
      try {
        await surface.announce(step.capabilities || []);
        return { ok: true };
      } catch (e) {
        return typedFailure(e);
      }
    }

    case 'query': {
      const surface = surfaceOf(step.session);
      try {
        const found = await surface.query(step.capability);
        return {
          ok: true,
          peers: found.map((d) => ({
            nodeId: d.nodeId,
            peerIdHex: d.peerIdHex,
            entityId: d.entityId,
            noisePubkey: d.noisePubkey,
            version: d.version,
          })),
        };
      } catch (e) {
        return typedFailure(e);
      }
    }

    // ── the direct-session drive (the §9 four wasm methods) ──
    case 'peer_offer':
    case 'peer_accept_offer':
    case 'peer_candidate':
    case 'peer_handshake': {
      const surface = surfaceOf(step.session);
      const wasm = wasmNodeOf(surface);
      if (!wasm) return { ok: false, error: 'no wasm node under ' + step.session };
      try {
        const method = step.kind;
        // The raw result is a DIALOG STRING (offer/accept/handshake)
        // or a JSON attempt READING (candidate) — peer6.js's exact
        // shape: parse when it parses, keep the raw when it does not.
        const raw = await wasm[method](step.peer_hex);
        let parsed;
        try {
          parsed = typeof raw === 'string' ? JSON.parse(raw) : raw;
        } catch (e) {
          parsed = { raw: String(raw) };
        }
        return { ok: true, stats: parsed, info: String(raw) };
      } catch (e) {
        return typedFailure(e);
      }
    }

    // ───────────────────────── serve side ─────────────────────────
    case 'org_serve': {
      const surface = surfaceOf(step.session);
      const state = serveState(step.handle);
      const shape = step.shape; // 'unary' | 'streaming' | 'client_stream' | 'duplex'
      const access = step.access; // 'same-org' | 'granted'
      const ownerOrg = step.owner_org;
      const label = step.label || 'ok';
      const pre = (step.pre_chunks || []).map(unhex);
      const post = (step.post_chunks || []).map(unhex);
      const hold = !!step.hold;
      const deferMs = step.defer_ms || 0;
      const opts = { ownerOrg };
      try {
        if (shape === 'unary') {
          const verb = requireVerb(surface, 'serveOrg');
          state.close = await verb(
            step.service,
            access,
            async (caller, request) => {
              const entry = recordInvocation(state, 'unary', caller, hex(request));
              const reply = new TextEncoder().encode(label + ':' + new TextDecoder().decode(request));
              entry.result_hex = hex(reply);
              entry.completed_at = performance.now();
              return reply;
            },
            opts,
          );
        } else if (shape === 'streaming') {
          const verb = requireVerb(surface, 'serveOrgStreaming');
          state.close = await verb(
            step.service,
            access,
            async (caller, request, sink) => {
              const entry = recordInvocation(state, 'streaming', caller, hex(request));
              const tracked = trackedSink(entry, sink);
              state.holds.push(entry);
              for (const chunk of pre) await tracked.send(chunk);
              if (hold) {
                await new Promise((resolve) => {
                  state.release = resolve;
                });
              }
              if (deferMs) await sleep(deferMs);
              for (const chunk of post) await tracked.send(chunk);
              await tracked.close();
              entry.completed_at = performance.now();
            },
            opts,
          );
        } else if (shape === 'client_stream') {
          const verb = requireVerb(surface, 'serveOrgClientStream');
          state.close = await verb(
            step.service,
            access,
            async (caller, requests) => {
              const entry = recordInvocation(state, 'client_stream', caller, '');
              const collected = [];
              for await (const chunk of requests) {
                collected.push(hex(chunk));
                entry.items.push(hex(chunk));
                // A SLOW CONSUMER: a handler that drains eagerly
                // grants credit as fast as it arrives and the upload
                // window can never park a send.
                if (step.defer_ms) await sleep(step.defer_ms);
              }
              entry.eof_at = performance.now();
              if (deferMs) await sleep(deferMs);
              // The terminal reply: the exact concatenation, so a
              // missing/extra upload byte is an identity mismatch.
              const total = collected.reduce((n, h) => n + h.length / 2, 0);
              const reply = new Uint8Array(total);
              let off = 0;
              for (const h of collected) {
                const bytes = unhex(h);
                reply.set(bytes, off);
                off += bytes.length;
              }
              entry.result_hex = hex(reply);
              entry.completed_at = performance.now();
              return reply;
            },
            opts,
          );
        } else if (shape === 'duplex') {
          const verb = requireVerb(surface, 'serveOrgDuplex');
          state.close = await verb(
            step.service,
            access,
            async (caller, requests, sink) => {
              const entry = recordInvocation(state, 'duplex', caller, '');
              const tracked = trackedSink(entry, sink);
              state.holds.push(entry);
              let index = 0;
              for await (const chunk of requests) {
                entry.items.push(hex(chunk));
                // Echo each request chunk, content-labelled, so
                // per-chunk pairing is an exact identity check.
                const echo = new TextEncoder().encode('e' + index + ':');
                const out = new Uint8Array(echo.length + chunk.length);
                out.set(echo, 0);
                out.set(chunk, echo.length);
                await tracked.send(out);
                index += 1;
              }
              entry.eof_at = performance.now();
              if (hold) {
                await new Promise((resolve) => {
                  state.release = resolve;
                });
              }
              if (deferMs) await sleep(deferMs);
              for (const chunk of post) await tracked.send(chunk);
              await tracked.close();
              entry.completed_at = performance.now();
            },
            opts,
          );
        } else {
          return { ok: false, error: 'unknown org_serve shape ' + shape };
        }
        await log(`serving ${step.service} (${shape}/${access}) as ${step.handle}`);
        return { ok: true };
      } catch (e) {
        return typedFailure(e);
      }
    }

    case 'org_release': {
      const state = serves.get(step.handle);
      if (!state) return { ok: false, error: 'no such serve ' + step.handle };
      state.release();
      state.release = () => {};
      return { ok: true };
    }

    case 'org_serve_report': {
      const state = serves.get(step.handle);
      if (!state) return { ok: false, error: 'no such serve ' + step.handle };
      return {
        ok: true,
        stats: {
          ran: state.calls.length,
          calls: state.calls,
        },
      };
    }

    case 'org_serve_close': {
      const state = serves.get(step.handle);
      if (!state) return { ok: false, error: 'no such serve ' + step.handle };
      try {
        state.close && (await state.close.close ? state.close.close() : state.close());
      } catch (e) {
        return typedFailure(e);
      }
      serves.delete(step.handle);
      return { ok: true };
    }

    // ───────────────────────── call side ─────────────────────────
    case 'org_call_unary': {
      const surface = surfaceOf(step.session);
      try {
        const verb = requireVerb(surface, 'callOrg');
        const started = performance.now();
        const reply = await verb(step.service, unhex(step.payload_hex), optsOf(step));
        return {
          ok: true,
          reply: hex(reply),
          elapsed_ms: performance.now() - started,
        };
      } catch (e) {
        return typedFailure(e);
      }
    }

    case 'org_stream_open': {
      const surface = surfaceOf(step.session);
      try {
        const verb = requireVerb(surface, 'callOrgStreaming');
        const stream = await verb(step.service, unhex(step.payload_hex), optsOf(step));
        const state = {
          kind: 'stream',
          stream,
          items: [],
          arrivals: [],
          terminal: null,
          done: false,
        };
        calls.set(step.handle, state);
        return { ok: true };
      } catch (e) {
        return typedFailure(e);
      }
    }

    case 'org_stream_read': {
      const state = calls.get(step.handle);
      if (!state || state.kind !== 'stream') {
        return { ok: false, error: 'no such open stream call ' + step.handle };
      }
      const want = step.want || 1;
      const deadline = performance.now() + (step.timeout_ms || 8000);
      while (
        state.items.length < want &&
        !state.terminal &&
        performance.now() < deadline
      ) {
        // TERMINAL ITEM ERRORS THROW through the async iterator (the
        // surface contract) — a catch here is the typed terminal, not
        // a step failure.
        let next;
        try {
          next = await Promise.race([
            state.stream.next(),
            sleep(step.timeout_ms || 8000).then(() => 'timeout'),
          ]);
        } catch (e) {
          state.done = true;
          state.terminal = typedFailure(e);
          break;
        }
        if (next === 'timeout') break;
        // ERROR BEFORE DONE: a terminal error item is
        // `{done: true, error}` on this surface — a done-first order
        // swallows the typed terminal as a clean end.
        if (next.error) {
          state.done = true;
          state.terminal = typedFailure(next.error);
          break;
        }
        if (next.done) {
          state.done = true;
          state.terminal = { done: true };
          break;
        }
        state.items.push(hex(next.value));
        state.arrivals.push(performance.now());
      }
      // An iterator that ends without `done` still reports its
      // terminal exactly once.
      if (state.done && !state.terminal) state.terminal = { done: true };
      return {
        ok: true,
        stats: {
          items: state.items,
          arrivals: state.arrivals,
          terminal: state.terminal,
          done: state.done,
        },
      };
    }

    case 'org_stream_cancel': {
      const state = calls.get(step.handle);
      if (!state) return { ok: false, error: 'no such call ' + step.handle };
      try {
        state.stream.cancel();
        return { ok: true };
      } catch (e) {
        return typedFailure(e);
      }
    }

    case 'org_upload_open': {
      const surface = surfaceOf(step.session);
      try {
        const verb = requireVerb(surface, 'callOrgClientStream');
        const upload = await verb(step.service, optsOf(step));
        const state = {
          kind: 'upload',
          upload,
          send_log: [],
          reply: null,
          terminal: null,
        };
        calls.set(step.handle, state);
        return { ok: true };
      } catch (e) {
        return typedFailure(e);
      }
    }

    case 'org_upload_send': {
      const state = calls.get(step.handle);
      if (!state || state.kind !== 'upload') {
        return { ok: false, error: 'no such upload call ' + step.handle };
      }
      let sent = 0;
      for (const payload of step.payloads || []) {
        const sentAt = performance.now();
        try {
          await state.upload.send(unhex(payload));
          state.send_log.push({ payload, sent_at: sentAt, resolved_at: performance.now(), ok: true });
          sent += 1;
        } catch (e) {
          // A send AFTER finish() is a typed refusal, and the call
          // must still hold its result — recorded, not thrown.
          state.send_log.push({
            payload,
            sent_at: sentAt,
            resolved_at: performance.now(),
            ok: false,
            error: typedFailure(e),
          });
          return {
            ok: true,
            stats: { sent, send_log: state.send_log, refused: typedFailure(e) },
          };
        }
      }
      return { ok: true, stats: { sent, send_log: state.send_log } };
    }

    case 'org_upload_finish': {
      const state = calls.get(step.handle);
      if (!state || state.kind !== 'upload') {
        return { ok: false, error: 'no such upload call ' + step.handle };
      }
      try {
        const reply = await state.upload.finish();
        state.reply = hex(reply);
        return {
          ok: true,
          reply: state.reply,
          stats: { send_log: state.send_log },
        };
      } catch (e) {
        state.terminal = typedFailure(e);
        return { ...state.terminal, stats: { send_log: state.send_log } };
      }
    }

    case 'org_duplex_open': {
      const surface = surfaceOf(step.session);
      try {
        const verb = requireVerb(surface, 'callOrgDuplex');
        const call = await verb(step.service, optsOf(step));
        const state = {
          kind: 'duplex',
          call,
          items: [],
          arrivals: [],
          send_log: [],
          terminal: null,
          finished_sending: false,
          done: false,
        };
        calls.set(step.handle, state);
        return { ok: true };
      } catch (e) {
        return typedFailure(e);
      }
    }

    case 'org_duplex_send': {
      const state = calls.get(step.handle);
      if (!state || state.kind !== 'duplex') {
        return { ok: false, error: 'no such duplex call ' + step.handle };
      }
      let sent = 0;
      for (const payload of step.payloads || []) {
        const sentAt = performance.now();
        try {
          await state.call.sink.send(unhex(payload));
          state.send_log.push({ payload, sent_at: sentAt, resolved_at: performance.now(), ok: true });
          sent += 1;
        } catch (e) {
          state.send_log.push({
            payload,
            sent_at: sentAt,
            resolved_at: performance.now(),
            ok: false,
            error: typedFailure(e),
          });
          return {
            ok: true,
            stats: { sent, send_log: state.send_log, refused: typedFailure(e) },
          };
        }
      }
      return { ok: true, stats: { sent, send_log: state.send_log } };
    }

    case 'org_duplex_finish': {
      const state = calls.get(step.handle);
      if (!state || state.kind !== 'duplex') {
        return { ok: false, error: 'no such duplex call ' + step.handle };
      }
      try {
        await state.call.sink.finishSending();
        state.finished_sending = true;
        return { ok: true };
      } catch (e) {
        return typedFailure(e);
      }
    }

    case 'org_duplex_read': {
      const state = calls.get(step.handle);
      if (!state || state.kind !== 'duplex') {
        return { ok: false, error: 'no such duplex call ' + step.handle };
      }
      const want = step.want || 1;
      const deadline = performance.now() + (step.timeout_ms || 8000);
      while (state.items.length < want && !state.terminal && performance.now() < deadline) {
        let next;
        try {
          next = await Promise.race([
            state.call.stream.next(),
            sleep(step.timeout_ms || 8000).then(() => 'timeout'),
          ]);
        } catch (e) {
          state.done = true;
          state.terminal = typedFailure(e);
          break;
        }
        if (next === 'timeout') break;
        // ERROR BEFORE DONE (see `org_stream_read`).
        if (next.error) {
          state.done = true;
          state.terminal = typedFailure(next.error);
          break;
        }
        if (next.done) {
          state.done = true;
          state.terminal = { done: true };
          break;
        }
        state.items.push(hex(next.value));
        state.arrivals.push(performance.now());
      }
      if (state.done && !state.terminal) state.terminal = { done: true };
      return {
        ok: true,
        stats: {
          items: state.items,
          arrivals: state.arrivals,
          terminal: state.terminal,
          done: state.done,
          finished_sending: state.finished_sending,
          send_log: state.send_log,
        },
      };
    }

    case 'counters': {
      const surface = surfaceOf(step.session);
      return { ok: true, stats: { counters: surface.counters ? surface.counters() : {} } };
    }

    case 'close': {
      const surface = surfaceOf(step.session);
      surface.close();
      return { ok: true };
    }

    case 'idle':
      await sleep(step.millis || 50);
      return { ok: true };

    case 'done':
      return { ok: true };

    default:
      return { ok: false, error: 'unknown stage-4 org step kind ' + step.kind };
  }
}

// The step loop. Identical framing to leaf5.js/peer6.js: one long-poll
// per tab, results on the shared `/harness/result` keyed by step id.
async function main() {
  await log(`[org:${TAB}] up feed=${globalThis.__netOrgControl}`);
  for (;;) {
    let step;
    try {
      const res = await fetch('/harness/stepOrg?tab=' + encodeURIComponent(TAB));
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
      result = { ok: false, error: (e && e.message) || String(e) };
    }
    if (!result) result = { ok: false, error: 'the step returned nothing' };
    result.id = step.id;
    await fetch('/harness/result', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(result),
    }).catch(() => {});
  }
}

main();
