// SDK audit 2026-10-11: a stream-data handler that throws must not escape
// into the native callback, where napi turns it into a fatal
// `uncaughtException` — a remote peer's payload could crash a game host.
// `meshStoreTransport` also isolates each handler, so one throwing handler
// no longer stops delivery to the rest.

import { describe, expect, it, vi } from 'vitest';

vi.mock('@net-mesh/core', () => ({
  streamIdFromLabel: (label: string) => BigInt(label.length),
}));

import { meshStoreTransport } from '../src/store-transport';

type Native = (data: { peerNodeId: bigint; streamId: bigint; payload: Buffer }) => void;

function fakeMesh() {
  const callbacks = new Map<bigint, Native>();
  const mesh = {
    onStreamData: (id: bigint, cb: Native) => {
      callbacks.set(id, cb);
      return { streamId: id, close: () => callbacks.delete(id) };
    },
  };
  return { mesh, callbacks };
}

describe('meshStoreTransport handler isolation', () => {
  it('keeps delivering after a handler throws, and nothing escapes', () => {
    const warn = vi.spyOn(process, 'emitWarning').mockImplementation(() => {});
    const { mesh, callbacks } = fakeMesh();
    const transport = meshStoreTransport(mesh as never, { listen: ['room'] });
    const seen: string[] = [];
    transport.onEvent(() => {
      throw new Error('bad peer payload');
    });
    transport.onEvent(frame => seen.push(frame.peerNode));

    const native = callbacks.get(4n)!;
    expect(() =>
      native({ peerNodeId: 7n, streamId: 4n, payload: Buffer.from('x') }),
    ).not.toThrow();
    expect(seen).toEqual(['7']);
    expect(warn).toHaveBeenCalledWith(expect.stringContaining('bad peer payload'), 'NetStreamHandlerError');
    warn.mockRestore();
  });
});
