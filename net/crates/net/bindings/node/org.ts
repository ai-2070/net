/**
 * Organization capability auth for Node (OSDK-L Workstream N).
 *
 * Two verbs over `@net-mesh/core`'s native `OrgCredentials` / `OrgClient`:
 *
 * ```ts
 * const credentials = OrgCredentials.create({
 *   membership, dispatcher, grants,
 *   audienceSecretPaths: ['/etc/net/grants/customer-read.audience'],
 * })
 * const org = TypedOrgClient.bind(mesh, credentials)
 * const customer = await org.call<GetCustomer, CustomerRecord>('customer.read', req)
 * ```
 *
 * This file is the typed + error layer only; it holds no policy. It mirrors how
 * `mesh_rpc.ts`'s `TypedMeshRpc` sits over the raw nRPC surface — JSON in one
 * place, the native module doing the authority work.
 *
 * ## The credential asymmetry
 *
 * Public signed credentials cross as `Buffer`s; the audience secret crosses as
 * a **path** and never as bytes, so the raw discovery key is never in
 * garbage-collected memory. There is deliberately no bytes variant.
 *
 * ## Teardown order
 *
 * ```text
 * orgClient.close()  →  serveHandle.close()  →  await mesh.shutdown()
 * ```
 *
 * An un-closed client holds an `Arc<MeshNode>`, so `mesh.shutdown()` drains for
 * ~250 ms and then REJECTS with "cannot shutdown: outstanding references
 * exist", leaving the node usable for a retry. It does not hang — but the first
 * shutdown fails.
 */

import {
  OrgAccess,
  OrgClient as NativeOrgClient,
  OrgCredentials,
  serveOrg as nativeServeOrg,
  serveOrgStreaming as nativeServeOrgStreaming,
  serveOrgClientStream as nativeServeOrgClientStream,
  serveOrgDuplex as nativeServeOrgDuplex,
  installOrgAuthority,
  installProviderGrantAudience,
} from './index'
import type { JsRequestStream, JsResponseSink, NetMesh, OrgCaller, OrgRequest, OrgServeHandle } from './index'
import {
  TypedClientStreamCall,
  TypedDuplexSink,
  TypedDuplexStream,
  TypedRequestStream,
  TypedResponseSink,
  TypedRpcStream,
} from './mesh_rpc'
import type {
  RawClientStreamCall,
  RawDuplexSink,
  RawDuplexStream,
  RawRequestStream,
  RawResponseSink,
  RawRpcStream,
} from './mesh_rpc'
import { classifyOrgError } from './errors'

export { OrgAccess, OrgCredentials, installOrgAuthority, installProviderGrantAudience }
export {
  classifyOrgError,
  OrgAdmissionDeniedError,
  OrgCredentialsError,
  OrgDiscoveryError,
  OrgError,
  OrgUnclassifiedError,
} from './errors'
export type { OrgCaller, OrgRequest, OrgServeHandle }
export type { OrgCredentialsOptions } from './index'
// The typed stream/sink handles the verbs below return — re-exported so an
// org consumer needs one import (`instanceof TypedRpcStream` etc.).
export {
  TypedClientStreamCall,
  TypedDuplexSink,
  TypedDuplexStream,
  TypedRequestStream,
  TypedResponseSink,
  TypedRpcStream,
}

// ---------------------------------------------------------------------------
// The typed client
// ---------------------------------------------------------------------------

function encode(value: unknown): Buffer {
  return Buffer.from(new TextEncoder().encode(JSON.stringify(value)))
}

function decode<T>(bytes: Buffer): T {
  return JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(bytes)) as T
}

/**
 * The typed layer takes `mesh: unknown` because class identity is
 * unreliable across the CJS/ESM dual-module boundary (the convention note
 * in `mesh_rpc.ts` explains why `instanceof` fails there), while the native
 * entry points want the real `NetMesh`. The one unchecked cast lives here,
 * once, instead of at every call site.
 */
function nativeMesh(mesh: unknown): NetMesh {
  return mesh as unknown as NetMesh // dual-module class identity, see above
}

/**
 * Execution control for the streaming org call verbs (the
 * `*_bytes_deadline` seam contract, §4.3). Neither field is an
 * authorization input — they select no grant and no authority.
 */
