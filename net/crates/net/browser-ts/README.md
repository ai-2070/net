# @net-mesh/browser

[![npm](https://img.shields.io/npm/v/@net-mesh/browser.svg)](https://www.npmjs.com/package/@net-mesh/browser)

**Multiplayer state for browser games.** One player's page hosts the world;
everyone else joins it. Each player gets a live copy of the parts of the world
they're allowed to see, and your Three.js scene stays in sync with it
automatically.

- **One source of truth.** The host runs your game rules. Players ask for things
  to happen; the host decides. Nobody can cheat by editing their own copy.
- **Two kinds of messages, matched to how games work.** *Inputs* for things you
  send every frame (steering, aim) — the newest one wins. *Actions* for things
  that need an answer (fire, pick up, buy) — you get the result back, or a clear
  reason it was refused.
- **Hidden information stays hidden.** The host decides what each player can
  see. Fog of war, other players' hands, a secret objective: it never reaches a
  player who shouldn't have it, so it can't be read out of the browser.
- **Three.js in a few lines.** Say what an entity looks like; meshes are
  created, updated and cleaned up for you — and only the ones that changed are
  touched.
- **Peer to peer.** Players connect to each other directly over WebRTC.

```sh
npm install @net-mesh/browser three
```

`three` is only needed for the Three.js helpers. The package itself has no
dependency on it, so you use whichever Three.js version you like.

---

## Building with Claude Code (or another AI agent)

If an AI agent is writing your game code, give it the Net skills first.
Without them, an agent will write multiplayer code that looks right and runs,
but gets the details wrong — like re-running game rules on each player instead
of on the host, or retrying an action that may already have happened.

```sh
npx skills add ai-2070/net-claude-skill -g     # drop -g to install for this project only
```

The `net-browser` skill in that set covers this package end to end: connecting,
hosting and joining a world, inputs vs actions, hidden information, the Three.js
binding, and every error code. Ask for what you want in plain words — *"make
this a two-player game where one player hosts"* — and the agent will use it.

To let the agent read Net's actual source instead of guessing, add
[`opensrc`](https://github.com/vercel-labs/opensrc):

```sh
npx -y opensrc@latest path ai-2070/net
```

More install options: [Claude Skills](https://ai2070.net/docs/start/claude-skills).

---

## Before players can connect

A browser can't find other browsers on its own, so every multiplayer game on
Net needs two things that don't come from the page:

1. **An anchor** — a small server that introduces players to each other and
   helps them open a direct connection. Your game traffic doesn't pass through
   it.
2. **A credential per player** — a string the anchor issues, which the page
   passes to `connect()`.

**Today, the anchor that works with browsers is the browser-demo host in this
repository.** The general-purpose `net-mesh anchor serve` command doesn't yet
accept browser players (they connect, then time out). To run one locally:

```sh
git clone https://github.com/ai-2070/net
cd net/net/crates/net
cargo run --release --manifest-path examples/browser-demo/host/Cargo.toml -- \
  --headless --seconds 600
```

It prints its address and hands out a credential per player:

```sh
curl -s "http://localhost:<port>/config?tab=1" | jq -r .credentialB64
```

> **Building the game before you have an anchor?** The demo in
> [`demo/`](https://github.com/ai-2070/net/tree/master/net/crates/net/browser-ts/demo)
> runs a host and two players in a single page with no network at all, using the
> real store. Use it to build your game logic, then switch to a real anchor for
> actual multiplayer.

---

## Your first multiplayer scene

Five steps: describe the game, connect, host (or join), render, and play.

### 1. Describe the game

A *store definition* says what the world looks like and which messages players
can send. It contains no game rules, so it's safe to ship to every player.

```js
import { defineStore } from '@net-mesh/browser';

// Your validators: turn untrusted data into a clean value, or throw.
// (Zod or Valibot `schema.parse` works here too.)
const num = v => (Number.isFinite(Number(v)) ? Number(v) : 0);

export const arena = defineStore({
  id: 'my-game.arena',
  version: 1,
  state: v => ({ ships: v?.ships ?? {} }),   // the whole world
  empty: () => ({ ships: {} }),              // what a player sees before joining
  actions: {
    // Something that needs an answer. `enlist` gives the sender a ship…
    enlist: {
      input: () => ({}),
      output: v => ({ id: String(v.id) }),
    },
    // …and `fire` returns the target's new hull.
    fire: {
      input: v => ({ at: String(v.at) }),
      output: v => ({ hull: num(v.hull) }),
    },
  },
  inputs: {
    // Something sent every frame: only the newest matters.
    steer: v => ({ dx: num(v.dx), dz: num(v.dz), dt: num(v.dt) }),
  },
});
```

### 2. Connect

```js
import { connect } from '@net-mesh/browser';

const node = await connect({
  credentialB64,   // the credential the anchor issued for this player
  bootstrapUrl,    // the anchor's address (optional — the credential carries it)
});

const myId = node.nodeIdHex();   // this player's id, shared with whoever joins
```

### 3a. Host the world (on one player's page)

The host holds the real world and runs the rules. `context.peer` is the id of
the player who sent the message, and the connection proves it, so a player
can't pretend to be someone else.

```js
import { hostStore } from '@net-mesh/browser';

const host = hostStore({
  definition: arena,
  transport: node,
  initialState: { ships: {} },
  maxEventBytes: 8104,                         // the largest message the connection carries

  // Who may do what. Here: no shooting yourself.
  authorize: request =>
    request.type !== 'action' || request.input.at !== request.peer,

  // What each player may see. Everyone sees every ship here.
  project: (state, audience) => state,

  actions: {
    enlist: (input, context) => {
      const ships = { ...context.getState().ships };
      ships[context.peer] ??= { x: 0, z: 0, hull: 100 };   // one ship per player
      context.setState({ ships });
      return { id: context.peer };
    },
    fire: (input, context) => {
      const ships = { ...context.getState().ships };
      const target = ships[input.at];
      if (!target) throw new Error('no such ship');   // refused: the player gets
                                                      // code 'action-rejected' — your
                                                      // message stays on the host
      ships[input.at] = { ...target, hull: Math.max(0, target.hull - 25) };
      context.setState({ ships });
      return { hull: ships[input.at].hull };
    },
  },
  inputs: {
    steer: (input, context) => {
      const ships = { ...context.getState().ships };
      const ship = ships[context.peer];
      if (!ship) return;
      ships[context.peer] = { ...ship, x: ship.x + input.dx * 6 * input.dt,
                                        z: ship.z + input.dz * 6 * input.dt };
      context.setState({ ships });
    },
  },
});

// Let other players find this host. Announcements expire after a few seconds,
// so keep refreshing them.
const TAG = 'my-game.host';
await node.announce([TAG]);
setInterval(() => node.announce([TAG]).catch(() => {}), 2_000);
```

### 3b. Join the world (on everyone else's page)

```js
import { joinStore, peerIdHex } from '@net-mesh/browser';

// Wait until the host's announcement is visible, then join.
async function findHost(hostId) {
  for (let i = 0; i < 80; i++) {
    const hosts = await node.query('my-game.host');
    if (hosts.some(h => peerIdHex(h.nodeId) === hostId)) return;
    await new Promise(resolve => setTimeout(resolve, 250));
  }
  throw new Error('the host never showed up');
}

await findHost(hostId);            // `hostId` = the host's `node.nodeIdHex()`
const world = joinStore({
  definition: arena,
  transport: node,
  host: hostId,
  audience: ['crew'],              // which view of the world to ask for
  key: 'player',                   // an opaque join token (required)
  maxEventBytes: 8104,
});
await world.ready();               // resolves once the first copy has arrived
await world.act('enlist', {});     // ask the host for a ship
```

### 4. Render it with Three.js

```js
import * as THREE from 'three';
import { bindEntities } from '@net-mesh/browser/three';

const geometry = new THREE.ConeGeometry(0.4, 1, 8);   // shared by every ship
const material = new THREE.MeshStandardMaterial({ color: 0x44aaff });

const ships = bindEntities({
  store: world,                   // a joined world, or the `host` itself
  scene,
  select: state => state.ships,   // the entities to draw, by id
  binding: {
    create: (ship, id) => {       // a ship appeared: build it where it is
      const mesh = new THREE.Mesh(geometry, material);
      mesh.position.set(ship.x, 0, ship.z);
      return mesh;
    },
    update: (mesh, ship) => mesh.position.set(ship.x, 0, ship.z),
    remove: mesh => {},           // free anything you created per ship here
  },
});
```

`create` runs once when a ship appears, `update` only for ships that actually
changed afterwards, and a ship that leaves the world is removed from the scene
for you. Shared geometry and materials (like the
two above) are yours to dispose once, when the match ends.

### 5. Play

```js
function frame(now) {
  // Every frame: send the held direction. The newest input wins.
  world.input('steer', { dx: keys.dx, dz: keys.dz, dt: 1 / 60 });
  renderer.render(scene, camera);
  requestAnimationFrame(frame);
}
requestAnimationFrame(frame);

// On a key press: an action, answered by the host.
try {
  const { hull } = await world.act('fire', { at: targetId });
  showHit(targetId, hull);
} catch (error) {
  showMessage(`Refused: ${error.code}`);   // e.g. 'forbidden' for firing at yourself
}
```

> **The hosting player doesn't join its own world** — a page can't connect to
> itself. On the host's page, render from `host` directly
> (`bindEntities({ store: host, … })`) and apply that player's moves on the host.
> The demo's
> [`main.js`](https://github.com/ai-2070/net/blob/master/net/crates/net/browser-ts/demo/main.js)
> shows a small helper that does this and applies the same `authorize` rules to
> the host's own player.

---

## Things worth knowing

**Inputs vs actions.** `world.input(name, value)` never waits and never
answers. It tells you only what happened locally (`queued`, `replaced`, or
`dropped`), and a dropped input may simply be superseded by the next one — write
your game so that's fine. `world.act(name, input)` returns a promise with the
host's answer, or rejects with a reason.

**Hidden information.** A joining player asks for an *audience* (`['crew']`).
The host's `project(state, audience)` returns what that audience may see. Leave
secret things out of the returned state (or set them to `null`). Hidden data is
never sent, so it isn't sitting in the page waiting to be found.

**One tab per player.** All tabs of the same site in one browser profile share a
single identity, so two tabs are the *same* player — and a player can't join
itself. To test with two players on one machine, use two browser profiles, or
two different browsers.

**Announcements expire.** A host that announces once disappears from searches
a few seconds later. Keep re-announcing on a timer, as in step 3a.

**Several worlds on one host.** A host can run a lobby and a match at the same
time: give each `hostStore` a `store: 'lobby'` / `store: 'match'` name, and pass
the same name to `joinStore`.

**Cleaning up.** `ships.dispose()` removes everything the binding added to the
scene. `world.close()` leaves a world, `host.close()` stops hosting, and
`node.close()` disconnects.

---

## When something goes wrong

A refused store operation rejects with a `StoreError`. Check its `.code`:

| `code` | What it means |
|---|---|
| `forbidden` | Your `authorize` said no |
| `action-rejected` | The host refused the action — your handler threw |
| `invalid-data` | A message didn't pass the definition's validators |
| `version-mismatch` | Host and player were built from different game versions |
| `not-ready` | There's no synchronized copy yet (before `ready()`, or while reconnecting) |
| `timeout` | Your deadline passed before an answer arrived |
| `aborted` | The world handle closed, or a newer request replaced this one |
| `indeterminate` | No answer came back — the action *may* have happened. Don't retry blindly |
| `owner-lost` | The host stopped hosting this world. Join a new one |
| `closed` | This connection to the world can't be used any more — join again |
| `capacity` | A limit was reached (too much in flight at once) |
| `result-expired` | The request can't be run again and its original result is gone. It does **not** mean it succeeded |

Connection problems reject with a `LeafError` whose `.kind` says what happened —
for example `rpc-timeout`, `session-lost`, or `udp-blocked` (the player's network
blocks the direct connection). The
[errors guide](https://ai2070.net/docs/sdk/browser/errors) lists them all.

---

## Loading the WebAssembly

The package includes a small WebAssembly module, and by default loads it from
the same place as the package's JavaScript. If your build serves it from
somewhere else, say where:

```js
await connect({ credentialB64, wasmUrl: '/assets/net_leaf_bg.wasm' });
```

A page downloads about **240 kB** gzipped in total.

---

## Beyond games

The same connection also gives you request/response calls between players,
publish/subscribe channels, raw streams, and a mode where all tabs of a site
share one connection. See the [Browser SDK docs](https://ai2070.net/docs/sdk/browser).

## Learn more

- [Browser quickstart](https://ai2070.net/docs/sdk/browser/quickstart) — connecting, step by step
- [The store](https://ai2070.net/docs/sdk/browser/store) — hosting, joining, audiences, every option
- [Three.js](https://ai2070.net/docs/sdk/browser/three) — the binding in depth, and disposing resources properly
- [Errors](https://ai2070.net/docs/sdk/browser/errors) — every error and what to do about it
- [The demo](https://github.com/ai-2070/net/tree/master/net/crates/net/browser-ts/demo) — a small playable game using all of the above

## Working on this package

```sh
npm install
npm run build   # needs the wasm leaf built first — see DESIGN.md
npm test
```

How the package is built, and why it works the way it does, is in
[DESIGN.md](https://github.com/ai-2070/net/blob/master/net/crates/net/browser-ts/DESIGN.md).

## License

MIT OR Apache-2.0
