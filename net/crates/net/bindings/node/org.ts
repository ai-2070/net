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

import { OrgClient as NativeOrgClient, OrgCredentials } from './index'

export { OrgCredentials }
export type { OrgCredentialsOptions } from './index'

// ---------------------------------------------------------------------------
// Error taxonomy — four domains plus the parser fallback
// ---------------------------------------------------------------------------

const ERR_ORG_PREFIX = 'org:'

/**
 * Base for every organization error.
 *
 * The `domain` is the load-bearing fact: it says WHERE the refusal happened.
 * Use {@link OrgError.isLocal} rather than inspecting the message.
 */
export class OrgError extends Error {
  /** Wire domain token: `credentials` | `discovery` | `admission_denied` | `rpc` | `unknown`. */
  readonly domain: string
  /** Wire kind token within the domain, when one could be parsed. */
  readonly kind?: string

  constructor(message: string, domain: string, kind?: string) {
    super(message)
    this.name = 'OrgError'
    this.domain = domain
    this.kind = kind
  }

  /**
   * Whether the request never left this process.
   *
   * `true` for credential and discovery failures — nothing was sent, so retry
   * and audit semantics are entirely local. `false` for everything else,
   * INCLUDING `unknown`, which claims nothing either way.
   */
  get isLocal(): boolean {
    return this.domain === 'credentials' || this.domain === 'discovery'
  }
}

/** Local: the credential set could not authorize this call. Nothing was sent. */
export class OrgCredentialsError extends OrgError {
  constructor(message: string, kind?: string) {
    super(message, 'credentials', kind)
    this.name = 'OrgCredentialsError'
  }
}

/** Local: no provider this credential set may call was found. Nothing was sent. */
export class OrgDiscoveryError extends OrgError {
  constructor(message: string, kind?: string) {
    super(message, 'discovery', kind)
    this.name = 'OrgDiscoveryError'
  }
}

/**
 * Remote: the provider's admission engine refused the call.
 *
 * `reason` is one of three coarse buckets by design — a precise remote reason
 * would be a credential oracle. Do not infer more from timing or retries.
 */
export class OrgAdmissionDeniedError extends OrgError {
  /** `denied` | `not_supported` | `unavailable`. */
  readonly reason: string
  constructor(message: string, reason: string) {
    super(message, 'admission_denied', reason)
    this.name = 'OrgAdmissionDeniedError'
    this.reason = reason
  }
}

/**
 * The vocabulary could not be parsed.
 *
 * This binding and the native module disagree about the `org:` contract — an
 * internal compatibility failure, not an admission result. It deliberately does
 * NOT impersonate one of the four domains: reporting `admission_denied` for a
 * string we could not parse would assert that a request reached a provider and
 * its admission engine evaluated it.
 */
export class OrgUnclassifiedError extends OrgError {
  constructor(message: string) {
    super(message, 'unknown')
    this.name = 'OrgUnclassifiedError'
  }
}

/**
 * Reclassify a thrown error into the org taxonomy.
 *
 * Mirrors Rust's `parse_org_wire`, and is pinned against it by
 * `tests/cross_lang_org/error_vectors.json`. Anything that is not an `org:`
 * string with a domain this build knows becomes {@link OrgUnclassifiedError}.
 */
export function classifyOrgError(e: unknown): unknown {
  const msg = e instanceof Error ? e.message : typeof e === 'string' ? e : ''
  if (!msg.startsWith(ERR_ORG_PREFIX)) return e

  const rest = msg.slice(ERR_ORG_PREFIX.length)
  const firstColon = rest.indexOf(':')
  if (firstColon <= 0) return new OrgUnclassifiedError(msg)

  const domain = rest.slice(0, firstColon)
  const afterDomain = rest.slice(firstColon + 1)
  const secondColon = afterDomain.indexOf(':')
  const kind = secondColon === -1 ? afterDomain : afterDomain.slice(0, secondColon)
  if (kind.length === 0) return new OrgUnclassifiedError(msg)

  switch (domain) {
    case 'credentials':
      return new OrgCredentialsError(msg, kind)
    case 'discovery':
      return new OrgDiscoveryError(msg, kind)
    case 'admission_denied':
      return new OrgAdmissionDeniedError(msg, kind)
    case 'rpc':
      // `org:rpc:` reuses the frozen nRPC kind vocabulary rather than minting
      // second names, so the kind is already meaningful to nRPC consumers.
      return new OrgError(msg, 'rpc', kind)
    default:
      // Includes a literal `unknown` domain: that is a fallback classification,
      // never something a peer asserts.
      return new OrgUnclassifiedError(msg)
  }
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
