/**
 * Owner dispatch (brief §1.3, §1.5, §1.12, §2) — slice C.
 *
 * Every row goes through the real codec and, where a snapshot is
 * involved, through the real assembler: the owner emits frames, a
 * replica-side `AssemblyTable` consumes them, and the assertion is on
 * **what the caller ends up able to see**. A dispatcher that answered
 * with plausible frames nobody could assemble would pass a
 * frame-shaped test and fail this one.
 *
 * The identity rows are the slice's acceptance case (§4 C): `authorize`
 * must receive the authenticated originating caller, and a handle must
 * stay bound to it. Both are asserted as observables — what the policy
 * was handed, and what a second peer gets back.
 */

import { describe, expect, it } from 'vitest';

import { Assembly, AssemblyTable, type AssemblyManifest } from '../../src/store/assembly.js';
import { StoreOwner, MAX_HANDLES, HANDLE_LEASE_MS, type Outbound } from '../../src/store/owner.js';
import type { AccessRequest } from '../../src/store/types.js';
import { decimalValue, decodeMessage, encodeMessage, type Hex } from '../../src/store/wire.js';

const MAX_EVENT_BYTES = 8104;
const PEER_A = '00000000000000aa';
const PEER_B = '00000000000000bb';

interface World {
  readonly ship: { readonly heading: number };
  readonly secret: Record<string, string>;
}

function world(heading = 90): World {
  return { ship: { heading }, secret: { plan: 'burn the fleet' } };
}

/** A definition with a real validator and a real `empty()`. */
function definition() {
  return {
    id: 'pirate.ship',
    version: 1,
    state: (raw: unknown): World => {
      const value = raw as World;
      if (typeof value !== 'object' || value === null) throw new Error('not a record');
      if (typeof value.ship?.heading !== 'number') throw new Error('heading');
      if (typeof value.secret !== 'object' || value.secret === null) throw new Error('secret');
      return { ship: { heading: value.ship.heading }, secret: { ...value.secret } };
    },
    empty: (): World => ({ ship: { heading: 0 }, secret: {} }),
    actions: {},
    inputs: {},
  };
}

interface Rig {
  readonly owner: StoreOwner<World, Record<string, never>, Record<string, never>>;
  readonly seen: AccessRequest<Record<string, never>, Record<string, never>>[];
  handles: number;
}

function rig(
  options: {
    authorize?: (request: AccessRequest<Record<string, never>, Record<string, never>>) => boolean;
    project?: (state: World, audience: readonly string[]) => World;
    maxEventBytes?: number;
  } = {},
): Rig {
  const seen: AccessRequest<Record<string, never>, Record<string, never>>[] = [];
  let handles = 0;
  const self: Rig = {
    seen,
    handles: 0,
    owner: new StoreOwner<World, Record<string, never>, Record<string, never>>({
      definition: definition(),
      authorize: (request) => {
        seen.push(request);
        return options.authorize ? options.authorize(request) : true;
      },
      // The default projection is an audience decision, not the whole
      // state: `crew` sees the ship, nobody else sees the secret.
      project:
        options.project ??
        ((state, audience) =>
          audience.includes('captain') ? state : { ship: state.ship, secret: {} }),
      maxEventBytes: options.maxEventBytes ?? MAX_EVENT_BYTES,
      now: () => 0,
      newHandle: () => {
        handles += 1;
        return handles.toString(16).padStart(32, '0');
      },
      newIncarnation: () => 'f'.repeat(16),
      actions: {},
      inputs: {},
    }),
  };
  return self;
}

let nextQ = 0;
function q(): Hex {
  nextQ += 1;
  return nextQ.toString(16).padStart(16, '0');
}

function joinFrame(aud: readonly string[], over: { def?: string; ver?: number } = {}): string {
  return encodeMessage({
    k: 'join',
    q: q(),
    def: over.def ?? 'pirate.ship',
    ver: over.ver ?? 1,
    key: 'black-petrel',
    aud,
  });
}

/** Assemble whatever snapshot the owner emitted, as a replica would. */
function assemble(out: readonly Outbound[]): unknown {
  const table = new AssemblyTable();
  let open: Assembly | undefined;
  let document: unknown;
  for (const frame of out) {
    const decoded = decodeMessage(frame.frame, { maxBytes: MAX_EVENT_BYTES, as: 'replica' });
    if (!decoded.ok) throw new Error(`the owner emitted an undecodable frame: ${decoded.reason}`);
    const message = decoded.message;
    if (message.k === 'man') {
      const opened = table.open(message as AssemblyManifest, 0);
      if (!(opened instanceof Assembly)) throw new Error('the manifest was refused');
      open = opened;
    } else if (message.k === 'snap') {
      if (open === undefined) throw new Error('a chunk arrived before its manifest');
      const outcome = open.accept(message, (raw) => raw, 0);
      if (!outcome.ok) throw new Error(`a chunk was refused: ${outcome.reason}`);
      if (outcome.done) document = outcome.document;
    }
  }
  return document;
}

