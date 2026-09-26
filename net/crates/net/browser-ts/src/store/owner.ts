/**
 * Owner dispatch: join, admission, `authorize`, projection, `man`
 * (brief §1.3, §1.5, §1.12, §2) — slice C.
 *
 * ## The authenticated origin is an argument, never a field
 *
 * {@link StoreOwner.receive} takes the **authenticated originating
 * peer** as a parameter, and there is no way to name a caller inside a
 * store frame at all (§2's shapes have no originator field). That is
 * the whole of §1.3's rule expressed as a signature: the identity comes
 * from the session the bytes arrived on, which the transport
 * authenticated — `LeafEvent::StreamData.peer_node` on the Rust side,
 * `LeafStream.peerNode` in the package — and a dispatcher cannot read
 * it off the payload because the payload does not carry one.
 *
 * A caller could still *lie to this function*, so the contract is
 * stated where it is enforceable: whoever calls `receive` must pass the
 * peer the transport proved, and the store's own entry points will
 * (slice G). Nothing here can make a wrong argument right, and
 * pretending otherwise is the mistake this module refuses to make.
 *
 * ## What is here, and what is deliberately not
 *
 * Here: `join` admission, handle binding and re-checking, `authorize`
 * for the read request, the audience projection, and the `man` + `snap`
 * emission that follows. Refusals for the paths that reach those.
 *
 * Not here: `act`/`in`/`res` and the ledger (slice E), `aud`/`resume`
 * transitions (slice F), `delta` emission (slice D). Those kinds are
 * refused `not-ready` or `invalid-data` rather than half-implemented,
 * and the refusal says which layer owns them.
 */

import { Assembly, AssemblyTable } from './assembly.js';
import { assertChunkingFits, chunkSnapshot } from './chunker.js';
import { StoreCore } from './core.js';
import { StoreError, type StoreErrorCode } from './errors.js';
import type { JsonObject, JsonValue } from './json.js';
import {
  canonicalRequest,
  digestBinding,
  inlineBinding,
  LedgerTable,
  needsDigest,
  type Outcome,
  type RequestBinding,
} from './ledger.js';
import type {
  AccessRequest,
  ActionContext,
  Cancel,
  ActionSpec,
  InputSpec,
  Parse,
  StoreDefinition,
} from './types.js';
import {
  decimalValue,
  decodeMessage,
  encodeMessage,
  utf8Length,
  HANDLE_HEX_LENGTH,
  INCARNATION_HEX_LENGTH,
  type CallerMessage,
  type Hex,
  type WireOp,
} from './wire.js';

/** Actions in flight per owner, against the pending bound (§2). */
export const MAX_PENDING_ACTIONS = 32;

/** Handles one owner will hold (brief §2). */
export const MAX_HANDLES = 256;

/** How long a handle lives without an accepted authenticated message. */
export const HANDLE_LEASE_MS = 60_000;

/** What the owner needs from its host. */
export interface OwnerDeps<S extends object, A extends ActionSpec, I extends InputSpec> {
  readonly definition: StoreDefinition<S, A, I>;
  /**
   * WHICH store of this definition this owner is — the address a
   * joiner names in `join.store`.
   *
   * A node may host several stores of one definition (a lobby and a
   * match; three worlds in a harness), every host store on that node
   * receives every frame, and a `join` names no handle. So without
   * this, the store that answers a join is whichever listener
   * happened to be registered first — and a caller asking for B got
   * A's document, A's handle and A's state mutated by its actions.
   * Found by review: two COLD stores, join the second, and the first
   * answers.
   */
  readonly store: string;
  /**
   * The owner's policy. Receives the **authenticated** caller, because
   * that is the only identity this module will ever hand it.
   */
  authorize(request: AccessRequest<A, I>): boolean;
  /**
   * The audience projection: what this caller may see. Returning the
   * whole state is a decision, not a default — `empty()` is what
   * absence looks like.
   */
  project(state: S, audience: readonly string[]): S;
  /** The admissible frame size, derived from the transport (§1.9). */
  readonly maxEventBytes: number;
  /** Monotone clock, milliseconds. */
  now(): number;
  /** 32 lowercase hex. Unguessable: a handle is a capability. */
  newHandle(): Hex;
  /** 16 lowercase hex, one per owner incarnation. */
  newIncarnation(): Hex;
  /**
   * Whether a projection can be taken right now.
   *
   * A host that is mid-frame, or whose world is being rebuilt, answers
   * `false` and the control is DEFERRED rather than refused (§1.8) — a
   * control is never answered `not-ready`.
   */
  canProject(): boolean;
  /**
   * The action handlers, one per declared action.
   *
   * Each runs inside ONE synchronous transaction: it stages writes
   * through its context and returns the output, or throws to reject.
   * A throw is an `action-rejected` outcome that is RETAINED (§1.10),
   * not an absent one.
   */
  readonly actions: ActionHandlers<S, A>;
  /** The latest-value input handlers. Fire-and-forget, no reply. */
  readonly inputs: InputHandlers<S, I>;
}

/** One handler per declared action. */
export type ActionHandlers<S extends object, A extends ActionSpec> = {
  readonly [K in keyof A]: (
    input: A[K]['input'],
    context: ActionContext<S>,
  ) => A[K]['output'];
};

/** One handler per declared input. */
export type InputHandlers<S extends object, I extends InputSpec> = {
  readonly [K in keyof I]: (input: I[K], context: ActionContext<S>) => void;
};

/** One admitted subscription. */
export interface OwnerHandle {
  readonly h: Hex;
  readonly inc: Hex;
  /** The authenticated peer this handle is bound to (§1.3). */
  readonly peer: string;
  readonly audience: readonly string[];
  /** Highest generation allocated for this handle; monotone (§1.7a). */
  readonly generation: number;
  /** The revision its current installation was taken at. */
  readonly revision: number;
  /** Last accepted authenticated message, for the lease. */
  readonly lastSeen: number;
}

/** A frame the owner wants sent, and to whom. */
export interface Outbound {
  readonly peer: string;
  readonly h: Hex | null;
  readonly frame: string;
}

