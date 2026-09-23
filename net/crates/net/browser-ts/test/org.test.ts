/**
 * The org surface's TypeScript mapping — plan §4.3's terminal
 * vocabulary, §4.5's suspension/closure contract, and the F-S3.1-2
 * handler-drop level — over faked wasm at the Rust shape.
 *
 * What these tests pin: typed errors thrown from terminal items, the
 * async-iterator throw semantics, the u64 decimal-string discipline,
 * and the retirement observables (`retired`, the typed closed
 * refusal, the terminal item) that the handler-drop level names.
 */

import { describe, expect, it } from 'vitest';

import {
  fromWasmError,
  OrgAdmissionDeniedError,
  OrgCancelledError,
  OrgIndeterminateError,
  OrgInternalError,
  OrgLeaderLostError,
  OrgMalformedError,
  OrgRefusedError,
  OrgRevokedError,
  OrgSessionLostError,
  OrgStreamError,
  OrgTimeoutError,
  orgRetireError,
  orgRetireReason,
  orgTerminalError,
  parseOrgError,
  RpcError,
} from '../src/errors.js';
import type { OrgErrorKind, OrgRetireReason } from '../src/errors.js';
import {
  OrgDuplex,
  OrgRequests,
  OrgSink,
  OrgStream,
  OrgUpload,
  toWasmOrgCallOptions,
} from '../src/org.js';
import type { OrgCallOptions, OrgCaller } from '../src/org.js';
import { connect } from '../src/node.js';
import type { BrowserNode } from '../src/node.js';
import { openSession } from '../src/leader/session.js';
import type { MeshSession } from '../src/leader/session.js';
import {
  FakeNode,
  FakeOrgByteStreamHandle,
  FakeOrgDuplexCallHandle,
  FakeOrgRequestStreamHandle,
  FakeOrgResponseSinkHandle,
  FakeOrgUploadCallHandle,
  fakeModule,
} from './fake-wasm.js';
import type { FakeNodeBehaviour } from './fake-wasm.js';
import { FakeSession, fakeSessionModule } from './fake-leader-wasm.js';
import type { FakeSessionBehaviour } from './fake-leader-wasm.js';

const BASE = {
  credentialB64: 'Y3JlZA==',
  bootstrapUrl: 'https://anchor.example/rtc/bootstrap',
  origin: 'https://page.example',
};

const MEMBERSHIP = new Uint8Array([1, 2, 3]);
const DISPATCHER = new Uint8Array([4, 5]);
const CREDENTIALS = {
  membership: MEMBERSHIP,
  dispatcher: DISPATCHER,
  actingOrg: 'org-a',
  providerOwnerOrg: 'org-a',
  provider: 'beefcafe00000002',
};
const CALL_OPTIONS: OrgCallOptions = { credentials: CREDENTIALS, deadlineMs: 5_000 };
const SERVE_OPTIONS = { ownerOrg: 'org-a' };
const BODY = new Uint8Array([9, 8, 7]);

const CALLER_JSON = JSON.stringify({
  entity: 'ab'.repeat(32),
  actingOrg: 'org-a',
  providerOrg: 'org-a',
  provider: 'beefcafe00000002',
  capability: 'transcribe',
  isSameOrg: true,
});

async function connected(behaviour: FakeNodeBehaviour = {}): Promise<{
  node: BrowserNode;
  fake: FakeNode;
}> {
  const fake = new FakeNode(behaviour);
  const node = await connect({ ...BASE, wasm: fakeModule(fake) });
  return { node, fake };
}

async function sessioned(behaviour: FakeSessionBehaviour = {}): Promise<{
  session: MeshSession;
  fake: FakeSession;
}> {
  const fake = new FakeSession(behaviour);
  const session = await openSession({ ...BASE, wasm: fakeSessionModule(fake) });
  return { session, fake };
}

