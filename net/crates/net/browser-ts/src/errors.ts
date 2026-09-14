/**
 * The leaf's error taxonomy, mirrored in TypeScript.
 *
 * One class per variant of `net_leaf::error::LeafError`, with the two
 * nested enums (`RtcError`, `RpcError`) flattened into `kind` so a
 * caller — or a Playwright assertion — can branch on one stable
 * string. `message` is verbatim the Rust `Display` text that crossed
 * the wasm boundary, so nothing is lost in the re-typing;
 * `errors.test.ts` pins that round-trip.
 *
 * **The failure-typing correction lives in this file's types.** There
 * is no way to build a {@link RtcErrorFailure} of type `udpBlocked`
 * without a {@link UdpBlockedEvidence}, and `udpBlockedEvidence()` —
 * the mirror of Rust's `UdpBlockedEvidence::new` — returns `null`
 * unless both observations hold. An ICE timeout on its own therefore
 * cannot become `udp-blocked` anywhere in this package.
 */

/** Every error kind the leaf can surface, one per Rust variant. */
export type LeafErrorKind =
  | 'wire'
  | 'session'
  | 'control-plane'
  | 'identity'
  | 'not-leader'
  | 'ice-timeout'
  | 'udp-blocked'
  | 'channel-closed'
  | 'rtc-unsupported'
  | 'rpc-refused'
  | 'rpc-timeout'
  | 'session-lost'
  | 'leader-lost'
  | 'rpc-indeterminate'
  | 'rpc-malformed'
  | 'unknown';

/**
 * The two observations that, together, distinguish blocked UDP from
 * an anchor that simply is not there. Mirrors
 * `net_leaf::error::UdpBlockedEvidence`.
 *
 * The field types are the literal `true`, not `boolean`: a value of
 * this type is itself the proof, so a caller cannot hand one over
 * with `bootstrapOk: false`.
 */
export interface UdpBlockedEvidence {
  /** The anchor answered over HTTPS — it is up and addressable. */
  readonly bootstrapOk: true;
  /** A STUN binding to the `rtc_addr` that anchor published went unanswered. */
  readonly stunProbeFailed: true;
  /** The address the probe was aimed at, so the claim names its subject. */
  readonly probed: string;
}

/**
 * `UdpBlockedEvidence` only when both observations hold; `null`
 * otherwise, which is the caller's instruction to stay with
 * `ice-timeout`. Mirror of `UdpBlockedEvidence::new`.
 */
export function udpBlockedEvidence(
  bootstrapOk: boolean,
  stunProbeFailed: boolean,
  probed: string,
): UdpBlockedEvidence | null {
  if (!bootstrapOk || !stunProbeFailed || probed.length === 0) return null;
  return { bootstrapOk: true, stunProbeFailed: true, probed };
}

/** Why an RTC attempt did not produce a DataChannel. */
export type RtcErrorFailure =
  | { readonly type: 'iceTimeout' }
  | { readonly type: 'udpBlocked'; readonly evidence: UdpBlockedEvidence }
  | { readonly type: 'channelClosed'; readonly detail: string }
  | { readonly type: 'unsupported'; readonly detail: string };

/** How an nRPC call ended when it did not return a reply. */
export type RpcErrorFailure =
  | { readonly type: 'refused'; readonly status: number; readonly message: string }
  | { readonly type: 'timeout' }
  | { readonly type: 'sessionLost' }
  | { readonly type: 'leaderLost'; readonly generation: number }
  | { readonly type: 'indeterminate'; readonly deadlineMs: number }
  | { readonly type: 'malformed'; readonly detail: string };

/** The base class of everything this package rejects with. */
export abstract class LeafError extends Error {
  /** The flat, stable discriminant. */
  abstract readonly kind: LeafErrorKind;

  protected constructor(message: string) {
    super(message);
    this.name = 'LeafError';
  }
}

/** The wire layer refused a packet: framing, AEAD, replay window. */
export class WireError extends LeafError {
  readonly kind = 'wire' as const;

  constructor(readonly detail: string) {
    super(`wire: ${detail}`);
    this.name = 'WireError';
  }
}

/** No session for a peer, an incomplete handshake, a replaced session. */
export class SessionError extends LeafError {
  readonly kind = 'session' as const;

  constructor(readonly detail: string) {
    super(`session: ${detail}`);
    this.name = 'SessionError';
  }
}

/** The control plane could not carry what the leaf asked it to. */
export class ControlPlaneError extends LeafError {
  readonly kind = 'control-plane' as const;

  constructor(readonly detail: string) {
    super(`control plane: ${detail}`);
    this.name = 'ControlPlaneError';
  }
}

/** Identity storage, generation or custody. */
export class IdentityError extends LeafError {
  readonly kind = 'identity' as const;

