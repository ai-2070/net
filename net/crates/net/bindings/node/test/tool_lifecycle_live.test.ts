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
// No reliance on V8 finalization anywhere below, and no sleep in any
// ASSERTION: if the release is not explicit it is not a fix, and a
// `setTimeout` before a shutdown check would hide exactly the difference
// under test. `peers()` does sleep, because a Noise handshake and a
// capability announce take the time they take — that is setup getting to
// a starting line, not a teardown being given time to look tidy.
//
// Gated behind RUN_INTEGRATION_TESTS: needs the built native binding.

import { describe, expect, it } from 'vitest'

import { fetchToolMetadata, serveTool, serveToolStreaming } from '../tool'
import type { ToolEvent } from '../tool'
import { TypedMeshRpc } from '../mesh_rpc'

// eslint-disable-next-line @typescript-eslint/no-explicit-any
const binding: any = await import('../index')
const { NetMesh } = binding

const RUN_INTEGRATION_TESTS = process.env.RUN_INTEGRATION_TESTS === '1'

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms))

// A PSK per node keeps a failure in one case from surfacing as a hang in
// another — same convention as `mesh_rpc_live.test.ts`. A connected pair
// needs both halves on the SAME psk, so `peers()` takes one and uses it
// twice; every other case still gets its own.
let pskCounter = 0
const nextPsk = () => (pskCounter += 1).toString(16).padStart(2, '0').repeat(32)

// eslint-disable-next-line @typescript-eslint/no-explicit-any
async function meshNode(psk: string = nextPsk()): Promise<any> {
  return NetMesh.create({ bindAddr: '127.0.0.1:0', psk })
}

/**
 * A connected, started, mutually-pinned pair.
 *
 * Lifted from `mesh_rpc_live.test.ts`'s `pair()`, including the two
 * non-obvious parts: `accept()` is started but NOT awaited before
 * `connect()` (the handshake needs both halves in flight), and both sides
 * announce capabilities so each TOFU-pins the other's entity id — the
 * serve bridge drops a REQUEST from an unpinned caller before it reaches
 * the fold, which presents as a call that never answers rather than as a
 * refusal.
 *
 * A pair is only needed where a call has to actually travel. A node
 * cannot call itself: the reply channel binds to this node's announced
 * identity and there is no session to itself, so `fetchToolMetadata` at
 * `mesh.nodeId()` comes back `nrpc:no_route`.
 */
// eslint-disable-next-line @typescript-eslint/no-explicit-any
async function peers(): Promise<{ host: any; agent: any; agentRpc: TypedMeshRpc }> {
  const psk = nextPsk()
  const host = await meshNode(psk)
  const agent = await meshNode(psk)

  const accepted = host.accept(agent.nodeId())
  await sleep(50)
  await agent.connect(host.localAddr(), host.publicKey(), host.nodeId())
  await accepted

  host.start()
  agent.start()
  await Promise.all([host.announceCapabilities({}), agent.announceCapabilities({})])
  await sleep(300)

  return { host, agent, agentRpc: TypedMeshRpc.fromMesh(agent) }
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

  it('a tool served again on the same rpc is still fetchable by a peer', async () => {
    const { host, agent, agentRpc } = await peers()
    const rpc = TypedMeshRpc.fromMesh(host)

    const first = serveTool(rpc, { name: 'echo' }, echo)
    await expect(
      fetchToolMetadata(agentRpc, host.nodeId(), 'echo'),
    ).resolves.toMatchObject({ type: 'found' })

    // Closing the last tool closes the shared metadata service and clears
    // the registry's handle to it. It has to be the SAME rpc: a fresh
    // `TypedMeshRpc` is a fresh WeakMap key, so re-serving on a new
    // envelope would install a service whatever the cleanup did or did
    // not do, and would witness nothing.
    first.close()

    const second = serveTool(rpc, { name: 'echo' }, echo)

    // The witness. If cleanup left the CLOSED `ServeHandle` in place,
    // `_ensureFetchInstalled` takes its early return on a truthy
    // `fetchHandle` and nothing is serving `tool.metadata.fetch` — the
    // tool is registered and its metadata is unreachable. Asking a real
    // peer is the only way to see that: the node's own shutdown
    // accounting cannot, because a closed handle holds no reference, and
    // the unit witnesses count registrations against a fake that cannot
    // fail to answer.
    //
    // Control: restore the closed handle instead of nulling it and this
    // assertion does not fail fast — it hangs to the 30s timeout, because
    // an unrouted request is indistinguishable from a slow one. Which is
    // the point: nothing on either side reports the defect.
    await expect(
      fetchToolMetadata(agentRpc, host.nodeId(), 'echo'),
    ).resolves.toMatchObject({ type: 'found' })

    second.close()
    rpc.raw.close()
    await expect(host.shutdown()).resolves.toBeUndefined()

    agentRpc.raw.close()
    await expect(agent.shutdown()).resolves.toBeUndefined()
  }, 30_000)
})
