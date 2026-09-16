/**
 * The failure-typing correction.
 *
 * The rule under test: `udp-blocked` is reachable only from the two
 * observations that distinguish it, and every other combination of
 * observations stays `ice-timeout`. These are the assertions that stop
 * a future refactor from quietly widening the narrow type.
 */

import { describe, expect, it } from 'vitest';

import {
  IceServerConflictError,
  isUdpBlocked,
  parseLeafError,
  RtcError,
  udpBlockedEvidence,
} from '../src/errors.js';
import { refineIceFailure } from '../src/node.js';
import {
  classifyRtcFailure,
  diagnosticStunUrl,
  probeBootstrapReachable,
  probeStunBinding,
  reflexiveAddress,
  type StunProbeOutcome,
} from '../src/udp-probe.js';

const PROBED = '203.0.113.9:4433';

describe('classifyRtcFailure', () => {
  it('promotes to udpBlocked only with a successful bootstrap, an unanswered probe and a named subject', () => {
    const classification = classifyRtcFailure({
      bootstrapOk: true,
      stunProbe: { type: 'unanswered', detail: '701: timed out' },
      probed: PROBED,
    });
    expect(classification).toEqual({
      type: 'udpBlocked',
      evidence: { bootstrapOk: true, stunProbeFailed: true, probed: PROBED },
    });
  });

  it('stays iceTimeout when the bootstrap also failed — an anchor that is simply down', () => {
    expect(
      classifyRtcFailure({
        bootstrapOk: false,
        stunProbe: { type: 'unanswered', detail: 'nothing came back' },
        probed: PROBED,
      }),
    ).toEqual({ type: 'iceTimeout' });
  });

  it('stays iceTimeout when there is no subject to name', () => {
    expect(
      classifyRtcFailure({
        bootstrapOk: true,
        stunProbe: { type: 'unanswered', detail: 'nothing came back' },
        probed: null,
      }),
    ).toEqual({ type: 'iceTimeout' });
  });

  it.each<StunProbeOutcome>([
    { type: 'reflexive', address: '198.51.100.7:54321' },
    { type: 'stunError', code: 401, detail: '401: Unauthorized' },
    { type: 'notRun', reason: 'no address' },
    { type: 'unsupported', detail: 'no RTCPeerConnection' },
  ])('stays iceTimeout when the probe outcome is $type, even with a good bootstrap', (stunProbe) => {
    expect(classifyRtcFailure({ bootstrapOk: true, stunProbe, probed: PROBED })).toEqual({
      type: 'iceTimeout',
    });
  });
});

describe('udpBlockedEvidence', () => {
  it('refuses to exist without both observations and a subject', () => {
    expect(udpBlockedEvidence(false, true, PROBED)).toBeNull();
    expect(udpBlockedEvidence(true, false, PROBED)).toBeNull();
    expect(udpBlockedEvidence(true, true, '')).toBeNull();
    expect(udpBlockedEvidence(true, true, PROBED)).toEqual({
      bootstrapOk: true,
      stunProbeFailed: true,
      probed: PROBED,
    });
  });
});

describe('diagnosticStunUrl', () => {
  it.each([
    ['203.0.113.9:4433', 'stun:203.0.113.9:4433'],
    ['anchor.example', 'stun:anchor.example:3478'],
    ['anchor.example:19302', 'stun:anchor.example:19302'],
    ['[2001:db8::1]:4433', 'stun:[2001:db8::1]:4433'],
    ['2001:db8::1', 'stun:[2001:db8::1]:3478'],
    ['stun:already.example:3478', 'stun:already.example:3478'],
  ])('maps %s to %s', (addr, expected) => {
    expect(diagnosticStunUrl(addr)).toBe(expected);
  });

  it('rejects what is not an address, so the probe is never aimed at nothing', () => {
    expect(diagnosticStunUrl('')).toBeNull();
    expect(diagnosticStunUrl('   ')).toBeNull();
    expect(diagnosticStunUrl('http://anchor.example/rtc')).toBeNull();
  });

  // The rename is the contract: what this helper produces is the
  // diagnostic probe's target, which is the anchor's ICE peer — so
  // handing it to the leaf as `iceServers` is exactly the
  // configuration the leaf now refuses. The old name implied the two
  // uses were interchangeable, and both of this repo's harnesses
  // took it up on that.
  it('produces the peer endpoint the leaf refuses as an iceServers entry', () => {
    const anchorRtcAddr = '203.0.113.9:4433';
    const url = diagnosticStunUrl(anchorRtcAddr);
    expect(url).toBe(`stun:${anchorRtcAddr}`);
    const refused = parseLeafError(new IceServerConflictError(url ?? '', anchorRtcAddr).message);
    expect(refused).toBeInstanceOf(IceServerConflictError);
    expect((refused as IceServerConflictError).entry).toBe(url);
  });
});

