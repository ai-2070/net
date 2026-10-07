/**
 * The shared peer drive loop, and the ownership rule it enforces for
 * a proxied caller.
 *
 * `BrowserNode` and `MeshSession` run this same loop, so what is
 * asserted here holds for both. The cases that matter are the two
 * sources of supersession, because they are the ones a proxied caller
 * meets that a direct one does not:
 *
 * - the leaf refused the step, **before** servicing anything, because
 *   the dialog named is not the live one; and
 * - the reading came back naming a different dialog.
 *
 * Both are `superseded` to a caller, and the first can name the
 * replacement while the second reads it off the reading.
 */

import { describe, expect, it, vi } from 'vitest';

import {
  acceptPeer,
  answerWaitingOffer,
  driveAttempt,
  connectPeer,
  handshakePeer,
  noteOffer,
  PEER_OFFER_FRESH_MS,
  PeerAttempts,
  type PeerPrimitives,
} from '../src/peer-driver.js';
import type { PeerConnectOutcome } from '../src/node.js';
import { parseAttemptStatus } from '../src/node.js';

const PEER = 'a1b2c3d4e5f60718';
const D1 = '00000000000000d1';
const D2 = '00000000000000d2';

/** The leaf's `attempt_for` refusal, verbatim in shape. */
function replaced(live: string): Error {
  return new Error(
    `dialog ${D1} on 0xa1b2c3d4e5f60718 is not the live attempt (dialog ${live} is); ` +
      'the attempt this request was issued for has been replaced',
  );
}

function reading(fields: Partial<{ dialog: string; state: string; direct: boolean }>): string {
  return JSON.stringify({
    dialog: fields.dialog ?? D1,
    state: fields.state ?? 'open',
    sent: 0,
    applied: 0,
    answered: true,
    direct: fields.direct ?? false,
    remainingMs: '9000',
  });
}

function primitives(overrides: Partial<PeerPrimitives>): PeerPrimitives {
  return {
    offer: async () => D1,
    acceptOffer: async () => D1,
    candidate: async () => reading({ state: 'open' }),
    handshake: async (_peer, dialog) => dialog,
    ...overrides,
  };
}

describe('supersession, from either source', () => {
  it('classifies the leaf refusal as superseded and recovers the live dialog', async () => {
    const candidate = vi.fn(async () => {
      throw replaced(D2);
    });

    const outcome = await driveAttempt(
      PEER,
      D1,
      (status) => status.direct,
      primitives({ candidate }),
      parseAttemptStatus,
    );

    expect(outcome).toEqual({ type: 'superseded', peer: PEER, dialog: D1, liveDialog: D2 });
    // The point of the fence: exactly one step was attempted, and it
    // was refused rather than serviced.
    expect(candidate).toHaveBeenCalledTimes(1);
  });

  it('classifies an ended attempt as superseded with no successor', async () => {
    const outcome = await driveAttempt(
      PEER,
      D1,
      (status) => status.direct,
      primitives({
        candidate: async () => {
          throw new Error('no live attempt with 0xa1b2c3d4e5f60718');
        },
      }),
      parseAttemptStatus,
    );

    // An ended attempt cannot name a successor, and `null` says so
    // rather than inventing one.
    expect(outcome).toEqual({ type: 'superseded', peer: PEER, dialog: D1, liveDialog: null });
  });

  it('classifies a reading for another dialog as superseded', async () => {
    const outcome = await driveAttempt(
      PEER,
      D1,
      (status) => status.direct,
      primitives({ candidate: async () => reading({ dialog: D2, direct: true }) }),
      parseAttemptStatus,
    );

    expect(outcome).toEqual({ type: 'superseded', peer: PEER, dialog: D1, liveDialog: D2 });
  });

  it('does not report a stale caller as connected when the handshake ran for another dialog', async () => {
    const outcome = await handshakePeer(
      PEER,
      D1,
      primitives({ handshake: async () => D2 }),
    );

    // The regression this check exists for: returning `direct` with
    // the caller's own dialog on any success told a stale caller its
    // superseded attempt had connected.
    expect(outcome).toEqual({ type: 'superseded', peer: PEER, dialog: D1, liveDialog: D2 });
  });
});

