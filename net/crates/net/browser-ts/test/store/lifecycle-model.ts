/**
 * A local, executable model of the store's installation lifecycle.
 *
 * **Test-only and deliberately so.** Nothing here is exported from the
 * package, nothing touches a transport, and no protocol subsystem is
 * introduced: this is the brief's normative tables
 * (`spikes/S7_STORE_BRIEF_DRAFT.md` §1.7a/§1.7b/§1.8) turned into code
 * so the tables can be *executed* instead of read.
 *
 * Three reducers' worth of state and one scheduler:
 *
 * - {@link ownerStep} — handle issuance, allocation, emission, and
 *   retirement of unsent work.
 * - {@link replicaStep} — the five states, the four generation values,
 *   the desired-transition slot, and the learned handle.
 * - the harness in `lifecycle.test.ts` — delivers messages in
 *   per-direction FIFO order, stalls the owner, and breaks sessions.
 *
 * ## What the model does and does not represent
 *
 * - **Handle admission is modelled**, because it is lifecycle
 *   semantics, not a byte-level detail: only `join` creates a handle,
 *   every other request carries one, and an unknown or expired handle
 *   is refused rather than silently honoured.
 * - **Envelopes are objects, not JSON.** The codec has its own
 *   witnesses (§5.1); mixing the two would make a lifecycle failure
 *   look like a parse failure.
 * - **Transport assumption: one reliable, ordered stream per handle**,
 *   all kinds interleaved in emission order (§1.9a). The harness
 *   therefore cannot reorder owner→replica messages, and loss is
 *   modelled as a *session break* or an *owner stall*, never as
 *   selective per-message loss. The model's coverage is loss and
 *   delay, **not** cross-kind reordering.
 * - That assumption is nevertheless not load-bearing for safety: a
 *   delta naming the generation currently being assembled — which
 *   ordering makes unreachable — sets the `behind` flag rather than
 *   being dropped, so a violated assumption cannot leave the replica
 *   silently behind the owner. See `delta-ahead-of-assembly`.
 */

/** Replica lifecycle states (§1.7a). */
export type ReplicaState = 'joining' | 'installing' | 'ready' | 'fenced' | 'closed';

/** One in-flight snapshot assembly. */
export interface Assembly {
  readonly g: number;
  readonly r: number;
  readonly n: number;
  /** The `q` that opened it, or `null` when it was an owner refresh. */
  readonly q: string | null;
  readonly have: Set<number>;
}

/** What the owner emits and the replica consumes. */
export type Message =
  | { k: 'man'; h: string; g: number; r: number; n: number; q: string | null }
  | { k: 'snap'; h: string; g: number; r: number; n: number; i: number }
  | { k: 'delta'; h: string; g: number; base: number; r: number }
  | { k: 'no'; h: string; q: string | null; code: string };

/**
 * What the replica sends. `join` is the only request that does not
 * carry a handle, because it is the only one that creates one.
 *
 * `resume` carries the caller's latest desired audience (§2's schema,
 * authorized exactly like `aud`); `resync` does not, because it is a
 * position recovery within the audience already bound to the handle.
 */
export type Request =
  | { k: 'join'; q: string; aud: readonly string[] }
  | { k: 'aud'; h: string; q: string; aud: readonly string[] }
  | { k: 'resume'; h: string; q: string; aud: readonly string[] }
  | { k: 'resync'; h: string; q: string; g: number | null; have: number | null };