/** What one inbound frame produced. */
export interface Dispatched {
  readonly out: readonly Outbound[];
  /** A stable, countable refusal reason, when the frame was refused. */
  readonly refused: string | null;
  /**
   * The rest of an action whose request binding needed a digest.
   *
   * §1.10 puts one `await` in the ledger path, for inputs too large to
   * retain verbatim. Rather than make every join and `alive` async to
   * serve that one case, the synchronous answer comes back immediately
   * and the remainder is handed over explicitly. A transport sends
   * `out`, then sends the deferred result's `out` when it settles.
   *
   * `null` on every other path, which is all of them but one.
   */
  readonly deferred: Promise<Dispatched> | null;
}

interface Counters {
  [reason: string]: number;
}

/**
 * Placeholder correlation and handle for sizing a local result as the
 * `res` a replica would receive. Same lengths as real ones, so the
 * budget test measures the same bytes.
 */
const LOCAL_Q = '0'.repeat(16) as Hex;
const LOCAL_H = '0'.repeat(32) as Hex;

/**
 * The owner half of the store protocol.
 *
 * Holds no transport: frames come in as text with an authenticated
 * peer, and go out as text addressed to a peer. That is what lets the
 * whole of slice C be exercised without a network.
 */
export class StoreOwner<S extends object, A extends ActionSpec, I extends InputSpec> {
  private readonly handles = new Map<Hex, OwnerHandle>();
  private readonly counters: Counters = {};
  private readonly chunkBytes: number;
  private readonly incarnation: Hex;
  /**
   * The authoritative document.
   *
   * The owner's state IS a store: one place that validates, reconciles,
   * counts revisions and runs a handler's synchronous transaction.
   * Keeping a second copy beside it would give handler writes and
   * `commit` two truths to disagree about.
   */
  private readonly core: StoreCore<S, A, I>;
  /** Digest computations in flight, against the pending bound (§2). */
  private pending = 0;
  private readonly ledgers = new LedgerTable();
  /**
   * One pending projection per handle (§1.8).
   *
   * A newer control REPLACES the pending one rather than queueing
   * behind it, which is what keeps this a bound rather than a buffer.
   */
  private readonly deferred = new Map<Hex, { readonly q: Hex; readonly peer: string }>();
  /** The revision every installed view is at, before the commit. */
  private revisionBeforeCommit = 0;

  constructor(private readonly deps: OwnerDeps<S, A, I>) {
    // Fail fast rather than discover the numbers on the first
    // projection (§1.9).
    this.chunkBytes = assertChunkingFits(deps.maxEventBytes);
    this.incarnation = deps.newIncarnation();
    if (this.incarnation.length !== INCARNATION_HEX_LENGTH) {
      throw new StoreError('invalid-data', 'newIncarnation must return 16 lowercase hex');
    }
    this.core = new StoreCore({ definition: deps.definition, initialState: deps.definition.empty() });
  }

  /** Counters a host can report. Every refusal moves exactly one. */
  snapshotCounters(): Readonly<Counters> {
    return { ...this.counters };
  }

  /** Live handles, for a host's own bounds reporting. */
  get handleCount(): number {
    return this.handles.size;
  }

  /** The handle record, for a test or a host's introspection. */
  handle(h: Hex): OwnerHandle | undefined {
    return this.handles.get(h);
  }

  /**
   * Replace the authoritative state, as `setState` does.
   *
   * Inside an open transaction the write JOINS it (`applySnapshot`
   * stages) and the delta emission is DEFERRED to that transaction's
   * commit (#5): `decide`/`input` diff the committed before/after at
   * the transaction's end and ship exactly one delta, and a rollback
   * ships nothing at all. Emitting here would put a staged-world
   * delta on the wire that no rollback retracts.
   */
  commit(next: S): Dispatched {
    const inTransaction = this.core.inTransaction;
    const previous = this.core.getState() as S;
    if (!inTransaction) this.revisionBeforeCommit = this.core.revision;
    this.core.applySnapshot(next);
    if (inTransaction) return this.accept([]);
    const current = this.core.getState() as S;
    if (Object.is(previous, current)) {
      // An equivalent commit is not a change (§4): no revision moved,
      // so there is nothing to tell anyone.
      return this.accept([]);
    }
    return this.accept(this.propagate(previous, current));
  }

