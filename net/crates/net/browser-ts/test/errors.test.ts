/**
 * The wasm→TS error mapping.
 *
 * The contract is exact: `wasm-bindgen` throws a `JsError` whose
 * message is `LeafError`'s Rust `Display`, and this package must
 * recover the variant from it and reproduce the same text. The
 * round-trip assertions below are the tripwire for a `Display` change
 * on the Rust side — `net/crates/net/leaf/src/error.rs`.
 */

import { describe, expect, it } from 'vitest';

import {
  ControlPlaneError,
  fromWasmError,
  IdentityError,
  isUdpBlocked,
  LeafError,
  NotLeaderError,
  RpcError,
  RtcError,
  SessionError,
  UnknownLeafError,
  WireError,
  type LeafErrorKind,
} from '../src/errors.js';

/** Every variant, with the Display text the Rust side emits for it. */
const VARIANTS: Array<{ error: LeafError; kind: LeafErrorKind; display: string }> = [
  { error: new WireError('replay window rejected seq 7'), kind: 'wire', display: 'wire: replay window rejected seq 7' },
  { error: new SessionError('no session for peer 4'), kind: 'session', display: 'session: no session for peer 4' },
  {
    error: new ControlPlaneError('offer refused'),
    kind: 'control-plane',
    display: 'control plane: offer refused',
  },
  { error: new IdentityError('IndexedDB is unavailable'), kind: 'identity', display: 'identity: IndexedDB is unavailable' },
  {
    error: new NotLeaderError(3),
    kind: 'not-leader',
    display: 'not the leader: this tab holds generation 3',
  },
  {
    error: new NotLeaderError(3, 4),
    kind: 'not-leader',
    display: 'not the leader: this tab holds generation 3, the leader holds 4',
  },
  {
    error: new RtcError({ type: 'iceTimeout' }),
    kind: 'ice-timeout',
    display: 'rtc: ICE did not connect inside the deadline (this does not establish that UDP is blocked)',
  },
  {
    error: new RtcError({
      type: 'udpBlocked',
      evidence: { bootstrapOk: true, stunProbeFailed: true, probed: '203.0.113.9:4433' },
    }),
    kind: 'udp-blocked',
    display:
      "rtc: UDP appears blocked: the anchor's HTTPS bootstrap succeeded but a STUN binding to 203.0.113.9:4433 was unanswered",
  },
  {
    error: new RtcError({ type: 'channelClosed', detail: 'remote reset' }),
    kind: 'channel-closed',
    display: 'rtc: the DataChannel closed: remote reset',
  },
  {
    error: new RtcError({ type: 'unsupported', detail: 'no RTCPeerConnection' }),
    kind: 'rtc-unsupported',
    display: 'rtc: this browser refused the attempt: no RTCPeerConnection',
  },
  {
    error: new RpcError({ type: 'refused', status: 404, message: 'no such service: summarise' }),
    kind: 'rpc-refused',
    display: 'rpc: refused (404): no such service: summarise',
  },
  { error: new RpcError({ type: 'timeout' }), kind: 'rpc-timeout', display: "rpc: the call's deadline elapsed" },
  {
    error: new RpcError({ type: 'sessionLost' }),
    kind: 'session-lost',
    display: 'rpc: the session carrying the call went away',
  },
  {
    error: new RpcError({ type: 'leaderLost', generation: 9 }),
    kind: 'leader-lost',
    display: 'rpc: the leader holding generation 9 was replaced',
  },
  {
    error: new RpcError({ type: 'malformed', detail: 'unexpected end of input' }),
    kind: 'rpc-malformed',
    display: 'rpc: the reply did not decode: unexpected end of input',
  },
];

describe('the Rust Display round-trip', () => {
  it.each(VARIANTS)('$kind reproduces its Rust Display', ({ error, kind, display }) => {
    expect(error.kind).toBe(kind);
    expect(error.message).toBe(display);
  });

  it.each(VARIANTS)('$kind is recovered from that Display text', ({ kind, display }) => {
    const recovered = fromWasmError(new Error(display));
    expect(recovered.kind).toBe(kind);
    expect(recovered.message).toBe(display);
    expect(recovered).toBeInstanceOf(LeafError);
  });
});

