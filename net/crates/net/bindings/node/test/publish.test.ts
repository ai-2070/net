// Local tool-publication binding tests (PAYMENTS_PY_TS_SDK_GAP_PLAN B5
// prerequisite). The Node twin of bindings/python/tests/test_publish.py:
// `NetMesh.publishTools` announces a node's OWN tools — backed by a JS async
// handler — and returns a live publication handle.
//
// The whole publish/announce/serve/merge machinery is single-sourced in
// `net_mcp::wrap::ServerPublisher` and proven cross-node by the Rust
// `publish_tools_end_to_end.rs` + the Python `test_publish.py` over the *same*
// path; the binding's job is marshaling (JS tool specs → SDK tools, a JS async
// handler → `ToolInvoker` via the TSFN→Promise bridge, and the publication
// handle). This suite asserts that Node surface: input validation and the
// publish → serving → withdraw/stop lifecycle on a single node.
//
// Present iff the .node was built with the `publish` feature (the vitest CI job
// is; the suite skips cleanly otherwise).

import { describe, expect, it } from 'vitest'

// eslint-disable-next-line @typescript-eslint/no-explicit-any
const binding: any = await import('../index')
const NetMesh = binding.NetMesh
// `publishTools` rides the `publish` feature; probe the prototype so the suite
// skips when the .node was built without it.
const HAS_PUBLISH = typeof NetMesh?.prototype?.publishTools === 'function'

const PSK = '5b'.repeat(32)

// The published tools ride dynamically-named service channels, so the node must
// opt out of channel-config ACL (`permissiveChannels`) — the Node analog of the
// Python test's `permissive_channels=True`.
// eslint-disable-next-line @typescript-eslint/no-explicit-any
async function withPublishingMesh(fn: (mesh: any) => Promise<void>): Promise<void> {
  const mesh = await NetMesh.create({ bindAddr: '127.0.0.1:0', psk: PSK, permissiveChannels: true })
  try {
    mesh.start()
    await fn(mesh)
  } finally {
    await mesh.shutdown()
  }
}

const ECHO = {
  name: 'echo',
  description: 'echoes its message',
  inputSchema: JSON.stringify({
    type: 'object',
    properties: { message: { type: 'string' } },
  }),
}

// A no-op handler — this suite exercises the publish lifecycle, not invocation
// (invocation is proven cross-node by the Rust/Python e2es).
const noopHandler = async (_args: { toolName: string; argumentsJson: string }) => ({
  text: 'ok',
})

describe.skipIf(!HAS_PUBLISH)('NetMesh.publishTools', () => {
  it('publishes a tool and reports it on the handle', async () => {
    await withPublishingMesh(async (mesh) => {
      const handle = await mesh.publishTools([ECHO], noopHandler)
      expect(handle.serving).toBe(true)
      // `echo` sanitizes to a channel-safe id; it must appear among the served
      // ids and nothing should be skipped (the name is non-empty).
      expect(handle.tools.length).toBe(1)
      expect(handle.skippedTools).toEqual([])
      await handle.withdraw()
    })
  }, 20000)

  it('withdraw is idempotent and flips serving to false', async () => {
    await withPublishingMesh(async (mesh) => {
      const handle = await mesh.publishTools([ECHO], noopHandler)
      expect(handle.serving).toBe(true)
      await handle.withdraw()
      expect(handle.serving).toBe(false)
      // A second withdraw is a no-op, never a throw.
      await handle.withdraw()
      expect(handle.serving).toBe(false)
      expect(handle.tools).toEqual([])
    })
  }, 20000)

  it('stop drops the publication without a round-trip', async () => {
    await withPublishingMesh(async (mesh) => {
      const handle = await mesh.publishTools([ECHO], noopHandler)
      expect(handle.serving).toBe(true)
      handle.stop()
      expect(handle.serving).toBe(false)
    })
  }, 20000)

  it('accepts publish options (version + allowAnyCaller)', async () => {
    await withPublishingMesh(async (mesh) => {
      const handle = await mesh.publishTools([ECHO], noopHandler, {
        version: '2.1',
        allowAnyCaller: true,
      })
      expect(handle.serving).toBe(true)
      await handle.stop()
    })
  }, 20000)

  it('a malformed inputSchema is a construction error, not a rejection', async () => {
    await withPublishingMesh(async (mesh) => {
      // The schema is validated synchronously before the publish round-trip, so
      // `publishTools` throws rather than returning a rejecting Promise.
      expect(() =>
        mesh.publishTools(
          [{ name: 'broken', inputSchema: 'not json' }],
          noopHandler,
        ),
      ).toThrow()
    })
  })

  it('a negative ownerOrigin is a construction error', async () => {
    await withPublishingMesh(async (mesh) => {
      expect(() =>
        mesh.publishTools([ECHO], noopHandler, { ownerOrigin: -1n }),
      ).toThrow()
    })
  })
})