  /**
   * Tell every installed handle what changed (§1.9).
   *
   * Per audience, because a delta of the raw state would ship what a
   * projection exists to withhold: the previous and current
   * projections are both taken and the difference between THOSE is
   * what goes out. The previous one is recomputed from the previous
   * state rather than retained per handle — one projection can be a
   * megabyte, and keeping one per handle is 256 of them.
   *
   * When a delta would not fit the message budget the owner allocates
   * a new generation instead and sends a manifest: an owner-initiated
   * replacement, admissible at a `ready` replica (§1.8).
   */
  private propagate(previous: S, current: S): Outbound[] {
    const out: Outbound[] = [];
    const base = String(this.revisionBeforeCommit);
    const r = String(this.core.revision);
    // One projection pair and one root diff per DISTINCT audience,
    // shared by every handle that carries it. Projecting and diffing
    // per handle made this O(handles × world) JSON work per commit:
    // both projections were freshly built per handle, so `shallowDiff`'s
    // identity test could never fire and every key paid its
    // serialization comparison once per handle.
    const diffs = new Map<string, readonly WireOp[] | null>();
    for (const handle of [...this.handles.values()]) {
      // No `generation === 0` test: a handle is created and installed
      // in one synchronous `join`, and a join whose emission fails
      // deletes it, so a live handle always has a generation. What
      // does need testing is a handle whose NEXT projection is still
      // pending — a delta against a view it has not installed would
      // name a revision it never had, and it has already cleared the
      // old one locally.
      if (this.deferred.has(handle.h)) continue;

      // Re-authorize before shipping. Permission does not carry forward
      // across time (the rule the read paths below all hold to), and the
      // delta feed IS the read delivered fresh: a policy revocation has to
      // stop it, not only the next projection. `join`, `aud`/`resume` and
      // `resync` are all gated, and the `resync` refusal deliberately keeps
      // the handle warm — which is why the feed cannot rely on the grant
      // having been checked once at admission. The handle goes with the
      // refusal here because the feed is the grant: leaving it alive would
      // let `alive` renew the lease of a peer the policy now forbids.
      if (!this.permitsRead(handle.peer, handle.audience)) {
        this.forget(handle.h);
        // `closed`, the wire's legal unsolicited refusal (§1.12 admits
        // exactly `closed` and `owner-lost` without a `q`): it closes
        // the subscription, the replica rejoins, and the JOIN — a
        // request path that CAN carry a refusal — answers `forbidden`
        // correlated. A q-less `forbidden` is not a frame the codec
        // admits; `encodeMessage` throws it out of `propagate`.
        out.push(this.no(handle.peer, handle.h, 'closed', null));
        continue;
      }

      const audienceKey = JSON.stringify(handle.audience);
      let ops = diffs.get(audienceKey);
      if (ops === undefined) {
        const before = this.project(handle.audience, previous);
        const after = this.project(handle.audience, current);
        ops =
          before === null || after === null
            ? null
            : shallowDiff(before as JsonObject, after as JsonObject);
        diffs.set(audienceKey, ops);
      }
      if (ops === null) {
        // `project` and its `empty()` fallback both failed: there is
        // nothing truthful to ship, exactly as when `install` cannot
        // carry a projection. Silence — the previous `continue` — left
        // the replica at its old revision reporting `ready, stale:
        // false` until the next change to that audience happened to
        // resync it. The handle goes with a typed notice: `closed`,
        // the only unsolicited refusal the wire admits (§1.12), which
        // closes the subscription and provokes a rejoin — and the
        // JOIN, a request path that CAN carry a refusal, answers
        // `capacity` correlated if the projection is still broken.
        this.forget(handle.h);
        out.push(this.no(handle.peer, handle.h, 'closed', null));
        continue;
      }
      if (ops.length === 0) continue;

      const delta = encodeDeltaWithin(
        { k: 'delta', h: handle.h, g: String(handle.generation), base, r, ops },
        this.deps.maxEventBytes,
      );
      if (delta !== null) {
        out.push({ peer: handle.peer, h: handle.h, frame: delta });
        continue;
      }
      // Too large to carry as a patch: replace the whole view.
      const frames = this.install(handle, null);
      if (frames === null) {
        this.forget(handle.h);
        // `closed`, the legal unsolicited refusal: the replacement
        // could not be carried at all. The rejoin it provokes is
        // refused `capacity` correlated at the join.
        out.push(this.no(handle.peer, handle.h, 'closed', null));
        continue;
      }
      out.push(...frames);
    }
    return out;
  }

  /** The authoritative document, for a host's own reads. */
  getState(): S {
    return this.core.getState() as S;
  }

  /**
   * Subscribe to the authoritative document.
   *
   * The owner's own code watches the state it serves — a host renders
   * from this, and an action's commit is the same revision its
   * subscribers see.
   */
  subscribe(listener: (state: S, previous: S) => void): Cancel {
    return this.core.subscribe((state, previous) => {
      listener(state as S, previous as S);
    });
  }

  /** The current authoritative revision. */
  get currentRevision(): number {
    return this.core.revision;
  }

  /**
   * The host's own player: whether `peer` may read `audience`.
   *
   * The same `authorize` call a `join` makes. There is no handle, no
   * lease and no ledger — the host's player has no wire to lose a
   * frame on and no request to replay — but there is the policy.
   */
  localRead(peer: string, audience: readonly string[]): boolean {
    return this.permitsRead(peer, audience);
  }

  /** The host's own player: the projection a replica of `audience` receives. */
  localView(audience: readonly string[]): S | null {
    return this.project(audience);
  }

  /**
   * The host's own player: one action, through the ladder a replica's
   * `act` takes after the wire — known name, `authorize`, then one
   * transaction that parses the input, runs the handler and validates
   * the output, and a result that must fit the message budget a
   * replica would have received it in.
   *
   * `input` is what `parseStoreJson` produced from the caller's value,
   * so the handler sees exactly what it would see from a replica.
   */
  localAct(
    name: string,
    input: JsonValue,
    peer: string,
  ): { readonly outcome: Outcome; readonly dispatched: Dispatched } {
    if (!Object.prototype.hasOwnProperty.call(this.deps.definition.actions, name)) {
      return {
        outcome: { kind: 'refusal', code: 'invalid-data' },
        dispatched: this.refuse('local-act-unknown-name'),
      };
    }
    if (!this.permitsAction(peer, name, input)) {
      return {
        outcome: { kind: 'refusal', code: 'forbidden' },
        dispatched: this.refuse('local-act-forbidden'),
      };
    }
    const spec = this.deps.definition.actions[name as keyof A];
    const before = this.core.getState() as S;
    this.revisionBeforeCommit = this.core.revision;
    let outcome: Outcome;
    try {
      const handler = this.deps.actions[name as keyof A];
      const produced = this.core.transact(context => {
        const out = spec.output(handler(spec.input(input), context)) as JsonObject;
        // The budget a replica's `res` would have had to fit. A result
        // that works for the host and not for its players is a bug the
        // host would never see, so it is refused here the same way.
        const frame = encodeMessage({
          k: 'res',
          q: LOCAL_Q,
          h: LOCAL_H,
          s: '1',
          out,
        });
        if (utf8Length(frame) > this.deps.maxEventBytes) {
          throw new StoreError('capacity', 'the result does not fit the message budget');
        }
        return out;
      }, peer);
      outcome = { kind: 'result', out: produced };
    } catch (error) {
      outcome = {
        kind: 'refusal',
        code:
          error instanceof StoreError && error.code === 'capacity' ? 'capacity' : 'action-rejected',
      };
    }
    const after = this.core.getState() as S;
    const propagated = Object.is(before, after) ? [] : this.propagate(before, after);
    return {
      outcome,
      dispatched:
        outcome.kind === 'refusal'
          ? this.refuse(`local-act-${outcome.code}`, propagated)
          : this.accept(propagated),
    };
  }

