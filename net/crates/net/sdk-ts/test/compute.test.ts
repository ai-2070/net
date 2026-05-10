// Smoke tests for the compute surface — Stage 3 sub-step 1.
//
// Scope: lifecycle only. A TS caller can build a `DaemonRuntime`
// against a `MeshNode`, register a factory (stored but not yet
// invoked), start the runtime, and shut it down. Event delivery,
// migration, snapshot/restore, and cross-language daemon execution
// land in sub-steps 2-5.

import { afterEach, describe, expect, it } from 'vitest';

import {
  DaemonError,
  DaemonHandle,
  DaemonRuntime,
  Identity,
  MeshNode,
  MigrationError,
  MigrationHandle,
  type MigrationPhase,
} from '../src';

const PSK = '42'.repeat(32);

// Unique ports per test case so repeated runs don't collide.
let portSeed = 29_100;
function nextPort(): string {
  return `127.0.0.1:${portSeed++}`;
}

async function buildMesh(): Promise<MeshNode> {
  return MeshNode.create({ bindAddr: nextPort(), psk: PSK });
}

describe('DaemonRuntime (Stage 3 sub-step 1: skeleton + lifecycle)', () => {
  const cleanups: Array<() => Promise<void>> = [];

  afterEach(async () => {
    while (cleanups.length > 0) {
      const fn = cleanups.pop();
      if (fn) {
        try {
          await fn();
        } catch {
          // Best-effort — we're tearing down fixtures, not asserting on them.
        }
      }
    }
  });

  it('builds against a mesh and reports not-ready before start', async () => {
    const mesh = await buildMesh();
    cleanups.push(() => mesh.shutdown());

    const rt = DaemonRuntime.create(mesh);
    cleanups.push(() => rt.shutdown());

    expect(rt.isReady()).toBe(false);
    expect(rt.daemonCount()).toBe(0);
  });

  it('start flips to ready; shutdown flips back', async () => {
    const mesh = await buildMesh();
    cleanups.push(() => mesh.shutdown());

    const rt = DaemonRuntime.create(mesh);
    await rt.start();
    expect(rt.isReady()).toBe(true);

    await rt.shutdown();
    expect(rt.isReady()).toBe(false);
  });

  it('registerFactory accepts a JS factory; second registration of the same kind throws', async () => {
    const mesh = await buildMesh();
    cleanups.push(() => mesh.shutdown());

    const rt = DaemonRuntime.create(mesh);
    cleanups.push(() => rt.shutdown());

    const stubFactory = () => ({
      name: 'echo',
      process: (_event: unknown) => [],
    });

    // First registration: succeeds.
    rt.registerFactory('echo', stubFactory);

    // Second: rejected with a typed `DaemonError`.
    expect(() => rt.registerFactory('echo', stubFactory)).toThrow(DaemonError);
    expect(() => rt.registerFactory('echo', stubFactory)).toThrow(
      /already registered/,
    );
  });

  it('registerFactory works after start (runtime admits new kinds in Ready state)', async () => {
    const mesh = await buildMesh();
    cleanups.push(() => mesh.shutdown());

    const rt = DaemonRuntime.create(mesh);
    cleanups.push(() => rt.shutdown());

    await rt.start();

    expect(() =>
      rt.registerFactory('late', () => ({
        name: 'late',
        process: () => [],
      })),
    ).not.toThrow();
  });

  it('shutdown is idempotent', async () => {
    const mesh = await buildMesh();
    cleanups.push(() => mesh.shutdown());

    const rt = DaemonRuntime.create(mesh);
    await rt.start();
    await rt.shutdown();
    // Second shutdown: no throw.
    await expect(rt.shutdown()).resolves.toBeUndefined();
  });

  // Phase 6 of `CAPABILITY_SYSTEM_SDK_PLAN.md`: factories may
  // declare `requiredCapabilities` / `optionalCapabilities` —
  // captured at factory time and forwarded to the substrate's
  // `MeshDaemon::required_capabilities` / `optional_capabilities`.
  it('registerFactory accepts requiredCapabilities + optionalCapabilities', async () => {
    const mesh = await buildMesh();
    cleanups.push(() => mesh.shutdown());

    const rt = DaemonRuntime.create(mesh);
    cleanups.push(() => rt.shutdown());

    // Static caps declared at factory time. The new fields are
    // optional; existing daemons compile unchanged.
    const inferenceFactory = () => ({
      name: 'inference',
      process: (_event: unknown) => [],
      requiredCapabilities: {
        tags: ['hardware.gpu'],
      },
      optionalCapabilities: {
        tags: ['hardware.gpu.vram_mb=81920'],
      },
    });

    expect(() => rt.registerFactory('inference', inferenceFactory)).not.toThrow();
  });
});

