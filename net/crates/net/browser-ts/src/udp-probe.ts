/**
 * The failure-typing correction, implemented.
 *
 * An ICE timeout is not evidence that UDP is blocked: an anchor that
 * is down, wrong or saturated produces exactly the same symptom. The
 * leaf therefore surfaces `rtc: ICE did not connect…` by default, and
 * only the two observations below may narrow it:
 *
 * 1. the HTTPS bootstrap to *that anchor* succeeded — it is up and
 *    addressable; and
 * 2. a STUN binding to the `rtc_addr` *that same anchor published*
 *    went unanswered.
 *
 * {@link classifyRtcFailure} is a pure function of those two
 * observations, so the rule is unit-testable without a browser, and
 * {@link probeStunBinding} / {@link probeBootstrapReachable} are the
 * two impure halves that produce them. Nothing else in this package
 * may construct a `udp-blocked` error.
 *
 * **What the STUN probe actually keys on** (measured in headless
 * Chromium, not assumed — see `docs/internal/spikes/S5_REPORT.md`):
 * host candidates are gathered whatever the network does, so their
 * presence or absence proves nothing and they are ignored. The signals
 * that mean something are
 *
 * - a **server-reflexive** candidate — the STUN binding was answered,
 *   so UDP to the anchor works and the ICE failure is something else;
 * - an `icecandidateerror` with a STUN **error code** below 700 — an
 *   error *response* came back, so packets reach the anchor;
 * - an `icecandidateerror` with code 701, or gathering completing with
 *   no reflexive candidate at all — nothing came back.
 */

import { RtcError, udpBlockedEvidence, type UdpBlockedEvidence } from './errors.js';

/** What a STUN binding attempt against the anchor's `rtc_addr` did. */
export type StunProbeOutcome =
  /** A reflexive candidate came back: the binding was answered. */
  | { readonly type: 'reflexive'; readonly address: string }
  /**
   * A STUN **error response** came back (code < 700). The anchor's ICE
   * agent refusing an unauthenticated binding still proves the packet
   * arrived and the reply got home, so this is *not* blocked UDP.
   */
  | { readonly type: 'stunError'; readonly code: number; readonly detail: string }
  /** Nothing came back inside the deadline. */
  | { readonly type: 'unanswered'; readonly detail: string }
  /** The probe was not run — no address to aim at, or it was disabled. */
  | { readonly type: 'notRun'; readonly reason: string }
  /** This engine could not run the probe at all. */
  | { readonly type: 'unsupported'; readonly detail: string };

/** Everything the classification is allowed to look at. */
export interface RtcFailureObservations {
  /** Did the HTTPS bootstrap to this anchor succeed? */
  readonly bootstrapOk: boolean;
  /** What the STUN probe saw. */
  readonly stunProbe: StunProbeOutcome;
  /** The address the probe was aimed at, so the claim names its subject. */
  readonly probed: string | null;
}

/** The only two classifications an ICE failure may end up with. */
export type IceFailureClassification =
  | { readonly type: 'iceTimeout' }
  | { readonly type: 'udpBlocked'; readonly evidence: UdpBlockedEvidence };

/**
 * The rule, pure.
 *
 * `udpBlocked` requires a named subject, a successful bootstrap and a
 * STUN probe that went unanswered. Every other combination — including
 * "we did not probe", "the probe is unsupported here" and "an error
 * response came back" — stays `iceTimeout`.
 */
export function classifyRtcFailure(observations: RtcFailureObservations): IceFailureClassification {
  const { bootstrapOk, stunProbe, probed } = observations;
  if (stunProbe.type !== 'unanswered') return { type: 'iceTimeout' };
  const evidence = udpBlockedEvidence(bootstrapOk, true, probed ?? '');
  return evidence === null ? { type: 'iceTimeout' } : { type: 'udpBlocked', evidence };
}

/** {@link classifyRtcFailure}, as the error a caller will be rejected with. */
export function classifyRtcError(observations: RtcFailureObservations): RtcError {
  return new RtcError(classifyRtcFailure(observations));
}

/** Injection seam for the probe, so tests never need a real engine. */
export interface StunProbeOptions {
  /** How long to wait for an answer. Default 2500 ms. */
  readonly timeoutMs?: number;
  /**
   * Builds the `RTCPeerConnection` used for the probe. Defaults to the
   * global constructor; tests pass a fake.
   */
  readonly peerConnectionFactory?: (config: RTCConfiguration) => RTCPeerConnection;
}

