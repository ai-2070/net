/**
 * Device enrollment: invite → join → approve, and the device lifecycle.
 *
 * The operator side ({@link OperatorEnrollment}) mints an {@link InviteToken},
 * receives a {@link JoinRequest}, and approves it into a delegation chain,
 * recording a {@link DeviceRecord}. The device side ({@link DeviceEnrollment})
 * holds the granted chain and knows when it needs renewal. Rejections are
 * coded {@link JoinOutcome}s, never thrown. {@link fingerprint} renders an
 * entity id for humans.
 *
 * **Identity boundary.** Entry points that take an identity (the
 * `OperatorEnrollment` constructor and `withDefaultPaths`, `JoinRequest.create`,
 * `DeviceEnrollment`) take the *native* `Identity`: pass an SDK `Identity` as
 * `identity.toNapi()`.
 *
 * The over-the-mesh half (`rendezvousString`, `join`, `renew`,
 * `serveEnrollmentAuto`) lives on `MeshNode`.
 *
 * Thin re-exports of `@net-mesh/core`, mirroring Python's
 * `net_sdk.enrollment`. Present when the native module was built with the
 * `delegation` feature; published packages ship every feature. Failures
 * throw an `Error` whose message starts `enrollment: ` — see
 * {@link isEnrollmentError}.
 */

export {
  DeviceEnrollment,
  DeviceRecord,
  EnrollmentServeHandle,
  InviteToken,
  JoinOutcome,
  JoinRequest,
  OperatorEnrollment,
  fingerprint,
} from '@net-mesh/core';

/** The prefix every native enrollment failure carries. */
export const ENROLLMENT_ERROR_PREFIX = 'enrollment: ';

/**
 * Whether `err` is a failure raised by the native enrollment surface (a plain
 * `Error` with an `enrollment: ` message prefix). Join *rejections* are not
 * errors: they come back as a {@link JoinOutcome} with a `rejectCode`.
 */
export function isEnrollmentError(err: unknown): err is Error {
  return err instanceof Error && err.message.startsWith(ENROLLMENT_ERROR_PREFIX);
}