/** The refusal code the owner answered with, if it answered one. */
/** The `man` frame an emission opened with, decoded. */
function manifestOf(out: readonly Outbound[]) {
  const decoded = decodeMessage(out[0]!.frame, { maxBytes: MAX_EVENT_BYTES, as: 'replica' });
  if (!decoded.ok || decoded.message.k !== 'man') throw new Error('the emission does not open with a manifest');
  return decoded.message;
}

function refusalCode(out: readonly Outbound[]): string | null {
  for (const frame of out) {
    const decoded = decodeMessage(frame.frame, { maxBytes: MAX_EVENT_BYTES, as: 'replica' });
    if (decoded.ok && decoded.message.k === 'no') return decoded.message.code;
  }
  return null;
}

function handleOf(out: readonly Outbound[]): Hex {
  for (const frame of out) {
    const decoded = decodeMessage(frame.frame, { maxBytes: MAX_EVENT_BYTES, as: 'replica' });
    if (decoded.ok && decoded.message.k === 'man') return decoded.message.h;
  }
  throw new Error('no manifest was emitted');
}

describe('a join is admitted and served', () => {
  it('emits a manifest and chunks a replica can assemble', () => {
    const r = rig();
    r.owner.commit(world());

    const { out, refused } = r.owner.receive(joinFrame(['crew']), PEER_A);

    expect(refused).toBeNull();
    // Addressed to the caller, and to nobody else.
    expect(new Set(out.map((o) => o.peer))).toEqual(new Set([PEER_A]));
    expect(assemble(out)).toEqual({ ship: { heading: 90 }, secret: {} });
  });

  it('carries the manifest fields the assembler needs', () => {
    const r = rig();
    r.owner.commit(world());
    const { out } = r.owner.receive(joinFrame(['crew']), PEER_A);

    const first = decodeMessage(out[0]!.frame, { maxBytes: MAX_EVENT_BYTES, as: 'replica' });
    if (!first.ok || first.message.k !== 'man') throw new Error('the first frame must be the manifest');
    // Solicited, so it echoes the join's `q`; generation 1 on a fresh
    // handle; the incarnation the owner was built with.
    expect(first.message.q).toBeDefined();
    expect(first.message.g).toBe('1');
    expect(first.message.inc).toBe('f'.repeat(16));
    expect(first.message.n).toBeGreaterThanOrEqual(1);
  });

  it('allocates a monotone generation per handle, only on acceptance', () => {
    const r = rig({ authorize: (request) => request.type === 'read' && request.audience.includes('crew') });
    r.owner.commit(world());

    const admitted = r.owner.receive(joinFrame(['crew']), PEER_A);
    const h = handleOf(admitted.out);
    expect(r.owner.handle(h)?.generation).toBe(1);

    // A refused join allocates nothing: no handle, no generation.
    const refused = r.owner.receive(joinFrame(['stowaway']), PEER_B);
    expect(refusalCode(refused.out)).toBe('forbidden');
    expect(r.owner.handleCount).toBe(1);
  });

  it('serves each audience its own projection', () => {
    const r = rig();
    r.owner.commit(world());

    const crew = r.owner.receive(joinFrame(['crew']), PEER_A);
    const captain = r.owner.receive(joinFrame(['captain']), PEER_B);

    // The crew cannot see the secret; the captain can. `empty()` is
    // what absence looks like, not a plausible value.
    expect(assemble(crew.out)).toEqual({ ship: { heading: 90 }, secret: {} });
    expect(assemble(captain.out)).toEqual({ ship: { heading: 90 }, secret: { plan: 'burn the fleet' } });
  });
});

