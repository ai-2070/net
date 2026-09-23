/**
 * S4 consumer-compile probe — a CONSUMER program against the shipped org
 * surface: the typed layer (`../org`) and the generated native declarations
 * (`../index`) with `skipLibCheck: false`, the check a careful consumer
 * runs. Compiled, never executed: the compile IS the evidence (the S3
 * `guards/org_api_probe` discipline). Any signature/context/enum break in
 * the pinned surface — including a `ts_args_type` regression that leaves
 * the generated `index.d.ts` naming a type nothing declares — fails it.
 *
 * Everything is exported so the probe reads as real consumer code and no
 * linter drops an "unused" pin.
 */

import {
  serveOrgClientStream as nativeServeOrgClientStream,
  serveOrgDuplex as nativeServeOrgDuplex,
  serveOrgStreaming as nativeServeOrgStreaming,
} from '../../index'
import type {
  JsRequestStream,
  JsResponseSink,
  NetMesh,
  OrgAccess as NativeOrgAccess,
  OrgCaller as NativeOrgCaller,
  OrgClient as NativeOrgClient,
  OrgServeHandle as NativeOrgServeHandle,
} from '../../index'
import {
  classifyOrgError,
  OrgAdmissionDeniedError,
  OrgError,
  serveOrgClientStreamTyped,
  serveOrgDuplexTyped,
  serveOrgStreamingTyped,
  TypedOrgClient,
} from '../../org'
import type {
  OrgAccess,
  OrgCallOptions,
  OrgCaller,
  OrgServeHandle,
  TypedOrgClientStreamHandler,
  TypedOrgDuplexHandler,
  TypedOrgStreamingHandler,
} from '../../org'
import type {
  TypedClientStreamCall,
  TypedDuplexSink,
  TypedDuplexStream,
  TypedRequestStream,
  TypedResponseSink,
  TypedRpcStream,
} from '../../org'

export interface Ping {
  n: number
}

export interface Pong {
  n: number
}

export interface Summary {
  count: number
}

/** The execution-control surface (§4.3 seam contract) pins by literal. */
export const CALL_OPTIONS: OrgCallOptions = { deadlineMs: 300_000, cancelToken: 0n }

/** §4.4's `callStreamingBytes` / `callClientStreamBytes` / `callDuplexBytes`. */
export async function probeTypedCalls(client: TypedOrgClient): Promise<void> {
  const stream: TypedRpcStream<Pong> = await client.callStreaming<Ping, Pong>(
    'svc.ss',
    { n: 1 },
    CALL_OPTIONS,
  )
  for await (const item of stream) {
    const n: number = item.n
    void n
  }

  const cs: TypedClientStreamCall<Ping, Summary> = await client.callClientStream<Ping, Summary>(
    'svc.cs',
    CALL_OPTIONS,
  )
  await cs.send({ n: 1 })
  const summary: Summary = await cs.finish()
  void summary

  const halves: [TypedDuplexSink<Ping>, TypedDuplexStream<Pong>] = await client.callDuplex<Ping, Pong>(
    'svc.dx',
    CALL_OPTIONS,
  )
  const [sink, resp] = halves
  await sink.send({ n: 2 })
  await sink.finish()
  const first: Pong | null = await resp.next()
  void first
  await resp.close()
}

/** The raw byte verbs over the generated declarations. */
export async function probeRawCalls(raw: NativeOrgClient, request: Buffer): Promise<void> {
  const stream = await raw.callStreamingBytes(
    'svc.ss',
    request,
    CALL_OPTIONS.deadlineMs,
    CALL_OPTIONS.cancelToken,
  )
  await stream.next()
  await stream.close()

  const cs = await raw.callClientStreamBytes('svc.cs', CALL_OPTIONS.deadlineMs, CALL_OPTIONS.cancelToken)
  await cs.send(request)
  await cs.finish()

  const [sink, resp] = await raw.callDuplexBytes('svc.dx', CALL_OPTIONS.deadlineMs, CALL_OPTIONS.cancelToken)
  await sink.send(request)
  await sink.finish()
  await resp.next()
}

/** The three typed serve rows — `OrgCaller` first in every handler. */
export function probeTypedServes(mesh: unknown, access: OrgAccess): OrgServeHandle[] {
  const ss: TypedOrgStreamingHandler<Ping, Pong> = (
    caller: OrgCaller,
    req: Ping,
    sink: TypedResponseSink<Pong>,
  ): void => {
    const entity: Buffer = caller.entity
    void entity
    sink.send({ n: req.n + 1 })
  }
  const cs: TypedOrgClientStreamHandler<Ping, Summary> = async (
    caller: OrgCaller,
    requests: TypedRequestStream<Ping>,
  ): Promise<Summary> => {
    void caller
    let count = 0
    for await (const item of requests) {
      count += item.n
    }
    return { count }
  }
  const dx: TypedOrgDuplexHandler<Ping, Pong> = async (
    caller: OrgCaller,
    requests: TypedRequestStream<Ping>,
    sink: TypedResponseSink<Pong>,
  ): Promise<void> => {
    void caller
    for await (const item of requests) {
      sink.send({ n: item.n })
    }
  }
  return [
    serveOrgStreamingTyped<Ping, Pong>(mesh, 'svc.ss', access, ss),
    serveOrgClientStreamTyped<Ping, Summary>(mesh, 'svc.cs', access, cs),
    serveOrgDuplexTyped<Ping, Pong>(mesh, 'svc.dx', access, dx),
  ]
}

/**
 * The generated declarations for the §4.4 serve verbs — the `[caller, req,
 * sink]` TSFN tuple shapes pinned exactly as a consumer sees them.
 */
export function probeRawServes(mesh: NetMesh, access: NativeOrgAccess): NativeOrgServeHandle[] {
  const streamingSig: (
    mesh: NetMesh,
    service: string,
    access: NativeOrgAccess,
    handler: (args: [NativeOrgCaller, Buffer, JsResponseSink]) => Promise<Buffer>,
    handlerTimeoutMs?: number,
  ) => NativeOrgServeHandle = nativeServeOrgStreaming
  const clientStreamSig: (
    mesh: NetMesh,
    service: string,
    access: NativeOrgAccess,
    handler: (args: [NativeOrgCaller, JsRequestStream]) => Promise<Buffer>,
    handlerTimeoutMs?: number,
  ) => NativeOrgServeHandle = nativeServeOrgClientStream
  const duplexSig: (
    mesh: NetMesh,
    service: string,
    access: NativeOrgAccess,
    handler: (args: [NativeOrgCaller, JsRequestStream, JsResponseSink]) => Promise<Buffer>,
    handlerTimeoutMs?: number,
  ) => NativeOrgServeHandle = nativeServeOrgDuplex
  const done = async (): Promise<Buffer> => Buffer.alloc(0)
  return [
    streamingSig(mesh, 'svc.ss', access, async () => done()),
    clientStreamSig(mesh, 'svc.cs', access, async () => done()),
    duplexSig(mesh, 'svc.dx', access, async () => done()),
  ]
}

/** The midstream error vocabulary (§4.4) — `classifyOrgError` in, classes out. */
export function probeErrors(e: unknown): string {
  const classified: unknown = classifyOrgError(e)
  if (classified instanceof OrgAdmissionDeniedError) {
    const reason: string = classified.reason
    return reason
  }
  if (classified instanceof OrgError) {
    return `${classified.domain}:${classified.kind ?? ''}`
  }
  return 'unclassified'
}
