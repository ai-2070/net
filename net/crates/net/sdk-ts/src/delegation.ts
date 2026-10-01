/**
 * Delegated agent identity: the `root → machine → gateway → subagent` chain.
 *
 * {@link DelegationChain} carries authority down from a root identity;
 * {@link RevocationRegistry} holds per-issuer revocation floors, so revoking
 * a machine kills its gateway and subagents but not a sibling.
 * {@link deriveChildIdentity} derives deterministic child keys, separated by
 * label and parent.
 *
 * **Identity boundary.** These take and return the *native* identity
 * (`@net-mesh/core`'s `Identity`). Pass an SDK `Identity` as
 * `identity.toNapi()`, and wrap a returned one with `Identity.fromNapi(…)`.
 *
 * Thin re-exports of `@net-mesh/core`, mirroring Python's
 * `net_sdk.delegation`. Present when the native module was built with the
 * `delegation` feature; published packages ship every feature.
 *
 * **Errors, two families.** A bad argument (an entity id that isn't 32 bytes,
 * a revocation floor that would go backwards) throws an `Error` whose message
 * starts `delegation: `: see {@link isDelegationError}. A malformed or
 * unverifiable *chain* (`DelegationChain.fromBytes`) throws with the token
 * taxonomy instead, `token: <kind>` (for example `token: invalid_format`), the
 * same kinds as the SDK's `TokenErrorKind`.
 *
 * @example
 * ```typescript
 * import { DelegationChain, Identity, RevocationRegistry, deriveChildIdentity } from '@net-mesh/sdk';
 *
 * const root = Identity.generate().toNapi();
 * const machine = deriveChildIdentity(root, 'machine:hostA');
 * const gateway = deriveChildIdentity(root, 'gateway:hostA:agent');
 * const chain = DelegationChain.deriveGateway(root, machine, gateway, 3600);
 * chain.verify(gateway.entityId, root.entityId, new RevocationRegistry()); // true
 * ```
 */

export {
  DelegationChain,
  GATEWAY_DELEGATION_CHANNEL,
  RevocationRegistry,
  defaultRevocationStorePath,
  deriveChildIdentity,
} from '@net-mesh/core';

/** The prefix every native delegation failure carries. */
export const DELEGATION_ERROR_PREFIX = 'delegation: ';

/**
 * Whether `err` is a delegation *argument* error: a plain `Error` with the
 * `delegation: ` message prefix. Chain-format failures are token errors
 * (`token: <kind>`), not these; see the module docs.
 */
export function isDelegationError(err: unknown): err is Error {
  return err instanceof Error && err.message.startsWith(DELEGATION_ERROR_PREFIX);
}
