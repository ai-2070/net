/**
 * Stream ownership: one wire id, two peers, four operations.
 *
 * A stream id is an application label scoped to a session, so **two
 * streams to two peers under one id is the ordinary case**, not a
 * collision. Every operation therefore has to be owner-scoped, and
 * before this repair three of the four were not on a proxied stream:
 * the handle could not name its peer at all, so receive fell back to
 * id-only admission, and the leader's backend keyed its open-stream
 * table by the wire id, so `send` and `close` selected another peer's
 * stream.
 *
 * These witnesses drive the **production** `LeafStream` over a handle
 * boundary a test controls, in both shapes — the direct one whose
 * `send` is synchronous and the proxied one whose `send` is a promise
 * — because a consumer must not need to know which it holds.
 *
 * The inverse of each is the pre-repair behaviour, and it is available
 * as a first-class case rather than a mutation: constructing with
 * `identity: 'optional'` is exactly the peer-blind stream, and the
 * control at the end asserts it admits both peers' frames. A witness
 * whose failure mode is not demonstrable is a witness that has not
 * been shown to discriminate.
 */

import { describe, expect, it } from 'vitest';

import { LeafStream, StreamIdentityError } from '../src/stream.js';
import type { LeafWasmProxyStream, LeafWasmStream } from '../src/wasm.js';
import { streamDataEvent } from './leaf-abi.js';

const WIRE_ID = '0000000000000009';
const WIRE_ID_DECIMAL = '9';
const PEER_A = '00000000000000aa';
const PEER_B = '00000000000000bb';

interface Handle {
  readonly sent: Uint8Array[];
  closed: boolean;
  /** Deliver one event to every listener this handle has. */
  deliver(event: string): void;
  readonly wasm: LeafWasmStream & LeafWasmProxyStream;
}

/**
 * A handle in either shape.
 *
 * `shape: 'proxied'` makes `send` a promise, which is the only
 * difference the boundary has — the point of the repair being that the
 * identity is no longer the other difference.
 */
function handle(options: {
  peer?: string | null;
  id?: string;
  shape?: 'direct' | 'proxied';
}): Handle {
  const listeners: ((event: string) => void)[] = [];
  const state: Handle = {
    sent: [],
    closed: false,
    deliver(event) {
      for (const listener of listeners) listener(event);
    },
    wasm: {
      stream_id_hex: () => options.id ?? WIRE_ID,
      peer_node_hex: () => {
        if (options.peer === null) throw new Error('this handle has no peer accessor');
        return options.peer ?? PEER_A;
      },
      incarnation: () => '1',
      is_reliable: () => true,
      on_message: (callback: (event: string) => void) => {
        listeners.push(callback);
      },
      send:
        options.shape === 'proxied'
          ? (payload: Uint8Array) => {
              state.sent.push(payload);
              return Promise.resolve();
            }
          : (payload: Uint8Array) => {
              state.sent.push(payload);
            },
      close: () => {
        state.closed = true;
      },
    } as unknown as LeafWasmStream & LeafWasmProxyStream,
  };
  return state;
}

function frame(peer: string, byte: number, seq: string): string {
  return streamDataEvent({
    peerNode: BigInt(`0x${peer}`).toString(10),
    incarnation: '1',
    streamId: WIRE_ID_DECIMAL,
    seq,
    payload: new Uint8Array([byte]),
  });
}

/** Collect what a stream delivers to a listener. */
function received(stream: LeafStream): number[] {
  const bytes: number[] = [];
  stream.onMessage((payload) => bytes.push(payload[0] as number));
  return bytes;
}

