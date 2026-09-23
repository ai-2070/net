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
  | 'ice-server-conflict'
  | 'org-admission-denied'
  | 'org-revoked'
  | 'org-timeout'
  | 'org-cancelled'
  | 'org-leader-lost'
  | 'org-session-lost'
  | 'org-indeterminate'
  | 'org-refused'
  | 'org-internal'
  | 'org-malformed'
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
 * A supplied `iceServers` entry named **this connection's own peer**
 * as its STUN server.
 *
 * The mirror of Rust's `LeafError::IceServerConflictsWithPeer`. An
 * anchor's `rtc_addr` is the ICE peer of a connection with that
 * anchor, not a STUN server for it: the connection gathers no
 * server-reflexive candidate and times out. The leaf refuses this
 * **before any ICE work**, and refuses rather than silently
 * stripping the entry — stripping would turn an explicit
 * NAT-traversal configuration into a host-candidate-only attempt
 * while appearing to have accepted it.
 *
 * The fix is to omit `iceServers` and let `connect()` use the
 * endpoint the anchor announces as `stun_addr` on
 * `GET /rtc/anchor`, or to name a STUN server that is not this
 * peer. {@link diagnosticStunUrl} exists for the throwaway UDP
 * probe and is not a source of `iceServers`.
 *
 * **Detection is endpoint equality only**, after default-port
 * normalisation. A URL naming a DNS alias that resolves to the peer
 * is not detected: the leaf resolves no names. The announced
 * endpoint is what makes detection unnecessary for the configuration
 * Net supplies.
 */
export class IceServerConflictError extends LeafError {
  readonly kind = 'ice-server-conflict' as const;

