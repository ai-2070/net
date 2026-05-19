// CR-3: regression tests for the `RedisStreamDedup` re-export from
// `@net-mesh/sdk`. Pre-fix the class was registered in the NAPI
// module (`@net-mesh/core`) but `sdk-ts/src/index.ts` never re-exported
// it, so users following the README's `import { RedisStreamDedup }
// from '@net-mesh/sdk'` pattern hit a runtime undefined.
//
// These tests are tiny smoke tests — the underlying LRU semantics
// are pinned in the Rust unit tests (`adapter::redis_dedup::tests`)
// and the NAPI smoke tests (`bindings/node/src/redis_dedup.rs`).
// We only need to confirm the SDK package surfaces the symbol and
// the constructor / method shape matches the README.

import { describe, expect, it } from 'vitest';

import { RedisStreamDedup } from '../src';

describe('RedisStreamDedup re-export (CR-3)', () => {
  it('is exported as a constructable class from @net-mesh/sdk', () => {
    expect(typeof RedisStreamDedup).toBe('function');
  });

  it('default-constructed instance reports capacity 4096', () => {
    const dedup = new RedisStreamDedup();
    expect(dedup.capacity).toBe(4096);
    expect(dedup.len).toBe(0);
    expect(dedup.isEmpty).toBe(true);
  });

  it('explicit capacity is honored', () => {
    const dedup = new RedisStreamDedup(64);
    expect(dedup.capacity).toBe(64);
  });

  it('first observation is not a duplicate; repeat is', () => {
    const dedup = new RedisStreamDedup(64);
    expect(dedup.isDuplicate('abc:0:0:0')).toBe(false);
    expect(dedup.isDuplicate('abc:0:0:0')).toBe(true);
    expect(dedup.len).toBe(1);
  });

  it('clear() resets state', () => {
    const dedup = new RedisStreamDedup(64);
    dedup.isDuplicate('a');
    dedup.isDuplicate('b');
    expect(dedup.len).toBe(2);
    dedup.clear();
    expect(dedup.len).toBe(0);
    expect(dedup.isEmpty).toBe(true);
    // Post-clear: re-observation looks new.
    expect(dedup.isDuplicate('a')).toBe(false);
  });
});
