/**
 * SDK audit 2026-10-11: a `LeafStream` nobody reads must not keep every
 * frame the peer sends.
 *
 * The store, netcode and region-link layers open streams only to `send`
 * and read inbound traffic through the transport's event hub, so the
 * pre-consumer buffer of each of those streams used to grow at the peer's
 * send rate for the stream's lifetime. It is now bounded (frames and
 * bytes, oldest dropped and counted), while a consumer that attaches
 * within the bound still receives everything in order.
 */

import { describe, expect, it } from 'vitest';

import {
  LEAF_STREAM_PENDING_MAX_BYTES,
  LEAF_STREAM_PENDING_MAX_FRAMES,
  LeafStream,
} from '../src/stream.js';
import type { LeafWasmStream } from '../src/wasm.js';

function fake() {
  let deliver: (payload: Uint8Array) => void = () => {};
  const inner = {
    stream_id_hex: () => '0000000000000009',
    peer_node_hex: () => '00000000000000aa',
    incarnation: () => '1',
    is_reliable: () => true,
    on_message: (cb: (event: Uint8Array) => void) => {
      deliver = cb;
    },
    send: () => undefined,
    close: () => undefined,
  } as unknown as LeafWasmStream;
  return { stream: new LeafStream(inner), deliver: (p: Uint8Array) => deliver(p) };
}

const frame = (n: number, size = 4) => {
  const out = new Uint8Array(size);
  new DataView(out.buffer).setUint32(0, n);
  return out;
};
const idOf = (p: Uint8Array) => new DataView(p.buffer, p.byteOffset).getUint32(0);

describe('LeafStream pre-consumer buffer', () => {
  it('caps buffered frames, dropping the oldest and counting them', () => {
    const { stream, deliver } = fake();
    const total = LEAF_STREAM_PENDING_MAX_FRAMES + 50;
    for (let i = 0; i < total; i += 1) deliver(frame(i));
    expect(stream.droppedBeforeConsumer).toBe(50);

    const seen: number[] = [];
    stream.onMessage((p) => seen.push(idOf(p)));
    expect(seen.length).toBe(LEAF_STREAM_PENDING_MAX_FRAMES);
    expect(seen[0]).toBe(50); // the newest frames survive, in order
    expect(seen[seen.length - 1]).toBe(total - 1);
  });

  it('caps buffered bytes', () => {
    const { stream, deliver } = fake();
    const big = 256 * 1024;
    const count = Math.ceil(LEAF_STREAM_PENDING_MAX_BYTES / big) + 3;
    for (let i = 0; i < count; i += 1) deliver(frame(i, big));
    const seen: number[] = [];
    stream.onMessage((p) => seen.push(idOf(p)));
    expect(seen.length * big).toBeLessThanOrEqual(LEAF_STREAM_PENDING_MAX_BYTES);
    expect(stream.droppedBeforeConsumer).toBe(count - seen.length);
    expect(seen[seen.length - 1]).toBe(count - 1);
  });

  it('a consumer attaching within the bound loses nothing', () => {
    const { stream, deliver } = fake();
    for (let i = 0; i < 10; i += 1) deliver(frame(i));
    const seen: number[] = [];
    stream.onMessage((p) => seen.push(idOf(p)));
    deliver(frame(10));
    expect(seen).toEqual([0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
    expect(stream.droppedBeforeConsumer).toBe(0);
  });

  // Code review 2026-10-11 BROWSER-1: draining the buffer left the byte
  // count at the drained total, so a stream that rebuffered after its
  // consumer detached evicted nearly every frame.
  it.each(['onMessage', 'asyncIterator'])(
    'rebuffers in full after a %s drain and detach',
    async (how) => {
      const { stream, deliver } = fake();
      const big = 256 * 1024;
      const fill = Math.floor(LEAF_STREAM_PENDING_MAX_BYTES / big) - 1;
      for (let i = 0; i < fill; i += 1) deliver(frame(i, big));
      if (how === 'onMessage') {
        const cancel = stream.onMessage(() => {});
        cancel();
      } else {
        const it = stream[Symbol.asyncIterator]();
        for (let i = 0; i < fill; i += 1) await it.next();
        await it.return?.();
      }
      expect(stream.droppedBeforeConsumer).toBe(0);

      for (let i = 0; i < fill; i += 1) deliver(frame(100 + i, big));
      expect(stream.droppedBeforeConsumer).toBe(0);
      const seen: number[] = [];
      stream.onMessage((p) => seen.push(idOf(p)));
      expect(seen.length).toBe(fill);
    },
  );
});
