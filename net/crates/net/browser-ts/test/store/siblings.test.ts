/**
 * Two stores of one definition hosted on ONE node (the `store` option): a
 * player's messages reach the store that issued its handle, and a handle
 * no store issued is refused exactly once.
 *
 * Every hosted store on a node sees every frame. Before, a store that did
 * not know a handle answered `closed` on the owner's behalf, and when it
 * answered first the player's action was refused although the store it was
 * for was serving it.
 */

import { describe, expect, it } from 'vitest';

import { createLocalMesh } from '../../src/local.js';
import { defineStore } from '../../src/store/definition.js';
import { hostStore } from '../../src/store/host.js';
import { joinStore } from '../../src/store/join.js';
import { decodeMessage, encodeMessage, type Hex } from '../../src/store/wire.js';

const HOST = '00000000000000aa';
const PLAYER = '00000000000000b1';
const PROBE = '00000000000000b2';

type Actions = { name: { input: Record<string, never>; output: { readonly store: string } } };
const definition = defineStore<{ readonly n: number }, Actions, Record<string, never>>({
  id: 'siblings-test',
  version: 1,
  state: value => ({ n: Number((value as { n: unknown }).n) }),
  empty: () => ({ n: 0 }),
  visibility: 'open',
  actions: { name: { input: () => ({}), output: value => ({ store: String((value as { store: unknown }).store) }) } },
  inputs: {},
});

describe('stores sharing a node', () => {
  it('route every message to the store that issued its handle, and refuse an unknown one once', async () => {
    const mesh = createLocalMesh();
    const hostNode = mesh.node(HOST);
    const hosts = ['north', 'south'].map(store =>
      hostStore({
        definition,
        store,
        transport: hostNode,
        initialState: { n: 0 },
        maxEventBytes: 8104,
        authorize: () => true,
        actions: { name: () => ({ store }) },
        inputs: {},
      }),
    );
    const playerNode = mesh.node(PLAYER);
    const replicas = ['north', 'south'].map(store =>
      joinStore({ definition, store, transport: playerNode, host: HOST, audience: [], key: 'p', maxEventBytes: 8104 }),
    );
    await Promise.all(replicas.map(replica => replica.ready()));
    for (let round = 0; round < 5; round += 1) {
      await expect(replicas[0]!.act('name', {})).resolves.toEqual({ store: 'north' });
      await expect(replicas[1]!.act('name', {})).resolves.toEqual({ store: 'south' });
    }

    // A handle nobody issued: one `closed`, not one per store.
    const probe = mesh.node(PROBE);
    const refusals: string[] = [];
    probe.onEvent(event => {
      if (event.payload === undefined) return;
      const decoded = decodeMessage(new TextDecoder().decode(event.payload), { maxBytes: 8104, as: 'replica' });
      if (decoded.ok && decoded.message.k === 'no') refusals.push(decoded.message.code);
    });
    const stream = await probe.openStream({ reliability: 'reliable', peer: HOST, label: 'store/siblings-test' });
    await stream.send(
      new TextEncoder().encode(encodeMessage({ k: 'alive', q: '0123456789abcdef' as Hex, h: 'f'.repeat(32) as Hex })),
    );
    await new Promise(resolve => setTimeout(resolve, 50));
    expect(refusals).toEqual(['closed']);

    for (const replica of replicas) await replica.close();
    for (const host of hosts) await host.close();
  });
});