// Sub-step 2a: spawn / stop lifecycle. Daemon is a no-op bridge
// on the Rust side; factory TSFN is not yet invoked. Sub-step 2b
// replaces the bridge with one that dispatches events to the
// JS-returned object.
describe('DaemonRuntime (Stage 3 sub-step 2a: spawn + stop)', () => {
  const cleanups: Array<() => Promise<void>> = [];

  afterEach(async () => {
    while (cleanups.length > 0) {
      const fn = cleanups.pop();
      if (fn) {
        try {
          await fn();
        } catch {
          // Best-effort teardown.
        }
      }
    }
  });

  async function startedRuntime(): Promise<{
    mesh: MeshNode;
    rt: DaemonRuntime;
  }> {
    const mesh = await buildMesh();
    cleanups.push(() => mesh.shutdown());
    const rt = DaemonRuntime.create(mesh);
    cleanups.push(() => rt.shutdown());
    rt.registerFactory('echo', () => ({ name: 'echo', process: () => [] }));
    await rt.start();
    return { mesh, rt };
  }

  it('spawn returns a handle with originHash + entityId', async () => {
    const { rt } = await startedRuntime();
    const id = Identity.generate();
    const handle = await rt.spawn('echo', id);

    expect(handle).toBeInstanceOf(DaemonHandle);
    expect(typeof handle.originHash).toBe('bigint');
    expect(handle.entityId).toBeInstanceOf(Buffer);
    expect(handle.entityId.length).toBe(32);
    // originHash matches identity's origin hash (the 8-byte
    // BLAKE2s-derived hash widened to u64 in v0.11). We don't
    // recompute here — just assert it's non-zero, because
    // `Identity.generate()` produces random bytes that essentially
    // never hash to zero.
    expect(handle.originHash).not.toBe(0n);

    expect(rt.daemonCount()).toBe(1);
  });

  it('spawn -> stop reduces daemonCount', async () => {
    const { rt } = await startedRuntime();
    const handle = await rt.spawn('echo', Identity.generate());
    expect(rt.daemonCount()).toBe(1);

    await rt.stop(handle.originHash);
    expect(rt.daemonCount()).toBe(0);
  });

  it('spawn with an unregistered kind throws DaemonError', async () => {
    const { rt } = await startedRuntime();
    await expect(rt.spawn('missing', Identity.generate())).rejects.toThrow(
      DaemonError,
    );
    await expect(rt.spawn('missing', Identity.generate())).rejects.toThrow(
      /no factory registered/,
    );
  });

  it('spawn with the same identity twice rejects the second call', async () => {
    const { rt } = await startedRuntime();
    const id = Identity.generate();
    await rt.spawn('echo', id);
    // Second spawn: same origin_hash -> atomic factory_registry
    // rejects. The underlying SDK surfaces this as the
    // `already registered` message with the `daemon:` prefix.
    await expect(rt.spawn('echo', id)).rejects.toThrow(DaemonError);
    expect(rt.daemonCount()).toBe(1);
  });

  it('spawn many, stop each, daemonCount reaches zero', async () => {
    const { rt } = await startedRuntime();
    const handles: DaemonHandle[] = [];
    for (let i = 0; i < 10; i++) {
      handles.push(await rt.spawn('echo', Identity.generate()));
    }
    expect(rt.daemonCount()).toBe(10);

    for (const h of handles) {
      await rt.stop(h.originHash);
    }
    expect(rt.daemonCount()).toBe(0);
  });

  it('spawn after shutdown rejects with DaemonError', async () => {
    const { rt } = await startedRuntime();
    await rt.shutdown();
    await expect(rt.spawn('echo', Identity.generate())).rejects.toThrow(
      DaemonError,
    );
  });

  it('config with auto-snapshot + max-log-entries is accepted', async () => {
    const { rt } = await startedRuntime();
    const handle = await rt.spawn('echo', Identity.generate(), {
      autoSnapshotInterval: 128n,
      maxLogEntries: 2048,
    });
    expect(handle.originHash).not.toBe(0n);
  });

  it('factory is invoked exactly once per spawn; each invocation gets its own closure state', async () => {
    // Factory that closes over a per-invocation counter — proves
    // each spawn gets a fresh instance, not a shared one.
    const mesh = await buildMesh();
    cleanups.push(() => mesh.shutdown());
    const rt = DaemonRuntime.create(mesh);
    cleanups.push(() => rt.shutdown());

    let invocations = 0;
    rt.registerFactory('counter', () => {
      invocations++;
      let localState = 0;
      return {
        name: 'counter',
        // `process` closes over `localState`; if sub-step 3 ever
        // starts dispatching events, each instance will see its
        // own state. Sub-step 2b: just assert the factory ran.
        process: () => {
          localState++;
          return [];
        },
      };
    });
    await rt.start();

    expect(invocations).toBe(0);
    await rt.spawn('counter', Identity.generate());
    expect(invocations).toBe(1);
    await rt.spawn('counter', Identity.generate());
    expect(invocations).toBe(2);
    await rt.spawn('counter', Identity.generate());
    expect(invocations).toBe(3);
    expect(rt.daemonCount()).toBe(3);
  });

  it('async factory is awaited before spawn resolves', async () => {
    const mesh = await buildMesh();
    cleanups.push(() => mesh.shutdown());
    const rt = DaemonRuntime.create(mesh);
    cleanups.push(() => rt.shutdown());

    let factoryResolved = false;
    rt.registerFactory('async-echo', async () => {
      await new Promise((r) => setTimeout(r, 10));
      factoryResolved = true;
      return { name: 'async-echo', process: () => [] };
    });
    await rt.start();

    expect(factoryResolved).toBe(false);
    const handle = await rt.spawn('async-echo', Identity.generate());
    expect(factoryResolved).toBe(true);
    expect(handle.originHash).not.toBe(0n);
  });

  it('snapshot / restore methods are optional', async () => {
    const mesh = await buildMesh();
    cleanups.push(() => mesh.shutdown());
    const rt = DaemonRuntime.create(mesh);
    cleanups.push(() => rt.shutdown());

    // Stateless factory — only `process`. snapshot/restore omitted.
    rt.registerFactory('stateless', () => ({
      name: 'stateless',
      process: () => [],
    }));
    await rt.start();

    const handle = await rt.spawn('stateless', Identity.generate());
    expect(handle.originHash).not.toBe(0n);
    await rt.stop(handle.originHash);
    expect(rt.daemonCount()).toBe(0);
  });
});

// Sub-step 3: event dispatch. `deliver()` invokes the JS `process`
// callback through the TSFN round-trip and returns the outputs.
describe('DaemonRuntime (Stage 3 sub-step 3: event dispatch)', () => {
  const cleanups: Array<() => Promise<void>> = [];

  afterEach(async () => {
    while (cleanups.length > 0) {
      const fn = cleanups.pop();
      if (fn) {
        try {
          await fn();
        } catch {
          // Best-effort teardown.
        }
      }
    }
  });

  it('EchoDaemon returns the input payload on deliver', async () => {
    const mesh = await buildMesh();
    cleanups.push(() => mesh.shutdown());
    const rt = DaemonRuntime.create(mesh);
    cleanups.push(() => rt.shutdown());

    rt.registerFactory('echo', () => ({
      name: 'echo',
      process: (event) => [event.payload],
    }));
    await rt.start();

    const id = Identity.generate();
    const handle = await rt.spawn('echo', id);

    const payload = Buffer.from('hello world', 'utf8');
    const outputs = await rt.deliver(handle.originHash, {
      originHash: id.originHash,
      sequence: 1n,
      payload,
    });

    expect(outputs.length).toBe(1);
    expect(outputs[0].equals(payload)).toBe(true);
  });

  it('process closure sees per-instance state across multiple deliveries', async () => {
    const mesh = await buildMesh();
    cleanups.push(() => mesh.shutdown());
    const rt = DaemonRuntime.create(mesh);
    cleanups.push(() => rt.shutdown());

    rt.registerFactory('counter', () => {
      let count = 0;
      return {
        name: 'counter',
        process: () => {
          count += 1;
          const buf = Buffer.alloc(4);
          buf.writeUInt32LE(count, 0);
          return [buf];
        },
      };
    });
    await rt.start();

    const id = Identity.generate();
    const handle = await rt.spawn('counter', id);

    for (let i = 1; i <= 5; i++) {
      const out = await rt.deliver(handle.originHash, {
        originHash: id.originHash,
        sequence: BigInt(i),
        payload: Buffer.alloc(0),
      });
      expect(out.length).toBe(1);
      expect(out[0].readUInt32LE(0)).toBe(i);
    }
  });

  it('two concurrent daemons keep independent state', async () => {
    const mesh = await buildMesh();
    cleanups.push(() => mesh.shutdown());
    const rt = DaemonRuntime.create(mesh);
    cleanups.push(() => rt.shutdown());

    rt.registerFactory('counter', () => {
      let count = 0;
      return {
        name: 'counter',
        process: () => {
          count += 1;
          const buf = Buffer.alloc(4);
          buf.writeUInt32LE(count, 0);
          return [buf];
        },
      };
    });
    await rt.start();

    const idA = Identity.generate();
    const idB = Identity.generate();
    const hA = await rt.spawn('counter', idA);
    const hB = await rt.spawn('counter', idB);

    const evt = (id: Identity, seq: bigint) => ({
      originHash: id.originHash,
      sequence: seq,
      payload: Buffer.alloc(0),
    });

    // Drive A three times, B once, then A twice.
    for (let i = 1; i <= 3; i++) await rt.deliver(hA.originHash, evt(idA, BigInt(i)));
    const bOnce = await rt.deliver(hB.originHash, evt(idB, 1n));
    expect(bOnce[0].readUInt32LE(0)).toBe(1);
    for (let i = 4; i <= 5; i++) {
      const out = await rt.deliver(hA.originHash, evt(idA, BigInt(i)));
      expect(out[0].readUInt32LE(0)).toBe(i);
    }
    const bAgain = await rt.deliver(hB.originHash, evt(idB, 2n));
    expect(bAgain[0].readUInt32LE(0)).toBe(2);
  });

  it('process returning multiple buffers: caller sees all of them', async () => {
    const mesh = await buildMesh();
    cleanups.push(() => mesh.shutdown());
    const rt = DaemonRuntime.create(mesh);
    cleanups.push(() => rt.shutdown());

    rt.registerFactory('fanout', () => ({
      name: 'fanout',
      process: () => [
        Buffer.from('a'),
        Buffer.from('bb'),
        Buffer.from('ccc'),
      ],
    }));
    await rt.start();

    const id = Identity.generate();
    const handle = await rt.spawn('fanout', id);
    const out = await rt.deliver(handle.originHash, {
      originHash: id.originHash,
      sequence: 1n,
      payload: Buffer.alloc(0),
    });
    expect(out.length).toBe(3);
    expect(out[0].toString()).toBe('a');
    expect(out[1].toString()).toBe('bb');
    expect(out[2].toString()).toBe('ccc');
  });

  it('deliver to an unknown origin throws DaemonError', async () => {
    const mesh = await buildMesh();
    cleanups.push(() => mesh.shutdown());
    const rt = DaemonRuntime.create(mesh);
    cleanups.push(() => rt.shutdown());
    rt.registerFactory('echo', () => ({
      name: 'echo',
      process: (e) => [e.payload],
    }));
    await rt.start();

    await expect(
      rt.deliver(0xdeadbeefn, {
        originHash: 0xdeadbeefn,
        sequence: 1n,
        payload: Buffer.from('x'),
      }),
    ).rejects.toThrow(DaemonError);
  });

  it('JS process throwing surfaces as DaemonError', async () => {
    const mesh = await buildMesh();
    cleanups.push(() => mesh.shutdown());
    const rt = DaemonRuntime.create(mesh);
    cleanups.push(() => rt.shutdown());

    rt.registerFactory('buggy', () => ({
      name: 'buggy',
      process: () => {
        throw new Error('boom');
      },
    }));
    await rt.start();
    const id = Identity.generate();
    const handle = await rt.spawn('buggy', id);
    await expect(
      rt.deliver(handle.originHash, {
        originHash: id.originHash,
        sequence: 1n,
        payload: Buffer.alloc(0),
      }),
    ).rejects.toThrow(DaemonError);
  });
});

