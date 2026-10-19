"use client";

import Link from "next/link";
import { useState } from "react";
import { FieldLabel as SectionLabel } from "../field/FieldLabel";
import { FieldHeading as DisplayHeading } from "../field/FieldHeading";
import { Lede } from "../field/Lede";
import { CopyButton } from "../CopyButton";
import globals from "@/lib/globals";

// ── §01 speed ───────────────────────────────────────────────────────────────

interface SpeedStat {
  value: string;
  unit: string;
  title: string;
  body: string;
}

const SPEED: ReadonlyArray<SpeedStat> = [
  {
    value: "0",
    unit: "ms",
    title: "input delay",
    body: "Your own moves are predicted on the same frame you press the key. The host confirms them in the background.",
  },
  {
    value: "~5",
    unit: "ms",
    title: "host tick, 8,000 entities",
    body: "A benchmark, not a game: one machine, 16 players, 5% of the world moving each tick. Only the entities that changed are checked, and each player is sent only what is near them.",
  },
  {
    value: "~240",
    unit: "kB",
    title: "total download",
    body: "The whole networking engine, WebAssembly and JavaScript, gzipped. Smaller than most of your textures.",
  },
];

export function GamesSpeedSection() {
  return (
    <section id="speed" className="relative px-6 lg:px-12 py-24 lg:py-32">
      <SectionLabel>§01 / speed</SectionLabel>
      <DisplayHeading>
        natively fast.
        <br />
        <span className="text-accent">forget the network.</span>
      </DisplayHeading>

      <Lede>
        Most online games send every move to a server somewhere and wait for the
        answer. With NET, you see your own moves right away, and players link
        directly to each other whenever the network allows. No game server runs
        the match.
      </Lede>

      <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-3 gap-5 lg:gap-6">
        {SPEED.map((s) => (
          <div key={s.title} className="slab slab-lift p-7">
            <div className="font-display font-bold text-accent leading-none tabular-nums text-[56px] tracking-[-0.035em]">
              {s.value}
              <span className="text-ink-dim text-[13px] font-mono font-normal ml-2 uppercase tracking-[0.1em]">
                {s.unit}
              </span>
            </div>
            <div className="text-[12px] tracking-[0.16em] uppercase text-ink mt-4 mb-2 font-semibold">
              {s.title}
            </div>
            <p className="text-ink-dim text-[15px] leading-[1.6]">{s.body}</p>
          </div>
        ))}
      </div>
    </section>
  );
}

// ── §04 modes ───────────────────────────────────────────────────────────────

interface Mode {
  tag: string;
  title: string;
  players: string;
  body: string;
  apis: ReadonlyArray<string>;
  href: string;
}

const MODES: ReadonlyArray<Mode> = [
  {
    tag: "01",
    title: "Co-op",
    players: "2–4 players",
    body: "One friend hosts in their browser and plays too. The others join. The host plays through the same view as everyone else.",
    apis: ["hostStore", "joinStore", "hostPlayer"],
    href: "/docs/sdk/browser/store",
  },
  {
    tag: "02",
    title: "Multiplayer",
    players: "128 on one host",
    body: "Create a lobby, share a room code, and play. Lobbies are found over the network itself, so there is no matchmaking server. Movement stays smooth even when packets drop. One host held 128 players in testing; past that, split the match across hosts.",
    apis: ["createLobby", "joinLobby", "joinNetcode"],
    href: "/docs/sdk/browser/netcode",
  },
  {
    tag: "03",
    title: "MMO worlds",
    players: "many hosts, one map",
    body: "Split a big map into regions, each run by a different host. Players only receive what is near them, and characters cross region borders without popping or duplicating.",
    apis: ["joinWorld", "cellsAround", "regionHandoffs"],
    href: "/docs/sdk/browser/world",
  },
];