export interface Replica {
  readonly state: ReplicaState;
  /** The handle the owner issued, once one has been **learned**. */
  readonly handle: string | null;
  /** The generation whose snapshot is published, or null. */
  readonly installed: number | null;
  /** The published revision, or null. */
  readonly revision: number | null;
  /** The open assembly, or null. */
  readonly assembling: Assembly | null;
  /** Highest generation ever ADMITTED, installed or not (§1.7a). */
  readonly retired: number;
  /** The desired-transition slot: one `q` per shared transition. */
  readonly slot: string | null;
  /** Local waiters on the slot's transition, independently cancellable. */
  readonly waiters: number;
  /** The caller's latest desired audience — survives reconnect. */
  readonly desired: readonly string[];
  /**
   * Highest owner generation this replica was told about and could not
   * take because it was mid-transition. Bounded: one number, coalesced.
   */
  readonly skipped: number | null;
  /**
   * The owner is known to have moved past the revision being assembled.
   * Bounded: one flag, coalesced into the same recovery as `skipped`.
   */
  readonly behind: boolean;
  /** Retained-but-stale view (reconnect) vs cleared (audience change). */
  readonly stale: boolean;
  /** Whether a view is currently published to the application. */
  readonly published: boolean;
  /** Requests emitted by the last step, in order. */
  readonly out: readonly Request[];
  /** Dropped/ignored arrivals, by reason — the counters §1.7a requires. */
  readonly dropped: Readonly<Record<string, number>>;
}

export function newReplica(desired: readonly string[]): Replica {
  return {
    state: 'joining',
    handle: null,
    installed: null,
    revision: null,
    assembling: null,
    retired: 0,
    slot: null,
    waiters: 0,
    desired,
    skipped: null,
    behind: false,
    stale: false,
    published: false,
    out: [],
    dropped: {},
  };
}

export type ReplicaEvent =
  | { t: 'join'; q: string }
  | { t: 'setAudience'; q: string; aud: readonly string[] }
  | { t: 'cancel' }
  | { t: 'reconnect'; q: string }
  | { t: 'assemblyDeadline' }
  | { t: 'recv'; m: Message };

function drop(r: Replica, reason: string): Replica {
  return {
    ...r,
    out: [],
    dropped: { ...r.dropped, [reason]: (r.dropped[reason] ?? 0) + 1 },
  };
}

function sameAudience(a: readonly string[], b: readonly string[]): boolean {
  return a.length === b.length && a.every((v, i) => v === b[i]);
}

/** Open a transition, taking the slot and clearing the view. */
function beginAudience(r: Replica, q: string, aud: readonly string[]): Replica {
  const h = r.handle;
  return {
    ...r,
    state: 'installing',
    slot: q,
    waiters: 1,
    desired: aud,
    assembling: null, // replacement retires the open assembly
    published: false, // an audience change CLEARS
    stale: false,
    out: h === null ? [{ k: 'join', q, aud }] : [{ k: 'aud', h, q, aud }],
  };
}

/** One resync for the position this replica actually has (§1.8). */
function recover(r: Replica, q: string): Replica {
  const h = r.handle;
  if (h === null) {
    // No handle was ever learned: the only legal recovery is a join.
    return { ...r, state: 'joining', slot: q, waiters: Math.max(r.waiters, 1), out: [
      { k: 'join', q, aud: r.desired },
    ] };
  }
  return {
    ...r,
    state: 'installing',
    slot: q,
    waiters: Math.max(r.waiters, 1),
    out: [{ k: 'resync', h, q, g: r.installed, have: r.revision }],
  };
}

/**
 * One replica transition.
 *
 * Every admission test reads off `state`, `handle`, `slot`,
 * `installed`, `assembling` and `retired` — the same values the
 * brief's table names, so a divergence between this function and that
 * table is a documentation bug that shows up as a failing witness.
 */
