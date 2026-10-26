"use client";

import { FieldLabel as SectionLabel } from "../field/FieldLabel";
import { FieldHeading as DisplayHeading } from "../field/FieldHeading";
import { Lede } from "../field/Lede";
import { CopyButton } from "../CopyButton";
import { Shot } from "./Shot";
import { ModelRoster } from "./ModelRoster";
import {
  BLACKSITE_DOWNLOAD_URL,
  BLACKSITE_GUIDE_URL,
  BLACKSITE_REPO_URL,
} from "./links";

// Every fact on this page comes from the Blacksite README and its 0.1.0
// release notes. Keep them in step when either changes.

function Numbered({
  points,
}: {
  points: ReadonlyArray<{ title: string; body: string }>;
}) {
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

// ── §01 the station ─────────────────────────────────────────────────────────

const STATION_POINTS: ReadonlyArray<{ title: string; body: string }> = [
  {
    title: "Teams are pods",
    body: "Each team is a pod with its agents at their desks. Click a team or an agent to open its panel and chat.",
  },
  {
    title: "The Brain sits in the middle",
    body: "It sets up teams, skills and routines when you ask, and hands work to the right agent.",
  },
  {
    title: "Lines light up when work happens",
    body: "Programs and services run along the top. A line lights up each time an agent calls one.",
  },
  {
    title: "Projects are floors",
    body: "Each project has its own agents, teams and chats. Press Up and Down to move between them.",
  },
];

export function BlacksiteStationSection() {
  return (
    <section id="station" className="relative px-6 lg:px-12 py-24 lg:py-32">
      <SectionLabel>§01 / the station</SectionLabel>
      <DisplayHeading>
        every agent.
        <br />
        <span className="text-accent">on one live map.</span>
      </DisplayHeading>

      <Lede>
        The Station is a live 3D view of your project. You see who is working,
        who is waiting on you, and which tools they are using, without opening a
        single tab.
      </Lede>

      <div className="grid grid-cols-1 lg:grid-cols-[1.45fr_1fr] gap-10 items-start">
        <Shot
          src="/blacksite/station.webp"
          alt="The Station: a live 3D map with three team pods around the Brain, and lines running from connected programs to the agents that use them"
          caption="the station, three teams"
          tag="demo data"
        />
        <Numbered points={STATION_POINTS} />
      </div>
    </section>
  );
}

// ── §02 the brain ───────────────────────────────────────────────────────────

const BRAIN_TEAM: ReadonlyArray<{ role: string; note: string }> = [
  {
    role: "Studio Producer",
    note: "lead, from the library: plans the asset list and hands out work",
  },
  {
    role: "3D Artist",
    note: "models props and set pieces in Blender, exports .glb",
  },
  {
    role: "Technical Artist",
    note: "owns poly and texture budgets, checks models in the game",
  },
  {
    role: "Texture Artist",
    note: "new: makes PBR texture sets in the repo's format",
  },
  {
    role: "Historian",
    note: "from the library: keeps everything true to England in 1461",
  },
];

const BRAIN_POINTS: ReadonlyArray<{ title: string; body: string }> = [
  {
    title: "Describe the outcome",
    body: "Open Ask the Brain, press Ctrl+K, or press Space on the Station. Say what you want done, in plain words.",
  },
  {
    title: "Review the team it builds",
    body: "Check each agent's model, instructions, tools and work folders before anything starts. Change what you like.",
  },
  {
    title: "Or hire your own",
    body: "The Library has ready-made starters and The Agency's 270+ specialists, included with the app.",
  },
];

export function BlacksiteBrainSection() {
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
        The Brain is the project-wide assistant. One sentence, and it builds the
        agents, writes the brief, picks the tools and gets them started.
      </Lede>

      <div className="grid grid-cols-1 lg:grid-cols-[1.1fr_1fr] gap-10 items-start">
        <div data-ui className="slab">
          <div className="flex items-center justify-between border-b border-line px-4 py-2 bg-bg-2/60">
            <span className="font-mono text-[12px] tracking-[0.14em] uppercase text-accent-dim">
              <span className="text-accent">▸</span> brain chat
            </span>
            <span className="font-mono text-[12px] text-ink-dim">
              Rose &amp; Blade · demo
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
                <span className="font-semibold">Rose &amp; Blade Art Team</span>
                :
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

// ── §04 models and effort ───────────────────────────────────────────────────────────

const ENGINES: ReadonlyArray<{ name: string; needs: string }> = [
  {
    name: "Claude Code",
    needs:
      "The claude CLI, with your Claude login, an Anthropic key or OpenRouter.",
  },
  { name: "OpenAI Codex", needs: "The codex CLI and your ChatGPT login." },
  { name: "Hermes", needs: "An installed Hermes setup with a profile to use." },
  { name: "pi", needs: "The pi coding agent, signed in to a model provider." },
  { name: "oh-my-pi", needs: "The omp CLI, signed in with /login." },
  {
    name: "OpenClaw",
    needs: "The openclaw CLI. Pick which of your OpenClaw agents answers.",
  },
  {
    name: "Local models",
    needs: "A running LM Studio or Ollama server, on your own machine.",
  },
  {
    name: "Any API",
    needs: "OpenRouter, or any other OpenAI-compatible endpoint.",
  },
];

const MODEL_POINTS: ReadonlyArray<{ title: string; body: string }> = [
  {
    title: "A model for every agent",
    body: "Claude Opus, Sonnet, Haiku or Fable, GPT through Codex, DeepSeek, Gemini, Llama and 300+ more through OpenRouter, or a model on your own PC.",
  },
  {
    title: "Set how hard it thinks",
    body: "Effort goes from Minimal to Max, per agent. Each engine has its own range, and Blacksite sends the nearest level it takes.",
  },
  {
    title: "Choose by price or by the job",
    body: "OpenRouter models show their price per million tokens right in the picker. Give the hard parts to a big model and the routine work to a cheaper or local one.",
  },
  {
    title: "Still one team",
    body: "Handoffs, @mentions and the team chat work the same whatever each agent runs on, so every agent does the part it is best at.",
  },
];

export function BlacksiteEnginesSection() {
  return (
    <section
      id="models"
      className="bg-bg-2 relative px-6 lg:px-12 py-24 lg:py-32"
    >
      <SectionLabel>§04 / models and effort</SectionLabel>
      <DisplayHeading>
        the right model.
        <br />
        <span className="text-accent">for every job.</span>
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
      <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-4 gap-5 lg:gap-6">
        {ENGINES.map((e) => (
          <div key={e.name} className="slab slab-lift p-6">
            <h3 className="font-display font-bold tracking-[-0.015em] text-[22px] leading-[1.1] mb-2 text-ink">
              {e.name}
            </h3>
            <p className="text-ink-dim text-[15px] leading-[1.6]">{e.needs}</p>
          </div>
        ))}
      </div>

      <p className="font-sans text-[15px] text-ink-dim leading-[1.6] mt-8 max-w-[44em]">
        <span className="text-accent font-mono">{"//"}</span> Set up only the
        engines you want, under Settings → General &amp; keys, and star the
        models you use most so they are one click away.
      </p>
    </section>
  );
}

// ── §05 team chats ──────────────────────────────────────────────────────────

const CHAT_POINTS: ReadonlyArray<{ title: string; body: string }> = [
  {
    title: "Everyone sees the whole chat",
    body: "Your message goes to the team lead, or to whoever you @mention. Each turn streams in live.",
  },
  {
    title: "Agents hand work to each other",
    body: "An agent @mentions the next one when its part is done. Write #Team Name and the message goes to another team.",
  },
  {
    title: "Files show up in the chat",
    body: "Images, video, audio, and 3D models you can turn with the mouse, right where the agent mentions them.",
  },
  {
    title: "Commands when you need them",
    body: "Type / for /queue, /stop, /resume, /loop, /goal and /compact. Long chats are compacted so agents keep what matters.",
  },
];

export function BlacksiteChatsSection() {
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
            src="/blacksite/team-chat.webp"
            alt="A team chat with an audio player and a 3D model preview, next to the team's panel listing its members and their models"
            caption="video & motion team chat"
            tag="demo data"
          />
        </div>
      </div>
    </section>
  );
}

// ── §06 programs and mcp ────────────────────────────────────────────────────

const PROGRAMS: ReadonlyArray<{ group: string; items: ReadonlyArray<string> }> =
  [
    {
      group: "3D and games",
      items: [
        "Blender",
        "Maya",
        "Houdini",
        "Substance 3D Painter",
        "Unity",
        "Unreal Engine",
        "Godot",
      ],
    },
    {
      group: "Images and design",
      items: ["ComfyUI", "Canva", "GIMP", "Krita", "Inkscape", "Topaz"],
    },
    {
      group: "Audio and video",
      items: ["ffmpeg", "ElevenLabs", "Audacity", "REAPER", "OBS"],
    },
    {
      group: "CAD and 3D printing",
      items: ["FreeCAD", "Rhino", "OpenSCAD", "PrusaSlicer", "your 3D printer"],
    },
    {
      group: "Work",
      items: ["a real browser", "GitHub", "Notion", "Linear", "web search"],
    },
  ];

export function BlacksiteProgramsSection() {
  return (
    <section
      id="programs"
      className="bg-bg-2 relative px-6 lg:px-12 py-24 lg:py-32"
    >
      <SectionLabel>§06 / programs and mcp</SectionLabel>
      <DisplayHeading>
        your programs.
        <br />
        <span className="text-accent">their hands.</span>
      </DisplayHeading>

      <Lede>
        Agents work with your files and the programs you already use. Add MCP
        servers from a list in one click, or ask the Brain to do it.
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
            The Programs page also finds Adobe apps, video editors such as
            DaVinci Resolve, and CAD programs already on your computer. Agents
            can pull free textures, HDRIs and models from Poly Haven and
            ambientCG.
          </p>
        </div>
        <Shot
          src="/blacksite/mcp-servers.webp"
          alt="The MCP servers page with ready-made presets such as Playwright, Chrome DevTools, Blender and GitHub, some marked as needing a key"
          caption="mcp servers, ready-made presets"
          tag="one click"
        />
      </div>
    </section>
  );
}

// ── §07 review ──────────────────────────────────────────────────────────────

const REVIEW: ReadonlyArray<{ title: string; body: string }> = [
  {
    title: "Needs you",
    body: "Permission requests wait here. The Brain lets routine, reversible work through and sends risky or unclear requests to you, with a reason.",
  },
  {
    title: "Outbox",
    body: "X and Reddit drafts wait for you to approve, edit or reject. Nothing is posted on its own.",
  },
  {
    title: "Artifacts",
    body: "Results, files and folders agents saved, each with a preview, who made it, for which team, and when.",
  },
];

const AUTONOMY: ReadonlyArray<{ level: string; asks: string; on?: boolean }> = [
  {
    level: "Ask first",
    asks: "Asks you before low- and high-risk actions. Tools that do nothing risky never ask.",
  },
  {
    level: "Auto",
    asks: "Asks you before high-risk actions: shell commands, scripts, sending messages.",
    on: true,
  },
  {
    level: "Full",
    asks: "Never asks. Use it only when you trust the agent and its access.",
  },
];

export function BlacksiteReviewSection() {
  return (
    <section id="review" className="relative px-6 lg:px-12 py-24 lg:py-32">
      <SectionLabel>§07 / approvals</SectionLabel>
      <DisplayHeading>
        nothing goes out
        <br />
        <span className="text-accent">without you.</span>
      </DisplayHeading>

      <Lede>
        You decide how much each agent may do alone. Anything that needs your OK
        waits in one place, and finished work is kept where you can find it.
      </Lede>

      <div className="grid grid-cols-1 lg:grid-cols-3 gap-5 lg:gap-6 mb-10">
        {REVIEW.map((r) => (
          <div key={r.title} className="slab slab-lift p-7">
            <h3 className="font-display font-bold tracking-[-0.015em] text-[24px] leading-[1.1] mb-2 text-ink">
              {r.title}
            </h3>
            <p className="text-ink-dim text-[15px] leading-[1.6]">{r.body}</p>
          </div>
        ))}
      </div>

      <div className="grid grid-cols-1 lg:grid-cols-[1fr_1.2fr] gap-10 items-start">
        <div data-ui className="slab">
          <div className="border-b border-line px-4 py-2 bg-bg-2/60 font-mono text-[12px] tracking-[0.14em] uppercase text-accent-dim">
            <span className="text-accent">▸</span> autonomy, per agent
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
            <span className="text-accent font-mono">{"//"}</span> The
            Brain&apos;s approvals are a model&apos;s judgment, not a hard
            safety guarantee. Autonomy is not an operating-system sandbox: shell
            commands run with your account&apos;s access.
          </div>
        </div>

        <Shot
          src="/blacksite/artifacts.webp"
          alt="The Artifacts page: saved files with image and 3D model previews, who made them, for which team and when"
          caption="artifacts"
          tag="demo data"
        />
      </div>
    </section>
  );
}

// ── §08 local ───────────────────────────────────────────────────────────────

const LOCAL: ReadonlyArray<{ title: string; body: string }> = [
  {
    title: "No account, no login",
    body: "The dashboard runs on your computer. There is nothing to sign up for.",
  },
  {
    title: "Only your computer can reach it",
    body: "The server listens on 127.0.0.1 and refuses requests from other sites.",
  },
  {
    title: "Keys stay on the server",
    body: "API keys and account tokens are never sent to the browser or put in exports.",
  },
  {
    title: "Saved with the project",
    body: "Each project folder keeps a copy of its agents, teams, chats and skills. Open the folder again and it all comes back.",
  },
  {
    title: "Backups in one file",
    body: "Back up a project to a single file and restore it as a new project, with schedules paused.",
  },
  {
    title: "History you can undo",
    body: "Compare and restore the last 50 versions of a skill, agent, team or memory file.",
  },
];

export function BlacksiteLocalSection() {
  return (
    <section
      id="local"
      className="bg-bg-2 relative px-6 lg:px-12 py-24 lg:py-32"
    >
      <SectionLabel>§08 / runs locally</SectionLabel>
      <DisplayHeading>
        your computer.
        <br />
        <span className="text-accent">your work.</span>
      </DisplayHeading>

      <Lede>
        Blacksite&apos;s server and saved state stay on your machine. Local
        doesn&apos;t mean offline: hosted models and connected services get the
        requests you send them. For fully local work, use LM Studio or Ollama.
      </Lede>

      <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-3 gap-5 lg:gap-6">
        {LOCAL.map((p) => (
          <div key={p.title} className="slab slab-lift p-7">
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

// ── §09 get it ──────────────────────────────────────────────────────────────

const DOWNLOADS: ReadonlyArray<{ os: string; files: string }> = [
  { os: "Windows", files: "x64 or Arm · .exe installer, or .msi" },
  { os: "macOS", files: "Apple silicon or Intel · .dmg" },
  { os: "Linux", files: "x64 or Arm · .deb, .rpm, or .AppImage" },
];

const FROM_SOURCE = `git clone ${BLACKSITE_REPO_URL}
cd blacksite
bun install
bun run start`;

export function BlacksiteGetSection() {
  return (
    <section id="install" className="relative px-6 lg:px-12 py-24 lg:py-32">
      <SectionLabel>§09 / get it</SectionLabel>
      <DisplayHeading>
        free to use.
        <br />
        <span className="text-accent">yours to change.</span>
      </DisplayHeading>

      <Lede>
        Blacksite is open source, licensed MIT or Apache-2.0. Install the
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
              href={BLACKSITE_DOWNLOAD_URL}
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
            → Run anyway. On macOS, Control-click the app and choose Open. The
            app checks for updates and asks before installing one.
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
              <span className="font-mono text-[14px] text-ink">Bun 1.2+</span>{" "}
              and a model for your agents. The dashboard opens at{" "}
              <span className="font-mono text-[14px] text-ink">
                localhost:4800
              </span>
              .
            </div>
          </div>

          <div className="slab p-6 flex flex-col gap-4 font-mono text-[12px] tracking-[0.14em] uppercase">
            <a
              href={BLACKSITE_GUIDE_URL}
              target="_blank"
              rel="noopener noreferrer"
              className="text-ink hover:text-accent transition-colors"
            >
              ▸ the full guide ↗
            </a>
            <a
              href={BLACKSITE_REPO_URL}
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
        <a href={BLACKSITE_DOWNLOAD_URL} className="btn-field mt-5">
          ↓ Download Blacksite
        </a>
      </div>
    </section>
  );
}
