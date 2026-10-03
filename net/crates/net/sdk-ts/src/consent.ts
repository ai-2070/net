/**
 * Consent and pins — which mesh capabilities this caller may use.
 *
 * A capability is named by its `provider/capability` id
 * ({@link CapabilityId}). A {@link ConsentPolicy} decides in memory whether a
 * call needs approval; a {@link PinStore} is the machine-shared, file-backed
 * record of what the operator has approved (`request` is pending-only and
 * never upgrades). {@link CapabilityGateway} is the consent-gated caller that
 * ties them together.
 *
 * Thin re-exports of `@net-mesh/core`, mirroring Python's `net_sdk.consent`.
 * Present when the native module was built with the `consent` feature
 * (`CapabilityGateway` additionally needs `payments`); published packages
 * ship every feature.
 *
 * `CapabilityGateway`'s constructor takes a **native** `NetMesh`, which a
 * {@link MeshNode} does not hand out: from the SDK, build it with
 * `createCapabilityGateway(meshNode, options)` (`./payments`), which adapts
 * the handle and returns this same native class.
 *
 * @example
 * ```typescript
 * import { PinStore } from '@net-mesh/sdk';
 *
 * const pins = new PinStore('/var/lib/net/pins.json');
 * if ((await pins.request('acme/search')) === 'pending') {
 *   // ask the operator, then: await pins.approve('acme/search')
 * }
 * ```
 */

export {
  CapabilityGateway,
  CapabilityId,
  ConsentPolicy,
  PinStore,
  credentialRequiresConsent,
} from '@net-mesh/core';
export type { PinRecordJs as PinRecord } from '@net-mesh/core';
