/**
 * Slice D — the replica: snapshot install, delta apply, resync,
 * generations.
 *
 * ## What this module is
 *
 * The other half of slice C. The owner emits `man` + `snap` chunks and
 * later `delta`s; this decides what a caller is allowed to SEE as a
 * result. It is the production form of the state machine
 * `test/store/lifecycle-model.ts` models abstractly (§1.7a), with the
 * real codec, the real assembler (slice B1) and the real document.
 *
 * ## The three monotone numbers (§1.7a), which are different things
 *
 * - `installed` — the generation whose snapshot is published.
 * - `assembling` — the generation of the open assembly.
 * - `retired` — the highest generation ever **admitted**, installed or
 *   not. This is the watermark that makes an abandoned generation
 *   inadmissible for ever: `g > installed` would let a delayed manifest
 *   reopen a generation that was admitted, abandoned, and never
 *   published. Admission tests `g > retired`.
 *
 * Duplicate active manifests fall out of the same rule rather than
 * needing their own: admitting the first set `retired := g`, so the
 * second fails `g > retired` and the open assembly is untouched.
 *
 * ## What an empty slot is not
 *
 * `fenced` exists because an empty correlation slot is not consent. A
 * refused request fences the subscription's view; only `ready` accepts
 * unsolicited owner refreshes, and a `fenced` replica records no skip,
 * so an owner emission cannot lift a fence the caller placed.
 *
 * ## Scope
 *
 * Audience transitions and `resume` are slice F, actions and the ledger
 * are slice E. Both are refused here rather than half-served: this
 * module issues `join` and `resync` and nothing else.
 *
 * Its acceptance gate is reliable transfer (§4 D), exactly as B2′ — a
 * replica that applies deltas in order is not evidence that the
 * transport delivers them in order.
 */

import {
  Assembly,
  ASSEMBLY_DEADLINE_MS,
  AssemblyTable,
  type AssemblyManifest,
} from './assembly.js';
import { StoreCore } from './core.js';
import { StoreError, type StoreErrorCode } from './errors.js';
import type { Decimal, DeltaMessage, Hex, ManifestMessage, SnapMessage } from './wire.js';
import { decimalValue, decodeMessage, encodeMessage } from './wire.js';
import type { ActionSpec, InputSpec, StoreDefinition } from './types.js';

/** A frame to hand to the transport. */
export interface Request {
  readonly kind: 'join' | 'resync' | 'aud' | 'resume';
  readonly q: Hex;
  readonly frame: string;
}

/** The five states of §1.7a. */
export type ReplicaState = 'joining' | 'installing' | 'ready' | 'fenced' | 'closed';

/**
 * How many times an unanswered transition re-asks before the replica
 * gives the caller a terminal answer.
 *
 * Three: enough that a join survives independent losses, few enough
 * that a store nobody is serving stops asking. Each re-ask abandons
 * the subscription the owner may have opened for the previous one;
 * nothing can release it by name, because a replica whose MANIFEST
 * was lost never learned a handle to release — the owner's own idle
 * lease (`owner.ts`'s `HANDLE_LEASE_MS`) is what reclaims it, which
 * is why the ladder is bounded rather than endless.
 */
export const MAX_JOIN_REASKS = 3;

/** What one arrival produced. */
export interface Received {
  readonly out: readonly Request[];
  /** A stable, countable reason, when the arrival was not taken. */
  readonly dropped: string | null;
}

export interface ReplicaDeps<S extends object, A extends ActionSpec, I extends InputSpec> {
  readonly definition: StoreDefinition<S, A, I>;
  /** The document and its publication — the replica never commits directly. */
  readonly core: StoreCore<S, A, I>;
  /** The transport's current per-event budget. */
  readonly maxEventBytes: number;
  /** A fresh correlation id per request. Its uniqueness is the caller's. */
  readonly newQ: () => Hex;
  readonly now: () => number;
  /**
   * The audience this caller wants at the start.
   *
   * After a `setAudience` the DESIRED one is the replica's own, and it
   * is what every later request states — never the owner's older one.
   */
  readonly audience: readonly string[];
  /**
   * WHICH store of this definition to join — the owner's declared
   * address, carried on the join so the owner it is NOT addressed to
   * can refuse it. See `wire.ts`'s field table.
   */
  readonly store: string;
  /** The opaque join key the owner's policy reads. */
  readonly key: string;
}

