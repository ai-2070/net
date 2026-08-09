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
// `MeshRpc.close()` now releases the node reference, so `shutdown()`
// succeeds and nothing survives a case — but the isolation stays. It
// costs nothing, and it keeps a failure in one case from showing up as
// a hang in another, which is how the duplex defect below first
// presented.
let pskCounter = 0
const nextPsk = () => (pskCounter += 1).toString(16).padStart(2, '0').repeat(32)

const meshes: Mesh[] = []
const rpcs: Rpc[] = []

/// Set by a case that knowingly leaves a node reference behind.
///
/// `afterEach` asserts shutdown succeeds, because a shutdown that
/// silently cannot run leaves live nodes behind — which is what made
/// this suite order-dependent before `MeshRpc.close()` existed. One
/// case opts out, and says why at the opt-out.
let tolerateShutdownFailure = false

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

  const serverRpc = MeshRpc.fromMesh(server)
  const clientRpc = MeshRpc.fromMesh(client)
  rpcs.push(serverRpc, clientRpc)

  return { server, client, serverRpc, clientRpc }
}

afterEach(async () => {
  // Every `MeshRpc` must be closed before its mesh can shut down: the
  // envelope holds an `Arc<MeshNode>` and `shutdown()` needs sole
  // ownership.
  rpcs.splice(0).forEach((rpc) => rpc.close())
  const tolerate = tolerateShutdownFailure
  tolerateShutdownFailure = false
  await Promise.all(
    meshes
      .splice(0)
      .map((m) => (tolerate ? m.shutdown().catch(() => {}) : m.shutdown())),
  )
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
    // completed earlier in the same process. The chunks still arrive;
    // only the terminal frame does not.
    //
    // Narrowed since: registering a server-streaming handler is
    // harmless, so it is the completed call. A completed
    // `callClientStream` before a duplex call is also harmless, so it
    // is specifically the server-streaming response path. And the
    // substrate is not at fault — `a_completed_streaming_call_does_not
    // _disturb_a_later_duplex` in `tests/integration_nrpc_duplex.rs`
    // runs the same sequence against core, with one node pair and with
    // two, and passes. That leaves this binding layer.
    //
    // Not fixed, and not this decision's subject: decision 1 is about
    // registration reaching an entered runtime, which the chunks
    // arriving already proves. Asserting EOF here would tie the
    // witness to an unrelated defect; asserting the chunks holds the
    // line without pretending the defect is absent.
    const got: string[] = []
    for (let i = 0; i < 2; i += 1) {
      const chunk = await call.next()
      expect(chunk).not.toBeNull()
      got.push(chunk.toString())
    }
    expect(got).toEqual(['re:one', 're:two'])

    await call.close()
    handle.close()

    // Even so, this case cannot shut its nodes down.
    //
    // `MeshRpc.close()` releases the envelope's reference and the call
    // above releases its own, but something on the server side of an
    // un-drained duplex call still holds one — most likely the
    // handler's `JsRequestStream` / `JsResponseSink`, which are
    // arguments the caller never owns and so has no way to release.
    // Every other case here shuts down cleanly, so the strict
    // assertion stays on for them; this one opts out rather than
    // pretending the reference is gone.
    //
    // It is downstream of the same un-drained call the note above is
    // about. Both belong to the duplex-termination investigation.
    tolerateShutdownFailure = true
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

  it('close releases the node so the mesh can shut down', async () => {
    const { server, client, serverRpc, clientRpc } = await pair()

    // The envelope holds an `Arc<MeshNode>`; `shutdown()` needs sole
    // ownership. Before `close()` existed there was no way to give it
    // up, and this rejected until V8 finalized the class.
    await expect(server.shutdown()).rejects.toThrow(/outstanding references/)

    expect(serverRpc.isClosed).toBe(false)
    serverRpc.close()
    expect(serverRpc.isClosed).toBe(true)
    serverRpc.close() // idempotent

    await expect(server.shutdown()).resolves.toBeUndefined()

    clientRpc.close()
    await client.shutdown()
    // Already drained; keep afterEach from double-shutting them.
    meshes.length = 0
    rpcs.length = 0
  }, 30_000)

  it('a closed MeshRpc refuses work instead of using a stale node', async () => {
    const { server, serverRpc, clientRpc } = await pair()
    const handle = serverRpc.serve('live.after.close', async () =>
      Buffer.from('ok'),
    )

    clientRpc.close()

    expect(() => clientRpc.reserveCancelToken()).toThrow(/nrpc:closed/)
    expect(() => clientRpc.findServiceNodes('live.after.close')).toThrow(
      /nrpc:closed/,
    )
    expect(() => clientRpc.metricsSnapshot()).toThrow(/nrpc:closed/)
    await expect(
      clientRpc.call(server.nodeId(), 'live.after.close', Buffer.alloc(0)),
    ).rejects.toThrow(/nrpc:closed/)

    // A handle issued before the close still owns its registration —
    // it holds the serve handle, not this envelope.
    expect(handle.isClosed()).toBe(false)
    handle.close()
  }, 30_000)
})