export function replicaStep(r: Replica, e: ReplicaEvent): Replica {
  if (r.state === 'closed') return drop(r, 'closed');

  switch (e.t) {
    case 'join':
      // `joining` holds the pending join's `q` in the slot, which is
      // what makes the INITIAL manifest admissible.
      return { ...r, state: 'joining', handle: null, slot: e.q, waiters: 1, out: [
        { k: 'join', q: e.q, aud: r.desired },
      ] };

    case 'setAudience': {
      // A second request for the audience already in flight joins the
      // shared transition as another local waiter: one wire request and
      // one `q` per transition.
      if (
        (r.state === 'installing' || r.state === 'joining') &&
        r.slot !== null &&
        sameAudience(r.desired, e.aud)
      ) {
        return { ...r, waiters: r.waiters + 1, out: [] };
      }
      return beginAudience(r, e.q, e.aud);
    }

    case 'cancel': {
      if (r.state !== 'installing' && r.state !== 'joining') return drop(r, 'cancel-idle');
      const waiters = r.waiters - 1;
      if (waiters > 0) return { ...r, waiters, out: [] };
      // Last waiter gone: fenced, assembly retired, watermark kept.
      return {
        ...r,
        state: 'fenced',
        slot: null,
        waiters: 0,
        assembling: null,
        published: false,
        out: [],
      };
    }

    case 'reconnect': {
      // From EVERY live state, not only `ready`. A reconnect keeps the
      // last snapshot but marks it stale, and carries the caller's
      // LATEST desired audience so the owner cannot resume an older
      // one. With no handle learned yet there is nothing to resume:
      // only `join` creates a handle.
      if (r.state === 'fenced') return drop(r, 'reconnect-while-fenced');
      const h = r.handle;
      if (h === null) {
        return {
          ...r,
          state: 'joining',
          slot: e.q,
          waiters: Math.max(r.waiters, 1),
          assembling: null,
          stale: r.published,
          out: [{ k: 'join', q: e.q, aud: r.desired }],
        };
      }
      return {
        ...r,
        state: 'installing',
        slot: e.q,
        waiters: Math.max(r.waiters, 1),
        assembling: null,
        stale: r.published,
        out: [{ k: 'resume', h, q: e.q, aud: r.desired }],
      };
    }

    case 'assemblyDeadline': {
      if (r.assembling === null) return drop(r, 'deadline-no-assembly');
      return recover({ ...r, assembling: null }, `rs-${r.retired}`);
    }

    case 'recv':
      return receive(r, e.m);
  }
}

