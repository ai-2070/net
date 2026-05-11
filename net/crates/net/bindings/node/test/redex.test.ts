// Smoke tests for the raw RedEX file surface exposed on Redex.

import { describe, expect, it } from 'vitest';

import { Redex } from '../index';

describe('Redex.openFile', () => {
  it('returns a handle, appends, reads back via readRange', () => {
    const redex = new Redex();
    const file = redex.openFile('test/basic');
    expect(file.len()).toBe(0n);

    const seq = file.append(Buffer.from('hello'));
    expect(seq).toBe(0n);
    expect(file.len()).toBe(1n);

    const events = file.readRange(0n, 10n);
    expect(events).toHaveLength(1);
    expect(events[0].seq).toBe(0n);
    expect(Buffer.from(events[0].payload).toString()).toBe('hello');
  });

  it('append_batch returns the first seq; events land contiguously', () => {
    const redex = new Redex();
    const file = redex.openFile('test/batch');

    const first = file.appendBatch([
      Buffer.from('a'),
      Buffer.from('b'),
      Buffer.from('c'),
    ]);
    // appendBatch returns BigInt | null. Empty input → null;
    // non-empty → BigInt of the first seq.
    expect(first).toBe(0n);
    expect(file.len()).toBe(3n);

    const events = file.readRange(0n, 5n);
    expect(events.map((e) => Buffer.from(e.payload).toString())).toEqual([
      'a',
      'b',
      'c',
    ]);
  });

  it('append_batch returns null on empty input', () => {
    const redex = new Redex();
    const file = redex.openFile('test/batch-empty');

    const first = file.appendBatch([]);
    expect(first).toBeNull();
    expect(file.len()).toBe(0n);
  });

  it('repeat openFile with the same name returns the live handle', () => {
    const redex = new Redex();
    const a = redex.openFile('test/shared');
    a.append(Buffer.from('from-a'));

    const b = redex.openFile('test/shared');
    expect(b.len()).toBe(1n);
    const [event] = b.readRange(0n, 1n);
    expect(Buffer.from(event.payload).toString()).toBe('from-a');
  });

  it('invalid channel name raises a redex: error', () => {
    const redex = new Redex();
    expect(() => redex.openFile('bad name with spaces')).toThrow(/redex:/);
  });

  it('mutually exclusive fsync options are rejected', () => {
    const redex = new Redex();
    expect(() =>
      redex.openFile('test/bad-fsync', {
        persistent: false,
        fsyncEveryN: 10n,
        fsyncIntervalMs: 100,
      }),
    ).toThrow(/mutually exclusive/);
  });
});

describe('RedexTailIter', () => {
  it('backfills retained events and streams live appends', async () => {
    const redex = new Redex();
    const file = redex.openFile('test/tail');
    file.append(Buffer.from('early-1'));
    file.append(Buffer.from('early-2'));

    const iter = await file.tail(0n);

    const backfill1 = await iter.next();
    const backfill2 = await iter.next();
    expect(backfill1).not.toBeNull();
    expect(backfill2).not.toBeNull();
    expect(Buffer.from(backfill1!.payload).toString()).toBe('early-1');
    expect(Buffer.from(backfill2!.payload).toString()).toBe('early-2');

    // Append live and pick it up.
    file.append(Buffer.from('live'));
    const live = await iter.next();
    expect(live).not.toBeNull();
    expect(Buffer.from(live!.payload).toString()).toBe('live');

    iter.close();
    const afterClose = await iter.next();
    expect(afterClose).toBeNull();
  });

  it('close() on the file ends the tail iter cleanly', async () => {
    const redex = new Redex();
    const file = redex.openFile('test/closing');
    file.append(Buffer.from('x'));
    const iter = await file.tail(0n);
    const first = await iter.next();
    expect(first).not.toBeNull();

    await file.close();
    // The next `.next()` either resolves null (stream ended cleanly
    // via the Closed error mapping) or unblocks promptly; we bound
    // it so a regression that hangs the tail is visible.
    const afterClose = await Promise.race([
      iter.next(),
      new Promise<'timeout'>((r) => setTimeout(() => r('timeout'), 500)),
    ]);
    expect(afterClose).toBe(null);
  });

  it('tail(fromSeq) skips events before fromSeq', async () => {
    const redex = new Redex();
    const file = redex.openFile('test/from-seq');
    for (let i = 0; i < 5; i++) {
      file.append(Buffer.from(`e${i}`));
    }

    const iter = await file.tail(3n);
    const ev = await iter.next();
    expect(ev).not.toBeNull();
    expect(ev!.seq).toBe(3n);
    iter.close();
  });
});
