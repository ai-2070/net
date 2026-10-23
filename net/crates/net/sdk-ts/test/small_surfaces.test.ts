// The small unwrapped items (NODE_SDK_GAPS_PLAN.md S6), through
// @net-mesh/sdk: the aggregator clients, WriteToken with the adapters'
// waitForToken, normalizeGpuVendor, and the MCP helpers in `./tool`.
//
// Imports are from the SDK root and `../src/tool` only: that is the
// witness. Nothing skips; CI's sdk-ts build compiles aggregator, cortex and
// mcp.

import { describe, expect, it } from 'vitest';

import {
  FoldQueryClient,
  FoldQueryClientError,
  MemoriesAdapter,
  MeshNode,
  Redex,
  RegistryClient,
  RegistryClientError,
  TasksAdapter,
  WriteToken,
  classifyAggregatorError,
  createFoldQueryClient,
  createRegistryClient,
  normalizeGpuVendor,
} from '../src/index';
import { classifyMcpServer, lowerMcpTool } from '../src/tool';

const PSK = '6c'.repeat(32);

describe('aggregator clients', () => {
  it('build from a MeshNode, and a failed call classifies to a typed error', async () => {
    const node = await MeshNode.create({ bindAddr: '127.0.0.1:0', psk: PSK });
    try {
      const registry = createRegistryClient(node);
      expect(registry).toBeInstanceOf(RegistryClient);
      const fold = createFoldQueryClient(node);
      expect(fold).toBeInstanceOf(FoldQueryClient);
      fold.close();

      // No such node: the call fails, and the SDK's classifier types it.
      let caught: unknown;
      try {
        await registry.withDeadline(200).list(0xdeadn);
      } catch (e) {
        caught = classifyAggregatorError(e, 'registry');
      }
      expect(caught).toBeInstanceOf(RegistryClientError);
      registry.close();
    } finally {
      await node.shutdown();
    }
  }, 15_000);

  it('close() releases the node, so shutdown succeeds while the client is alive', async () => {
    // The client held the node's Arc until V8 finalized it, so shutdown
    // failed with "outstanding references exist" (the MeshRpc.close
    // precedent).
    const node = await MeshNode.create({ bindAddr: '127.0.0.1:0', psk: PSK });
    let shutDown = false;
    try {
      const registry = createRegistryClient(node);
      const fold = createFoldQueryClient(node);
      const alias = registry.withDeadline(500);
      expect(registry.isClosed).toBe(false);

      registry.close();
      fold.close();
      // Aliases share state: closing one closes them all.
      expect(alias.isClosed).toBe(true);
      expect(fold.isClosed).toBe(true);
      await node.shutdown();
      shutDown = true;

      let caught: unknown;
      try {
        await alias.list(1n);
      } catch (e) {
        caught = classifyAggregatorError(e, 'registry');
      }
      expect(caught).toBeInstanceOf(RegistryClientError);
      expect((caught as RegistryClientError).kind).toBe('invalid-args');
      // The cache methods honour close() too.
      expect(() => fold.invalidateCache()).toThrow(/client is closed/);
    } finally {
      // An assertion that failed before the planned shutdown must not
      // leave the node bound for the rest of the worker.
      if (!shutDown) await node.shutdown().catch(() => {});
    }
  }, 15_000);

  // fold.query.v2 (AGGREGATOR_MULTI_HOST_PLAN.md Q4). Serving it is the
  // aggregator daemon's job, so the live check runs against a peer in the
  // shape of a pre-v2 daemon: one with no channel registry, as every
  // daemon before Q1 ran. The v2 call must come back as the typed
  // `server-too-old`, which proves the request reached the peer and its
  // NotFound answer was mapped.
  //
  // Why no registry: the reply channel is per service
  // (`<service>.replies.<origin>`). A peer with a registry lists only the
  // reply channels of services it serves, so the caller's reply-channel
  // subscribe for an unserved service is refused (`UnknownChannel`). The
  // call then fails as `transport` before any NotFound can come back.
  // Named-group and unknown-group answers are covered by the Rust tests
  // against real registries.
  it('queryGroupLatest against a pre-v2 (registry-less) peer is server-too-old', async () => {
    const a = await MeshNode.create({ bindAddr: '127.0.0.1:0', psk: PSK });
    const b = await MeshNode.create({
      bindAddr: '127.0.0.1:0',
      psk: PSK,
      permissiveChannels: true,
    });
    const fold = createFoldQueryClient(a).withDeadline(3_000).withTtl(0);
    try {
      await Promise.all([
        b.accept(a.nodeId()),
        (async () => {
          await new Promise((r) => setTimeout(r, 50));
          await a.connect(b.localAddr(), b.publicKey(), b.nodeId());
        })(),
      ]);
      await a.start();
      await b.start();

      for (const call of [
        () => fold.queryGroupLatest(b.nodeId(), 1, 'some-group'),
        () => fold.queryGroupSummarizeNow(b.nodeId(), 1),
      ]) {
        let caught: unknown;
        try {
          await call();
        } catch (e) {
          caught = classifyAggregatorError(e, 'fold-query');
        }
        expect(caught).toBeInstanceOf(FoldQueryClientError);
        expect((caught as FoldQueryClientError).kind).toBe('server-too-old');
      }
    } finally {
      fold.close();
      await a.shutdown().catch(() => {});
      await b.shutdown().catch(() => {});
    }
  }, 20_000);
});