export interface OrgCallOptions {
  /**
   * Hard deadline, milliseconds from now. `0`/omitted is the facade's
   * default lifetime (Owner Q1, 300 s) — NEVER "no deadline": a protected
   * call's lifetime is finite by contract (D3).
   */
  deadlineMs?: number
  /**
   * A pre-reserved cancel token from the SAME node
   * (`MeshRpc.reserveCancelToken()`); fire `MeshRpc.cancelCall(token)` to
   * retire the call midstream. `0`/omitted is uncancellable.
   */
  cancelToken?: bigint
}

/**
 * Route a raw handle's failures through {@link classifyOrgError} — §4.4's
 * "midstream errors route through `classifyOrgError`".
 *
 * The raw org verbs render call outcomes in the `org:` wire vocabulary
 * (opening refusals and midstream retirement alike), so classifying them
 * here hands the caller typed `OrgError` instances at EVERY throw site, the
 * same contract `TypedOrgClient.call` has at its opening. Local
 * binding-usage refusals (`nrpc:stream_closed`) are not call outcomes;
 * `classifyOrgError` passes them through unchanged. The typed classes are
 * reused as-is (`TypedRpcStream` etc.); these shims only wrap the raw seam
 * they consume — no second stream type.
 */
function classifiedThrow<A extends unknown[], R>(
  fn: (...args: A) => Promise<R>,
): (...args: A) => Promise<R> {
  return async (...args: A) => {
    try {
      return await fn(...args)
    } catch (e) {
      throw classifyOrgError(e)
    }
  }
}

function classifiedRawStream(raw: RawRpcStream): RawRpcStream {
  return {
    next: classifiedThrow(raw.next.bind(raw)),
    grant: raw.grant.bind(raw),
    flowControlled: raw.flowControlled.bind(raw),
    close: raw.close.bind(raw),
  }
}

function classifiedRawClientStream(raw: RawClientStreamCall): RawClientStreamCall {
  return {
    send: classifiedThrow(raw.send.bind(raw)),
    finish: classifiedThrow(raw.finish.bind(raw)),
    callId: raw.callId.bind(raw),
    flowControlled: raw.flowControlled.bind(raw),
    close: raw.close.bind(raw),
  }
}

function classifiedRawDuplexSink(raw: RawDuplexSink): RawDuplexSink {
  return {
    send: classifiedThrow(raw.send.bind(raw)),
    finish: classifiedThrow(raw.finish.bind(raw)),
    callId: raw.callId.bind(raw),
    flowControlled: raw.flowControlled.bind(raw),
    close: raw.close.bind(raw),
  }
}

function classifiedRawDuplexStream(raw: RawDuplexStream): RawDuplexStream {
  return {
    next: classifiedThrow(raw.next.bind(raw)),
    callId: raw.callId.bind(raw),
    close: raw.close.bind(raw),
  }
}

/** See {@link classifiedThrow} — provider-side inbound request stream. */
function classifiedRawRequestStream(raw: RawRequestStream): RawRequestStream {
  return {
    next: classifiedThrow(raw.next.bind(raw)),
    callerOrigin: raw.callerOrigin,
    callId: raw.callId,
    deadlineNs: raw.deadlineNs,
    headers: raw.headers,
  }
}

/**
 * JSON-typed wrapper over the native {@link NativeOrgClient}.
 *
 * The codec is JSON, hard-coded, matching every other typed layer in the SDK.
 * Drop to `raw.callBytes` if you marshal yourself.
 */
export class TypedOrgClient {
  /** The native client. Exposed for `callBytes` and lifecycle. */
  readonly raw: NativeOrgClient

  private constructor(raw: NativeOrgClient) {
    this.raw = raw
  }

  /** Bind credentials to a mesh. Consumes `credentials`. */
  static bind(mesh: unknown, credentials: OrgCredentials): TypedOrgClient {
    try {
      return new TypedOrgClient(NativeOrgClient.bind(nativeMesh(mesh), credentials))
    } catch (e) {
      throw classifyOrgError(e)
    }
  }

  /**
   * Call a protected service.
   *
   * Discovers privately, selects one authorized provider, and issues ONE
   * exact-target call. Never retries — a signed proof is bound to one call id,
   * so a second attempt must be one you make deliberately.
   */
  async call<Req = unknown, Resp = unknown>(service: string, request: Req): Promise<Resp> {
    try {
      const reply = await this.raw.callBytes(service, encode(request))
      return decode<Resp>(reply)
    } catch (e) {
      throw classifyOrgError(e)
    }
  }