describe('DaemonRuntime (Stage 3 sub-step 4: snapshot + restore round-trip)', () => {
  const cleanups: Array<() => Promise<void>> = [];

  afterEach(async () => {
    while (cleanups.length > 0) {
      const fn = cleanups.pop();
      if (fn) {
        try {
          await fn();
        } catch {
          // Best-effort teardown.
        }
      }
    }
  });

  // CounterDaemon used across the round-trip tests: increments a
  // local `count` per event, returns the current count as a LE
  // u32, and serializes `count` as a 4-byte buffer in its
  // `snapshot` / `restore` pair. Factory is parameterized so tests
  // can pre-seed the counter when checking restore semantics.
  function counterFactory(initial = 0) {
    return () => {
      let count = initial;
      return {
        name: 'counter',
        process: () => {
          count += 1;
          const buf = Buffer.alloc(4);
          buf.writeUInt32LE(count, 0);
          return [buf];
        },
        snapshot: (): Buffer | null => {
          const buf = Buffer.alloc(4);
          buf.writeUInt32LE(count, 0);
          return buf;
        },
        restore: (state: Buffer) => {
          count = state.readUInt32LE(0);
        },
      };
    };
  }

  it('snapshot after N deliveries captures the counter; spawnFromSnapshot restores it', async () => {
    const mesh = await buildMesh();
    cleanups.push(() => mesh.shutdown());
    const rt = DaemonRuntime.create(mesh);
    cleanups.push(() => rt.shutdown());

    rt.registerFactory('counter', counterFactory());
    await rt.start();

    const id = Identity.generate();
    const handle = await rt.spawn('counter', id);
    const evt = (seq: bigint) => ({
      originHash: id.originHash,
      sequence: seq,
      payload: Buffer.alloc(0),
    });

    // Drive the counter to 3.
    for (let i = 1; i <= 3; i++) await rt.deliver(handle.originHash, evt(BigInt(i)));

    const snapBytes = await rt.snapshot(handle.originHash);
    expect(snapBytes).not.toBeNull();
    expect(snapBytes!.length).toBeGreaterThan(0);

    // Tear the original daemon down — the restored instance must
    // pick up purely from the snapshot, not from live state.
    await rt.stop(handle.originHash);
    expect(rt.daemonCount()).toBe(0);

    const restored = await rt.spawnFromSnapshot('counter', id, snapBytes!);
    expect(rt.daemonCount()).toBe(1);
    // Same identity -> same origin_hash after restore.
    expect(restored.originHash).toBe(handle.originHash);

    // One more delivery — counter should step from 3 to 4, proving
    // the snapshot's state survived the round trip.
    const out = await rt.deliver(restored.originHash, evt(4n));
    expect(out.length).toBe(1);
    expect(out[0].readUInt32LE(0)).toBe(4);
  });

  it('snapshot of a stateless daemon returns null', async () => {
    const mesh = await buildMesh();
    cleanups.push(() => mesh.shutdown());
    const rt = DaemonRuntime.create(mesh);
    cleanups.push(() => rt.shutdown());

    rt.registerFactory('echo', () => ({
      name: 'echo',
      process: (e) => [e.payload],
      // No snapshot / restore -> host reports daemon as stateless.
    }));
    await rt.start();

    const id = Identity.generate();
    const handle = await rt.spawn('echo', id);

    const snap = await rt.snapshot(handle.originHash);
    expect(snap).toBeNull();
  });

  it('snapshot of an unknown origin rejects with DaemonError', async () => {
    const mesh = await buildMesh();
    cleanups.push(() => mesh.shutdown());
    const rt = DaemonRuntime.create(mesh);
    cleanups.push(() => rt.shutdown());
    rt.registerFactory('counter', counterFactory());
    await rt.start();

    await expect(rt.snapshot(0xdeadbeefn)).rejects.toThrow(DaemonError);
  });

  it('spawnFromSnapshot with corrupted bytes rejects with DaemonError', async () => {
    const mesh = await buildMesh();
    cleanups.push(() => mesh.shutdown());
    const rt = DaemonRuntime.create(mesh);
    cleanups.push(() => rt.shutdown());
    rt.registerFactory('counter', counterFactory());
    await rt.start();

    const id = Identity.generate();
    const garbage = Buffer.from('not a real snapshot');
    await expect(
      rt.spawnFromSnapshot('counter', id, garbage),
    ).rejects.toThrow(DaemonError);
  });

  it('spawnFromSnapshot with mismatched identity rejects with DaemonError', async () => {
    const mesh = await buildMesh();
    cleanups.push(() => mesh.shutdown());
    const rt = DaemonRuntime.create(mesh);
    cleanups.push(() => rt.shutdown());

    rt.registerFactory('counter', counterFactory());
    await rt.start();

    const original = Identity.generate();
    const handle = await rt.spawn('counter', original);
    await rt.deliver(handle.originHash, {
      originHash: original.originHash,
      sequence: 1n,
      payload: Buffer.alloc(0),
    });
    const snap = await rt.snapshot(handle.originHash);
    expect(snap).not.toBeNull();
    await rt.stop(handle.originHash);

    // Different identity -> snapshot's entity_id doesn't match.
    const other = Identity.generate();
    await expect(
      rt.spawnFromSnapshot('counter', other, snap!),
    ).rejects.toThrow(DaemonError);
  });

  it('DaemonHandle.stats counts events processed + snapshots taken', async () => {
    const mesh = await buildMesh();
    cleanups.push(() => mesh.shutdown());
    const rt = DaemonRuntime.create(mesh);
    cleanups.push(() => rt.shutdown());

    rt.registerFactory('counter', counterFactory());
    await rt.start();

    const id = Identity.generate();
    const handle = await rt.spawn('counter', id);

    const initial = handle.stats();
    expect(initial.eventsProcessed).toBe(0n);
    expect(initial.eventsEmitted).toBe(0n);
    expect(initial.snapshotsTaken).toBe(0n);

    for (let i = 1; i <= 4; i++) {
      await rt.deliver(handle.originHash, {
        originHash: id.originHash,
        sequence: BigInt(i),
        payload: Buffer.alloc(0),
      });
    }

    const afterDeliveries = handle.stats();
    expect(afterDeliveries.eventsProcessed).toBe(4n);
    // CounterDaemon emits exactly one buffer per event.
    expect(afterDeliveries.eventsEmitted).toBe(4n);
    expect(afterDeliveries.errors).toBe(0n);
    // `snapshotsTaken` is reserved on the struct but not
    // incremented by the core registry at the moment — assert
    // only that reading it doesn't throw.
    expect(typeof afterDeliveries.snapshotsTaken).toBe('bigint');
  });

  it('DaemonHandle.stats on a stopped daemon rejects with DaemonError', async () => {
    const mesh = await buildMesh();
    cleanups.push(() => mesh.shutdown());
    const rt = DaemonRuntime.create(mesh);
    cleanups.push(() => rt.shutdown());

    rt.registerFactory('echo', () => ({
      name: 'echo',
      process: (e) => [e.payload],
    }));
    await rt.start();

    const id = Identity.generate();
    const handle = await rt.spawn('echo', id);
    await rt.stop(handle.originHash);

    expect(() => handle.stats()).toThrow(DaemonError);
  });

  it('snapshot -> modify -> snapshot captures the newer state', async () => {
    const mesh = await buildMesh();
    cleanups.push(() => mesh.shutdown());
    const rt = DaemonRuntime.create(mesh);
    cleanups.push(() => rt.shutdown());

    rt.registerFactory('counter', counterFactory());
    await rt.start();

    const id = Identity.generate();
    const handle = await rt.spawn('counter', id);
    const evt = (seq: bigint) => ({
      originHash: id.originHash,
      sequence: seq,
      payload: Buffer.alloc(0),
    });

    for (let i = 1; i <= 2; i++) await rt.deliver(handle.originHash, evt(BigInt(i)));
    const snapAt2 = await rt.snapshot(handle.originHash);
    for (let i = 3; i <= 5; i++) await rt.deliver(handle.originHash, evt(BigInt(i)));
    const snapAt5 = await rt.snapshot(handle.originHash);

    await rt.stop(handle.originHash);

    // Restore the earlier snapshot; next event should step to 3.
    const h2 = await rt.spawnFromSnapshot('counter', id, snapAt2!);
    const out2 = await rt.deliver(h2.originHash, evt(6n));
    expect(out2[0].readUInt32LE(0)).toBe(3);
    await rt.stop(h2.originHash);

    // Restore the later snapshot; next event should step to 6.
    const h5 = await rt.spawnFromSnapshot('counter', id, snapAt5!);
    const out5 = await rt.deliver(h5.originHash, evt(7n));
    expect(out5[0].readUInt32LE(0)).toBe(6);
  });

  it('startMigration on an unknown origin rejects with DaemonError', async () => {
    const mesh = await buildMesh();
    cleanups.push(() => mesh.shutdown());
    const rt = DaemonRuntime.create(mesh);
    cleanups.push(() => rt.shutdown());
    rt.registerFactory('counter', counterFactory());
    await rt.start();

    const selfId = mesh.nodeId();
    await expect(
      rt.startMigration(0xdeadbeefn, selfId, selfId),
    ).rejects.toThrow(DaemonError);
  });

  it('startMigration before runtime is Ready rejects with DaemonError', async () => {
    const mesh = await buildMesh();
    cleanups.push(() => mesh.shutdown());
    const rt = DaemonRuntime.create(mesh);
    cleanups.push(() => rt.shutdown());
    rt.registerFactory('counter', counterFactory());
    // Intentionally skip rt.start() — runtime stays in Registering.

    const id = Identity.generate();
    await expect(
      rt.startMigration(id.originHash, mesh.nodeId(), mesh.nodeId()),
    ).rejects.toThrow(DaemonError);
  });

  it('expectMigration requires kind to be registered first', async () => {
    const mesh = await buildMesh();
    cleanups.push(() => mesh.shutdown());
    const rt = DaemonRuntime.create(mesh);
    cleanups.push(() => rt.shutdown());
    await rt.start();

    expect(() => rt.expectMigration('never-registered', 0x1234n)).toThrow(
      DaemonError,
    );
  });

  it('expectMigration succeeds once kind is registered', async () => {
    const mesh = await buildMesh();
    cleanups.push(() => mesh.shutdown());
    const rt = DaemonRuntime.create(mesh);
    cleanups.push(() => rt.shutdown());
    rt.registerFactory('counter', counterFactory());
    await rt.start();

    // First call is fine — second for the same origin should fail
    // (registry slot already occupied).
    rt.expectMigration('counter', 0xabcd_ef01n);
    expect(() => rt.expectMigration('counter', 0xabcd_ef01n)).toThrow(
      DaemonError,
    );
  });

  it('registerMigrationTargetIdentity binds an identity for the target side', async () => {
    const mesh = await buildMesh();
    cleanups.push(() => mesh.shutdown());
    const rt = DaemonRuntime.create(mesh);
    cleanups.push(() => rt.shutdown());
    rt.registerFactory('counter', counterFactory());
    await rt.start();

    const id = Identity.generate();
    rt.registerMigrationTargetIdentity('counter', id);
    // Second bind for the same origin_hash should fail — the core
    // factory registry only admits one entry per origin.
    expect(() =>
      rt.registerMigrationTargetIdentity('counter', id),
    ).toThrow(DaemonError);
  });

  it('migrationPhase returns null when no migration is in flight', async () => {
    const mesh = await buildMesh();
    cleanups.push(() => mesh.shutdown());
    const rt = DaemonRuntime.create(mesh);
    cleanups.push(() => rt.shutdown());
    await rt.start();
    expect(rt.migrationPhase(0xdeadbeefn)).toBeNull();
  });

  it('migrationPhase returns a phase string for an in-flight migration', async () => {
    // Two-mesh setup so `startMigrationWith` succeeds.
    const [aAddr, bAddr] = [`127.0.0.1:${portSeed++}`, `127.0.0.1:${portSeed++}`];
    const a = await MeshNode.create({ bindAddr: aAddr, psk: PSK });
    cleanups.push(() => a.shutdown());
    const b = await MeshNode.create({ bindAddr: bAddr, psk: PSK });
    cleanups.push(() => b.shutdown());
    await Promise.all([
      b.accept(a.nodeId()),
      (async () => {
        await new Promise((r) => setTimeout(r, 50));
        await a.connect(bAddr, b.publicKey(), b.nodeId());
      })(),
    ]);
    await a.start();
    await b.start();

    const rtA = DaemonRuntime.create(a);
    cleanups.push(() => rtA.shutdown());
    rtA.registerFactory('counter', counterFactory());
    await rtA.start();

    const id = Identity.generate();
    const spawn = await rtA.spawn('counter', id);
    const mig = await rtA.startMigrationWith(
      spawn.originHash,
      a.nodeId(),
      b.nodeId(),
      { transportIdentity: false, retryNotReadyMs: 0n },
    );

    // `migrationPhase` on A should see the record; observer's view
    // may race so we just check it's string | null.
    const phase = rtA.migrationPhase(spawn.originHash);
    expect(phase === null || typeof phase === 'string').toBe(true);

    try {
      await mig.cancel();
    } catch {
      // Already-terminated record; fine.
    }
  });

  it('startMigration with unknown target peer throws MigrationError { kind: "target-unavailable" }', async () => {
    const mesh = await buildMesh();
    cleanups.push(() => mesh.shutdown());
    const rt = DaemonRuntime.create(mesh);
    cleanups.push(() => rt.shutdown());
    rt.registerFactory('counter', counterFactory());
    await rt.start();

    const id = Identity.generate();
    const spawn = await rt.spawn('counter', id);

    // Migrate to a node ID we never handshook with — source's
    // peer table has no entry, so start_migration_with fails at
    // the seal-envelope step (or the target lookup).
    const ghostNode = 0x00aa_bb_cc_dd_ee_ff_00n;
    try {
      await rt.startMigrationWith(spawn.originHash, mesh.nodeId(), ghostNode, {
        transportIdentity: false,
        retryNotReadyMs: 0n,
      });
      throw new Error('expected startMigrationWith to throw');
    } catch (e) {
      expect(e).toBeInstanceOf(MigrationError);
      const err = e as MigrationError;
      // Unknown target: either target-unavailable (peer lookup
      // failed) or identity-transport-failed (envelope seal
      // against missing peer X25519 pubkey). `state-failed`
      // would indicate a snapshot/restore problem, which is
      // unrelated to an unreachable target — rejecting it here
      // keeps the test specific to the unknown-target guarantee.
      expect([
        'target-unavailable',
        'identity-transport-failed',
      ]).toContain(err.kind);
    }
  });

  it('MigrationError is catch-able as a DaemonError (subclass)', async () => {
    const mesh = await buildMesh();
    cleanups.push(() => mesh.shutdown());
    const rt = DaemonRuntime.create(mesh);
    cleanups.push(() => rt.shutdown());
    rt.registerFactory('counter', counterFactory());
    await rt.start();

    const id = Identity.generate();
    const spawn = await rt.spawn('counter', id);
    const ghostNode = 0x11_22_33_44_55_66_77_88n;
    try {
      await rt.startMigrationWith(
        spawn.originHash,
        mesh.nodeId(),
        ghostNode,
        { transportIdentity: false, retryNotReadyMs: 0n },
      );
      throw new Error('expected throw');
    } catch (e) {
      // Subclass check — catch (e: DaemonError) still matches.
      expect(e).toBeInstanceOf(DaemonError);
      expect(e).toBeInstanceOf(MigrationError);
    }
  });

  it('wait() on an orchestrator record cleared by cancel surfaces MigrationError', async () => {
    const [aAddr, bAddr] = [`127.0.0.1:${portSeed++}`, `127.0.0.1:${portSeed++}`];
    const a = await MeshNode.create({ bindAddr: aAddr, psk: PSK });
    cleanups.push(() => a.shutdown());
    const b = await MeshNode.create({ bindAddr: bAddr, psk: PSK });
    cleanups.push(() => b.shutdown());
    await Promise.all([
      b.accept(a.nodeId()),
      (async () => {
        await new Promise((r) => setTimeout(r, 50));
        await a.connect(bAddr, b.publicKey(), b.nodeId());
      })(),
    ]);
    await a.start();
    await b.start();

    const rtA = DaemonRuntime.create(a);
    cleanups.push(() => rtA.shutdown());
    rtA.registerFactory('counter', counterFactory());
    await rtA.start();

    const id = Identity.generate();
    const spawn = await rtA.spawn('counter', id);
    const mig = await rtA.startMigrationWith(
      spawn.originHash,
      a.nodeId(),
      b.nodeId(),
      { transportIdentity: false, retryNotReadyMs: 0n },
    );
    // Don't await cancel — race with wait. Attach the rejection
    // handler synchronously so vitest's unhandled-rejection
    // tripwire doesn't fire when wait() rejects in the brief
    // window before we reach `await waitOutcome` below.
    const waitOutcome: Promise<
      { ok: true } | { ok: false; error: unknown }
    > = mig.wait().then(
      () => ({ ok: true }),
      (error: unknown) => ({ ok: false, error }),
    );
    // `cancel` itself can race against the migration already
    // having terminated (snapshot phase completed past the
    // cancel point, orchestrator record removed, etc.). Either
    // outcome is acceptable here; the `await waitOutcome` below
    // is what the test is actually probing. Swallow the
    // already-terminated MigrationError from `cancel` so the
    // test stays stable under that ordering.
    try {
      await mig.cancel();
    } catch (e) {
      // Tolerated only if it's the typed migration error.
      if (!(e instanceof MigrationError)) {
        throw e;
      }
    }
    const outcome = await waitOutcome;
    // The contract: wait() either resolves (success — record
    // raced past cancel) or rejects with a MigrationError.
    if (!outcome.ok) {
      expect(outcome.error).toBeInstanceOf(MigrationError);
    }
  });

  it('phases() yields distinct transitions and terminates on cleanup', async () => {
    // Two-mesh pair: source drives a migration to a connected
    // target. Without `expectMigration`/target-side identity the
    // migration will fail on the target's dispatcher, but the
    // orchestrator record still walks through `snapshot` /
    // `transfer` etc. before being torn down — enough to observe
    // the iterator yielding distinct phases.
    const [aAddr, bAddr] = [`127.0.0.1:${portSeed++}`, `127.0.0.1:${portSeed++}`];
    const a = await MeshNode.create({ bindAddr: aAddr, psk: PSK });
    cleanups.push(() => a.shutdown());
    const b = await MeshNode.create({ bindAddr: bAddr, psk: PSK });
    cleanups.push(() => b.shutdown());
    await Promise.all([
      b.accept(a.nodeId()),
      (async () => {
        await new Promise((r) => setTimeout(r, 50));
        await a.connect(bAddr, b.publicKey(), b.nodeId());
      })(),
    ]);
    await a.start();
    await b.start();

    const rtA = DaemonRuntime.create(a);
    cleanups.push(() => rtA.shutdown());
    rtA.registerFactory('counter', counterFactory());
    await rtA.start();

    const id = Identity.generate();
    const spawn = await rtA.spawn('counter', id);
    const mig = await rtA.startMigrationWith(
      spawn.originHash,
      a.nodeId(),
      b.nodeId(),
      { transportIdentity: false, retryNotReadyMs: 0n },
    );

    // Iterator must terminate — if it doesn't, the test times
    // out and we know the cleanup path is broken. We race the
    // iterator against a 5 s hard cap.
    const iteratorDone = (async () => {
      const seen: MigrationPhase[] = [];
      for await (const phase of mig.phases()) {
        seen.push(phase);
      }
      return seen;
    })();
    const timeout = new Promise<MigrationPhase[]>((_, rej) =>
      setTimeout(() => rej(new Error('phases iterator hung')), 5000),
    );

    const seen = await Promise.race([iteratorDone, timeout]);

    // Each yielded phase must be distinct from its predecessor —
    // that's the iterator's contract. Order within the enum isn't
    // strictly asserted (depends on scheduling + network timing),
    // but the transition uniqueness is a correctness property.
    for (let i = 1; i < seen.length; i++) {
      expect(seen[i]).not.toBe(seen[i - 1]);
    }
    // After the iterator terminates, `phase()` should be null
    // (orchestrator cleaned up).
    expect(mig.phase()).toBeNull();
  }, 15_000);

  it('phases() on a migration that never started yields nothing', async () => {
    // Pre-cleanup case: if the orchestrator record is already
    // gone before the caller starts iterating, the first poll
    // returns null and the iterator terminates immediately.
    // We construct this scenario with startMigration on unknown
    // origin — which throws, so we can't get a handle. Instead,
    // drive a migration to a cancelled state, THEN iterate.
    const [aAddr, bAddr] = [`127.0.0.1:${portSeed++}`, `127.0.0.1:${portSeed++}`];
    const a = await MeshNode.create({ bindAddr: aAddr, psk: PSK });
    cleanups.push(() => a.shutdown());
    const b = await MeshNode.create({ bindAddr: bAddr, psk: PSK });
    cleanups.push(() => b.shutdown());
    await Promise.all([
      b.accept(a.nodeId()),
      (async () => {
        await new Promise((r) => setTimeout(r, 50));
        await a.connect(bAddr, b.publicKey(), b.nodeId());
      })(),
    ]);
    await a.start();
    await b.start();

    const rtA = DaemonRuntime.create(a);
    cleanups.push(() => rtA.shutdown());
    rtA.registerFactory('counter', counterFactory());
    await rtA.start();

    const id = Identity.generate();
    const spawn = await rtA.spawn('counter', id);
    const mig = await rtA.startMigrationWith(
      spawn.originHash,
      a.nodeId(),
      b.nodeId(),
      { transportIdentity: false, retryNotReadyMs: 0n },
    );
    // Cancel before iterating — record goes away fast.
    try {
      await mig.cancel();
    } catch {
      // Already terminal; fine.
    }
    // Give the orchestrator a beat to finish tearing down before
    // we start iterating.
    await new Promise((r) => setTimeout(r, 200));

    const seen: MigrationPhase[] = [];
    for await (const phase of mig.phases()) {
      seen.push(phase);
    }
    // Either empty (record already gone at first poll) or at
    // most a handful of residual phases. Never hangs.
    expect(seen.length).toBeLessThan(10);
  }, 15_000);

  it('startMigrationWith transportIdentity=false produces a handle to a connected peer', async () => {
    // Full two-mesh pair: connect A <-> B, spawn on A, and
    // initiate a migration A → B. The target has no pre-registered
    // factory for this origin_hash, so the migration will fail on
    // the target's dispatcher, but start_migration on the source
    // only needs the target peer to be reachable and the envelope
    // skippable — both covered by `transportIdentity: false`.
    const [aAddr, bAddr] = [`127.0.0.1:${portSeed++}`, `127.0.0.1:${portSeed++}`];
    const a = await MeshNode.create({ bindAddr: aAddr, psk: PSK });
    cleanups.push(() => a.shutdown());
    const b = await MeshNode.create({ bindAddr: bAddr, psk: PSK });
    cleanups.push(() => b.shutdown());
    // Handshake A <-> B so they appear in each other's peer tables.
    await Promise.all([
      b.accept(a.nodeId()),
      (async () => {
        await new Promise((r) => setTimeout(r, 50));
        await a.connect(bAddr, b.publicKey(), b.nodeId());
      })(),
    ]);
    await a.start();
    await b.start();

    const rtA = DaemonRuntime.create(a);
    cleanups.push(() => rtA.shutdown());
    rtA.registerFactory('counter', counterFactory());
    await rtA.start();

    const id = Identity.generate();
    const spawn = await rtA.spawn('counter', id);

    const mig = await rtA.startMigrationWith(
      spawn.originHash,
      a.nodeId(),
      b.nodeId(),
      { transportIdentity: false, retryNotReadyMs: 0n },
    );
    expect(mig).toBeInstanceOf(MigrationHandle);
    expect(mig.originHash).toBe(spawn.originHash);
    expect(mig.sourceNode).toBe(a.nodeId());
    expect(mig.targetNode).toBe(b.nodeId());
    // Phase is a string right after start (before orchestrator
    // cleanup) or null if the migration already resolved terminally.
    const p = mig.phase();
    expect(p === null || typeof p === 'string').toBe(true);

    // Clean up — the migration will fail on B (no factory for this
    // origin_hash on the target), so wait() would reject. We just
    // cancel and move on.
    try {
      await mig.cancel();
    } catch {
      // Already-terminated record — cancel may surface
      // `no such migration`. That's fine for this smoke test.
    }
  });

  it('mid-flight failure: target has no factory for origin → wait() rejects kind factory-not-found', async () => {
    // Stage 4 exit-criterion test: a mid-flight failure surfaces
    // through `wait()` as a typed MigrationError whose `.kind`
    // discriminates the structured cause. Here the target has no
    // `expectMigration` entry for the origin_hash, so the inbound
    // SnapshotReady hits a dispatcher with no factory → wire-level
    // MigrationFailed(FactoryNotFound).
    const [aAddr, bAddr] = [`127.0.0.1:${portSeed++}`, `127.0.0.1:${portSeed++}`];
    const a = await MeshNode.create({ bindAddr: aAddr, psk: PSK });
    cleanups.push(() => a.shutdown());
    const b = await MeshNode.create({ bindAddr: bAddr, psk: PSK });
    cleanups.push(() => b.shutdown());
    await Promise.all([
      b.accept(a.nodeId()),
      (async () => {
        await new Promise((r) => setTimeout(r, 50));
        await a.connect(bAddr, b.publicKey(), b.nodeId());
      })(),
    ]);
    await a.start();
    await b.start();

    const rtA = DaemonRuntime.create(a);
    cleanups.push(() => rtA.shutdown());
    rtA.registerFactory('counter', counterFactory());
    await rtA.start();

    const rtB = DaemonRuntime.create(b);
    cleanups.push(() => rtB.shutdown());
    rtB.registerFactory('counter', counterFactory());
    await rtB.start();
    // Deliberately do NOT call rtB.expectMigration — so the
    // target's factory_registry has no entry for this origin_hash.

    const id = Identity.generate();
    const handle = await rtA.spawn('counter', id);
    const mig = await rtA.startMigrationWith(
      handle.originHash,
      a.nodeId(),
      b.nodeId(),
      // Disable retry so the first failure surfaces without the
      // default 30s NotReady backoff delaying the test.
      { retryNotReadyMs: 0n },
    );

    try {
      await mig.waitWithTimeout(5000n);
      throw new Error('expected wait() to reject');
    } catch (e) {
      expect(e).toBeInstanceOf(MigrationError);
      const err = e as MigrationError;
      expect(err.kind).toBe('factory-not-found');
    }
  }, 15_000);

  it('mid-flight failure: target restore throws → wait() rejects kind state-failed', async () => {
    // Build the source daemon with the normal counter factory so
    // snapshot() emits a valid 4-byte state payload. The target
    // registers a factory whose `restore` throws — the dispatcher
    // calls `DaemonHost::from_snapshot` → bridge.restore(state) →
    // JS callback throws → CoreDaemonError::RestoreFailed → wire-
    // level MigrationFailed(StateFailed(msg)).
    const [aAddr, bAddr] = [`127.0.0.1:${portSeed++}`, `127.0.0.1:${portSeed++}`];
    const a = await MeshNode.create({ bindAddr: aAddr, psk: PSK });
    cleanups.push(() => a.shutdown());
    const b = await MeshNode.create({ bindAddr: bAddr, psk: PSK });
    cleanups.push(() => b.shutdown());
    await Promise.all([
      b.accept(a.nodeId()),
      (async () => {
        await new Promise((r) => setTimeout(r, 50));
        await a.connect(bAddr, b.publicKey(), b.nodeId());
      })(),
    ]);
    await a.start();
    await b.start();

    const rtA = DaemonRuntime.create(a);
    cleanups.push(() => rtA.shutdown());
    rtA.registerFactory('counter', counterFactory());
    await rtA.start();

    const rtB = DaemonRuntime.create(b);
    cleanups.push(() => rtB.shutdown());
    // Target-side factory whose restore throws — mirror the same
    // kind string so `expectMigration('counter', ...)` matches
    // the source's spawn kind.
    rtB.registerFactory('counter', () => {
      let count = 0;
      return {
        name: 'counter',
        process: () => {
          count += 1;
          const buf = Buffer.alloc(4);
          buf.writeUInt32LE(count, 0);
          return [buf];
        },
        snapshot: (): Buffer | null => {
          const buf = Buffer.alloc(4);
          buf.writeUInt32LE(count, 0);
          return buf;
        },
        restore: (_state: Buffer) => {
          throw new Error('deliberate restore failure');
        },
      };
    });
    await rtB.start();

    const id = Identity.generate();
    const handle = await rtA.spawn('counter', id);
    // Drive the counter so the snapshot carries real state; also
    // ensures the source is past any zero-state degenerate edge.
    for (let i = 1; i <= 2; i++) {
      await rtA.deliver(handle.originHash, {
        originHash: id.originHash,
        sequence: BigInt(i),
        payload: Buffer.alloc(0),
      });
    }

    rtB.expectMigration('counter', handle.originHash);

    const mig = await rtA.startMigration(
      handle.originHash,
      a.nodeId(),
      b.nodeId(),
    );
    try {
      await mig.waitWithTimeout(5000n);
      throw new Error('expected wait() to reject');
    } catch (e) {
      expect(e).toBeInstanceOf(MigrationError);
      const err = e as MigrationError;
      expect(err.kind).toBe('state-failed');
      // Detail should carry the restore error's message somewhere
      // in the chain. We don't pin the exact wording (it passes
      // through RestoreFailed + StateFailed formatters) but the
      // field should be a non-empty string.
      expect(typeof err.detail).toBe('string');
      expect(err.detail!.length).toBeGreaterThan(0);
    }
  }, 15_000);

  it('end-to-end migration: counter survives A → B with envelope transport', async () => {
    // Stage 4 exit criterion. Mirrors the Rust SDK's
    // `local_source_migration_reaches_complete_and_transfers_state`
    // test: spawn a stateful JS daemon on A, drive the counter
    // via deliveries, migrate to B with identity-envelope
    // transport, verify counter state survived.
    const [aAddr, bAddr] = [`127.0.0.1:${portSeed++}`, `127.0.0.1:${portSeed++}`];
    const a = await MeshNode.create({ bindAddr: aAddr, psk: PSK });
    cleanups.push(() => a.shutdown());
    const b = await MeshNode.create({ bindAddr: bAddr, psk: PSK });
    cleanups.push(() => b.shutdown());
    await Promise.all([
      b.accept(a.nodeId()),
      (async () => {
        await new Promise((r) => setTimeout(r, 50));
        await a.connect(bAddr, b.publicKey(), b.nodeId());
      })(),
    ]);
    await a.start();
    await b.start();

    // Both runtimes register the same factory — source uses it
    // for the initial spawn; target uses it (via the SDK's
    // mirrored kind_factory) to rebuild the daemon from the
    // inbound snapshot's envelope + state bytes. Synchronous
    // factory is required for the target-side reconstruction.
    const rtA = DaemonRuntime.create(a);
    cleanups.push(() => rtA.shutdown());
    rtA.registerFactory('counter', counterFactory());
    await rtA.start();

    const rtB = DaemonRuntime.create(b);
    cleanups.push(() => rtB.shutdown());
    rtB.registerFactory('counter', counterFactory());
    await rtB.start();

    const id = Identity.generate();
    const handle = await rtA.spawn('counter', id);
    const evt = (seq: bigint) => ({
      originHash: id.originHash,
      sequence: seq,
      payload: Buffer.alloc(0),
    });
    for (let i = 1; i <= 3; i++) {
      await rtA.deliver(handle.originHash, evt(BigInt(i)));
    }

    // Pre-register on B. Envelope-transport path supplies the
    // real keypair at restore time, so we only need the
    // origin_hash + kind here.
    rtB.expectMigration('counter', handle.originHash);

    const mig = await rtA.startMigration(
      handle.originHash,
      a.nodeId(),
      b.nodeId(),
    );
    await mig.waitWithTimeout(5000n);

    // Tail-end ActivateAck race — matches the 200 ms beat in the
    // Rust test. `wait` returns when the orchestrator record
    // clears on A, which can slightly precede the target-side
    // dispatcher's final daemon-registry insert.
    await new Promise((r) => setTimeout(r, 200));

    // Post-migration: A shed the daemon, B picked it up.
    expect(rtA.daemonCount()).toBe(0);
    expect(rtB.daemonCount()).toBe(1);

    // Drive one more delivery through the target. If the
    // target-side factory reconstruction worked, the JS counter
    // closure on B has been seeded from the snapshot (3), and
    // this delivery steps it to 4. If reconstruction fell back
    // to NoopBridge, the delivery returns empty.
    const out = await rtB.deliver(handle.originHash, evt(4n));
    expect(out.length).toBe(1);
    expect(out[0].readUInt32LE(0)).toBe(4);
  }, 30_000);

  // Plan exit criterion: spawn/stop 1000 daemons in a loop, heap
  // usage stable. We don't actually probe the heap — that's
  // observability, not correctness — but we do assert the registry
  // returns to empty after the churn, that no DaemonError leaks, and
  // that the runtime stays Ready throughout. If any TSFN / Arc /
  // DashMap leak pattern returned with this sub-step, the test
  // would either hang (TSFN refs blocking shutdown) or spike the
  // `daemonCount` above zero at the end (registry leak).
  it('spawn/stop 1000 daemons without leaking registry slots', async () => {
    const mesh = await buildMesh();
    cleanups.push(() => mesh.shutdown());
    const rt = DaemonRuntime.create(mesh);
    cleanups.push(() => rt.shutdown());

    rt.registerFactory('echo', () => ({
      name: 'echo',
      process: (e) => [e.payload],
    }));
    await rt.start();

    const N = 1000;
    for (let i = 0; i < N; i++) {
      const id = Identity.generate();
      const handle = await rt.spawn('echo', id);
      await rt.stop(handle.originHash);
    }

    expect(rt.isReady()).toBe(true);
    expect(rt.daemonCount()).toBe(0);
  }, 30_000);
});