describe('WriteToken', () => {
  it("waitForToken resolves once the adapter's fold has the write", async () => {
    const origin = 0x5eedn;
    const tasks = await TasksAdapter.open(new Redex(), origin);
    try {
      const seq = tasks.create(1n, 'write me', 1n);
      const token = tasks.token(seq);
      expect(token.originHash).toBe(origin);
      expect(token.channelHash).toBe(tasks.channelHash());
      expect(token.seq).toBe(seq);
      const parsed = WriteToken.fromString(token.toString());
      expect([parsed.originHash, parsed.channelHash, parsed.seq]).toEqual([
        origin,
        tasks.channelHash(),
        seq,
      ]);
      await tasks.waitForToken(token, 2_000);
      expect(tasks.count()).toBe(1);

      // A token from another origin is rejected at once.
      const hex = (n: bigint) => n.toString(16).padStart(16, '0');
      const foreign = WriteToken.fromString(`${hex(origin + 1n)}:${hex(tasks.channelHash())}:${seq}`);
      await expect(tasks.waitForToken(foreign, 2_000)).rejects.toThrow(/origin/);
      // The two-part form from before tokens carried a channel is refused.
      expect(() => WriteToken.fromString(`${hex(origin)}:${seq}`)).toThrow();
    } finally {
      tasks.close();
    }
  });

  it('a token from another channel is refused, even with the same origin', async () => {
    const origin = 0x5eedn;
    const redex = new Redex();
    const tasks = await TasksAdapter.open(redex, origin);
    const memories = await MemoriesAdapter.open(redex, origin);
    try {
      expect(tasks.channelHash()).not.toBe(memories.channelHash());
      let memSeq = 0n;
      for (let id = 0n; id < 3n; id++) memSeq = memories.store(id, 'm', [], 'src', 1n);
      await memories.waitForToken(memories.token(memSeq), 2_000);
      const taskSeq = tasks.create(1n, 't', 1n);
      expect(taskSeq <= memSeq).toBe(true);

      // Memories is past the task's seq; only the channel tells them apart.
      await expect(memories.waitForToken(tasks.token(taskSeq), 2_000)).rejects.toThrow(/channel/);
      await tasks.waitForToken(tasks.token(taskSeq), 2_000);
    } finally {
      tasks.close();
      memories.close();
    }
  });
});

describe('normalizeGpuVendor', () => {
  it('is the core normalizer', () => {
    expect(normalizeGpuVendor('NVIDIA')).toBe('nvidia');
    expect(normalizeGpuVendor('Apple')).toBe('apple');
    expect(normalizeGpuVendor('not-a-vendor')).toBe('unknown');
  });
});

describe('MCP helpers in ./tool', () => {
  it('classifyMcpServer never yields the ungated "none" from detection', () => {
    const label = classifyMcpServer('npx', ['some-mcp-server'], [{ key: 'API_KEY', value: 'x' }]);
    expect(['credentialed', 'external_api', 'unknown']).toContain(label);
  });

  it('lowerMcpTool lowers a tools/list entry to the discovery shape', () => {
    const tool = {
      name: 'read_file',
      description: 'Read a file',
      inputSchema: { type: 'object', properties: { path: { type: 'string' } } },
    };
    const lowered = lowerMcpTool(JSON.stringify(tool), '1.0.0', 'unknown');
    expect(lowered.mcpName).toBe('read_file');
    expect(lowered.toolId.length).toBeGreaterThan(0);
    expect(JSON.parse(lowered.descriptor)).toBeTypeOf('object');
  });
});