  /**
   * The host's own player: one latest-value input, through the same
   * ladder as a replica's `in` after the wire. No sequence: a local
   * call cannot arrive out of order.
   */
  localInput(name: string, input: JsonValue, peer: string): Dispatched {
    if (!Object.prototype.hasOwnProperty.call(this.deps.definition.inputs, name)) {
      return this.refuse('local-in-unknown-name');
    }
    if (!this.permitsInput(peer, name, input)) return this.refuse('local-in-forbidden');
    const before = this.core.getState() as S;
    this.revisionBeforeCommit = this.core.revision;
    try {
      this.core.transact(context => {
        const parse = this.deps.definition.inputs[name as keyof I];
        const handler = this.deps.inputs[name as keyof I];
        handler(parse(input), context);
      }, peer);
    } catch {
      return this.refuse('local-in-rejected');
    }
    const after = this.core.getState() as S;
    return this.accept(Object.is(before, after) ? [] : this.propagate(before, after));
  }

  /**
   * Dispatch one inbound frame.
   *
   * `peer` is the **authenticated** originating caller — see the module
   * note. It is not read from `frame`, and no frame can carry one.
   */
  receive(frame: string, peer: string, now: number = this.deps.now()): Dispatched {
    const decoded = decodeMessage(frame, { maxBytes: this.deps.maxEventBytes, as: 'owner' });
    if (!decoded.ok) return this.refuse(decoded.reason, []);

    const message = decoded.message;
    if (message.k === 'join') return this.join(message, peer, now);

    // Every other kind names a handle. Binding is re-checked on every
    // message, not only at admission (§1.3): a session replacement does
    // not re-authorize a handle.
    const bound = this.bind(message.h, peer, now);
    if (typeof bound === 'string') {
      return this.refuse(bound, [this.no(peer, message.h, 'closed', requestOf(message))]);
    }

    switch (message.k) {
      case 'alive': {
        this.handles.set(bound.h, { ...bound, lastSeen: now });
        return this.accept([{ peer, h: bound.h, frame: encodeMessage({ k: 'ok', q: message.q, h: bound.h }) }]);
      }
      case 'leave': {
        this.forget(bound.h);
        return this.accept([{ peer, h: bound.h, frame: encodeMessage({ k: 'ok', q: message.q, h: bound.h }) }]);
      }
      case 'act':
        return this.action(message, bound, peer, now);
      case 'in':
        return this.input(message, bound, peer, now);
      case 'resync': {
        // §1.8. `g` and `have` are ADVISORY historical position, so
        // they are deliberately not read: a replica installed at A
        // whose replacement B timed out can only honestly name A, and
        // a current-generation check would refuse exactly the caller
        // that most needs recovering.
        //
        // But a resync takes a FRESH projection at the current
        // revision, which makes it a READ — so the policy rules on it
        // exactly as it ruled on the join. Binding the handle proves
        // WHO is asking; it does not carry a permission decision
        // forward across time, and a read the policy has since revoked
        // must not be served because the handle is still warm. The
        // handle survives the refusal: the request was refused, not
        // the subscription.
        if (!this.permitsRead(peer, bound.audience)) {
          return this.refuse(
            'resync-forbidden',
            [this.no(peer, bound.h, 'forbidden', message.q)]
          );
        }
        const renewed = { ...bound, lastSeen: now };
        this.handles.set(bound.h, renewed);
        const frames = this.install(renewed, message.q);
        if (frames === null) {
          this.forget(bound.h);
          return this.refuse(
            'resync-projection-capacity',
            [this.no(peer, bound.h, 'capacity', message.q)]
          );
        }
        return this.accept(frames);
      }
      case 'aud':
      case 'resume': {
        // §1.7/§1.6. Both state the caller's DESIRED audience and both
        // are reads, so both are authorized for that audience before
        // any projection is taken — an audience the caller may not
        // read is refused, never narrowed to one it may.
        //
        // `resume` differs from `resync` exactly here: a reconnecting
        // caller must not be re-shown whatever audience the owner
        // happened to hold last, because that audience may be wider
        // than what the caller now wants.
        // No audience-bound check here: the parser refuses
        // `audience-too-many` at rung 6 (wire.ts, witnessed in
        // wire.test.ts), so a frame carrying more than the bound never
        // reaches dispatch. Re-checking it would be a claim.
        if (!this.permitsRead(peer, message.aud)) {
          return this.refuse(
            `${message.k}-forbidden`,
            [this.no(peer, bound.h, 'forbidden', message.q)]
          );
        }

        // The audience is rebound BEFORE the projection, so the
        // projection can only ever be of the audience just authorized.
        const rebound: OwnerHandle = {
          ...bound,
          audience: [...message.aud],
          lastSeen: now,
        };
        this.handles.set(rebound.h, rebound);

        // A caller control supersedes an in-flight emission (§1.7b).
        // There is nothing to retire on THIS side: an emission is
        // built complete and handed to the transport in one return, so
        // the owner holds no unsent remainder. What enforces the rule
        // is the replica refusing the superseded generation's chunks,
        // and the new generation this allocates — the superseded one
        // stays consumed either way.

        if (!this.deps.canProject()) {
          // §1.8's one disposition: DEFER, never refuse. A control is
          // never answered `not-ready`, and holding one pending
          // projection per handle is what lets the caller's own
          // deadline be the bound.
          this.deferred.set(rebound.h, { q: message.q, peer });
          return this.accept([]);
        }

        const frames = this.install(rebound, message.q);
        if (frames === null) {
          this.forget(rebound.h);
          return this.refuse(
            `${message.k}-projection-capacity`,
            [this.no(peer, rebound.h, 'capacity', message.q)]
          );
        }
        return this.accept(frames);
      }
      default:
        return this.refuse('unreachable-kind');
    }
  }