export function GamesModesSection() {
  return (
    <section
      id="modes"
      className="bg-bg-2 relative px-6 lg:px-12 py-24 lg:py-32"
    >
      <SectionLabel>§04 / game modes</SectionLabel>
      <DisplayHeading>
        couch co-op
        <br />
        to <span className="text-accent">open world.</span>
      </DisplayHeading>

      <Lede>
        One package covers all three. Pick a mode and bring your own gameplay.
      </Lede>

      <div className="grid grid-cols-1 lg:grid-cols-3 gap-5 lg:gap-6">
        {MODES.map((m) => (
          <div key={m.title} className="slab slab-lift p-8 flex flex-col">
            <div className="flex items-baseline justify-between mb-6">
              <span className="font-display font-bold text-[44px] leading-none text-ink-faint tabular-nums">
                {m.tag}
              </span>
              <span className="text-[12px] tracking-[0.14em] uppercase text-bg bg-accent px-2 py-1 font-semibold">
                {m.players}
              </span>
            </div>
            <h3 className="font-display font-bold tracking-[-0.015em] text-[28px] leading-[1.05] mb-3 text-ink">
              {m.title}
            </h3>
            <p className="text-ink-dim text-[15px] leading-[1.6] mb-6 flex-1">
              {m.body}
            </p>
            <div className="flex flex-wrap gap-1.5 mb-6">
              {m.apis.map((a) => (
                <span
                  key={a}
                  className="text-[12px] text-accent border border-accent-dim px-1.5 py-0.5 font-mono"
                >
                  {a}
                </span>
              ))}
            </div>
            <Link
              href={m.href}
              className="self-start font-mono text-[12px] tracking-[0.16em] uppercase text-ink hover:text-accent transition-colors"
            >
              how it works →
            </Link>
          </div>
        ))}
      </div>
    </section>
  );
}

// ── §05 many hosts ──────────────────────────────────────────────────────────

const SHARD_POINTS: ReadonlyArray<{ title: string; body: string }> = [
  {
    title: "Each host runs its own part",
    body: "A host owns some of the players and NPCs, and runs their AI. Every player joins every host, and NET merges them into one view of one world.",
  },
  {
    title: "Hits across hosts are fair",
    body: "A hit on someone run by another host is checked against that host's record of where the attacker really was.",
  },
  {
    title: "Players move between hosts",
    body: "When a player is handed to another host, an input the old host already applied is never applied twice.",
  },
  {
    title: "A host drops, the match goes on",
    body: "The remaining hosts take over the lost host's players from where they last saw them.",
  },
];

const SHARD_STATS: ReadonlyArray<{ value: string; label: string }> = [
  { value: "128", label: "players on one host, measured" },
  { value: "~30×", label: "less traffic for NPCs on known paths" },
  { value: "~7×", label: "smaller snapshots with a codec" },
];

export function GamesShardsSection() {
  return (
    <section id="hosts" className="relative px-6 lg:px-12 py-24 lg:py-32">
      <SectionLabel>§05 / many hosts</SectionLabel>
      <DisplayHeading>
        one match.
        <br />
        <span className="text-accent">many hosts.</span>
      </DisplayHeading>

      <Lede>
        One player doesn&apos;t have to carry the whole match. NET can split it
        across several hosts in the same game, each running its own part, so no
        single page does all the work.
      </Lede>

      <div className="grid grid-cols-1 lg:grid-cols-[1.1fr_1fr] gap-10 items-start">
        <figure className="slab p-5 m-0">
          <ShardDiagram />
          <figcaption className="font-sans text-[15px] text-ink-dim leading-[1.6] mt-4 border-t border-line pt-4">
            You fight three enemies. Another player fights three more, 50 m
            away. Each of you hosts your own fight and runs its AI. Your page
            doesn&apos;t run the far fight: it just draws what the other host
            sends.
          </figcaption>
        </figure>

        <div className="flex flex-col gap-6">
          {SHARD_POINTS.map((p, i) => (
            <div key={p.title} className="flex gap-5">
              <span className="font-display font-bold text-[28px] text-accent leading-none w-8 shrink-0 tabular-nums">
                {i + 1}
              </span>
              <div>
                <h3 className="font-display font-bold tracking-[-0.015em] text-[21px] leading-[1.15] text-ink mb-1.5">
                  {p.title}
                </h3>
                <p className="text-ink-dim text-[15px] leading-[1.6]">
                  {p.body}
                </p>
              </div>
            </div>
          ))}
          <div className="flex flex-wrap gap-1.5">
            {["joinShards", "hostNetcode", "sampleAt"].map((a) => (
              <span
                key={a}
                className="text-[12px] text-accent border border-accent-dim px-1.5 py-0.5 font-mono"
              >
                {a}
              </span>
            ))}
          </div>
          <Link
            href="/docs/sdk/browser/netcode#several-hosts-a-sharded-match"
            className="self-start font-mono text-[12px] tracking-[0.16em] uppercase text-ink hover:text-accent transition-colors"
          >
            how sharding works →
          </Link>
        </div>
      </div>

      <div className="grid grid-cols-1 sm:grid-cols-3 border-t border-l border-line mt-12">
        {SHARD_STATS.map((s) => (
          <div
            key={s.label}
            className="border-r border-b border-line px-6 py-6"
          >
            <div className="font-display font-bold text-accent text-[38px] tracking-[-0.03em] leading-none tabular-nums">
              {s.value}
            </div>
            <div className="text-[12px] tracking-[0.14em] uppercase text-ink-dim mt-2">
              {s.label}
            </div>
          </div>
        ))}
      </div>
    </section>
  );
}

