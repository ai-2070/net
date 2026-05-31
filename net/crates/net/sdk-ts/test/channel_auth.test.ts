// Integration tests for channel authentication enforcement
// on `MeshNode`. Mirrors `tests/channel_auth.rs` at the SDK layer —
// cap-filter denial, token-denied, token round-trip, publish-
// denied, plus the unauthenticated-channel backward-compat path.

import { afterEach, describe, expect, it } from 'vitest';

import { Identity, TokenScope } from '../src/identity';
import { MeshNode, type ChannelConfig } from '../src/mesh';
import { randomBytes } from 'node:crypto';

const PSK = '42'.repeat(32);

let portSeed = 31_400;
function nextPort(): string {
  return `127.0.0.1:${portSeed++}`;
}

/** 32-byte random seed for a reproducible mesh identity. */
function newSeed(): Buffer {
  return randomBytes(32);
}

async function handshakeNoStart(a: MeshNode, b: MeshNode, bAddr: string): Promise<void> {
  const bPub = b.publicKey();
  const aId = a.nodeId();
  const bId = b.nodeId();
  await Promise.all([
    b.accept(aId),
    (async () => {
      await new Promise((r) => setTimeout(r, 50));
      await a.connect(bAddr, bPub, bId);
    })(),
  ]);
}

async function startAll(...nodes: MeshNode[]): Promise<void> {
  await Promise.all(nodes.map((n) => n.start()));
}

async function waitUntil(fn: () => boolean, timeoutMs = 2_000): Promise<boolean> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (fn()) return true;
    await new Promise((r) => setTimeout(r, 25));
  }
  return fn();
}

const nodes: MeshNode[] = [];
afterEach(async () => {
  while (nodes.length > 0) {
    const n = nodes.pop()!;
    try {
      await n.shutdown();
    } catch {
      // Ignore — may have already been shut down by the test.
    }
  }
});