function receive(r: Replica, m: Message): Replica {
  // Once a handle is learned, traffic for any other handle is not ours.
  if (r.handle !== null && m.h !== r.handle) return drop(r, 'foreign-handle');

  switch (m.k) {
    case 'man': {
      if (m.g <= r.retired) return drop(r, 'man-retired');
      if (m.q === null) {
        // An owner refresh. Admissible ONLY in `ready`: an empty slot
        // is not consent, and `fenced` must not be reopened.
        if (r.state !== 'ready') {
          // Remember that a newer owner installation was skipped, so
          // convergence stays recoverable. Coalesced to one number, and
          // NOT recorded while fenced — a fenced client is waiting for
          // its own caller, not for the owner.
          const skipped = r.state === 'fenced' ? r.skipped : Math.max(r.skipped ?? 0, m.g);
          return { ...drop(r, `man-unsolicited-in-${r.state}`), skipped };
        }
      } else if (m.q !== r.slot || (r.state !== 'installing' && r.state !== 'joining')) {
        return drop(r, 'man-wrong-slot');
      }
      return {
        ...r,
        state: 'installing',
        // The initial manifest is where the issued handle is LEARNED.
        handle: m.h,
        retired: m.g,
        assembling: { g: m.g, r: m.r, n: m.n, q: m.q, have: new Set() },
        out: [],
      };
    }

    case 'snap': {
      const a = r.assembling;
      if (a === null || m.g !== a.g || m.r !== a.r || m.n !== a.n) {
        return drop(r, 'snap-no-assembly');
      }
      if (a.have.has(m.i)) return drop(r, 'snap-duplicate');
      const have = new Set(a.have).add(m.i);
      if (have.size < a.n) {
        return { ...r, assembling: { ...a, have }, out: [] };
      }
      // Complete. Publication is fenced against the EXACT active
      // installation: an assembly whose transition was superseded must
      // not publish, even though its chunks all arrived.
      if (a.q !== null && a.q !== r.slot) return drop(r, 'publish-superseded');
      if (a.q === null && r.state !== 'installing') return drop(r, 'publish-not-installing');
      const settled: Replica = {
        ...r,
        state: 'ready',
        installed: a.g,
        revision: a.r,
        assembling: null,
        slot: null,
        waiters: 0,
        published: true,
        stale: false,
        out: [],
      };
      // Having reached `ready`, recover anything the transition made us
      // miss: a skipped owner refresh, or an owner that moved past the
      // revision we were assembling. Both coalesce into ONE resync.
      const missedRefresh = settled.skipped !== null && settled.skipped > (settled.installed ?? 0);
      if (missedRefresh || settled.behind) {
        const q = `rs-recover-${settled.installed}`;
        return recover({ ...settled, skipped: null, behind: false }, q);
      }
      // Subsumed: the installation we just took is at least as new as
      // the refresh we skipped, so the knowledge has served its purpose
      // and must not linger to provoke a later spurious recovery.
      return { ...settled, skipped: null };
    }

    case 'delta': {
      // State-gated, not only generation-gated: a cleared or fenced
      // replica must not be repopulated by an old delta.
      if (r.state !== 'ready') {
        // One ordered stream per handle (§1.9a) makes a delta for the
        // generation being assembled unreachable — it would have to
        // overtake that generation's own chunks. If the assumption is
        // ever violated, this must not become a silent gap: record that
        // the owner is ahead so completion triggers recovery.
        if (r.assembling !== null && m.g === r.assembling.g) {
          return { ...drop(r, 'delta-ahead-of-assembly'), behind: true };
        }
        return drop(r, `delta-in-${r.state}`);
      }
      if (m.g !== r.installed) return drop(r, 'delta-wrong-generation');
      if (m.base !== r.revision) return recover(r, `rs-gap-${m.r}`);
      return { ...r, revision: m.r, out: [] };
    }

    case 'no': {
      if (m.q !== null && m.q !== r.slot) return drop(r, 'no-wrong-slot');
      if (m.code === 'closed') return { ...r, state: 'closed', published: false, out: [] };
      if (m.code === 'unknown-handle') {
        // The handle is gone. Only `join` can create one, so automatic
        // recovery rejoins rather than resuming something the owner has
        // never heard of.
        //
        // Generations are monotone **per handle** (§1.7a), so every
        // generation value is scoped to the handle that issued it and
        // MUST be discarded with it. Carrying `retired` across would
        // make the new handle's first installation — generation 1 —
        // fail `g > retired` and strand the rejoin.
        const q = `j-re-${m.q ?? 'x'}`;
        return {
          ...r,
          handle: null,
          state: 'joining',
          slot: q,
          waiters: Math.max(r.waiters, 1),
          installed: null,
          revision: null,
          assembling: null,
          retired: 0,
          skipped: null,
          behind: false,
          published: false,
          stale: false,
          out: [{ k: 'join', q, aud: r.desired }],
        };
      }
      return {
        ...r,
        state: 'fenced',
        slot: null,
        waiters: 0,
        assembling: null,
        published: false,
        out: [],
      };
    }
  }
}

// ───────────────────────────── the owner ─────────────────────────────

/**
 * Per-handle installation state.
 *
 * **Everything about an installation is scoped to its handle.** The
 * round-4 model kept `g`, `aud`, `unsent` and `deferred` owner-global,
 * so a second join retired the first handle's unsent chunks and
 * redirected the remainder — two replicas sharing one installation
 * slot. Only the store revision is genuinely global, because it is a
 * property of the document rather than of a subscription.
 */
export interface OwnerHandle {
  readonly live: boolean;
  /** The audience bound to this handle — what a `resync` recovers. */
  readonly aud: readonly string[];
  /** Monotone generation allocator, **per handle** (§1.7a). */
  readonly allocated: number;
  /** The current installation generation, or 0 before the first. */
  readonly g: number;
  /** The revision the current snapshot was taken at. */
  readonly at: number;
  /** The highest revision emitted to this handle. */
  readonly sent: number;
  /** Chunks of the current installation not yet handed over. */
  readonly unsent: number;
  /** A projection waiting for availability (§1.8). */
  readonly deferred: { readonly q: string | null; readonly aud: readonly string[] } | null;
}

export interface Owner {
  /** The store revision: global, because the document is. */
  readonly r: number;
  /** Handles this owner has issued, live or expired. */
  readonly handles: Readonly<Record<string, OwnerHandle>>;
  /** How many handles have ever been issued — never reused. */
  readonly issued: number;
  /** Whether a projection can be taken right now. */
  readonly canProject: boolean;
  readonly out: readonly Message[];
  /** Chunks retired without being emitted. */
  readonly retiredUnsent: number;
  /** Deferred projections discarded because their handle died first. */
  readonly deferredRetired: number;
  /** Chunks each snapshot is split into, for this model. */
  readonly chunkCount: number;
}