  /**
   * Call a subnet-exported service (`SUBNET_AUTH_SDK_PLAN.md` §3.6).
   *
   * Discovers on the PUBLIC plane through the verified ownership
   * projection, derives the same-org/granted relation from the VERIFIED
   * owner, mints the same canonical proof as {@link call}, and sends
   * exactly once — never a retry. Deliberately `callExported`, not
   * `callSubnet`: you name a service, not a subnet — the caller never
   * joins the provider's subnet and receives no subnet context. Whether
   * the export exists is decided provider-side, per call; provider-side
   * authority movement surfaces as one coarse admission denial, and
   * rediscovery/retry policy is yours.
   */
  async callExported<Req = unknown, Resp = unknown>(
    service: string,
    request: Req,
  ): Promise<Resp> {
    try {
      const reply = await this.raw.callExportedBytes(service, encode(request))
      return decode<Resp>(reply)
    } catch (e) {
      throw classifyOrgError(e)
    }
  }

  /**
   * Call a protected service whose response is a STREAM (§4.4). One JSON
   * request in; a {@link TypedRpcStream} of decoded responses out — drain
   * with `for await` or `next()` until `null`.
   *
   * Discovers privately, selects one authorized provider, mints ONE
   * request-bound proof, and never retries (the proof binds one call id).
   * Opening errors are classified here; midstream outcomes surface from the
   * stream's `next()` already routed through `classifyOrgError`
   * (`OrgError{rpc}` on deadline/cancel retirement, an
   * `OrgAdmissionDeniedError` on revocation). Drop or `close()` emits one
   * CANCEL.
   */
  async callStreaming<Req = unknown, Resp = unknown>(
    service: string,
    request: Req,
    opts?: OrgCallOptions,
  ): Promise<TypedRpcStream<Resp>> {
    try {
      const inner = await this.raw.callStreamingBytes(
        service,
        encode(request),
        opts?.deadlineMs,
        opts?.cancelToken,
      )
      return new TypedRpcStream<Resp>(classifiedRawStream(inner))
    } catch (e) {
      throw classifyOrgError(e)
    }
  }

  /**
   * Call a protected service with a STREAM OF REQUESTS and one terminal
   * response (§4.4). Push typed requests via `send`, then `finish()` for
   * the decoded terminal response. The opening is lazy — it rides the first
   * `send`/`finish` against the provider this call pinned.
   *
   * Error routing matches {@link callStreaming}.
   */
  async callClientStream<Req = unknown, Resp = unknown>(
    service: string,
    opts?: OrgCallOptions,
  ): Promise<TypedClientStreamCall<Req, Resp>> {
    try {
      const inner = await this.raw.callClientStreamBytes(
        service,
        opts?.deadlineMs,
        opts?.cancelToken,
      )
      return new TypedClientStreamCall<Req, Resp>(classifiedRawClientStream(inner))
    } catch (e) {
      throw classifyOrgError(e)
    }
  }

  /**
   * Call a protected service DUPLEX (§4.4): returns the split
   * `[TypedDuplexSink<Req>, TypedDuplexStream<Resp>]` halves directly —
   * push via `sink.send`, pull via `for await (const item of stream)`.
   * CANCEL fires only when BOTH halves drop without observing the
   * response stream's terminal frame.
   *
   * Error routing matches {@link callStreaming}.
   */
  async callDuplex<Req = unknown, Resp = unknown>(
    service: string,
    opts?: OrgCallOptions,
  ): Promise<[TypedDuplexSink<Req>, TypedDuplexStream<Resp>]> {
    try {
      const [rawSink, rawStream] = await this.raw.callDuplexBytes(
        service,
        opts?.deadlineMs,
        opts?.cancelToken,
      )
      return [
        new TypedDuplexSink<Req>(classifiedRawDuplexSink(rawSink)),
        new TypedDuplexStream<Resp>(classifiedRawDuplexStream(rawStream)),
      ]
    } catch (e) {
      throw classifyOrgError(e)
    }
  }

