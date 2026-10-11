/**
 * SDK audit 2026-10-11: `joinStore().reconnect()` dropped its upstream
 * stream without closing it, so every reconnect leaked one stream handle
 * (and its wasm listener on a real node). It now closes the old stream.
 */

import { describe, expect, it } from 'vitest';

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
  id: 'reconnect-stream-test.world',
  version: 1,
  state: value => ({ n: Number((value as { n: unknown }).n) }),
  empty: () => ({ n: 0 }),
  visibility: 'open',
  actions: { poke: { input: () => ({}), output: () => ({}) } },
  inputs: {},
});

function counting(inner: StoreTransport) {
  const counts = { opened: 0, closed: 0 };
  const transport: StoreTransport = {
    nodeIdHex: () => inner.nodeIdHex(),
    onEvent: handler => inner.onEvent(handler),
    openStream: async options => {
      const stream = await inner.openStream(options);
      counts.opened += 1;
      let open = true;
      return {
        send: payload => stream.send(payload),
        close: () => {
          if (open) {
            open = false;
            counts.closed += 1;
          }
          stream.close();
        },
      };
    },
  };
  return { transport, counts };
}

describe('joinStore reconnect', () => {
  it('closes the stream it replaces instead of leaking it', async () => {
    const mesh = createLocalMesh();
    const host = hostStore<World, Actions, Record<string, never>>({
      definition: world,
      transport: mesh.node(HOST),
      initialState: { n: 0 },
      maxEventBytes: 8104,
      schedule: never,
      authorize: () => true,
      actions: { poke: () => ({}) },
      inputs: {},
    });
    const { transport, counts } = counting(mesh.node(PLAYER));
    const player = joinStore<World, Actions, Record<string, never>>({
      definition: world,
      transport,
      host: HOST,
      audience: [],
      key: 'player',
      maxEventBytes: 8104,
      schedule: never,
    });
    await player.ready();
    for (let i = 0; i < 3; i += 1) await player.reconnect();
    // Only the current upstream may be open.
    expect(counts.opened - counts.closed).toBe(1);
    await player.close();
    expect(counts.opened - counts.closed).toBe(0);
    await host.close();
  });

  // Code review 2026-10-11 BROWSER-2: reconnect() left an in-flight open
  // from the lost session in place, so it inherited that open's late
  // rejection (or hung on one that never settled).
  it("does not inherit a lost session's in-flight open", async () => {
    const mesh = createLocalMesh();
    const host = hostStore<World, Actions, Record<string, never>>({
      definition: world,
      transport: mesh.node(HOST),
      initialState: { n: 0 },
      maxEventBytes: 8104,
      schedule: never,
      authorize: () => true,
      actions: { poke: () => ({}) },
      inputs: {},
    });
    const inner = mesh.node(PLAYER);
    let holdNext = false;
    let failHeld: (error: Error) => void = () => {};
    const transport: StoreTransport = {
      nodeIdHex: () => inner.nodeIdHex(),
      onEvent: handler => inner.onEvent(handler),
      openStream: options => {
        if (holdNext) {
          holdNext = false;
          // The lost session's open: pending until the test fails it.
          return new Promise((_, reject) => {
            failHeld = reject;
          });
        }
        return inner.openStream(options);
      },
    };
    const player = joinStore<World, Actions, Record<string, never>>({
      definition: world,
      transport,
      host: HOST,
      audience: [],
      key: 'player',
      maxEventBytes: 8104,
      schedule: never,
    });
    await player.ready();

    holdNext = true;
    const lost = player.reconnect(); // its open is held, then fails
    lost.catch(() => {});
    await new Promise(resolve => setTimeout(resolve, 0));
    const fresh = player.reconnect();
    failHeld(new Error('no session (lost)'));
    await expect(fresh).resolves.toBeUndefined();
    await expect(lost).rejects.toBeDefined();

    await player.close();
    await host.close();
  });
});
