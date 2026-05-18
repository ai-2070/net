import { describe, expect, it } from 'vitest';

import {
  Identity,
  IdentityError,
  Token,
  TokenError,
  channelHash,
  delegateToken,
} from '../src/identity';

describe('Identity — round-trip + persistence', () => {
  it('generate() produces distinct entities', () => {
    const a = Identity.generate();
    const b = Identity.generate();
    expect(a.entityId.equals(b.entityId)).toBe(false);
  });

  it('toBytes() round-trips through fromSeed / fromBytes', () => {
    const a = Identity.generate();
    const seed = a.toBytes();
    expect(seed.length).toBe(32);

    const viaSeed = Identity.fromSeed(seed);
    const viaBytes = Identity.fromBytes(seed);

    expect(a.entityId.equals(viaSeed.entityId)).toBe(true);
    expect(a.entityId.equals(viaBytes.entityId)).toBe(true);
    expect(a.nodeId).toBe(viaSeed.nodeId);
    expect(a.originHash).toBe(viaSeed.originHash);
  });

  it('fromSeed rejects wrong length with IdentityError', () => {
    expect(() => Identity.fromSeed(Buffer.alloc(16))).toThrow(IdentityError);
    expect(() => Identity.fromSeed(Buffer.alloc(64))).toThrow(IdentityError);
  });

  it('sign returns a 64-byte signature', () => {
    const id = Identity.generate();
    const sig = id.sign(Buffer.from('hello'));
    expect(sig.length).toBe(64);
  });
});

describe('Token — issue / parse / verify', () => {
  it('issues a token whose fields parse back correctly', () => {
    const pub = Identity.generate();
    const sub = Identity.generate();
    const token = pub.issueToken({
      subject: sub.entityId,
      scope: ['subscribe'],
      channel: 'sensors/temp',
      ttlSeconds: 300,
    });

    // Token wire size: 32 (issuer) + 32 (subject) + 4 (scope) +
    // 8 (channel_hash, widened from 2 → 4 → 8 bytes — canonical
    // ChannelHash) + 4 (issuer_generation) + 8 (not_before) + 8
    // (not_after) + 1 (delegation_depth) + 8 (nonce) + 64
    // (signature) = 169. Matches `PermissionToken::WIRE_SIZE`.
    expect(token.bytes.length).toBe(169);
    expect(token.issuer.equals(pub.entityId)).toBe(true);
    expect(token.subject.equals(sub.entityId)).toBe(true);
    expect(token.channelHash).toBe(channelHash('sensors/temp'));
    expect([...token.scope]).toEqual(['subscribe']);
    expect(token.delegationDepth).toBe(0);
    expect(token.notAfter.getTime()).toBeGreaterThan(token.notBefore.getTime());
  });

  it('verify() is true for a fresh token, false after tamper', () => {
    const pub = Identity.generate();
    const sub = Identity.generate();
    const token = pub.issueToken({
      subject: sub.entityId,
      scope: ['subscribe'],
      channel: 'c',
      ttlSeconds: 60,
    });
    expect(token.verify()).toBe(true);

    // Flip a bit in the signature region (last 64 bytes).
    const tampered = Buffer.from(token.bytes);
    tampered[tampered.length - 1] ^= 0x01;
    const t2 = Token.parse(tampered);
    expect(t2.verify()).toBe(false);
  });

  it('parse rejects malformed bytes with TokenError.kind = invalid_format', () => {
    try {
      Token.parse(Buffer.from([1, 2, 3]));
      expect.fail('expected TokenError');
    } catch (e) {
      expect(e).toBeInstanceOf(TokenError);
      expect((e as TokenError).kind).toBe('invalid_format');
    }
  });

  it('installToken rejects tampered bytes with TokenError.kind = invalid_signature', () => {
    const pub = Identity.generate();
    const sub = Identity.generate();
    const token = pub.issueToken({
      subject: sub.entityId,
      scope: ['subscribe'],
      channel: 'c',
      ttlSeconds: 60,
    });

    const tampered = Buffer.from(token.bytes);
    tampered[tampered.length - 1] ^= 0xff;

    try {
      sub.installToken(tampered);
      expect.fail('expected TokenError');
    } catch (e) {
      expect(e).toBeInstanceOf(TokenError);
      expect((e as TokenError).kind).toBe('invalid_signature');
    }
  });

  it('install then lookup returns the token', () => {
    const pub = Identity.generate();
    const sub = Identity.generate();
    const token = pub.issueToken({
      subject: sub.entityId,
      scope: ['subscribe'],
      channel: 'sensors/temp',
      ttlSeconds: 60,
    });

    expect(sub.tokenCacheLen).toBe(0);
    sub.installToken(token);
    expect(sub.tokenCacheLen).toBe(1);

    const found = sub.lookupToken(sub.entityId, 'sensors/temp');
    expect(found).not.toBeNull();
    expect(found!.bytes.equals(token.bytes)).toBe(true);

    // Unknown channel returns null.
    expect(sub.lookupToken(sub.entityId, 'other')).toBeNull();
  });
});

