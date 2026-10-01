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

    let caught: unknown;
    try {
      await alias.list(1n);
    } catch (e) {
      caught = classifyAggregatorError(e, 'registry');
    }
    expect(caught).toBeInstanceOf(RegistryClientError);
    expect((caught as RegistryClientError).kind).toBe('invalid-args');
  }, 15_000);
});

describe('WriteToken', () => {
  it("waitForToken resolves once the adapter's fold has the write", async () => {
    const origin = 0x5eedn;
    const tasks = await TasksAdapter.open(new Redex(), origin);
    try {
      const seq = tasks.create(1n, 'write me', 1n);
      const token = new WriteToken(origin, seq);
      expect(WriteToken.fromString(token.toString()).seq).toBe(seq);
      await tasks.waitForToken(token, 2_000);
      expect(tasks.count()).toBe(1);

      // A token from another origin is rejected at once.
      await expect(tasks.waitForToken(new WriteToken(origin + 1n, seq), 2_000)).rejects.toThrow();
    } finally {
      tasks.close();
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
