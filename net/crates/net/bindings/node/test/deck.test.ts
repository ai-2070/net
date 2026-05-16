// Tests for the Deck SDK operator-side bindings — Phase 5 slice 1.
//
// Requires the binding to have been built with the `deck` Cargo
// feature: `npm run build:debug` enables it by default per
// `package.json:scripts.build:debug`.

import { describe, expect, it } from 'vitest';

let symbols: Record<string, unknown> = {};
try {
  // eslint-disable-next-line @typescript-eslint/no-require-imports
  symbols = require('../index');
} catch {
  symbols = {};
}

const hasDeck =
  typeof symbols.DeckClient === 'function' &&
  typeof symbols.OperatorIdentity === 'function' &&
  typeof symbols.MeshOsDaemonSdk === 'function';

const d = hasDeck ? describe : describe.skip;

function parseKind(err: unknown): string | null {
  if (!(err instanceof Error)) return null;
  const m = err.message.match(/<<deck-sdk-kind:([^>]+)>>/);
  return m ? m[1] : null;
}

d('Deck SDK operator-side bindings (Phase 5 slice 1)', () => {
  const {
    DeckClient,
    OperatorIdentity,
    MeshOsDaemonSdk,
  } = symbols as {
    DeckClient: any;
    OperatorIdentity: any;
    MeshOsDaemonSdk: any;
  };

  // -------------------------------------------------------------------------
  // OperatorIdentity
  // -------------------------------------------------------------------------

  it('generate returns distinct operator ids', () => {
    const a = OperatorIdentity.generate();
    const b = OperatorIdentity.generate();
    expect(a.operatorId).not.toBe(b.operatorId);
  });

  it('fromSeed is deterministic', () => {
    const seed = Buffer.alloc(32, 0x42);
    const a = OperatorIdentity.fromSeed(seed);
    const b = OperatorIdentity.fromSeed(seed);
    expect(a.operatorId).toBe(b.operatorId);
  });

  it('fromSeed rejects wrong length with invalid_argument', () => {
    try {
      OperatorIdentity.fromSeed(Buffer.alloc(31, 0x42));
      throw new Error('expected throw');
    } catch (e) {
      expect(parseKind(e)).toBe('invalid_argument');
    }
  });

  // -------------------------------------------------------------------------
  // DeckClient construction
  // -------------------------------------------------------------------------

  it('constructs against a running MeshOsDaemonSdk', async () => {
    const sdk = await MeshOsDaemonSdk.start();
    try {
      const identity = OperatorIdentity.generate();
      const client = await DeckClient.fromMeshos(sdk, identity);
      const bound = client.identity();
      expect(bound.operatorId).toBe(identity.operatorId);
    } finally {
      await sdk.shutdown();
    }
  });

  it('rejects a shutdown MeshOsDaemonSdk with already_shutdown', async () => {
    const sdk = await MeshOsDaemonSdk.start();
    await sdk.shutdown();
    try {
      await DeckClient.fromMeshos(sdk, OperatorIdentity.generate());
      throw new Error('expected throw');
    } catch (e) {
      expect(parseKind(e)).toBe('already_shutdown');
    }
  });

  // -------------------------------------------------------------------------
  // status / statusSummary
  // -------------------------------------------------------------------------

  it('status() returns a JSON-encoded MeshOsSnapshot', async () => {
    const sdk = await MeshOsDaemonSdk.start();
    try {
      const client = await DeckClient.fromMeshos(sdk, OperatorIdentity.generate());
      const snap = client.status();
      expect(typeof snap).toBe('string');
      const parsed = JSON.parse(snap);
      expect(typeof parsed).toBe('object');
    } finally {
      await sdk.shutdown();
    }
  });

  it('statusSummary() returns a typed object with peer + daemon counts', async () => {
    const sdk = await MeshOsDaemonSdk.start();
    try {
      const client = await DeckClient.fromMeshos(sdk, OperatorIdentity.generate());
      const summary = client.statusSummary();
      expect(summary).toHaveProperty('peers.healthy');
      expect(summary).toHaveProperty('peers.degraded');
      expect(summary).toHaveProperty('peers.unreachable');
      expect(summary).toHaveProperty('peers.unknown');
      expect(summary).toHaveProperty('daemons.running');
      expect(summary).toHaveProperty('localMaintenanceActive');
    } finally {
      await sdk.shutdown();
    }
  });

  // -------------------------------------------------------------------------
  // AdminCommands — every method
  // -------------------------------------------------------------------------

  it('drain commits and returns a ChainCommit', async () => {
    const sdk = await MeshOsDaemonSdk.start();
    try {
      const identity = OperatorIdentity.generate();
      const client = await DeckClient.fromMeshos(sdk, identity);
      const commit = await client.admin.drain(0xABCDn, 60_000n);
      expect(commit.eventKind).toBe('drain');
      expect(commit.operatorId).toBe(identity.operatorId);
      expect(commit.commitId > 0n).toBe(true);
    } finally {
      await sdk.shutdown();
    }
  });

  it('enterMaintenance with and without deadline', async () => {
    const sdk = await MeshOsDaemonSdk.start();
    try {
      const client = await DeckClient.fromMeshos(sdk, OperatorIdentity.generate());
      const c1 = await client.admin.enterMaintenance(0x1234n);
      const c2 = await client.admin.enterMaintenance(0x5678n, 300_000n);
      expect(c1.eventKind).toBe('enter_maintenance');
      expect(c2.eventKind).toBe('enter_maintenance');
      expect(c2.commitId > c1.commitId).toBe(true);
    } finally {
      await sdk.shutdown();
    }
  });

  it('every admin method commits with the expected event_kind', async () => {
    const sdk = await MeshOsDaemonSdk.start();
    try {
      const client = await DeckClient.fromMeshos(sdk, OperatorIdentity.generate());
      const node = 0xCAFEn;
      const cases: Array<[string, Promise<{ eventKind: string; commitId: bigint }>]> = [
        ['drain', client.admin.drain(node, 1_000n)],
        ['enter_maintenance', client.admin.enterMaintenance(node)],
        ['exit_maintenance', client.admin.exitMaintenance(node)],
        ['cordon', client.admin.cordon(node)],
        ['uncordon', client.admin.uncordon(node)],
        ['drop_replicas', client.admin.dropReplicas(node, [0xDEADn, 0xBEEFn])],
        ['invalidate_placement', client.admin.invalidatePlacement(node)],
        ['restart_all_daemons', client.admin.restartAllDaemons(node)],
        ['clear_avoid_list', client.admin.clearAvoidList(node)],
      ];
      const results = await Promise.all(cases.map(async ([kind, p]) => [kind, await p] as const));
      for (const [kind, commit] of results) {
        expect(commit.eventKind).toBe(kind);
        expect(commit.commitId > 0n).toBe(true);
      }
    } finally {
      await sdk.shutdown();
    }
  });

  // -------------------------------------------------------------------------
  // Streams
  // -------------------------------------------------------------------------

  it('snapshots() yields parseable JSON strings', async () => {
    const sdk = await MeshOsDaemonSdk.start();
    try {
      const client = await DeckClient.fromMeshos(sdk, OperatorIdentity.generate());
      const stream = await client.snapshots();
      try {
        const ret = await stream.nextSnapshot();
        expect(typeof ret).toBe('string');
        const parsed = JSON.parse(ret as string);
        expect(typeof parsed).toBe('object');
      } finally {
        await stream.close();
      }
    } finally {
      await sdk.shutdown();
    }
  });

  it('statusSummaryStream() yields typed objects', async () => {
    const sdk = await MeshOsDaemonSdk.start();
    try {
      const client = await DeckClient.fromMeshos(sdk, OperatorIdentity.generate());
      const stream = await client.statusSummaryStream();
      try {
        const summary = await stream.nextSummary();
        expect(summary).not.toBeNull();
        expect(summary).toHaveProperty('peers');
        expect(summary).toHaveProperty('daemons');
      } finally {
        await stream.close();
      }
    } finally {
      await sdk.shutdown();
    }
  });

  it('closed snapshot stream returns null', async () => {
    const sdk = await MeshOsDaemonSdk.start();
    try {
      const client = await DeckClient.fromMeshos(sdk, OperatorIdentity.generate());
      const stream = await client.snapshots();
      await stream.close();
      const next = await stream.nextSnapshot();
      expect(next).toBeNull();
    } finally {
      await sdk.shutdown();
    }
  });
});
