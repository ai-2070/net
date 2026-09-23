/**
 * Organization capability auth for the pure TS SDK (`@net-mesh/sdk`).
 *
 * Stage 4 Q6's thin pass-through over the typed wrappers in
 * `@net-mesh/core/org` — NO protocol logic lives here. Proof minting,
 * private discovery, admission, provider selection, routing, and
 * retirement are the native layer's; this module resolves the SDK's
 * {@link MeshNode} handle to its native mesh, projects the verified
 * {@link OrgCaller} to handlers, routes every throw site through the
 * {@link classifyOrgError} mirror, and re-exports the typed surface —
 * typed streams included. The frozen rule holds at this level too:
 * **no new stream wrapper**; the verbs return the SAME
 * {@link TypedRpcStream} / {@link TypedClientStreamCall} /
 * {@link TypedDuplexSink} + {@link TypedDuplexStream} classes every
 * other typed surface returns.
 *
 * ```ts
 * import { MeshNode } from '@net-mesh/sdk'
 * import { OrgAccess, OrgClient, OrgCredentials, serveOrgStreaming } from '@net-mesh/sdk/org'
 *
 * const handle = serveOrgStreaming(mesh, 'customer.read', OrgAccess.SameOrg,
 *   (caller, req: GetCustomer, sink) => {
 *     sink.send(readChunk(caller, req))
 *   })
 * const org = OrgClient.bind(mesh, credentials)
 * const stream = await org.callStreaming<GetCustomer, CustomerRecord>(
 *   'customer.read', req)
 * ```
 *
 * ## The §4.4 seam contract (inherited — stated here for the SDK surface)
 *
 * - ONE authorized provider is selected and pinned PER CALL: the call
 *   discovers privately, mints ONE request-bound proof, sends exactly
 *   once, and NEVER retries underneath (a signed proof binds one call
 *   id — a second attempt must be one you make deliberately).
 * - `deadlineMs` `0`/omitted is the facade's default lifetime (Owner Q1,
 *   300 s) — NEVER "no deadline": a protected call's lifetime is finite
 *   by contract (D3).
 * - `cancelToken` `0`/omitted is uncancellable. Reserve a token from the
 *   same node (`MeshRpc.reserveCancelToken()`) and fire
 *   `MeshRpc.cancelCall(token)` to retire the call midstream.
 * - Neither field is an authorization input — they select no grant and
 *   no authority.
 *
 * ## Teardown order
 *
 * ```text
 * orgClient.close()  →  serveHandle.close()  →  await mesh.shutdown()
 * ```
 *
 * An un-closed client holds a mesh reference, so `mesh.shutdown()`
 * rejects with "outstanding references exist" (leaving the node usable
 * for a retry) instead of completing. Idempotent `close()`s; do them in
 * this order.
 */

import type { NetMesh } from '@net-mesh/core'
import {
  OrgAccess,
  OrgCredentials,
  TypedClientStreamCall,
  TypedDuplexSink,
  TypedDuplexStream,
  TypedOrgClient,
  TypedRequestStream,
  TypedResponseSink,
  TypedRpcStream,
  classifyOrgError,
  installOrgAuthority as nativeInstallOrgAuthority,
  installProviderGrantAudience as nativeInstallProviderGrantAudience,
  serveOrgClientStreamTyped,
  serveOrgDuplexTyped,
  serveOrgStreamingTyped,
  serveOrgTyped,
} from '@net-mesh/core/org'
import type {
  OrgCallOptions,
  OrgCaller,
  OrgCredentialsOptions,
  OrgRequest,
  OrgServeHandle,
  TypedOrgClientStreamHandler,
  TypedOrgDuplexHandler,
  TypedOrgHandler,
  TypedOrgStreamingHandler,
} from '@net-mesh/core/org'
import { getNapiMesh } from '../_internal'
import { MeshNode } from '../mesh'

export {
  OrgAccess,
  OrgCredentials,
  TypedClientStreamCall,
  TypedDuplexSink,
  TypedDuplexStream,
  TypedOrgClient,
  TypedRequestStream,
  TypedResponseSink,
  TypedRpcStream,
  classifyOrgError,
}
export {
  OrgAdmissionDeniedError,
  OrgCredentialsError,
  OrgDiscoveryError,
  OrgError,
  OrgUnclassifiedError,
} from '@net-mesh/core/errors'
export type {
  OrgCallOptions,
  OrgCaller,
  OrgCredentialsOptions,
  OrgRequest,
  OrgServeHandle,
  TypedOrgClientStreamHandler,
  TypedOrgDuplexHandler,
  TypedOrgHandler,
  TypedOrgStreamingHandler,
}