/**
 * A replica of one owner's store.
 *
 * Every method returns the requests it wants sent; nothing here touches
 * a transport, so the state machine is testable without one.
 */
export class StoreReplica<S extends object, A extends ActionSpec, I extends InputSpec> {
  #state: ReplicaState = 'joining';
  /** The handle, once LEARNED from a manifest. Only `join` creates one. */
  #handle: Hex | null = null;
  #incarnation: Hex | null = null;
  #installed: bigint | null = null;
  /** The owner's revision for the installed view — not `core.revision`. */
  #revision: bigint | null = null;
  #assembling: bigint | null = null;
  #retired = 0n;
  #skipped: bigint | null = null;
  #behind = false;
  /** The desired-transition slot: one `q` per shared transition. */
  #slot: Hex | null = null;
  /**
   * When the outstanding transition was ASKED for, and how many times
   * the deadline has re-asked it.
   *
   * The assembly deadline covers an assembly that stalls half-built.
   * It cannot cover a transition that never produced one: a `join`
   * whose MANIFEST is lost opens nothing, so there is nothing to
   * expire, and before this the caller's `ready()` never settled and
   * nothing was ever sent again — the failure was pure silence. A
   * real browser produced exactly that, and the lost datagram was
   * 130-200 bytes: the manifest, far too small to be one of the
   * multi-kilobyte chunks.
   *
   * Never cleared: `tick` reads it only for a state with no open
   * assembly, and every path into `joining` or `installing` stamps it
   * on entry, so a stale value cannot be read.
   */
  #askedAt: number | null = null;
  #reasks = 0;
  /**
   * Bumped by every caller-initiated transition.
   *
   * The replica calls APPLICATION code in two places inside an
   * installation — the definition's validator, from the assembler, and
   * a subscriber, from the publication — and either may cancel or
   * change audience re-entrantly. Comparing the epoch across those
   * calls is what stops an installation that was cancelled halfway
   * from finishing and reporting `ready`: without it, a cancellation
   * from a validator was simply overwritten, and one from a
   * subscriber left the caller `ready` and not stale over the empty
   * view `cancel` had just published.
   */
  #epoch = 0;
  #assembly: Assembly | null = null;
  /** The caller's latest desired audience. Survives every transition. */
  #desired: readonly string[];
  /** Local waiters on the slot's transition, independently cancellable. */
  #waiters = 0;
  readonly #assemblies = new AssemblyTable();
  readonly #dropped: Record<string, number> = {};

  constructor(private readonly deps: ReplicaDeps<S, A, I>) {
    this.#desired = [...deps.audience];
  }

