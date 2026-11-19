"use client";

import { FieldLabel as SectionLabel } from "../field/FieldLabel";
import { FieldHeading as DisplayHeading } from "../field/FieldHeading";
import { Lede } from "../field/Lede";
import { CopyButton } from "../CopyButton";
import { Shot } from "./Shot";
import { ModelRoster } from "./ModelRoster";
import {
  ATHENA_DOWNLOAD_URL,
  ATHENA_GUIDE_URL,
  ATHENA_REPO_URL,
} from "./links";

// Every fact on this page comes from the Athena Agents README, its guide and
// the 1.0 release notes. Keep them in step when any of them changes. The
// screenshots are the README's own (docs/images), from its made-up coffee
// shop, Fernwood Coffee.

type Point = { title: string; body: string };

function Numbered({ points }: { points: ReadonlyArray<Point> }) {
  return (
    <div className="flex flex-col gap-6">
      {points.map((p, i) => (
        <div key={p.title} className="flex gap-5">
          <span className="font-display font-bold text-[28px] text-accent leading-none w-8 shrink-0 tabular-nums">
            {i + 1}
          </span>
          <div>
            <h3 className="font-display font-bold tracking-[-0.015em] text-[21px] leading-[1.15] text-ink mb-1.5">
              {p.title}
            </h3>
            <p className="text-ink-dim text-[15px] leading-[1.6]">{p.body}</p>
          </div>
        </div>
      ))}
    </div>
  );
}

function Cards({
  items,
  cols = "lg:grid-cols-3",
  size = "text-[24px]",
}: {
  items: ReadonlyArray<Point>;
  cols?: string;
  size?: string;
}) {
  return (
    <div className={`grid grid-cols-1 sm:grid-cols-2 ${cols} gap-5 lg:gap-6`}>
      {items.map((p) => (
        <div key={p.title} className="slab slab-lift p-7">
          <h3
            className={`font-display font-bold tracking-[-0.015em] ${size} leading-[1.1] mb-2 text-ink`}
          >
            {p.title}
          </h3>
          <p className="text-ink-dim text-[15px] leading-[1.6]">{p.body}</p>
        </div>
      ))}
    </div>
  );
}

function Tags({ items }: { items: ReadonlyArray<string> }) {
  return (
    <div className="flex flex-wrap gap-1.5">
      {items.map((a) => (
        <span
          key={a}
          className="text-[12px] text-accent border border-accent-dim px-1.5 py-0.5 font-mono"
        >
          {a}
        </span>
      ))}
    </div>
  );
}

function Note({ children }: { children: React.ReactNode }) {
  return (
    <p className="font-sans text-[15px] text-ink-dim leading-[1.6] mt-8 max-w-[48em]">
      <span className="text-accent font-mono">{"//"}</span> {children}
    </p>
  );
}

// ── §01 the station ─────────────────────────────────────────────────────────

const STATION_POINTS: ReadonlyArray<Point> = [
  {
    title: "Teams are pods",
    body: "Each team is a pod with its agents at their desks. Click a team to open its panel and chat.",
  },
  {
    title: "The Brain sits in the middle",
    body: "It runs the project for you: it sets up teams, hands out the work and checks it got done.",
  },
  {
    title: "Lines light up when work happens",
    body: "Programs and services run along the top. A line lights up each time an agent calls one, and the feed at the bottom left shows what's happening now.",
  },
  {
    title: "Pick a look",
    body: "Turn on Voxel world and your agents build a little world around the teams, or Scene: a web page your agents draw for the project, live.",
  },
];

export function AthenaStationSection() {
  return (
    <section id="station" className="relative px-6 lg:px-12 py-24 lg:py-32">
      <SectionLabel>§01 / the station</SectionLabel>
      <DisplayHeading>
        every agent.
        <br />
        <span className="text-accent">on one live map.</span>
      </DisplayHeading>

      <Lede>
        The Station is your project as a live 3D map. You see who is working,
        who is waiting on you, and which tools they are using, without opening a
        single tab.
      </Lede>

      <div className="grid grid-cols-1 lg:grid-cols-[1.45fr_1fr] gap-10 items-start">
        <Shot
          src="/athena-agents/station.webp"
          alt="The Station: the Website, Launch and Research teams around the Brain, with the programs they use along the top"
          caption="the station, three teams"
          tag="demo data"
        />
        <Numbered points={STATION_POINTS} />
      </div>
    </section>
  );
}