// Two fights, each run by its own host, linked by the snapshots they trade.
function ShardDiagram() {
  const zones = [
    { cx: 130, label: "host a", who: "you" },
    { cx: 430, label: "host b", who: "another player" },
  ];
  const enemies = [
    [-46, -30],
    [44, -36],
    [10, 48],
  ];

  return (
    <svg
      viewBox="0 0 560 260"
      className="w-full h-auto"
      role="img"
      aria-label="Two hosts 50 metres apart, each running its own fight of one player against three enemies, sending each other snapshots"
    >
      <line
        x1="215"
        y1="120"
        x2="345"
        y2="120"
        className="stroke-accent-dim"
        strokeWidth="1.5"
        strokeDasharray="5 5"
      />
      <polygon points="345,120 337,115 337,125" className="fill-accent-dim" />
      <polygon points="215,120 223,115 223,125" className="fill-accent-dim" />
      <text
        x="280"
        y="108"
        textAnchor="middle"
        className="fill-ink-dim font-mono"
        fontSize="10"
        letterSpacing="1.5"
      >
        SNAPSHOTS
      </text>
      <text
        x="280"
        y="140"
        textAnchor="middle"
        className="fill-ink-dim font-mono"
        fontSize="10"
        letterSpacing="1.5"
      >
        50 M
      </text>

      {zones.map((z) => (
        <g key={z.label}>
          <circle
            cx={z.cx}
            cy="120"
            r="84"
            className="stroke-line fill-accent/[0.03]"
            strokeWidth="1"
            strokeDasharray="3 4"
          />
          {enemies.map(([dx, dy]) => (
            <g key={`${dx},${dy}`}>
              <line
                x1={z.cx}
                y1="120"
                x2={z.cx + dx}
                y2={120 + dy}
                className="stroke-warn/40"
                strokeWidth="1"
              />
              <circle
                cx={z.cx + dx}
                cy={120 + dy}
                r="6"
                className="fill-warn"
              />
            </g>
          ))}
          <circle cx={z.cx} cy="120" r="9" className="fill-accent" />
          <text
            x={z.cx}
            y="226"
            textAnchor="middle"
            className="fill-accent font-mono"
            fontSize="11"
            letterSpacing="2"
          >
            {z.label.toUpperCase()}
          </text>
          <text
            x={z.cx}
            y="244"
            textAnchor="middle"
            className="fill-ink-dim font-mono"
            fontSize="10"
            letterSpacing="1"
          >
            {z.who} + 3 enemies
          </text>
        </g>
      ))}
    </svg>
  );
}

// ── §06 code ────────────────────────────────────────────────────────────────

interface Snippet {
  key: string;
  label: string;
  file: string;
  note: string;
  code: string;
}

