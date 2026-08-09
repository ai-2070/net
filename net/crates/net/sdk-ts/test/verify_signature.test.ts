// Sign then verify, in one round trip, through the TS SDK.
//
// The identity tests assert that `sign()` returns 64 bytes, which
// passes for any 64 bytes — including 64 zeros. Nothing checked that a
// signature produced here actually verifies, because this SDK exposed
// signing and no way to check the result: `verifySignature` closed that
// for C, Go, Node and Python, and the wrapper was the last surface
// without it.

import { describe, expect, it } from 'vitest';

import { Identity, IdentityError, verifySignature } from '../src/identity';

describe('verifySignature', () => {
  it('accepts a signature the same identity produced', () => {
    const id = Identity.generate();
    const message = Buffer.from('the exact bytes that were signed');
    const sig = id.sign(message);

    expect(verifySignature(id.entityId, message, sig)).toBe(true);
    // And through the convenience method on the identity itself.
    expect(id.verify(message, sig)).toBe(true);
  });

  it('rejects another message, another key, and a tampered signature', () => {
    const id = Identity.generate();
    const other = Identity.generate();
    const message = Buffer.from('payload');
    const sig = id.sign(message);

    expect(verifySignature(id.entityId, Buffer.from('different'), sig)).toBe(
      false,
    );
    expect(verifySignature(other.entityId, message, sig)).toBe(false);

    const tampered = Buffer.from(sig);
    tampered[0] ^= 0xff;
    expect(verifySignature(id.entityId, message, tampered)).toBe(false);
  });

  it('rejects the all-zero signature a length check accepts', () => {
    const id = Identity.generate();
    const message = Buffer.from('payload');
    expect(verifySignature(id.entityId, message, Buffer.alloc(64))).toBe(false);
  });

  it('treats an empty message as a message, not a missing argument', () => {
    const id = Identity.generate();
    const empty = Buffer.alloc(0);
    expect(verifySignature(id.entityId, empty, id.sign(empty))).toBe(true);
    // …and does not accept some other message's signature for it.
    expect(
      verifySignature(id.entityId, empty, id.sign(Buffer.from('x'))),
    ).toBe(false);
  });

  // A malformed argument throws; it never returns `false`. A caller
  // that cannot tell the two apart treats its own bug as a failed
  // signature check.
  it('throws on a wrong-length entity id or signature', () => {
    const id = Identity.generate();
    const message = Buffer.from('payload');
    const sig = id.sign(message);

    for (const n of [0, 31, 33]) {
      expect(() => verifySignature(Buffer.alloc(n), message, sig)).toThrow(
        IdentityError,
      );
    }
    for (const n of [0, 63, 65]) {
      expect(() =>
        verifySignature(id.entityId, message, Buffer.alloc(n)),
      ).toThrow(IdentityError);
    }
  });
});