  /**
   * One action (§1.10).
   *
   * The ladder: gameplay readiness, then the sequence, then the
   * ledger's disposition, then — only for a genuinely new request —
   * `authorize` and one synchronous transaction.
   *
   * The digest path is the one place this module awaits. Everything
   * decided before the await is discarded and decided again after it:
   * the handle may have expired, the policy may have been revoked, and
   * the ledger may have retained this very sequence while the digest
   * was computing.
   */
  private action(
    message: Extract<CallerMessage, { k: 'act' }>,
    bound: OwnerHandle,
    peer: string,
    now: number,
  ): Dispatched {
    // No gameplay-readiness refusal here, and the absence is the
    // honest answer rather than an oversight. §1.6 makes gameplay
    // admissible once a handle's current generation has been emitted
    // in full, and in this owner every accepted emission is built and
    // handed over synchronously, before the reply that admits the
    // handle — so a live handle is always emitted for its current
    // generation. A `not-ready` branch here could not fire, and a
    // check that cannot fire is a claim.
    //
    // Slice F introduces the state that makes it meaningful: a
    // deferred projection (§1.8) allocates a generation whose chunks
    // are not yet out, and an `act` arriving in that window is the
    // first one that must be refused `not-ready`. It belongs in the
    // slice that can produce it.
    const name = message.name;
    if (!Object.prototype.hasOwnProperty.call(this.deps.definition.actions, name)) {
      return this.refuse('act-unknown-name', [this.no(peer, bound.h, 'invalid-data', message.q)]);
    }

    let canonical: string;
    try {
      canonical = canonicalRequest(name, message.in);
    } catch {
      return this.refuse('act-uncanonical', [this.no(peer, bound.h, 'invalid-data', message.q)]);
    }

    if (!needsDigest(canonical)) {
      return this.decide(message, bound, peer, now, inlineBinding(canonical));
    }

    // The large-input path. The synchronous answer is "nothing yet";
    // the rest arrives when the digest does.
    if (this.pending >= MAX_PENDING_ACTIONS) {
      return this.refuse('act-pending-bound', [this.no(peer, bound.h, 'capacity', message.q)]);
    }
    this.pending += 1;
    const deferred = digestBinding(canonical)
      .then(binding => {
        // REVALIDATE. Nothing decided before the await is reused: the
        // handle is re-bound to this peer and incarnation, readiness is
        // re-checked, and the ledger is re-read.
        const at = this.deps.now();
        const rebound = this.bind(message.h, peer, at);
        if (typeof rebound === 'string') {
          return this.refuse(rebound, [this.no(peer, message.h, 'closed', message.q)]);
        }
        return this.decide(message, rebound, peer, at, binding);
      })
      .catch(() =>
        // A digest that fails leaves the owner unable to say whether
        // this request is the retained one, so it refuses rather than
        // guessing either way.
        this.refuse('act-digest-failed', [this.no(peer, message.h, 'indeterminate', message.q)]),
      )
      .finally(() => {
        this.pending -= 1;
      });
    return { out: [], refused: null, deferred };
  }

  /**
   * The ledger's decision, and the execution it may authorize.
   *
   * Synchronous from here on: no `await` may appear below this line,
   * because the transaction it enters is synchronous by construction
   * and a permission checked before a wait can have been revoked
   * during it.
   */
  private decide(
    message: Extract<CallerMessage, { k: 'act' }>,
    bound: OwnerHandle,
    peer: string,
    now: number,
    binding: RequestBinding,
  ): Dispatched {
    const ledger = this.ledgers.ledger(bound.h);
    const s = decimalValue(message.s);
    const disposition = ledger.disposition(s, binding, now);

    switch (disposition.kind) {
      case 'exhausted':
        // The counter does not wrap: a fresh handle is the only way
        // on, and prior outcomes are then unknown.
        return this.refuse('act-sequence-exhausted', [this.no(peer, bound.h, 'capacity', message.q)]);
      case 'conflict':
        // A sequence identifies one request, not a slot. Not executed,
        // and the retained entry is left exactly as it was.
        return this.refuse('act-binding-mismatch', [this.no(peer, bound.h, 'invalid-data', message.q)]);
      case 'expired':
        // Used, and the outcome is gone. This asserts NOTHING about
        // whether it committed.
        return this.refuse('act-result-expired', [this.no(peer, bound.h, 'result-expired', message.q)]);
      case 'replay': {
        // A replay is re-authorized first: the retained outcome is not
        // a permission that keeps working.
        if (!this.permitsAction(peer, message.name, message.in)) {
          return this.refuse('act-forbidden-replay', [this.no(peer, bound.h, 'forbidden', message.q)]);
        }
        this.renew(bound, now);
        return this.accept([this.reply(bound, peer, message.q, s, disposition.outcome)]);
      }
      case 'execute':
        break;
    }

    if (!this.permitsAction(peer, message.name, message.in)) {
      // A denial is RETAINED. Otherwise a later policy change turns
      // this refusal into an execution of the same sequence.
      ledger.retain(s, binding, { kind: 'refusal', code: 'forbidden' }, now);
      this.renew(bound, now);
      return this.refuse('act-forbidden', [this.no(peer, bound.h, 'forbidden', message.q)]);
    }

    const spec = this.deps.definition.actions[message.name as keyof A];
    const before = this.core.getState() as S;
    this.revisionBeforeCommit = this.core.revision;
    let outcome: Outcome;
    try {
      const handler = this.deps.actions[message.name as keyof A];
      // Everything that can REFUSE this action happens inside the
      // transaction — the input parse INCLUDED, because it is
      // application code and a write it makes through the public
      // `host.setState` must join the transaction like the handler's
      // own: discarded on a throw, shipped once at the commit (#5).
      // A parse run beside the transaction published its write and an
      // emission of its own, and the rollback then answered
      // `action-rejected` for a change that had already shipped.
      //
      // The output is validated here for the same reason: a throw
      // inside the transaction discards the staged writes and a throw
      // after it does not. Validating the output afterwards let a
      // handler whose result the definition refuses commit its writes
      // and ship a delta, answering `action-rejected` for a change
      // that had already happened — and the same for a result too
      // large to send, which left the caller with a document it could
      // not be told about.
      const produced = this.core.transact(context => {
        const input = spec.input(message.in);
        const out = spec.output(handler(input, context)) as JsonObject;
        const frame = encodeMessage({
          k: 'res',
          q: message.q,
          h: bound.h,
          s: message.s,
          out,
        });
        if (utf8Length(frame) > this.deps.maxEventBytes) {
          throw new StoreError('capacity', 'the result does not fit the message budget');
        }
        return out;
      }, peer);
      outcome = { kind: 'result', out: produced };
    } catch (error) {
      // A handler that threw rejected the action, and the transaction
      // discarded its writes. So did a result the definition refused
      // or one that does not fit — and a result that cannot be sent
      // is `capacity`, not `action-rejected`: the caller needs to know
      // its request was well-formed and the ANSWER was not.
      outcome = {
        kind: 'refusal',
        code:
          error instanceof StoreError && error.code === 'capacity' ? 'capacity' : 'action-rejected',
      };
    }

    ledger.retain(s, binding, outcome, now);
    this.renew(bound, now);
    // The handler's writes are a change like any other: every
    // installed handle is told, including this caller's — the `res`
    // answers the request, the delta moves the view, and they are two
    // different facts.
    const after = this.core.getState() as S;
    const propagated = Object.is(before, after) ? [] : this.propagate(before, after);
    return this.accept([this.reply(bound, peer, message.q, s, outcome), ...propagated]);
  }