describe('authorize receives the authenticated caller', () => {
  it('is handed the peer the transport proved, and the requested audience', () => {
    const r = rig();
    r.owner.receive(joinFrame(['crew', 'deck']), PEER_A);

    expect(r.seen).toHaveLength(1);
    expect(r.seen[0]).toEqual({ type: 'read', peer: PEER_A, audience: ['crew', 'deck'] });
  });

  it('cannot be told a different caller by the frame', () => {
    // There is no originator field in any store frame (§2), so a
    // caller cannot name itself. The proof is at the codec: a frame
    // that tries is refused before dispatch sees it.
    const forged = JSON.stringify({
      v: 1,
      k: 'join',
      q: q(),
      def: 'pirate.ship',
      ver: 1,
      key: 'k',
      aud: ['crew'],
      peer: PEER_B,
    });
    const r = rig();
    const { refused } = r.owner.receive(forged, PEER_A);

    expect(refused).toBe('unknown-field');
    expect(r.seen).toHaveLength(0);
  });

  it('refuses when the policy refuses, allocating nothing', () => {
    const r = rig({ authorize: () => false });
    const { out, refused } = r.owner.receive(joinFrame(['crew']), PEER_A);

    expect(refused).toBe('join-forbidden');
    expect(refusalCode(out)).toBe('forbidden');
    expect(r.owner.handleCount).toBe(0);
    // No snapshot was emitted either: a refusal must not leak a
    // projection.
    expect(out.every((o) => !o.frame.includes('"k":"snap"'))).toBe(true);
  });

  it('treats a throwing policy as a refusal', () => {
    const r = rig({
      authorize: () => {
        throw new Error('the policy exploded');
      },
    });
    const { out, refused } = r.owner.receive(joinFrame(['crew']), PEER_A);

    expect(refused).toBe('join-forbidden');
    expect(refusalCode(out)).toBe('forbidden');
    expect(r.owner.handleCount).toBe(0);
  });
});

describe('a handle stays bound to its authenticated peer', () => {
  it('refuses another peer using a valid handle, without disclosing it', () => {
    const r = rig();
    r.owner.commit(world());
    const h = handleOf(r.owner.receive(joinFrame(['crew']), PEER_A).out);

    const stolen = r.owner.receive(encodeMessage({ k: 'alive', q: q(), h }), PEER_B);

    expect(stolen.refused).toBe('handle-foreign-peer');
    // `closed`, not `forbidden`: the refusal must not disclose that the
    // handle exists (§2).
    expect(refusalCode(stolen.out)).toBe('closed');
    // And the handle is untouched for its real owner.
    expect(r.owner.receive(encodeMessage({ k: 'alive', q: q(), h }), PEER_A).refused).toBeNull();
  });

  it('answers an unknown handle exactly as it answers a foreign one', () => {
    const r = rig();
    const unknown = 'd'.repeat(32);
    const outcome = r.owner.receive(encodeMessage({ k: 'alive', q: q(), h: unknown }), PEER_A);

    expect(outcome.refused).toBe('handle-unknown');
    // One code for both, so a caller cannot probe for existence.
    expect(refusalCode(outcome.out)).toBe('closed');
  });

  it('re-checks the binding on every message, not only at admission', () => {
    const r = rig();
    const h = handleOf(r.owner.receive(joinFrame(['crew']), PEER_A).out);

    // Three different kinds, each re-checked.
    for (const frame of [
      encodeMessage({ k: 'alive', q: q(), h }),
      encodeMessage({ k: 'leave', q: q(), h }),
      encodeMessage({ k: 'resync', q: q(), h, g: '1', have: '0' }),
    ]) {
      expect(r.owner.receive(frame, PEER_B).refused).toBe('handle-foreign-peer');
    }
    expect(r.owner.handle(h)).toBeDefined();
  });
});

