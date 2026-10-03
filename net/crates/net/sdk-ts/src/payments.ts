/**
 * Payments and paid agent-to-agent tasks over the SDK's {@link MeshNode}
 * (`docs/internal/plans/NODE_A2A_PAID_ADMISSION_PLAN.md` D7 / WS-E).
 *
 * The native `PaymentProvider` and `CapabilityGateway` take a native
 * `NetMesh`, which a {@link MeshNode} deliberately does not hand out — so
 * without this module an SDK user could not construct either. This module
 * **adapts handles and nothing else**: each factory resolves a `MeshNode` to
 * its native mesh (a native `NetMesh` passes straight through), maps named
 * options onto the native constructor, and returns the **native** object.
 * No method is re-implemented or forwarded, so the SDK cannot drift from the
 * binding — every verb, document and refusal is the native one.
 *
 * Errors are the native ones: plain `Error`s with stable prefixes until
 * {@link classifyError} turns them into `PaymentRefusedError`,
 * `JournalOwnedElsewhereError` or `A2aInvalidArgumentError`. Exporting the
 * classes converts nothing on its own.
 *
 * **Read nested documents with {@link a2aDocument} / {@link a2aU64}, never
 * `JSON.parse` + `JSON.stringify`.** Paid-A2A documents carry u64 integers
 * (provider node ids, generations, quote expiries) that a JS double rounds.
 *
 * **Lifecycle.** The returned objects retain the node: `stop()` serve handles
 * and `close()` providers and gateways before `MeshNode.shutdown()`.
 *
 * @example
 * ```typescript
 * import {
 *   MeshNode, createPaymentProvider, createCapabilityGateway, a2aDocument,
 * } from '@net-mesh/sdk';
 *
 * const provider = createPaymentProvider(node, {
 *   statePath: 'state/engine.json',
 *   facilitatorUrl: 'https://facilitator.example.com',
 * });
 * const gateway = createCapabilityGateway(caller, {
 *   paymentPolicyPath: 'state/spend-policy.json',
 *   paymentProfile: 'production',
 *   a2aPurchasePath: 'state/a2a-purchases.json',
 * });
 * const env = await gateway.prepareTask(providerNodeId, 'summarize', prompt);
 * const prepared = a2aDocument(env, '/prepared');
 * const bought = JSON.parse(await gateway.purchaseTask(prepared)); // status only
 * await gateway.submitTask(prepared);
 * ```
 *
 * @packageDocumentation
 */

import {
  CapabilityGateway as NapiCapabilityGateway,
  PaymentProvider as NapiPaymentProvider,
} from '@net-mesh/core';
import type { NetMesh as NapiNetMesh, OrgClient as NapiOrgClient } from '@net-mesh/core';
import type { TypedOrgClient } from '@net-mesh/core/org';

import { getNapiMesh, nativeOrgClientOf } from './_internal.js';
import { MeshNode } from './mesh.js';
import type { OrgClient } from './org/index.js';

export { a2aDocument, a2aU64 } from '@net-mesh/core';
export {
  A2aInvalidArgumentError,
  JournalOwnedElsewhereError,
  PaymentRefusedError,
  classifyError,
} from '@net-mesh/core/errors';

/** A per-scheme external signer: the payer address and the callback that
 *  signs its typed intent (the artifact crosses; keys never do). */
export interface PaymentSignerOptions {
  address: string;
  sign: (intentJson: string) => Promise<string>;
}

/** Named options for {@link createPaymentProvider}. */
export interface PaymentProviderOptions {
  /** The settlement store — durable and single-owner. */
  statePath: string;
  /** Records the immutable `net.billing.event@1` stream. */
  billingLogPath?: string;
  /** A real facilitator. Exactly one of this and `unsafeDevMockFacilitator`. */
  facilitatorUrl?: string;
  facilitatorAuthToken?: string;
  /** The in-process mock, which moves no value. Development only. */
  unsafeDevMockFacilitator?: boolean;
  /** Defaults to `true` natively. */
  requireInvocationBinding?: boolean;
}

/** Named options for {@link createCapabilityGateway}. */
export interface CapabilityGatewayOptions {
  /** The machine-shared pin store (`net mcp pin`'s file). */
  pinStorePath?: string;
  /** The shared spend-policy store every outbound payment reserves against. */
  paymentPolicyPath?: string;
  paymentProfile?: string;
  paymentUnsafeMockAutoAllow?: boolean;
  /** eip155 (EVM) signer. */
  paymentSigner?: PaymentSignerOptions;
  /** solana signer. */
  paymentSignerSvm?: PaymentSignerOptions;
  /** xrpl signer. */
  paymentSignerXrpl?: PaymentSignerOptions;
  /** The durable paid-A2A purchase store; needs `paymentPolicyPath`. */
  a2aPurchasePath?: string;
}