describe('every step names the dialog it is driving', () => {
  it('passes the offered dialog to candidate and handshake', async () => {
    const candidate = vi.fn(async () => reading({ state: 'open' }));
    const handshake = vi.fn(async (_peer: string, dialog: string) => dialog);

    const outcome = await connectPeer(
      PEER,
      primitives({ offer: async () => D1, candidate, handshake }),
      parseAttemptStatus,
    );

    expect(outcome).toEqual({ type: 'direct', peer: PEER, dialog: D1 });
    // Not the peer alone. A peer-only step is resolved against
    // whichever attempt is live when the leader runs it, which for a
    // request that crossed a channel is how a superseded request
    // drives its own replacement.
    expect(candidate).toHaveBeenCalledWith(PEER, D1);
    expect(handshake).toHaveBeenCalledWith(PEER, D1);
  });
});

describe('terminal states end the loop', () => {
  it.each([
    ['iceTimeout', 'iceTimeout'],
    ['udpBlocked', 'udpBlocked'],
  ])('%s ends it with its own disposition', async (state, type) => {
    const outcome = await driveAttempt(
      PEER,
      D1,
      (status) => status.direct,
      primitives({ candidate: async () => reading({ state }) }),
      parseAttemptStatus,
    );
    expect(outcome).toEqual({ type, peer: PEER, dialog: D1 });
  });

  it('failed ends it as handshakeFailed, not as a timeout', async () => {
    const outcome = await driveAttempt(
      PEER,
      D1,
      (status) => status.direct,
      primitives({ candidate: async () => reading({ state: 'failed' }) }),
      parseAttemptStatus,
    );

    // A channel that opened with a session that never installed
    // satisfies neither `direct` nor `iceTimeout`; the leaf reports
    // `failed` and this is the loop's exit for it.
    expect(outcome.type).toBe('handshakeFailed');
  });
});

describe('proxy traffic per attempt, measured not promised', () => {
  /**
   * Count the primitive calls the loop makes. On a follower each one
   * is a request and a reply over the `BroadcastChannel`, so the
   * message count is twice this.
   */
  async function stepsToConnect(gatheringPolls: number): Promise<{
    offers: number;
    candidates: number;
    handshakes: number;
  }> {
    let polls = 0;
    const counts = { offers: 0, candidates: 0, handshakes: 0 };
    const outcome = await connectPeer(
      PEER,
      primitives({
        offer: async () => {
          counts.offers += 1;
          return D1;
        },
        candidate: async () => {
          counts.candidates += 1;
          polls += 1;
          // `gathering` until the channel opens: the leaf owns the
          // deadline, so the loop polls for as long as the attempt
          // takes.
          return reading({ state: polls > gatheringPolls ? 'open' : 'gathering' });
        },
        handshake: async (_peer, dialog) => {
          counts.handshakes += 1;
          return dialog;
        },
      }),
      parseAttemptStatus,
    );
    expect(outcome.type).toBe('direct');
    return counts;
  }

  it('costs one offer, one handshake, and one candidate step per poll', async () => {
    // There is no fixed attempt cost, and quoting one would be wrong:
    // `candidate` is POLLED, so the step count is a function of how
    // long ICE takes, which is the leaf's deadline and not this
    // loop's. What IS fixed is the shape.
    expect(await stepsToConnect(0)).toEqual({ offers: 1, candidates: 1, handshakes: 1 });
    expect(await stepsToConnect(3)).toEqual({ offers: 1, candidates: 4, handshakes: 1 });
    expect(await stepsToConnect(9)).toEqual({ offers: 1, candidates: 10, handshakes: 1 });
  });

  it('adds one accept step per retry while the offer is still in flight', async () => {
    let attempts = 0;
    const acceptOffer = vi.fn(async () => {
      attempts += 1;
      // The leaf's "no verified offer yet" is retried; it is a
      // message in flight, not a refusal.
      if (attempts < 3) throw new Error('no verified offer from 0xa1b2c3d4e5f60718');
      return D1;
    });

    const outcome = await acceptPeer(
      PEER,
      primitives({ acceptOffer, candidate: async () => reading({ direct: true }) }),
      parseAttemptStatus,
    );

    expect(outcome.type).toBe('direct');
    // So an accept costs 1..N accept steps plus the poll steps, and N
    // is bounded by PEER_OFFER_WAIT_MS rather than by a count.
    expect(acceptOffer).toHaveBeenCalledTimes(3);
  });
});

