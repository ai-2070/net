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
import { StoreError, type StoreErrorCode } from './errors.js';
import { reconcile } from './state.js';
import type { AccessRequest, ActionSpec, InputSpec, Parse, StoreDefinition } from './types.js';
import {
  decodeMessage,
  encodeMessage,
  HANDLE_HEX_LENGTH,
  INCARNATION_HEX_LENGTH,
  MAX_AUDIENCE_LABELS,
  type CallerMessage,
  type Hex,
} from './wire.js';

/** Handles one owner will hold (brief §2). */
export const MAX_HANDLES = 256;

/** How long a handle lives without an accepted authenticated message. */
export const HANDLE_LEASE_MS = 60_000;

/** What the owner needs from its host. */
export interface OwnerDeps<S extends object, A extends ActionSpec, I extends InputSpec> {
  readonly definition: StoreDefinition<S, A, I>;
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
}

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
}

interface Counters {
  [reason: string]: number;
}

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
  private state: S;
  private revision = 0;
  /** Assemblies are the replica's concern; the owner keeps the type. */
  readonly assemblies = new AssemblyTable();

  constructor(private readonly deps: OwnerDeps<S, A, I>) {
    // Fail fast rather than discover the numbers on the first
    // projection (§1.9).
    this.chunkBytes = assertChunkingFits(deps.maxEventBytes);
    this.incarnation = deps.newIncarnation();
    if (this.incarnation.length !== INCARNATION_HEX_LENGTH) {
      throw new StoreError('invalid-data', 'newIncarnation must return 16 lowercase hex');
    }
    this.state = deps.definition.empty();
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

  /** Replace the authoritative state, as `setState` does. */
  commit(next: S): void {
    const reconciled = reconcile(this.state, this.deps.definition.state(next));
    if (reconciled === this.state) return;
    this.state = reconciled;
    this.revision += 1;
  }

  /** The current authoritative revision. */
  get currentRevision(): number {
    return this.revision;
  }

  /**
   * Dispatch one inbound frame.
   *
   * `peer` is the **authenticated** originating caller — see the module
   * note. It is not read from `frame`, and no frame can carry one.
   */
  receive(frame: string, peer: string, now: number = this.deps.now()): Dispatched {
    const decoded = decodeMessage(frame, { maxBytes: this.deps.maxEventBytes, as: 'owner' });
    if (!decoded.ok) return this.refuse(decoded.reason, [], null, null);

    const message = decoded.message;
    if (message.k === 'join') return this.join(message, peer, now);

    // Every other kind names a handle. Binding is re-checked on every
    // message, not only at admission (§1.3): a session replacement does
    // not re-authorize a handle.
    const bound = this.bind(message.h, peer, now);
    if (typeof bound === 'string') {
      return this.refuse(bound, [this.no(peer, message.h, 'closed', requestOf(message))], null, null);
    }

    switch (message.k) {
      case 'alive': {
        this.handles.set(bound.h, { ...bound, lastSeen: now });
        return { out: [{ peer, h: bound.h, frame: encodeMessage({ k: 'ok', q: message.q, h: bound.h }) }], refused: null };
      }
      case 'leave': {
        this.handles.delete(bound.h);
        this.assemblies.reclaimHandle(bound.h);
        return { out: [{ peer, h: bound.h, frame: encodeMessage({ k: 'ok', q: message.q, h: bound.h }) }], refused: null };
      }
      case 'act':
      case 'in':
        // Gameplay before an installed view is `not-ready` (§1.7b), and
        // the ledger that would answer it is slice E. Refused rather
        // than half-answered.
        return this.refuse(
          `unimplemented-kind:${message.k}`,
          message.k === 'act' ? [this.no(peer, bound.h, 'not-ready', message.q)] : [],
          null,
          null,
        );
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
            [this.no(peer, bound.h, 'forbidden', message.q)],
            null,
            null,
          );
        }
        const renewed = { ...bound, lastSeen: now };
        this.handles.set(bound.h, renewed);
        const frames = this.install(renewed, message.q);
        if (frames === null) {
          this.handles.delete(bound.h);
          this.assemblies.reclaimHandle(bound.h);
          return this.refuse(
            'resync-projection-capacity',
            [this.no(peer, bound.h, 'capacity', message.q)],
            null,
            null,
          );
        }
        return { out: frames, refused: null };
      }
      case 'aud':
      case 'resume':
        // Audience transitions and reconnection are slice F. A control
        // is never answered `not-ready` (§1.7b), so this is the honest
        // refusal for "this owner does not implement it yet".
        return this.refuse(
          `unimplemented-kind:${message.k}`,
          [this.no(peer, bound.h, 'invalid-data', message.q)],
          null,
          null,
        );
      default:
        return this.refuse('unreachable-kind', [], null, null);
    }
  }

  /** Expire handles whose lease has run out (§2). */
  sweep(now: number): Hex[] {
    const expired: Hex[] = [];
    for (const [h, handle] of [...this.handles]) {
      if (now - handle.lastSeen >= HANDLE_LEASE_MS) {
        this.handles.delete(h);
        this.assemblies.reclaimHandle(h);
        expired.push(h);
      }
    }
    return expired;
  }

  /**
   * Admit a join.
   *
   * Order matters and is the ladder's: definition identity, audience
   * bounds, capacity, `authorize`, and only then a handle. Nothing is
   * allocated before the policy has ruled, so a refused caller costs
   * one counter and no state.
   */
  private join(
    message: Extract<CallerMessage, { k: 'join' }>,
    peer: string,
    now: number,
  ): Dispatched {
    if (message.def !== this.deps.definition.id) {
      return this.refuse('join-wrong-definition', [this.no(peer, null, 'version-mismatch', message.q)], null, null);
    }
    if (message.ver !== this.deps.definition.version) {
      return this.refuse('join-wrong-version', [this.no(peer, null, 'version-mismatch', message.q)], null, null);
    }
    if (message.aud.length > MAX_AUDIENCE_LABELS) {
      return this.refuse('join-audience-bound', [this.no(peer, null, 'capacity', message.q)], null, null);
    }
    if (this.handles.size >= MAX_HANDLES) {
      return this.refuse('join-capacity', [this.no(peer, null, 'capacity', message.q)], null, null);
    }

    // `authorize` receives the AUTHENTICATED peer and the audience it
    // asked to read. A refusal is `forbidden` and allocates nothing.
    if (!this.permitsRead(peer, message.aud)) {
      return this.refuse('join-forbidden', [this.no(peer, null, 'forbidden', message.q)], null, null);
    }

    const h = this.newHandle();
    if (h === null || h.length !== HANDLE_HEX_LENGTH || this.handles.has(h)) {
      return this.refuse('join-handle-unusable', [this.no(peer, null, 'owner-lost', message.q)], null, null);
    }

    const handle: OwnerHandle = {
      h,
      inc: this.incarnation,
      peer,
      audience: [...message.aud],
      generation: 0,
      revision: this.revision,
      lastSeen: now,
    };
    this.handles.set(h, handle);

    const emitted = this.install(handle, message.q);
    if (emitted === null) {
      // The projection could not be encoded. The handle goes with it:
      // admitting one that can never be served is worse than refusing.
      this.handles.delete(h);
      return this.refuse('join-projection-capacity', [this.no(peer, null, 'capacity', message.q)], null, null);
    }
    return { out: emitted, refused: null };
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
    this.handles.set(handle.h, { ...handle, generation, revision: this.revision });

    const g = String(generation);
    const r = String(this.revision);
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
  private project(audience: readonly string[]): S | null {
    try {
      return this.deps.definition.state(this.deps.project(this.state, audience));
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
      this.handles.delete(h);
      this.assemblies.reclaimHandle(h);
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

  private refuse(reason: string, out: Outbound[], _a: null, _b: null): Dispatched {
    this.counters[reason] = (this.counters[reason] ?? 0) + 1;
    return { out, refused: reason };
  }
}

/** The `q` a refusal for this message should carry, if any. */
function requestOf(message: CallerMessage): Hex | null {
  return 'q' in message && typeof message.q === 'string' ? message.q : null;
}

/** Re-exported so a host can hold assemblies without another import. */
export { Assembly, AssemblyTable };
export type { Parse };
