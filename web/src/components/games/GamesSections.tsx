"use client";

import Link from "next/link";
import { useState } from "react";
import { SectionLabel } from "../SectionLabel";
import { DisplayHeading } from "../DisplayHeading";
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
    body: "Your character moves on the same frame you press the key. The host confirms it in the background.",
  },
  // {
  //   value: "1",
  //   unit: "hop",
  //   title: "player to player",
  //   body: "Players connect directly over WebRTC. There is no game server in the middle adding its own ping.",
  // },
  // {
  //   value: "30",
  //   unit: "/ sec",
  //   title: "world updates",
  //   body: "The default tick rate. Raise it for a twitch shooter, lower it for a strategy game.",
  // },
  {
    value: "~2",
    unit: "ms",
    title: "to update 8,000 npcs",
    body: "Only the entities that changed are checked and sent.",
  },
  // {
  //   value: "16",
  //   unit: "players",
  //   title: "on one host",
  //   body: "Tested with 16 players and 8,000 entities. Host splitting for bigger worlds is supported.",
  // },
  {
    value: "240",
    unit: "kB",
    title: "total download",
    body: "The whole networking engine, gzipped. Smaller than most of your textures.",
  },
];

export function GamesSpeedSection() {
  return (
    <section id="speed" className="border-b border-line px-6 py-20">
      <SectionLabel>§01 / speed</SectionLabel>
      <DisplayHeading>
        it's natively fast
        <br />
        <span className="text-accent">forget the network.</span>
      </DisplayHeading>

      <p className="font-sans text-[18px] text-ink max-w-[700px] leading-[1.55] mb-12">
        Most online game sends every move to a server somewhere and waits
        for the answer. With NET, you see your own moves right away, players
        talk directly to each other. There's no server round-trip.
      </p>

      <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-3 gap-4">
        {SPEED.map((s) => (
          <div
            key={s.title}
            className="game-card relative border border-line bg-bg-2 p-7 transition-colors hover:border-accent-dim group"
          >
            <div className="font-sans text-accent font-semibold leading-none tabular-nums text-[56px] group-hover:game-glow">
              {s.value}
              <span className="text-ink-dim text-[14px] font-mono font-normal ml-2 uppercase tracking-[0.1em]">
                {s.unit}
              </span>
            </div>
            <div className="text-[11px] tracking-[0.16em] uppercase text-ink mt-4 mb-2 font-semibold">
              {s.title}
            </div>
            <p className="font-sans text-ink-dim text-[14px] leading-[1.55]">
              {s.body}
            </p>
          </div>
        ))}
      </div>
    </section>
  );
}

// ── §03 modes ───────────────────────────────────────────────────────────────

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
    body: "One friend hosts in their browser and plays too. The others join. Nobody gets an unfair edge just for hosting.",
    apis: ["hostStore", "joinStore", "hostPlayer"],
    href: "/docs/sdk/browser/store",
  },
  {
    tag: "02",
    title: "Multiplayer",
    players: "8–16 per room",
    body: "Create a lobby, share a room code, and play. Lobbies are found over the network itself, so there is no matchmaking server. Movement stays smooth even when packets drop.",
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
    <section id="modes" className="bg-bg-2 border-b border-line px-6 py-20">
      <SectionLabel>§03 / game modes</SectionLabel>
      <DisplayHeading>
        couch co-op
        <br />
        to <span className="text-accent">open world.</span>
      </DisplayHeading>

      <p className="font-sans text-[18px] text-ink max-w-[700px] leading-[1.55] mb-12">
        One package covers all three. Pick a mode and bring your own gameplay.
      </p>

      <div className="grid grid-cols-1 lg:grid-cols-3 gap-4">
        {MODES.map((m) => (
          <div
            key={m.title}
            className="game-card relative border border-line p-8 bg-bg transition-colors hover:border-accent-dim flex flex-col"
          >
            <div className="flex items-baseline justify-between mb-6">
              <span className="font-display text-[44px] leading-none text-ink-faint">
                {m.tag}
              </span>
              <span className="text-[10px] tracking-[0.14em] uppercase text-bg bg-accent px-2 py-1 font-semibold">
                {m.players}
              </span>
            </div>
            <h3 className="font-sans text-[28px] font-semibold leading-tight mb-3 text-ink">
              {m.title}
            </h3>
            <p className="font-sans text-ink-dim text-[15px] leading-[1.6] mb-6 flex-1">
              {m.body}
            </p>
            <div className="flex flex-wrap gap-1.5 mb-6">
              {m.apis.map((a) => (
                <span
                  key={a}
                  className="text-[10px] text-accent border border-accent-dim px-1.5 py-0.5 font-mono"
                >
                  {a}
                </span>
              ))}
            </div>
            <Link
              href={m.href}
              className="text-[11px] tracking-[0.12em] uppercase text-ink hover:text-accent transition-colors"
            >
              how it works →
            </Link>
          </div>
        ))}
      </div>
    </section>
  );
}

