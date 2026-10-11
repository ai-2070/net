// SDK audit 2026-10-11: a stream-data handler that throws must not escape
// into the native callback, where napi turns it into a fatal
// `uncaughtException` — a remote peer's payload could crash a game host.
// `meshStoreTransport` also isolates each handler, so one throwing handler
// no longer stops delivery to the rest.

import { describe, expect, it, vi } from 'vitest';

vi.mock('@net-mesh/core', () => ({
  streamIdFromLabel: (label: string) => BigInt(label.length),
}));

import { callIsolated } from '../src/_internal';
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

  // Code review 2026-10-11 TS-1: a plain try/catch caught only a
  // synchronous throw. An async handler's rejection escaped as an
  // unhandled rejection — fatal under Node's default policy.
  it('routes an async handler rejection to the error report', async () => {
    const warn = vi.spyOn(process, 'emitWarning').mockImplementation(() => {});
    const unhandled = vi.fn();
    process.on('unhandledRejection', unhandled);
    try {
      const { mesh, callbacks } = fakeMesh();
      const transport = meshStoreTransport(mesh as never, { listen: ['room'] });
      const seen: string[] = [];
      transport.onEvent(async () => {
        throw new Error('bad async peer payload');
      });
      transport.onEvent(frame => seen.push(frame.peerNode));

      callbacks.get(4n)!({ peerNodeId: 7n, streamId: 4n, payload: Buffer.from('x') });
      await new Promise(resolve => setTimeout(resolve, 10));

      expect(seen).toEqual(['7']);
      expect(warn).toHaveBeenCalledWith(
        expect.stringContaining('bad async peer payload'),
        'NetStreamHandlerError',
      );
      expect(unhandled).not.toHaveBeenCalled();
    } finally {
      process.off('unhandledRejection', unhandled);
      warn.mockRestore();
    }
  });
});

describe('callIsolated', () => {
  it('reports a sync throw, a rejection, and swallows a throwing onError', async () => {
    const errors: unknown[] = [];
    callIsolated(() => {
      throw new Error('sync');
    }, e => errors.push((e as Error).message));
    callIsolated(() => Promise.reject(new Error('async')), e => errors.push((e as Error).message));
    expect(() =>
      callIsolated(
        () => {
          throw new Error('x');
        },
        () => {
          throw new Error('onError threw');
        },
      ),
    ).not.toThrow();
    await new Promise(resolve => setTimeout(resolve, 0));
    expect(errors).toEqual(['sync', 'async']);
  });
});
