// The public tool-serving cleanup journey, on a real mesh (U-1).
//
// The documented path is: `serveTool` → `handle.close()` →
// `rpc.raw.close()` → `mesh.shutdown()`. On 0.35 the last step rejected
// with `cannot shutdown: outstanding references exist`, because
// `serveTool` lazy-installs a `tool.metadata.fetch` nRPC registration
// that NO handle owns. The consumer closed everything they were handed
// and the node still would not go down.
//
// The unit witnesses in `tool.test.ts` pin the registration count against
// a fake rpc. This file is the one that proves the count corresponds to a
// real mesh reference — a fake `ServeHandle` cannot fail to release a node.
//
// No sleeps and no reliance on V8 finalization anywhere below. If the
// release is not explicit it is not a fix, and a `setTimeout` would hide
// exactly the difference under test.
//
// Gated behind RUN_INTEGRATION_TESTS: needs the built native binding.

import { describe, expect, it } from 'vitest'

import { serveTool, serveToolStreaming } from '../tool'
import type { ToolEvent } from '../tool'
import { TypedMeshRpc } from '../mesh_rpc'

// eslint-disable-next-line @typescript-eslint/no-explicit-any
const binding: any = await import('../index')
const { NetMesh } = binding

const RUN_INTEGRATION_TESTS = process.env.RUN_INTEGRATION_TESTS === '1'

// A PSK per node keeps a failure in one case from surfacing as a hang in
// another — same convention as `mesh_rpc_live.test.ts`.
let pskCounter = 0
const nextPsk = () => (pskCounter += 1).toString(16).padStart(2, '0').repeat(32)

// eslint-disable-next-line @typescript-eslint/no-explicit-any
async function meshNode(): Promise<any> {
  return NetMesh.create({ bindAddr: '127.0.0.1:0', psk: nextPsk() })
}

const echo = async (req: unknown) => req

async function* oneResult(): AsyncGenerator<ToolEvent> {
  yield { type: 'result', data: 'ok' }
}

describe.skipIf(!RUN_INTEGRATION_TESTS)('serveTool lifecycle (live)', () => {
  it('closing the last tool and the rpc lets the node shut down', async () => {
    const mesh = await meshNode()
    const rpc = TypedMeshRpc.fromMesh(mesh)
    const handle = serveTool(rpc, { name: 'echo' }, echo)

    // Control: the reference accounting is live. The rpc envelope alone
    // blocks shutdown, so a green final assertion below cannot be the
    // node simply never having been held.
    await expect(mesh.shutdown()).rejects.toThrow(/outstanding references/)

    handle.close()
    rpc.raw.close()

    // The witness. Before the repair, the `tool.metadata.fetch`
    // registration `serveTool` installed was still holding the node here.
    await expect(mesh.shutdown()).resolves.toBeUndefined()
  }, 30_000)

  it('a streaming tool releases the node the same way', async () => {
    const mesh = await meshNode()
    const rpc = TypedMeshRpc.fromMesh(mesh)
    const handle = serveToolStreaming(rpc, { name: 'streamer' }, oneResult)

    handle.close()
    rpc.raw.close()

    await expect(mesh.shutdown()).resolves.toBeUndefined()
  }, 30_000)

  it('the node stays held until the LAST of several tools is closed', async () => {
    const mesh = await meshNode()
    const rpc = TypedMeshRpc.fromMesh(mesh)
    const alpha = serveTool(rpc, { name: 'alpha' }, echo)
    const beta = serveToolStreaming(rpc, { name: 'beta' }, oneResult)

    alpha.close()
    rpc.raw.close()

    // `beta` is still served. Its registration — and the metadata service
    // it shares with `alpha` — must still hold the node.
    await expect(mesh.shutdown()).rejects.toThrow(/outstanding references/)

    beta.close()
    await expect(mesh.shutdown()).resolves.toBeUndefined()
  }, 30_000)

  it('a tool can be served again on a fresh rpc after a full teardown', async () => {
    const mesh = await meshNode()

    const first = TypedMeshRpc.fromMesh(mesh)
    serveTool(first, { name: 'echo' }, echo).close()
    first.raw.close()

    // Releasing the last tool deletes the per-rpc registry entry, so this
    // round has to install its own metadata service rather than reuse a
    // closed one. If it reused the corpse, the registration would be gone
    // and the node would shut down here regardless — so the real check is
    // that serving still works and teardown is still explicit.
    const second = TypedMeshRpc.fromMesh(mesh)
    const handle = serveTool(second, { name: 'echo' }, echo)
    await expect(mesh.shutdown()).rejects.toThrow(/outstanding references/)

    handle.close()
    second.raw.close()
    await expect(mesh.shutdown()).resolves.toBeUndefined()
  }, 30_000)
})