const SNIPPETS: ReadonlyArray<Snippet> = [
  {
    key: "connect",
    label: "1 · connect",
    file: "net.ts",
    note: "Each player gets a free, anonymous pass. Returning players are remembered.",
    code: `import { connect, rememberedIdentity, requestCredential }
  from '@net-mesh/browser';

const { credentialB64, bootstrapUrl } = await requestCredential({
  anchorUrl: 'https://anchor.ai2070.net', // NET's public anchor
  game: 'my-game',
});

const node = await connect({
  ...rememberedIdentity(),
  credentialB64,
  bootstrapUrl,
});`,
  },
  {
    key: "lobby",
    label: "2 · lobby",
    file: "lobby.ts",
    note: "Rooms are found over the network itself. No matchmaking server.",
    code: `import { createLobby, listLobbies, joinLobby }
  from '@net-mesh/browser';

// the host
const lobby = await createLobby({
  node, game: 'arena', name: 'Friday arena', capacity: 8,
  definition, initialState, project, actions, inputs,
});
console.log('room code:', lobby.code);

// everyone else
const lobbies = await listLobbies({ node, game: 'arena' });
const world = await joinLobby({
  node, definition, game: 'arena', code: lobby.code,
});`,
  },
  {
    key: "store",
    label: "3 · game state",
    file: "store.ts",
    note: "The host checks every action. Players can't fake who they are or peek at hidden data.",
    code: `import { defineStore } from '@net-mesh/browser';

const definition = defineStore({
  id: 'arena', version: 1, state, empty,
  actions: { fire: { input: parseFire, output: parseHull } },
  inputs: {},
  visibility: { 'players.*.hand': 'owner' }, // secrets stay secret
});

// on the host
const actions = {
  fire: (input, context) => {
    const target = context.getState().ships[input.at];
    if (target === undefined) throw new Error('no such ship');
    const hull = Math.max(0, target.hull - 25);
    context.setState({ ships: {
      ...context.getState().ships,
      [input.at]: { ...target, hull },
    }});
    return { hull };
  },
};

// on any player
await world.act('fire', { at: enemyId });`,
  },
  {
    key: "three",
    label: "4 · three.js",
    file: "scene.ts",
    note: "Meshes are added, moved and cleaned up for you. Only the ones that changed are touched.",
    code: `import * as THREE from 'three';
import { bindEntities } from '@net-mesh/browser/three';

bindEntities({
  store: world,              // a hosted store or a joined replica
  scene,
  select: state => state.ships,
  binding: {
    create: (ship, id) => buildShip(ship, id),
    update: (mesh, ship) => {
      mesh.position.set(ship.x, 0, ship.z);
      mesh.rotation.y = ship.heading;
    },
    remove: mesh => disposeOf(mesh),
  },
});`,
  },
  {
    key: "netcode",
    label: "5 · movement",
    file: "movement.ts",
    note: "Your moves show instantly. Everyone else is smoothed, so lag spikes don't make them jump.",
    code: `import { joinNetcode } from '@net-mesh/browser/netcode';

const net = joinNetcode<Ship, Move>({
  transport: node,
  host: hostNodeId,
  label: 'my-game.movement',
  local: { id: node.nodeIdHex()!, predict: move }, // same rule as host
  interpolationDelayMs: 100,
});

onKey(input => net.input(input));   // moves now, host gets it too

function frame() {
  draw(net.view());
  requestAnimationFrame(frame);
}`,
  },
];

export function GamesCodeSection() {
  const [active, setActive] = useState<string>(SNIPPETS[0]?.key ?? "");
  const snippet = SNIPPETS.find((s) => s.key === active) ?? SNIPPETS[0];

  return (
    <section id="code" className="relative px-6 lg:px-12 py-24 lg:py-32">
      <SectionLabel>§06 / add it to your game</SectionLabel>
      <DisplayHeading>
        five steps.
        <br />
        <span className="text-accent">then you&apos;re online.</span>
      </DisplayHeading>

      <Lede>
        Connect, open a room, share the game state, draw it, move. A few lines
        each, and this is the real API.
      </Lede>

      <div data-ui className="slab grid grid-cols-1 lg:grid-cols-[240px_1fr]">
        <div className="flex lg:flex-col overflow-x-auto border-b lg:border-b-0 lg:border-r border-line">
          {SNIPPETS.map((s) => {
            const on = s.key === snippet?.key;
            return (
              <button
                key={s.key}
                type="button"
                onClick={() => setActive(s.key)}
                aria-pressed={on}
                className={`text-left px-5 py-4 text-[12px] tracking-[0.12em] uppercase whitespace-nowrap border-l-2 transition-colors cursor-pointer ${
                  on
                    ? "border-accent text-accent bg-accent/[0.05] font-semibold"
                    : "border-transparent text-ink-dim hover:text-ink hover:bg-bg-2"
                }`}
              >
                {s.label}
              </button>
            );
          })}
        </div>

        {snippet ? (
          <div className="min-w-0 bg-black/30">
            <div className="flex items-center justify-between border-b border-line px-4 py-2 bg-bg-2/60">
              <span className="font-mono text-[12px] tracking-[0.14em] uppercase text-accent-dim">
                <span className="text-accent">▸</span> {snippet.file}
              </span>
              <CopyButton text={snippet.code} />
            </div>
            <pre className="p-5 text-[13px] leading-[1.65] text-ink overflow-x-auto font-mono min-h-[360px]">
              {snippet.code}
            </pre>
            <div className="border-t border-line px-5 py-4 font-sans text-[15px] text-ink-dim leading-[1.6]">
              <span className="text-accent font-mono">{"//"}</span>{" "}
              {snippet.note}
            </div>
          </div>
        ) : null}
      </div>
    </section>
  );
}

