/**
 * Sharded arena: 4 shard hosts and 32 bot players in one page, over the
 * local mesh. The canvas draws player 0's merged view (`joinShards`),
 * each entity coloured by the shard that owns it; the HUD prints what the
 * fencing counted.
 *
 * Local mode only: delivery is a function call, so this shows the recipe's
 * wiring and ownership rules, not that browsers can reach each other (the
 * S0b harness, `examples/shard-topology`, is where that is measured).
 */

import { hostNetcode, joinNetcode, joinShards } from '../../dist/netcode/index.js';
import { handoffLink, regionHandoffs, regionState } from '../../dist/world/index.js';
import { createLocalMesh } from '../../dist/local.js';
import { shardHost } from './shard.js';

const lib = { hostNetcode, joinNetcode, handoffLink, regionHandoffs, regionState };
const LABEL = 'arena.move';
const SIDE = 200;
const HOSTS = 4;
const PLAYERS = 32;
const COLOURS = ['#e4572e', '#29335c', '#f3a712', '#669bbc'];

const hex = (prefix, i) => (prefix * 0x100 + i).toString(16).padStart(16, '0');
const clamp = v => Math.max(0, Math.min(SIDE, v));
const apply = (body, input) => ({ x: clamp(body.x + input.dx), z: clamp(body.z + input.dz) });

const mesh = createLocalMesh();
const shards = Array.from({ length: HOSTS }, (_, k) => `s${k}`);
const playerIds = Array.from({ length: PLAYERS }, (_, i) => hex(0xb, i));
const owner = {};
const initial = Object.fromEntries(shards.map(name => [name, {}]));
playerIds.forEach((id, i) => {
  owner[id] = shards[i % HOSTS];
  initial[owner[id]][id] = { x: Math.random() * SIDE, z: Math.random() * SIDE };
});
const assignment = {
  epoch: 1,
  hosts: Object.fromEntries(shards.map((name, k) => [name, hex(0xa, k)])),
  owner,
};

const hosts = shards.map((shard, k) =>
  shardHost(lib, {
    transport: mesh.node(hex(0xa, k)),
    label: LABEL,
    assignment,
    shard,
    roster: playerIds,
    initial: initial[shard],
    apply,
    fogRadius: 60,
  }),
);
const players = playerIds.map(id =>
  joinShards({ transport: mesh.node(id), label: LABEL, assignment, local: { id, predict: apply } }),
);

// Bots: each holds a heading for a while, then turns — back towards the
// middle when it nears a wall, so the arena does not empty into its edges.
const headings = playerIds.map(() => Math.random() * Math.PI * 2);
setInterval(() => {
  players.forEach((player, i) => {
    const at = player.view()[playerIds[i]];
    const nearWall = at !== undefined && (Math.min(at.x, at.z) < 10 || Math.max(at.x, at.z) > SIDE - 10);
    if (nearWall) headings[i] = Math.atan2(SIDE / 2 - at.z, SIDE / 2 - at.x) + (Math.random() - 0.5);
    else if (Math.random() < 0.05) headings[i] = Math.random() * Math.PI * 2;
    player.input({ dx: Math.cos(headings[i]) * 2, dz: Math.sin(headings[i]) * 2 });
  });
}, 50);

const canvas = document.getElementById('arena');
const draw = canvas.getContext('2d');
const hud = document.getElementById('hud');
const me = players[0];
const frame = () => {
  const scale = canvas.width / SIDE;
  draw.clearRect(0, 0, canvas.width, canvas.height);
  const view = me.view();
  for (const [id, body] of Object.entries(view)) {
    draw.fillStyle = COLOURS[shards.indexOf(owner[id])] ?? '#888';
    const size = id === playerIds[0] ? 8 : 5;
    draw.fillRect(body.x * scale - size / 2, body.z * scale - size / 2, size, size);
  }
  const self = view[playerIds[0]];
  if (self !== undefined) {
    draw.strokeStyle = '#888';
    draw.beginPath();
    draw.arc(self.x * scale, self.z * scale, 60 * scale, 0, Math.PI * 2);
    draw.stroke();
  }
  const stats = me.stats();
  hud.textContent = [
    `player 0 sees ${Object.keys(view).length} of ${PLAYERS} (fog 60) · owned by ${owner[playerIds[0]]}`,
    `epoch ${stats.epoch} · dropped not-owner ${stats.droppedNotOwner} · stale-epoch ${stats.droppedStaleEpoch}`,
    ...Object.entries(stats.shards).map(
      ([name, s]) => `${name}: ${s.snapshots} snapshots, silent ${Math.round(s.silentMs)} ms, ${hosts[shards.indexOf(name)].netcode.players().length} peers`,
    ),
  ].join('\n');
  requestAnimationFrame(frame);
};
requestAnimationFrame(frame);