describe('the org terminal vocabulary (plan §4.3)', () => {
  const KINDS: Array<[string, OrgErrorKind]> = [
    ['admission-denied', 'org-admission-denied'],
    ['revoked', 'org-revoked'],
    ['timeout', 'org-timeout'],
    ['cancelled', 'org-cancelled'],
    ['leader-lost', 'org-leader-lost'],
    ['session-lost', 'org-session-lost'],
    ['indeterminate', 'org-indeterminate'],
    ['refused', 'org-refused'],
    ['internal', 'org-internal'],
    ['malformed', 'org-malformed'],
  ];

  it.each(KINDS)('a %s terminal item becomes its typed class, message verbatim', (wire, kind) => {
    const error = orgTerminalError({ kind: wire, message: `boundary text for ${wire}` });
    expect(error).toBeInstanceOf(OrgStreamError);
    expect(error.kind).toBe(kind);
    // The doctrine of this taxonomy: `message` is the boundary's own
    // text, verbatim.
    expect(error.message).toBe(`boundary text for ${wire}`);
  });

  it('a revocation terminal IS an admission denial with coarse denied', () => {
    const error = orgTerminalError({ kind: 'revoked', message: 'revoked mid-call' });
    // §4.3's frozen Revoked → Denied coarse map, visible as types:
    // `AdmissionDenied('denied')`, with the cause on `instanceof`.
    expect(error).toBeInstanceOf(OrgRevokedError);
    expect(error).toBeInstanceOf(OrgAdmissionDeniedError);
    expect((error as OrgAdmissionDeniedError).coarse).toBe('denied');
  });

  it('an admission denial exposes only its coarse bucket', () => {
    const denied = orgTerminalError({ kind: 'admission-denied', coarse: 'denied', message: 'x' });
    const unsupported = orgTerminalError({
      kind: 'admission-denied',
      coarse: 'not-supported',
      message: 'x',
    });
    const unavailable = orgTerminalError({
      kind: 'admission-denied',
      coarse: 'unavailable',
      message: 'x',
    });
    expect((denied as OrgAdmissionDeniedError).coarse).toBe('denied');
    expect((unsupported as OrgAdmissionDeniedError).coarse).toBe('not-supported');
    expect((unavailable as OrgAdmissionDeniedError).coarse).toBe('unavailable');
  });

  it('an undecodable coarse body is the least-informative bucket, never an error about an error', () => {
    const fromGarbage = parseOrgError('rpc: refused (9): \u0003garbage');
    expect(fromGarbage).toBeInstanceOf(OrgAdmissionDeniedError);
    expect((fromGarbage as OrgAdmissionDeniedError).coarse).toBe('denied');
  });

  it('a u64 generation crosses as an exact decimal string, never a JS number', () => {
    const error = orgTerminalError({
      kind: 'leader-lost',
      generation: '18446744073709551615',
      message: 'gone',
    });
    expect(error).toBeInstanceOf(OrgLeaderLostError);
    const leaderLost = error as OrgLeaderLostError;
    expect(typeof leaderLost.generation).toBe('string');
    // The point of the rule: this number does not survive a JS
    // `number`, and a rounded generation is a fence that has stopped
    // fencing.
    expect(leaderLost.generation).toBe('18446744073709551615');
  });

  it('an opening denial re-types from the leaf Display', () => {
    const thrown = new Error('rpc: refused (9): not-supported');
    const typed = fromWasmError(thrown);
    expect(typed).toBeInstanceOf(OrgAdmissionDeniedError);
    expect((typed as OrgAdmissionDeniedError).coarse).toBe('not-supported');
    // Verbatim Display preserved, as every class here does.
    expect(typed.message).toBe('rpc: refused (9): not-supported');
  });

  it('a non-admission refusal stays in the nRPC taxonomy', () => {
    const typed = fromWasmError(new Error('rpc: refused (404): no such service'));
    expect(typed).toBeInstanceOf(RpcError);
    expect(typed).not.toBeInstanceOf(OrgStreamError);
  });

  it('the org wire vocabulary classifies where names coincide', () => {
    expect(parseOrgError('org:admission_denied:unavailable')?.kind).toBe('org-admission-denied');
    expect(parseOrgError('org:rpc:leader_lost')?.kind).toBe('org-leader-lost');
    expect(parseOrgError('org:rpc:cancelled')?.kind).toBe('org-cancelled');
    // A domain this build does not know is NOT guessed into a merits
    // bucket.
    expect(parseOrgError('org:credentials:nope')).toBeNull();
    expect(fromWasmError(new Error('org:credentials:nope'))).not.toBeInstanceOf(OrgStreamError);
  });

  it('every retire verdict maps onto the terminal vocabulary', () => {
    const rows: Array<[OrgRetireReason, OrgErrorKind]> = [
      ['timeout', 'org-timeout'],
      ['cancelled', 'org-cancelled'],
      ['revoked', 'org-revoked'],
      ['session-lost', 'org-session-lost'],
      ['leader-lost', 'org-leader-lost'],
      // The two teardown verdicts arrive as `cancelled` (the frozen
      // retire → terminal map); the raw string is what tells them
      // apart.
      ['node-closed', 'org-cancelled'],
      ['replaced', 'org-cancelled'],
    ];
    for (const [verdict, kind] of rows) {
      expect(orgRetireError(verdict).kind, verdict).toBe(kind);
    }
  });

  it('an unrecognized retire verdict is reported as replaced, never guessed', () => {
    expect(orgRetireReason('timeout')).toBe('timeout');
    expect(orgRetireReason('something-new')).toBe('replaced');
  });

  it('an unknown terminal kind is named internal, not guessed into a merits bucket', () => {
    const error = orgTerminalError({ kind: 'brand-new', message: 'm' });
    expect(error).toBeInstanceOf(OrgInternalError);
  });

  it('the typed classes carry their structured fields', () => {
    expect(
      (orgTerminalError({ kind: 'indeterminate', deadlineMs: 100, message: 'm' }) as OrgIndeterminateError)
        .deadlineMs,
    ).toBe(100);
    expect((orgTerminalError({ kind: 'refused', status: 404, message: 'm' }) as OrgRefusedError).status).toBe(404);
    expect(orgTerminalError({ kind: 'malformed', message: 'm' })).toBeInstanceOf(OrgMalformedError);
    expect(orgTerminalError({ kind: 'session-lost', message: 'm' })).toBeInstanceOf(OrgSessionLostError);
    expect(orgTerminalError({ kind: 'timeout', message: 'm' })).toBeInstanceOf(OrgTimeoutError);
  });
});