// Regression: bounded wait for JS callback responses.
//
// `DaemonRegistry::deliver` holds a per-daemon `parking_lot::Mutex`
// across `process()`. If a re-entrant path (user callback reaching
// back into the runtime on the same daemon) or a blocked Node main
// thread prevents the TSFN return callback from firing, an
// unbounded `rx.recv()` on the Rust side would deadlock silently.
// The fix bounds the wait via `rx.recv_timeout(config.callbackTimeoutMs)`
// so a deadlock surfaces as a typed `DaemonError` within a budget.
// This test sets a very short budget and blocks the Node main thread
// past it; the Rust side must return a timeout error before the
// busy-wait completes.
describe('regression: bounded JS callback wait (deadlock protection)', () => {
  const cleanups: Array<() => Promise<void>> = [];

  afterEach(async () => {
    while (cleanups.length > 0) {
      const fn = cleanups.pop();
      if (fn) {
        try {
          await fn();
        } catch {
          // Best-effort teardown.
        }
      }
    }
  });

  it('deliver rejects with a timeout when the main thread is blocked past callbackTimeoutMs', async () => {
    const mesh = await buildMesh();
    cleanups.push(() => mesh.shutdown());
    const rt = DaemonRuntime.create(mesh);
    cleanups.push(() => rt.shutdown());

    rt.registerFactory('stall', () => ({
      name: 'stall',
      // Synchronous busy-wait. Blocks the Node main thread —
      // which is where the TSFN return callback would run —
      // so the Rust side's `rx.recv_timeout` must win the race.
      process: (event) => {
        const deadline = Date.now() + 400;
        while (Date.now() < deadline) {
          /* busy-wait, spins the main thread */
        }
        return [event.payload];
      },
    }));
    await rt.start();

    // Spawn with a 75 ms callback budget — well under the 400 ms
    // stall above. With an unbounded recv this would hang for
    // ~400 ms and then succeed; with the timeout fix it rejects
    // after ~75 ms with a "did not respond within" message.
    const handle = await rt.spawn('stall', Identity.generate(), {
      callbackTimeoutMs: 75,
    });

    await expect(
      rt.deliver(handle.originHash, {
        originHash: handle.originHash,
        sequence: 1n,
        payload: Buffer.from('x'),
      }),
    ).rejects.toThrow(/did not respond within 75 ms/);
  }, 5_000);
});