describe('Channel authentication', () => {
  it('subscribe_denied_by_cap_filter', async () => {
    const aAddr = nextPort();
    const bAddr = nextPort();
    const a = await MeshNode.create({ bindAddr: aAddr, psk: PSK });
    const b = await MeshNode.create({ bindAddr: bAddr, psk: PSK });
    nodes.push(a, b);

    await handshakeNoStart(a, b, bAddr);
    await startAll(a, b);
    // B announces empty caps; A's channel requires a "gpu" tag.
    await a.announceCapabilities({});
    await b.announceCapabilities({});

    const chan: ChannelConfig = {
      name: 'lab/gpu-only',
      subscribeCaps: { requireTags: ['gpu'] },
    };
    a.registerChannel(chan);

    // Wait until A has indexed B (proxy for peer_entity_ids
    // populated — required for the cap-filter check to consult
    // B's caps, even though in this case there are no caps to
    // match and the check fails for that reason).
    const bId = b.nodeId();
    await waitUntil(() => a.findNodes({}).includes(bId));

    await expect(b.subscribeChannel(a.nodeId(), chan.name)).rejects.toThrow();
  });

  it('subscribe_denied_by_missing_token', async () => {
    const aAddr = nextPort();
    const bAddr = nextPort();
    const a = await MeshNode.create({ bindAddr: aAddr, psk: PSK });
    const b = await MeshNode.create({ bindAddr: bAddr, psk: PSK });
    nodes.push(a, b);
    await handshakeNoStart(a, b, bAddr);
    await startAll(a, b);
    await a.announceCapabilities({});
    await b.announceCapabilities({});

    const chan: ChannelConfig = {
      name: 'lab/secret-no-token',
      tokenRoots: [a.entityId()],
    };
    a.registerChannel(chan);

    // B subscribes with no token — reject.
    await expect(b.subscribeChannel(a.nodeId(), chan.name)).rejects.toThrow();
  });

  it('subscribe_accepted_with_valid_token', async () => {
    // Both meshes use caller-supplied seeds so tokens can be
    // issued from corresponding `Identity` instances.
    const aSeed = newSeed();
    const bSeed = newSeed();
    const aIdentity = Identity.fromSeed(aSeed);
    const bIdentity = Identity.fromSeed(bSeed);

    const aAddr = nextPort();
    const bAddr = nextPort();
    const a = await MeshNode.create({ bindAddr: aAddr, psk: PSK, identitySeed: aSeed });
    const b = await MeshNode.create({ bindAddr: bAddr, psk: PSK, identitySeed: bSeed });
    nodes.push(a, b);

    // Mesh's entity_id must match the identity constructed from
    // the same seed.
    expect(a.entityId().equals(aIdentity.entityId)).toBe(true);
    expect(b.entityId().equals(bIdentity.entityId)).toBe(true);

    await handshakeNoStart(a, b, bAddr);
    await startAll(a, b);
    await a.announceCapabilities({});
    await b.announceCapabilities({});

    const chan: ChannelConfig = {
      name: 'lab/token-gated',
      // A (the publisher / token issuer) is the channel's root of
      // trust; the single-token chain B presents must root at it.
      tokenRoots: [a.entityId()],
    };
    a.registerChannel(chan);

    // Wait until A has indexed B — the publisher's
    // `authorize_subscribe` looks up the subscriber's entity_id in
    // `peer_entity_ids` BEFORE comparing it against the token
    // subject. If the announcement hasn't landed yet, A rejects
    // with `Unauthorized` for the wrong reason (missing entity,
    // not bad token) and the test flakes positive → negative
    // under load. Flagged by cubic as a race.
    const bId = b.nodeId();
    await waitUntil(() => a.findNodes({}).includes(bId));

    // Publisher issues a SUBSCRIBE-scoped token for B.
    const token = aIdentity.issueToken({
      subject: bIdentity.entityId,
      scope: ['subscribe'],
      channel: chan.name,
      ttlSeconds: 300,
    });

    await expect(
      b.subscribeChannel(a.nodeId(), chan.name, { token }),
    ).resolves.toBeUndefined();
  });

  it('subscribe_rejected_with_wrong_subject_token', async () => {
    const aSeed = newSeed();
    const bSeed = newSeed();
    const aIdentity = Identity.fromSeed(aSeed);

    const aAddr = nextPort();
    const bAddr = nextPort();
    const a = await MeshNode.create({ bindAddr: aAddr, psk: PSK, identitySeed: aSeed });
    const b = await MeshNode.create({ bindAddr: bAddr, psk: PSK, identitySeed: bSeed });
    nodes.push(a, b);
    await handshakeNoStart(a, b, bAddr);
    await startAll(a, b);
    await a.announceCapabilities({});
    await b.announceCapabilities({});

    const chan: ChannelConfig = { name: 'lab/wrong-subject', tokenRoots: [a.entityId()] };
    a.registerChannel(chan);

    // Same race as `subscribe_accepted_with_valid_token`: if A
    // hasn't indexed B's announcement yet, the rejection fires on
    // the "entity unknown" branch, not the "token subject
    // mismatch" branch we want to exercise.
    const bId = b.nodeId();
    await waitUntil(() => a.findNodes({}).includes(bId));

    // Token issued for a third, unrelated entity — B attempts to use
    // it anyway. The chain roots at A (a valid root) but its leaf
    // subject is the bystander, not B (the presenter), so the
    // leaf-binding check rejects it.
    const bystander = Identity.generate();
    const token = aIdentity.issueToken({
      subject: bystander.entityId,
      scope: ['subscribe'],
      channel: chan.name,
      ttlSeconds: 300,
    });

    await expect(
      b.subscribeChannel(a.nodeId(), chan.name, { token }),
    ).rejects.toThrow();
  });

  it('publish_denied_by_own_cap_filter', async () => {
    const aAddr = nextPort();
    const bAddr = nextPort();
    const a = await MeshNode.create({ bindAddr: aAddr, psk: PSK });
    const b = await MeshNode.create({ bindAddr: bAddr, psk: PSK });
    nodes.push(a, b);
    await handshakeNoStart(a, b, bAddr);
    await startAll(a, b);
    await a.announceCapabilities({}); // A has no tags
    await b.announceCapabilities({});

    const chan: ChannelConfig = {
      name: 'lab/admin-only',
      publishCaps: { requireTags: ['admin'] },
    };
    a.registerChannel(chan);

    await expect(
      a.publish(chan.name, Buffer.from('x'), {
        reliability: 'fire_and_forget',
        onFailure: 'best_effort',
        maxInflight: 16,
      }),
    ).rejects.toThrow();
  });

  it('unauth_channel_accepts_everyone', async () => {
    // Regression for the backward-compat default: no caps, no
    // require_token ⇒ channel is open.
    const aAddr = nextPort();
    const bAddr = nextPort();
    const a = await MeshNode.create({ bindAddr: aAddr, psk: PSK });
    const b = await MeshNode.create({ bindAddr: bAddr, psk: PSK });
    nodes.push(a, b);
    await handshakeNoStart(a, b, bAddr);
    await startAll(a, b);

    const chan: ChannelConfig = { name: 'lab/open' };
    a.registerChannel(chan);

    await expect(b.subscribeChannel(a.nodeId(), chan.name)).resolves.toBeUndefined();
    const report = await a.publish(chan.name, Buffer.from('hi'), {
      reliability: 'fire_and_forget',
      onFailure: 'best_effort',
      maxInflight: 16,
    });
    expect(report.attempted).toBe(1);
    expect(report.delivered).toBe(1);
  });
});
