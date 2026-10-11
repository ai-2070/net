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