// ── §07 any engine ──────────────────────────────────────────────────────────

interface EngineTier {
  level: string;
  title: string;
  examples: string;
  body: string;
  gets: ReadonlyArray<string>;
  href: string;
  highlight?: boolean;
}

const ENGINE_TIERS: ReadonlyArray<EngineTier> = [
  {
    level: "plug and play",
    title: "Three.js",
    examples: "including AI-generated games",
    body: "Everything on this page, ready to use. Lobbies, shared game state, smooth movement, and a scene binding that adds, moves and removes your meshes for you.",
    gets: ["lobbies", "game state", "netcode", "scene binding"],
    href: "/docs/sdk/browser/three",
    highlight: true,
  },
  {
    level: "almost plug and play",
    title: "Other web engines",
    examples: "e.g. Babylon.js, PlayCanvas, Phaser, plain canvas",
    body: "The same package works in any browser game. You get the lobbies, game state and netcode as-is, and write the few lines that draw entities in your engine.",
    gets: ["lobbies", "game state", "netcode"],
    href: "/docs/sdk/browser",
  },
  {
    level: "speaks the protocol",
    title: "Any other engine",
    examples: "e.g. Unity, Unreal, Godot, custom engines",
    body: "NET is an open protocol, not a web library. Native games connect through the Rust, Go, Python or C SDKs and get encrypted connections, channels, streams and RPC. The game layer on top is yours to build for now.",
    gets: ["encrypted mesh", "channels", "streams", "rpc"],
    href: "/docs",
  },
];

export function GamesEnginesSection() {
  return (
    <section id="engines" className="relative px-6 lg:px-12 py-24 lg:py-32">
      <SectionLabel>§07 / any engine</SectionLabel>
      <DisplayHeading>
        a protocol,
        <br />
        <span className="text-accent">not a plugin.</span>
      </DisplayHeading>

      <Lede>
        NET works with any game engine, because it is a network protocol. How
        much comes ready-made depends on your engine. Three.js gets the full kit
        today.
      </Lede>

      <div className="grid grid-cols-1 lg:grid-cols-3 gap-5 lg:gap-6">
        {ENGINE_TIERS.map((t) => (
          <div
            key={t.title}
            className={`slab p-8 flex flex-col ${
              t.highlight ? "slab-held" : "slab-lift"
            }`}
          >
            <span
              className={`self-start text-[12px] tracking-[0.14em] uppercase px-2 py-1 font-semibold mb-6 ${
                t.highlight
                  ? "text-bg bg-accent"
                  : "text-accent border border-accent-dim"
              }`}
            >
              {t.level}
            </span>
            <h3 className="font-display font-bold tracking-[-0.015em] text-[28px] leading-[1.05] mb-1.5 text-ink">
              {t.title}
            </h3>
            <div className="text-[12px] tracking-[0.06em] text-ink-dim mb-4">
              {t.examples}
            </div>
            <p className="text-ink-dim text-[15px] leading-[1.6] mb-6 flex-1">
              {t.body}
            </p>
            <ul className="flex flex-col gap-1.5 mb-6 text-[12px] tracking-[0.1em] uppercase">
              {t.gets.map((g) => (
                <li key={g} className="text-ink">
                  <span className="text-accent mr-2">✓</span>
                  {g}
                </li>
              ))}
            </ul>
            <Link
              href={t.href}
              className="self-start font-mono text-[12px] tracking-[0.16em] uppercase text-ink hover:text-accent transition-colors"
            >
              read the docs →
            </Link>
          </div>
        ))}
      </div>
    </section>
  );
}