  constructor(readonly detail: string) {
    super(`identity: ${detail}`);
    this.name = 'IdentityError';
  }
}

/**
 * This tab is not the leader for the origin and the operation needs
 * the node. Carries the generation the caller presented so a stale
 * follower's failure is legible.
 */
export class NotLeaderError extends LeafError {
  readonly kind = 'not-leader' as const;

  constructor(
    readonly presented: number,
    readonly current: number | null = null,
  ) {
    super(
      current === null
        ? `not the leader: this tab holds generation ${presented}`
        : `not the leader: this tab holds generation ${presented}, the leader holds ${current}`,
    );
    this.name = 'NotLeaderError';
  }
}

/** The RTC transport itself. `kind` is the flattened failure. */
export class RtcError extends LeafError {
  readonly kind: 'ice-timeout' | 'udp-blocked' | 'channel-closed' | 'rtc-unsupported';

  constructor(readonly failure: RtcErrorFailure) {
    super(`rtc: ${rtcDisplay(failure)}`);
    this.name = 'RtcError';
    this.kind = RTC_KINDS[failure.type];
  }
}

/** An nRPC call failed or was disposed of. `kind` is the flattened failure. */
export class RpcError extends LeafError {
  readonly kind:
    | 'rpc-refused'
    | 'rpc-timeout'
    | 'session-lost'
    | 'leader-lost'
    | 'rpc-indeterminate'
    | 'rpc-malformed';

  constructor(readonly failure: RpcErrorFailure) {
    super(`rpc: ${rpcDisplay(failure)}`);
    this.name = 'RpcError';
    this.kind = RPC_KINDS[failure.type];
  }
}

/**
 * Something crossed the boundary that this taxonomy does not
 * recognise — a browser exception, or a leaf `Display` string this
 * version of the package predates.
 *
 * Deliberately not folded into a neighbouring variant: mis-typing a
 * failure is the exact mistake the Stage 5 correction exists to
 * prevent, so an unrecognised message stays unrecognised.
 */
export class UnknownLeafError extends LeafError {
  readonly kind = 'unknown' as const;

  constructor(
    message: string,
    /** The value as it crossed the boundary, for diagnosis. */
    readonly cause?: unknown,
  ) {
    super(message);
    this.name = 'UnknownLeafError';
  }
}

const RTC_KINDS = {
  iceTimeout: 'ice-timeout',
  udpBlocked: 'udp-blocked',
  channelClosed: 'channel-closed',
  unsupported: 'rtc-unsupported',
} as const;

const RPC_KINDS = {
  refused: 'rpc-refused',
  timeout: 'rpc-timeout',
  sessionLost: 'session-lost',
  leaderLost: 'leader-lost',
  indeterminate: 'rpc-indeterminate',
  malformed: 'rpc-malformed',
} as const;

/** The exact text `RtcError`'s Rust `Display` produces. */
const ICE_TIMEOUT_DISPLAY =
  'ICE did not connect inside the deadline (this does not establish that UDP is blocked)';

function rtcDisplay(failure: RtcErrorFailure): string {
  switch (failure.type) {
    case 'iceTimeout':
      return ICE_TIMEOUT_DISPLAY;
    case 'udpBlocked':
      return (
        "UDP appears blocked: the anchor's HTTPS bootstrap succeeded " +
        `but a STUN binding to ${failure.evidence.probed} was unanswered`
      );
    case 'channelClosed':
      return `the DataChannel closed: ${failure.detail}`;
    case 'unsupported':
      return `this browser refused the attempt: ${failure.detail}`;
  }
}

function rpcDisplay(failure: RpcErrorFailure): string {
  switch (failure.type) {
    case 'refused':
      return `refused (${failure.status}): ${failure.message}`;
    case 'timeout':
      return "the call's deadline elapsed";
    case 'sessionLost':
      return 'the session carrying the call went away';
    case 'leaderLost':
      return `the leader holding generation ${failure.generation} was replaced`;
    case 'indeterminate':
      return (
        `the local deadline of ${failure.deadlineMs}ms elapsed before the tab running ` +
        'the node answered; the remote operation may still have executed (it was not retried)'
      );
    case 'malformed':
      return `the reply did not decode: ${failure.detail}`;
  }
}

/**
 * Re-type whatever the wasm boundary threw.
 *
 * `wasm-bindgen` hands us a `JsError` whose `message` is the Rust
 * `Display` of `LeafError`, so the taxonomy is recovered by parsing
 * exactly those prefixes. Anything else — a `DOMException` from
 * `RTCPeerConnection`, a string, a plain object — becomes
 * {@link UnknownLeafError} with the original value as `cause`.
 */
