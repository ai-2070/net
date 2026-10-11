// Code review 2026-10-11 NODE-1: the native StreamDataSubscription now
// unregisters when garbage-collected, so a caller that discarded the handle
// `onStreamData` returned had delivery stop at an arbitrary GC. MeshNode
// holds each subscription until `close()` (or shutdown), so its lifetime is
// the caller's choice again, not the collector's.

import { describe, expect, it, vi } from 'vitest';

const subs = vi.hoisted(() => [] as { closed: number }[]);
vi.mock('@net-mesh/core', async (importOriginal) => {
  const real = await importOriginal<typeof import('@net-mesh/core')>();
  return {
    ...real,
    NetMesh: {
      create: async () => ({
        onStreamData: (streamId: bigint) => {
          const sub = {
            closed: 0,
            streamId,
            droppedEvents: 0n,
            close() {
              this.closed += 1;
              return this.closed === 1;
            },
          };
          subs.push(sub);
          return sub;
        },
        shutdown: async () => {},
      }),
    },
  };
});

import { MeshNode } from '../src/index';

const held = (node: MeshNode) =>
  (node as unknown as { streamSubscriptions: Set<unknown> }).streamSubscriptions;

describe('MeshNode.onStreamData keeps its subscriptions alive', () => {
  it('retains a discarded handle until close()', async () => {
    const node = await MeshNode.create({ bindAddr: '127.0.0.1:0', psk: '42'.repeat(32) });
    node.onStreamData(7n, () => {}); // handle discarded
    const kept = node.onStreamData(8n, () => {});
    expect(held(node).size).toBe(2);
    expect(held(node).has(subs[0])).toBe(true);

    expect(kept.close()).toBe(true);
    expect(held(node).size).toBe(1);
    expect(kept.close()).toBe(false); // idempotent

    await node.shutdown();
    expect(held(node).size).toBe(0);
  });
});