// ── §04 code ────────────────────────────────────────────────────────────────

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
  anchorUrl: 'https://anchor.example.com',
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
  actions: { fire },
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
    <section id="code" className="border-b border-line px-6 py-20">
      <SectionLabel>§04 / add it to your game</SectionLabel>
      <DisplayHeading>
        five steps.
        <br />
        <span className="text-accent">then you&apos;re online.</span>
      </DisplayHeading>

      <p className="font-sans text-[18px] text-ink max-w-[700px] leading-[1.55] mb-12">
        Connect, open a room, share the game state, draw it, move. A few lines
        each, and this is the real API.
      </p>

      <div className="grid grid-cols-1 lg:grid-cols-[240px_1fr] border border-line">
        <div className="flex lg:flex-col overflow-x-auto border-b lg:border-b-0 lg:border-r border-line">
          {SNIPPETS.map((s) => {
            const on = s.key === snippet?.key;
            return (
              <button
                key={s.key}
                type="button"
                onClick={() => setActive(s.key)}
                aria-pressed={on}
                className={`text-left px-5 py-4 text-[11px] tracking-[0.12em] uppercase whitespace-nowrap border-l-2 transition-colors cursor-pointer ${
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
          <div className="min-w-0 bg-[#050706]">
            <div className="flex items-center justify-between border-b border-line px-4 py-2 bg-bg-2/60">
              <span className="font-mono text-[10px] tracking-[0.14em] uppercase text-accent-dim">
                <span className="text-accent">▸</span> {snippet.file}
              </span>
              <CopyButton text={snippet.code} />
            </div>
            <pre className="p-5 text-[12px] leading-[1.6] text-ink overflow-x-auto font-mono min-h-[340px]">
              {snippet.code}
            </pre>
            <div className="border-t border-line px-5 py-4 font-sans text-[14px] text-ink-dim leading-[1.6]">
              <span className="text-accent font-mono">//</span> {snippet.note}
            </div>
          </div>
        ) : null}
      </div>
    </section>
  );
}

// ── §05 any engine ──────────────────────────────────────────────────────────

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
    <section id="engines" className="border-b border-line px-6 py-20">
      <SectionLabel>§05 / any engine</SectionLabel>
      <DisplayHeading>
        a protocol,
        <br />
        <span className="text-accent">not a plugin.</span>
      </DisplayHeading>

      <p className="font-sans text-[18px] text-ink max-w-[700px] leading-[1.55] mb-12">
        NET works with any game engine, because it is a network protocol. How
        much comes ready-made depends on your engine. Three.js gets the full kit
        today.
      </p>

      <div className="grid grid-cols-1 lg:grid-cols-3 gap-4">
        {ENGINE_TIERS.map((t) => (
          <div
            key={t.title}
            className={`game-card relative border p-8 flex flex-col transition-colors ${
              t.highlight
                ? "border-accent-dim bg-accent/[0.04] hover:border-accent"
                : "border-line bg-bg-2 hover:border-accent-dim"
            }`}
          >
            <span
              className={`self-start text-[10px] tracking-[0.14em] uppercase px-2 py-1 font-semibold mb-6 ${
                t.highlight
                  ? "text-bg bg-accent"
                  : "text-accent border border-accent-dim"
              }`}
            >
              {t.level}
            </span>
            <h3 className="font-sans text-[26px] font-semibold leading-tight mb-1 text-ink">
              {t.title}
            </h3>
            <div className="text-[11px] tracking-[0.06em] text-ink-dim mb-4">
              {t.examples}
            </div>
            <p className="font-sans text-ink-dim text-[15px] leading-[1.6] mb-6 flex-1">
              {t.body}
            </p>
            <ul className="flex flex-col gap-1.5 mb-6 text-[11px] tracking-[0.1em] uppercase">
              {t.gets.map((g) => (
                <li key={g} className="text-ink">
                  <span className="text-accent mr-2">✓</span>
                  {g}
                </li>
              ))}
            </ul>
            <Link
              href={t.href}
              className="text-[11px] tracking-[0.12em] uppercase text-ink hover:text-accent transition-colors"
            >
              read the docs →
            </Link>
          </div>
        ))}
      </div>
    </section>
  );
}

// ── §06 AI-generated games ──────────────────────────────────────────────────

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
    <section id="ai" className="bg-bg-2 border-b border-line px-6 py-20">
      <SectionLabel>§06 / made for ai-built games</SectionLabel>
      <DisplayHeading>
        prompt the game.
        <br />
        <span className="text-accent">prompt the multiplayer.</span>
      </DisplayHeading>

      <div className="grid grid-cols-1 lg:grid-cols-2 gap-10 mt-6">
        <div className="flex flex-col gap-6">
          {AI_POINTS.map((p, i) => (
            <div key={p.title} className="flex gap-5">
              <span className="font-display text-[28px] text-accent leading-none w-8 shrink-0">
                {i + 1}
              </span>
              <div>
                <h3 className="font-sans text-[18px] font-semibold text-ink mb-1.5">
                  {p.title}
                </h3>
                <p className="font-sans text-ink-dim text-[15px] leading-[1.6]">
                  {p.body}
                </p>
              </div>
            </div>
          ))}
        </div>

        <div className="relative border border-line bg-bg self-start">
          <div className="flex items-center justify-between border-b border-line px-4 py-2 bg-bg-2/60">
            <span className="font-mono text-[10px] tracking-[0.14em] uppercase text-accent-dim">
              <span className="text-accent">▸</span> paste this into your ai
            </span>
            <CopyButton text={PROMPT} />
          </div>
          <pre className="p-5 text-[12px] leading-[1.7] text-ink whitespace-pre-wrap font-mono">
            <span className="text-accent">&gt; </span>
            {PROMPT}
          </pre>
        </div>
      </div>
    </section>
  );
}

// ── §07 built in ────────────────────────────────────────────────────────────

const PROPS: ReadonlyArray<{ k: string; title: string; body: string }> = [
  {
    k: "01",
    title: "Cheat-resistant",
    body: "Players send what they want to do. The host decides what actually happens.",
  },
  {
    k: "02",
    title: "No wallhacks",
    body: "Hidden data is never sent to players who shouldn't see it, so there is nothing to dig out.",
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
    body: "Every connection runs over an encrypted Noise session, from the first packet.",
  },
];

export function GamesPropsSection() {
  return (
    <section id="properties" className="border-b border-line px-6 py-20">
      <SectionLabel>§07 / built in</SectionLabel>
      <DisplayHeading>
        the hard parts,
        <br />
        <span className="text-accent">already done.</span>
      </DisplayHeading>

      <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-3 border-t border-l border-line mt-12">
        {PROPS.map((p) => (
          <div
            key={p.k}
            className="border-r border-b border-line p-7 transition-colors hover:bg-bg-2"
          >
            <div className="text-[10px] tracking-[0.15em] text-accent-dim mb-3 font-mono">
              [{p.k}]
            </div>
            <h3 className="font-sans text-[20px] font-semibold leading-tight mb-2 text-ink">
              {p.title}
            </h3>
            <p className="font-sans text-ink-dim text-[14px] leading-[1.6]">
              {p.body}
            </p>
          </div>
        ))}
      </div>
    </section>
  );
}

// ── §08 open source ─────────────────────────────────────────────────────────

export function GamesOpenSection() {
  const [copied, setCopied] = useState(false);
  const cmd = "npm install @net-mesh/browser";

  return (
    <section id="install" className="bg-bg-2 border-b border-line px-6 py-20">
      <SectionLabel>§08 / open source</SectionLabel>
      <DisplayHeading>
        free to use.
        <br />
        <span className="text-accent">free to ship.</span>
      </DisplayHeading>

      <p className="font-sans text-[18px] text-ink max-w-[700px] leading-[1.55] mb-12">
        NET is an open protocol, licensed MIT or Apache-2.0. Use it in a game
        jam, a weekend project or a commercial release. No sign-up, no keys, no
        per-player pricing.
      </p>

      <div className="grid grid-cols-1 md:grid-cols-3 gap-4">
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
          className="game-card text-left border border-line p-6 bg-bg transition-colors hover:border-accent-dim cursor-pointer focus:outline-none focus:border-accent md:col-span-2"
        >
          <div className="flex items-center justify-between mb-4">
            <span className="text-[11px] text-ink tracking-[0.15em] uppercase font-semibold">
              Install
            </span>
            <span
              className={`text-[10px] px-1.5 py-0.5 transition-colors ${
                copied
                  ? "text-bg bg-accent border border-accent font-semibold"
                  : "text-accent border border-accent-dim"
              }`}
            >
              {copied ? "✓ COPIED" : "CLICK TO COPY"}
            </span>
          </div>
          <pre className="bg-bg-2 p-4 text-[14px] text-accent border-l-2 border-accent overflow-x-auto font-mono leading-[1.5]">
            $ {cmd}
          </pre>
          <div className="font-sans text-ink-dim text-[13px] mt-3">
            Players get their pass from a small anchor you run with{" "}
            <span className="text-ink font-mono text-[12px]">
              net-mesh anchor serve
            </span>
            . It holds no game state and one anchor serves many games.
          </div>
        </button>

        <div className="border border-line p-6 bg-bg flex flex-col gap-3.5 text-[11px] tracking-[0.1em] uppercase">
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

      <div className="relative mt-16 text-center py-16 border-t border-b border-accent-dim bg-accent/[0.02]">
        <div
          className="font-display text-ink leading-[1.1] mb-5"
          style={{ fontSize: "clamp(28px, 4vw, 48px)" }}
        >
          player two has <span className="text-accent game-glow">joined.</span>
        </div>
        <Link
          href="/docs/sdk/browser/quickstart"
          className="btn-primary game-btn inline-flex items-center gap-2.5 px-6 py-3.5 text-[12px] tracking-[0.14em] uppercase font-semibold no-underline border border-accent bg-accent text-bg transition-all mt-5"
        >
          ▶ Start building
        </Link>
      </div>
    </section>
  );
}