export function fromWasmError(thrown: unknown): LeafError {
  if (thrown instanceof LeafError) return thrown;
  const message = messageOf(thrown);
  return parseLeafError(message) ?? new UnknownLeafError(message, thrown);
}

function messageOf(thrown: unknown): string {
  if (typeof thrown === 'string') return thrown;
  if (thrown instanceof Error) return thrown.message;
  if (thrown !== null && typeof thrown === 'object' && 'message' in thrown) {
    const message = thrown.message;
    if (typeof message === 'string') return message;
  }
  return String(thrown);
}

/**
 * Parse one `LeafError` `Display` string, or `null` if it is not one.
 *
 * Exported because it is the whole of the wasm→TS error contract and
 * deserves to be unit-testable without a wasm module.
 */
export function parseLeafError(message: string): LeafError | null {
  const rtc = after(message, 'rtc: ');
  if (rtc !== null) {
    const failure = parseRtcFailure(rtc);
    return failure === null ? null : new RtcError(failure);
  }

  const rpc = after(message, 'rpc: ');
  if (rpc !== null) {
    const failure = parseRpcFailure(rpc);
    return failure === null ? null : new RpcError(failure);
  }

  const wire = after(message, 'wire: ');
  if (wire !== null) return new WireError(wire);

  const session = after(message, 'session: ');
  if (session !== null) return new SessionError(session);

  const controlPlane = after(message, 'control plane: ');
  if (controlPlane !== null) return new ControlPlaneError(controlPlane);

  const identity = after(message, 'identity: ');
  if (identity !== null) return new IdentityError(identity);

  const notLeader = /^not the leader: this tab holds generation (\d+)(?:, the leader holds (\d+))?$/.exec(
    message,
  );
  if (notLeader) {
    const presented = Number(notLeader[1]);
    const current = notLeader[2] === undefined ? null : Number(notLeader[2]);
    return new NotLeaderError(presented, current);
  }

  return null;
}

/** Parse the nested `RtcError` `Display`, or `null`. */
export function parseRtcFailure(text: string): RtcErrorFailure | null {
  if (text === ICE_TIMEOUT_DISPLAY) return { type: 'iceTimeout' };

  const blocked = /^UDP appears blocked: the anchor's HTTPS bootstrap succeeded but a STUN binding to (.+) was unanswered$/.exec(
    text,
  );
  if (blocked) {
    const probed = blocked[1] ?? '';
    const evidence = udpBlockedEvidence(true, true, probed);
    // The regex matched the message the Rust side only emits when it
    // holds the evidence, so `probed` is non-empty and this is
    // `UdpBlockedEvidence`; an empty subject is not evidence.
    if (evidence === null) return null;
    return { type: 'udpBlocked', evidence };
  }

  const closed = after(text, 'the DataChannel closed: ');
  if (closed !== null) return { type: 'channelClosed', detail: closed };

  const unsupported = after(text, 'this browser refused the attempt: ');
  if (unsupported !== null) return { type: 'unsupported', detail: unsupported };

  return null;
}

/** Parse the nested `RpcError` `Display`, or `null`. */
export function parseRpcFailure(text: string): RpcErrorFailure | null {
  const refused = /^refused \((\d+)\): ([\s\S]*)$/.exec(text);
  if (refused) {
    return { type: 'refused', status: Number(refused[1]), message: refused[2] ?? '' };
  }
  if (text === "the call's deadline elapsed") return { type: 'timeout' };
  if (text === 'the session carrying the call went away') return { type: 'sessionLost' };

  const leaderLost = /^the leader holding generation (\d+) was replaced$/.exec(text);
  if (leaderLost) return { type: 'leaderLost', generation: Number(leaderLost[1]) };

  // A follower's own deadline, not the node's. The distinction is
  // the whole point: `rpc-timeout` says the call died on the leaf's
  // deadline, this says nobody knows — the request may be executing
  // in the tab that holds the node, and this package does not
  // reissue it.
  const indeterminate = /^the local deadline of (\d+)ms elapsed before the tab running the node answered; the remote operation may still have executed \(it was not retried\)$/.exec(
    text,
  );
  if (indeterminate) return { type: 'indeterminate', deadlineMs: Number(indeterminate[1]) };

  const malformed = after(text, 'the reply did not decode: ');
  if (malformed !== null) return { type: 'malformed', detail: malformed };

  return null;
}

/** `true` when this error carries the UDP-blocked evidence. */
export function isUdpBlocked(
  error: unknown,
): error is RtcError & { failure: { type: 'udpBlocked'; evidence: UdpBlockedEvidence } } {
  return error instanceof RtcError && error.failure.type === 'udpBlocked';
}

function after(text: string, prefix: string): string | null {
  return text.startsWith(prefix) ? text.slice(prefix.length) : null;
}