describe('Token — delegation chain', () => {
  it('A → B (depth 2) → C (depth 1) → D (depth 0), further delegation fails', () => {
    const a = Identity.generate();
    const b = Identity.generate();
    const c = Identity.generate();
    const d = Identity.generate();
    const e = Identity.generate();

    const fromA = a.issueToken({
      subject: b.entityId,
      scope: ['subscribe', 'delegate'],
      channel: 'chan',
      ttlSeconds: 300,
      delegationDepth: 2,
    });
    expect(fromA.delegationDepth).toBe(2);

    const fromB = delegateToken(b, fromA, c.entityId, ['subscribe', 'delegate']);
    expect(fromB.delegationDepth).toBe(1);
    expect(fromB.issuer.equals(b.entityId)).toBe(true);
    expect(fromB.subject.equals(c.entityId)).toBe(true);

    const fromC = delegateToken(c, fromB, d.entityId, ['subscribe', 'delegate']);
    expect(fromC.delegationDepth).toBe(0);

    // D cannot delegate further — depth exhausted.
    try {
      delegateToken(d, fromC, e.entityId, ['subscribe']);
      expect.fail('expected TokenError');
    } catch (err) {
      expect(err).toBeInstanceOf(TokenError);
      expect((err as TokenError).kind).toBe('delegation_exhausted');
    }
  });

  it('non-subject cannot delegate', () => {
    const a = Identity.generate();
    const b = Identity.generate();
    const impersonator = Identity.generate();

    const token = a.issueToken({
      subject: b.entityId,
      scope: ['subscribe', 'delegate'],
      channel: 'chan',
      ttlSeconds: 300,
      delegationDepth: 1,
    });

    try {
      delegateToken(impersonator, token, Identity.generate().entityId, ['subscribe']);
      expect.fail('expected TokenError');
    } catch (err) {
      expect(err).toBeInstanceOf(TokenError);
      expect((err as TokenError).kind).toBe('not_authorized');
    }
  });

  it('token without delegate scope cannot be re-delegated', () => {
    const a = Identity.generate();
    const b = Identity.generate();
    const c = Identity.generate();

    const token = a.issueToken({
      subject: b.entityId,
      scope: ['subscribe'], // no delegate!
      channel: 'chan',
      ttlSeconds: 300,
      delegationDepth: 3,
    });

    try {
      delegateToken(b, token, c.entityId, ['subscribe']);
      expect.fail('expected TokenError');
    } catch (err) {
      expect(err).toBeInstanceOf(TokenError);
      expect((err as TokenError).kind).toBe('delegation_not_allowed');
    }
  });
});

describe('channelHash helper', () => {
  it('matches the token-side channel_hash', () => {
    const pub = Identity.generate();
    const sub = Identity.generate();
    const token = pub.issueToken({
      subject: sub.entityId,
      scope: ['publish'],
      channel: 'events/deployments',
      ttlSeconds: 60,
    });
    expect(token.channelHash).toBe(channelHash('events/deployments'));
  });

  it('rejects malformed names with IdentityError', () => {
    expect(() => channelHash('has spaces')).toThrow(IdentityError);
  });
});
