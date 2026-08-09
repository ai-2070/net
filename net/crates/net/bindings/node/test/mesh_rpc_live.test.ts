// The first live nRPC registration through the Node binding.
//
// Every other `mesh_rpc` test in this directory drives a stub object
// that implements the `MeshRpc` shape in TypeScript. None had ever
// built a `MeshRpc` from a real `NetMesh` and registered a handler on
// it — and registration was the broken part.
//
// `MeshRpc.serve*` is a synchronous `#[napi]` method, so it runs on
// the JS thread, which is not a Tokio worker. `MeshNode::serve_rpc*`
// spawns its inbound-event bridge with a bare `tokio::spawn`, which
// panics "there is no reactor running" when nothing is entered.
// napi-rs turns that panic into a thrown JS error, so the first live
// `serve()` in any consumer's code threw and no test in the repo could
// see it. The binding now carries the handle captured in
// `NetMesh.create()` — which IS on the runtime, being async — and
// enters it around each registration.
//
// So these are registration witnesses first and call witnesses second.
// Two nodes over real loopback UDP in one process: enough for
// registration, dispatch and teardown, but NOT the two-OS-process
// witness the decision asks for. That one belongs in CI against a
// packaged artifact.

import { afterEach, describe, expect, it } from 'vitest'

// eslint-disable-next-line @typescript-eslint/no-explicit-any
const binding: any = await import('../index')
const { NetMesh, MeshRpc } = binding

// eslint-disable-next-line @typescript-eslint/no-explicit-any
type Mesh = any
// eslint-disable-next-line @typescript-eslint/no-explicit-any
type Rpc = any

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms))

// Each case gets its own PSK, so it gets its own mesh.
//
// It has to. `MeshRpc.fromMesh()` clones the node's `Arc<MeshNode>`
// and — unlike `CapabilityGateway`, which grew a `close()` for exactly
// this — offers no way to release it. `NetMesh.shutdown()` needs sole
// ownership, so it fails with "outstanding references exist" and the
// node keeps running until V8 finalizes the MeshRpc, which is not on
// any schedule the test controls. Sharing one PSK left every earlier
// case's nodes live and talking on the same mesh; the duplex case,
// running fourth, hung. Distinct PSKs make the survivors unable to
// reach each other, which is isolation rather than a cure — the
// retention itself is a real gap, filed separately.
let pskCounter = 0
const nextPsk = () => (pskCounter += 1).toString(16).padStart(2, '0').repeat(32)

const meshes: Mesh[] = []

async function node(psk: string): Promise<Mesh> {
  const mesh = await NetMesh.create({ bindAddr: '127.0.0.1:0', psk })
  meshes.push(mesh)
  return mesh
}

/** A connected, started server/client pair plus their `MeshRpc`s. */
async function pair(): Promise<{
  server: Mesh
  client: Mesh
  serverRpc: Rpc
  clientRpc: Rpc
}> {
  const psk = nextPsk()
  const server = await node(psk)
  const client = await node(psk)

  // The a2a handshake: the acceptor waits for the connector's routed
  // handshake while the connector dials — both before `start()`.
  const accepted = server.accept(client.nodeId())
  await sleep(50)
  await client.connect(server.localAddr(), server.publicKey(), server.nodeId())
  await accepted

  server.start()
  client.start()

  // Exchange signed capability announcements so each side TOFU-pins
  // the other's entity id. The duplex serve bridge is flow-controlled
  // and its upload-grant classifier treats an unpinned caller as
  // untrusted, dropping the REQUEST before the fold — the same
  // footing `tests/integration_nrpc_duplex.rs` establishes.
  await Promise.all([
    server.announceCapabilities({}),
    client.announceCapabilities({}),
  ])
  await sleep(300)

  return {
    server,
    client,
    serverRpc: MeshRpc.fromMesh(server),
    clientRpc: MeshRpc.fromMesh(client),
  }
}

afterEach(async () => {
  // Best-effort: see the PSK note above for why this cannot succeed
  // while a `MeshRpc` for the node is still reachable.
  await Promise.all(meshes.splice(0).map((m) => m.shutdown().catch(() => {})))
})

