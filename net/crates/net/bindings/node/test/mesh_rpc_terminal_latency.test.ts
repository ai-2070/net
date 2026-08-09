// The terminal frame must not wait for a garbage collection.
//
// Every streaming shape hands the JS handler a `JsResponseSink`, and
// the Rust side used to keep no reference to it — so the inner
// `RpcResponseSink`, whose drop tells the substrate fold the response
// side is finished, was owned solely by a `#[napi]` class. Those are
// released by V8 finalization, not by scope. The handler returned in
// 0 ms and the caller saw the terminal frame whenever a collection
// happened to run: measured at 7.6 s, 7.7 s and 15.8 s on loopback,
// and unmoved by mesh traffic, because GC is not driven by the mesh.
//
// It presented as a hang, not as latency: a `for await` drain past a
// 30 s test timeout. It also looked like cross-call interference,
// because only the first streaming call in a process paid the full
// price — later ones landed after a collection had already run.
//
// Chunk delivery was never affected, which is why this survived: a
// test that reads a known number of chunks passes. Only a drain to EOF
// sees it. These assert the latency directly so a regression reports
// as a number rather than as a timeout.

import { afterEach, describe, expect, it } from 'vitest'

// eslint-disable-next-line @typescript-eslint/no-explicit-any
const binding: any = await import('../index')
const { NetMesh, MeshRpc } = binding

// eslint-disable-next-line @typescript-eslint/no-explicit-any
type Any = any

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms))

/** Loopback with nothing else running; a collection is seconds away. */
const TERMINAL_BUDGET_MS = 500

let pskCounter = 0
const nextPsk = () => (pskCounter += 1).toString(16).padStart(2, '0').repeat(32)

const meshes: Any[] = []
const rpcs: Any[] = []

async function pair() {
  const psk = nextPsk()
  const server = await NetMesh.create({ bindAddr: '127.0.0.1:0', psk })
  const client = await NetMesh.create({ bindAddr: '127.0.0.1:0', psk })
  const accepted = server.accept(client.nodeId())
  await sleep(50)
  await client.connect(server.localAddr(), server.publicKey(), server.nodeId())
  await accepted
  server.start()
  client.start()
  await Promise.all([
    server.announceCapabilities({}),
    client.announceCapabilities({}),
  ])
  await sleep(300)
  const serverRpc = MeshRpc.fromMesh(server)
  const clientRpc = MeshRpc.fromMesh(client)
  // Retain both meshes. A `NetMesh` that becomes unreachable from JS
  // is finalized on GC and stops receiving, which looks exactly like
  // the defect these tests measure.
  meshes.push(server, client)
  rpcs.push(serverRpc, clientRpc)
  return { server, serverRpc, clientRpc }
}

afterEach(async () => {
  rpcs.splice(0).forEach((rpc) => rpc.close())
  await Promise.all(meshes.splice(0).map((m) => m.shutdown()))
})

describe('streaming terminal-frame latency', () => {
  it('server-streaming reaches EOF promptly', async () => {
    const { server, serverRpc, clientRpc } = await pair()
    const handle = serverRpc.serveStreaming(
      'lat.ss',
      async ([, sink]: [Buffer, Any]) => {
        sink.send(Buffer.from('x'))
        return Buffer.alloc(0)
      },
    )

    const stream = await clientRpc.callStreaming(
      server.nodeId(),
      'lat.ss',
      Buffer.from('go'),
    )
    const started = Date.now()
    const got: string[] = []
    for (let c = await stream.next(); c !== null; c = await stream.next()) {
      got.push(c.toString())
    }
    const elapsed = Date.now() - started

    expect(got).toEqual(['x'])
    expect(elapsed).toBeLessThan(TERMINAL_BUDGET_MS)
    handle.close()
  }, 60_000)

  it('duplex reaches EOF promptly', async () => {
    const { server, serverRpc, clientRpc } = await pair()
    const handle = serverRpc.serveDuplex(
      'lat.dx',
      async ([stream, sink]: [Any, Any]) => {
        for (let c = await stream.next(); c !== null; c = await stream.next()) {
          sink.send(Buffer.concat([Buffer.from('re:'), c]))
        }
        return Buffer.alloc(0)
      },
    )

    const call = await clientRpc.callDuplex(server.nodeId(), 'lat.dx')
    await call.send(Buffer.from('one'))
    await call.finishSending()

    const started = Date.now()
    const got: string[] = []
    for (let c = await call.next(); c !== null; c = await call.next()) {
      got.push(c.toString())
    }
    const elapsed = Date.now() - started

    expect(got).toEqual(['re:one'])
    expect(elapsed).toBeLessThan(TERMINAL_BUDGET_MS)
    await call.close()
    handle.close()
  }, 60_000)

  it('a handler that emits nothing still reaches EOF promptly', async () => {
    // The degenerate case, and the sharpest one: with no chunks at all
    // the terminal frame is the only thing the caller ever waits for.
    const { server, serverRpc, clientRpc } = await pair()
    const handle = serverRpc.serveStreaming(
      'lat.empty',
      async () => Buffer.alloc(0),
    )

    const stream = await clientRpc.callStreaming(
      server.nodeId(),
      'lat.empty',
      Buffer.from('go'),
    )
    const started = Date.now()
    let count = 0
    for (let c = await stream.next(); c !== null; c = await stream.next()) {
      count += 1
    }
    const elapsed = Date.now() - started

    expect(count).toBe(0)
    expect(elapsed).toBeLessThan(TERMINAL_BUDGET_MS)
    handle.close()
  }, 60_000)

  it('a handler that throws still releases the sink promptly', async () => {
    // The sink is dropped when the promise *settles*, either way. A
    // rejecting handler must not leave the caller waiting on a
    // collection for its error.
    const { server, serverRpc, clientRpc } = await pair()
    const handle = serverRpc.serveStreaming('lat.throw', async () => {
      throw new Error('handler exploded')
    })

    const stream = await clientRpc.callStreaming(
      server.nodeId(),
      'lat.throw',
      Buffer.from('go'),
    )
    const started = Date.now()
    // Either a thrown terminal status or a clean EOF is acceptable —
    // that it resolves at all, promptly, is the property.
    try {
      for (let c = await stream.next(); c !== null; c = await stream.next()) {
        /* drain */
      }
    } catch {
      // A terminal non-Ok status leaves the stream un-consumed, so it
      // still holds a node reference — release it or `afterEach`
      // cannot shut the mesh down.
      await stream.close()
    }
    expect(Date.now() - started).toBeLessThan(TERMINAL_BUDGET_MS)
    handle.close()
  }, 60_000)
})