// ── §02 the brain ───────────────────────────────────────────────────────────

// The same made-up studio as the hero's Station.
const BRAIN_TEAM: ReadonlyArray<{ role: string; note: string }> = [
  {
    role: "Studio Producer",
    note: "lead: plans the asset list and hands out the work",
  },
  {
    role: "3D Artist",
    note: "from the library: models props and set pieces in Blender, exports .glb",
  },
  {
    role: "Technical Artist",
    note: "from the library: owns poly and texture budgets, checks models in the game",
  },
  {
    role: "Texture Artist",
    note: "new: makes PBR texture sets in the repo's format",
  },
  {
    role: "Historian",
    note: "new: keeps everything true to England in 1461",
  },
];

const BRAIN_POINTS: ReadonlyArray<Point> = [
  {
    title: "Describe the job",
    body: "Press Space on the map, or Ctrl+K / ⌘K anywhere, and say what you want done in plain words.",
  },
  {
    title: "Check the team it builds",
    body: "Look over each agent's model, instructions and tools before anything starts. Change what you like.",
  },
  {
    title: "It follows up",
    body: "When you ask for real work, the Brain checks it got done and follows up. Anything that needs you lands in Needs you.",
  },
  {
    title: "Ask it where things stand",
    body: "Ask what still needs you before Friday, and it answers from the whole project: drafts in the Outbox, open questions, work in review.",
  },
];

export function AthenaBrainSection() {
  return (
    <section
      id="brain"
      className="bg-bg-2 relative px-6 lg:px-12 py-24 lg:py-32"
    >
      <SectionLabel>§02 / the brain</SectionLabel>
      <DisplayHeading>
        describe the job.
        <br />
        <span className="text-accent">get a team.</span>
      </DisplayHeading>

      <Lede>
        The Brain is the assistant that runs the project for you. Tell it what
        you want done: it sets up the agents, writes their instructions, picks
        their tools and hands out the work.
      </Lede>

      <div className="grid grid-cols-1 lg:grid-cols-[1.1fr_1fr] gap-10 items-start">
        <div data-ui className="slab">
          <div className="flex items-center justify-between border-b border-line px-4 py-2 bg-bg-2/60">
            <span className="font-mono text-[12px] tracking-[0.14em] uppercase text-accent-dim">
              <span className="text-accent">▸</span> brain chat
            </span>
            <span className="font-mono text-[12px] text-ink-dim">
              demo project
            </span>
          </div>
          <div className="p-5 flex flex-col gap-5 bg-black/30">
            <p className="self-end max-w-[30em] border border-accent-dim bg-accent/[0.06] px-4 py-3 font-sans text-[15px] leading-[1.55] text-ink">
              Set up a team of 3D modelers and texture artists, alongside a
              historian to make sure our models use accurate real-life
              references.
            </p>
            <div className="font-sans text-[15px] leading-[1.55] text-ink">
              <div className="font-mono text-[12px] tracking-[0.14em] uppercase text-accent mb-3">
                brain
              </div>
              <p className="mb-3">
                I&apos;ve set up the{" "}
                <span className="font-semibold">Art Team</span>:
              </p>
              <ul className="flex flex-col gap-2">
                {BRAIN_TEAM.map((m) => (
                  <li key={m.role} className="flex gap-3">
                    <span className="text-accent font-mono">▪</span>
                    <span>
                      <span className="font-semibold">{m.role}</span>{" "}
                      <span className="text-ink-dim">({m.note})</span>
                    </span>
                  </li>
                ))}
              </ul>
            </div>
          </div>
        </div>

        <Numbered points={BRAIN_POINTS} />
      </div>
    </section>
  );
}

// ── §04 models and effort ───────────────────────────────────────────────────