/**
 * The native handle behind `mesh`. A {@link MeshNode} registers its
 * napi mesh on the SDK-internal WeakMap (see `../_internal`); a raw
 * `NetMesh` passes straight through — the typed layer's
 * `mesh: unknown` convention, kept so the two levels interoperate.
 */
function nativeMesh(mesh: MeshNode | NetMesh): NetMesh {
  return mesh instanceof MeshNode ? getNapiMesh(mesh) : mesh
}

/**
 * The ONE attribution hand-off every serve wrapper below routes
 * through.
 *
 * A handler receives the admission-verified {@link OrgCaller} — the
 * invoked capability plus five checked facts (acting entity, acting
 * org, provider org, provider entity, same-org relation), every one
 * verified by the provider's admission engine before the handler ran,
 * NONE caller-claimed and NONE derived from routing origin. Passing
 * `caller` through is the whole function: it exists so the
 * verified-not-origin rule has exactly one production site at the SDK's
 * wrapper level. The request-stream's `callerOrigin` accessor is the
 * wrong source by construction — on the org seam it reports its
 * documented EMPTY value; attribution rides the `OrgCaller`.
 */
function projectVerifiedCaller(caller: OrgCaller): OrgCaller {
  return caller
}

/**
 * Execution control + authorization for the protected call verbs.
 * Re-exported from the typed layer: `deadlineMs` `0`/omitted is the
 * facade's 300 s default lifetime (never "no deadline"); `cancelToken`
 * `0`/omitted is uncancellable. Neither is an authorization input. See
 * the module docs.
 */

// ---------------------------------------------------------------------------
// The typed client
// ---------------------------------------------------------------------------

/**
 * The SDK's organization client — a thin forwarding wrapper over the
 * typed layer's {@link TypedOrgClient} (reachable at {@link typed} for
 * drop-through). Every call-outcome throw site routes through
 * {@link classifyOrgError} AT THIS WRAPPER LEVEL, so a consumer sees
 * typed `OrgError` instances at every failure — openings and midstream
 * alike — no matter which layer surfaced them.
 *
 * Discovers privately, selects ONE authorized provider per call, mints
 * ONE request-bound proof, and never retries underneath. Consumes the
 * `credentials` at {@link bind}.
 */
export class OrgClient {
  /** The typed wrapper this facade forwards to (drop-through). */
  readonly typed: TypedOrgClient

  private constructor(typed: TypedOrgClient) {
    this.typed = typed
  }

  /** Bind credentials to a mesh. Consumes `credentials`. */
  static bind(mesh: MeshNode | NetMesh, credentials: OrgCredentials): OrgClient {
    try {
      return new OrgClient(TypedOrgClient.bind(nativeMesh(mesh), credentials))
    } catch (e) {
      throw classifyOrgError(e)
    }
  }

  /**
   * Call a protected service (the PRESERVED unary shape — its semantics
   * are unchanged by the streaming surface).
   */
  async call<Req = unknown, Resp = unknown>(service: string, request: Req): Promise<Resp> {
    try {
      return await this.typed.call<Req, Resp>(service, request)
    } catch (e) {
      throw classifyOrgError(e)
    }
  }

  /**
   * Call a subnet-exported service (`SUBNET_AUTH_SDK_PLAN.md` §3.6):
   * names a service, never a subnet — see `TypedOrgClient.callExported`.
   */
  async callExported<Req = unknown, Resp = unknown>(
    service: string,
    request: Req,
  ): Promise<Resp> {
    try {
      return await this.typed.callExported<Req, Resp>(service, request)
    } catch (e) {
      throw classifyOrgError(e)
    }
  }

  /**
   * Call a protected service whose response is a STREAM (§4.4): one
   * typed request in; a {@link TypedRpcStream} of typed responses out —
   * the EXISTING typed stream class, never a new wrapper. Drain with
   * `for await` or `next()` until `null`.
   *
   * The provider is pinned per call (module docs); `opts` is execution
   * control only — see {@link OrgCallOptions}. Opening errors are
   * classified here; midstream outcomes surface from the stream's
   * `next()` already routed through {@link classifyOrgError}
   * (`OrgError{rpc}` on deadline/cancel retirement, an
   * `OrgAdmissionDeniedError` on revocation) — a terminal error is
   * NEVER a false clean `null` EOF. Drop or `close()` emits one CANCEL.
   */
  async callStreaming<Req = unknown, Resp = unknown>(
    service: string,
    request: Req,
    opts?: OrgCallOptions,
  ): Promise<TypedRpcStream<Resp>> {
    try {
      return await this.typed.callStreaming<Req, Resp>(service, request, opts)
    } catch (e) {
      throw classifyOrgError(e)
    }
  }

