// Forwarding for the MeshNode methods S5 added (NODE_SDK_GAPS_PLAN.md S5).
//
// `src/mesh.ts`'s compile-time guard proves each native NetMesh method has a
// MeshNode member; this proves each member forwards: the native receives the
// caller's arguments by identity and the caller gets the native's result.
// Behaviour is covered live in `mesh_node_live.test.ts`.

import { readFileSync } from 'node:fs';
import { join } from 'node:path';

import { describe, expect, it, vi } from 'vitest';

const native = vi.hoisted(() => ({}) as Record<string, ReturnType<typeof vi.fn>>);
vi.mock('@net-mesh/core', async (importOriginal) => {
  const real = await importOriginal<typeof import('@net-mesh/core')>();
  return {
    ...real,
    NetMesh: {
      create: async () =>
        new Proxy(native, {
          // Not `then`: `await` treats any object with a `then` as a promise,
          // and a mock `then` never settles.
          get: (target, prop: string) =>
            prop === 'then' ? undefined : (target[prop] ??= vi.fn(() => ({ from: prop }))),
        }),
    },
  };
});

import { MeshNode } from '../src/index';

// name → the arguments to pass. Sentinel objects, so identity is checked.
const S = (tag: string) => ({ sentinel: tag });
const ROWS: [string, unknown[]][] = [
  ['natType', []],
  ['reflexAddr', []],
  ['peerNatType', [S('peer')]],
  ['probeReflex', [S('peer')]],
  ['reclassifyNat', []],
  ['setReflexOverride', [S('external')]],
  ['clearReflexOverride', []],
  ['connectDirect', [S('peer'), S('pub'), S('coord')]],
  ['connectDirectAuto', [S('peer'), S('pub')]],
  ['traversalStats', []],
  ['serveA2a', [S('executor'), S('options')]],
  ['submitTask', [S('target'), S('prompt'), S('refs'), S('tags'), S('taskId')]],
  ['taskStatus', [S('target'), S('taskId')]],
  ['cancelTask', [S('target'), S('taskId')]],
  ['publishTools', [S('tools'), S('handler'), S('options')]],
  ['rendezvousString', []],
  ['serveEnrollmentAuto', [S('operator'), S('ttl'), S('depth')]],
  ['join', [S('device'), S('invite'), S('name'), S('tags')]],
  ['renew', [S('enrollment')]],
  ['registerPlacementFilter', [S('id'), S('predicate')]],
  ['unregisterPlacementFilter', [S('id')]],
  ['hasPlacementFilter', [S('id')]],
  ['discoveredNodes', []],
  ['pushTo', [S('addr'), S('data')]],
  ['addRoute', [S('dest'), S('hop')]],
];

describe('MeshNode forwards every S5 method to the native node', async () => {
  const node = await MeshNode.create({ bindAddr: '127.0.0.1:0', psk: '42'.repeat(32) });

  it.each(ROWS)('%s', (name, args) => {
    const result = (node as unknown as Record<string, (...a: unknown[]) => unknown>)[name](...args);
    expect(native[name], name).toHaveBeenCalledTimes(1);
    const passed = native[name].mock.calls[0];
    expect(passed.length).toBe(args.length);
    passed.forEach((value: unknown, i: number) => expect(value).toBe(args[i]));
    expect(result).toEqual({ from: name });
  });

  it('the table covers every forwarding method in src/mesh.ts', () => {
    // Read the forwarders out of the source rather than restating a count:
    // a method added to MeshNode in this shape but not to ROWS fails here.
    const src = readFileSync(join(__dirname, '../src/mesh.ts'), 'utf8');
    const forwarders = [
      ...src.matchAll(/^\s+(\w+)\(\.\.\.args: Parameters<NapiNetMesh\['(\w+)'\]>\)/gm),
    ].map((m) => {
      expect(m[2], `${m[1]} forwards to a different native method`).toBe(m[1]);
      return m[1];
    });
    expect(forwarders.length).toBeGreaterThanOrEqual(25);
    expect(ROWS.map(([name]) => name).sort()).toEqual(forwarders.sort());
  });
});