  /** The caller's latest desired audience. */
  get desired(): readonly string[] {
    return [...this.#desired];
  }

  /** Local waiters on the transition in flight. */
  get waiters(): number {
    return this.#waiters;
  }

  get state(): ReplicaState {
    return this.#state;
  }

  /** The learned handle, or null while none has been learned. */
  get handle(): Hex | null {
    return this.#handle;
  }

  /** The installed generation, as a canonical decimal, or null. */
  get installed(): Decimal | null {
    return this.#installed === null ? null : this.#installed.toString();
  }

  /** The owner's revision for the installed view, or null. */
  get revision(): Decimal | null {
    return this.#revision === null ? null : this.#revision.toString();
  }

  /** The watermark: the highest generation ever admitted. */
  get retired(): Decimal {
    return this.#retired.toString();
  }

  /** The highest refresh dropped mid-transition, or null. */
  get skipped(): Decimal | null {
    return this.#skipped === null ? null : this.#skipped.toString();
  }

  /** Arrivals not taken, by reason. */
  get dropped(): Readonly<Record<string, number>> {
    return { ...this.#dropped };
  }

  /**
   * The first request: a `join`.
   *
   * Only `join` creates a handle, so this is the only entry point from
   * a state with none — including after `no {closed}`, which discards
   * the handle and all of its generation state.
   */
  join(): Request {
    if (this.#state === 'closed') {
      throw new StoreError('closed', 'a closed replica cannot join again');
    }
    this.#epoch += 1;
    this.#waiters = 1;
    // A join is a FRESH SUBSCRIPTION with a new handle, and §1.7a
    // restarts that handle's generations at 1 — so every value scoped
    // to the old handle goes, `retired` included. Keeping the old
    // watermark wedges the rejoin permanently: the new handle's first
    // manifest names generation 1, fails `g > retired` against the
    // watermark of a handle that no longer exists, and nothing can
    // ever install again. Recovery knowledge goes for the same reason
    // — a skip recorded before a cancellation must not make the next
    // installation ask for work the caller cancelled.
    this.#reset();
    const q = this.deps.newQ();
    this.#slot = q;
    this.#askedAt = this.deps.now();
    this.#reasks = 0;
    this.#state = 'joining';
    this.deps.core.setStatus({ phase: 'connecting', stale: this.#published() });
    return {
      kind: 'join',
      q,
      frame: encodeMessage({
        k: 'join',
        q,
        def: this.deps.definition.id,
        store: this.deps.store,
        ver: this.deps.definition.version,
        key: this.deps.key,
        aud: [...this.#desired],
      }),
    };
  }

  /**
   * Change what this caller wants to see (§1.7).
   *
   * Three moments, deliberately separate: local intent is IMMEDIATE —
   * the view is cleared to `empty()` and the status is `syncing`,
   * before the owner has agreed to anything, because continuing to
   * render the old audience is the disclosure this exists to stop.
   * Owner acceptance allocates the generation, and installation
   * publishes it.
   *
   * Equal pending requests share one wire request and one `q`, with a
   * local waiter each: `q` is the transition's identity on the wire,
   * not a waiter's.
   */
  setAudience(names: readonly string[]): readonly Request[] {
    if (this.#state === 'closed') {
      throw new StoreError('closed', 'a closed replica cannot change audience');
    }
    this.#epoch += 1;
    const next = [...names];
    if (this.#state === 'installing' && sameAudience(next, this.#desired)) {
      // The same transition, already in flight. One wire request, one
      // slot, one more waiter.
      this.#waiters += 1;
      return [];
    }

    this.#desired = next;
    const h = this.#handle;
    // Immediately, whatever the owner later says.
    this.#retireAssembly();
    this.#installed = null;
    this.#revision = null;
    this.#skipped = null;
    this.#behind = false;
    this.deps.core.applySnapshot(this.deps.definition.empty());
    this.deps.core.setStatus({ phase: 'syncing', stale: false, error: null });

    if (h === null) {
      // Nothing to change the audience OF: only `join` creates a
      // handle, so the desired audience rides the join instead.
      this.#waiters = 1;
      return [this.join()];
    }

    // A newer transition rejects the previous waiters and takes the
    // slot with a NEW `q`.
    const q = this.deps.newQ();
    this.#slot = q;
    this.#askedAt = this.deps.now();
    this.#waiters = 1;
    this.#state = 'installing';
    return [{ kind: 'aud', q, frame: encodeMessage({ k: 'aud', q, h, aud: next }) }];
  }

  /**
   * The session was replaced (§1.6).
   *
   * A reconnect is not an audience change: the last snapshot is
   * RETAINED and marked stale, because what the caller may see has not
   * changed — only whether it is current. The request states the
   * caller's latest desired audience, never the owner's older one.
   */
  reconnect(): readonly Request[] {
    if (this.#state === 'closed') {
      throw new StoreError('closed', 'a closed replica cannot reconnect');
    }
    this.#epoch += 1;
    const h = this.#handle;
    // The lost session's assembly cannot be completed on the new one.
    this.#retireAssembly();

    if (h === null) {
      // No handle learned, so there is nothing to resume: a forged `h`
      // is exactly what `resume` must never be allowed to mint, so the
      // only honest request is a fresh `join`.
      return [this.join()];
    }

    const q = this.deps.newQ();
    this.#slot = q;
    this.#state = 'installing';
    this.deps.core.setStatus({ phase: 'reconnecting', stale: this.#published() });
    return [
      { kind: 'resume', q, frame: encodeMessage({ k: 'resume', q, h, aud: [...this.#desired] }) },
    ];
  }

  /**
   * One local waiter gives up.
   *
   * Cancellation is per waiter; the wire request is not. Cancelling the
   * last one fences the subscription — the view was invalidated at
   * request time and is NOT restored, because the caller asked for a
   * different one.
   */
  cancelWaiter(): void {
    if (this.#waiters > 0) this.#waiters -= 1;
    if (this.#waiters > 0) return;
    if (this.#state !== 'installing') return;
    this.cancel();
  }

  /** One decoded arrival from the owner. */
  receive(frame: string): Received {
    if (this.#state === 'closed') return this.#drop('closed');

    const decoded = decodeMessage(frame, { maxBytes: this.deps.maxEventBytes, as: 'replica' });
    if (!decoded.ok) return this.#drop(decoded.reason);
    const message = decoded.message;

    // A message naming a handle other than the learned one is dropped
    // and counted, whatever else it says. This is checked before the
    // kind, so a `delta` for a foreign handle cannot reach the patch
    // path at all.
    if ('h' in message && message.h !== undefined && this.#handle !== null && message.h !== this.#handle) {
      return this.#drop('foreign-handle');
    }

    switch (message.k) {
      case 'man':
        return this.#manifest(message);
      case 'snap':
        return this.#chunk(message);
      case 'delta':
        return this.#delta(message);
      case 'no':
        return this.#refused(message.code, message.q);
      case 'ok':
      case 'res':
        // Slice E's correlated replies. Counted, never half-applied.
        return this.#drop('unimplemented-kind');
    }
  }

  /**
   * The assembly deadline, and the lease the caller keeps by asking.
   *
   * Returns the requests the elapsed time produced — at most one
   * `resync`, because recovery is coalesced.
   */
  tick(): readonly Request[] {
    const now = this.deps.now();
    if (this.#state === 'installing' && this.#assembly !== null) {
      if (!this.#assembly.expired(now)) return [];
      return this.#abandon('assembly-deadline');
    }
    // A transition that produced NO assembly, past its deadline: the
    // manifest that would have opened one never arrived.
    if (this.#askedAt === null || now - this.#askedAt < ASSEMBLY_DEADLINE_MS) return [];
    if (this.#state !== 'joining' && this.#state !== 'installing') return [];
    if (this.#reasks >= MAX_JOIN_REASKS) {
      // Asked and re-asked, and nothing answered. The caller gets a
      // terminal answer rather than more silence: `#clearView` is
      // what carries the error to `getStatus()`, and the fence is
      // what makes `ready()` settle.
      this.#askedAt = null;
      this.#state = 'fenced';
      this.#slot = null;
      this.#retireAssembly();
      this.#clearView(
        'failed',
        new StoreError(
          'timeout',
          // NAMES THE STORE. A mistyped address settles typed, but
          // after the full ladder — and "the store never answered"
          // sends the reader looking at the network when the only
          // evidence of the real cause is a `join-other-store`
          // counter on a host they may not be able to read.
          `no store named ${this.deps.store} answered the join`,
        ),
      );
      return [];
    }
    // `join` resets the ladder, because a join is normally the CALLER
    // asking afresh. This one is the deadline's, so the count is
    // carried across it deliberately.
    const spent = this.#reasks;
    const again = this.join();
    this.#reasks = spent + 1;
    return [again];
  }

  /** The caller gives up this subscription. Fences it; no view. */
  cancel(): void {
    if (this.#state === 'closed') return;
    this.#epoch += 1;
    this.#retireAssembly();
    this.#slot = null;
    this.#state = 'fenced';
    // Recovery knowledge from a cancelled epoch must never make the
    // next installation ask for work the caller cancelled. `join`
    // clears it through `#reset`; `setAudience` clears it itself,
    // because it keeps the handle and so does not reset; and `resume`
    // deliberately does NOT, because a reconnect resumes the same
    // subscription and a skip recorded before the session was lost is
    // still news the owner has moved on.
    this.#skipped = null;
    this.#behind = false;
    this.#clearView('failed');
  }

  #manifest(message: ManifestMessage): Received {
    const g = decimalValue(message.g);

    // Solicited or not is decided by the slot, and the answer changes
    // which states may admit it.
    const solicited = message.q !== undefined && message.q === this.#slot;
    if (message.q !== undefined && !solicited) return this.#drop('stale-correlation');

    // The watermark goes FIRST, before any state-dependent branch. A
    // generation at or below it is spent — a duplicate of the
    // manifest being assembled, or one admitted and then abandoned —
    // and it is neither a skipped refresh nor news of anything. Asking
    // "am I mid-transition?" first reported a duplicate as a skip,
    // which would have provoked a resynchronization for a generation
    // the replica had already been told about.
    if (g <= this.#retired) return this.#drop('stale-generation');

    if (!solicited) {
      // An unsolicited refresh. `ready` accepts one; a replica
      // mid-transition records the skip so the owner's newer state is
      // recoverable rather than stranded; a fenced one records
      // nothing, because the fence is the caller's to lift.
      if (this.#state === 'fenced') return this.#drop('fenced');
      if (this.#state !== 'ready') {
        this.#skipped = this.#skipped === null ? g : max(this.#skipped, g);
        return this.#drop('mid-transition');
      }
    } else if (this.#state !== 'joining' && this.#state !== 'installing' && this.#state !== 'fenced') {
      return this.#drop('unexpected-manifest');
    }

    if (this.#handle === null) {
      // The handle is LEARNED here, from the manifest that answered the
      // join — the one moment a replica acquires one.
      this.#handle = message.h;
      this.#incarnation = message.inc;
    } else if (this.#incarnation !== null && message.inc !== this.#incarnation) {
      // A different incarnation is a different document, not a
      // refresh of this one.
      return this.#drop('foreign-incarnation');
    }

    // The previous assembly goes first: opening the replacement and
    // THEN retiring by handle reclaims the one just opened, and the
    // next chunk arrives at a closed assembly.
    this.#retireAssembly();
    // Admitted, so the watermark moves whatever happens next. A
    // generation this replica could not take must not be reopened by a
    // delayed manifest either.
    this.#retired = g;

    const manifest: AssemblyManifest = {
      h: message.h,
      g: message.g,
      r: message.r,
      n: message.n,
      bytes: message.bytes,
    };
    const opened = this.#assemblies.open(manifest, this.deps.now());
    if (!('accept' in opened)) {
      // An admissible manifest this replica cannot hold — over the
      // byte bound, or the table is full. The generation is consumed,
      // so recovery is a new one, not a retry of this.
      return { out: this.#abandon(opened.reason), dropped: opened.reason };
    }

    this.#assembling = g;
    this.#assembly = opened;
    this.#state = 'installing';
    this.deps.core.setStatus({ phase: 'syncing', stale: this.#published() });
    return { out: [], dropped: null };
  }

  #chunk(message: SnapMessage): Received {
    const assembly = this.#assembly;
    // No open assembly, no chunk: every supersession path — a newer
    // manifest, cancellation, the deadline, a refusal — retires the
    // assembly, so a chunk for a superseded installation arrives here
    // with nothing to join and cannot publish.
    if (assembly === null) return this.#drop('no-open-assembly');
    // The chunk's generation is NOT re-checked here. `Assembly.accept`
    // admits a chunk only when its whole `(h, g, r, n)` matches the
    // manifest it was opened with, and that manifest's `g` is what
    // `assembling` holds — so identity is the assembler's property
    // (slice B1) and a second comparison here would be a claim that
    // cannot fail rather than a check that can.
    const epoch = this.#epoch;
    const outcome = assembly.accept(
      { h: message.h, g: message.g, r: message.r, n: message.n, i: message.i, d: message.d },
      raw => this.deps.definition.state(raw),
      this.deps.now(),
    );
    // No epoch check here, and not for want of trying: a
    // cancellation from the VALIDATOR is caught by the one in
    // `#install` — the epoch it compares moved for the validator's
    // cancellation exactly as it does for a subscriber's — and I
    // could not construct an observable the two answer differently,
    // including the notification count. An inverse that goes green is
    // a guard that is not there, so it is not there.
    if (!outcome.ok) {
      // A fatal refusal has destroyed the assembly; a non-fatal one
      // (a duplicate) leaves it open and is only counted.
      if (!outcome.fatal) return this.#drop(outcome.reason);
      return { out: this.#abandon(outcome.reason), dropped: outcome.reason };
    }
    if (!outcome.done) return { out: [], dropped: null };

    return this.#install(outcome.document, message.g, message.r, epoch);
  }

  #install(document: unknown, g: Decimal, r: Decimal, epoch: number): Received {
    // Reached only from a completed assembly, which the assembler
    // opened for this generation and which every supersession path
    // retires — so `g` is `assembling` by construction and there is
    // nothing left here to fence against. The fence is the retirement,
    // witnessed on each path that supersedes ("publishes nothing after
    // cancellation mid-assembly"), not a comparison at the end.
    this.deps.core.applySnapshot(document);
    this.#assembling = null;
    this.#assembly = null;
    this.#assemblies.reclaim(this.#handle as Hex, g);
    if (this.#epoch !== epoch) {
      // A SUBSCRIBER cancelled while this document was being
      // published. Its `cancel` already cleared the view; saying
      // `ready` over that is the worst of both — an empty world
      // reported as current.
      this.#installed = null;
      this.#revision = null;
      this.#clearView('failed');
      return this.#drop('superseded-installation');
    }
    this.#installed = decimalValue(g);
    this.#revision = decimalValue(r);
    this.#slot = null;
    this.#state = 'ready';
    this.deps.core.setStatus({ phase: 'ready', stale: false, error: null });

    // Then §1.8's skipped-refresh recovery: a refresh dropped while
    // mid-transition would otherwise make every later delta
    // inadmissible, and owner and replica would never converge. One
    // coalesced request, however many were dropped.
    return { out: this.#recoverIfBehind(), dropped: null };
  }

  #delta(message: DeltaMessage): Received {
    if (this.#state !== 'ready') {
      // Mid-transition the owner has moved past the revision being
      // assembled. One flag, coalesced into the same recovery.
      this.#behind = true;
      return this.#drop('not-ready');
    }
    if (this.#installed === null || decimalValue(message.g) !== this.#installed) {
      return this.#drop('stale-generation');
    }
    if (this.#revision === null || decimalValue(message.base) !== this.#revision) {
      // A gap. The delta is not applied to the wrong base — that would
      // publish a document neither side believes in.
      return { out: this.#resync('gap'), dropped: 'gap' };
    }

    const outcome = this.deps.core.applyDelta(message.ops);
    if (!outcome.ok) {
      // A patch that does not apply or does not validate is a
      // divergence, so the view is refetched rather than guessed at.
      return { out: this.#resync(outcome.reason), dropped: outcome.reason };
    }
    this.#revision = decimalValue(message.r);
    return { out: [], dropped: null };
  }

  #refused(code: StoreErrorCode, q: Hex | undefined): Received {
    if (code === 'owner-lost') {
      // Terminal: the incarnation that held the document is gone, so
      // there is nothing to rejoin.
      this.#retireAssembly();
      this.#state = 'closed';
      this.#handle = null;
      this.#clearView('closed');
      return { out: [], dropped: 'owner-lost' };
    }

    if (code === 'closed') {
      // The handle is unusable — but only if this refusal is about the
      // handle NOW. An unsolicited `no {closed}` is the expiry notice
      // (§1.7a), and one correlated to the live slot refuses the
      // request in flight. A refusal carrying the `q` of a request
      // this replica has already finished or abandoned is news about
      // nothing: tearing down a healthy view on it discards an
      // installed document, the watermark and the handle because a
      // late frame answered a dead question.
      if (q !== undefined && q !== this.#slot) return this.#drop('stale-correlation');
      const wasFenced = this.#state === 'fenced';
      this.#reset();
      if (wasFenced) {
        // An expiry notice is no more the caller's consent than an
        // owner refresh was: the fence stays.
        this.#clearView('failed');
        return { out: [], dropped: 'closed-while-fenced' };
      }
      this.#clearView('connecting');
      return { out: [this.join()], dropped: 'closed' };
    }

    if (q === undefined || q !== this.#slot) return this.#drop('stale-correlation');

    // The REQUEST was refused, not the subscription: the handle
    // survives, and the caller's next request lifts the fence.
    this.#retireAssembly();
    this.#slot = null;
    this.#state = 'fenced';
    this.#clearView('failed', new StoreError(code, `the request was refused: ${code}`));
    return { out: [], dropped: `refused-${code}` };
  }

  /** Abandon the open assembly and ask for the view again. */
  #abandon(reason: string): readonly Request[] {
    this.#retireAssembly();
    // `retired` KEEPS the generation: the abandoned one must not be
    // reopened by a delayed manifest.
    this.#assembling = null;
    this.#state = 'installing';
    this.deps.core.setStatus({ phase: 'syncing', stale: this.#published() });
    return this.#resync(reason);
  }

  #resync(_reason: string): readonly Request[] {
    const h = this.#handle;
    if (h === null) return [];
    const q = this.deps.newQ();
    this.#slot = q;
    this.#askedAt = this.deps.now();
    this.#state = 'installing';
    this.#skipped = null;
    this.#behind = false;
    // The published view is known to be behind — that is why this is
    // being sent. Retained, because the audience has not changed, but
    // NOT current, and the application is told so: reporting `ready`
    // here means every consumer believes a stale world is live.
    this.deps.core.setStatus({
      phase: this.#published() ? 'reconnecting' : 'syncing',
      stale: this.#published(),
    });
    // `(g, have)` are ADVISORY historical position — what this replica
    // actually has, never a claim about the owner's current
    // generation. A replica installed at A whose replacement B timed
    // out can only honestly name A, and that is exactly the replica
    // that most needs recovering (§1.8).
    return [
      {
        kind: 'resync',
        q,
        frame: encodeMessage({
          k: 'resync',
          q,
          h,
          g: this.#installed === null ? '0' : this.#installed.toString(),
          have: this.#revision === null ? '0' : this.#revision.toString(),
        }),
      },
    ];
  }

  #recoverIfBehind(): readonly Request[] {
    const skipped = this.#skipped;
    const behind = this.#behind;
    if (!behind && (skipped === null || this.#installed === null || skipped <= this.#installed)) {
      this.#skipped = null;
      this.#behind = false;
      return [];
    }
    return this.#resync(behind ? 'behind' : 'skipped-refresh');
  }

  /** Discard everything scoped to one handle's subscription. */
  #reset(): void {
    this.#retireAssembly();
    this.#handle = null;
    this.#incarnation = null;
    this.#installed = null;
    this.#revision = null;
    this.#retired = 0n;
    this.#skipped = null;
    this.#behind = false;
    this.#slot = null;
  }

  #retireAssembly(): void {
    if (this.#assembly !== null) {
      this.#assembly.reclaim();
      this.#assembly = null;
    }
    if (this.#handle !== null) this.#assemblies.reclaimHandle(this.#handle);
    this.#assembling = null;
  }

  #published(): boolean {
    return this.#installed !== null;
  }

  #clearView(phase: 'connecting' | 'failed' | 'closed', error: StoreError | null = null): void {
    this.#installed = null;
    this.#revision = null;
    this.deps.core.applySnapshot(this.deps.definition.empty());
    this.deps.core.setStatus({ phase, stale: false, error });
  }

  #drop(reason: string): Received {
    this.#dropped[reason] = (this.#dropped[reason] ?? 0) + 1;
    return { out: [], dropped: reason };
  }
}

function max(a: bigint, b: bigint): bigint {
  return a > b ? a : b;
}

function sameAudience(a: readonly string[], b: readonly string[]): boolean {
  return a.length === b.length && a.every((label, index) => label === b[index]);
}