const ENGINES: ReadonlyArray<Point> = [
  {
    title: "Claude Code",
    body: "The claude CLI, signed in with your Claude account, an Anthropic key or OpenRouter.",
  },
  {
    title: "OpenAI Codex",
    body: "The codex CLI and your ChatGPT login. Settings can install it and sign you in.",
  },
  {
    title: "Eve",
    body: "Built in. Any model from an OpenRouter or Venice key, a local Ollama or LM Studio server, or any OpenAI-compatible address.",
  },
  {
    title: "opencode",
    body: "opencode 2 or later. Its free models work right away; sign in to your own providers with opencode auth login.",
  },
  { title: "Hermes", body: "Hermes installed, with a profile to use." },
  {
    title: "pi / oh-my-pi",
    body: "The pi or omp CLI, signed in to a model provider.",
  },
  {
    title: "OpenClaw",
    body: "The openclaw CLI, set up with openclaw onboard.",
  },
  {
    title: "The Brain's model",
    body: "The Brain can think with Claude Code, Eve, Hermes, Codex or opencode. Pick it on the same page.",
  },
];

const MODEL_POINTS: ReadonlyArray<Point> = [
  {
    title: "A model for every agent",
    body: "Claude Opus, Sonnet, Haiku or Fable, GPT through Codex, DeepSeek, Gemini and Llama through Eve, or a model on your own PC with Ollama or LM Studio.",
  },
  {
    title: "Set how hard it thinks",
    body: "Effort goes from Minimal to Max, per agent. Each engine has its own range, and Athena sends the nearest level it takes.",
  },
  {
    title: "Choose by price or by the job",
    body: "API models show their price per million tokens right in the picker. Give the hard parts to a big model and the routine work to a cheaper or local one.",
  },
  {
    title: "Still one team",
    body: "Handoffs, @mentions and the team chat work the same whatever each agent runs on, so every agent does the part it is best at.",
  },
];

export function AthenaEnginesSection() {
  return (
    <section
      id="models"
      className="bg-bg-2 relative px-6 lg:px-12 py-24 lg:py-32"
    >
      <SectionLabel>§04 / models and effort</SectionLabel>
      <DisplayHeading>
        any model.
        <br />
        <span className="text-accent">mixed freely.</span>
      </DisplayHeading>

      <Lede>
        Each agent picks its own model and how hard it thinks. Put the writer on
        Fable and the 3D artist on Opus, give the routine work to DeepSeek or a
        local model, and they still work as one team.
      </Lede>

      <div className="grid grid-cols-1 lg:grid-cols-[1.35fr_1fr] gap-10 items-start">
        <ModelRoster />
        <Numbered points={MODEL_POINTS} />
      </div>

      <h3 className="font-mono text-[12px] tracking-[0.16em] uppercase text-ink font-semibold mt-20 mb-5">
        Engines you can use
      </h3>
      <Cards items={ENGINES} cols="lg:grid-cols-4" size="text-[22px]" />

      <Note>
        Set up only the engines you want, under Settings → General &amp; keys:
        you need just one. Star the models you use most so they are one click
        away.
      </Note>
    </section>
  );
}

// ── §05 team chats ──────────────────────────────────────────────────────────

const CHAT_POINTS: ReadonlyArray<Point> = [
  {
    title: "Everyone sees the whole chat",
    body: "Your message goes to the lead, or to whoever you @mention. Write #Team name to ask another team.",
  },
  {
    title: "Agents pass the work on",
    body: "They hand work to each other with @mentions and post what they make right in the chat: images, video, audio, and 3D models you can turn with the mouse.",
  },
  {
    title: "Change course any time",
    body: "Write while they work: most agents read your message between steps and carry on with it, without starting over.",
  },
  {
    title: "Commands when you need them",
    body: "Type / for them: /queue lines up the next message, /loop repeats one, /goal says what done looks like, and /learn makes a skill.",
  },
  {
    title: "Floating windows",
    body: "Chats open in windows you can tab, snap side by side and minimize. Earlier chats are a click away to pick up again.",
  },
];

export function AthenaChatsSection() {
  return (
    <section id="chats" className="relative px-6 lg:px-12 py-24 lg:py-32">
      <SectionLabel>§05 / team chats</SectionLabel>
      <DisplayHeading>
        agents that
        <br />
        <span className="text-accent">pass the work on.</span>
      </DisplayHeading>

      <Lede>
        A team works in one group chat. You watch the work move from agent to
        agent, and step in whenever you want.
      </Lede>

      <div className="grid grid-cols-1 lg:grid-cols-[1fr_1.45fr] gap-10 items-start">
        <div className="order-2 lg:order-1">
          <Numbered points={CHAT_POINTS} />
        </div>
        <div className="order-1 lg:order-2">
          <Shot
            src="/athena-agents/team-chat.webp"
            alt="The Launch team's chat with the launch card the X Writer made, next to the team panel listing its members and their models"
            caption="launch team chat"
            tag="demo data"
          />
        </div>
      </div>
    </section>
  );
}