  /** One latest-value input (§1.11). Fire and forget: no reply, ever. */
  private input(
    message: Extract<CallerMessage, { k: 'in' }>,
    bound: OwnerHandle,
    peer: string,
    now: number,
  ): Dispatched {
    const name = message.name;
    if (!Object.prototype.hasOwnProperty.call(this.deps.definition.inputs, name)) {
      return this.refuse('in-unknown-name');
    }
    if (!this.ledgers.inputs(bound.h).admit(name, decimalValue(message.s))) {
      // Stale. Loss is not an error here, and a dropped input may be
      // the LAST one — the store promises no successor.
      return this.refuse('in-stale-sequence');
    }
    if (!this.permitsInput(peer, name, message.in)) return this.refuse('in-forbidden');

    const before = this.core.getState() as S;
    this.revisionBeforeCommit = this.core.revision;
    try {
      // The parse runs INSIDE the transaction, exactly as the action
      // ladder does (#5): it is application code, and a write it makes
      // through the public `host.setState` joins the transaction —
      // discarded with the handler's own writes on a throw, shipped
      // once in the change this path reports at the end.
      this.core.transact(context => {
        const parse = this.deps.definition.inputs[name as keyof I];
        const handler = this.deps.inputs[name as keyof I];
        handler(parse(message.in), context);
      }, peer);
    } catch {
      return this.refuse('in-rejected');
    }
    this.renew(bound, now);
    // An input has no reply of its own, but the change it made is
    // still a change every installed handle is told about.
    const after = this.core.getState() as S;
    return this.accept(Object.is(before, after) ? [] : this.propagate(before, after));
  }

  /** A `res` for a result, a `no` for a retained refusal. */
  private reply(
    bound: OwnerHandle,
    peer: string,
    q: Hex,
    s: bigint,
    outcome: Outcome,
  ): Outbound {
    // A freshly constructed envelope carrying the retained OUTCOME —
    // never a retained reply message, whose `q` belonged to the
    // original request (§1.10).
    const frame =
      outcome.kind === 'result'
        ? encodeMessage({ k: 'res', q, h: bound.h, s: s.toString(), out: outcome.out })
        : encodeMessage({ k: 'no', q, h: bound.h, code: outcome.code, s: s.toString() });
    return { peer, h: bound.h, frame };
  }

  private permitsAction(peer: string, name: string, input: JsonValue): boolean {
    const request = { type: 'action', peer, name, input } as unknown as AccessRequest<A, I>;
    try {
      return this.deps.authorize(request) === true;
    } catch {
      return false;
    }
  }

  private permitsInput(peer: string, name: string, input: JsonValue): boolean {
    const request = { type: 'input', peer, name, input } as unknown as AccessRequest<A, I>;
    try {
      return this.deps.authorize(request) === true;
    } catch {
      return false;
    }
  }

  /** The lease is renewed by an accepted message, refusals excepted. */
  private renew(bound: OwnerHandle, now: number): void {
    const live = this.handles.get(bound.h);
    if (live === undefined) return;
    this.handles.set(bound.h, { ...live, lastSeen: now });
  }

  /**
   * Everything that belongs to one handle, gone together.
   *
   * §2: the ledger goes with the handle. That is what makes a replay
   * after eviction `closed` rather than `result-expired` — the owner
   * no longer knows the sequence existed — and it is what keeps the
   * ledger table bounded by live handles instead of by the owner's
   * lifetime.
   */
  private forget(h: Hex): void {
    this.handles.delete(h);
    this.ledgers.forget(h);
    // §1.8: expiry retires any projection already pending, and a dead
    // handle never acquires one.
    this.deferred.delete(h);
  }

  /**
   * Emit the projections that were deferred, now that one can be
   * taken.
   *
   * Completion **revalidates**: a handle can die between the deferral
   * and the availability, and an expired one never acquires a pending
   * projection in the first place (§1.8).
   */
  resumeDeferred(now: number = this.deps.now()): Dispatched {
    if (!this.deps.canProject()) return this.accept([]);
    const out: Outbound[] = [];
    for (const [h, pending] of [...this.deferred]) {
      this.deferred.delete(h);
      const bound = this.bind(h, pending.peer, now);
      if (typeof bound === 'string') {
        // The handle went while the projection was unavailable. The
        // caller's own deadline covers this; nothing is emitted to a
        // handle that no longer exists.
        this.counters[`deferred-${bound}`] = (this.counters[`deferred-${bound}`] ?? 0) + 1;
        continue;
      }
      const frames = this.install(bound, pending.q);
      if (frames === null) {
        this.forget(h);
        out.push(this.no(pending.peer, h, 'capacity', pending.q));
        continue;
      }
      out.push(...frames);
    }
    return this.accept(out);
  }

