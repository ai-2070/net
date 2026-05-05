// Integration tests for subnet-based visibility enforcement on
// `MeshNode`. Mirrors `tests/subnet_enforcement.rs` at the SDK layer.
//
// The plan's original three-node example (A=[3,7,2] / B=[3,7,3] /
// C=[3,8,1], "SubnetLocal delivers A↔B") conflated "level N" with
// the actual enum semantics. `Visibility::SubnetLocal` is strict
// same-subnet equality, so these tests use A and B on the exact
// same SubnetId, C on a different one.

import { afterEach, describe, expect, it } from 'vitest';

import { MeshNode, type ChannelConfig } from '../src/mesh';
import type { SubnetPolicy } from '../src/subnets';

const PSK = '42'.repeat(32);

let portSeed = 30_400;
function nextPort(): string {
  const p = portSeed++;
  return `127.0.0.1:${p}`;
}

const sharedPolicy: SubnetPolicy = {
  rules: [
    { tagPrefix: 'region:', level: 0, values: { us: 3 } },
    { tagPrefix: 'fleet:', level: 1, values: { blue: 7, green: 8 } },
    { tagPrefix: 'unit:', level: 2, values: { alpha: 2, beta: 3, gamma: 1 } },
    // Level 3 exists only for the ParentVisible descendant test —
    // nodes that don't carry a `host:` tag derive a 3-level
    // SubnetId (depth=3), nodes that do derive a 4-level
    // descendant. Keeps the other tests untouched since none of
    // them tag `host:*`.
    { tagPrefix: 'host:', level: 3, values: { h1: 5 } },
  ],
};

/**
 * Handshake A↔B without starting either node. Hub topologies
 * (multiple handshakes sharing one initiator) must defer `start()`
 * until *all* handshakes complete — otherwise the receive loop
 * consumes the next handshake's msg2 packet before
 * `handshake_initiator` can read it. Matches the pattern in
 * `tests/three_node_integration.rs`.
 */
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
      // Already shut down, or the test never got that far.
    }
  }
});

// Three-node handshake + two subscribes + publish takes longer
// than vitest's 5s default on slower boxes; bump to 15s.
const LONG_TIMEOUT = 15_000;