const DEFAULT_PROBE_TIMEOUT_MS = 2500;
/** Anything at or above this `errorCode` means "no response arrived". */
const STUN_NO_RESPONSE_CODE = 700;
const DEFAULT_STUN_PORT = 3478;

/**
 * Aim a STUN binding at `rtcAddr` and report what came back.
 *
 * Uses an `RTCPeerConnection` whose *only* ICE server is that address:
 * the browser then sends a STUN binding request to it as part of
 * gathering, and surfaces the outcome as either a reflexive candidate
 * or an `icecandidateerror`. The probe connection is never used for
 * media and is always closed.
 */
export async function probeStunBinding(
  rtcAddr: string,
  options: StunProbeOptions = {},
): Promise<StunProbeOutcome> {
  const url = diagnosticStunUrl(rtcAddr);
  if (url === null) return { type: 'notRun', reason: `not an address: ${rtcAddr}` };

  const factory = options.peerConnectionFactory ?? defaultPeerConnectionFactory();
  if (factory === null) {
    return { type: 'unsupported', detail: 'RTCPeerConnection is not available in this context' };
  }

  const timeoutMs = options.timeoutMs ?? DEFAULT_PROBE_TIMEOUT_MS;
  const { promise, resolve } = Promise.withResolvers<StunProbeOutcome>();
  let pc: RTCPeerConnection;
  try {
    pc = factory({ iceServers: [{ urls: url }], iceCandidatePoolSize: 0 });
  } catch (error) {
    return { type: 'unsupported', detail: String(error) };
  }

  let settled = false;
  const settle = (outcome: StunProbeOutcome): void => {
    if (settled) return;
    settled = true;
    resolve(outcome);
  };

  // A "no response" report from the engine, when it makes one. Chromium
  // does not always: against a black-holed address it emits no error and
  // never completes gathering, which is why the deadline below is
  // load-bearing rather than a safety net.
  let noResponse: string | null = null;
  const timer = setTimeout(
    () => settle({ type: 'unanswered', detail: noResponse ?? 'no answer inside the probe deadline' }),
    timeoutMs,
  );

  const gatheringDone = (): void => {
    settle({
      type: 'unanswered',
      detail: noResponse ?? 'gathering completed with no reflexive candidate',
    });
  };

  pc.onicecandidate = (event) => {
    const candidate = event.candidate;
    if (candidate === null) {
      gatheringDone();
      return;
    }
    // Host candidates are gathered regardless of what the network does
    // to UDP — Chromium even hides them behind an mDNS `.local` name —
    // so only a reflexive one is evidence.
    const reflexive = reflexiveAddress(candidate);
    if (reflexive !== null) settle({ type: 'reflexive', address: reflexive });
  };

  pc.onicecandidateerror = (event) => {
    const code = event.errorCode;
    const detail = `${code}: ${event.errorText} (${event.url})`;
    // A STUN *error response* is still a response: the packet reached
    // the anchor and the reply got home, so UDP is not blocked. (An
    // anchor's ICE agent refusing an unauthenticated binding lands
    // here; Chromium reports the STUN error number, e.g. `1` for a 401.)
    if (code < STUN_NO_RESPONSE_CODE) {
      settle({ type: 'stunError', code, detail });
      return;
    }
    noResponse = detail;
  };

  pc.onicegatheringstatechange = () => {
    if (pc.iceGatheringState === 'complete') gatheringDone();
  };

  try {
    // A data section is required or there is nothing to gather for.
    pc.createDataChannel('net-udp-probe');
    await pc.setLocalDescription(await pc.createOffer());
  } catch (error) {
    settle({ type: 'unsupported', detail: String(error) });
  }

  try {
    return await promise;
  } finally {
    clearTimeout(timer);
    pc.onicecandidate = null;
    pc.onicecandidateerror = null;
    pc.onicegatheringstatechange = null;
    pc.close();
  }
}

/** Injection seam for the bootstrap observation. */
export interface BootstrapProbeOptions {
  /** How long to wait for the anchor to answer. Default 3000 ms. */
  readonly timeoutMs?: number;
  /** Defaults to the global `fetch`; tests pass a fake. */
  readonly fetchImpl?: typeof fetch;
}

