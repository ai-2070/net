/**
 * The game, as a store definition — no rendering, no transport.
 *
 * This is the half a reviewer should read first: it says what the world
 * is, who may see what, and what a player may do to it. Everything else
 * in this directory is either Three.js or plumbing.
 */

/** How far a ship moves per second of held input. */
const SPEED = 6;

/** The arena's half-extent; a ship is clamped to it. */
export const ARENA = 12;

function record(value) {
  if (typeof value !== 'object' || value === null) throw new Error('not a record');
  return value;
}

function number(value, fallback = 0) {
  const parsed = Number(value);
  return Number.isFinite(parsed) ? parsed : fallback;
}

/**
 * One ship per player.
 *
 * `crew` is the audience every player reads; `command` is a second
 * audience carrying the host's private waypoint, so the demo shows an
 * audience actually withholding something rather than asserting that it
 * would.
 */
function shipOf(value) {
  const raw = record(value);
  return {
    x: number(raw.x),
    z: number(raw.z),
    heading: number(raw.heading),
    hull: number(raw.hull, 100),
    colour: number(raw.colour),
  };
}

export function defineGame(defineStore) {
  return defineStore({
    id: 'net.demo.fleet',
    version: 1,
    state: value => {
      const raw = record(value);
      const ships = {};
      for (const [id, ship] of Object.entries(record(raw.ships ?? {}))) ships[id] = shipOf(ship);
      return {
        ships,
        waypoint:
          raw.waypoint === undefined || raw.waypoint === null
            ? null
            : { x: number(record(raw.waypoint).x), z: number(record(raw.waypoint).z) },
        tick: number(raw.tick),
      };
    },
    empty: () => ({ ships: {}, waypoint: null, tick: 0 }),
    actions: {
      /** Take a ship. One per player, named by the authenticated peer. */
      enlist: {
        input: value => ({ colour: number(record(value).colour) }),
        output: value => ({ id: String(record(value).id) }),
      },
      fire: {
        input: value => ({ at: String(record(value).at) }),
        output: value => ({ hull: number(record(value).hull) }),
      },
    },
    inputs: {
      /** Held direction, coalesced: the newest wins and loss is fine. */
      steer: value => {
        const raw = record(value);
        return { dx: number(raw.dx), dz: number(raw.dz), dt: number(raw.dt) };
      },
    },
  });
}

/**
 * What each audience may see.
 *
 * `crew` sees every ship. `command` additionally sees the waypoint. A
 * player who asks for `crew` alone never receives the waypoint — not
 * hidden in the renderer, absent from the frames.
 */
export function project(state, audience) {
  return {
    ships: audience.includes('crew') ? state.ships : {},
    waypoint: audience.includes('command') ? state.waypoint : null,
    tick: state.tick,
  };
}

/** The handlers the host runs. Synchronous, one transaction each. */
export function handlers() {
  return {
    actions: {
      enlist: (input, context) => {
        const id = context.peer;
        const ships = { ...context.getState().ships };
        if (ships[id] === undefined) {
          ships[id] = {
            x: (Object.keys(ships).length % 5) * 3 - 6,
            z: 0,
            heading: 0,
            hull: 100,
            colour: input.colour,
          };
          context.setState({ ships });
        }
        return { id };
      },
      fire: (input, context) => {
        const ships = { ...context.getState().ships };
        const target = ships[input.at];
        if (target === undefined) throw new Error('no such ship');
        const hull = Math.max(0, target.hull - 25);
        ships[input.at] = { ...target, hull };
        context.setState({ ships });
        return { hull };
      },
    },
    inputs: {
      steer: (input, context) => {
        const state = context.getState();
        const ship = state.ships[context.peer];
        if (ship === undefined) return;
        const dt = Math.min(Math.max(input.dt, 0), 0.25);
        const x = Math.min(Math.max(ship.x + input.dx * SPEED * dt, -ARENA), ARENA);
        const z = Math.min(Math.max(ship.z + input.dz * SPEED * dt, -ARENA), ARENA);
        const heading =
          input.dx === 0 && input.dz === 0 ? ship.heading : Math.atan2(input.dx, input.dz);
        context.setState({
          ships: { ...state.ships, [context.peer]: { ...ship, x, z, heading } },
          tick: state.tick + 1,
        });
      },
    },
  };
}

/**
 * Who may read what, and who may act.
 *
 * The peer is the authenticated one — the store hands this the identity
 * the transport proved, and there is no frame field it could have come
 * from. `fire` at yourself is refused, which is a rule a page cannot
 * enforce for itself.
 */
export function authorize(request) {
  if (request.type === 'read') {
    // Anyone may watch the fleet; only the host's own page may read
    // the waypoint.
    return !request.audience.includes('command') || request.peer === request.host;
  }
  if (request.type === 'action' && request.name === 'fire') {
    return request.input.at !== request.peer;
  }
  return true;
}
