// `_channel` is routing metadata, not part of the caller's event type.
//
// `TypedChannel.publish()` stamps `_channel` onto every payload so
// subscribers can filter on it. TypeScript used to hand the whole
// parsed object — tag included — to the validator or cast it straight
// to `T`, so a strict runtime validator that rejects unknown properties
// rejected every event on the channel, and a plain `subscribe<T>()`
// returned an object with an undeclared extra key. Python already
// stripped it. These tests pin the parity.

import { describe, expect, it } from 'vitest';

import type { Net as NapiNet } from '@net-mesh/core';
import { CHANNEL_TAG_KEY, TypedChannel } from '../src/channel';

interface Reading {
  sensorId: string;
  celsius: number;
}

/** Records ingested payloads and replays them from `poll()`. */
function recordingBus(): NapiNet & { ingested: string[] } {
  const ingested: string[] = [];
  let served = 0;
  const bus = {
    ingested,
    ingestFire(raw: string): boolean {
      ingested.push(raw);
      return true;
    },
    ingestBatchFire(raws: string[]): number {
      ingested.push(...raws);
      return raws.length;
    },
    async poll() {
      const events = ingested.slice(served).map((raw, i) => ({
        id: String(served + i),
        raw,
        insertionTs: 0,
        shardId: 0,
      }));
      served = ingested.length;
      return { events, nextId: String(served) };
    },
  };
  return bus as unknown as NapiNet & { ingested: string[] };
}

/** Consume at most `n` events, then stop the stream. */
async function take<T>(stream: AsyncIterable<T>, n: number): Promise<T[]> {
  const out: T[] = [];
  for await (const item of stream) {
    out.push(item);
    if (out.length >= n) break;
  }
  return out;
}

describe('_channel tag handling', () => {
  it('stamps the tag on the wire', () => {
    const bus = recordingBus();
    const ch = new TypedChannel<Reading>(bus, 'sensors/temperature');
    ch.publish({ sensorId: 'a1', celsius: 22.5 });

    expect(JSON.parse(bus.ingested[0])).toEqual({
      sensorId: 'a1',
      celsius: 22.5,
      [CHANNEL_TAG_KEY]: 'sensors/temperature',
    });
  });

  it('strips the tag before the default cast', async () => {
    const bus = recordingBus();
    const ch = new TypedChannel<Reading>(bus, 'sensors/temperature');
    ch.publish({ sensorId: 'a1', celsius: 22.5 });

    const [reading] = await take(ch.subscribe(), 1);
    expect(reading).toEqual({ sensorId: 'a1', celsius: 22.5 });
    expect(CHANNEL_TAG_KEY in (reading as object)).toBe(false);
  });

  it('strips the tag before the validator sees it', async () => {
    const bus = recordingBus();
    const seen: unknown[] = [];
    const ch = new TypedChannel<Reading>(bus, 'sensors/temperature', (data) => {
      seen.push(data);
      return data as Reading;
    });
    ch.publish({ sensorId: 'a1', celsius: 22.5 });

    await take(ch.subscribe(), 1);
    expect(seen).toEqual([{ sensorId: 'a1', celsius: 22.5 }]);
  });

  it('lets a strict validator accept channel events', async () => {
    const bus = recordingBus();
    // A validator that rejects unknown properties — the shape Zod's
    // `.strict()` and equivalents produce.
    const strict = (data: unknown): Reading => {
      const allowed = new Set(['sensorId', 'celsius']);
      for (const key of Object.keys(data as object)) {
        if (!allowed.has(key)) {
          throw new Error(`unrecognized key: ${key}`);
        }
      }
      return data as Reading;
    };
    const ch = new TypedChannel<Reading>(bus, 'sensors/temperature', strict);
    ch.publish({ sensorId: 'a1', celsius: 22.5 });

    await expect(take(ch.subscribe(), 1)).resolves.toEqual([
      { sensorId: 'a1', celsius: 22.5 },
    ]);
  });

  it('strips the tag on batch-published events too', async () => {
    const bus = recordingBus();
    const ch = new TypedChannel<Reading>(bus, 'sensors/temperature');
    ch.publishBatch([
      { sensorId: 'a1', celsius: 1 },
      { sensorId: 'a2', celsius: 2 },
    ]);

    const got = await take(ch.subscribe(), 2);
    expect(got).toEqual([
      { sensorId: 'a1', celsius: 1 },
      { sensorId: 'a2', celsius: 2 },
    ]);
  });

  it('keeps the tag on subscribeRaw — that surface is the escape hatch', async () => {
    const bus = recordingBus();
    const ch = new TypedChannel<Reading>(bus, 'sensors/temperature');
    ch.publish({ sensorId: 'a1', celsius: 22.5 });

    const [event] = await take(ch.subscribeRaw(), 1);
    expect(JSON.parse(event.raw)[CHANNEL_TAG_KEY]).toBe('sensors/temperature');
  });

  it('does not mangle a non-object payload', async () => {
    const bus = recordingBus();
    const ch = new TypedChannel<number>(bus, 'sensors/temperature');
    // A raw `ingest` of a JSON scalar can land in a channel-filtered
    // stream; stripping must be a no-op rather than a crash.
    bus.ingestFire('42');

    const [value] = await take(ch.subscribe(), 1);
    expect(value).toBe(42);
  });
});
