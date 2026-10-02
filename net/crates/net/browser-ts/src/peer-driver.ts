/**
 * The peer-attempt drive loop, once, for both surfaces.
 *
 * `BrowserNode` drives an attempt on this tab's own node;
 * `MeshSession` drives one on whichever tab holds the lock. The steps
 * and the classification are identical, and they must stay identical:
 * the offerer and the answerer disagreeing about what supersession or
 * a terminal reading looks like is a defect neither side can detect.
 * So the loop lives here and each surface supplies only the four
 * primitives ({@link PeerPrimitives}).
 *
 * **Supersession now has two sources, and that is the reason this
 * file exists rather than a second copy of the loop.**
 *
 * 1. *Client-side*, as before: the reading names the dialog that is
 *    live, so a reading for a different dialog means this attempt was
 *    replaced.
 * 2. *Server-side*, new with the proxy: a request that named a dialog
 *    is refused by the leaf **before** it services anything, because
 *    the attempt it was issued for is no longer the live one. A
 *    proxied request crosses a channel and is served later, so this is
 *    the common case for a follower rather than an edge.
 *
 * Both are the same disposition to a caller, and both are classified
 * here. A direct caller sees (1) almost always and (2) never; a
 * proxied caller sees either.
 */

import { fromWasmError } from './errors.js';
import type { PeerAttemptStatus, PeerConnectOutcome } from './node.js';

/**
 * The four steps an attempt needs, as whichever surface provides
 * them.
 *
 * `candidate` and `handshake` take the dialog because the leaf
 * requires it: a peer-only poll is resolved against whatever attempt
 * is live when it runs, which for a request that crossed a channel is
 * how a superseded request drives its own replacement.
 */
export interface PeerPrimitives {
  /** Mint an offer and start an attempt; resolves to its dialog. */
  offer(peer: string): Promise<string>;
  /** Answer the offer already filed; resolves to its dialog. */
  acceptOffer(peer: string): Promise<string>;
  /** Service one polling step of `dialog`; resolves to the reading JSON. */
  candidate(peer: string, dialog: string): Promise<string>;
  /** Run the Noise handshake for `dialog`; resolves to the dialog it ran for. */
  handshake(peer: string, dialog: string): Promise<string>;
}

/**
 * The exact prefix the leaf's "peer not discovered" refusal carries
 * (`wasm.rs`'s `NO_ANNOUNCEMENT_PREFIX`).
 *
 * Pinned because the wasm boundary carries messages, so a message is
 * a contract: a reworded refusal demotes a typed disposition to a
 * thrown error with nothing red.
 */
export const NO_ANNOUNCEMENT_PREFIX = 'no verified announcement for';

/** `wasm.rs`'s `NO_LIVE_ATTEMPT_PREFIX`. */
export const NO_LIVE_ATTEMPT_PREFIX = 'no live attempt with';

/** `wasm.rs`'s refusal when no verified offer is waiting yet. */
export const NO_OFFER_PREFIX = 'no verified offer from';

/**
 * `wasm.rs`'s `attempt_for` refusal: the named dialog is not the one
 * now live.
 *
 * Distinct from {@link NO_LIVE_ATTEMPT_PREFIX} on purpose — "your
 * attempt was replaced" and "there is no attempt" are different
 * facts, and the leaf says which. Both classify as `superseded`, but
 * only this one can name the replacement.
 */
export const REPLACED_ATTEMPT_MARKER = 'is not the live attempt';

/** How long the drive loop waits between candidate steps. */
export const PEER_TICK_MS = 50;

/**
 * How long {@link acceptPeer} waits for the offer envelope to arrive
 * before it treats "no offer" as the answer.
 *
 * Two anchor hops and one pump, so it is generous rather than tight.
 */
export const PEER_OFFER_WAIT_MS = 5_000;

/**
 * The pair's live dialog when it is already direct and open, `undefined`
 * (or `null`) otherwise: the healthy-pair reading a surface's
 * `connectPeer` must not offer over.
 */
export type HealthyDialog = () => Promise<string | null | undefined> | string | null | undefined;

/**
 * Whether an answer's outcome settles the pair for a `connectPeer` that
 * waited on it, rather than calling for an offer of its own.
 *
 * - `direct`: the pair has its link.
 * - `iceTimeout`, `udpBlocked`: this network could not go direct just
 *   now, and the routed session the answer ran over is still in place.
 *   An offer straight after would almost surely end the same way, a whole
 *   ICE deadline later, so the caller gets the answer's verdict instead.
 * - `superseded` naming a successor: a newer attempt is live, and an
 *   offer would cancel it.
 *
 * Anything else (`handshakeFailed`, `noAnnouncement`, a `superseded`
 * with no successor) says nothing about the pair, so the waiter offers.
 */
function answerSettles(outcome: PeerConnectOutcome): boolean {
  switch (outcome.type) {
    case 'direct':
    case 'iceTimeout':
    case 'udpBlocked':
      return true;
    case 'superseded':
      return outcome.liveDialog !== null;
    default:
      return false;
  }
}