// ── §06 tasks and artifacts ─────────────────────────────────────────────────

export function AthenaWorkSection() {
  return (
    <section
      id="work"
      className="bg-bg-2 relative px-6 lg:px-12 py-24 lg:py-32"
    >
      <SectionLabel>§06 / tasks and artifacts</SectionLabel>
      <DisplayHeading>
        the work.
        <br />
        <span className="text-accent">and what came of it.</span>
      </DisplayHeading>

      <Lede>
        A board for what the project has to do, and one place for everything the
        teams finished.
      </Lede>

      <div className="grid grid-cols-1 lg:grid-cols-2 gap-10 items-start">
        <div className="flex flex-col gap-6">
          <Shot
            src="/athena-agents/tasks.webp"
            alt="The Tasks board with cards in five columns, each showing its priority and who has it"
            caption="tasks"
            tag="demo data"
            sizes="(min-width: 1440px) 680px, (min-width: 1024px) 46vw, 100vw"
          />
          <div>
            <h3 className="font-display font-bold tracking-[-0.015em] text-[24px] leading-[1.1] mb-2 text-ink">
              Tasks
            </h3>
            <p className="text-ink-dim text-[15px] leading-[1.6]">
              To do, In progress, In review, Blocked and Done. Give a task to an
              agent, a team or the Brain and move it to In progress: they pick
              it up, comment with what they made, and move it on. The Brain can
              add, move, assign and tag tasks for you.
            </p>
          </div>
        </div>
        <div className="flex flex-col gap-6">
          <Shot
            src="/athena-agents/artifacts.webp"
            alt="The Artifacts page with a coffee bag label image and a launch plan"
            caption="artifacts"
            tag="demo data"
            sizes="(min-width: 1440px) 680px, (min-width: 1024px) 46vw, 100vw"
          />
          <div>
            <h3 className="font-display font-bold tracking-[-0.015em] text-[24px] leading-[1.1] mb-2 text-ink">
              Artifacts
            </h3>
            <p className="text-ink-dim text-[15px] leading-[1.6]">
              Written results, files and folders, each with a preview, who made
              it, for which team, and when.
            </p>
          </div>
        </div>
      </div>

      <Note>
        Overview shows every project&apos;s agents, teams, Brain and tasks at
        work on one page, with what needs you first. Open their chats side by
        side and write to another project&apos;s agents without switching to it.
      </Note>
    </section>
  );
}

// ── §07 approvals ───────────────────────────────────────────────────────────

const REVIEW: ReadonlyArray<Point> = [
  {
    title: "Needs you",
    body: "Permission requests for risky steps wait here. Agents ask you here when they're stuck.",
  },
  {
    title: "Outbox",
    body: "Questions come with the choices as buttons. Drafts for X, Reddit, Telegram, Slack and Discord wait for you to approve, edit, talk over or reject. X replies always wait.",
  },
  {
    title: "The Brain lets routine work through",
    body: "It approves routine, reversible requests for you and sends anything risky or unclear to Needs you.",
  },
];

const AUTONOMY: ReadonlyArray<{ level: string; asks: string; on?: boolean }> = [
  {
    level: "Ask first",
    asks: "Before any step that could change something.",
  },
  {
    level: "Auto",
    asks: "Before risky steps: shell commands, scripts, sending messages.",
    on: true,
  },
  {
    level: "Full",
    asks: "Never. Only for agents and access you trust.",
  },
];

