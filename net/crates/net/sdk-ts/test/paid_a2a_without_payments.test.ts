// `setA2aOrgCaller` against a `@net-mesh/core` built without `payments`
// (code review): that build exports no `CapabilityGateway`, and the mesh
// slot — which needs only `a2a` + `org` — must still work. An unguarded
// `instanceof` against the missing class threw "Right-hand side of
// 'instanceof' is not callable" for every target.

import { describe, expect, it, vi } from 'vitest';

vi.mock('@net-mesh/core', async (importOriginal) => {
  const real = await importOriginal<typeof import('@net-mesh/core')>();
  return { ...real, CapabilityGateway: undefined };
});

import { setA2aOrgCaller } from '../src/payments';

describe('setA2aOrgCaller without the payments feature', () => {
  it('installs on a native mesh target', () => {
    const calls: unknown[] = [];
    const mesh = { setA2aOrgCaller: (org: unknown) => void calls.push(org) };
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    expect(() => setA2aOrgCaller(mesh as any, null)).not.toThrow();
    expect(calls).toEqual([null]);
  });
});