/**
 * One attempt per peer at a time, per surface.
 *
 * **Every offer replaces the pair's link.** The leaf retires whatever
 * attempt is live for the peer and replaces its DataChannel, so a
 * second `connectPeer` while the first is still under way cancels it,
 * and both callers end on the relay or in `iceTimeout`. Nothing about
 * that is exotic: `joinLobby` reaches the host and then the store it
 * joins reaches it again before its first stream, and netcode beside a
 * store does the same. The healthy-direct check in
 * `BrowserNode.connectPeer` covers a pair that has already arrived; this
 * covers one that is still arriving.
 *
 * So, on one surface:
 *
 * - a `connectPeer` while another for the same peer is in flight is
 *   answered by that one's outcome, not by a second offer;
 * - a `connectPeer` while an `acceptPeer` for the peer is in flight
 *   waits for it (offering over it would cancel the attempt it is
 *   answering), and takes its outcome when that settles the pair
 *   ({@link answerSettles}): it offers only after an inconclusive one;
 * - concurrent `acceptPeer`s share one answer.
 *
 * Settled attempts are forgotten: the next call starts fresh. Another
 * tab's node is outside this gate, as it is outside this surface.
 */
export class PeerAttempts {
  readonly #connecting = new Map<string, Promise<PeerConnectOutcome>>();
  readonly #accepting = new Map<string, Promise<PeerConnectOutcome>>();

  /**
   * Run `drive` as this peer's offer, unless one is already under way,
   * the pair is already direct, or an answer under way settles it.
   *
   * `healthy` is read HERE, inside the gate and just before the offer,
   * never by the caller ahead of it: on a proxied surface the reading is
   * a round trip, and an `acceptPeer` could take the pair direct and
   * leave the gate between a caller's early reading and its offer, which
   * would then replace the healthy link.
   */
  connect(
    peer: string,
    healthy: HealthyDialog,
    drive: () => Promise<PeerConnectOutcome>,
  ): Promise<PeerConnectOutcome> {
    const key = peer.toLowerCase();
    const joining = this.#connecting.get(key);
    if (joining !== undefined) return joining;
    const answering = this.#accepting.get(key);
    const started = (async (): Promise<PeerConnectOutcome> => {
      if (answering !== undefined) {
        const answered = await answering.catch(() => null);
        if (answered !== null && answerSettles(answered)) return answered;
      }
      const dialog = await healthy();
      if (dialog !== undefined && dialog !== null) return { type: 'direct', peer, dialog };
      return drive();
    })();
    return PeerAttempts.#track(this.#connecting, key, started);
  }

  /** Run `drive` as this peer's answer, unless one is already under way. */
  accept(peer: string, drive: () => Promise<PeerConnectOutcome>): Promise<PeerConnectOutcome> {
    const key = peer.toLowerCase();
    const joining = this.#accepting.get(key);
    if (joining !== undefined) return joining;
    return PeerAttempts.#track(this.#accepting, key, drive());
  }

  static #track(
    map: Map<string, Promise<PeerConnectOutcome>>,
    key: string,
    started: Promise<PeerConnectOutcome>,
  ): Promise<PeerConnectOutcome> {
    map.set(key, started);
    // Forget it once settled, and only if no newer one took its place.
    const forget = () => {
      if (map.get(key) === started) map.delete(key);
    };
    started.then(forget, forget);
    return started;
  }
}

/** Offer, drive to an open channel, then handshake (§9 steps 3-4). */
export async function connectPeer(
  peer: string,
  primitives: PeerPrimitives,
  parse: (json: string) => PeerAttemptStatus,
): Promise<PeerConnectOutcome> {
  let dialog: string;
  try {
    dialog = await primitives.offer(peer);
  } catch (thrown) {
    const error = fromWasmError(thrown);
    if (error.message.includes(NO_ANNOUNCEMENT_PREFIX)) {
      return { type: 'noAnnouncement', peer, detail: error.message };
    }
    throw error;
  }

  const gathered = await driveAttempt(
    peer,
    dialog,
    (status) => status.state === 'open',
    primitives,
    parse,
  );
  if (gathered.type !== 'direct') return gathered;
  return handshakePeer(peer, dialog, primitives);
}

/**
 * Answer an offer and drive to a direct session.
 *
 * The accept is retried while the leaf reports no offer yet: the
 * envelope crosses A → anchor → B, so a page reacting to "the peer is
 * here" a moment before it lands would be told there is no offer,
 * which is true and not an answer. Every other refusal is returned or
 * thrown at once, because retrying those would only hide them.
 */
export async function acceptPeer(
  peer: string,
  primitives: PeerPrimitives,
  parse: (json: string) => PeerAttemptStatus,
): Promise<PeerConnectOutcome> {
  let dialog: string | null = null;
  const until = Date.now() + PEER_OFFER_WAIT_MS;
  for (;;) {
    try {
      dialog = await primitives.acceptOffer(peer);
      break;
    } catch (thrown) {
      const error = fromWasmError(thrown);
      if (error.message.includes(NO_ANNOUNCEMENT_PREFIX)) {
        return { type: 'noAnnouncement', peer, detail: error.message };
      }
      if (!error.message.includes(NO_OFFER_PREFIX) || Date.now() >= until) {
        throw error;
      }
      await tick();
    }
  }
  return driveAttempt(peer, dialog, (status) => status.direct, primitives, parse);
}

