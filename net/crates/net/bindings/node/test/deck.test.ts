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
  // Standalone constructor (operator-only mode — mirrors net_deck_client_new)
  // -------------------------------------------------------------------------

  it('DeckClient.new builds a standalone client from an operator seed', async () => {
    const seed = Buffer.alloc(32, 0x55);
    const client = await DeckClient.new(seed);
    try {
      const identity = client.identity();
      // Same seed must derive the same operator id as
      // OperatorIdentity.fromSeed.
      const reference = OperatorIdentity.fromSeed(seed);
      expect(identity.operatorId).toBe(reference.operatorId);
      // The supervisor is alive — status() returns a parseable
      // snapshot rather than throwing already_shutdown.
      const snap = client.status();
      expect(typeof snap).toBe('string');
      expect(typeof JSON.parse(snap)).toBe('object');
    } finally {
      // No explicit teardown yet (close() lands in a follow-up).
      // The supervisor releases on GC.
    }
  });

  it('DeckClient.new rejects a wrong-length seed with invalid_argument', async () => {
    try {
      await DeckClient.new(Buffer.alloc(31, 0x55));
      throw new Error('expected throw');
    } catch (e) {
      expect(parseKind(e)).toBe('invalid_argument');
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

  // -------------------------------------------------------------------------
  // Slice 2 — Audit query
  // -------------------------------------------------------------------------

  it('audit().collect() returns an array even on a fresh runtime', async () => {
    const sdk = await MeshOsDaemonSdk.start();
    try {
      const client = await DeckClient.fromMeshos(sdk, OperatorIdentity.generate());
      const q = client.audit();
      q.recent(100);
      const records = q.collect();
      expect(Array.isArray(records)).toBe(true);
      for (const r of records) {
        const parsed = JSON.parse(r);
        expect(typeof parsed).toBe('object');
      }
    } finally {
      await sdk.shutdown();
    }
  });

  it('audit ring eventually carries a record after admin commit', async () => {
    // The substrate folds admin commits on a tick (default
    // 500ms). Configure a fast tick + poll briefly.
    const sdk = await MeshOsDaemonSdk.start({ tickIntervalMs: 20n });
    try {
      const identity = OperatorIdentity.generate();
      const client = await DeckClient.fromMeshos(sdk, identity);
      await client.admin.cordon(0xCAFEn);
      const deadline = Date.now() + 2_000;
      let parsed: Array<Record<string, unknown>> = [];
      while (Date.now() < deadline) {
        const q = client.audit();
        q.recent(100);
        const raw = q.collect();
        if (raw.length > 0) {
          parsed = raw.map((s) => JSON.parse(s) as Record<string, unknown>);
          break;
        }
        await new Promise((r) => setTimeout(r, 50));
      }
      expect(parsed.length).toBeGreaterThan(0);
      const first = parsed[0];
      for (const key of ['seq', 'committed_at_ms', 'event', 'operator_ids', 'outcome']) {
        expect(first).toHaveProperty(key);
      }
    } finally {
      await sdk.shutdown();
    }
  });

  it('audit query accepts every filter combination', async () => {
    const sdk = await MeshOsDaemonSdk.start();
    try {
      const client = await DeckClient.fromMeshos(sdk, OperatorIdentity.generate());
      const q = client.audit();
      q.recent(10);
      q.byOperator(0x123n);
      q.between(0n, 2_000_000_000_000n);
      q.forceOnly();
      q.since(0n);
      const records = q.collect();
      expect(Array.isArray(records)).toBe(true);
    } finally {
      await sdk.shutdown();
    }
  });

  // -------------------------------------------------------------------------
  // Slice 2 — Log + Failure streams
  // -------------------------------------------------------------------------

  it('subscribeLogs() returns a stream with close support', async () => {
    const sdk = await MeshOsDaemonSdk.start();
    try {
      const client = await DeckClient.fromMeshos(sdk, OperatorIdentity.generate());
      const stream = await client.subscribeLogs();
      await stream.close();
    } finally {
      await sdk.shutdown();
    }
  });

  it('subscribeLogs() accepts a typed filter dict', async () => {
    const sdk = await MeshOsDaemonSdk.start();
    try {
      const client = await DeckClient.fromMeshos(sdk, OperatorIdentity.generate());
      const stream = await client.subscribeLogs({
        minLevel: 'warn',
        sinceSeq: 0n,
      });
      await stream.close();
    } finally {
      await sdk.shutdown();
    }
  });

  it('subscribeLogs() rejects invalid level with invalid_log_level kind', async () => {
    const sdk = await MeshOsDaemonSdk.start();
    try {
      const client = await DeckClient.fromMeshos(sdk, OperatorIdentity.generate());
      try {
        await client.subscribeLogs({ minLevel: 'verbose' as never });
        throw new Error('expected throw');
      } catch (e) {
        expect(parseKind(e)).toBe('invalid_log_level');
      }
    } finally {
      await sdk.shutdown();
    }
  });

  it('subscribeFailures() returns a stream with close support', async () => {
    const sdk = await MeshOsDaemonSdk.start();
    try {
      const client = await DeckClient.fromMeshos(sdk, OperatorIdentity.generate());
      const stream = await client.subscribeFailures(0n);
      await stream.close();
    } finally {
      await sdk.shutdown();
    }
  });

  it('subscribeFailures() defaults sinceSeq to 0n', async () => {
    const sdk = await MeshOsDaemonSdk.start();
    try {
      const client = await DeckClient.fromMeshos(sdk, OperatorIdentity.generate());
      const stream = await client.subscribeFailures();
      await stream.close();
    } finally {
      await sdk.shutdown();
    }
  });

  // -------------------------------------------------------------------------
  // Slice 3 — ICE break-glass surface
  // -------------------------------------------------------------------------

  it('all 7 ice factories return IceProposal instances', async () => {
    const sdk = await MeshOsDaemonSdk.start();
    try {
      const client = await DeckClient.fromMeshos(sdk, OperatorIdentity.generate());
      const ice = client.ice;
      const proposals = [
        ice.freezeCluster(60_000n),
        ice.flushAvoidLists({ kind: 'global', node: undefined, peer: undefined }),
        ice.forceEvictReplica(1n, 2n),
        ice.forceRestartDaemon(3n, 'echo'),
        ice.forceCutover(4n, 5n),
        ice.killMigration(6n),
        ice.thawCluster(),
      ];
      for (const p of proposals) {
        expect(p.issuedAtMs > 0n).toBe(true);
      }
    } finally {
      await sdk.shutdown();
    }
  });

  it('ice proposal exposes simulate but NOT commit (typestate)', async () => {
    const sdk = await MeshOsDaemonSdk.start();
    try {
      const client = await DeckClient.fromMeshos(sdk, OperatorIdentity.generate());
      const proposal = client.ice.freezeCluster(60_000n);
      expect(typeof (proposal as any).simulate).toBe('function');
      expect((proposal as any).commit).toBeUndefined();
    } finally {
      await sdk.shutdown();
    }
  });

  it('flushAvoidLists accepts all three AvoidScope variants', async () => {
    const sdk = await MeshOsDaemonSdk.start();
    try {
      const client = await DeckClient.fromMeshos(sdk, OperatorIdentity.generate());
      client.ice.flushAvoidLists({ kind: 'global', node: undefined, peer: undefined });
      client.ice.flushAvoidLists({ kind: 'local', node: 0xCAFEn, peer: undefined });
      client.ice.flushAvoidLists({ kind: 'onPeer', node: undefined, peer: 0xBEEFn });
    } finally {
      await sdk.shutdown();
    }
  });

  it('invalid avoid scope kind throws invalid_avoid_scope', async () => {
    const sdk = await MeshOsDaemonSdk.start();
    try {
      const client = await DeckClient.fromMeshos(sdk, OperatorIdentity.generate());
      try {
        client.ice.flushAvoidLists({ kind: 'nonsense' as never, node: undefined, peer: undefined });
        throw new Error('expected throw');
      } catch (e) {
        expect(parseKind(e)).toBe('invalid_avoid_scope');
      }
    } finally {
      await sdk.shutdown();
    }
  });

  it('simulate() advances to SimulatedIceProposal with blast radius + hash', async () => {
    const sdk = await MeshOsDaemonSdk.start();
    try {
      const client = await DeckClient.fromMeshos(sdk, OperatorIdentity.generate());
      const proposal = client.ice.freezeCluster(60_000n);
      const simulated = await proposal.simulate();
      // Typestate: simulated has commit + blast radius + blast hash.
      expect(typeof (simulated as any).commit).toBe('function');
      expect(typeof (simulated as any).blastRadius).toBe('function');
      expect(typeof (simulated as any).blastHash).toBe('function');
      const blast = JSON.parse(await simulated.blastRadius());
      expect(typeof blast).toBe('object');
      const hash = await simulated.blastHash();
      expect(hash.length).toBe(32);
      expect(simulated.issuedAtMs > 0n).toBe(true);
    } finally {
      await sdk.shutdown();
    }
  });

  it('double simulate throws already_simulated', async () => {
    const sdk = await MeshOsDaemonSdk.start();
    try {
      const client = await DeckClient.fromMeshos(sdk, OperatorIdentity.generate());
      const proposal = client.ice.freezeCluster(60_000n);
      await proposal.simulate();
      try {
        await proposal.simulate();
        throw new Error('expected throw');
      } catch (e) {
        expect(parseKind(e)).toBe('already_simulated');
      }
    } finally {
      await sdk.shutdown();
    }
  });

  it('commit with empty signatures fails with insufficient_signatures', async () => {
    const sdk = await MeshOsDaemonSdk.start();
    try {
      const client = await DeckClient.fromMeshos(sdk, OperatorIdentity.generate());
      const proposal = client.ice.freezeCluster(60_000n);
      const simulated = await proposal.simulate();
      try {
        await simulated.commit([]);
        throw new Error('expected throw');
      } catch (e) {
        expect(parseKind(e)).toBe('insufficient_signatures');
      }
    } finally {
      await sdk.shutdown();
    }
  });

  it('commit consumes the simulated proposal — second commit throws already_committed', async () => {
    const sdk = await MeshOsDaemonSdk.start();
    try {
      const client = await DeckClient.fromMeshos(sdk, OperatorIdentity.generate());
      const proposal = client.ice.freezeCluster(60_000n);
      const simulated = await proposal.simulate();
      const sig = {
        operatorId: 1n,
        signature: Buffer.alloc(64, 0),
      };
      // First commit — default threshold=1, no OperatorRegistry,
      // so substrate publishes via unsigned admin path.
      const c = await simulated.commit([sig]);
      expect(c.eventKind).toBe('freeze_cluster');
      // Second commit — proposal consumed.
      try {
        await simulated.commit([sig]);
        throw new Error('expected throw');
      } catch (e) {
        expect(parseKind(e)).toBe('already_committed');
      }
    } finally {
      await sdk.shutdown();
    }
  });

  // -------------------------------------------------------------------------
  // Operator-policy verifier surface: OperatorRegistry,
  // AdminVerifier, OperatorIdentity.signProposal / signPayload /
  // publicKey, SimulatedIceProposal.signingPayload.
  // -------------------------------------------------------------------------

  const hasVerifier =
    typeof symbols.OperatorRegistry === 'function' &&
    typeof symbols.AdminVerifier === 'function';
  const v = hasVerifier ? it : it.skip;

  v('OperatorRegistry lifecycle: register, insert, contains, size', () => {
    const { OperatorRegistry } = symbols as { OperatorRegistry: any };
    const reg = new OperatorRegistry();
    expect(reg.size).toBe(0);
    expect(reg.isEmpty()).toBe(true);
    const a = OperatorIdentity.generate();
    const b = OperatorIdentity.generate();
    reg.register(a);
    reg.insert(b.operatorId, b.publicKey());
    expect(reg.size).toBe(2);
    expect(reg.contains(a.operatorId)).toBe(true);
    expect(reg.contains(b.operatorId)).toBe(true);
    expect(reg.contains(0xDEADBEEFn)).toBe(false);
  });

  v('OperatorRegistry rejects bad public key length', () => {
    const { OperatorRegistry } = symbols as { OperatorRegistry: any };
    const reg = new OperatorRegistry();
    try {
      reg.insert(1n, Buffer.alloc(31));
      throw new Error('expected throw');
    } catch (e) {
      expect(parseKind(e)).toBe('invalid_public_key');
    }
  });

  v('signPayload + registry.verify round-trip any byte payload', () => {
    const { OperatorRegistry } = symbols as { OperatorRegistry: any };
    const identity = OperatorIdentity.generate();
    const reg = new OperatorRegistry();
    reg.register(identity);

    const payload = Buffer.from('verify-roundtrip-canary');
    const sig = identity.signPayload(payload);
    expect(sig.operatorId).toBe(identity.operatorId);
    expect(sig.signature.length).toBe(64);
    reg.verify(sig, payload); // no throw

    try {
      reg.verify(sig, Buffer.concat([payload, Buffer.from('!')]));
      throw new Error('expected throw');
    } catch (e) {
      expect(parseKind(e)).toBe('signature_invalid');
    }
  });

  v('registry.verify rejects unknown operator id', () => {
    const { OperatorRegistry } = symbols as { OperatorRegistry: any };
    const reg = new OperatorRegistry();
    const stranger = OperatorIdentity.generate();
    const sig = stranger.signPayload(Buffer.from('hello'));
    try {
      reg.verify(sig, Buffer.from('hello'));
      throw new Error('expected throw');
    } catch (e) {
      expect(parseKind(e)).toBe('not_authorized');
    }
  });

  v('verifyBundle enforces distinct-operator dedup', () => {
    const { OperatorRegistry } = symbols as { OperatorRegistry: any };
    const a = OperatorIdentity.generate();
    const b = OperatorIdentity.generate();
    const reg = new OperatorRegistry();
    reg.register(a);
    reg.register(b);
    const payload = Buffer.from('bundle-payload');
    const sigA = a.signPayload(payload);
    const sigB = b.signPayload(payload);

    reg.verifyBundle([sigA, sigB], payload, 2);

    try {
      reg.verifyBundle([sigA, sigA], payload, 2);
      throw new Error('expected throw');
    } catch (e) {
      expect(parseKind(e)).toBe('insufficient_signatures');
    }
  });

  v('AdminVerifier exposes policy knobs', () => {
    const { OperatorRegistry, AdminVerifier } = symbols as {
      OperatorRegistry: any;
      AdminVerifier: any;
    };
    const reg = new OperatorRegistry();
    const v1 = new AdminVerifier(reg, 3);
    expect(v1.threshold).toBe(3);
    expect(v1.freshnessWindowMs).toBe(300_000n);
    expect(v1.futureSkewMs).toBe(30_000n);
    expect(v1.iceCooldownMs).toBe(300_000n);

    const v2 = AdminVerifier.withFreshness(reg, 2, 60_000n, 5_000n);
    expect(v2.threshold).toBe(2);
    expect(v2.freshnessWindowMs).toBe(60_000n);
    expect(v2.futureSkewMs).toBe(5_000n);
    expect(v2.iceCooldownMs).toBe(300_000n);

    const v3 = AdminVerifier.withFullPolicy(reg, 1, 1_000n, 500n, 250n);
    expect(v3.threshold).toBe(1);
    expect(v3.iceCooldownMs).toBe(250n);
  });

  v('AdminVerifier clamps zero threshold to one', () => {
    const { OperatorRegistry, AdminVerifier } = symbols as {
      OperatorRegistry: any;
      AdminVerifier: any;
    };
    const v = new AdminVerifier(new OperatorRegistry(), 0);
    expect(v.threshold).toBe(1);
  });

  v('signingPayload matches signProposal payload byte-for-byte', async () => {
    const { OperatorRegistry } = symbols as { OperatorRegistry: any };
    const sdk = await MeshOsDaemonSdk.start();
    try {
      const identity = OperatorIdentity.generate();
      const client = await DeckClient.fromMeshos(sdk, identity);
      const proposal = client.ice.freezeCluster(60_000n);
      const simulated = await proposal.simulate();
      const payload = await simulated.signingPayload();
      // ICE_SIGNING_DOMAIN prefix — substrate const
      // `b"net.meshos.ice.v1\0"` (17 ASCII chars + trailing NUL,
      // 18 bytes total). Comparing the first 17 bytes keeps the
      // test source ASCII-clean; assert the 18th byte separately.
      expect(payload.subarray(0, 17).toString('latin1')).toBe(
        'net.meshos.ice.v1',
      );
      expect(payload[17]).toBe(0);

      const sigViaProposal = await identity.signProposal(simulated);
      const sigViaPayload = identity.signPayload(payload);
      expect(sigViaProposal.operatorId).toBe(sigViaPayload.operatorId);
      expect(Buffer.from(sigViaProposal.signature).equals(sigViaPayload.signature)).toBe(true);

      const reg = new OperatorRegistry();
      reg.register(identity);
      reg.verify(sigViaProposal, payload); // no throw
    } finally {
      await sdk.shutdown();
    }
  });

  v('signingPayload after commit throws already_committed', async () => {
    const sdk = await MeshOsDaemonSdk.start();
    try {
      const identity = OperatorIdentity.generate();
      const client = await DeckClient.fromMeshos(sdk, identity);
      const proposal = client.ice.freezeCluster(60_000n);
      const simulated = await proposal.simulate();
      await simulated.commit([
        { operatorId: 1n, signature: Buffer.alloc(64, 0) },
      ]);
      try {
        await simulated.signingPayload();
        throw new Error('expected throw');
      } catch (e) {
        expect(parseKind(e)).toBe('already_committed');
      }
      try {
        await identity.signProposal(simulated);
        throw new Error('expected throw');
      } catch (e) {
        expect(parseKind(e)).toBe('already_committed');
      }
    } finally {
      await sdk.shutdown();
    }
  });
});
