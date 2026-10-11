// SDK audit 2026-10-11: native invokes a registered daemon factory itself
// (migration-in, ReplicaGroup / ForkGroup / StandbyGroup spawn) and calls
// the methods it reads off the result through threadsafe functions with an
// `undefined` receiver. `registerFactory` used to hand native the raw user
// factory, so a class-based daemon using `this` threw on every event there
// while working under `spawn()`. The SDK now hands native a synchronous
// adapter returning methods bound to the instance.

import { describe, expect, it, vi } from 'vitest';

const registered = vi.hoisted(() => new Map<string, () => Record<string, unknown>>());
vi.mock('@net-mesh/core', () => ({
  DaemonRuntime: {
    create: () => ({
      registerFactory: (kind: string, f: () => Record<string, unknown>) => {
        registered.set(kind, f);
      },
    }),
  },
  DaemonHandle: class {},
  MigrationHandle: class {},
}));

import { bindDaemonObject, setNapiMesh } from '../src/_internal';
import { DaemonError, DaemonRuntime, type CausalEvent } from '../src/compute';

class Counter {
  readonly name = 'counter';
  count = 0;
  requiredCapabilities = { tags: ['hardware.gpu'] };
  process(_event: CausalEvent): Buffer[] {
    this.count += 1;
    return [Buffer.from(String(this.count))];
  }
  snapshot(): Buffer {
    return Buffer.from(String(this.count));
  }
}

function runtime(): DaemonRuntime {
  const mesh = {};
  setNapiMesh(mesh, {} as never);
  return DaemonRuntime.create(mesh as never);
}

// Invoke the way the native TSFN does: detached, `this === undefined`.
function callDetached<T>(fn: unknown, ...args: unknown[]): T {
  return (fn as (...a: unknown[]) => T).apply(undefined, args);
}

describe('registerFactory native adapter', () => {
  it('binds class methods so native calls keep `this`', () => {
    const rt = runtime();
    rt.registerFactory('counter', () => new Counter());
    const flat = registered.get('counter')!();
    expect(flat.name).toBe('counter');
    expect(flat.requiredCapabilities).toEqual({ tags: ['hardware.gpu'] });
    const out = callDetached<Buffer[]>(flat.process, {} as CausalEvent);
    expect(out[0].toString()).toBe('1');
    expect(callDetached<Buffer>(flat.snapshot).toString()).toBe('1');
    expect(flat.restore).toBeUndefined();
  });

  it('rejects an async factory on the native path with a named DaemonError', () => {
    const rt = runtime();
    rt.registerFactory('slow', async () => new Counter());
    expect(() => registered.get('slow')!()).toThrow(DaemonError);
    expect(() => registered.get('slow')!()).toThrow(/synchronous factory/);
  });
});

describe('bindDaemonObject', () => {
  it('reads prototype methods and skips absent keys', () => {
    const c = new Counter();
    const flat = bindDaemonObject(c, ['process', 'restore', 'name']);
    expect(Object.keys(flat).sort()).toEqual(['name', 'process']);
    callDetached(flat.process, {});
    expect(c.count).toBe(1);
  });
});
