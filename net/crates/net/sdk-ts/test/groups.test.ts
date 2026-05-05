// Stage 2 tests for the groups surface — ReplicaGroup, ForkGroup,
// StandbyGroup. Mirrors the Rust SDK's `groups_surface.rs`.

import { afterEach, describe, expect, it } from 'vitest';

import {
  DaemonError,
  DaemonRuntime,
  ForkGroup,
  GroupError,
  Identity,
  MeshNode,
  ReplicaGroup,
  StandbyGroup,
  type MeshDaemon,
} from '../src';

const PSK = '42'.repeat(32);

// Start high to avoid collisions with other test files in the
// suite. Each port is used exactly once per vitest run.
let portSeed = 31_500;
function nextPort(): string {
  return `127.0.0.1:${portSeed++}`;
}

async function buildMesh(): Promise<MeshNode> {
  return MeshNode.create({ bindAddr: nextPort(), psk: PSK });
}

class NoopDaemon implements MeshDaemon {
  readonly name = 'noop';
  process(): Buffer[] {
    return [];
  }
}

/** Build a started runtime with the given number of synthetic
 *  peer entries in the capability index, so `place_with_spread`
 *  has enough candidates for ≥ 2 member groups. */
async function runtimeWithPeers(extraPeers: number): Promise<{ rt: DaemonRuntime; mesh: MeshNode }> {
  const mesh = await buildMesh();
  for (let i = 1; i <= extraPeers; i++) {
    // Synthetic node IDs above 0x1000_0000 so they never collide
    // with real node IDs derived from ed25519 pubkeys.
    mesh._testInjectSyntheticPeer(BigInt(0x1000_0000_0000_0000) + BigInt(i));
  }
  const rt = DaemonRuntime.create(mesh);
  rt.registerFactory('noop', () => new NoopDaemon());
  await rt.start();
  return { rt, mesh };
}

const seed = (byte: number): Buffer => Buffer.alloc(32, byte);

describe('ReplicaGroup', () => {
  const cleanups: Array<() => Promise<void>> = [];
  afterEach(async () => {
    while (cleanups.length > 0) {
      const fn = cleanups.pop();
      if (fn) {
        try {
          await fn();
        } catch {
          // Best-effort teardown
        }
      }
    }
  });

  it('spawn registers all members and reports healthy aggregate', async () => {
    const { rt, mesh } = await runtimeWithPeers(3);
    cleanups.push(() => mesh.shutdown());
    cleanups.push(() => rt.shutdown());

    const group = await ReplicaGroup.spawn(rt, 'noop', {
      replicaCount: 3,
      groupSeed: seed(0x11),
      lbStrategy: 'round-robin',
    });

    expect(group.replicaCount).toBe(3);
    expect(group.healthyCount).toBe(3);
    expect(group.health.status).toBe('healthy');
    expect(rt.daemonCount()).toBe(3);
    expect(group.replicas).toHaveLength(3);
    for (const r of group.replicas) {
      expect(r.healthy).toBe(true);
      expect(r.originHash).not.toBe(0n);
    }
  });

  it('routeEvent returns a live member origin', async () => {
    const { rt, mesh } = await runtimeWithPeers(3);
    cleanups.push(() => mesh.shutdown());
    cleanups.push(() => rt.shutdown());

    const group = await ReplicaGroup.spawn(rt, 'noop', {
      replicaCount: 3,
      groupSeed: seed(0x22),
      lbStrategy: 'consistent-hash',
    });

    const live = new Set(group.replicas.map((m) => m.originHash));
    for (let i = 0; i < 30; i++) {
      const origin = group.routeEvent({ routingKey: `req-${i}` });
      expect(live.has(origin)).toBe(true);
    }
  });

  it('scaleTo grows and shrinks the daemon registry', async () => {
    const { rt, mesh } = await runtimeWithPeers(5);
    cleanups.push(() => mesh.shutdown());
    cleanups.push(() => rt.shutdown());

    const group = await ReplicaGroup.spawn(rt, 'noop', {
      replicaCount: 2,
      groupSeed: seed(0x33),
      lbStrategy: 'round-robin',
    });
    expect(rt.daemonCount()).toBe(2);

    await group.scaleTo(5);
    expect(group.replicaCount).toBe(5);
    expect(rt.daemonCount()).toBe(5);

    await group.scaleTo(1);
    expect(group.replicaCount).toBe(1);
    expect(rt.daemonCount()).toBe(1);
  });

  it('spawn before runtime start throws GroupError kind not-ready', async () => {
    const mesh = await buildMesh();
    cleanups.push(() => mesh.shutdown());
    const rt = DaemonRuntime.create(mesh);
    cleanups.push(() => rt.shutdown());
    rt.registerFactory('noop', () => new NoopDaemon());
    // Intentionally skip rt.start()

    try {
      await ReplicaGroup.spawn(rt, 'noop', {
        replicaCount: 2,
        groupSeed: seed(0x44),
        lbStrategy: 'round-robin',
      });
      expect.fail('expected GroupError');
    } catch (e) {
      expect(e).toBeInstanceOf(GroupError);
      expect(e).toBeInstanceOf(DaemonError);
      expect((e as GroupError).kind).toBe('not-ready');
    }
  });

  it('spawn with unregistered kind throws factory-not-found', async () => {
    const { rt, mesh } = await runtimeWithPeers(2);
    cleanups.push(() => mesh.shutdown());
    cleanups.push(() => rt.shutdown());

    try {
      await ReplicaGroup.spawn(rt, 'never-registered', {
        replicaCount: 2,
        groupSeed: seed(0x55),
        lbStrategy: 'round-robin',
      });
      expect.fail('expected GroupError');
    } catch (e) {
      expect(e).toBeInstanceOf(GroupError);
      expect((e as GroupError).kind).toBe('factory-not-found');
      expect((e as GroupError).requestedKind).toBe('never-registered');
    }
  });
});

