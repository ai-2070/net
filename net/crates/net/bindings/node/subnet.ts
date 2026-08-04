/**
 * Subnet authority for Node (SSDK S4a, `SUBNET_AUTH_SDK_PLAN.md` §6.1).
 *
 * Two layers, deliberately separated:
 *
 * **The ordinary application layer is one verb** (plus the caller verb,
 * `org.callExported`, which lives on the org client where it belongs):
 *
 * ```ts
 * const mesh = await NetMesh.create({
 *   // …,
 *   subnetAuthorities: [{ authorityHex, rootHexes, maximumGrantLifetimeSecs }],
 *   subnetAttachment: { levels: [3] },
 *   subnetExports: [{
 *     name: 'factory-export', access: 'granted',
 *     binding: { subnet: { authorityHex, path: { levels: [3, 9] } }, topologyEpoch: 0 },
 *   }],
 * })
 * const handle = serveSubnetExportedTyped(mesh, 'fleet.telemetry', 'factory-export',
 *   async (caller, req: Telemetry) => answer(caller, req))
 * ```
 *
 * The provider names a service and a locally configured export — it
 * constructs no roots, credentials, boundaries, epochs, or refs. The
 * export name is provider-local configuration: never announced, never
 * accepted from callers.
 *
 * **Everything else is administration**, under {@link admin}: installing
 * gateway credential-set bytes (wholesale replace), declaring
 * boundaries (also wholesale), and applying signed control facts (the
 * one door, floors included). Signed artifacts are minted by
 * `net-mesh subnet …` and cross as opaque canonical wire `Buffer`s —
 * nothing on this surface signs.
 *
 * Errors carry the stable `subnet:<kind>` envelope; see
 * {@link SubnetProvisionError} in `./errors`.
 */

import {
  applySubnetControlFact as nativeApplyControlFact,
  declareSubnetBoundaries as nativeDeclareBoundaries,
  installSubnetGatewayCredentials as nativeInstallGatewayCredentials,
  serveSubnetExported as nativeServeSubnetExported,
} from './index'
import type {
  OrgCaller,
  OrgRequest,
  OrgServeHandle,
  SubnetAuthorityConfigJs,
  SubnetBoundaryDeclarationJs,
  SubnetControlOutcomeJs,
  SubnetExportBindingJs,
  SubnetNamedExportJs,
  SubnetPathJs,
  SubnetRefJs,
} from './index'
import { classifyError, classifySubnetError } from './errors'

export { classifySubnetError }
export { SubnetProvisionError } from './errors'
export type {
  SubnetAuthorityConfigJs as SubnetAuthorityConfig,
  SubnetBoundaryDeclarationJs as SubnetBoundaryDeclaration,
  SubnetControlOutcomeJs as SubnetControlOutcome,
  SubnetExportBindingJs as SubnetExportBinding,
  SubnetNamedExportJs as SubnetNamedExport,
  SubnetPathJs as SubnetPath,
  SubnetRefJs as SubnetRef,
}

// ---------------------------------------------------------------------------
// Administration — explicitly advanced (plan §6)
// ---------------------------------------------------------------------------

/**
 * Runtime subnet administration. Operator surface, deliberately not
 * placed beside ordinary service calls — the ordinary application never
 * performs these.
 */
export const admin = {
  /**
   * Decode and install this node's own gateway credential sets —
   * WHOLESALE REPLACE: pass every currently held set, not a delta.
   * Every artifact decodes before anything installs, so one malformed
   * `Buffer` in the batch mutates no node state at all.
   */
  installGatewayCredentials(mesh: unknown, credentialSets: readonly Buffer[]): void {
    try {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      nativeInstallGatewayCredentials(mesh as any, credentialSets as Buffer[])
    } catch (e) {
      throw classifySubnetError(e)
    }
  },

  /**
   * Declare this node's protected boundary inventory — also wholesale:
   * the set replaces the previous declaration.
   */
  declareBoundaries(mesh: unknown, declaration: SubnetBoundaryDeclarationJs): void {
    try {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      nativeDeclareBoundaries(mesh as any, declaration)
    } catch (e) {
      throw classifySubnetError(e)
    }
  },

  /**
   * Apply one signed control fact from its outer wire frame — the ONE
   * door for floors and descriptive facts alike. `applied: false` is an
   * authenticated stale/idempotent outcome, not a failure.
   */
  applyControlFact(mesh: unknown, fact: Buffer): SubnetControlOutcomeJs {
    try {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      return nativeApplyControlFact(mesh as any, fact)
    } catch (e) {
      throw classifySubnetError(e)
    }
  },
}

// ---------------------------------------------------------------------------
// The ordinary provider verb
// ---------------------------------------------------------------------------

function encode(value: unknown): Buffer {
  return Buffer.from(new TextEncoder().encode(JSON.stringify(value)))
}

function decode<T>(bytes: Buffer): T {
  return JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(bytes)) as T
}

/**
 * A JSON-typed subnet-exported handler — the same provider-verified
 * {@link OrgCaller} shape as `serveOrgTyped`, because the admitted
 * facts are the same admission engine's.
 */
export type TypedSubnetExportedHandler<Req = unknown, Resp = unknown> = (
  caller: OrgCaller,
  req: Req,
) => Resp | Promise<Resp>

/**
 * Serve a subnet-exported, organization-protected service against a
 * NAMED export configured at mesh construction, with a JSON codec.
 *
 * An unknown export name fails HERE — before anything is registered or
 * announced (`subnet:unknown_export_name` rides the error). Dispatch
 * revalidates the exact crossing against this node's live gateway
 * authority on every call, before organization admission; announcement
 * visibility is always public, and the external caller never joins this
 * node's subnet.
 */
export function serveSubnetExportedTyped<Req = unknown, Resp = unknown>(
  mesh: unknown,
  service: string,
  exportName: string,
  handler: TypedSubnetExportedHandler<Req, Resp>,
  handlerTimeoutMs?: number,
): OrgServeHandle {
  try {
    return nativeServeSubnetExported(
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      mesh as any,
      service,
      exportName,
      async (req: OrgRequest): Promise<Buffer> => {
        const decoded = decode<Req>(req.request)
        const resp = await handler(req.caller, decoded)
        return encode(resp)
      },
      handlerTimeoutMs,
    )
  } catch (e) {
    // Registration failures ride a plain message with the stable kind
    // inside; route through the general classifier so a `subnet:`-
    // prefixed refusal classifies and anything else passes through.
    throw classifyError(e)
  }
}