  constructor(
    readonly entry: string,
    readonly peerRtcAddr: string,
  ) {
    super(
      `ice configuration: the iceServers entry ${entry} names this connection's peer RTC ` +
        `endpoint ${peerRtcAddr}; a peer cannot be its own STUN server. Omit iceServers to ` +
        'use the STUN endpoint the anchor announces (the stun_addr field of GET /rtc/anchor), ' +
        'or name a STUN server that is not this peer',
    );
    this.name = 'IceServerConflictError';
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

// ── Organization-scoped streaming (plan §4.3) ────────────────────────────
//
// The typed terminal vocabulary of an org call or stream. The classes
// mirror `classifyOrgError`'s vocabulary from the Node binding where
// names coincide — {@link OrgAdmissionDeniedError} exposes exactly the
// three coarse buckets and nothing finer, because a precise remote
// reason would be a credential oracle (OA2-E2).
//
// The §4.3 mapping, exactly:
//
// - **Opening denial** — the call (or the stream opener) rejects with
//   {@link OrgAdmissionDeniedError} carrying the coarse reason.
// - **Midstream revocation** — the stream's FINAL error item is
//   `AdmissionDenied('denied')`; where the leaf knows the cause was
//   revocation it arrives as {@link OrgRevokedError}, which IS an
//   `OrgAdmissionDeniedError` with `coarse === 'denied'` (the frozen
//   Revoked → Denied coarse map) — a page that speaks §4.3 sees
//   `AdmissionDenied('denied')`, a page that wants the cause checks
//   `instanceof`.
// - **Deadline / cancel retirement** — {@link OrgTimeoutError} /
//   {@link OrgCancelledError}.

/**
 * The frozen coarse vocabulary of an admission denial — the whole of
 * what a remote provider ever says about why. A precise remote reason
 * would be a credential oracle, so the wire carries one of these three
 * bytes and the detailed refusal stays provider-side audit only.
 */
export type CoarseAdmissionReason = 'denied' | 'not-supported' | 'unavailable';

/** Every org terminal kind, one per the leaf's frozen terminal vocabulary. */
export type OrgErrorKind =
  | 'org-admission-denied'
  | 'org-revoked'
  | 'org-timeout'
  | 'org-cancelled'
  | 'org-leader-lost'
  | 'org-session-lost'
  | 'org-indeterminate'
  | 'org-refused'
  | 'org-internal'
  | 'org-malformed';

/**
 * The base class of the org terminal vocabulary — what
 * {@link OrgByteStream}'s terminal error item carries and what the org
 * verbs reject with. `message` is the boundary's own text, verbatim,
 * exactly as the rest of this taxonomy keeps it.
 */
export abstract class OrgStreamError extends LeafError {
  abstract readonly kind: OrgErrorKind;
}

/**
 * The provider's admission engine refused the call (`RpcStatus 0x0009`).
 * `coarse` is the frozen three-bucket reason and nothing finer.
 */
export class OrgAdmissionDeniedError extends OrgStreamError {
  readonly kind: OrgErrorKind = 'org-admission-denied';

  constructor(
    readonly coarse: CoarseAdmissionReason,
    message?: string,
  ) {
    super(message ?? `org:admission_denied:${WIRE_COARSE[coarse]}`);
    this.name = 'OrgAdmissionDeniedError';
  }
}

/**
 * Credentials were revoked mid-call. **Is** an admission denial:
 * `coarse` is fixed to `'denied'` (the frozen Revoked → Denied coarse
 * map), so `instanceof OrgAdmissionDeniedError` and
 * `coarse === 'denied'` hold — `instanceof OrgRevokedError` is how a
 * page that wants the cause gets it.
 */
export class OrgRevokedError extends OrgAdmissionDeniedError {
  override readonly kind: OrgErrorKind = 'org-revoked';

  constructor(message = 'org:admission_denied:denied') {
    super('denied', message);
    this.name = 'OrgRevokedError';
  }
}

/** The call's deadline elapsed. Nothing is re-issued. */
export class OrgTimeoutError extends OrgStreamError {
  readonly kind = 'org-timeout' as const;

  constructor(message = 'org:rpc:timeout') {
    super(message);
    this.name = 'OrgTimeoutError';
  }
}

/** The call was cancelled — by its owner, or by a teardown that owns it. */
export class OrgCancelledError extends OrgStreamError {
  readonly kind = 'org-cancelled' as const;

  constructor(message = 'org:rpc:cancelled') {
    super(message);
    this.name = 'OrgCancelledError';
  }
}

/**
 * The generation holding the call was replaced (the shared-session
 * surface's typed `LeaderLost`). `generation` is the generation that
 * owned the call, as an exact decimal string — the u64 never becomes a
 * JS number here. Never resumed: a successor generation's calls are
 * fresh calls with fresh correlation and fresh proofs.
 */
export class OrgLeaderLostError extends OrgStreamError {
  readonly kind = 'org-leader-lost' as const;

  constructor(
    readonly generation: string,
    message = 'org:rpc:leader_lost',
  ) {
    super(message);
    this.name = 'OrgLeaderLostError';
  }
}

/** The session carrying the call went away. */
export class OrgSessionLostError extends OrgStreamError {
  readonly kind = 'org-session-lost' as const;

  constructor(message = 'org:rpc:session_lost') {
    super(message);
    this.name = 'OrgSessionLostError';
  }
}

/**
 * The caller's own deadline elapsed before an answer arrived; the
 * remote operation may still have executed and was not retried.
 */
export class OrgIndeterminateError extends OrgStreamError {
  readonly kind = 'org-indeterminate' as const;

  constructor(
    readonly deadlineMs: number,
    message = 'org:rpc:indeterminate',
  ) {
    super(message);
    this.name = 'OrgIndeterminateError';
  }
}

/** The application refused the call. `status` is its status code. */
export class OrgRefusedError extends OrgStreamError {
  readonly kind = 'org-refused' as const;

  constructor(
    readonly status: number,
    message = 'org:rpc:refused',
  ) {
    super(message);
    this.name = 'OrgRefusedError';
  }
}

/**
 * The boundary failed internally. Also what a terminal item whose
 * `kind` this build does not know becomes: mis-typing a failure is the
 * exact mistake this taxonomy exists to prevent, so an unknown kind is
 * named internal rather than guessed into a merits bucket.
 */
export class OrgInternalError extends OrgStreamError {
  readonly kind = 'org-internal' as const;

  constructor(message = 'org:rpc:internal') {
    super(message);
    this.name = 'OrgInternalError';
  }
}

/** The reply or item did not decode. */
export class OrgMalformedError extends OrgStreamError {
  readonly kind = 'org-malformed' as const;

  constructor(message = 'org:rpc:malformed') {
    super(message);
    this.name = 'OrgMalformedError';
  }
}

/**
 * Why a call, stream, sink or request stream retired — the exact
 * string the retirement observables resolve with, kept verbatim.
 *
 * `node-closed` and `replaced` are teardown verdicts rather than call
 * outcomes; both arrive at the terminal vocabulary as `cancelled`
 * (the frozen retire → terminal map), and the raw string is what tells
 * them apart.
 */
export type OrgRetireReason =
  | 'timeout'
  | 'cancelled'
  | 'revoked'
  | 'session-lost'
  | 'leader-lost'
  | 'node-closed'
  | 'replaced';

/**
 * THE typed closed refusal of a handler-side sink after retirement —
 * one text whatever the verdict, so the wrapper's belt (its own
 * short-circuit) and the leaf's hardening (its rejection) are the
 * same observable. The verdict itself is what `retired` reports.
 */
export const ORG_SINK_CLOSED_REFUSAL = 'org: the response sink is closed: the call was retired';

const ORG_WIRE_PREFIX = 'org:';

/** The three coarse buckets' wire tokens (`OrgSdkError::to_wire`). */
const WIRE_COARSE: Record<CoarseAdmissionReason, string> = {
  'denied': 'denied',
  'not-supported': 'not_supported',
  'unavailable': 'unavailable',
};

/**
 * Parse one wire token — or the one-byte coarse body a `0x0009`
 * refusal carries — into the coarse bucket. Anything undecodable is
 * the least-informative bucket (`denied`), never an error about an
 * error: the caller still learns it was denied.
 */
export function coarseAdmissionReason(token: string | undefined): CoarseAdmissionReason {
  const text = (token ?? '').trim();
  if (text === 'unavailable') return 'unavailable';
  if (text === 'not_supported' || text === 'not-supported') return 'not-supported';
  // The `map_rpc_error` body: the coarse reason's wire byte 0/1/2, as
  // a raw byte or its decimal digit.
  if (text === '\x00' || text === '0') return 'denied';
  if (text === '\x01' || text === '1') return 'not-supported';
  if (text === '\x02' || text === '2') return 'unavailable';
  return 'denied';
}

/** The terminal error a retirement verdict maps to (the frozen map). */
export function orgRetireError(reason: OrgRetireReason): OrgStreamError {
  switch (reason) {
    case 'timeout':
      return new OrgTimeoutError();
    case 'revoked':
      return new OrgRevokedError();
    case 'session-lost':
      return new OrgSessionLostError();
    case 'leader-lost':
      // The retire signal carries no generation; the call sites that
      // know one build `OrgLeaderLostError` themselves.
      return new OrgLeaderLostError('');
    case 'cancelled':
    case 'node-closed':
    case 'replaced':
      return new OrgCancelledError();
  }
}

/**
 * Type one terminal item from the boundary — the frozen `kind`
 * vocabulary plus its fields — as the class §4.3 names.
 *
 * `message` is kept verbatim as the error's message.
 */
export function orgTerminalError(item: {
  kind: string;
  message?: string;
  coarse?: string;
  generation?: string;
  status?: number;
  deadlineMs?: number;
}): OrgStreamError {
  const message = item.message;
  switch (item.kind) {
    case 'admission-denied':
      return new OrgAdmissionDeniedError(coarseAdmissionReason(item.coarse), message);
    case 'revoked':
      return message === undefined ? new OrgRevokedError() : new OrgRevokedError(message);
    case 'timeout':
      return message === undefined ? new OrgTimeoutError() : new OrgTimeoutError(message);
    case 'cancelled':
      return message === undefined ? new OrgCancelledError() : new OrgCancelledError(message);
    case 'leader-lost':
      return new OrgLeaderLostError(item.generation ?? '', message);
    case 'session-lost':
      return message === undefined ? new OrgSessionLostError() : new OrgSessionLostError(message);
    case 'indeterminate':
      return new OrgIndeterminateError(item.deadlineMs ?? 0, message);
    case 'refused':
      return new OrgRefusedError(item.status ?? 0, message);
    case 'malformed':
      return message === undefined ? new OrgMalformedError() : new OrgMalformedError(message);
    case 'internal':
      return message === undefined ? new OrgInternalError() : new OrgInternalError(message);
    default:
      return new OrgInternalError(
        `org:rpc:internal: unrecognized terminal kind ${JSON.stringify(item.kind)}` +
          (message === undefined ? '' : `: ${message}`),
      );
  }
}

/**
 * Type one retirement verdict coming back from the boundary's
 * `retired()`. A string this build does not know is a boundary
 * disagreement — named `replaced`-as-cancelled would lie about which
 * teardown ran, so it becomes the cancelled terminal and the raw
 * string stays in `OrgRetireReason`'s consumers' hands unchanged.
 */
export function orgRetireReason(raw: string): OrgRetireReason {
  switch (raw) {
    case 'timeout':
    case 'cancelled':
    case 'revoked':
    case 'session-lost':
    case 'leader-lost':
    case 'node-closed':
    case 'replaced':
      return raw;
    default:
      return 'replaced';
  }
}

/**
 * Parse one `org:` wire string (`org:<domain>:<kind>[:detail]`, the
 * `OrgSdkError::to_wire` vocabulary) or a `0x0009` admission refusal,
 * or `null` if the message is not one.
 *
 * The domains and kind tokens mirror `classifyOrgError` from the Node
 * binding where names coincide; the `rpc` domain reuses the frozen
 * nRPC kind vocabulary rather than minting second names for the same
 * conditions.
 */
export function parseOrgError(message: string): OrgStreamError | null {
  // The handler-side sink's typed closed refusal comes first: it
  // starts with the wire prefix but is prose, not `org:<domain>:<kind>`.
  if (message.startsWith('org: the response sink is closed:')) {
    return new OrgCancelledError(message);
  }
  if (message.startsWith(ORG_WIRE_PREFIX)) {
    const rest = message.slice(ORG_WIRE_PREFIX.length);
    const firstColon = rest.indexOf(':');
    if (firstColon <= 0) return null;
    const domain = rest.slice(0, firstColon);
    const afterDomain = rest.slice(firstColon + 1);
    const secondColon = afterDomain.indexOf(':');
    const token = secondColon === -1 ? afterDomain : afterDomain.slice(0, secondColon);
    if (token.length === 0) return null;
    if (domain === 'admission_denied') {
      return new OrgAdmissionDeniedError(coarseAdmissionReason(token), message);
    }
    if (domain !== 'rpc') return null;
    switch (token) {
      case 'revoked':
        return new OrgRevokedError(message);
      case 'timeout':
        return new OrgTimeoutError(message);
      case 'cancelled':
        return new OrgCancelledError(message);
      case 'leader_lost':
      case 'leader-lost':
        return new OrgLeaderLostError('', message);
      case 'session_lost':
      case 'session-lost':
        return new OrgSessionLostError(message);
      case 'indeterminate':
        return new OrgIndeterminateError(0, message);
      case 'refused':
        return new OrgRefusedError(0, message);
      case 'malformed':
        return new OrgMalformedError(message);
      case 'internal':
        return new OrgInternalError(message);
      default:
        return null;
    }
  }

  // The leaf's own Display for an admission refusal: `RpcStatus
  // 0x0009` carrying the single coarse reason byte as its body —
  // exactly the mapping `map_rpc_error` performs one layer up. The
  // seam renders that byte as the coarse token text verbatim
  // (`denied` | `not-supported` | `unavailable`); anything
  // undecodable is the least-informative bucket.
  const refused = /^rpc: refused \(9\): ([\s\S]*)$/.exec(message);
  if (refused) return new OrgAdmissionDeniedError(coarseAdmissionReason(refused[1]), message);
  return null;
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
  // The org vocabulary first: a `0x0009` refusal would otherwise
  // become a plain `rpc-refused` and lose that it was an admission
  // denial.
  const org = parseOrgError(message);
  if (org !== null) return org;

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

  // Re-typed, not flattened to `unknown`: a page that catches this
  // has a configuration to fix, and `kind` is what tells it apart
  // from a transient ICE failure it might retry.
  const iceConflict = after(message, 'ice configuration: ');
  if (iceConflict !== null) {
    const parsed = /^the iceServers entry (.+?) names this connection's peer RTC endpoint (.+?); a peer cannot be its own STUN server\./.exec(
      iceConflict,
    );
    if (parsed) return new IceServerConflictError(parsed[1] ?? '', parsed[2] ?? '');
  }

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