describe('Subnet enforcement', () => {
  it('SubnetLocal partitions A/B (same subnet) from C (other)', async () => {
    const sharedSubnet = { levels: [3, 7, 2] };
    const aAddr = nextPort();
    const bAddr = nextPort();
    const cAddr = nextPort();

    const a = await MeshNode.create({
      bindAddr: aAddr,
      psk: PSK,
      subnet: sharedSubnet,
      subnetPolicy: sharedPolicy,
    });
    const b = await MeshNode.create({
      bindAddr: bAddr,
      psk: PSK,
      subnet: sharedSubnet,
      subnetPolicy: sharedPolicy,
    });
    const c = await MeshNode.create({
      bindAddr: cAddr,
      psk: PSK,
      subnet: { levels: [3, 8, 1] },
      subnetPolicy: sharedPolicy,
    });
    nodes.push(a, b, c);

    // Hub topology — A connects to B and C. Defer `start()` on
    // all three until both handshakes complete (see helper doc).
    await handshakeNoStart(a, b, bAddr);
    await handshakeNoStart(a, c, cAddr);
    await startAll(a, b, c);

    await a.announceCapabilities({ tags: ['region:us', 'fleet:blue', 'unit:alpha'] });
    await b.announceCapabilities({ tags: ['region:us', 'fleet:blue', 'unit:alpha'] });
    await c.announceCapabilities({ tags: ['region:us', 'fleet:green', 'unit:gamma'] });

    const bId = b.nodeId();
    const cId = c.nodeId();

    const learned = await waitUntil(() => {
      const bMatches = a.findNodes({ requireTags: ['fleet:blue'] }).includes(bId);
      const cMatches = a.findNodes({ requireTags: ['fleet:green'] }).includes(cId);
      return bMatches && cMatches;
    });
    expect(learned).toBe(true);

    const chanName = 'lab/metrics';
    const chan: ChannelConfig = { name: chanName, visibility: 'subnet-local' };
    a.registerChannel(chan);

    const aId = a.nodeId();

    // B subscribes — same subnet → accepted.
    await expect(b.subscribeChannel(aId, chanName)).resolves.toBeUndefined();

    // C subscribes — A's authorize_subscribe rejects with Unauthorized.
    await expect(c.subscribeChannel(aId, chanName)).rejects.toThrow();

    // A publishes — attempted should be 1 (only B passed both the
    // subscribe gate AND the publish visibility filter).
    const report = await a.publish(chanName, Buffer.from('ok'), {
      reliability: 'fire_and_forget',
      onFailure: 'best_effort',
      maxInflight: 16,
    });
    expect(report.attempted).toBe(1);
    expect(report.delivered).toBe(1);
  }, LONG_TIMEOUT);

  it('ParentVisible admits an ancestor, rejects a sibling at the same depth', async () => {
    // `Visibility::ParentVisible` is **strictly upward** since
    // commit `0a79c368` ("Make ParentVisible strictly upward in
    // gateway and mesh"): the predicate is
    // `dest.is_ancestor_of(source)`. The channel owner is the
    // *source* (publisher); a subscriber is admitted only if
    // its subnet is an ancestor of the channel owner's subnet.
    // A descendant of the channel owner is now REJECTED — that
    // would be downward leakage, the exact thing the tightening
    // prevents. So this test pins the post-fix behavior:
    // ancestor admitted, sibling rejected.
    const aAddr = nextPort();
    const ancAddr = nextPort();
    const sibAddr = nextPort();

    const a = await MeshNode.create({
      bindAddr: aAddr,
      psk: PSK,
      subnet: { levels: [3, 7, 2] },
      subnetPolicy: sharedPolicy,
    });
    const ancestor = await MeshNode.create({
      bindAddr: ancAddr,
      psk: PSK,
      // Ancestor lives one level shallower than A. Its subnet
      // [3, 7] is a strict prefix of A's [3, 7, 2], so
      // `ancestor.is_ancestor_of(A)` holds.
      subnet: { levels: [3, 7] },
      subnetPolicy: sharedPolicy,
    });
    const sibling = await MeshNode.create({
      bindAddr: sibAddr,
      psk: PSK,
      // Sibling lives at the same depth as A but with a different
      // last level — same parent subnet, different leaf. NOT an
      // ancestor of A.
      subnet: { levels: [3, 7, 3] },
      subnetPolicy: sharedPolicy,
    });
    nodes.push(a, ancestor, sibling);

    await handshakeNoStart(a, ancestor, ancAddr);
    await handshakeNoStart(a, sibling, sibAddr);
    await startAll(a, ancestor, sibling);

    // Subnet derivation on A's side (used by the auth check):
    //
    //   a's tags        = [region:us, fleet:blue, unit:alpha]   → [3,7,2]
    //   ancestor's tags = [region:us, fleet:blue]               → [3,7]
    //   sibling's tags  = [region:us, fleet:blue, unit:beta]    → [3,7,3]
    //
    // ancestor has only the first two policy-recognized tags, so
    // its derived SubnetId is depth 2 — a strict prefix of A's.
    // sibling has unit:beta (level 2 = 3), giving [3,7,3] at the
    // same depth as A but with a different last level.
    //
    // The `role:*` tags below aren't in the policy and so don't
    // affect derivation; they're just unique identifiers for
    // `findNodes` to wait on each peer being learned.
    await a.announceCapabilities({
      tags: ['region:us', 'fleet:blue', 'unit:alpha', 'role:owner'],
    });
    await ancestor.announceCapabilities({
      tags: ['region:us', 'fleet:blue', 'role:ancestor'],
    });
    await sibling.announceCapabilities({
      tags: ['region:us', 'fleet:blue', 'unit:beta', 'role:sibling'],
    });

    const learned = await waitUntil(() => {
      const ancMatches = a
        .findNodes({ requireTags: ['role:ancestor'] })
        .includes(ancestor.nodeId());
      const sibMatches = a
        .findNodes({ requireTags: ['role:sibling'] })
        .includes(sibling.nodeId());
      return ancMatches && sibMatches;
    });
    expect(learned).toBe(true);

    const chanName = 'lab/parent';
    a.registerChannel({ name: chanName, visibility: 'parent-visible' });

    const aId = a.nodeId();
    // Strict-upward predicate: `dest.is_ancestor_of(source)`.
    //   ancestor=[3,7].is_ancestor_of(A=[3,7,2]) → true  → admitted.
    //   sibling=[3,7,3].is_ancestor_of(A=[3,7,2]) → false → rejected.
    await expect(ancestor.subscribeChannel(aId, chanName)).resolves.toBeUndefined();
    await expect(sibling.subscribeChannel(aId, chanName)).rejects.toThrow();

    const report = await a.publish(chanName, Buffer.from('pv'), {
      reliability: 'fire_and_forget',
      onFailure: 'best_effort',
      maxInflight: 16,
    });
    expect(report.attempted).toBe(1);
    expect(report.delivered).toBe(1);
  }, LONG_TIMEOUT);

  it('without a policy, SubnetLocal delivers (both default to GLOBAL)', async () => {
    const aAddr = nextPort();
    const bAddr = nextPort();
    // No subnet, no policy — default GLOBAL on both sides.
    const a = await MeshNode.create({ bindAddr: aAddr, psk: PSK });
    const b = await MeshNode.create({ bindAddr: bAddr, psk: PSK });
    nodes.push(a, b);
    await handshakeNoStart(a, b, bAddr);
    await startAll(a, b);

    const chanName = 'lab/global';
    a.registerChannel({ name: chanName, visibility: 'subnet-local' });

    // Same-GLOBAL satisfies SubnetLocal → subscribe accepted.
    await expect(b.subscribeChannel(a.nodeId(), chanName)).resolves.toBeUndefined();

    const report = await a.publish(chanName, Buffer.from('hi'), {
      reliability: 'fire_and_forget',
      onFailure: 'best_effort',
      maxInflight: 16,
    });
    expect(report.attempted).toBe(1);
    expect(report.delivered).toBe(1);
  });

  it('rejects malformed SubnetId in config', async () => {
    // 5 levels — core enforces max 4.
    await expect(
      MeshNode.create({
        bindAddr: nextPort(),
        psk: PSK,
        subnet: { levels: [1, 2, 3, 4, 5] },
      }),
    ).rejects.toThrow(/subnet:/);

    // Out-of-range byte.
    await expect(
      MeshNode.create({
        bindAddr: nextPort(),
        psk: PSK,
        subnet: { levels: [300] },
      }),
    ).rejects.toThrow(/subnet:/);
  });
});
