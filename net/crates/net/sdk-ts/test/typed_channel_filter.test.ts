// SDK audit 2026-10-11: a caller filter on TypedChannel.subscribe() used to
// REPLACE the channel scoping, so events from every channel matching it came
// back typed as T (with the revealing `_channel` tag stripped). It is now
// AND-ed with the channel's own filter.
import { describe, expect, it } from 'vitest';
import { TypedChannel } from '../src/channel';

function capturingBus() {
  const filters: (string | undefined)[] = [];
  return {
    filters,
    poll: async (req: { filter?: string }) => {
      filters.push(req.filter);
      return { events: [{ id: '1', raw: '{"_channel":"temps","v":1}' }], nextId: '1', hasMore: false };
    },
  };
}

describe('TypedChannel subscribe filter', () => {
  it('ANDs a caller filter with the channel scoping', async () => {
    const bus = capturingBus();
    const ch = new TypedChannel<{ v: number }>(bus as never, 'temps');
    const stream = ch.subscribe({ filter: '{"path":"v","value":1}', pollIntervalMs: 0 });
    for await (const ev of stream) {
      expect(ev).toEqual({ v: 1 });
      break;
    }
    expect(JSON.parse(bus.filters[0]!)).toEqual({
      $and: [
        { path: '_channel', value: 'temps' },
        { path: 'v', value: 1 },
      ],
    });
  });

  it('uses the channel filter alone when none is given', async () => {
    const bus = capturingBus();
    const ch = new TypedChannel<{ v: number }>(bus as never, 'temps');
    for await (const _ of ch.subscribe({ pollIntervalMs: 0 })) break;
    expect(JSON.parse(bus.filters[0]!)).toEqual({ path: '_channel', value: 'temps' });
  });
});

// SDK audit 2026-10-11: spreading a scalar published `{"_channel": …}`
// and the subscriber got `{}`. Non-object events are wrapped as
// `{ _value: x }`, the Python SDK's shape, and unwrapped on subscribe.
describe('TypedChannel non-object events', () => {
  function recordingBus() {
    const payloads: string[] = [];
    return {
      payloads,
      ingestFire: (p: string) => (payloads.push(p), true),
      ingestRawBatch: (ps: string[]) => (payloads.push(...ps), ps.length),
      poll: async () => ({
        events: payloads.map((raw, i) => ({ id: String(i), raw })),
        nextId: String(payloads.length),
        hasMore: false,
      }),
    };
  }

  it('wraps scalars and arrays as _value, matching Python', () => {
    const bus = recordingBus();
    const ch = new TypedChannel<unknown>(bus as never, 'metrics/x');
    ch.publish(42);
    ch.publish([1, 2]);
    ch.publish({ v: 1 });
    expect(bus.payloads.map((p) => JSON.parse(p))).toEqual([
      { _value: 42, _channel: 'metrics/x' },
      { _value: [1, 2], _channel: 'metrics/x' },
      { v: 1, _channel: 'metrics/x' },
    ]);
  });

  it('hands the subscriber the value back', async () => {
    const bus = recordingBus();
    const ch = new TypedChannel<unknown>(bus as never, 'metrics/x');
    ch.publish(42);
    for await (const ev of ch.subscribe({ pollIntervalMs: 0 })) {
      expect(ev).toBe(42);
      break;
    }
  });
});