describe('typed errors thrown from terminal items', () => {
  it('the async iterator throws the typed error at the terminal error item', async () => {
    const fake = new FakeOrgByteStreamHandle();
    const stream = new OrgStream(fake, () => fake.cancel());
    fake.deliver({ done: false, value: new Uint8Array([1]) });
    fake.deliver({ done: true, error: { kind: 'revoked', message: 'revoked mid-call' } });
    const seen: Uint8Array[] = [];
    const drained = (async () => {
      for await (const payload of stream) seen.push(payload);
    })();
    const error = await drained.then(
      () => null,
      (e: unknown) => e,
    );
    expect(seen).toEqual([new Uint8Array([1])]);
    expect(error).toBeInstanceOf(OrgRevokedError);
    expect(error).toBeInstanceOf(OrgAdmissionDeniedError);
    expect((error as OrgAdmissionDeniedError).coarse).toBe('denied');
  });

  it('a clean end is a plain loop end', async () => {
    const fake = new FakeOrgByteStreamHandle();
    const stream = new OrgStream(fake, () => fake.cancel());
    fake.deliver({ done: false, value: new Uint8Array([2]) });
    fake.deliver({ done: true });
    const seen: Uint8Array[] = [];
    for await (const payload of stream) seen.push(payload);
    expect(seen).toEqual([new Uint8Array([2])]);
  });

  it('a boundary rejection propagates as its classified self and is latched', async () => {
    const fake = new FakeOrgByteStreamHandle();
    const stream = new OrgStream(fake, () => fake.cancel());
    const parked = stream.next();
    fake.fail(new Error('rpc: the leader holding generation 3 was replaced'));
    // The frozen nRPC vocabulary, unchanged: a proxied LeaderLost is
    // the existing `RpcError` kind — "typed LeaderLost".
    const first = await parked.then(
      () => null,
      (e: unknown) => e,
    );
    expect(first).toBeInstanceOf(RpcError);
    expect((first as RpcError).failure).toEqual({ type: 'leaderLost', generation: 3 });
    // Latched: a second pull sees the same typed failure, not silence.
    await expect(stream.next()).rejects.toBeInstanceOf(RpcError);
  });

  it('finish() rejects with the typed terminal error', async () => {
    const fake = new FakeOrgUploadCallHandle();
    const upload = new OrgUpload(fake);
    const pending = upload.finish();
    fake.failFinish(new Error('org:rpc:timeout'));
    await expect(pending).rejects.toBeInstanceOf(OrgTimeoutError);
  });

  it('cancel emits exactly one CANCEL however many halves drop the call', async () => {
    const fake = new FakeOrgDuplexCallHandle();
    const call = new OrgDuplex(fake);
    call.sink.cancel();
    call.stream.cancel();
    call.retire(new OrgCancelledError());
    // The two halves share one drop: one CANCEL leaves per call,
    // however many halves initiate it.
    expect(fake.cancels).toBe(1);
  });
});