export function newOwner(chunks = 2): Owner {
  return {
    r: 100,
    handles: {},
    issued: 0,
    canProject: true,
    out: [],
    retiredUnsent: 0,
    deferredRetired: 0,
    chunkCount: chunks,
  };
}

export type OwnerEvent =
  | { t: 'recv'; q: Request }
  | { t: 'advance' }
  | { t: 'refresh'; h: string }
  | { t: 'stall'; h: string }
  | { t: 'expire'; h: string }
  | { t: 'projectable'; can: boolean };

function refuse(o: Owner, h: string, q: string | null): Owner {
  return { ...o, out: [{ k: 'no', h, q, code: 'unknown-handle' }] };
}

/**
 * Allocate this handle's next installation generation and emit its
 * manifest. Liveness is revalidated here, which is what makes it safe
 * to reach from deferred completion: an expired handle is refused, and
 * refusal never recreates admission.
 */
function install(o: Owner, h: string, q: string | null, aud: readonly string[]): Owner {
  const rec = o.handles[h];
  if (rec === undefined || !rec.live) return refuse(o, h, q);
  const g = rec.allocated + 1;
  return {
    ...o,
    handles: {
      ...o.handles,
      [h]: {
        ...rec,
        aud,
        allocated: g,
        g,
        at: o.r,
        sent: o.r,
        // A superseded installation's unsent chunks are retired.
        unsent: o.chunkCount,
        deferred: null,
      },
    },
    retiredUnsent: o.retiredUnsent + rec.unsent,
    out: [{ k: 'man', h, g, r: o.r, n: o.chunkCount, q }],
  };
}

