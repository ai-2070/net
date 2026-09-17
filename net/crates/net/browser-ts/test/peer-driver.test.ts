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
  driveAttempt,
  connectPeer,
  handshakePeer,
  type PeerPrimitives,
} from '../src/peer-driver.js';
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