export function AthenaReviewSection() {
  return (
    <section id="review" className="relative px-6 lg:px-12 py-24 lg:py-32">
      <SectionLabel>§07 / approvals</SectionLabel>
      <DisplayHeading>
        nothing goes out
        <br />
        <span className="text-accent">without you.</span>
      </DisplayHeading>

      <Lede>
        You decide how much each agent may do alone. Risky steps wait for your
        OK, agents ask you when they&apos;re stuck, and posts and messages sit
        in the Outbox until you approve them.
      </Lede>

      <div className="mb-10">
        <Cards items={REVIEW} />
      </div>

      <div className="grid grid-cols-1 lg:grid-cols-[1fr_1.2fr] gap-10 items-start">
        <div data-ui className="slab">
          <div className="border-b border-line px-4 py-2 bg-bg-2/60 font-mono text-[12px] tracking-[0.14em] uppercase text-accent-dim">
            <span className="text-accent">▸</span> when it asks you first, per
            agent
          </div>
          {AUTONOMY.map((a) => (
            <div
              key={a.level}
              className="flex gap-5 px-5 py-4 border-b border-line last:border-b-0"
            >
              <span
                className={`shrink-0 w-[88px] font-mono text-[12px] tracking-[0.14em] uppercase pt-0.5 ${
                  a.on ? "text-accent font-semibold" : "text-ink"
                }`}
              >
                {a.level}
              </span>
              <span className="font-sans text-[15px] text-ink-dim leading-[1.55]">
                {a.asks}
                {a.on ? (
                  <span className="ml-2 text-[11px] tracking-[0.14em] uppercase text-bg bg-accent px-1.5 py-0.5 font-mono font-semibold">
                    default
                  </span>
                ) : null}
              </span>
            </div>
          ))}
          <div className="border-t border-line px-5 py-4 font-sans text-[14px] text-ink-dim leading-[1.6]">
            <span className="text-accent font-mono">{"//"}</span> What the Brain
            may do starts on Full: set it to Ask first, or Read only. Its
            approvals are a model&apos;s judgment, not a guarantee, and autonomy
            is not a sandbox: shell commands run with your account&apos;s
            access.
          </div>
        </div>

        <Shot
          src="/athena-agents/outbox.webp"
          alt="The Outbox: a question from the Market Researcher about pricing, with three choices, above X drafts waiting for approval"
          caption="the outbox"
          tag="demo data"
        />
      </div>
    </section>
  );
}

// ── §08 learning and reach ──────────────────────────────────────────────────

const LEARN: ReadonlyArray<Point> = [
  {
    title: "Memory",
    body: "Every agent and the Brain keep a MEMORY.md for each project with how you like things done and what you decided, read with every message. You can read and change it.",
  },
  {
    title: "/learn",
    body: "Turns a document, a link, a video or a screen recording of you doing a task into a skill any agent can use.",
  },
  {
    title: "Skills",
    body: "Instructions in plain words, like our brand voice or how we test checkout, shared across agents and teams. Many come with the app; write your own or install one from GitHub.",
  },
  {
    title: "Chat gateways",
    body: "Talk to the Brain, a team or an agent from Telegram, Slack or Discord, files included. Each bot answers only the people and channels you let in.",
  },
  {
    title: "Routines",
    body: "Work you want done again and again, run by hand, on a schedule, or by a webhook from GitHub, Stripe, Linear, Slack and others.",
  },
  {
    title: "Webhooks, safely",
    body: "They have a port of their own that answers nothing else, and the Cloudflare or Tailscale tunnel Athena starts in one click only ever points there.",
  },
];

export function AthenaLearnSection() {
  return (
    <section
      id="learn"
      className="bg-bg-2 relative px-6 lg:px-12 py-24 lg:py-32"
    >
      <SectionLabel>§08 / learning and reach</SectionLabel>
      <DisplayHeading>
        it learns how you work.
        <br />
        <span className="text-accent">reach it from anywhere.</span>
      </DisplayHeading>

      <Lede>
        Agents keep notes on what you like and what you decided, and turn what
        you show them into skills. Then talk to them from your phone, or let
        other services start the work.
      </Lede>

      <Cards items={LEARN} />
    </section>
  );
}

// ── §09 programs and mcp ────────────────────────────────────────────────────

const PROGRAMS: ReadonlyArray<{ group: string; items: ReadonlyArray<string> }> =
  [
    {
      group: "3D and games",
      items: ["Blender", "Maya", "Houdini", "Unity", "Unreal Engine", "Godot"],
    },
    {
      group: "Images, video and audio",
      items: [
        "ComfyUI",
        "Photoshop",
        "GIMP",
        "Canva",
        "DaVinci Resolve",
        "ElevenLabs",
      ],
    },
    {
      group: "CAD and 3D printing",
      items: ["FreeCAD", "OpenSCAD", "your 3D printer"],
    },
    {
      group: "Work",
      items: ["browsers", "Cua", "GitHub", "Notion", "Linear", "Composio"],
    },
  ];

