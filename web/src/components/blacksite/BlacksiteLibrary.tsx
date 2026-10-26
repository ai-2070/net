"use client";

import { useMemo, useState } from "react";
import { FieldLabel as SectionLabel } from "../field/FieldLabel";
import { FieldHeading as DisplayHeading } from "../field/FieldHeading";
import { Lede } from "../field/Lede";

// A working copy of the Library's "The Agency" tab. The specialists are real:
// names, emoji and one-liners from msitarzewski/agency-agents at the commit
// Blacksite bundles (library/agency-agents/README.md), so "Read" opens the
// same file the app hires from.
const AGENCY_REPO = "https://github.com/msitarzewski/agency-agents";
const AGENCY_COMMIT = "b37b5bb33b78f2c1d6567d98e25ca818e3259219";

interface Specialist {
  name: string;
  emoji: string;
  division: string;
  vibe: string;
  path: string;
}

const AGENCY: ReadonlyArray<Specialist> = [
  {
    name: "Game Designer",
    emoji: "🎮",
    division: "Game Development",
    vibe: "Thinks in loops, levers, and player motivations to architect compelling gameplay.",
    path: "game-development/game-designer.md",
  },
  {
    name: "Level Designer",
    emoji: "🗺️",
    division: "Game Development",
    vibe: "Treats every level as an authored experience where space tells the story.",
    path: "game-development/level-designer.md",
  },
  {
    name: "Narrative Designer",
    emoji: "📖",
    division: "Game Development",
    vibe: "Architects story systems where narrative and gameplay are inseparable.",
    path: "game-development/narrative-designer.md",
  },
  {
    name: "Technical Artist",
    emoji: "🎨",
    division: "Game Development",
    vibe: "The bridge between artistic vision and engine reality.",
    path: "game-development/technical-artist.md",
  },
  {
    name: "Game Audio Engineer",
    emoji: "🎵",
    division: "Game Development",
    vibe: "Makes every gunshot, footstep, and musical cue feel alive in the game world.",
    path: "game-development/game-audio-engineer.md",
  },
  {
    name: "Economy Designer",
    emoji: "💰",
    division: "Game Development",
    vibe: "Sees every game as a flow of currencies, and every player decision as a transaction.",
    path: "game-development/economy-designer.md",
  },
  {
    name: "Blender Add-on Engineer",
    emoji: "🧩",
    division: "Game Development",
    vibe: "Turns repetitive Blender pipeline work into reliable one-click tools that artists actually use.",
    path: "game-development/blender/blender-addon-engineer.md",
  },
  {
    name: "Godot Multiplayer Engineer",
    emoji: "🌐",
    division: "Game Development",
    vibe: "Masters Godot's MultiplayerAPI to make real-time netcode feel seamless.",
    path: "game-development/godot/godot-multiplayer-engineer.md",
  },
  {
    name: "Unity Architect",
    emoji: "🏛️",
    division: "Game Development",
    vibe: "Designs data-driven, decoupled Unity systems that scale without spaghetti.",
    path: "game-development/unity/unity-architect.md",
  },
  {
    name: "Unreal World Builder",
    emoji: "🌍",
    division: "Game Development",
    vibe: "Builds seamless open worlds with World Partition, Nanite, and procedural foliage.",
    path: "game-development/unreal-engine/unreal-world-builder.md",
  },
  {
    name: "Roblox Experience Designer",
    emoji: "🎪",
    division: "Game Development",
    vibe: "Designs engagement loops and monetization systems that keep players coming back.",
    path: "game-development/roblox-studio/roblox-experience-designer.md",
  },
  {
    name: "UI Designer",
    emoji: "🎨",
    division: "Design",
    vibe: "Creates beautiful, consistent, accessible interfaces that feel just right.",
    path: "design/design-ui-designer.md",
  },
  {
    name: "UX Researcher",
    emoji: "🔬",
    division: "Design",
    vibe: "Validates design decisions with real user data, not assumptions.",
    path: "design/design-ux-researcher.md",
  },
  {
    name: "Brand Guardian",
    emoji: "🎨",
    division: "Design",
    vibe: "Your brand's fiercest protector and most passionate advocate.",
    path: "design/design-brand-guardian.md",
  },
  {
    name: "Image Prompt Engineer",
    emoji: "📷",
    division: "Design",
    vibe: "Translates visual concepts into precise prompts that produce stunning AI photography.",
    path: "design/design-image-prompt-engineer.md",
  },
  {
    name: "Whimsy Injector",
    emoji: "✨",
    division: "Design",
    vibe: "Adds the unexpected moments of delight that make brands unforgettable.",
    path: "design/design-whimsy-injector.md",
  },
  {
    name: "Visual Storyteller",
    emoji: "🎬",
    division: "Design",
    vibe: "Transforms complex information into visual narratives that move people.",
    path: "design/design-visual-storyteller.md",
  },
  {
    name: "Frontend Developer",
    emoji: "🖥️",
    division: "Engineering",
    vibe: "Builds responsive, accessible web apps with pixel-perfect precision.",
    path: "engineering/engineering-frontend-developer.md",
  },
  {
    name: "Backend Architect",
    emoji: "🏗️",
    division: "Engineering",
    vibe: "Designs the systems that hold everything up — databases, APIs, cloud, scale.",
    path: "engineering/engineering-backend-architect.md",
  },
  {
    name: "AI Engineer",
    emoji: "🤖",
    division: "Engineering",
    vibe: "Turns ML models into production features that actually scale.",
    path: "engineering/engineering-ai-engineer.md",
  },
  {
    name: "Code Reviewer",
    emoji: "👁️",
    division: "Engineering",
    vibe: "Reviews code like a mentor, not a gatekeeper. Every comment teaches something.",
    path: "engineering/engineering-code-reviewer.md",
  },
  {
    name: "Data Engineer",
    emoji: "🔧",
    division: "Engineering",
    vibe: "Builds the pipelines that turn raw data into trusted, analytics-ready assets.",
    path: "engineering/engineering-data-engineer.md",
  },
  {
    name: "TikTok Strategist",
    emoji: "🎵",
    division: "Marketing",
    vibe: "Rides the algorithm and builds community through authentic TikTok culture.",
    path: "marketing/marketing-tiktok-strategist.md",
  },
  {
    name: "SEO Specialist",
    emoji: "🔍",
    division: "Marketing",
    vibe: "Drives sustainable organic traffic through technical SEO and content strategy.",
    path: "marketing/marketing-seo-specialist.md",
  },
  {
    name: "Content Creator",
    emoji: "✍️",
    division: "Marketing",
    vibe: "Crafts compelling stories across every platform your audience lives on.",
    path: "marketing/marketing-content-creator.md",
  },
  {
    name: "App Store Optimizer",
    emoji: "📱",
    division: "Marketing",
    vibe: "Gets your app found, downloaded, and loved in the store.",
    path: "marketing/marketing-app-store-optimizer.md",
  },
  {
    name: "Book Co-Author",
    emoji: "📘",
    division: "Marketing",
    vibe: "Turns rough expertise into a recognizable book people can quote, remember, and buy into.",
    path: "marketing/marketing-book-co-author.md",
  },
  {
    name: "Penetration Tester",
    emoji: "🗡️",
    division: "Security",
    vibe: "Breaks into your systems so the real attackers can't.",
    path: "security/security-penetration-tester.md",
  },
  {
    name: "Application Security Engineer",
    emoji: "🔐",
    division: "Security",
    vibe: "Makes developers write secure code without even realizing it.",
    path: "security/security-appsec-engineer.md",
  },
  {
    name: "Incident Responder",
    emoji: "🚨",
    division: "Security",
    vibe: "Runs toward the breach while everyone else runs away.",
    path: "security/security-incident-responder.md",
  },
  {
    name: "Accessibility Auditor",
    emoji: "♿",
    division: "Testing",
    vibe: "If it's not tested with a screen reader, it's not accessible.",
    path: "testing/testing-accessibility-auditor.md",
  },
  {
    name: "Reality Checker",
    emoji: "🧐",
    division: "Testing",
    vibe: 'Defaults to "NEEDS WORK" — requires overwhelming proof for production readiness.',
    path: "testing/testing-reality-checker.md",
  },
  {
    name: "API Tester",
    emoji: "🔌",
    division: "Testing",
    vibe: "Breaks your API before your users do.",
    path: "testing/testing-api-tester.md",
  },
  {
    name: "Product Manager",
    emoji: "🧭",
    division: "Product",
    vibe: "Ships the right thing, not just the next thing — outcome-obsessed, user-grounded, and diplomatically ruthless about focus.",
    path: "product/product-manager.md",
  },
  {
    name: "Trend Researcher",
    emoji: "🔭",
    division: "Product",
    vibe: "Spots emerging trends before they hit the mainstream.",
    path: "product/product-trend-researcher.md",
  },
  {
    name: "Studio Producer",
    emoji: "🎬",
    division: "Project Management",
    vibe: "Aligns creative vision with business objectives across complex initiatives.",
    path: "project-management/project-management-studio-producer.md",
  },
  {
    name: "Deal Strategist",
    emoji: "♟️",
    division: "Sales",
    vibe: "Qualifies deals like a surgeon and kills happy ears on contact.",
    path: "sales/sales-deal-strategist.md",
  },
  {
    name: "Analytics Reporter",
    emoji: "📊",
    division: "Support",
    vibe: "Transforms raw data into the insights that drive your next decision.",
    path: "support/support-analytics-reporter.md",
  },
  {
    name: "XR Immersive Developer",
    emoji: "🌐",
    division: "Spatial Computing",
    vibe: "Builds browser-based AR/VR/XR experiences that push WebXR to its limits.",
    path: "spatial-computing/xr-immersive-developer.md",
  },
  {
    name: "Agents Orchestrator",
    emoji: "🎛️",
    division: "Specialized",
    vibe: "The conductor who runs the entire dev pipeline from spec to ship.",
    path: "specialized/agents-orchestrator.md",
  },
];