  /** Projections waiting on availability, for a host's bounds report. */
  get deferredCount(): number {
    return this.deferred.size;
  }

  /** Ledgers held, for a host's own bounds reporting (§2). */
  get ledgerCount(): number {
    return this.ledgers.size;
  }

  /** An accepted frame's reply. */
  private accept(out: readonly Outbound[], deferred: Promise<Dispatched> | null = null): Dispatched {
    return { out, refused: null, deferred };
  }

  /**
   * Expire handles whose lease has run out (§2).
   *
   * Reports the PEER with the handle, because §1.6's expiry notice is
   * addressed to a caller and the handle is gone by the time anyone
   * could look it up. A host that cannot reach that peer sends
   * nothing; no tombstone is retained to announce later.
   */
  sweep(now: number): readonly { readonly h: Hex; readonly peer: string }[] {
    const expired: { h: Hex; peer: string }[] = [];
    for (const [h, handle] of [...this.handles]) {
      if (now - handle.lastSeen >= HANDLE_LEASE_MS) {
        this.forget(h);
        expired.push({ h, peer: handle.peer });
      }
    }
    return expired;
  }

  /**
   * Say goodbye, once, to every replica this owner is serving.
   *
   * §1.6's doctrine is that a caller learns why rather than inferring
   * it from silence, and it was applied to EXPIRY (`sweep`) and not
   * to closure: a host that closed took its answer with it, so every
   * bound replica's next write reached a store that no longer
   * existed and the caller's only signal was a deadline — the
   * `indeterminate` "the store did not answer before the deadline",
   * measured over the real transport.
   *
   * The code is `owner-lost`, not `closed`, and the difference is
   * the whole semantics: `closed` is the expiry notice and the
   * replica REJOINS on it, which after a close would either hang or
   * silently attach the caller to a SUCCESSOR's different document
   * under the same handle. `owner-lost` is terminal — this
   * incarnation held the document and is gone — so the replica's
   * view is cleared, `ready()` rejects, and rejoining is the
   * caller's decision to make explicitly.
   *
   * Every handle is forgotten here, so the frames are a one-shot:
   * a second call has nothing to say.
   */
  farewell(): readonly Outbound[] {
    const out: Outbound[] = [];
    for (const [h, handle] of [...this.handles]) {
      this.forget(h);
      out.push(this.no(handle.peer, h, 'owner-lost', null));
    }
    return out;
  }

  /** This owner's incarnation, which every manifest carries. */
  get incarnationHex(): Hex {
    return this.incarnation;
  }

  /**
   * Admit a join.
   *
   * Order matters and is the ladder's: **the store address first**,
   * because it decides whether this owner may answer at all; then
   * definition identity and version, which are loud for the store
   * they ARE addressed to; then capacity, `authorize`, and only then
   * a handle. Nothing is allocated before the policy has ruled, so a
   * refused caller costs one counter and no state.
   *
   * This summary is written out in full because it was wrong for one
   * commit — it still listed the definition first, ten lines above
   * the code that had just been reordered, in the rung whose
   * position was the point of the change.
   */
  private join(
    message: Extract<CallerMessage, { k: 'join' }>,
    peer: string,
    now: number,
  ): Dispatched {
    // THE ADDRESS DECIDES WHETHER THIS OWNER MAY SPEAK AT ALL, so it
    // is read before anything else about the frame.
    //
    // Refused SILENTLY — no `no`, no reply: every host store on a
    // node sees every join, and an owner that is not the addressee
    // must neither answer it nor refuse it on the addressee's
    // behalf. The one it IS addressed to answers.
    //
    // The order matters and used to be wrong. The definition and
    // version checks below answer LOUDLY, with the joiner's own `q`,
    // so a node hosting `app.chat` and `app.world` had the chat
    // store reject a perfectly good `app.world` join —
    // `version-mismatch`, status failed — before the world store's
    // manifest could land. Same defect as the one this address
    // exists to fix, two rungs higher, and found by review probe K.
    if (message.store !== this.deps.store) {
      return this.refuse('join-other-store', []);
    }
    // Addressed to THIS store, and wrong about it: that is worth
    // saying out loud, and it is how a stale client learns it is
    // stale.
    if (message.def !== this.deps.definition.id) {
      return this.refuse('join-wrong-definition', [this.no(peer, null, 'version-mismatch', message.q)]);
    }
    if (message.ver !== this.deps.definition.version) {
      return this.refuse('join-wrong-version', [this.no(peer, null, 'version-mismatch', message.q)]);
    }
    if (this.handles.size >= MAX_HANDLES) {
      return this.refuse('join-capacity', [this.no(peer, null, 'capacity', message.q)]);
    }

    // `authorize` receives the AUTHENTICATED peer and the audience it
    // asked to read. A refusal is `forbidden` and allocates nothing.
    if (!this.permitsRead(peer, message.aud)) {
      return this.refuse('join-forbidden', [this.no(peer, null, 'forbidden', message.q)]);
    }

    const h = this.newHandle();
    if (h === null || h.length !== HANDLE_HEX_LENGTH || this.handles.has(h)) {
      return this.refuse('join-handle-unusable', [this.no(peer, null, 'owner-lost', message.q)]);
    }

    const handle: OwnerHandle = {
      h,
      inc: this.incarnation,
      peer,
      audience: [...message.aud],
      generation: 0,
      revision: this.core.revision,
      lastSeen: now,
    };
    this.handles.set(h, handle);

    const emitted = this.install(handle, message.q);
    if (emitted === null) {
      // The projection could not be encoded. The handle goes with it:
      // admitting one that can never be served is worse than refusing.
      this.forget(h);
      return this.refuse('join-projection-capacity', [this.no(peer, null, 'capacity', message.q)]);
    }
    return this.accept(emitted);
  }