const MCP_POINTS: ReadonlyArray<Point> = [
  {
    title: "Suggested for this PC",
    body: "The Programs page finds the creative and CAD apps you have, and Athena points out the MCP servers that fit them.",
  },
  {
    title: "One click, or ask the Brain",
    body: "Add servers from a list of ready-made ones, or just ask. Agents run your ComfyUI workflows and get the images, video, audio and 3D files back.",
  },
  {
    title: "The Brain uses them too",
    body: "“Check my Gmail through Composio.” Settings → The Brain's tools sets whether it runs them all, asks before ones that change things, or doesn't use them.",
  },
];

export function AthenaProgramsSection() {
  return (
    <section id="programs" className="relative px-6 lg:px-12 py-24 lg:py-32">
      <SectionLabel>§09 / programs and mcp</SectionLabel>
      <DisplayHeading>
        your programs.
        <br />
        <span className="text-accent">their hands.</span>
      </DisplayHeading>

      <Lede>
        Connect agents to programs and services with the Model Context Protocol.
        Athena finds what you have installed, suggests the server that fits, and
        adds it in one click.
      </Lede>

      <div className="grid grid-cols-1 lg:grid-cols-[1fr_1.3fr] gap-10 items-start">
        <div className="flex flex-col gap-7">
          {PROGRAMS.map((p) => (
            <div key={p.group}>
              <div className="text-[12px] tracking-[0.16em] uppercase text-ink font-semibold mb-2.5">
                {p.group}
              </div>
              <Tags items={p.items} />
            </div>
          ))}
          <p className="font-sans text-[15px] text-ink-dim leading-[1.6] border-t border-line pt-5">
            Agents can also pull free textures, HDRIs and models from Poly Haven
            and ambientCG.
          </p>
        </div>
        <Shot
          src="/athena-agents/mcp.webp"
          alt="The MCP servers settings page with six servers added"
          caption="mcp servers"
          tag="one click"
        />
      </div>

      <div className="mt-10">
        <Cards items={MCP_POINTS} />
      </div>
    </section>
  );
}

// ── §10 local ───────────────────────────────────────────────────────────────

const LOCAL: ReadonlyArray<Point> = [
  {
    title: "No account, no login",
    body: "Athena runs on your computer. There is nothing to sign up for.",
  },
  {
    title: "Only your computer can reach it",
    body: "It listens on 127.0.0.1. No login means no lock, so never put it behind a public tunnel or proxy.",
  },
  {
    title: "Keys stay on the server",
    body: "API keys and account tokens are never sent to the browser or put into project saves and backups.",
  },
  {
    title: "Saved with the project",
    body: "Each project keeps a copy of its agents, teams, chats and results in its folder. Open the folder again and it all comes back.",
  },
  {
    title: "Backups in one file",
    body: "Back up a project to a single .dashbackup file and restore it as a new project.",
  },
  {
    title: "History you can undo",
    body: "Every agent, team, skill and memory keeps its last 50 versions, so any change can be undone.",
  },
];

export function AthenaLocalSection() {
  return (
    <section
      id="local"
      className="bg-bg-2 relative px-6 lg:px-12 py-24 lg:py-32"
    >
      <SectionLabel>§10 / runs locally</SectionLabel>
      <DisplayHeading>
        your computer.
        <br />
        <span className="text-accent">your work.</span>
      </DisplayHeading>

      <Lede>
        Athena&apos;s server and saved work stay on your machine. Local
        doesn&apos;t mean offline: hosted models and connected services receive
        what you send them. Use Ollama or LM Studio to keep the model on your
        machine too.
      </Lede>

      <Cards items={LOCAL} />
    </section>
  );
}

// ── §11 get it ──────────────────────────────────────────────────────────────

const DOWNLOADS: ReadonlyArray<{ os: string; files: string }> = [
  { os: "Windows", files: "x64 or Arm · .exe installer, or .msi on x64" },
  { os: "macOS", files: "Apple silicon or Intel · .dmg" },
  { os: "Linux", files: "x64 or Arm · .deb, .rpm, or .AppImage on x64" },
];