/**
 * Run the offerer's handshake on an attempt whose channel is open.
 *
 * The dialog is **checked** rather than assumed: the handshake
 * resolves to the dialog it actually ran for, and a mismatch is this
 * caller's attempt having been replaced underneath it. Reporting
 * `direct` with the caller's own dialog on any success is how a stale
 * caller was once told its superseded attempt had connected, while
 * the leaf had handshaken the attempt that replaced it.
 */
export async function handshakePeer(
  peer: string,
  dialog: string,
  primitives: PeerPrimitives,
): Promise<PeerConnectOutcome> {
  let handshaken: string;
  try {
    handshaken = await primitives.handshake(peer, dialog);
  } catch (thrown) {
    const superseded = classifySuperseded(thrown, peer, dialog);
    if (superseded !== null) return superseded;
    return { type: 'handshakeFailed', peer, dialog, detail: fromWasmError(thrown).message };
  }
  if (handshaken !== dialog) {
    return { type: 'superseded', peer, dialog, liveDialog: handshaken };
  }
  return { type: 'direct', peer, dialog };
}

/**
 * Pump one attempt until `done`, its terminal transition, or a newer
 * attempt replaces it.
 *
 * `{ type: 'direct' }` here means only "`done` answered true" — the
 * caller decides what remains.
 *
 * **Every terminal state ends the loop**, not only the two ICE ones.
 * An answerer waits for `direct`, and a channel that opened with a
 * session that never installed satisfies neither `direct` nor
 * `iceTimeout`: the leaf reports `failed`, which is this loop's exit.
 * The bound is the leaf's, which owns the attempt's deadline — a
 * timeout added here would be a second clock disagreeing with it.
 */
export async function driveAttempt(
  peer: string,
  dialog: string,
  done: (status: PeerAttemptStatus) => boolean,
  primitives: PeerPrimitives,
  parse: (json: string) => PeerAttemptStatus,
): Promise<PeerConnectOutcome> {
  for (;;) {
    let status: PeerAttemptStatus;
    try {
      status = parse(await primitives.candidate(peer, dialog));
    } catch (thrown) {
      const superseded = classifySuperseded(thrown, peer, dialog);
      if (superseded !== null) return superseded;
      throw fromWasmError(thrown);
    }
    if (status.dialog !== dialog) {
      return { type: 'superseded', peer, dialog, liveDialog: status.dialog };
    }
    if (done(status)) return { type: 'direct', peer, dialog };
    if (status.state === 'iceTimeout') return { type: 'iceTimeout', peer, dialog };
    if (status.state === 'udpBlocked') return { type: 'udpBlocked', peer, dialog };
    if (status.state === 'failed') {
      return {
        type: 'handshakeFailed',
        peer,
        dialog,
        detail:
          status.candidateError ??
          'the attempt reached its terminal transition without installing a session',
      };
    }
    await tick();
  }
}

/**
 * `superseded` when `thrown` is one of the leaf's two
 * attempt-ownership refusals, `null` when it is anything else.
 *
 * The two are kept apart in the reading, not collapsed: an ended
 * attempt cannot name a successor (`liveDialog: null`), while a
 * replaced one is refused by a leaf that knows which dialog took over
 * — so the replacement's id is recovered from the message rather than
 * reported as absent.
 */
function classifySuperseded(
  thrown: unknown,
  peer: string,
  dialog: string,
): PeerConnectOutcome | null {
  const { message } = fromWasmError(thrown);
  if (message.includes(REPLACED_ATTEMPT_MARKER)) {
    return { type: 'superseded', peer, dialog, liveDialog: liveDialogFrom(message) };
  }
  if (message.includes(NO_LIVE_ATTEMPT_PREFIX)) {
    return { type: 'superseded', peer, dialog, liveDialog: null };
  }
  return null;
}

/**
 * The dialog `attempt_for`'s refusal names as live, or `null` if the
 * message does not carry one.
 *
 * Reads the parenthesised `(dialog <id> is)` the leaf writes, in the
 * **16-hex spelling** every dialog on every surface uses — so a
 * recovered `liveDialog` compares equal to one that came out of a
 * reading. The leaf's message was decimal when this was first
 * written, which would have produced a `liveDialog` matching nothing.
 *
 * A refusal whose wording changed yields `null` — a missing successor
 * id, which is honest — rather than a wrong one.
 */
function liveDialogFrom(message: string): string | null {
  const match = /\(dialog ([0-9a-f]{16}) is\)/.exec(message);
  return match?.[1] ?? null;
}

/** One drive-loop tick. */
async function tick(): Promise<void> {
  const waited = Promise.withResolvers<void>();
  setTimeout(waited.resolve, PEER_TICK_MS);
  await waited.promise;
}