/** The native constructor arguments after the mesh. */
type ProviderCtorTail = ConstructorParameters<typeof NapiPaymentProvider> extends [
  unknown,
  ...infer R,
]
  ? R
  : never;
type GatewayCtorTail = ConstructorParameters<typeof NapiCapabilityGateway> extends [
  unknown,
  ...infer R,
]
  ? R
  : never;

// Compile-time guard: the option mappings below name every native
// constructor argument. A native constructor that gains (or loses) one
// fails `tsc` here, instead of a user finding the option silently missing.
type Equal<A, B> = (<T>() => T extends A ? 1 : 2) extends <T>() => T extends B ? 1 : 2
  ? true
  : false;
const PROVIDER_ARGS_AFTER_MESH = 6;
const GATEWAY_ARGS_AFTER_MESH = 11;
const _providerArity: Equal<
  Required<ProviderCtorTail>['length'],
  typeof PROVIDER_ARGS_AFTER_MESH
> = true;
const _gatewayArity: Equal<
  Required<GatewayCtorTail>['length'],
  typeof GATEWAY_ARGS_AFTER_MESH
> = true;
void _providerArity;
void _gatewayArity;

function nativeMesh(mesh: MeshNode | NapiNetMesh): NapiNetMesh {
  return mesh instanceof MeshNode ? getNapiMesh(mesh) : mesh;
}

/** @internal The positional native arguments for these options. */
export function providerArgs(options: PaymentProviderOptions): ProviderCtorTail {
  return [
    options.statePath,
    options.billingLogPath,
    options.facilitatorUrl,
    options.facilitatorAuthToken,
    options.unsafeDevMockFacilitator,
    options.requireInvocationBinding,
  ];
}

/** @internal The positional native arguments for these options. */
export function gatewayArgs(options: CapabilityGatewayOptions): GatewayCtorTail {
  return [
    options.pinStorePath,
    options.paymentPolicyPath,
    options.paymentProfile,
    options.paymentUnsafeMockAutoAllow,
    options.paymentSigner?.address,
    options.paymentSigner?.sign,
    options.paymentSignerSvm?.address,
    options.paymentSignerSvm?.sign,
    options.paymentSignerXrpl?.address,
    options.paymentSignerXrpl?.sign,
    options.a2aPurchasePath,
  ];
}

/**
 * A native `PaymentProvider` over `mesh` (the SDK's {@link MeshNode} or a
 * native `NetMesh`). Throws exactly what the native constructor throws.
 */
export function createPaymentProvider(
  mesh: MeshNode | NapiNetMesh,
  options: PaymentProviderOptions,
): NapiPaymentProvider {
  return new NapiPaymentProvider(nativeMesh(mesh), ...providerArgs(options));
}

/**
 * A native `CapabilityGateway` over `mesh` (the SDK's {@link MeshNode} or a
 * native `NetMesh`). Throws exactly what the native constructor throws.
 */
export function createCapabilityGateway(
  mesh: MeshNode | NapiNetMesh,
  options: CapabilityGatewayOptions = {},
): NapiCapabilityGateway {
  return new NapiCapabilityGateway(nativeMesh(mesh), ...gatewayArgs(options));
}

/**
 * Install (or clear with `null`) the organization identity paid A2A presents
 * to a PROTECTED provider.
 *
 * `target` picks the slot: a {@link MeshNode} or native `NetMesh` sets the
 * mesh's own (the raw requester verbs — `describeA2a`, `submitTask`,
 * `submitTaskPaid`, `taskStatus`, `cancelTask`); a `CapabilityGateway` sets the
 * gateway's (its `prepareTask` → `purchaseTask` → `submitTask` lifecycle).
 * Setting one does not set the other. `org` may be the SDK's
 * {@link OrgClient}, a `TypedOrgClient`, or the native client.
 */
export function setA2aOrgCaller(
  target: MeshNode | NapiNetMesh | NapiCapabilityGateway,
  org: OrgClient | TypedOrgClient | NapiOrgClient | null,
): void {
  const native = nativeOrgClientOf(org) as NapiOrgClient | null;
  if (target instanceof NapiCapabilityGateway) {
    target.setA2aOrgCaller(native);
    return;
  }
  nativeMesh(target).setA2aOrgCaller(native);
}
