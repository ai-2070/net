/**
 * A refusal that lands while the request's `send` is still pending.
 *
 * A native mesh answers inside `send` (the frame is delivered and the
 * host's `no` arrives before the send's own promise settles). The
 * replica's request promise then rejected with no handler attached yet:
 * an unhandled rejection, though the caller received the error a moment
 * later. Found by the sdk-ts end-to-end test in CI.
 */

import { afterEach, describe, expect, it } from 'vitest';

import { createLocalMesh } from '../../src/local.js';
import { defineStore } from '../../src/store/definition.js';
import { hostStore, type StoreTransport } from '../../src/store/host.js';
import { joinStore } from '../../src/store/join.js';
import type { Cancel } from '../../src/store/types.js';

const HOST = '00000000000000aa';
const PLAYER = '00000000000000b1';
const never = (): Cancel => () => {};

interface World {
  readonly n: number;
}
type Actions = { poke: { input: Record<string, never>; output: Record<string, never> } };

const world = defineStore<World, Actions, Record<string, never>>({
  id: 'slow-send-test.world',
  version: 1,
  state: value => ({ n: Number((value as { n: unknown }).n) }),
  empty: () => ({ n: 0 }),
  visibility: 'open',
  actions: { poke: { input: () => ({}), output: () => ({}) } },
  inputs: {},
});

/** The player's transport: every send delivers at once but settles 20 ms later. */
function slowSender(inner: StoreTransport): StoreTransport {
  return {
    nodeIdHex: () => inner.nodeIdHex(),
    onEvent: handler => inner.onEvent(handler),
    openStream: async options => {
      const stream = await inner.openStream(options);
      return {
        send: async payload => {
          await stream.send(payload);
          await new Promise(resolve => setTimeout(resolve, 20));
        },
        close: () => stream.close(),
      };
    },
  };
}

// Node's process, without Node's types (this package's tests are typed for
// the browser).
const node = (
  globalThis as unknown as {
    process: {
      on(event: 'unhandledRejection', listener: (reason: unknown) => void): void;
      off(event: 'unhandledRejection', listener: (reason: unknown) => void): void;
    };
  }
).process;

const unhandled: unknown[] = [];
const onUnhandled = (reason: unknown): void => {
  unhandled.push(reason);
};

afterEach(() => {
  node.off('unhandledRejection', onUnhandled);
});

describe('a refusal during a pending send', () => {
  it('reaches the caller and is never an unhandled rejection', async () => {
    node.on('unhandledRejection', onUnhandled);
    const mesh = createLocalMesh();
    const host = hostStore<World, Actions, Record<string, never>>({
      definition: world,
      transport: mesh.node(HOST),
      initialState: { n: 0 },
      maxEventBytes: 8104,
      schedule: never,
      authorize: request => request.type !== 'action',
      actions: { poke: () => ({}) },
      inputs: {},
    });
    const player = joinStore<World, Actions, Record<string, never>>({
      definition: world,
      transport: slowSender(mesh.node(PLAYER)),
      host: HOST,
      audience: [],
      key: 'player',
      maxEventBytes: 8104,
      schedule: never,
    });
    await player.ready();

    await expect(player.act('poke', {})).rejects.toMatchObject({ code: 'forbidden' });
    await new Promise(resolve => setTimeout(resolve, 50));
    expect(unhandled).toEqual([]);
    await player.close();
    await host.close();
  });
});