  /**
   * Call a protected service with a STREAM OF REQUESTS and one typed
   * terminal response (§4.4): push via `send`, then `finish()` for the
   * decoded terminal response. The opening is lazy — it rides the first
   * `send`/`finish` against the provider this call pinned. The reused
   * {@link TypedClientStreamCall}; error routing matches
   * {@link callStreaming}.
   */
  async callClientStream<Req = unknown, Resp = unknown>(
    service: string,
    opts?: OrgCallOptions,
  ): Promise<TypedClientStreamCall<Req, Resp>> {
    try {
      return await this.typed.callClientStream<Req, Resp>(service, opts)
    } catch (e) {
      throw classifyOrgError(e)
    }
  }

  /**
   * Call a protected service DUPLEX (§4.4): returns the split
   * `[TypedDuplexSink<Req>, TypedDuplexStream<Resp>]` halves — the
   * EXISTING typed halves, never new wrappers. Push via `sink.send`,
   * pull via `for await (const item of stream)`. CANCEL fires only when
   * BOTH halves drop without observing the response stream's terminal
   * frame. Error routing matches {@link callStreaming}.
   */
  async callDuplex<Req = unknown, Resp = unknown>(
    service: string,
    opts?: OrgCallOptions,
  ): Promise<[TypedDuplexSink<Req>, TypedDuplexStream<Resp>]> {
    try {
      return await this.typed.callDuplex<Req, Resp>(service, opts)
    } catch (e) {
      throw classifyOrgError(e)
    }
  }

  /** The organization this client acts for, as 32 raw bytes. */
  get actingOrg(): Buffer {
    return this.typed.actingOrg
  }

  /** The entity this client calls as, as 32 raw bytes. */
  get caller(): Buffer {
    return this.typed.caller
  }

  /** Whether {@link close} has been called. */
  get isClosed(): boolean {
    return this.typed.isClosed
  }

  /**
   * Release the client — drops its audience lease and mesh reference.
   * Idempotent. Call before `mesh.shutdown()`; see the module docs.
   */
  close(): void {
    this.typed.close()
  }
}

// ---------------------------------------------------------------------------
// The provider verbs
// ---------------------------------------------------------------------------

/**
 * Install a node's adopted org authority (the startup step — adoption
 * itself stays in the CLI). REQUIRED before {@link OrgClient.bind} can
 * succeed or a `Granted` service can serve. See
 * `installOrgAuthority`'s docs on the binding.
 */
export function installOrgAuthority(mesh: MeshNode | NetMesh, authorityDir: string): void {
  try {
    nativeInstallOrgAuthority(nativeMesh(mesh), authorityDir)
  } catch (e) {
    throw classifyOrgError(e)
  }
}

/**
 * Install a provider grant audience so a `Granted` service can seal
 * envelopes: the grant's wire bytes plus its out-of-band secret (a PATH
 * — the raw key never enters JS). A `SameOrg` provider does NOT need
 * this; it seals under the owner audience carried by the installed
 * authority.
 */
export function installProviderGrantAudience(
  mesh: MeshNode | NetMesh,
  grant: Buffer,
  audienceSecretPath: string,
): void {
  try {
    nativeInstallProviderGrantAudience(nativeMesh(mesh), grant, audienceSecretPath)
  } catch (e) {
    throw classifyOrgError(e)
  }
}

/**
 * Serve a protected, privately-discoverable service (the PRESERVED
 * unary shape) with a JSON codec.
 *
 * `access` selects both who may call AND how the service is announced —
 * both variants ship only inside an encrypted audience, never on the
 * plaintext plane. The handler receives the admission-verified
 * {@link OrgCaller} via {@link projectVerifiedCaller}: five checked
 * facts, none caller-claimed and none routing-derived. Throwing (or
 * rejecting) surfaces as an application error, never as an admission
 * denial.
 */
export function serveOrg<Req = unknown, Resp = unknown>(
  mesh: MeshNode | NetMesh,
  service: string,
  access: OrgAccess,
  handler: TypedOrgHandler<Req, Resp>,
  handlerTimeoutMs?: number,
): OrgServeHandle {
  try {
    return serveOrgTyped<Req, Resp>(
      nativeMesh(mesh),
      service,
      access,
      (caller, req) => handler(projectVerifiedCaller(caller), req),
      handlerTimeoutMs,
    )
  } catch (e) {
    throw classifyOrgError(e)
  }
}