describe('the suspension and closure contract (plan §4.5)', () => {
  it('closing the node retires live handles typed and emits one CANCEL each', async () => {
    const { node, fake } = await connected();
    const stream = await node.callOrgStreaming('transcribe', BODY, CALL_OPTIONS);
    const upload = await node.callOrgClientStream('upload', CALL_OPTIONS);
    node.close();
    const item = await stream.next();
    expect(item.done).toBe(true);
    expect(item.error).toBeInstanceOf(OrgCancelledError);
    await expect(upload.finish()).rejects.toBeInstanceOf(OrgCancelledError);
    expect(fake.orgStreamHandles[0]?.cancels).toBe(1);
    expect(fake.orgUploadHandles[0]?.cancels).toBe(1);
  });

  it('a retired call is never transparently re-opened', async () => {
    const { node, fake } = await connected();
    const stream = await node.callOrgStreaming('transcribe', BODY, CALL_OPTIONS);
    node.close();
    const terminal = await stream.next();
    // The terminal is latched: a later pull sees the same outcome and
    // no second CANCEL leaves. Re-opening is a fresh call with a fresh
    // proof — this wrapper never does it on the caller's behalf.
    expect(await stream.next()).toBe(terminal);
    expect(fake.orgStreamHandles[0]?.cancels).toBe(1);
  });

  it('a generation move retires live calls typed LeaderLost with the owning generation', async () => {
    const { session, fake } = await sessioned({ generation: '1' });
    const stream = await session.callOrgStreaming('transcribe', BODY, CALL_OPTIONS);
    fake.handoff('2');
    const item = await stream.next();
    expect(item.error).toBeInstanceOf(OrgLeaderLostError);
    // The generation that OWNED the call, exact decimal — not the
    // successor's.
    expect((item.error as OrgLeaderLostError).generation).toBe('1');
  });

  it('an explicit leader_lost retires the same way', async () => {
    const { session, fake } = await sessioned({ generation: '1' });
    const upload = await session.callOrgClientStream('upload', CALL_OPTIONS);
    fake.emit('{"type":"leader_lost","generation":"1","failed":"2"}');
    const error = await upload.finish().then(
      () => null,
      (e: unknown) => e,
    );
    expect(error).toBeInstanceOf(OrgLeaderLostError);
    expect((error as OrgLeaderLostError).generation).toBe('1');
  });

  it('an open that lands after a generation move is retired on arrival', async () => {
    const { session, fake } = await sessioned({ generation: '1' });
    const release = fake.parkNextOrgOpen();
    const pending = session.callOrgStreaming('transcribe', BODY, CALL_OPTIONS);
    fake.handoff('2');
    release();
    const stream = await pending;
    // Never live on a successor generation: the handle it carries was
    // stamped by the generation it was issued under.
    const item = await stream.next();
    expect(item.error).toBeInstanceOf(OrgLeaderLostError);
    expect((item.error as OrgLeaderLostError).generation).toBe('1');
  });

  it('closing the session retires live calls typed', async () => {
    const { session, fake } = await sessioned();
    const stream = await session.callOrgStreaming('transcribe', BODY, CALL_OPTIONS);
    session.close();
    const item = await stream.next();
    expect(item.error).toBeInstanceOf(OrgCancelledError);
    expect(fake.orgStreamHandles[0]?.cancels).toBe(1);
  });
});

