// Token numeric options must be rejected, not coerced.
//
// The native signature takes `u32` TTL and `u8` delegation depth, and
// napi truncates or wraps to fit. So `ttlSeconds: 1.5` silently became
// 1 second, `2 ** 32` became 0 — which then failed as `zero_ttl`,
// blaming a value the caller never supplied — and
// `delegationDepth: 1.5` became 1. A credential's validity window is
// not a value to round on the caller's behalf.
//
// These run against the SDK's validation layer, before NAPI, so they
// need no native module.

import { describe, expect, it } from 'vitest';

import { Identity, IdentityError, TokenError } from '../src/identity';

// The validation under test runs before any native call, so a stub
// inner is enough to construct the wrapper.
function identityWithStub(): Identity {
  const stub = {
    issueToken() {
      throw new Error('native issueToken must not be reached');
    },
  };
  const id = Object.create(Identity.prototype) as Identity;
  (id as unknown as { inner: unknown }).inner = stub;
  return id;
}

const SUBJECT = Buffer.alloc(32, 1);

function issue(ttlSeconds: number, delegationDepth?: number) {
  return identityWithStub().issueToken({
    subject: SUBJECT,
    scope: ['publish'],
    channel: 'sensors/temp',
    ttlSeconds,
    delegationDepth,
  });
}

describe('issueToken numeric validation', () => {
  it('rejects a fractional ttl instead of truncating it', () => {
    // Was silently 1 second.
    expect(() => issue(1.5)).toThrow(IdentityError);
    expect(() => issue(1.5)).toThrow(/safe integer/);
  });

  it('rejects a ttl past u32 instead of wrapping to zero', () => {
    // Was 0, then surfaced as `zero_ttl` — the wrong diagnosis.
    expect(() => issue(2 ** 32)).toThrow(/in 1\.\.=4294967295/);
    expect(() => issue(2 ** 53)).toThrow(IdentityError);
  });

  it('rejects a zero or negative ttl at the SDK boundary', () => {
    expect(() => issue(0)).toThrow(/in 1\.\.=4294967295/);
    expect(() => issue(-1)).toThrow(/in 1\.\.=4294967295/);
  });

  it('rejects non-finite ttl', () => {
    expect(() => issue(NaN)).toThrow(/finite/);
    expect(() => issue(Infinity)).toThrow(/finite/);
  });

  it('rejects a fractional or out-of-range delegation depth', () => {
    expect(() => issue(60, 1.5)).toThrow(/safe integer/);
    expect(() => issue(60, 256)).toThrow(/in 0\.\.=255/);
    expect(() => issue(60, -1)).toThrow(/in 0\.\.=255/);
  });

  it('names the offending option', () => {
    expect(() => issue(1.5)).toThrow(/ttlSeconds/);
    expect(() => issue(60, 1.5)).toThrow(/delegationDepth/);
  });

  it('lets valid values through to the native call', () => {
    // Reaching the stub proves validation passed rather than silently
    // rewriting the input.
    expect(() => issue(60, 0)).toThrow(/native issueToken must not be reached/);
    expect(() => issue(0xffff_ffff, 255)).toThrow(
      /native issueToken must not be reached/
    );
  });
});

describe('TokenErrorKind covers the native inventory', () => {
  // Each of these is emitted by the binding as `token: <kind>` and used
  // to be remapped to `invalid_format`, so a caller could not tell a
  // revoked credential from malformed bytes.
  it.each(['revoked', 'read_only', 'zero_ttl', 'ttl_too_long'])(
    'preserves the %s kind',
    (kind) => {
      const err = new TokenError(kind as never, `token: ${kind}`);
      expect(err.kind).toBe(kind);
    }
  );
});