  /**
   * Allocate the next generation and emit `man` + every `snap`.
   *
   * Returns `null` when the projection cannot be carried, so the caller
   * decides what to do with the handle rather than this function
   * guessing.
   */
  private install(handle: OwnerHandle, q: Hex | null): Outbound[] | null {
    const projected = this.project(handle.audience);
    // `null` means the application could produce neither a projection
    // nor its own empty value. There is nothing truthful to ship, so
    // the emission fails and the caller refuses.
    if (projected === null) return null;
    let chunked;
    try {
      chunked = chunkSnapshot(projected, this.chunkBytes);
    } catch {
      return null;
    }

    // Owner-allocated, monotone per handle, only on acceptance (§1.7a).
    const generation = handle.generation + 1;
    this.handles.set(handle.h, { ...handle, generation, revision: this.core.revision });

    const g = String(generation);
    const r = String(this.core.revision);
    const frames: Outbound[] = [
      {
        peer: handle.peer,
        h: handle.h,
        frame: encodeMessage(
          q === null
            ? { k: 'man', h: handle.h, inc: handle.inc, g, r, n: chunked.n, bytes: String(chunked.bytes) }
            : { k: 'man', q, h: handle.h, inc: handle.inc, g, r, n: chunked.n, bytes: String(chunked.bytes) },
        ),
      },
    ];
    for (let i = 0; i < chunked.n; i += 1) {
      frames.push({
        peer: handle.peer,
        h: handle.h,
        frame: encodeMessage({ k: 'snap', h: handle.h, g, r, i, n: chunked.n, d: chunked.pieces[i] as string }),
      });
    }
    return frames;
  }

  /**
   * One read decision, for the join and for every later read.
   *
   * A throwing policy is a refusal, never an admission.
   */
  private permitsRead(peer: string, audience: readonly string[]): boolean {
    const request = { type: 'read', peer, audience: [...audience] } as AccessRequest<A, I>;
    try {
      return this.deps.authorize(request) === true;
    } catch {
      return false;
    }
  }

  /** A handle from the application's allocator, or null if it threw. */
  private newHandle(): Hex | null {
    try {
      return this.deps.newHandle();
    } catch {
      return null;
    }
  }

  /**
   * The audience projection, validated by the definition.
   *
   * A projection that does not satisfy `state()` is the owner's bug and
   * must not be shipped as a snapshot, so it becomes `empty()` — the
   * value that represents absence — rather than the full state.
   */
  private project(audience: readonly string[], state: S = this.core.getState() as S): S | null {
    try {
      return this.deps.definition.state(this.deps.project(state, audience));
    } catch {
      // `empty()` is application code as well, and it is reached
      // precisely when the application has already thrown once. A
      // second failure has no value left to fall back to, so it
      // becomes a refusal rather than an exception escaping the frame
      // handler — which would take down the transport loop for every
      // other handle.
      try {
        return this.deps.definition.empty();
      } catch {
        return null;
      }
    }
  }

  /**
   * Re-check the binding for every message (§1.3).
   *
   * Returns a countable reason instead of a handle when the handle is
   * unknown, expired, or bound to a different authenticated peer — and
   * the three are **one** refusal on the wire, so a refusal cannot
   * disclose that a handle exists (§2).
   */
  private bind(h: Hex, peer: string, now: number): OwnerHandle | string {
    const handle = this.handles.get(h);
    if (handle === undefined) return 'handle-unknown';
    if (now - handle.lastSeen >= HANDLE_LEASE_MS) {
      this.forget(h);
      return 'handle-expired';
    }
    if (handle.peer !== peer) return 'handle-foreign-peer';
    // No incarnation check here: a handle is created with this owner's
    // incarnation and does not outlive the owner, so a stale one cannot
    // reach this table. The incarnation rides the manifest as
    // provenance for the replica, which is a different job. A check
    // that cannot fire is not a defence — it is a claim.
    return handle;
  }

  private no(peer: string, h: Hex | null, code: StoreErrorCode, q: Hex | null): Outbound {
    return {
      peer,
      h,
      frame: encodeMessage(
        q === null
          ? { k: 'no', h: h as Hex, code }
          : h === null
            ? { k: 'no', q, code }
            : { k: 'no', q, h, code },
      ),
    };
  }

  private refuse(reason: string, out: readonly Outbound[] = []): Dispatched {
    this.counters[reason] = (this.counters[reason] ?? 0) + 1;
    return { out, refused: reason, deferred: null };
  }
}

/**
 * The root-level difference between two projections.
 *
 * Root level only, deliberately: §1.13's patch semantics replace a
 * whole subtree at a path, and a deeper diff would buy smaller frames
 * at the cost of a second traversal per handle per commit — the thing
 * a 60 Hz loop cannot afford. A subtree that did not change is not
 * emitted at all, which is where the saving actually is.
 */
function shallowDiff(before: JsonObject, after: JsonObject): WireOp[] {
  const ops: WireOp[] = [];
  for (const key of Object.keys(after)) {
    const nextValue = after[key];
    if (Object.prototype.hasOwnProperty.call(before, key) && Object.is(before[key], nextValue)) continue;
    // Not `Object.is` alone: reconciliation shares unchanged subtrees,
    // so identity IS the comparison for anything it touched, and a
    // value it could not share is compared by its serialization.
    if (
      Object.prototype.hasOwnProperty.call(before, key) &&
      JSON.stringify(before[key]) === JSON.stringify(nextValue)
    ) {
      continue;
    }
    ops.push({ o: 'r', p: [key], val: nextValue as JsonValue });
  }
  for (const key of Object.keys(before)) {
    if (!Object.prototype.hasOwnProperty.call(after, key)) ops.push({ o: 'x', p: [key] });
  }
  return ops;
}

/** The delta frame, or null when it will not fit the budget (§1.9). */
function encodeDeltaWithin(
  message: Parameters<typeof encodeMessage>[0],
  maxEventBytes: number,
): string | null {
  let frame: string;
  try {
    frame = encodeMessage(message);
  } catch {
    return null;
  }
  return utf8Length(frame) <= maxEventBytes ? frame : null;
}

/** The `q` a refusal for this message should carry, if any. */
function requestOf(message: CallerMessage): Hex | null {
  return 'q' in message && typeof message.q === 'string' ? message.q : null;
}

/** Re-exported so a host can hold assemblies without another import. */
export { Assembly, AssemblyTable };

export type { Parse };