describe('fromWasmError', () => {
  it('recovers the nested failure, not just the kind', () => {
    const refused = fromWasmError(new Error('rpc: refused (503): overloaded'));
    expect(refused).toBeInstanceOf(RpcError);
    if (refused instanceof RpcError) {
      expect(refused.failure).toEqual({ type: 'refused', status: 503, message: 'overloaded' });
    }
  });

  it('recovers the evidence a udp-blocked claim is built on', () => {
    const blocked = fromWasmError(
      new Error(
        "rtc: UDP appears blocked: the anchor's HTTPS bootstrap succeeded but a STUN binding to [2001:db8::1]:4433 was unanswered",
      ),
    );
    expect(isUdpBlocked(blocked)).toBe(true);
    if (isUdpBlocked(blocked)) {
      expect(blocked.failure.evidence).toEqual({
        bootstrapOk: true,
        stunProbeFailed: true,
        probed: '[2001:db8::1]:4433',
      });
    }
  });

  it('accepts a bare string and an object with a message, as the boundary may throw either', () => {
    expect(fromWasmError('session: gone').kind).toBe('session');
    expect(fromWasmError({ message: 'identity: no key' }).kind).toBe('identity');
  });

  it('does not fold an unrecognised failure into a neighbouring variant', () => {
    const browserFailure = new DOMException('Failed to set remote answer sdp', 'InvalidStateError');
    const mapped = fromWasmError(browserFailure);
    expect(mapped).toBeInstanceOf(UnknownLeafError);
    expect(mapped.kind).toBe('unknown');
    expect(mapped.message).toBe('Failed to set remote answer sdp');
    expect(mapped.cause).toBe(browserFailure);
  });

  it('does not invent an rtc variant from an unknown rtc Display', () => {
    const mapped = fromWasmError(new Error('rtc: something Stage 6 added'));
    expect(mapped.kind).toBe('unknown');
  });

  it('passes an already-typed error straight through', () => {
    const already = new WireError('bad tag');
    expect(fromWasmError(already)).toBe(already);
  });
});

describe('enrollment failures', () => {
  // The property: the §12 admission step routes by PREFIX, and a page's
  // whole diagnosis rests on telling admission apart from carriage — a
  // refusal is `identity:` (the anchor answered and said no, or the
  // invite is unusable), a silent anchor is `rpc:` (nothing answered).
  //
  // The sentences below are representative, NOT pinned: the detail text
  // after the prefix is the leaf's human-facing prose and must stay free
  // to be reworded. So these assert the kind and that the detail crosses
  // verbatim, never the wording itself. (The 15 `Display` strings of the
  // error *enums* are a different matter and are pinned above — those are
  // the contract.)
  it.each([
    'the anchor rejected enrollment: replay (5): that invite was already redeemed',
    'the anchor rejected enrollment: unknown (42): a code newer than this package',
    "the credential's invite expired at 1757000000 (now 1757900000)",
    "the enrollment request is 20000 bytes, over \u00a712's 16384-byte bound",
  ])('routes an identity-prefixed refusal to identity, detail intact: %s', (detail) => {
    const error = fromWasmError(new Error(`identity: ${detail}`));
    expect(error.kind).toBe('identity');
    expect(error).toBeInstanceOf(IdentityError);
    if (error instanceof IdentityError) expect(error.detail).toBe(detail);
  });

  it.each([
    ["rpc: the call's deadline elapsed", 'rpc-timeout'],
    ['rpc: the session carrying the call went away', 'session-lost'],
  ])('keeps %s as transport, not admission', (display, kind) => {
    // These two ARE pinned: they are `RpcError`'s own `Display`.
    expect(fromWasmError(new Error(display)).kind).toBe(kind);
  });

  it('ignores an unrecognised own property, so the Rust side can ship data before TS reads it', () => {
    // The agreed `EnrollmentRejected` design is "additive on all three
    // sides, unordered": the leaf may start attaching
    // `enrollmentCode` to the thrown value before this package grows
    // the arm that narrows on it. That is only safe if an unknown own
    // property changes nothing today — routing still by prefix, no
    // throw, message intact.
    const thrown = Object.assign(
      new Error('identity: the anchor rejected enrollment: replay (5): that invite was already redeemed'),
      { enrollmentCode: 5 },
    );
    const error = fromWasmError(thrown);
    expect(error.kind).toBe('identity');
    expect(error.message).toBe(thrown.message);
  });
});