describe('the handler surface (F-S3.1-2, C9)', () => {
  it('retired is memoized once and reaches a detached observer', async () => {
    const fake = new FakeOrgResponseSinkHandle();
    const sink = new OrgSink(fake);
    // The boundary's `retired()` is called ONCE, at construction.
    expect(fake.retiredCalls).toBe(1);
    const detached = sink.retired;
    fake.emitRetired('revoked');
    // A detached observer holding `retired` observes the signal.
    expect(await detached).toBe('revoked');
    expect(await sink.retired).toBe('revoked');
    expect(fake.retiredCalls).toBe(1);
  });

  it('after retirement the sink refuses typed', async () => {
    const fake = new FakeOrgResponseSinkHandle();
    const sink = new OrgSink(fake);
    fake.emitRetired('timeout');
    // The verdict lands WITH the retirement observable; once it is
    // observed, every sink verb refuses typed — the "typed closed
    // refusal" half of the F-S3.1-2 observables.
    expect(await sink.retired).toBe('timeout');
    await expect(sink.send(new Uint8Array([1]))).rejects.toBeInstanceOf(OrgTimeoutError);
    await expect(sink.close()).rejects.toBeInstanceOf(OrgTimeoutError);
    // The refusal never reached the boundary — the retirement record
    // is enough, and the handler's return value is discarded.
    expect(fake.sent).toHaveLength(0);
    expect(fake.closes).toBe(0);
  });

  it('a request stream ends at its done item and reports retirement through retired', async () => {
    const handle = new FakeOrgRequestStreamHandle();
    const requests = new OrgRequests(handle);
    handle.deliver({ done: false, value: new Uint8Array([1]) });
    expect(await requests.next()).toEqual({ done: false, value: new Uint8Array([1]) });
    // A parked pull is settled by the retirement observables — the
    // `done` terminal item here, `retired` alongside it — never left
    // hanging on a handler future that may have been dropped without
    // a final poll.
    const parked = requests.next();
    handle.emitRetired('cancelled');
    expect(await parked).toEqual({ done: true });
    expect(await requests.retired).toBe('cancelled');
  });

  it('the unary trampoline hands over a typed OrgCaller', async () => {
    const { node, fake } = await connected();
    let seen: OrgCaller | null = null;
    node.serveOrg(
      'echo',
      'granted',
      async (caller, request) => {
        seen = caller;
        return request;
      },
      SERVE_OPTIONS,
    );
    const registration = fake.orgUnaryServes[0];
    expect(registration?.service).toBe('echo');
    expect(registration?.access).toBe('granted');
    const reply = await registration!.handler(CALLER_JSON, BODY);
    expect(reply).toEqual(BODY);
    expect(seen).toEqual({
      entity: 'ab'.repeat(32),
      actingOrg: 'org-a',
      providerOrg: 'org-a',
      provider: 'beefcafe00000002',
      capability: 'transcribe',
      isSameOrg: true,
    });
  });

  it('the streaming trampoline wraps the handler sink', async () => {
    const { node, fake } = await connected();
    let retired: Promise<OrgRetireReason> | null = null;
    node.serveOrgStreaming(
      'events',
      'same-org',
      async (_caller, _request, sink) => {
        retired = sink.retired;
        await sink.send(new Uint8Array([1]));
        await sink.close();
      },
      SERVE_OPTIONS,
    );
    const handle = new FakeOrgResponseSinkHandle();
    await fake.orgStreamingServes[0]!.handler(CALLER_JSON, BODY, handle);
    expect(handle.sent).toEqual([new Uint8Array([1])]);
    expect(handle.closes).toBe(1);
    // The one `retired()` the F-S3.1-2 level names, wrapped already.
    expect(handle.retiredCalls).toBe(1);
    handle.emitRetired('revoked');
    expect(await retired!).toBe('revoked');
  });

  it('the client-streaming trampoline wraps the request stream', async () => {
    const { node, fake } = await connected();
    node.serveOrgClientStream(
      'upload',
      'granted',
      async (_caller, requests) => {
        const items: Uint8Array[] = [];
        for await (const item of requests) items.push(item);
        return items[0] ?? new Uint8Array(0);
      },
      SERVE_OPTIONS,
    );
    const handle = new FakeOrgRequestStreamHandle();
    const pending = fake.orgClientStreamServes[0]!.handler(CALLER_JSON, handle);
    handle.deliver({ done: false, value: new Uint8Array([3]) });
    handle.deliver({ done: true });
    expect(await pending).toEqual(new Uint8Array([3]));
  });

  it('the duplex trampoline wraps both halves', async () => {
    const { node, fake } = await connected();
    let sent = 0;
    node.serveOrgDuplex(
      'chat',
      'same-org',
      async (_caller, requests, sink) => {
        for await (const item of requests) {
          sent += 1;
          await sink.send(item);
        }
        await sink.close();
      },
      SERVE_OPTIONS,
    );
    const requests = new FakeOrgRequestStreamHandle();
    const sink = new FakeOrgResponseSinkHandle();
    const pending = fake.orgDuplexServes[0]!.handler(CALLER_JSON, requests, sink);
    requests.deliver({ done: false, value: new Uint8Array([4]) });
    requests.deliver({ done: true });
    await pending;
    expect(sent).toBe(1);
    expect(sink.sent).toEqual([new Uint8Array([4])]);
    expect(sink.closes).toBe(1);
  });

  it('closing a serve handle is a retirement, not a graceful unregister', async () => {
    const { node, fake } = await connected();
    const handle = node.serveOrg('echo', 'granted', async (_c, request) => request, SERVE_OPTIONS);
    expect(handle.service).toBe('echo');
    handle.close();
    // C9's protected split: the registration's close is a retirement
    // of its live protected calls — observable here as the boundary
    // close, once.
    handle.close();
    expect(fake.orgServeHandles[0]?.closes).toBe(1);
  });
});