describe('bounds and the lease', () => {
  it('refuses a join past the handle bound', () => {
    const r = rig();
    for (let i = 0; i < MAX_HANDLES; i += 1) {
      expect(r.owner.receive(joinFrame(['crew']), PEER_A).refused).toBeNull();
    }
    expect(r.owner.handleCount).toBe(MAX_HANDLES);

    const outcome = r.owner.receive(joinFrame(['crew']), PEER_A);
    expect(outcome.refused).toBe('join-capacity');
    expect(refusalCode(outcome.out)).toBe('capacity');
  });

  it('expires a handle on its lease, and a later message is closed', () => {
    const r = rig();
    const h = handleOf(r.owner.receive(joinFrame(['crew']), PEER_A).out);

    expect(r.owner.sweep(HANDLE_LEASE_MS - 1)).toEqual([]);
    expect(r.owner.sweep(HANDLE_LEASE_MS)).toEqual([h]);

    const late = r.owner.receive(encodeMessage({ k: 'alive', q: q(), h }), PEER_A, HANDLE_LEASE_MS);
    expect(late.refused).toBe('handle-unknown');
    expect(refusalCode(late.out)).toBe('closed');
  });

  it('closes an expired handle at admission, with no sweep first', () => {
    // The sweep is a reclamation pass, not the gate: a message
    // arriving after the lease must be refused even if nothing has
    // swept yet, and the handle must be gone afterwards.
    const r = rig();
    const h = handleOf(r.owner.receive(joinFrame(['crew']), PEER_A).out);

    const late = r.owner.receive(encodeMessage({ k: 'alive', q: q(), h }), PEER_A, HANDLE_LEASE_MS);

    expect(late.refused).toBe('handle-expired');
    expect(refusalCode(late.out)).toBe('closed');
    expect(r.owner.handleCount).toBe(0);
  });

  it('keeps a handle alive on an accepted `alive`', () => {
    const r = rig();
    const h = handleOf(r.owner.receive(joinFrame(['crew']), PEER_A).out);

    // Accepted at t = lease/2, so the lease runs from there.
    expect(r.owner.receive(encodeMessage({ k: 'alive', q: q(), h }), PEER_A, HANDLE_LEASE_MS / 2).refused).toBeNull();
    expect(r.owner.sweep(HANDLE_LEASE_MS)).toEqual([]);
    expect(r.owner.sweep(HANDLE_LEASE_MS * 2)).toEqual([h]);
  });

  it('malformed traffic does not renew a lease', () => {
    const r = rig();
    const h = handleOf(r.owner.receive(joinFrame(['crew']), PEER_A).out);

    // A stream of refused frames across the lease.
    for (const t of [1000, 20_000, 40_000]) {
      r.owner.receive('{"v":1,"k":"nope"}', PEER_A, t);
      r.owner.receive(encodeMessage({ k: 'alive', q: q(), h }), PEER_B, t);
    }

    // The handle still expires on schedule: a refusal is not activity.
    expect(r.owner.sweep(HANDLE_LEASE_MS)).toEqual([h]);
  });

  it('refuses the wrong definition and the wrong version', () => {
    const r = rig();
    const wrongDef = r.owner.receive(joinFrame(['crew'], { def: 'other.store' }), PEER_A);
    expect(wrongDef.refused).toBe('join-wrong-definition');
    expect(refusalCode(wrongDef.out)).toBe('version-mismatch');

    const wrongVer = r.owner.receive(joinFrame(['crew'], { ver: 2 }), PEER_A);
    expect(wrongVer.refused).toBe('join-wrong-version');
    expect(refusalCode(wrongVer.out)).toBe('version-mismatch');
    expect(r.owner.handleCount).toBe(0);
  });

  it('refuses to construct at all when chunking cannot fit', () => {
    // §1.9's startup fail-fast: better than starting and discovering it
    // on the first projection.
    expect(() => rig({ maxEventBytes: 1024 })).toThrow(/below the 1024 B floor/);
  });
});

describe('the projection is the definition’s, or it is absence', () => {
  it('ships `empty()` when the projection does not validate', () => {
    // An owner bug must not become a shipped snapshot that a replica
    // refuses — and must not become the FULL state either.
    const r = rig({ project: () => ({ ship: { heading: 'north' } }) as unknown as World });
    r.owner.commit(world());

    const { out } = r.owner.receive(joinFrame(['crew']), PEER_A);

    expect(assemble(out)).toEqual({ ship: { heading: 0 }, secret: {} });
  });

  it('refuses the join when the projection cannot be carried', () => {
    const r = rig({ project: (state) => ({ ...state, secret: { pad: 'x'.repeat(2 * 1024 * 1024) } }) });
    r.owner.commit(world());

    const outcome = r.owner.receive(joinFrame(['crew']), PEER_A);

    expect(outcome.refused).toBe('join-projection-capacity');
    expect(refusalCode(outcome.out)).toBe('capacity');
    // The handle it had allocated goes with the refusal: admitting one
    // that can never be served is worse than refusing.
    expect(r.owner.handleCount).toBe(0);
  });

  it('projects the state as of the commit, at its revision', () => {
    const r = rig();
    r.owner.commit(world(90));
    r.owner.commit(world(180));
    expect(r.owner.currentRevision).toBe(2);

    const { out } = r.owner.receive(joinFrame(['crew']), PEER_A);
    const first = decodeMessage(out[0]!.frame, { maxBytes: MAX_EVENT_BYTES, as: 'replica' });
    if (!first.ok || first.message.k !== 'man') throw new Error('expected a manifest');

    expect(first.message.r).toBe('2');
    expect(assemble(out)).toEqual({ ship: { heading: 180 }, secret: {} });
  });

  it('does not move the revision for an unchanged commit', () => {
    const r = rig();
    r.owner.commit(world(90));
    const before = r.owner.currentRevision;
    r.owner.commit(world(90));
    expect(r.owner.currentRevision).toBe(before);
  });
});