const DEFAULT_BOOTSTRAP_TIMEOUT_MS = 3000;

/**
 * Did the anchor answer over HTTPS?
 *
 * Any response counts, including 4xx and 5xx: the question is whether
 * the anchor is up and addressable, not whether it liked this request.
 * The request is `no-cors` on purpose — a cross-origin anchor without
 * CORS headers would otherwise reject a perfectly delivered request
 * and we would under-report reachability — and `no-store`, so a cached
 * response cannot stand in for a live one.
 */
export async function probeBootstrapReachable(
  bootstrapUrl: string,
  options: BootstrapProbeOptions = {},
): Promise<boolean> {
  const fetchImpl = options.fetchImpl ?? globalThis.fetch;
  if (typeof fetchImpl !== 'function') return false;
  try {
    await fetchImpl(bootstrapUrl, {
      method: 'GET',
      mode: 'no-cors',
      cache: 'no-store',
      redirect: 'follow',
      signal: AbortSignal.timeout(options.timeoutMs ?? DEFAULT_BOOTSTRAP_TIMEOUT_MS),
    });
    return true;
  } catch {
    return false;
  }
}

/**
 * `host:port` (or `[v6]:port`, or a bare host) to a `stun:` URL for
 * the **throwaway diagnostic probe**, or `null` when it is not an
 * address at all. A missing port means the IANA STUN port, which is
 * what an anchor publishing a bare host would mean.
 *
 * This is for {@link probeStunBinding} and nothing else. It must
 * **NOT** be used to build the anchor connection's `iceServers`:
 * `rtc_addr` is that connection's ICE peer, and a peer cannot be its
 * own STUN server — configuring it that way gathers no reflexive
 * candidate and buys an ICE deadline instead of a connection. The
 * leaf refuses it outright ({@link IceServerConflictError}).
 *
 * The endpoint an anchor connection should gather against is the one
 * the anchor announces separately, as `stun_addr` on
 * `GET /rtc/anchor`; `connect()` uses it by default, so a page needs
 * no URL-building at all.
 */
export function diagnosticStunUrl(rtcAddr: string): string | null {
  const trimmed = rtcAddr.trim();
  if (trimmed.length === 0) return null;
  if (trimmed.startsWith('stun:') || trimmed.startsWith('stuns:')) return trimmed;

  const bracketed = /^\[([0-9a-fA-F:.]+)\](?::(\d+))?$/.exec(trimmed);
  if (bracketed) {
    const host = bracketed[1] ?? '';
    return `stun:[${host}]:${bracketed[2] ?? DEFAULT_STUN_PORT}`;
  }
  // A bare IPv6 literal has more than one colon and no brackets.
  if ((trimmed.match(/:/g)?.length ?? 0) > 1) return `stun:[${trimmed}]:${DEFAULT_STUN_PORT}`;

  const hostPort = /^([A-Za-z0-9._-]+)(?::(\d+))?$/.exec(trimmed);
  if (hostPort === null) return null;
  const host = hostPort[1] ?? '';
  return `stun:${host}:${hostPort[2] ?? DEFAULT_STUN_PORT}`;
}

/**
 * The reflexive address of a candidate, or `null` if it is not a
 * reflexive one.
 *
 * `RTCIceCandidate.type`/`.address` are the modern surface; the SDP
 * line is parsed as well because the fields are still nullable in
 * engines that predate them, and a missed `srflx` would be read as
 * blocked UDP — the exact mis-typing this module exists to prevent.
 */
export function reflexiveAddress(candidate: RTCIceCandidate): string | null {
  if (candidate.type === 'srflx') return candidate.address ?? sdpAddress(candidate.candidate);
  if (candidate.type !== null) return null;
  return / typ srflx /.test(candidate.candidate) ? sdpAddress(candidate.candidate) : null;
}

function sdpAddress(line: string): string {
  // candidate:<foundation> <component> <transport> <priority> <ip> <port> typ …
  const fields = line.split(' ');
  const ip = fields[4] ?? '';
  const port = fields[5] ?? '';
  return port.length > 0 ? `${ip}:${port}` : ip;
}

function defaultPeerConnectionFactory(): ((config: RTCConfiguration) => RTCPeerConnection) | null {
  if (typeof RTCPeerConnection === 'undefined') return null;
  return (config) => new RTCPeerConnection(config);
}
