/**
 * The Rust ↔ TypeScript stream ABI, driven from the Rust-generated
 * fixture.
 *
 * Every vector in `fixtures/leaf-abi.json` is a string
 * `net_leaf::node::LeafEvent::to_json` actually produces
 * (`leaf/tests/ts_abi_fixture.rs` asserts that from the other side).
 * Here they are pushed through the shipped {@link LeafStream}, which
 * is the whole of the promise that `onMessage` and `for await` yield
 * `Uint8Array`.
 *
 * The suite that existed before this one could not fail: its test
 * double emitted `Uint8Array` because the hand-written declaration
 * said `Uint8Array`, so it agreed with TypeScript and disagreed with
 * Rust. The fixture removes the double's freedom to be wrong.
 */

import { describe, expect, it } from 'vitest';

import { LeafStream } from '../src/stream.js';
import type { LeafWasmStreamLike, StreamCallbackPayload } from '../src/wasm.js';
import { FakeStream } from './fake-wasm.js';
import { FakeProxyStream } from './fake-leader-wasm.js';
import { STREAM_DATA_VECTORS, streamDataEvent, vectorPayload } from './leaf-abi.js';

/**
 * The decimal peer every hand-driven event below carries, and the
 * hex spelling the handle answers with — the two spellings of one
 * id, which is the whole reason the comparison is numeric.
 */
const PEER = '200';
const PEER_HEX = '00000000000000c8';

/** One `stream_data` event from `PEER`, at the Rust key order. */
function fromPeer(streamId: string, seq: string, payload: Uint8Array): string {
  return streamDataEvent({ peerNode: PEER, incarnation: '1', streamId, seq, payload });
}

/** A stream whose inner object is driven by hand, direct or proxied. */
function harness(streamIdHex: string, proxied = false, peerHex = PEER_HEX) {
  let emit: ((event: StreamCallbackPayload) => void) | undefined;
  let closed = false;
  const inner: LeafWasmStreamLike = {
    send: proxied ? async () => undefined : () => undefined,
    close: () => {
      closed = true;
    },
    is_reliable: () => true,
    stream_id_hex: () => streamIdHex,
    peer_node_hex: () => peerHex,
    incarnation: () => '1',
    on_message: (callback) => {
      emit = callback;
    },
  } as LeafWasmStreamLike;
  const stream = new LeafStream(inner);
  return {
    stream,
    emit: (event: StreamCallbackPayload) => emit?.(event),
    get closed() {
      return closed;
    },
  };
}

/** A vector's peer in the hex spelling the handle answers with. */
function vectorPeerHex(vector: { readonly peerNode: string }): string {
  return BigInt(vector.peerNode).toString(16).padStart(16, '0');
}

describe('the stream callback ABI', () => {
  for (const vector of STREAM_DATA_VECTORS) {
    it(`decodes the leaf's own event JSON: ${vector.note}`, async () => {
      const hex = BigInt(vector.streamId).toString(16);
      const rig = harness(hex.padStart(16, '0'), false, vectorPeerHex(vector));
      const next = rig.stream[Symbol.asyncIterator]().next();
      rig.emit(vector.json);
      await expect(next).resolves.toEqual({ value: vectorPayload(vector), done: false });
    });

    it(`delivers the same bytes on a proxied stream: ${vector.note}`, async () => {
      const hex = BigInt(vector.streamId).toString(16);
      const rig = harness(hex.padStart(16, '0'), true, vectorPeerHex(vector));
      const seen: Uint8Array[] = [];
      rig.stream.onMessage((payload) => seen.push(payload));
      rig.emit(vector.json);
      expect(seen).toEqual([vectorPayload(vector)]);
    });
  }

  it('rebuilds every pinned vector from the double\'s formatter', () => {
    // The lock in the other direction: if Rust's shape changes and
    // the fixture is regenerated, this fails until the double is
    // taught the new shape — so a test double can never again be the
    // only thing that agrees with the package.
    for (const vector of STREAM_DATA_VECTORS) {
      expect(
        streamDataEvent({
          peerNode: vector.peerNode,
          incarnation: vector.incarnation,
          streamId: vector.streamId,
          seq: vector.seq,
          payload: vectorPayload(vector),
        }),
      ).toBe(vector.json);
    }
  });

  it('ignores an event belonging to another stream', async () => {
    const rig = harness('0000000000000009');
    const seen: Uint8Array[] = [];
    rig.stream.onMessage((payload) => seen.push(payload));
    // The node's event stream is shared; Rust's filter is a substring
    // match on the JSON, so the exact one has to be here.
    rig.emit(fromPeer('19', '1', new Uint8Array([7])));
    rig.emit(fromPeer('9', '2', new Uint8Array([8])));
    expect(seen).toEqual([new Uint8Array([8])]);
  });

  it('matches ids across the decimal/hex spelling difference', async () => {
    // `stream_id_hex()` is hex, the event's `stream_id` is decimal.
    // Compared as text, 0x2a and "42" are different streams and every
    // payload is dropped.
    const rig = harness('000000000000002a');
    const next = rig.stream[Symbol.asyncIterator]().next();
    rig.emit(fromPeer('42', '1', new Uint8Array([1, 2, 3])));
    await expect(next).resolves.toEqual({ value: new Uint8Array([1, 2, 3]), done: false });
  });

  it('survives a u64 stream id past the JS safe integer', async () => {
    const rig = harness('ffffffffffffffff');
    const next = rig.stream[Symbol.asyncIterator]().next();
    rig.emit(fromPeer('18446744073709551615', '1', new Uint8Array([4])));
    await expect(next).resolves.toEqual({ value: new Uint8Array([4]), done: false });
  });

  it('drops a non-stream event without disturbing the consumer', async () => {
    const rig = harness('0000000000000009');
    const seen: Uint8Array[] = [];
    rig.stream.onMessage((payload) => seen.push(payload));
    rig.emit('{"type":"disconnected","peer_node":"7","reason":"anchor went away"}');
    rig.emit('not json at all');
    expect(seen).toEqual([]);
    rig.emit(fromPeer('9', '1', new Uint8Array([1])));
    expect(seen).toEqual([new Uint8Array([1])]);
  });

  it('takes bytes as an already-decoded payload', async () => {
    // A host may supply its own LeafWasmStreamLike; and this is the
    // control that proves the JSON decode did not swallow the plain
    // path.
    const rig = harness('0000000000000009');
    const next = rig.stream[Symbol.asyncIterator]().next();
    rig.emit(new Uint8Array([1, 2]));
    await expect(next).resolves.toEqual({ value: new Uint8Array([1, 2]), done: false });
  });

  it('carries the package test doubles, both surfaces', async () => {
    for (const inner of [
      new FakeStream({ reliability: 'reliable' }),
      new FakeProxyStream({ reliability: 'reliable' }),
    ]) {
      const stream = new LeafStream(inner);
      const next = stream[Symbol.asyncIterator]().next();
      inner.arrive(new Uint8Array([9, 9]));
      await expect(next).resolves.toEqual({ value: new Uint8Array([9, 9]), done: false });
    }
  });
});