/**
 * Serve a protected, privately-discoverable service whose response is a
 * STREAM, with a JSON codec (§4.4's `serveOrgStreaming` typed row).
 *
 * `access` and attribution are {@link serveOrg}'s contract, unchanged:
 * the handler receives the admission-verified {@link OrgCaller} via
 * {@link projectVerifiedCaller} and a {@link TypedResponseSink} to emit
 * through — the EXISTING typed sink, never a new wrapper. Throwing (or
 * rejecting) surfaces as an application error, never as an admission
 * denial.
 *
 * **Handler-drop contract — the §2.2 / F-S3.1-2 level, stated
 * explicitly.** The handler future is polled inside the call's retire
 * supervisor, which may DROP it without a final poll when the call
 * retires (caller cancel, deadline, revocation, teardown). Do NOT
 * assume cancellation is a handler-side event: the promise may simply
 * never settle and `finally` is not guaranteed to run. Cancellation is
 * observed through the retirement observables — the caller's terminal
 * stream outcome (routed through `classifyOrgError`), the one CANCEL
 * from a dropped call handle, and this handle's `close()`. The sink is
 * released by the Rust bridge when the handler settles or its future
 * drops — never on a handler-side `finally`.
 */
export function serveOrgStreaming<Req = unknown, Resp = unknown>(
  mesh: MeshNode | NetMesh,
  service: string,
  access: OrgAccess,
  handler: TypedOrgStreamingHandler<Req, Resp>,
  handlerTimeoutMs?: number,
): OrgServeHandle {
  try {
    return serveOrgStreamingTyped<Req, Resp>(
      nativeMesh(mesh),
      service,
      access,
      (caller, req, sink) => handler(projectVerifiedCaller(caller), req, sink),
      handlerTimeoutMs,
    )
  } catch (e) {
    throw classifyOrgError(e)
  }
}

/**
 * Serve a protected, privately-discoverable service with a STREAM OF
 * REQUESTS and one typed terminal response (§4.4's
 * `serveOrgClientStream` typed row). The reused
 * {@link TypedRequestStream}'s `callerOrigin` / `callId` / `deadlineNs`
 * / `headers` accessors report their documented EMPTY values on this
 * surface (the frozen org handler seam carries only the verified
 * {@link OrgCaller} and the stream); attribution rides the `OrgCaller`
 * via {@link projectVerifiedCaller} — never the stream origin.
 *
 * **Handler-drop contract (§2.2 / F-S3.1-2, stated level):** see
 * {@link serveOrgStreaming} — the retire supervisor may drop the
 * handler future without a final poll; cancellation is observed through
 * the retirement observables, never assumed as a handler-side event.
 */
export function serveOrgClientStream<Req = unknown, Resp = unknown>(
  mesh: MeshNode | NetMesh,
  service: string,
  access: OrgAccess,
  handler: TypedOrgClientStreamHandler<Req, Resp>,
  handlerTimeoutMs?: number,
): OrgServeHandle {
  try {
    return serveOrgClientStreamTyped<Req, Resp>(
      nativeMesh(mesh),
      service,
      access,
      (caller, requests) => handler(projectVerifiedCaller(caller), requests),
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
 * metadata accessors report their documented EMPTY values on this
 * surface — see {@link serveOrgClientStream}; attribution rides the
 * `OrgCaller` via {@link projectVerifiedCaller}.
 *
 * **Handler-drop contract (§2.2 / F-S3.1-2, stated level):** see
 * {@link serveOrgStreaming} — the retire supervisor may drop the
 * handler future without a final poll; cancellation is observed through
 * the retirement observables, never assumed as a handler-side event.
 * The sink is released by the Rust bridge when the handler settles or
 * its future drops.
 */
export function serveOrgDuplex<Req = unknown, Resp = unknown>(
  mesh: MeshNode | NetMesh,
  service: string,
  access: OrgAccess,
  handler: TypedOrgDuplexHandler<Req, Resp>,
  handlerTimeoutMs?: number,
): OrgServeHandle {
  try {
    return serveOrgDuplexTyped<Req, Resp>(
      nativeMesh(mesh),
      service,
      access,
      (caller, requests, sink) =>
        handler(projectVerifiedCaller(caller), requests, sink),
      handlerTimeoutMs,
    )
  } catch (e) {
    throw classifyOrgError(e)
  }
}