describe('ForkGroup', () => {
  const cleanups: Array<() => Promise<void>> = [];
  afterEach(async () => {
    while (cleanups.length > 0) {
      const fn = cleanups.pop();
      if (fn) {
        try {
          await fn();
        } catch {
          // best-effort
        }
      }
    }
  });

  it('forks have unique origins and verifiable lineage', async () => {
    const { rt, mesh } = await runtimeWithPeers(4);
    cleanups.push(() => mesh.shutdown());
    cleanups.push(() => rt.shutdown());

    const group = await ForkGroup.fork(rt, 'noop', 0xabcd_ef01n, 42n, {
      forkCount: 3,
      lbStrategy: 'round-robin',
    });

    expect(group.forkCount).toBe(3);
    expect(group.parentOrigin).toBe(0xabcd_ef01n);
    expect(group.forkSeq).toBe(42n);
    expect(group.verifyLineage()).toBe(true);

    const origins = new Set(group.members.map((m) => m.originHash));
    expect(origins.size).toBe(3);
    expect(group.forkRecords).toHaveLength(3);
  });

  it('fork before runtime start throws not-ready', async () => {
    const mesh = await buildMesh();
    cleanups.push(() => mesh.shutdown());
    const rt = DaemonRuntime.create(mesh);
    cleanups.push(() => rt.shutdown());
    rt.registerFactory('noop', () => new NoopDaemon());

    try {
      await ForkGroup.fork(rt, 'noop', 0x1234n, 1n, {
        forkCount: 2,
        lbStrategy: 'round-robin',
      });
      expect.fail('expected GroupError');
    } catch (e) {
      expect((e as GroupError).kind).toBe('not-ready');
    }
  });
});

describe('StandbyGroup', () => {
  const cleanups: Array<() => Promise<void>> = [];
  afterEach(async () => {
    while (cleanups.length > 0) {
      const fn = cleanups.pop();
      if (fn) {
        try {
          await fn();
        } catch {
          // best-effort
        }
      }
    }
  });

  it('spawn makes member 0 active and leaves standbys buffered-empty', async () => {
    const { rt, mesh } = await runtimeWithPeers(3);
    cleanups.push(() => mesh.shutdown());
    cleanups.push(() => rt.shutdown());

    const group = await StandbyGroup.spawn(rt, 'noop', {
      memberCount: 3,
      groupSeed: seed(0x77),
    });

    expect(group.memberCount).toBe(3);
    expect(group.standbyCount).toBe(2);
    expect(group.activeIndex).toBe(0);
    expect(group.activeHealthy).toBe(true);
    expect(group.activeOrigin).not.toBe(0n);
    expect(group.bufferedEventCount).toBe(0);
    expect(group.memberRole(0)).toBe('active');
    expect(group.memberRole(1)).toBe('standby');
    expect(group.memberRole(99)).toBeNull();
  });

  it('member_count below 2 surfaces as invalid-config', async () => {
    const { rt, mesh } = await runtimeWithPeers(3);
    cleanups.push(() => mesh.shutdown());
    cleanups.push(() => rt.shutdown());

    try {
      await StandbyGroup.spawn(rt, 'noop', {
        memberCount: 1,
        groupSeed: seed(0x88),
      });
      expect.fail('expected GroupError');
    } catch (e) {
      expect(e).toBeInstanceOf(GroupError);
      expect((e as GroupError).kind).toBe('invalid-config');
    }
  });

  it('spawn with unregistered kind throws factory-not-found', async () => {
    const { rt, mesh } = await runtimeWithPeers(3);
    cleanups.push(() => mesh.shutdown());
    cleanups.push(() => rt.shutdown());

    try {
      await StandbyGroup.spawn(rt, 'never-registered', {
        memberCount: 2,
        groupSeed: seed(0x99),
      });
      expect.fail('expected GroupError');
    } catch (e) {
      expect((e as GroupError).kind).toBe('factory-not-found');
    }
  });

  it('rejects a groupSeed buffer that is not exactly 32 bytes', async () => {
    const { rt, mesh } = await runtimeWithPeers(3);
    cleanups.push(() => mesh.shutdown());
    cleanups.push(() => rt.shutdown());

    // Invalid: 16 bytes instead of 32. NAPI-side `parse_seed`
    // surfaces this as `daemon: group: invalid-config: ...`.
    try {
      await StandbyGroup.spawn(rt, 'noop', {
        memberCount: 2,
        groupSeed: Buffer.alloc(16, 0xaa),
      });
      expect.fail('expected GroupError');
    } catch (e) {
      expect((e as GroupError).kind).toBe('invalid-config');
    }
  });
});