// ── §08 AI-generated games ──────────────────────────────────────────────────

// Absolute URL: the prompt is pasted into an agent outside this site. The
// browser SDK overview links every page an agent needs, quickstart first.
const PROMPT = `Make this Three.js game multiplayer with NET (@net-mesh/browser).
Follow the docs: ${globals.site.href}/docs/sdk/browser`;

const AI_POINTS: ReadonlyArray<{ title: string; body: string }> = [
  {
    title: "Hard to get wrong",
    body: "Typos become compile errors instead of silent bugs, and every error has a clear name. AI-written code tends to work on the first try.",
  },
  {
    title: "Comes with an agent skill",
    body: "The repo ships a net-browser skill that teaches Claude Code and other coding agents how to use the package properly.",
  },
  {
    title: "Test without a network",
    body: "createLocalMesh() runs several players in one page. Build the whole game offline, then switch to a real connection.",
  },
];

export function GamesAiSection() {
  return (
    <section id="ai" className="bg-bg-2 relative px-6 lg:px-12 py-24 lg:py-32">
      <SectionLabel>§08 / made for ai-built games</SectionLabel>
      <DisplayHeading>
        prompt the game.
        <br />
        <span className="text-accent">prompt the multiplayer.</span>
      </DisplayHeading>

      <div className="grid grid-cols-1 lg:grid-cols-2 gap-10 mt-6">
        <div className="flex flex-col gap-6">
          {AI_POINTS.map((p, i) => (
            <div key={p.title} className="flex gap-5">
              <span className="font-display font-bold text-[28px] text-accent leading-none w-8 shrink-0 tabular-nums">
                {i + 1}
              </span>
              <div>
                <h3 className="font-display font-bold tracking-[-0.015em] text-[21px] leading-[1.15] text-ink mb-1.5">
                  {p.title}
                </h3>
                <p className="text-ink-dim text-[15px] leading-[1.6]">
                  {p.body}
                </p>
              </div>
            </div>
          ))}
        </div>

        <div data-ui className="slab self-start">
          <div className="flex items-center justify-between border-b border-line px-4 py-2 bg-bg-2/60">
            <span className="font-mono text-[12px] tracking-[0.14em] uppercase text-accent-dim">
              <span className="text-accent">▸</span> paste this into your ai
            </span>
            <CopyButton text={PROMPT} />
          </div>
          <pre className="p-5 text-[14px] leading-[1.7] text-ink whitespace-pre-wrap font-mono">
            <span className="text-accent">&gt; </span>
            {PROMPT}
          </pre>
        </div>
      </div>
    </section>
  );
}

// ── §09 built in ────────────────────────────────────────────────────────────

const PROPS: ReadonlyArray<{ k: string; title: string; body: string }> = [
  {
    k: "01",
    title: "Cheat-resistant",
    body: "Players send what they want to do. The host decides what actually happens.",
  },
  {
    k: "02",
    title: "No wallhacks",
    body: "Hidden data is never sent to players who shouldn't see it, so there is nothing to dig out. (The host's own page holds everything; for real stakes, run a dedicated host.)",
  },
  {
    k: "03",
    title: "No impersonation",
    body: "Every player's identity is proven by the encrypted connection. Nobody can pretend to be someone else.",
  },
  {
    k: "04",
    title: "Fair hit detection",
    body: "Shots are checked against what the shooter actually saw, capped at 200 ms so faking lag doesn't help.",
  },
  {
    k: "05",
    title: "Smooth under packet loss",
    body: "Movement uses a fast lane that skips late packets instead of waiting for them.",
  },
  {
    k: "06",
    title: "Encrypted end to end",
    body: "Every NET packet travels inside an encrypted Noise session, even when the anchor has to relay it.",
  },
];

export function GamesPropsSection() {
  return (
    <section id="properties" className="relative px-6 lg:px-12 py-24 lg:py-32">
      <SectionLabel>§09 / built in</SectionLabel>
      <DisplayHeading>
        the hard parts,
        <br />
        <span className="text-accent">already done.</span>
      </DisplayHeading>

      <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-3 gap-5 lg:gap-6 mt-12">
        {PROPS.map((p) => (
          <div key={p.k} className="slab slab-lift p-7">
            <h3 className="font-display font-bold tracking-[-0.015em] text-[22px] leading-[1.1] mb-2 text-ink">
              {p.title}
            </h3>
            <p className="text-ink-dim text-[15px] leading-[1.6]">{p.body}</p>
          </div>
        ))}
      </div>
    </section>
  );
}