const DIVISIONS = [...new Set(AGENCY.map((a) => a.division))];

// The Library's other tabs, shown for the full picture.
const TABS: ReadonlyArray<{ label: string; on?: boolean }> = [
  { label: "Starters" },
  { label: "The Agency (270+)", on: true },
  { label: "Hermes profiles" },
  { label: "Claude Code subagents" },
  { label: "Your presets" },
];

// "Runs on": the engine a hire starts with (the app's first four, its labels).
const RUNS_ON = ["Claude Code", "API model", "Hermes", "Codex"] as const;
type RunsOn = (typeof RUNS_ON)[number];

const SHOWN = 9;

const LIBRARY_POINTS: ReadonlyArray<{ title: string; body: string }> = [
  {
    title: "Search 270+ specialists",
    body: "18 divisions, from game development and design to marketing, security and testing. Search by name or by what you need done.",
  },
  {
    title: "Read before you hire",
    body: "Every specialist comes with its instructions already written. Read them first, then hire with one click.",
  },
  {
    title: "Hire into a team",
    body: "Pick the engine it runs on, hire it straight into a team on the Station, and change anything afterwards.",
  },
];

export function BlacksiteLibrarySection() {
  const [query, setQuery] = useState("");
  const [division, setDivision] = useState<string>("");
  const [runsOn, setRunsOn] = useState<RunsOn>("Claude Code");
  const [hired, setHired] = useState<ReadonlyMap<string, RunsOn>>(new Map());

  const list = useMemo(() => {
    const q = query.trim().toLowerCase();
    return AGENCY.filter(
      (a) =>
        (!division || a.division === division) &&
        (!q || `${a.name} ${a.division} ${a.vibe}`.toLowerCase().includes(q)),
    );
  }, [query, division]);

  const hire = (name: string): void =>
    setHired((m) => new Map(m).set(name, runsOn));

  return (
    <section id="library" className="relative px-6 lg:px-12 py-24 lg:py-32">
      <SectionLabel>§03 / the library</SectionLabel>
      <DisplayHeading>
        270+ specialists.
        <br />
        <span className="text-accent">ready to hire.</span>
      </DisplayHeading>

      <Lede>
        Need someone the team doesn&apos;t have? Search the Library and hire
        one. Each specialist arrives with its role and instructions already
        written, so it can start on the first message.
      </Lede>

      <div data-ui className="slab">
        <div className="flex items-center justify-between gap-4 border-b border-line px-4 py-2 bg-bg-2/60">
          <span className="font-mono text-[12px] tracking-[0.14em] uppercase text-accent-dim">
            <span className="text-accent">▸</span> library
          </span>
          <span className="font-mono text-[12px] text-ink-dim">
            try it: search, hire, read
          </span>
        </div>

        <div className="px-5 pt-4 flex flex-wrap items-end justify-between gap-x-6 gap-y-3 border-b border-line">
          <div className="flex gap-5 overflow-x-auto -mb-px">
            {TABS.map((t) => (
              <span
                key={t.label}
                className={`whitespace-nowrap pb-2.5 font-mono text-[11px] tracking-[0.14em] uppercase border-b-2 ${
                  t.on
                    ? "border-accent text-accent"
                    : "border-transparent text-ink-faint"
                }`}
              >
                {t.label}
              </span>
            ))}
          </div>
          <div className="flex items-center gap-3 pb-2.5">
            <span className="font-mono text-[11px] tracking-[0.14em] uppercase text-ink-dim">
              runs on
            </span>
            <div
              className="flex border border-line divide-x divide-line"
              role="radiogroup"
              aria-label="Runs on"
            >
              {RUNS_ON.map((r) => (
                <button
                  key={r}
                  type="button"
                  role="radio"
                  aria-checked={runsOn === r}
                  onClick={() => setRunsOn(r)}
                  className={`px-2.5 py-1 font-mono text-[11px] cursor-pointer transition-colors ${
                    runsOn === r
                      ? "bg-ink text-bg"
                      : "text-ink-dim hover:text-ink hover:bg-bg-2"
                  }`}
                >
                  {r}
                </button>
              ))}
            </div>
          </div>
        </div>

        <div className="p-5 bg-black/30">
          <input
            type="search"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            aria-label="Search the specialists"
            placeholder='Search 270+ specialists: "unity", "seo", "security"…'
            className="w-full bg-bg border border-line focus:border-accent-dim outline-none px-4 py-3 font-sans text-[15px] text-ink placeholder:text-ink-faint"
          />

          <div
            className="flex flex-wrap gap-1.5 mt-4"
            role="radiogroup"
            aria-label="Division"
          >
            {["", ...DIVISIONS].map((d) => {
              const on = division === d;
              return (
                <button
                  key={d || "all"}
                  type="button"
                  role="radio"
                  aria-checked={on}
                  onClick={() => setDivision(d)}
                  className={`px-2 py-0.5 font-mono text-[11px] tracking-[0.06em] border cursor-pointer transition-colors ${
                    on
                      ? "border-accent text-bg bg-accent"
                      : "border-line text-ink-dim hover:text-ink hover:border-ink-faint"
                  }`}
                >
                  {d || "All"}
                </button>
              );
            })}
          </div>

          <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-3 gap-3 mt-5">
            {list.slice(0, SHOWN).map((a) => {
              const on = hired.get(a.name);
              return (
                <div
                  key={a.path}
                  className={`flex flex-col border bg-bg p-4 transition-colors ${
                    on ? "border-accent-dim" : "border-line"
                  }`}
                >
                  <div className="flex items-start gap-3">
                    <span
                      aria-hidden
                      className="grid place-items-center w-10 h-10 shrink-0 border border-line bg-bg-2 text-[20px]"
                    >
                      {a.emoji}
                    </span>
                    <div className="min-w-0">
                      <div className="font-display font-bold text-[17px] leading-[1.15] text-ink">
                        {a.name}
                      </div>
                      <div className="font-mono text-[10.5px] tracking-[0.14em] uppercase text-ink-faint mt-1">
                        {a.division}
                      </div>
                    </div>
                  </div>
                  <p className="font-sans text-[14px] leading-[1.5] text-ink-dim mt-3 flex-1">
                    {a.vibe}
                  </p>
                  <div className="flex items-center gap-2 mt-4">
                    {on ? (
                      <span className="font-mono text-[11px] tracking-[0.1em] uppercase text-accent">
                        ✓ hired · runs on {on}
                      </span>
                    ) : (
                      <>
                        <button
                          type="button"
                          onClick={() => hire(a.name)}
                          className="px-3 py-1 font-mono text-[11px] tracking-[0.14em] uppercase font-semibold bg-accent text-bg cursor-pointer hover:brightness-110"
                        >
                          Hire
                        </button>
                        <a
                          href={`${AGENCY_REPO}/blob/${AGENCY_COMMIT}/${a.path}`}
                          target="_blank"
                          rel="noopener noreferrer"
                          className="px-3 py-1 font-mono text-[11px] tracking-[0.14em] uppercase border border-line text-ink hover:border-ink-faint"
                        >
                          Read ↗
                        </a>
                      </>
                    )}
                  </div>
                </div>
              );
            })}
          </div>

          {!list.length ? (
            <p className="font-sans text-[15px] text-ink-dim mt-5">
              None of the {AGENCY.length} here match. The app searches all 270+.
            </p>
          ) : (
            <p className="font-mono text-[12px] text-ink-faint mt-5">
              Showing {Math.min(SHOWN, list.length)} of {list.length} picked for
              this page. The app has 270+ in 18 divisions.
            </p>
          )}
        </div>
      </div>

      <div className="grid grid-cols-1 md:grid-cols-3 gap-8 mt-12">
        {LIBRARY_POINTS.map((p, i) => (
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

      <p className="font-sans text-[15px] text-ink-dim leading-[1.6] mt-10 max-w-[48em]">
        <span className="text-accent font-mono">{"//"}</span> The specialists
        come from{" "}
        <a
          href={AGENCY_REPO}
          target="_blank"
          rel="noopener noreferrer"
          className="text-ink underline decoration-ink-faint underline-offset-4 hover:text-accent"
        >
          The Agency
        </a>{" "}
        (msitarzewski/agency-agents, MIT) and are included with the app, so the
        Library works with no download. It also has ready-made starters, your
        Hermes profiles and Claude Code subagents, and presets you saved.
      </p>
    </section>
  );
}
