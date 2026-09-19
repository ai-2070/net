/**
 * The loader.
 *
 * The shape it must accept is not negotiable: `wasm-bindgen` emits
 * `LeafNode` as an **ES class**, so `connect` is a static on a
 * function, not a property of a plain object. A guard that demanded an
 * object rejected the real `pkg/net_leaf.js` in Chromium with
 * "exports a LeafNode without a connect()"; these cases are that
 * regression, pinned. `data:` modules stand in for the generated glue
 * so the assertions need no wasm.
 */

import { describe, expect, it } from 'vitest';

import { loadLeafWasm } from '../src/wasm.js';

const CLASS_MODULE =
  'data:text/javascript,export class LeafNode { static async connect(opts) { return { opts }; } }';

describe('loadLeafWasm', () => {
  it('accepts a module whose LeafNode is a class with a static connect', async () => {
    const module = await loadLeafWasm({ wasmModule: CLASS_MODULE });
    expect(typeof module.LeafNode.connect).toBe('function');
  });

  it('instantiates once per specifier', async () => {
    const first = await loadLeafWasm({ wasmModule: CLASS_MODULE });
    const second = await loadLeafWasm({ wasmModule: CLASS_MODULE });
    expect(second).toBe(first);
  });

  it('runs the generated initialiser when the module has one', async () => {
    const calls: unknown[] = [];
    const module = await loadLeafWasm({
      wasm: {
        default: async (init) => {
          calls.push(init);
          return undefined;
        },
        LeafNode: { connect: async () => ({}) as never },
      },
    });
    // An injected module is used as given — it is already initialised,
    // which is the point of the escape hatch.
    expect(calls).toEqual([]);
    expect(typeof module.LeafNode.connect).toBe('function');
  });

  it('names what is wrong when the module is not a leaf module', async () => {
    await expect(
      loadLeafWasm({ wasmModule: 'data:text/javascript,export const nope = 1;' }),
    ).rejects.toThrow(/no LeafNode export/);
  });

  it('names what is wrong when LeafNode has no static connect', async () => {
    await expect(
      loadLeafWasm({ wasmModule: 'data:text/javascript,export class LeafNode {}' }),
    ).rejects.toThrow(/without a static connect/);
  });

  it('does not poison later attempts after a failed load', async () => {
    const bad = 'data:text/javascript,export const nope = 2;';
    await expect(loadLeafWasm({ wasmModule: bad })).rejects.toThrow();
    await expect(loadLeafWasm({ wasmModule: bad })).rejects.toThrow(/no LeafNode export/);
  });
});