  /** The organization this client acts for, as 32 raw bytes. */
  get actingOrg(): Buffer {
    return this.raw.actingOrg
  }

  /** The entity this client calls as, as 32 raw bytes. */
  get caller(): Buffer {
    return this.raw.caller
  }

  /** Whether {@link close} has been called. */
  get isClosed(): boolean {
    return this.raw.isClosed
  }

  /**
   * Release the client — drops its audience lease and node reference.
   * Idempotent. Call before `mesh.shutdown()`; see the module docs.
   */
  close(): void {
    this.raw.close()
  }
}

// ---------------------------------------------------------------------------
// The provider verb
// ---------------------------------------------------------------------------

/**
 * A JSON-typed org handler.
 *
 * Receives the provider-verified {@link OrgCaller} — five fields, every one
 * checked by the admission engine before the handler ran, none caller-claimed.
 */
export type TypedOrgHandler<Req = unknown, Resp = unknown> = (
  caller: OrgCaller,
  req: Req,
) => Resp | Promise<Resp>

/**
 * Serve a protected, privately-discoverable service with a JSON codec.
 *
 * `access` selects both who may call AND how the service is announced — both
 * variants ship only inside an encrypted audience, never on the plaintext
 * plane. There is no visibility knob to get wrong.
 *
 * Throwing (or rejecting) from the handler surfaces as an application error,
 * never as an admission denial: `0x0009` is the admission engine's word.
 *
 * ```ts
 * const handle = serveOrgTyped(mesh, 'customer.read', OrgAccess.Granted,
 *   async (caller, req: GetCustomer) => readCustomer(caller, req))
 * ```
 */
export function serveOrgTyped<Req = unknown, Resp = unknown>(
  mesh: unknown,
  service: string,
  access: OrgAccess,
  handler: TypedOrgHandler<Req, Resp>,
  handlerTimeoutMs?: number,
): OrgServeHandle {
  try {
    return nativeServeOrg(
      nativeMesh(mesh),
      service,
      access,
      async (req: OrgRequest): Promise<Buffer> => {
        const decoded = decode<Req>(req.request)
        const resp = await handler(req.caller, decoded)
        return encode(resp)
      },
      handlerTimeoutMs,
    )
  } catch (e) {
    throw classifyOrgError(e)
  }
}

// ---------------------------------------------------------------------------
// The streaming provider verbs (§4.4)
// ---------------------------------------------------------------------------

/** The handler's "done" Buffer — the Promise resolving is the signal. */
const ORG_HANDLER_DONE = Buffer.alloc(0)

/**
 * A JSON-typed server-streaming org handler: one request in, many typed
 * responses out through the reused {@link TypedResponseSink}.
 */
export type TypedOrgStreamingHandler<Req = unknown, Resp = unknown> = (
  caller: OrgCaller,
  req: Req,
  sink: TypedResponseSink<Resp>,
) => void | Promise<void>

/**
 * A JSON-typed client-streaming org handler: drain the typed request
 * stream, return the terminal response.
 */
export type TypedOrgClientStreamHandler<Req = unknown, Resp = unknown> = (
  caller: OrgCaller,
  requests: TypedRequestStream<Req>,
) => Resp | Promise<Resp>

/**
 * A JSON-typed duplex org handler: drain + emit concurrently.
 */
export type TypedOrgDuplexHandler<Req = unknown, Resp = unknown> = (
  caller: OrgCaller,
  requests: TypedRequestStream<Req>,
  sink: TypedResponseSink<Resp>,
) => void | Promise<void>

/**
 * Serve a protected, privately-discoverable service whose response is a
 * STREAM, with a JSON codec (§4.4's `serveOrgStreaming` typed row).
 *
 * `access` and attribution are {@link serveOrgTyped}'s contract, unchanged:
 * the handler receives the admission-verified {@link OrgCaller} — five
 * checked facts, none caller-claimed — and a {@link TypedResponseSink} to
 * emit through. Throwing (or rejecting) surfaces as an application error,
 * never as an admission denial.
 *
 * **Handler-drop contract — the §2.2 / F-S3.1-2 level, stated explicitly.**
 * The handler future is polled inside the call's retire supervisor, which
 * may DROP it without a final poll when the call retires (caller cancel,
 * deadline, revocation, teardown). Do NOT assume cancellation is a
 * handler-side event: the promise may simply never settle and `finally` is
 * not guaranteed to run. Cancellation is observed through the retirement
 * observables — the caller's terminal stream outcome (routed through
 * `classifyOrgError`), the one CANCEL from a dropped call handle, and this
 * handle's `close()`. The sink is released by the Rust bridge when the
 * handler settles or its future drops — never on a handler-side `finally`.
 */