export function ownerStep(o: Owner, e: OwnerEvent): Owner {
  switch (e.t) {
    case 'projectable': {
      if (!e.can) return { ...o, canProject: false, out: [] };
      // Completion after deferral is a second admission point, not a
      // continuation of the first. Each pending projection is cleared
      // and then re-submitted to `install`, which is the single place
      // where a projection becomes an installation and therefore the
      // single place the handle's liveness is revalidated. A dead
      // handle is refused — and the refusal is *delivered*, so the
      // caller rejoins rather than waiting on silence.
      let next: Owner = { ...o, canProject: true, out: [] };
      const emitted: Message[] = [];
      for (const [h, rec] of Object.entries(o.handles)) {
        const d = rec.deferred;
        if (d === null) continue;
        const current = next.handles[h];
        if (current === undefined) continue;
        const cleared: Owner = {
          ...next,
          handles: { ...next.handles, [h]: { ...current, deferred: null } },
        };
        const after = install(cleared, h, d.q, d.aud);
        const refused = after.out.some((m) => m.k === 'no');
        next = refused ? { ...after, deferredRetired: after.deferredRetired + 1 } : after;
        emitted.push(...next.out);
      }
      return { ...next, out: emitted };
    }

    case 'expire': {
      const rec = o.handles[e.h];
      if (rec === undefined) return { ...o, out: [] };
      // Expiry retires this handle's work: no chunks of a dead handle
      // are emitted, and no deferred projection of a dead handle is
      // completed later.
      return {
        ...o,
        handles: { ...o.handles, [e.h]: { ...rec, live: false, unsent: 0, deferred: null } },
        retiredUnsent: o.retiredUnsent + rec.unsent,
        deferredRetired: o.deferredRetired + (rec.deferred === null ? 0 : 1),
        out: [],
      };
    }

    case 'recv': {
      const req = e.q;

      // Only `join` creates a handle.
      if (req.k === 'join') {
        const h = `h${o.issued + 1}`;
        const fresh: OwnerHandle = {
          live: true,
          aud: req.aud,
          allocated: 0,
          g: 0,
          at: 0,
          sent: 0,
          unsent: 0,
          deferred: null,
        };
        const seeded: Owner = {
          ...o,
          issued: o.issued + 1,
          handles: { ...o.handles, [h]: fresh },
        };
        if (!o.canProject) {
          return {
            ...seeded,
            handles: { ...seeded.handles, [h]: { ...fresh, deferred: { q: req.q, aud: req.aud } } },
            out: [],
          };
        }
        return install(seeded, h, req.q, req.aud);
      }

      // Every other request must name a live handle this owner issued.
      const rec = o.handles[req.h];
      if (rec === undefined || !rec.live) return refuse(o, req.h, req.q);

      // Lifecycle controls are admitted whatever is being emitted
      // (§1.7b): a newer transition supersedes and retires unsent work.
      // `resync` recovers within the audience bound to the handle;
      // `aud`/`resume` carry the caller's desired audience.
      const aud = req.k === 'resync' ? rec.aud : req.aud;
      if (!o.canProject) {
        // One projection-unavailable disposition: DEFER, bounded by the
        // caller's own deadline. `not-ready` is never a control answer.
        // A newer control replaces the pending projection.
        return {
          ...o,
          handles: { ...o.handles, [req.h]: { ...rec, deferred: { q: req.q, aud } } },
          out: [],
        };
      }
      return install(o, req.h, req.q, aud);
    }

    case 'advance': {
      // The document advances for every live subscription at once. A
      // handle mid-emission still gets its delta, queued behind its own
      // chunks on its own ordered stream, based at the revision its
      // snapshot was taken at.
      const r = o.r + 1;
      const handles: Record<string, OwnerHandle> = { ...o.handles };
      const out: Message[] = [];
      for (const [h, rec] of Object.entries(o.handles)) {
        if (!rec.live || rec.g === 0) continue;
        out.push({ k: 'delta', h, g: rec.g, base: rec.sent, r });
        handles[h] = { ...rec, sent: r };
      }
      return { ...o, r, handles, out };
    }

    case 'refresh': {
      const rec = o.handles[e.h];
      if (rec === undefined || !rec.live || rec.g === 0) return { ...o, out: [] };
      // **The owner never supersedes its own in-flight emission.** If it
      // did, the replica — which refuses unsolicited manifests while
      // installing — would keep an assembly whose remaining chunks were
      // just retired, and would then drop the replacement's chunks as
      // belonging to no assembly: both generations stalled until a
      // deadline. A replacement is decided when emission is complete
      // (the delta-over-budget case in §1.9 is evaluated at delta time,
      // which is already idle), so refreshing mid-emission is refused
      // at the source rather than repaired at the replica.
      if (rec.unsent > 0) return { ...o, out: [] };
      return install(o, e.h, null, rec.aud);
    }

    case 'stall': {
      // The owner never hands the remainder over: no message is
      // produced, so nothing is "lost" on an ordered stream.
      const rec = o.handles[e.h];
      if (rec === undefined) return { ...o, out: [] };
      return {
        ...o,
        handles: { ...o.handles, [e.h]: { ...rec, unsent: 0 } },
        retiredUnsent: o.retiredUnsent + rec.unsent,
        out: [],
      };
    }
  }
}

/** The handles with chunks still to hand over, in issuance order. */
export function emitting(o: Owner): string[] {
  return Object.entries(o.handles)
    .filter(([, rec]) => rec.live && rec.unsent > 0)
    .map(([h]) => h);
}

/**
 * Hand the next unsent chunk of one handle to the transport. With no
 * handle named, the first one with work pending.
 */
export function emitChunk(o: Owner, handle?: string): { owner: Owner; message: Message | null } {
  const h = handle ?? emitting(o)[0];
  const rec = h === undefined ? undefined : o.handles[h];
  if (h === undefined || rec === undefined || !rec.live || rec.unsent === 0) {
    return { owner: { ...o, out: [] }, message: null };
  }
  const i = o.chunkCount - rec.unsent;
  return {
    owner: {
      ...o,
      handles: { ...o.handles, [h]: { ...rec, unsent: rec.unsent - 1 } },
      out: [],
    },
    message: { k: 'snap', h, g: rec.g, r: rec.at, n: o.chunkCount, i },
  };
}