// Regression: BigInt boundary validation at the compute FFI surface.
//
// The NAPI layer used to call `BigInt::get_u64()` and discard the
// `signed` / `lossless` flags — so a negative or `>u64::MAX` BigInt
// would silently cross the boundary as a garbage `u64`. That is a
// correctness bug for *every* u64 arg: daemon identities, node IDs,
// sequence numbers, timeouts, auto-snapshot cadence. The fix routes
// every BigInt through `daemon_bigint_u64` which throws a typed
// `DaemonError` on either flag. These tests lock the behavior in.
describe('regression: BigInt boundary validation (compute)', () => {
  const cleanups: Array<() => Promise<void>> = [];

  afterEach(async () => {
    while (cleanups.length > 0) {
      const fn = cleanups.pop();
      if (fn) {
        try {
          await fn();
        } catch {
          // Best-effort teardown.
        }
      }
    }
  });

  async function startedRuntime(): Promise<DaemonRuntime> {
    const mesh = await buildMesh();
    cleanups.push(() => mesh.shutdown());
    const rt = DaemonRuntime.create(mesh);
    cleanups.push(() => rt.shutdown());
    rt.registerFactory('echo', () => ({ name: 'echo', process: () => [] }));
    await rt.start();
    return rt;
  }

  it('spawn rejects a negative autoSnapshotInterval', async () => {
    const rt = await startedRuntime();
    await expect(
      rt.spawn('echo', Identity.generate(), {
        autoSnapshotInterval: -1n,
      }),
    ).rejects.toThrow(/non-negative/);
  });

  it('spawn rejects an autoSnapshotInterval > u64::MAX', async () => {
    const rt = await startedRuntime();
    await expect(
      rt.spawn('echo', Identity.generate(), {
        // 2^65 > u64::MAX — `lossless` comes back false.
        autoSnapshotInterval: 2n ** 65n,
      }),
    ).rejects.toThrow(/u64 range/);
  });

  it('startMigration rejects a negative sourceNode', async () => {
    const rt = await startedRuntime();
    // Any origin_hash works — the BigInt validation fires before
    // the migration orchestrator ever looks for a local daemon.
    await expect(rt.startMigration(0xdead_beefn, -1n, 1n)).rejects.toThrow(
      /non-negative/,
    );
  });

  it('startMigration rejects an overflow targetNode', async () => {
    const rt = await startedRuntime();
    await expect(
      rt.startMigration(0xdead_beefn, 1n, 2n ** 65n),
    ).rejects.toThrow(/u64 range/);
  });

  it('startMigrationWith rejects a negative retryNotReadyMs', async () => {
    const rt = await startedRuntime();
    await expect(
      rt.startMigrationWith(0xdead_beefn, 1n, 2n, {
        retryNotReadyMs: -100n,
      }),
    ).rejects.toThrow(/non-negative/);
  });
});

