import { describe, expect, it } from 'vitest';

import { DEFAULT_IDENTITY_KEY, rememberedIdentity } from '../src/index.js';

function memoryStorage() {
  const map = new Map<string, string>();
  return {
    map,
    getItem: (key: string) => map.get(key) ?? null,
    setItem: (key: string, value: string) => {
      map.set(key, value);
    },
  };
}

describe('rememberedIdentity', () => {
  it('creates the secrets once and returns the same player on every later visit', () => {
    const storage = memoryStorage();
    const first = rememberedIdentity(undefined, storage);
    expect(first.entitySecretHex).toMatch(/^[0-9a-f]{64}$/);
    expect(first.noiseSecretHex).toMatch(/^[0-9a-f]{64}$/);
    expect(first.entitySecretHex).not.toBe(first.noiseSecretHex);
    expect(rememberedIdentity(undefined, storage)).toEqual(first);
    expect(storage.map.has(DEFAULT_IDENTITY_KEY)).toBe(true);
  });

  it('keeps separate players under separate keys', () => {
    const storage = memoryStorage();
    expect(rememberedIdentity('a', storage)).not.toEqual(rememberedIdentity('b', storage));
  });

  it('replaces a damaged record instead of passing it to connect()', () => {
    const storage = memoryStorage();
    storage.setItem(DEFAULT_IDENTITY_KEY, JSON.stringify({ entitySecretHex: 'zz', noiseSecretHex: 1 }));
    const replaced = rememberedIdentity(undefined, storage);
    expect(replaced.entitySecretHex).toMatch(/^[0-9a-f]{64}$/);
    expect(rememberedIdentity(undefined, storage)).toEqual(replaced);
  });

  it('still returns a player when storage refuses — just not a kept one', () => {
    const blocked = {
      getItem: () => {
        throw new Error('SecurityError');
      },
      setItem: () => {
        throw new Error('SecurityError');
      },
    };
    const one = rememberedIdentity(undefined, blocked);
    const two = rememberedIdentity(undefined, blocked);
    expect(one.entitySecretHex).toMatch(/^[0-9a-f]{64}$/);
    expect(one).not.toEqual(two);
  });
});