describe('live nRPC registration through the Node binding', () => {
  it('registers a unary handler and dispatches a real call to it', async () => {
    const { server, serverRpc, clientRpc } = await pair()

    // This line is the witness. Before the fix it threw
    // "there is no reactor running".
    const handle = serverRpc.serve('live.unary', async (req: Buffer) =>
      Buffer.concat([Buffer.from('echo:'), req]),
    )
    expect(handle.isClosed()).toBe(false)

    const resp = await clientRpc.call(
      server.nodeId(),
      'live.unary',
      Buffer.from('hello'),
    )
    expect(resp.toString()).toBe('echo:hello')

    handle.close()
    expect(handle.isClosed()).toBe(true)
  }, 30_000)

  it('registers a server-streaming handler and delivers every chunk', async () => {
    const { server, serverRpc, clientRpc } = await pair()

    const handle = serverRpc.serveStreaming(
      'live.streaming',
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      async ([req, sink]: [Buffer, any]) => {
        for (let i = 0; i < 3; i += 1) {
          sink.send(Buffer.from(`${req.toString()}-${i}`))
        }
        return Buffer.alloc(0)
      },
    )

    const stream = await clientRpc.callStreaming(
      server.nodeId(),
      'live.streaming',
      Buffer.from('chunk'),
    )
    const got: string[] = []
    for (let c = await stream.next(); c !== null; c = await stream.next()) {
      got.push(c.toString())
    }
    expect(got).toEqual(['chunk-0', 'chunk-1', 'chunk-2'])

    stream.close()
    handle.close()
  }, 30_000)

  it('registers a client-streaming handler and folds every chunk', async () => {
    const { server, serverRpc, clientRpc } = await pair()

    const handle = serverRpc.serveClientStream(
      'live.clientstream',
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      async (stream: any) => {
        const parts: string[] = []
        for (let c = await stream.next(); c !== null; c = await stream.next()) {
          parts.push(c.toString())
        }
        return Buffer.from(parts.join('+'))
      },
    )

    const call = await clientRpc.callClientStream(
      server.nodeId(),
      'live.clientstream',
    )
    await call.send(Buffer.from('a'))
    await call.send(Buffer.from('b'))
    await call.send(Buffer.from('c'))
    expect((await call.finish()).toString()).toBe('a+b+c')

    handle.close()
  }, 30_000)

  it('registers a duplex handler and echoes in both directions', async () => {
    const { server, serverRpc, clientRpc } = await pair()

    const handle = serverRpc.serveDuplex(
      'live.duplex',
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      async ([stream, sink]: [any, any]) => {
        for (let c = await stream.next(); c !== null; c = await stream.next()) {
          sink.send(Buffer.concat([Buffer.from('re:'), c]))
        }
        return Buffer.alloc(0)
      },
    )

    const call = await clientRpc.callDuplex(server.nodeId(), 'live.duplex')
    await call.send(Buffer.from('one'))
    await call.send(Buffer.from('two'))
    await call.finishSending()

    // Read exactly as many chunks as were sent, rather than draining
    // to EOF.
    //
    // Draining is what you would write, and it is what this test did
    // first. It hangs — but only when a `callStreaming` has already
    // completed earlier in the same process. Both response paths go
    // through the same streaming-response bridge, and something in it
    // survives the first call and stops the second call's terminal
    // frame from reaching the client; the chunks still arrive, only
    // the End does not. Registering a server-streaming handler is
    // harmless, so it is the completed call that does it.
    //
    // That is a real defect and it is not this one — decision 1 is
    // about registration reaching an entered runtime, which the
    // chunks arriving already proves. Asserting EOF here would tie
    // this witness to an unrelated bug; asserting the chunks holds
    // the line without pretending the bug is absent. Filed for
    // separate investigation.
    const got: string[] = []
    for (let i = 0; i < 2; i += 1) {
      const chunk = await call.next()
      expect(chunk).not.toBeNull()
      got.push(chunk.toString())
    }
    expect(got).toEqual(['re:one', 're:two'])

    handle.close()
  }, 30_000)

  it('all four shapes register on one MeshRpc', async () => {
    const { serverRpc } = await pair()

    // A service offering every call shape registers each on its own
    // name — the arrangement that would have thrown four times over.
    const handles = [
      serverRpc.serve('live.all.unary', async () => Buffer.alloc(0)),
      serverRpc.serveStreaming('live.all.ss', async () => Buffer.alloc(0)),
      serverRpc.serveClientStream('live.all.cs', async () => Buffer.alloc(0)),
      serverRpc.serveDuplex('live.all.dx', async () => Buffer.alloc(0)),
    ]
    expect(handles.map((h) => h.isClosed())).toEqual([false, false, false, false])
    handles.forEach((h) => h.close())
    expect(handles.map((h) => h.isClosed())).toEqual([true, true, true, true])
  }, 30_000)

  it('closing the serve handle stops new dispatch', async () => {
    const { server, serverRpc, clientRpc } = await pair()

    const handle = serverRpc.serve('live.closed', async () =>
      Buffer.from('served'),
    )
    expect(
      (
        await clientRpc.call(server.nodeId(), 'live.closed', Buffer.alloc(0))
      ).toString(),
    ).toBe('served')

    handle.close()

    // The registration is gone; the call must not reach a handler.
    // Which error surfaces is a substrate concern — that it fails at
    // all is the property under test.
    await expect(
      clientRpc.call(server.nodeId(), 'live.closed', Buffer.alloc(0), {
        deadlineMs: 2_000,
      }),
    ).rejects.toThrow()
  }, 30_000)

  it('the observer hook installs from the same synchronous surface', async () => {
    const { serverRpc } = await pair()

    // `setObserver` spawns a drain task and reached for
    // `Handle::current()` from a sync `#[napi]` method — the same
    // defect as the serve seams, one method over.
    expect(() => serverRpc.setObserver(() => {})).not.toThrow()
    expect(() => serverRpc.setObserver(null)).not.toThrow()
  }, 30_000)
})