const FROM_SOURCE = `git clone ${ATHENA_REPO_URL}
cd athena-agents
bun install
bun run start`;

export function AthenaGetSection() {
  return (
    <section id="install" className="relative px-6 lg:px-12 py-24 lg:py-32">
      <SectionLabel>§11 / get it</SectionLabel>
      <DisplayHeading>
        free to use.
        <br />
        <span className="text-accent">yours to change.</span>
      </DisplayHeading>

      <Lede>
        Athena Agents is open source, licensed MIT or Apache-2.0. Install the
        desktop app like any other app, or run it from the source with Bun.
      </Lede>

      <div className="grid grid-cols-1 lg:grid-cols-[1.2fr_1fr] gap-5 lg:gap-6 items-start">
        <div data-ui className="slab">
          <div className="border-b border-line px-4 py-2 bg-bg-2/60 font-mono text-[12px] tracking-[0.14em] uppercase text-accent-dim">
            <span className="text-accent">▸</span> desktop app
          </div>
          {DOWNLOADS.map((d) => (
            <a
              key={d.os}
              href={ATHENA_DOWNLOAD_URL}
              className="group flex items-center justify-between gap-4 px-5 py-5 border-b border-line hover:bg-accent/[0.04] transition-colors"
            >
              <span>
                <span className="block font-display font-bold tracking-[-0.015em] text-[24px] leading-[1.1] text-ink group-hover:text-accent transition-colors">
                  {d.os}
                </span>
                <span className="block font-mono text-[12px] text-ink-dim mt-1">
                  {d.files}
                </span>
              </span>
              <span className="font-mono text-[12px] tracking-[0.16em] uppercase text-accent">
                ↓ download
              </span>
            </a>
          ))}
          <div className="px-5 py-4 font-sans text-[14px] text-ink-dim leading-[1.6]">
            The installers aren&apos;t signed yet. On Windows, choose More info
            → Run anyway. On macOS, Control-click the app in Finder and choose
            Open. Closing the window keeps it running in the tray, and it asks
            before installing an update.
          </div>
        </div>

        <div className="flex flex-col gap-5 lg:gap-6">
          <div data-ui className="slab">
            <div className="flex items-center justify-between border-b border-line px-4 py-2 bg-bg-2/60">
              <span className="font-mono text-[12px] tracking-[0.14em] uppercase text-accent-dim">
                <span className="text-accent">▸</span> from the source
              </span>
              <CopyButton text={FROM_SOURCE} />
            </div>
            <pre className="p-5 text-[14px] leading-[1.7] text-ink overflow-x-auto font-mono bg-black/30">
              {FROM_SOURCE}
            </pre>
            <div className="border-t border-line px-5 py-4 font-sans text-[15px] text-ink-dim leading-[1.6]">
              Needs{" "}
              <span className="font-mono text-[14px] text-ink">Bun 1.4+</span>{" "}
              and a model for your agents. It opens at{" "}
              <span className="font-mono text-[14px] text-ink">
                localhost:4800
              </span>
              . On Windows you can also double-click{" "}
              <span className="font-mono text-[14px] text-ink">start.cmd</span>.
            </div>
          </div>

          <div className="slab p-6 flex flex-col gap-4 font-mono text-[12px] tracking-[0.14em] uppercase">
            <a
              href={ATHENA_GUIDE_URL}
              target="_blank"
              rel="noopener noreferrer"
              className="text-ink hover:text-accent transition-colors"
            >
              ▸ the full guide ↗
            </a>
            <a
              href={ATHENA_REPO_URL}
              target="_blank"
              rel="noopener noreferrer"
              className="text-ink hover:text-accent transition-colors"
            >
              ▸ source // github ↗
            </a>
          </div>
        </div>
      </div>

      <div className="relative mt-24 -mx-6 lg:-mx-12 -mb-24 lg:-mb-32 text-center px-6 py-24 md:py-32 border-t border-line">
        <div
          className="font-display font-bold uppercase tracking-[-0.03em] text-ink leading-[0.95] mb-5"
          style={{ fontSize: "clamp(34px, 5.4vw, 80px)" }}
        >
          your team is <span className="text-accent">ready.</span>
        </div>
        <a href={ATHENA_DOWNLOAD_URL} className="btn-field mt-5">
          ↓ Download Athena Agents
        </a>
      </div>
    </section>
  );
}