describe('reflexiveAddress', () => {
  it('reads the modern fields when the engine sets them', () => {
    expect(reflexiveAddress(candidate({ type: 'srflx', address: '198.51.100.7', line: srflxLine() }))).toBe(
      '198.51.100.7',
    );
  });

  it('falls back to the SDP line when type is null, so a srflx is never missed', () => {
    expect(reflexiveAddress(candidate({ type: null, address: null, line: srflxLine() }))).toBe(
      '198.51.100.7:54321',
    );
  });

  it('ignores host candidates, which say nothing about UDP to the anchor', () => {
    const host = 'candidate:1 1 udp 2130706431 192.168.1.5 50001 typ host';
    expect(reflexiveAddress(candidate({ type: 'host', address: '192.168.1.5', line: host }))).toBeNull();
    expect(reflexiveAddress(candidate({ type: null, address: null, line: host }))).toBeNull();
  });
});

describe('probeStunBinding', () => {
  it('reports reflexive when a server-reflexive candidate arrives', async () => {
    const outcome = await probeStunBinding(PROBED, {
      peerConnectionFactory: fakePeerConnection((pc) => {
        pc.emitCandidate(candidate({ type: 'host', address: '192.168.1.5', line: hostLine() }));
        pc.emitCandidate(candidate({ type: 'srflx', address: '198.51.100.7', line: srflxLine() }));
      }),
    });
    expect(outcome).toEqual({ type: 'reflexive', address: '198.51.100.7' });
  });

  it('reports stunError when an error response comes back — packets reach the anchor', async () => {
    const outcome = await probeStunBinding(PROBED, {
      peerConnectionFactory: fakePeerConnection((pc) => {
        pc.emitError(401, 'Unauthorized');
      }),
    });
    expect(outcome).toEqual({
      type: 'stunError',
      code: 401,
      detail: '401: Unauthorized (stun:203.0.113.9:4433)',
    });
  });

  it('reports unanswered on a 701 followed by gathering completing with no reflexive candidate', async () => {
    const outcome = await probeStunBinding(PROBED, {
      peerConnectionFactory: fakePeerConnection((pc) => {
        pc.emitCandidate(candidate({ type: 'host', address: '192.168.1.5', line: hostLine() }));
        pc.emitError(701, 'STUN binding request timed out');
        pc.completeGathering();
      }),
    });
    expect(outcome).toEqual({
      type: 'unanswered',
      detail: '701: STUN binding request timed out (stun:203.0.113.9:4433)',
    });
  });

  it('reports unanswered when nothing at all happens before the deadline', async () => {
    // Chromium against a black-holed address emits no error event and
    // never completes gathering, so this is the path that decides — and
    // the evidence it records must name the deadline, not claim a
    // gathering completion that never happened.
    const outcome = await probeStunBinding(PROBED, {
      timeoutMs: 5,
      peerConnectionFactory: fakePeerConnection(() => {}),
    });
    expect(outcome).toEqual({ type: 'unanswered', detail: 'no answer inside the probe deadline' });
  });

  // Stage 6 added a SECOND announced STUN endpoint, and the
  // classification contract is that this probe did not move: the
  // `udp-blocked` claim is about `rtc_addr`, so the probe must be
  // aimed at `rtc_addr` and at nothing else. Asserted on the
  // configuration the factory is handed, because an aim is not
  // observable from the outcome.
  it('aims exactly one ICE server at rtc_addr, which is what udp-blocked is a claim about', async () => {
    const connections: FakePeerConnection[] = [];
    await probeStunBinding(PROBED, {
      timeoutMs: 5,
      peerConnectionFactory: fakePeerConnection(() => {}, connections),
    });
    expect(connections).toHaveLength(1);
    expect(connections[0]?.config.iceServers).toEqual([{ urls: `stun:${PROBED}` }]);
    expect(connections[0]?.config.iceCandidatePoolSize).toBe(0);
  });

  it('closes the probe connection whatever the outcome', async () => {
    const connections: FakePeerConnection[] = [];
    await probeStunBinding(PROBED, {
      timeoutMs: 5,
      peerConnectionFactory: fakePeerConnection(() => {}, connections),
    });
    expect(connections).toHaveLength(1);
    expect(connections[0]?.closed).toBe(true);
  });

  it('does not run at all against something that is not an address', async () => {
    const outcome = await probeStunBinding('not-an-address!', {
      peerConnectionFactory: fakePeerConnection(() => {
        throw new Error('the probe must not be attempted');
      }),
    });
    expect(outcome.type).toBe('notRun');
  });
});

