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
  installOrgAuthority,
  installProviderGrantAudience,
} from './index'
import type { OrgCaller, OrgRequest, OrgServeHandle } from './index'
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
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      return new TypedOrgClient(NativeOrgClient.bind(mesh as any, credentials))
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
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      mesh as any,
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