describe('one attempt per peer at a time (PeerAttempts)', () => {
  const direct: PeerConnectOutcome = { type: 'direct', peer: PEER, dialog: D1 };
  /** No healthy direct pair: the reading every case below starts from. */
  const notDirect = () => undefined;

  /** A drive that settles when told to. */
  function parked(outcome: PeerConnectOutcome) {
    const settle = Promise.withResolvers<PeerConnectOutcome>();
    const drive = vi.fn(() => settle.promise);
    return { drive, settle: () => settle.resolve(outcome) };
  }

  it('answers a connect in flight with its own outcome, never a second drive', async () => {
    const attempts = new PeerAttempts();
    const first = parked(direct);
    const second = vi.fn(async () => direct);
    const a = attempts.connect(PEER, notDirect, first.drive);
    const b = attempts.connect(PEER.toUpperCase(), notDirect, second);
    first.settle();
    expect(await a).toBe(direct);
    expect(await b).toBe(direct);
    expect(first.drive).toHaveBeenCalledTimes(1);
    expect(second).not.toHaveBeenCalled();
  });

  // An answer that settles the pair is the connect's answer too: offering
  // after `direct` would replace the link, after an ICE verdict would only
  // repeat it a deadline later, and after a supersession naming its
  // successor would cancel that successor.
  it.each<[string, PeerConnectOutcome]>([
    ['direct', direct],
    ['iceTimeout', { type: 'iceTimeout', peer: PEER, dialog: D1 }],
    ['udpBlocked', { type: 'udpBlocked', peer: PEER, dialog: D1 }],
    ['superseded by a named successor', { type: 'superseded', peer: PEER, dialog: D1, liveDialog: D2 }],
  ])('takes an answer that ended %s without offering', async (_name, outcome) => {
    const attempts = new PeerAttempts();
    const answer = parked(outcome);
    void attempts.accept(PEER, answer.drive);
    const offer = vi.fn(async () => direct);
    const connecting = attempts.connect(PEER, notDirect, offer);
    answer.settle();
    expect(await connecting).toBe(outcome);
    expect(offer).not.toHaveBeenCalled();
  });

  it.each<[string, PeerConnectOutcome]>([
    ['noAnnouncement', { type: 'noAnnouncement', peer: PEER, detail: 'x' }],
    ['handshakeFailed', { type: 'handshakeFailed', peer: PEER, dialog: D1, detail: 'no session' }],
    ['superseded with no successor', { type: 'superseded', peer: PEER, dialog: D1, liveDialog: null }],
  ])('offers after an answer that ended %s', async (_name, outcome) => {
    const attempts = new PeerAttempts();
    const answer = parked(outcome);
    void attempts.accept(PEER, answer.drive);
    const offer = vi.fn(async () => direct);
    const connecting = attempts.connect(PEER, notDirect, offer);
    answer.settle();
    expect(await connecting).toBe(direct);
    expect(offer).toHaveBeenCalledTimes(1);
  });

  // Read inside the gate, after any answer it waited on: a reading taken
  // ahead of it could be stale by the time the offer went out.
  it('reads the healthy pair inside the gate, after the answer it waited on', async () => {
    const attempts = new PeerAttempts();
    const answer = parked({ type: 'handshakeFailed', peer: PEER, dialog: D1, detail: 'x' });
    void attempts.accept(PEER, answer.drive);
    let readAt = 0;
    let settledAt = 0;
    let clock = 0;
    const healthy = vi.fn(() => {
      readAt = ++clock;
      return D2;
    });
    const offer = vi.fn(async () => direct);
    const connecting = attempts.connect(PEER, healthy, offer);
    settledAt = ++clock;
    answer.settle();
    expect(await connecting).toEqual({ type: 'direct', peer: PEER, dialog: D2 });
    expect(readAt).toBeGreaterThan(settledAt);
    expect(offer).not.toHaveBeenCalled();
  });

  it('shares one answer between concurrent accepts', async () => {
    const attempts = new PeerAttempts();
    const first = parked(direct);
    const second = vi.fn(async () => direct);
    const a = attempts.accept(PEER, first.drive);
    const b = attempts.accept(PEER, second);
    first.settle();
    expect(await b).toBe(await a);
    expect(second).not.toHaveBeenCalled();
  });

  it('forgets a settled attempt, including one that rejected', async () => {
    const attempts = new PeerAttempts();
    await expect(
      attempts.connect(PEER, notDirect, async () => Promise.reject(new Error('closed'))),
    ).rejects.toThrow('closed');
    const fresh = vi.fn(async () => direct);
    await expect(attempts.connect(PEER, notDirect, fresh)).resolves.toBe(direct);
    expect(fresh).toHaveBeenCalledTimes(1);
  });

  it('keeps peers apart', async () => {
    const attempts = new PeerAttempts();
    const other = 'ffffffffffffffff';
    const first = parked(direct);
    const second = vi.fn(async (): Promise<PeerConnectOutcome> => ({ type: 'direct', peer: other, dialog: D2 }));
    const a = attempts.connect(PEER, notDirect, first.drive);
    await attempts.connect(other, notDirect, second);
    first.settle();
    await a;
    expect(second).toHaveBeenCalledTimes(1);
  });
});

