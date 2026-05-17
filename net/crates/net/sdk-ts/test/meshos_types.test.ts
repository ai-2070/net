// Type-level tests for the MeshOsDaemon shape in sdk-ts/src/meshos.ts.
//
// These tests don't touch the napi runtime — they verify only that the
// TypeScript type accepts every callback / advertisement shape the
// underlying napi MeshOsDaemonBridge supports (process / snapshot /
// restore / onControl / health / saturation, plus
// requiredCapabilities / optionalCapabilities). A regression that
// trimmed a field off the TS interface (which the pre-fix shape did
// for health + saturation + capabilities) would fail to compile here.

import { describe, expectTypeOf, it } from 'vitest';

import type {
  CapabilityAdvert,
  CausalEvent,
  DaemonControl,
  DaemonHealth,
  MeshOsDaemon,
} from '../src/meshos';

describe('MeshOsDaemon TS surface', () => {
  it('accepts a minimal daemon with only required fields', () => {
    const d: MeshOsDaemon = {
      name: 'minimal',
      process: (_event: CausalEvent) => [Buffer.from('out')],
    };
    expectTypeOf(d).toEqualTypeOf<MeshOsDaemon>();
  });

  it('accepts every optional callback + capability shape', () => {
    const d: MeshOsDaemon = {
      name: 'full',
      process: (event: CausalEvent) => [event.payload],
      snapshot: () => Buffer.from('state'),
      restore: (_state: Buffer) => {},
      onControl: (_event: DaemonControl) => {},
      health: () => 'degraded' as DaemonHealth,
      saturation: () => 0.75,
      requiredCapabilities: ['hardware.gpu'],
      optionalCapabilities: () => ['software.cuda'],
    };
    expectTypeOf(d).toEqualTypeOf<MeshOsDaemon>();
  });

  it('accepts the health object form with optional reason', () => {
    const d: MeshOsDaemon = {
      name: 'health-obj',
      process: () => [],
      health: () => ({ kind: 'degraded', reason: 'queue depth high' }),
    };
    expectTypeOf(d.health).toMatchTypeOf<(() => DaemonHealth) | undefined>();
  });

  it('rejects an unknown health kind at the type level', () => {
    // @ts-expect-error — health kind must be one of the three literals
    const d: MeshOsDaemon = {
      name: 'bad-health',
      process: () => [],
      health: () => ({ kind: 'on-fire' }),
    };
    void d;
  });

  it('accepts capability advert as static array or callable', () => {
    const staticAdvert: CapabilityAdvert = ['hardware.gpu'];
    const dynamicAdvert: CapabilityAdvert = () => ['software.cuda'];
    expectTypeOf(staticAdvert).toMatchTypeOf<CapabilityAdvert>();
    expectTypeOf(dynamicAdvert).toMatchTypeOf<CapabilityAdvert>();
  });
});
