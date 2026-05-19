import { describe, expect, it } from 'vitest';

// eslint-disable-next-line @typescript-eslint/no-require-imports
const napi = require('@net-mesh/core') as { DeckClient?: { new?: unknown } };
import { DeckClient } from '../src/deck';

// The napi binding exposes `DeckClient.new` only when the
// compiled `.node` file is in sync with the SDK's `index.d.ts`
// surface. Local checkouts that haven't run `npm run build` in
// the binding's directory will hit a missing-symbol error. In
// CI the binding is rebuilt before this suite runs.
//
// We split into two tests so the static (no-binding-required)
// hook-shape check stays exercised even on a stale local build.

describe('DeckClient — async-dispose hook', () => {
  it('Symbol.asyncDispose is declared with the correct shape', () => {
    const proto = DeckClient.prototype as unknown as Record<symbol, unknown>;
    const hook = proto[Symbol.asyncDispose];
    expect(typeof hook).toBe('function');
  });

  it('Symbol.asyncDispose drains the owned supervisor at runtime', async ({ skip }) => {
    // Skip cleanly if the local napi binding predates the
    // standalone constructor (`DeckClient.new`). CI rebuilds
    // the binding before running this suite, so the skip path
    // must never fire there — `CI=true` is the standard GH
    // Actions / vitest signal, and we promote the skip to a
    // hard failure when it's set so a feature-flag regression
    // in the napi build can't silently hide.
    if (typeof napi.DeckClient?.new !== 'function') {
      if (process.env.CI) {
        throw new Error(
          'napi binding lacks DeckClient.new in CI — the binding build is misconfigured ' +
            '(check the --features list in .github/workflows/ci.yml includes `deck`).',
        );
      }
      skip('napi binding lacks DeckClient.new — rebuild bindings/node');
      return;
    }
    const client = await DeckClient.new(Buffer.alloc(32, 0x5a));
    // Calling the hook directly rather than via `await using`
    // keeps this test portable to the ES2020 vitest target. The
    // hook implementation is the contract we care about.
    await client[Symbol.asyncDispose]();
    // Second dispose is idempotent: close() short-circuits once
    // the SDK is drained.
    await client[Symbol.asyncDispose]();
  });
});