describe('the contract surface', () => {
  it('the four call verbs hand back their handles', async () => {
    const { node, fake } = await connected();
    const reply = await node.callOrg('echo', BODY, CALL_OPTIONS);
    expect(reply).toEqual(new Uint8Array([7]));
    const stream = await node.callOrgStreaming('events', BODY, CALL_OPTIONS);
    expect(typeof stream.next).toBe('function');
    expect(typeof stream.cancel).toBe('function');
    expect(typeof stream[Symbol.asyncIterator]).toBe('function');
    const upload = await node.callOrgClientStream('upload', CALL_OPTIONS);
    expect(typeof upload.send).toBe('function');
    expect(typeof upload.finish).toBe('function');
    expect(typeof upload.cancel).toBe('function');
    const duplex = await node.callOrgDuplex('chat', CALL_OPTIONS);
    expect(typeof duplex.sink.send).toBe('function');
    expect(typeof duplex.sink.finishSending).toBe('function');
    expect(typeof duplex.sink.cancel).toBe('function');
    expect(typeof duplex.stream.next).toBe('function');
    // The byte-stream opener's reply is what the leaf answered.
    fake.orgStreamHandles[0]?.deliver({ done: false, value: new Uint8Array([5]) });
    expect(await stream.next()).toEqual({ done: false, value: new Uint8Array([5]) });
  });

  it('the four serve verbs register on the node', async () => {
    const { node, fake } = await connected();
    node.serveOrg('u', 'same-org', async (_c, request) => request, SERVE_OPTIONS);
    node.serveOrgStreaming('s', 'granted', async () => {}, SERVE_OPTIONS);
    node.serveOrgClientStream('c', 'same-org', async () => new Uint8Array(0), SERVE_OPTIONS);
    node.serveOrgDuplex('d', 'granted', async () => {}, SERVE_OPTIONS);
    expect(fake.orgUnaryServes).toHaveLength(1);
    expect(fake.orgStreamingServes).toHaveLength(1);
    expect(fake.orgClientStreamServes).toHaveLength(1);
    expect(fake.orgDuplexServes).toHaveLength(1);
    expect(fake.orgServeHandles.map((handle) => handle.service())).toEqual(['u', 's', 'c', 'd']);
  });

  it('the session carries the same eight verbs', async () => {
    const { session, fake } = await sessioned();
    const reply = await session.callOrg('echo', BODY, CALL_OPTIONS);
    expect(reply).toEqual(new Uint8Array([7]));
    const stream = await session.callOrgStreaming('events', BODY, CALL_OPTIONS);
    const upload = await session.callOrgClientStream('upload', CALL_OPTIONS);
    const duplex = await session.callOrgDuplex('chat', CALL_OPTIONS);
    expect(typeof stream.next).toBe('function');
    expect(typeof upload.finish).toBe('function');
    expect(typeof duplex.sink.finishSending).toBe('function');
    session.serveOrg('u', 'same-org', async (_c, request) => request, SERVE_OPTIONS);
    session.serveOrgStreaming('s', 'granted', async () => {}, SERVE_OPTIONS);
    session.serveOrgClientStream('c', 'same-org', async () => new Uint8Array(0), SERVE_OPTIONS);
    session.serveOrgDuplex('d', 'granted', async () => {}, SERVE_OPTIONS);
    expect(fake.orgUnaryServes).toHaveLength(1);
    expect(fake.orgStreamingServes).toHaveLength(1);
    expect(fake.orgClientStreamServes).toHaveLength(1);
    expect(fake.orgDuplexServes).toHaveLength(1);
  });

  it('call options cross verbatim — bytes forwarded, absent keys absent', async () => {
    const { node, fake } = await connected();
    const capabilityGrant = new Uint8Array([6]);
    await node.callOrg(
      'echo',
      BODY,
      toWasmOrgCallOptions({
        credentials: { ...CREDENTIALS, capabilityGrant, proofTtlSecs: 30 },
        deadlineMs: 1_000,
      }),
    );
    const crossed = fake.orgCalls[0]!.options;
    // Identity, not a copy: the leaf validates the caller's own proof
    // bytes and a silent re-encode here would be a second codec.
    expect(crossed.credentials.membership).toBe(MEMBERSHIP);
    expect(crossed.credentials.dispatcher).toBe(DISPATCHER);
    expect(crossed.credentials.capabilityGrant).toBe(capabilityGrant);
    expect(crossed.credentials.proofTtlSecs).toBe(30);
    expect(crossed.deadlineMs).toBe(1_000);
    expect('streamWindowInitial' in crossed).toBe(false);
    expect('requestWindowInitial' in crossed).toBe(false);

    // Absence, not zero-filling: an option the caller did not set does
    // not cross.
    const minimal = toWasmOrgCallOptions({ credentials: CREDENTIALS });
    expect('capabilityGrant' in minimal.credentials).toBe(false);
    expect('proofTtlSecs' in minimal.credentials).toBe(false);
    expect('deadlineMs' in minimal).toBe(false);
    expect('streamWindowInitial' in minimal).toBe(false);
  });
});