for (const shape of ['direct', 'proxied'] as const) {
  describe(`ownership on a ${shape} stream`, () => {
    it('two peers under one wire id receive only their own frames', () => {
      const a = handle({ peer: PEER_A, shape });
      const b = handle({ peer: PEER_B, shape });
      const streamA = new LeafStream(a.wasm);
      const streamB = new LeafStream(b.wasm);
      const gotA = received(streamA);
      const gotB = received(streamB);

      // Both frames reach both listeners, which is what the node-wide
      // event vector does. The wrapper is the thing that must sort
      // them, and the wire id cannot: it is the same id.
      for (const h of [a, b]) {
        h.deliver(frame(PEER_A, 1, '1'));
        h.deliver(frame(PEER_B, 2, '1'));
      }

      expect(gotA).toEqual([1]);
      expect(gotB).toEqual([2]);
    });

    it('each stream sends to its own handle', async () => {
      const a = handle({ peer: PEER_A, shape });
      const b = handle({ peer: PEER_B, shape });
      const streamA = new LeafStream(a.wasm);
      const streamB = new LeafStream(b.wasm);

      await streamA.send(new Uint8Array([7]));

      expect(a.sent).toHaveLength(1);
      expect(a.sent[0]?.[0]).toBe(7);
      // The other peer's stream, under the same wire id, got nothing.
      expect(b.sent).toHaveLength(0);

      await streamB.send(new Uint8Array([8]));
      expect(b.sent[0]?.[0]).toBe(8);
      expect(a.sent).toHaveLength(1);
    });

    it('closing one leaves the other usable', async () => {
      const a = handle({ peer: PEER_A, shape });
      const b = handle({ peer: PEER_B, shape });
      const streamA = new LeafStream(a.wasm);
      const streamB = new LeafStream(b.wasm);
      const gotB = received(streamB);

      streamA.close();

      expect(a.closed).toBe(true);
      // B's underlying handle is untouched …
      expect(b.closed).toBe(false);
      // … and B still receives and still sends.
      b.deliver(frame(PEER_B, 5, '1'));
      expect(gotB).toEqual([5]);
      await streamB.send(new Uint8Array([6]));
      expect(b.sent[0]?.[0]).toBe(6);
    });

    it('exposes the peer it filters on', () => {
      const stream = new LeafStream(handle({ peer: PEER_B, shape }).wasm);
      expect(stream.peerNode).toBe(PEER_B);
    });
  });
}

describe('identity is required, and fails closed', () => {
  it('refuses a handle with no peer accessor', () => {
    const missing = handle({ peer: null });
    expect(() => new LeafStream(missing.wasm)).toThrow(StreamIdentityError);
  });

  it('refuses a handle whose peer is not readable', () => {
    for (const malformed of ['', 'zzzzzzzzzzzzzzzz', '0x00aa', 'aa'.repeat(20)]) {
      const bad = handle({ peer: malformed });
      expect(() => new LeafStream(bad.wasm), malformed).toThrow(StreamIdentityError);
    }
  });

  it('does not silently fall back to admitting every peer', () => {
    // The failure this replaces: a stream that could not read its peer
    // kept working and filtered on the id alone.
    const missing = handle({ peer: null });
    let constructed = false;
    try {
      new LeafStream(missing.wasm);
      constructed = true;
    } catch {
      /* expected */
    }
    expect(constructed).toBe(false);
  });

  it('accepts a shorter-than-16 hex peer, since the leaf pads', () => {
    // A control on the strictness: `u64FromHex` accepts 1..16 hex
    // digits, so this must NOT be refused — the repair is about
    // unreadable identities, not about spelling pedantry.
    const stream = new LeafStream(handle({ peer: 'aa' }).wasm);
    expect(stream.peerNode).toBe('aa');
  });
});

describe('the pre-repair behaviour, as a control', () => {
  it('a peer-blind stream admits both peers under one wire id', () => {
    // `identity: 'optional'` is the old disposition, kept for a
    // host-supplied wrapper with no peer to report. It is exactly the
    // cross-peer admixture, which is why nothing in `src/` uses it —
    // and why it belongs here, as the demonstration that the
    // witnesses above discriminate.
    const blind = handle({ peer: null });
    const stream = new LeafStream(blind.wasm, undefined, 'optional');
    const got = received(stream);

    blind.deliver(frame(PEER_A, 1, '1'));
    blind.deliver(frame(PEER_B, 2, '1'));

    expect(got).toEqual([1, 2]);
  });

  it('a peer-bearing stream refuses the same frames', () => {
    const owned = handle({ peer: PEER_A });
    const stream = new LeafStream(owned.wasm);
    const got = received(stream);

    owned.deliver(frame(PEER_A, 1, '1'));
    owned.deliver(frame(PEER_B, 2, '1'));

    expect(got).toEqual([1]);
  });
});