describe('probeBootstrapReachable', () => {
  it('counts any response, including a refusal — the question is reachability', async () => {
    const fetchImpl = (async () => new Response('no', { status: 405 })) as typeof fetch;
    await expect(probeBootstrapReachable('https://anchor.example/rtc', { fetchImpl })).resolves.toBe(true);
  });

  it('counts a network failure as unreachable', async () => {
    const fetchImpl = (async () => {
      throw new TypeError('Failed to fetch');
    }) as typeof fetch;
    await expect(probeBootstrapReachable('https://anchor.example/rtc', { fetchImpl })).resolves.toBe(false);
  });

  it('asks for an uncached, CORS-independent read', async () => {
    let seen: RequestInit | undefined;
    const fetchImpl = (async (_url: string, init?: RequestInit) => {
      seen = init;
      return new Response('');
    }) as unknown as typeof fetch;
    await probeBootstrapReachable('https://anchor.example/rtc', { fetchImpl });
    expect(seen?.mode).toBe('no-cors');
    expect(seen?.cache).toBe('no-store');
  });
});

describe('refineIceFailure', () => {
  const context = {
    bootstrapUrl: 'https://anchor.example/rtc/bootstrap',
    bootstrapObserved: true,
    anchorRtcAddr: PROBED,
    failureTyping: {},
  };

  it('leaves anything that is not an ice timeout alone', async () => {
    const closed = new RtcError({ type: 'channelClosed', detail: 'remote reset' });
    await expect(refineIceFailure(closed, context)).resolves.toBe(closed);
  });

  it('leaves an ice timeout alone when there is no address to probe', async () => {
    const timeout = new RtcError({ type: 'iceTimeout' });
    await expect(refineIceFailure(timeout, { ...context, anchorRtcAddr: null })).resolves.toBe(timeout);
  });

  it('leaves an ice timeout alone when probing is switched off', async () => {
    const timeout = new RtcError({ type: 'iceTimeout' });
    const refined = await refineIceFailure(timeout, {
      ...context,
      failureTyping: { probeOnIceTimeout: false },
    });
    expect(refined).toBe(timeout);
  });

  it('promotes to udp-blocked when the probe goes unanswered and the anchor answered HTTPS', async () => {
    const refined = await refineIceFailure(new RtcError({ type: 'iceTimeout' }), {
      ...context,
      failureTyping: {
        stun: { timeoutMs: 5, peerConnectionFactory: fakePeerConnection(() => {}) },
      },
    });
    expect(isUdpBlocked(refined)).toBe(true);
    expect(refined.kind).toBe('udp-blocked');
    expect(refined.message).toContain(PROBED);
  });

  it('stays ice-timeout when the probe is answered, however the ICE attempt died', async () => {
    const refined = await refineIceFailure(new RtcError({ type: 'iceTimeout' }), {
      ...context,
      failureTyping: {
        stun: {
          peerConnectionFactory: fakePeerConnection((pc) => {
            pc.emitCandidate(candidate({ type: 'srflx', address: '198.51.100.7', line: srflxLine() }));
          }),
        },
      },
    });
    expect(refined.kind).toBe('ice-timeout');
  });

  it('falls back to an HTTPS reachability probe when no session was ever observed', async () => {
    const refined = await refineIceFailure(new RtcError({ type: 'iceTimeout' }), {
      ...context,
      bootstrapObserved: false,
      failureTyping: {
        stun: { timeoutMs: 5, peerConnectionFactory: fakePeerConnection(() => {}) },
        bootstrap: {
          fetchImpl: (async () => {
            throw new TypeError('Failed to fetch');
          }) as typeof fetch,
        },
      },
    });
    // The anchor never answered HTTPS either, so "UDP is blocked" is
    // exactly the claim the evidence does not support.
    expect(refined.kind).toBe('ice-timeout');
  });
});

