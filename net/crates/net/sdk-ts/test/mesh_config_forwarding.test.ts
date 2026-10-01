// Every MeshNodeConfig option must reach the native constructor unchanged
// (NODE_SDK_GAPS_PLAN.md S2). `MeshNode.create` used to drop `reflexOverride`,
// `tryPortMapping`, `autoDirectUpgrade` and `permissiveChannels`: declaring an
// option is half the job, and `src/mesh.ts`'s compile-time guard covers only
// that half. This test covers the other: a distinct sentinel per option, and
// each must arrive at `NetMesh.create` as itself.

import { describe, expect, it, vi } from 'vitest';

// The napi class's static `create` is non-configurable, so it can't be
// spied on; substitute it at the module boundary instead. Everything else in
// `@net-mesh/core` stays real.
const seen = vi.hoisted(() => [] as Record<string, unknown>[]);
vi.mock('@net-mesh/core', async (importOriginal) => {
  const real = await importOriginal<typeof import('@net-mesh/core')>();
  return {
    ...real,
    NetMesh: {
      create: async (opts: Record<string, unknown>) => {
        seen.push(opts);
        return {};
      },
    },
  };
});

import { MeshNode, type MeshNodeConfig } from '../src/index';

describe('MeshNode.create option forwarding', () => {
  it('passes every config option to the native constructor unchanged', async () => {
    seen.length = 0;
    // Distinct sentinel objects: identity, not equality, so a swapped or
    // re-wrapped value is caught. The `as` reflects that the values are
    // deliberately not well-typed; only their identity matters here.
    const keys: (keyof MeshNodeConfig)[] = [
      'bindAddr',
      'psk',
      'heartbeatIntervalMs',
      'sessionTimeoutMs',
      'numShards',
      'capabilityGcIntervalMs',
      'requireSignedCapabilities',
      'subnet',
      'subnetPolicy',
      'identitySeed',
      'subnetAuthorities',
      'subnetAttachment',
      'subnetControlChannel',
      'subnetExports',
      'reflexOverride',
      'tryPortMapping',
      'autoDirectUpgrade',
      'permissiveChannels',
    ];
    const sentinels = Object.fromEntries(keys.map((k) => [k, { sentinel: k }]));
    await MeshNode.create(sentinels as unknown as MeshNodeConfig);

    expect(seen).toHaveLength(1);
    for (const key of keys) {
      expect(seen[0][key], key).toBe(sentinels[key]);
    }
  });

  it('the sentinel list covers every option the native constructor takes', () => {
    // Paired with the compile-time guard: if the native MeshOptions grows a
    // field, this list (and MeshNodeConfig) must grow with it. The native
    // declaration is read from the generated `index.d.ts`.
    // eslint-disable-next-line @typescript-eslint/no-require-imports
    const { readFileSync } = require('node:fs') as typeof import('node:fs');
    // eslint-disable-next-line @typescript-eslint/no-require-imports
    const { join, dirname } = require('node:path') as typeof import('node:path');
    const coreTypes = join(dirname(require.resolve('@net-mesh/core')), 'index.d.ts');
    const decl = readFileSync(coreTypes, 'utf8');
    const body = /export interface MeshOptions \{([\s\S]*?)\n\}/.exec(decl)?.[1] ?? '';
    const native = [...body.matchAll(/^\s+(\w+)\??:/gm)].map((m) => m[1]).sort();
    expect(native.length).toBeGreaterThan(10);
    expect(native).toEqual(
      [
        'bindAddr',
        'psk',
        'heartbeatIntervalMs',
        'sessionTimeoutMs',
        'numShards',
        'capabilityGcIntervalMs',
        'requireSignedCapabilities',
        'subnet',
        'subnetPolicy',
        'identitySeed',
        'subnetAuthorities',
        'subnetAttachment',
        'subnetControlChannel',
        'subnetExports',
        'reflexOverride',
        'tryPortMapping',
        'autoDirectUpgrade',
        'permissiveChannels',
      ].sort(),
    );
  });
});