export function serveOrgStreamingTyped<Req = unknown, Resp = unknown>(
  mesh: unknown,
  service: string,
  access: OrgAccess,
  handler: TypedOrgStreamingHandler<Req, Resp>,
  handlerTimeoutMs?: number,
): OrgServeHandle {
  try {
    return nativeServeOrgStreaming(
      nativeMesh(mesh),
      service,
      access,
      async (args: [OrgCaller, Buffer, JsResponseSink]): Promise<Buffer> => {
        const [caller, rawReq, rawSink] = args
        await handler(caller, decode<Req>(rawReq), new TypedResponseSink<Resp>(rawSink))
        return ORG_HANDLER_DONE
      },
      handlerTimeoutMs,
    )
  } catch (e) {
    throw classifyOrgError(e)
  }
}

/**
 * Serve a protected, privately-discoverable service with a STREAM OF
 * REQUESTS and one typed terminal response (§4.4's `serveOrgClientStream`
 * typed row). The reused {@link TypedRequestStream}'s `callerOrigin` /
 * `callId` / `deadlineNs` / `headers` accessors report their documented
 * EMPTY values on this surface (the frozen org handler seam carries only
 * the verified {@link OrgCaller} and the stream); attribution rides the
 * `OrgCaller`.
 *
 * **Handler-drop contract (§2.2 / F-S3.1-2, stated level):** see
 * {@link serveOrgStreamingTyped} — the retire supervisor may drop the
 * handler future without a final poll; cancellation is observed through the
 * retirement observables, never assumed as a handler-side event.
 */
export function serveOrgClientStreamTyped<Req = unknown, Resp = unknown>(
  mesh: unknown,
  service: string,
  access: OrgAccess,
  handler: TypedOrgClientStreamHandler<Req, Resp>,
  handlerTimeoutMs?: number,
): OrgServeHandle {
  try {
    return nativeServeOrgClientStream(
      nativeMesh(mesh),
      service,
      access,
      async (args: [OrgCaller, JsRequestStream]): Promise<Buffer> => {
        const [caller, rawStream] = args
        const resp = await handler(
          caller,
          new TypedRequestStream<Req>(classifiedRawRequestStream(rawStream)),
        )
        return encode(resp)
      },
      handlerTimeoutMs,
    )
  } catch (e) {
    throw classifyOrgError(e)
  }
}

/**
 * Serve a protected, privately-discoverable DUPLEX service with a JSON
 * codec (§4.4's `serveOrgDuplex` typed row). Both directions are
 * independent — emit before/after/while draining. The request stream's
 * metadata accessors report their documented EMPTY values on this surface
 * — see {@link serveOrgClientStreamTyped}.
 *
 * **Handler-drop contract (§2.2 / F-S3.1-2, stated level):** see
 * {@link serveOrgStreamingTyped} — the retire supervisor may drop the
 * handler future without a final poll; cancellation is observed through the
 * retirement observables, never assumed as a handler-side event. The sink
 * is released by the Rust bridge when the handler settles or its future
 * drops.
 */
export function serveOrgDuplexTyped<Req = unknown, Resp = unknown>(
  mesh: unknown,
  service: string,
  access: OrgAccess,
  handler: TypedOrgDuplexHandler<Req, Resp>,
  handlerTimeoutMs?: number,
): OrgServeHandle {
  try {
    return nativeServeOrgDuplex(
      nativeMesh(mesh),
      service,
      access,
      async (args: [OrgCaller, JsRequestStream, JsResponseSink]): Promise<Buffer> => {
        const [caller, rawStream, rawSink] = args
        await handler(
          caller,
          new TypedRequestStream<Req>(classifiedRawRequestStream(rawStream)),
          new TypedResponseSink<Resp>(rawSink),
        )
        return ORG_HANDLER_DONE
      },
      handlerTimeoutMs,
    )
  } catch (e) {
    throw classifyOrgError(e)
  }
}