function srflxLine(): string {
  return 'candidate:2 1 udp 1694498815 198.51.100.7 54321 typ srflx raddr 0.0.0.0 rport 0';
}

function hostLine(): string {
  return 'candidate:1 1 udp 2130706431 192.168.1.5 50001 typ host';
}

function candidate(spec: { type: string | null; address: string | null; line: string }): RTCIceCandidate {
  // Only the three fields `reflexiveAddress` reads exist on the real
  // object's relevant surface; the rest of `RTCIceCandidate` is inert
  // here and cannot be constructed without an engine.
  return { type: spec.type, address: spec.address, candidate: spec.line } as unknown as RTCIceCandidate;
}

class FakePeerConnection {
  onicecandidate: ((event: { candidate: RTCIceCandidate | null }) => void) | null = null;
  onicecandidateerror: ((event: { errorCode: number; errorText: string; url: string }) => void) | null = null;
  onicegatheringstatechange: (() => void) | null = null;
  iceGatheringState = 'new';
  closed = false;
  channels: string[] = [];

  constructor(
    readonly config: RTCConfiguration,
    private readonly script: (pc: FakePeerConnection) => void,
  ) {}

  createDataChannel(label: string): unknown {
    this.channels.push(label);
    return {};
  }

  async createOffer(): Promise<{ type: string; sdp: string }> {
    return { type: 'offer', sdp: '' };
  }

  async setLocalDescription(): Promise<void> {
    // Gathering starts here on a real engine, so this is where the
    // scripted outcome plays out.
    this.script(this);
  }

  close(): void {
    this.closed = true;
  }

  emitCandidate(value: RTCIceCandidate | null): void {
    this.onicecandidate?.({ candidate: value });
  }

  emitError(errorCode: number, errorText: string): void {
    const url = firstIceUrl(this.config);
    this.onicecandidateerror?.({ errorCode, errorText, url });
  }

  completeGathering(): void {
    this.iceGatheringState = 'complete';
    this.onicegatheringstatechange?.();
  }
}

function firstIceUrl(config: RTCConfiguration): string {
  const urls = config.iceServers?.[0]?.urls;
  if (typeof urls === 'string') return urls;
  return urls?.[0] ?? '';
}

function fakePeerConnection(
  script: (pc: FakePeerConnection) => void,
  collect: FakePeerConnection[] = [],
): (config: RTCConfiguration) => RTCPeerConnection {
  return (config) => {
    const pc = new FakePeerConnection(config, script);
    collect.push(pc);
    // The probe uses four members of `RTCPeerConnection`; a real one
    // cannot be constructed under Node, which is the point of the seam.
    return pc as unknown as RTCPeerConnection;
  };
}
