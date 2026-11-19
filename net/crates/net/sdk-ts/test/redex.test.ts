// Integration tests for the RedEX SDK wrapper in sdk-ts/src/cortex.ts.
//
// Exercises the AsyncIterable-wrapped tail iterator, the typed
// RedexError, and config-translation edge cases.

import { describe, expect, it } from 'vitest';

import { Redex, RedexError, type RedexEvent } from '../src/cortex';

describe('Redex.openFile', () => {
  it('appends and reads back', () => {
    const redex = new Redex();
    const file = redex.openFile('sdk/basic');
    const seq = file.append(Buffer.from('hello'));
    expect(seq).toBe(0n);
    expect(file.len()).toBe(1n);

    const events = file.readRange(0n, 10n);
    expect(events).toHaveLength(1);
    expect(events[0].seq).toBe(0n);
    expect(events[0].payload.toString()).toBe('hello');
    expect(events[0].isInline).toBe(false);
  });

  it('appendBatch returns first seq', () => {
    const redex = new Redex();
    const file = redex.openFile('sdk/batch');
    const first = file.appendBatch([
      Buffer.from('a'),
      Buffer.from('b'),
      Buffer.from('c'),
    ]);
    expect(first).toBe(0n);
    expect(file.len()).toBe(3n);
  });

  it('throws RedexError for invalid channel names', () => {
    const redex = new Redex();
    expect(() => redex.openFile('bad name')).toThrowError(RedexError);
  });

  it('throws RedexError for mutually-exclusive fsync options', () => {
    const redex = new Redex();
    expect(() =>
      redex.openFile('sdk/bad-fsync', {
        fsyncEveryN: 10n,
        fsyncIntervalMs: 100,
      }),
    ).toThrowError(RedexError);
  });

  it('translates retention options correctly', () => {
    const redex = new Redex();
    // Any non-error call exercises the toNapiFileConfig path for each
    // retention field; smoke check that nothing blows up.
    const file = redex.openFile('sdk/retention', {
      retentionMaxEvents: 100n,
      retentionMaxBytes: 1024n * 1024n,
      retentionMaxAgeMs: 60_000n,
    });
    expect(file.len()).toBe(0n);
  });
});

describe('RedexFile.tail — AsyncIterable', () => {
  it('backfills and streams live appends via for-await', async () => {
    const redex = new Redex();
    const file = redex.openFile('sdk/tail');
    file.append(Buffer.from('early'));

    const collected: RedexEvent[] = [];
    const stream = await file.tail(0n);

    const loop = (async () => {
      for await (const event of stream) {
        collected.push(event);
        if (collected.length === 2) break; // return() → close()
      }
    })();

    // Give the backfill a chance.
    await new Promise((r) => setTimeout(r, 30));
    file.append(Buffer.from('live'));

    await loop;
    expect(collected).toHaveLength(2);
    expect(collected[0].payload.toString()).toBe('early');
    expect(collected[1].payload.toString()).toBe('live');
  });

  it('ends cleanly when the file is closed', async () => {
    const redex = new Redex();
    const file = redex.openFile('sdk/close');
    file.append(Buffer.from('x'));
    const stream = await file.tail(0n);

    const iter = stream[Symbol.asyncIterator]();
    const first = await iter.next();
    expect(first.done).toBe(false);

    file.close();
    // Next read must complete (null → done) without hanging.
    const afterClose = await Promise.race([
      iter.next(),
      new Promise((r) => setTimeout(() => r({ value: undefined, done: 'timeout' }), 500)),
    ]);
    expect((afterClose as IteratorResult<RedexEvent>).done).toBe(true);
  });

  it('tail(fromSeq) skips earlier events', async () => {
    const redex = new Redex();
    const file = redex.openFile('sdk/from-seq');
    for (let i = 0; i < 5; i++) {
      file.append(Buffer.from(`e${i}`));
    }
    const stream = await file.tail(3n);
    const iter = stream[Symbol.asyncIterator]();
    const first = await iter.next();
    expect(first.done).toBe(false);
    expect((first.value as RedexEvent).seq).toBe(3n);
    await iter.return!();
  });
});

describe('Redex constructor', () => {
  it('refuses a bare directory string, naming the options object', () => {
    // An untyped caller's `new Redex(dir)` used to build an in-memory
    // store silently and fail much later at a persistent open.
    expect(() => new Redex('/tmp/some-dir' as never)).toThrow(
      /new Redex\(\{ persistentDir: "\/tmp\/some-dir" \}\)/,
    );
    expect(() => new Redex(null as never)).toThrow(/got null/);
  });

  it('still accepts no options and an options object', () => {
    expect(() => new Redex()).not.toThrow();
    expect(() => new Redex({})).not.toThrow();
  });
});