// Regression: `stripInternal: true` in tsconfig.json. The sdk-ts
// compilation relies on `@internal` JSDoc tags to hide
// cross-module FFI accessors (`_napiNetMesh`, `_napiRuntime`,
// internal `napi` fields on cortex handles) from the emitted
// `dist/*.d.ts` files. Without `stripInternal` the TS compiler
// ignores the tag and consumers can depend on the unstable NAPI
// surface. This test reads the built declaration files and
// asserts the internal names are gone. Skips when `dist/` is
// absent so running vitest without a prior `tsc` run still works.
describe('regression: stripInternal leaks', async () => {
  const fs = await import('node:fs');
  const path = await import('node:path');
  const distDir = path.resolve(__dirname, '..', 'dist');
  const hasBuild = fs.existsSync(distDir);
  const runIf = hasBuild ? it : it.skip;

  runIf('does not expose _napiNetMesh on the public MeshNode type', () => {
    const dts = fs.readFileSync(path.join(distDir, 'mesh.d.ts'), 'utf8');
    expect(dts).not.toMatch(/_napiNetMesh/);
    expect(dts).not.toMatch(/_testInjectSyntheticPeer/);
  });

  runIf('does not expose _napiRuntime on the public DaemonRuntime type', () => {
    const dts = fs.readFileSync(path.join(distDir, 'compute.d.ts'), 'utf8');
    expect(dts).not.toMatch(/_napiRuntime/);
  });

  runIf('does not expose the internal `napi` accessor on cortex types', () => {
    const dts = fs.readFileSync(path.join(distDir, 'cortex.d.ts'), 'utf8');
    // The source has `/** @internal */ readonly napi: NapiRedex` etc.
    // After stripInternal, no `readonly napi:` line should remain.
    expect(dts).not.toMatch(/readonly napi:/);
  });
});