// Two nodes offering to each other at once cancel each other: each
// answer retires the answerer's own offer, both run out their ICE
// deadline, and the pair stays relayed. The lower id offers; the higher
// id answers.
describe('one offerer per pair: the lower id (PeerAttempts glare)', () => {
  /** PEER's id is a1b2…: these sit either side of it. */
  const HIGHER = 'ff00000000000000';
  const LOWER = '0000000000000001';
  const offered: PeerConnectOutcome = { type: 'direct', peer: PEER, dialog: D1 };
  const answered: PeerConnectOutcome = { type: 'direct', peer: PEER, dialog: D2 };
  const notDirect = () => undefined;

  function parked(outcome: PeerConnectOutcome) {
    const settle = Promise.withResolvers<PeerConnectOutcome>();
    const drive = vi.fn(() => settle.promise);
    return { drive, settle: () => settle.resolve(outcome) };
  }

  it('the higher id answers a fresh offer from the peer instead of offering back', async () => {
    const attempts = new PeerAttempts(() => HIGHER);
    attempts.noteOffer(PEER);
    const offer = vi.fn(async () => offered);
    const answer = vi.fn(async () => answered);
    expect(await attempts.connect(PEER, notDirect, offer, answer)).toBe(answered);
    expect(offer).not.toHaveBeenCalled();
  });

  // The leaf holds an unanswered offer until something takes it, so an old
  // one would otherwise hijack every later connectPeer.
  it('the higher id offers over a stale offer', async () => {
    let clock = 0;
    const attempts = new PeerAttempts(() => HIGHER, () => clock);
    attempts.noteOffer(PEER);
    clock = PEER_OFFER_FRESH_MS + 1;
    const offer = vi.fn(async () => offered);
    const answer = vi.fn(async () => answered);
    expect(await attempts.connect(PEER, notDirect, offer, answer)).toBe(offered);
    expect(answer).not.toHaveBeenCalled();
  });

  it('the higher id offers when no offer is in fact waiting', async () => {
    const attempts = new PeerAttempts(() => HIGHER);
    attempts.noteOffer(PEER);
    const offer = vi.fn(async () => offered);
    const answer = vi.fn(async () => null);
    expect(await attempts.connect(PEER, notDirect, offer, answer)).toBe(offered);
    expect(answer).toHaveBeenCalledTimes(1);
    expect(offer).toHaveBeenCalledTimes(1);
  });

  it('the higher id answers an offer that crosses its own, and takes its outcome', async () => {
    const attempts = new PeerAttempts(() => HIGHER);
    const ownOffer = parked({ type: 'superseded', peer: PEER, dialog: D1, liveDialog: D2 });
    const answer = vi.fn(async () => answered);
    const connecting = attempts.connect(PEER, notDirect, ownOffer.drive, answer);
    await vi.waitFor(() => expect(ownOffer.drive).toHaveBeenCalled());
    attempts.noteOffer(PEER);
    expect(await connecting).toBe(answered);
    ownOffer.settle();
    expect(answer).toHaveBeenCalledTimes(1);
  });

  // Answering retires this node's own offer, so the offer's drive can settle
  // `superseded` while the answer still runs: that is not the call's outcome.
  it("takes the crossing answer's outcome even when its own offer settles first", async () => {
    const attempts = new PeerAttempts(() => HIGHER);
    const ownOffer = parked({ type: 'superseded', peer: PEER, dialog: D1, liveDialog: D2 });
    const answering = Promise.withResolvers<PeerConnectOutcome>();
    const answer = vi.fn(() => answering.promise);
    const connecting = attempts.connect(PEER, notDirect, ownOffer.drive, answer);
    await vi.waitFor(() => expect(ownOffer.drive).toHaveBeenCalled());
    attempts.noteOffer(PEER);
    await vi.waitFor(() => expect(answer).toHaveBeenCalled());
    ownOffer.settle();
    await new Promise((resolve) => setTimeout(resolve, 120));
    answering.resolve(answered);
    expect(await connecting).toBe(answered);
  });

  it('offers after a fresh answer that did not settle the pair', async () => {
    const attempts = new PeerAttempts(() => HIGHER);
    attempts.noteOffer(PEER);
    const offer = vi.fn(async () => offered);
    const answer = vi.fn(async (): Promise<PeerConnectOutcome> => (
      { type: 'handshakeFailed', peer: PEER, dialog: D2, detail: 'no session' }
    ));
    expect(await attempts.connect(PEER, notDirect, offer, answer)).toBe(offered);
    expect(offer).toHaveBeenCalledTimes(1);
  });

  it('the lower id offers whatever the peer sent', async () => {
    const attempts = new PeerAttempts(() => LOWER);
    attempts.noteOffer(PEER);
    const offer = vi.fn(async () => offered);
    const answer = vi.fn(async () => answered);
    expect(await attempts.connect(PEER, notDirect, offer, answer)).toBe(offered);
    expect(answer).not.toHaveBeenCalled();
  });

  // On the lower id the crossing offer is the one to ignore: answering it
  // would retire this node's own offer, the one the pair keeps.
  it("an accept during the node's own connect takes the connect's outcome", async () => {
    const attempts = new PeerAttempts(() => LOWER);
    const ownOffer = parked(offered);
    const connecting = attempts.connect(PEER, notDirect, ownOffer.drive, async () => answered);
    const answerDrive = vi.fn(async () => answered);
    const accepting = attempts.accept(PEER, answerDrive);
    ownOffer.settle();
    expect(await accepting).toBe(await connecting);
    expect(answerDrive).not.toHaveBeenCalled();
  });

  it('with its own id unknown, the node offers and answers as before', async () => {
    const attempts = new PeerAttempts();
    attempts.noteOffer(PEER);
    const answer = vi.fn(async () => answered);
    expect(await attempts.connect(PEER, notDirect, async () => offered, answer)).toBe(offered);
    expect(answer).not.toHaveBeenCalled();
    const ownOffer = parked(offered);
    void attempts.connect(PEER, notDirect, ownOffer.drive);
    const answerDrive = vi.fn(async () => answered);
    expect(await attempts.accept(PEER, answerDrive)).toBe(answered);
    ownOffer.settle();
  });

  it('notes only verified offers, from an exact decimal id', () => {
    const attempts = new PeerAttempts(() => HIGHER);
    const note = vi.spyOn(attempts, 'noteOffer');
    noteOffer(attempts, { type: 'signal', kind: '3', from: '1' });
    noteOffer(attempts, { type: 'channel_message' });
    noteOffer(attempts, { type: 'signal', kind: '1', from: '0x1' });
    noteOffer(attempts, { type: 'signal', kind: '1', from: '18446744073709551616' });
    expect(note).not.toHaveBeenCalled();
    noteOffer(attempts, { type: 'signal', kind: '1', from: '11651590505119483672' });
    expect(note).toHaveBeenCalledWith(PEER);
  });

  it('answerWaitingOffer is null when no offer is waiting, and drives one that is', async () => {
    const none = primitives({
      acceptOffer: async () => {
        throw new Error('no verified offer from 0xa1b2c3d4e5f60718 is waiting');
      },
    });
    expect(await answerWaitingOffer(PEER, none, parseAttemptStatus)).toBeNull();
    const waiting = primitives({ candidate: async () => reading({ direct: true }) });
    expect(await answerWaitingOffer(PEER, waiting, parseAttemptStatus)).toEqual({
      type: 'direct',
      peer: PEER,
      dialog: D1,
    });
  });
});