describe('kinds this slice does not implement are refused, not half-served', () => {
  it('refuses the controls it does not implement, never as `not-ready`', () => {
    const r = rig();
    const h = handleOf(r.owner.receive(joinFrame(['crew']), PEER_A).out);

    for (const frame of [
      encodeMessage({ k: 'aud', q: q(), h, aud: ['deck'] }),
      encodeMessage({ k: 'resume', q: q(), h, aud: ['crew'] }),
    ]) {
      const refused = r.owner.receive(frame, PEER_A);

      expect(refused.refused).toMatch(/^unimplemented-kind:/);
      // A control is NEVER answered `not-ready` (§1.7b), even when the
      // reason is "not implemented".
      expect(refusalCode(refused.out)).not.toBe('not-ready');
    }
  });

  it('accepts `leave` and gives up the handle', () => {
    const r = rig();
    const h = handleOf(r.owner.receive(joinFrame(['crew']), PEER_A).out);

    const left = r.owner.receive(encodeMessage({ k: 'leave', q: q(), h }), PEER_A);

    expect(left.refused).toBeNull();
    expect(r.owner.handleCount).toBe(0);
    // Anything after `leave` is closed.
    expect(r.owner.receive(encodeMessage({ k: 'alive', q: q(), h }), PEER_A).refused).toBe('handle-unknown');
  });
});

describe('a solicited `resync` is answered, never refused for its position', () => {
  it('answers with a newly allocated generation and its chunks', () => {
    const r = rig();
    const served = r.owner.receive(joinFrame(['crew']), PEER_A);
    const h = handleOf(served.out);
    const first = manifestOf(served.out);

    const again = r.owner.receive(
      encodeMessage({ k: 'resync', q: q(), h, g: first.g, have: first.r }),
      PEER_A,
    );

    expect(again.refused).toBeNull();
    const second = manifestOf(again.out);
    // A NEW installation generation, not a replay of the old one
    // (§1.7a: an allocated generation is consumed for ever).
    expect(decimalValue(second.g)).toBe(decimalValue(first.g) + 1n);
    expect(again.out).toHaveLength(second.n + 1);
  });

  it('never refuses a `resync` for naming a stale or unknown generation', () => {
    // §1.8: `(g, have)` are advisory historical position. The replica
    // that most needs recovering is precisely the one whose newer
    // generation never installed, so it can only name an older one —
    // and a replica recovering from a failed first install names
    // nothing at all.
    const r = rig();
    const h = handleOf(r.owner.receive(joinFrame(['crew']), PEER_A).out);

    for (const [g, have] of [
      ['0', '0'],
      ['1', '1'],
      ['99999', '99999'],
    ] as const) {
      const answered = r.owner.receive(encodeMessage({ k: 'resync', q: q(), h, g, have }), PEER_A);

      expect(answered.refused).toBeNull();
      expect(manifestOf(answered.out).k).toBe('man');
    }
  });

  it('refuses a `resync` on a dead handle as `closed`', () => {
    const r = rig();
    const h = handleOf(r.owner.receive(joinFrame(['crew']), PEER_A).out);
    r.owner.receive(encodeMessage({ k: 'leave', q: q(), h }), PEER_A);

    const orphan = r.owner.receive(encodeMessage({ k: 'resync', q: q(), h, g: '1', have: '1' }), PEER_A);

    expect(orphan.refused).toBe('handle-unknown');
    expect(refusalCode(orphan.out)).toBe('closed');
  });

  it('renews the lease it answers on', () => {
    const r = rig();
    const h = handleOf(r.owner.receive(joinFrame(['crew']), PEER_A).out);

    // A resync late in the lease keeps the handle alive, like any
    // accepted message.
    const late = HANDLE_LEASE_MS - 1;
    r.owner.receive(encodeMessage({ k: 'resync', q: q(), h, g: '1', have: '1' }), PEER_A, late);
    const after = r.owner.receive(encodeMessage({ k: 'alive', q: q(), h }), PEER_A, late + 1);

    expect(after.refused).toBeNull();
  });
});

describe('counters', () => {
  it('moves exactly one counter per refusal', () => {
    const r = rig({ authorize: () => false });
    r.owner.receive(joinFrame(['crew']), PEER_A);
    r.owner.receive(joinFrame(['crew']), PEER_A);
    r.owner.receive('not json', PEER_A);

    const counters = r.owner.snapshotCounters();
    expect(counters['join-forbidden']).toBe(2);
    expect(Object.values(counters).reduce((a, b) => a + b, 0)).toBe(3);
  });

  it('counts an accepted frame nowhere', () => {
    const r = rig();
    r.owner.receive(joinFrame(['crew']), PEER_A);
    expect(r.owner.snapshotCounters()).toEqual({});
  });
});