// ── §10 open source ─────────────────────────────────────────────────────────

export function GamesOpenSection() {
  const [copied, setCopied] = useState(false);
  const cmd = "npm install @net-mesh/browser";

  return (
    <section
      id="install"
      className="bg-bg-2 relative px-6 lg:px-12 py-24 lg:py-32"
    >
      <SectionLabel>§10 / open source</SectionLabel>
      <DisplayHeading>
        free to use.
        <br />
        <span className="text-accent">free to ship.</span>
      </DisplayHeading>

      <Lede>
        NET is an open protocol, licensed MIT or Apache-2.0. Use it in a game
        jam, a weekend project or a commercial release. No sign-up, no keys, no
        per-player pricing.
      </Lede>

      <div className="grid grid-cols-1 md:grid-cols-3 gap-5 lg:gap-6">
        <button
          type="button"
          onClick={async () => {
            try {
              await navigator.clipboard.writeText(cmd);
              setCopied(true);
              window.setTimeout(() => setCopied(false), 1800);
            } catch {
              // clipboard API can fail in insecure contexts; ignore silently
            }
          }}
          aria-label="Copy install command"
          className="slab slab-lift text-left p-6 cursor-pointer focus:outline-none md:col-span-2"
        >
          <div className="flex items-center justify-between mb-4">
            <span className="text-[12px] text-ink tracking-[0.15em] uppercase font-semibold">
              Install
            </span>
            <span
              className={`text-[12px] px-1.5 py-0.5 transition-colors ${
                copied
                  ? "text-bg bg-accent border border-accent font-semibold"
                  : "text-accent border border-accent-dim"
              }`}
            >
              {copied ? "✓ COPIED" : "CLICK TO COPY"}
            </span>
          </div>
          <pre className="bg-black/40 p-4 text-[15px] text-accent overflow-x-auto font-mono leading-[1.5] shadow-[inset_0_2px_6px_rgba(0,0,0,0.6)]">
            $ {cmd}
          </pre>
          <div className="font-sans text-ink-dim text-[15px] leading-[1.6] mt-3">
            Players get their pass from NET&apos;s public anchor,{" "}
            <span className="text-ink font-mono text-[14px]">
              anchor.ai2070.net
            </span>
            , with no setup. Or run your own with{" "}
            <span className="text-ink font-mono text-[14px]">
              net-mesh anchor serve
            </span>
            . An anchor holds no game state, and one serves many games.
          </div>
        </button>

        <div className="slab p-6 flex flex-col gap-4 font-mono text-[12px] tracking-[0.14em] uppercase">
          <Link
            href="/docs/sdk/browser/quickstart"
            className="text-ink hover:text-accent transition-colors"
          >
            ▸ quickstart →
          </Link>
          <Link
            href="/docs/sdk/browser/store"
            className="text-ink hover:text-accent transition-colors"
          >
            ▸ game state →
          </Link>
          <Link
            href="/docs/sdk/browser/three"
            className="text-ink hover:text-accent transition-colors"
          >
            ▸ three.js →
          </Link>
          <Link
            href="/docs/sdk/browser/netcode"
            className="text-ink hover:text-accent transition-colors"
          >
            ▸ netcode →
          </Link>
          <a
            href="https://github.com/ai-2070/net"
            target="_blank"
            rel="noopener noreferrer"
            className="text-ink hover:text-accent transition-colors"
          >
            ▸ source // github ↗
          </a>
        </div>
      </div>

      <div className="relative mt-24 -mx-6 lg:-mx-12 -mb-24 lg:-mb-32 text-center px-6 py-24 md:py-32 border-t border-line">
        <div
          className="font-display font-bold uppercase tracking-[-0.03em] text-ink leading-[0.95] mb-5"
          style={{ fontSize: "clamp(34px, 5.4vw, 80px)" }}
        >
          player two has <span className="text-accent">joined.</span>
        </div>
        <Link href="/docs/sdk/browser/quickstart" className="btn-field mt-5">
          ▶ Start building
        </Link>
      </div>
    </section>
  );
}
