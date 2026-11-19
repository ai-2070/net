import { FooterColumn } from "./FooterColumn";
import globals from "@/lib/globals";
import {
  ATHENA_DOWNLOAD_URL,
  ATHENA_GUIDE_URL,
  ATHENA_REPO_URL,
} from "@/components/athena/links";

const FOOTER_SPEC: ReadonlyArray<{
  href: string;
  label: string;
  class?: string;
}> = [
  { href: "/#topology", label: "Topology classes" },
  { href: "/#properties", label: "Protocol properties" },
  { href: "/#mikoshi", label: "Mikoshi" },
  { href: "/#runtime", label: "Compute runtime" },
  { href: "/#apps", label: "Applications" },
  { href: "/#meshos", label: "MeshOS" },
  { href: "/#wall", label: "The Blackwall" },
  { href: "/#releases", label: "Releases" },
];

const FOOTER_DOCS: ReadonlyArray<{
  href: string;
  label: string;
  class?: string;
}> = [
  {
    href: "/docs/start/quickstart",
    label: "Quickstart",
  },
  {
    href: "/docs/concepts/architecture",
    label: "Architecture",
  },
  {
    href: "/docs/concepts/channels",
    label: "Channels",
  },
  {
    href: "/docs/concepts/events-and-causality",
    label: "Events and Causality",
  },
  {
    href: "/docs/concepts/identity",
    label: "Identity",
  },
  {
    href: "/docs/concepts/capabilities",
    label: "Capabilities",
  },
  {
    href: "/docs/concepts/subnets",
    label: "Subnets",
  },
  {
    href: "/docs/concepts/storage-stack",
    label: "Storage",
  },
];

const FOOTER_RESOURCES: ReadonlyArray<{
  href: string;
  label: string;
  class?: string;
}> = [
  {
    href: "https://crates.io/crates/net-mesh",
    label: "Rust // crates.io",
  },
  {
    href: "https://www.npmjs.com/package/@net-mesh/core",
    label: "TypeScript // npm",
  },
  { href: "https://pypi.org/project/net-mesh/", label: "Python // PyPI" },
  {
    href: "https://github.com/ai-2070/net/tree/master/go",
    label: "Go // module",
  },
  {
    href: "https://github.com/ai-2070/net/tree/master/net/crates/net/include",
    label: "C // SDK",
  },
  { href: "https://github.com/ai-2070/net", label: "Source // GitHub" },
];

type FooterLink = { href: string; label: string; class?: string };

// On every page: the other parts of the site, and how to reach us.
const FOOTER_EXPLORE: ReadonlyArray<FooterLink> = [
  { href: "/", label: "Net" },
  { href: "/games", label: "Games" },
  { href: "/athena-agents", label: "Athena Agents" },
  { href: "/docs", label: "Docs" },
  {
    href: `mailto:${globals.email}`,
    label: "▸ Contact",
    class: "text-accent",
  },
];

const GAMES_PAGE: ReadonlyArray<FooterLink> = [
  { href: "/games#speed", label: "Speed" },
  { href: "/games#demo", label: "Play the demo" },
  { href: "/games#fps", label: "Net Shooter" },
  { href: "/games#modes", label: "Game modes" },
  { href: "/games#hosts", label: "Many hosts" },
  { href: "/games#code", label: "Add it to your game" },
  { href: "/games#engines", label: "Any engine" },
  { href: "/games#ai", label: "AI-built games" },
];

const GAMES_BUILD: ReadonlyArray<FooterLink> = [
  { href: "/docs/sdk/browser/quickstart", label: "Browser quickstart" },
  { href: "/docs/sdk/browser/store", label: "Networked store" },
  { href: "/docs/sdk/browser/three", label: "Three.js" },
  { href: "/docs/sdk/browser/netcode", label: "Netcode" },
  {
    href: "https://github.com/ai-2070/net-fps",
    label: "Net Shooter // GitHub",
  },
  { href: "https://github.com/ai-2070/net", label: "Net // GitHub" },
];

const ATHENA_PAGE: ReadonlyArray<FooterLink> = [
  { href: "/athena-agents#station", label: "The Station" },
  { href: "/athena-agents#brain", label: "The Brain" },
  { href: "/athena-agents#library", label: "Agent library" },
  { href: "/athena-agents#models", label: "Models and effort" },
  { href: "/athena-agents#chats", label: "Team chats" },
  { href: "/athena-agents#work", label: "Tasks and artifacts" },
  { href: "/athena-agents#review", label: "Approvals" },
  { href: "/athena-agents#learn", label: "Memory and skills" },
  { href: "/athena-agents#programs", label: "Programs and MCP" },
  { href: "/athena-agents#local", label: "Runs locally" },
];

const ATHENA_GET: ReadonlyArray<FooterLink> = [
  { href: ATHENA_DOWNLOAD_URL, label: "Download" },
  { href: ATHENA_GUIDE_URL, label: "User guide" },
  { href: ATHENA_REPO_URL, label: "Source // GitHub" },
];

export type FooterPage = "net" | "games" | "athena";

// What the footer says on each landing page: its name and line, its own
// columns (Explore is added to all of them) and the bottom strip.
const FOOTERS: Record<
  FooterPage,
  {
    name: string;
    line: string;
    columns: ReadonlyArray<{ title: string; items: ReadonlyArray<FooterLink> }>;
    mark: string;
    status: [string, string];
  }
> = {
  net: {
    name: "net",
    line: "Network Event Transport. A latency-first encrypted protocol for compute.",
    columns: [
      { title: "Spec", items: FOOTER_SPEC },
      { title: "Docs", items: FOOTER_DOCS },
      { title: "Resources", items: FOOTER_RESOURCES },
    ],
    mark: "NET // PROTOCOL.0x4E45·54",
    status: ["NET status:", "ONLINE"],
  },
  games: {
    name: "net",
    line: "Multiplayer for any game engine, plug and play for Three.js. Players connect directly, with no game server to run.",
    columns: [
      { title: "Games", items: GAMES_PAGE },
      { title: "Build", items: GAMES_BUILD },
    ],
    mark: "NET // PROTOCOL.0x4E45·54",
    status: ["NET status:", "ONLINE"],
  },
  athena: {
    name: "athena",
    line: "A home for your AI agents. Build teams, give each agent a job, a model and its tools, and watch them work. Runs on your computer. Free and open source.",
    columns: [
      { title: "Athena Agents", items: ATHENA_PAGE },
      { title: "Get it", items: ATHENA_GET },
    ],
    mark: "ATHENA AGENTS",
    status: ["Athena Agents 1.0:", "DESKTOP APP"],
  },
};

const ET_YEAR = new Date().toLocaleString("en-US", {
  timeZone: "America/New_York",
  year: "numeric",
});

export function Footer({
  world,
  page = "net",
}: { world?: "field"; page?: FooterPage } = {}) {
  if (world === "field") return <FieldFooter page={page} />;
  return (
    <footer className="px-6 pt-16 pb-7 border-t border-accent-dim">
      <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-[2fr_1fr_1fr_1fr_1fr] gap-8 mb-12">
        <div>
          <div className="logo-mark font-display text-[22px] text-ink tracking-[0.1em] flex items-baseline gap-2.5 mb-4">
            net{" "}
            <span className="font-mono text-[11px] tracking-[0.15em] font-semibold">
              {"// AI 2070"}
            </span>
          </div>
          <p className="text-ink-dim text-[12px] leading-[1.6] max-w-[380px]">
            Network Event Transport. A latency-first encrypted protocol for
            compute.
          </p>
        </div>
        <FooterColumn title="Spec" items={FOOTER_SPEC} />
        <FooterColumn title="Docs" items={FOOTER_DOCS} />
        <FooterColumn title="Resources" items={FOOTER_RESOURCES} />
        <FooterColumn title="Explore" items={FOOTER_EXPLORE} />
      </div>

      <div className="border-t border-line pt-6 flex justify-between text-[11px] text-ink-dim tracking-[0.1em] flex-wrap gap-4">
        <span>
          © {ET_YEAR} — {"NET // PROTOCOL.0x4E45·54"}
        </span>
        <span>
          <span className="text-accent">▸</span> NET status:{" "}
          <span className="text-accent">ONLINE</span>
        </span>
        <span className="shimmer-2070">{"// AI 2070"}</span>
      </div>
    </footer>
  );
}

// Landing-page footer: the floor of the hall. A long monolith edge, then
// the directory cast into it.
function FieldFooter({ page }: { page: FooterPage }) {
  const f = FOOTERS[page];
  const columns = [...f.columns, { title: "Explore", items: FOOTER_EXPLORE }];
  return (
    <footer className="relative px-6 lg:px-12 pt-10 pb-8">
      <div aria-hidden className="h-px bg-line -mx-6 lg:-mx-12 mb-16" />
      <div
        className={`grid grid-cols-1 md:grid-cols-2 gap-10 mb-16 ${
          columns.length === 4
            ? "lg:grid-cols-[2fr_1fr_1fr_1fr_1fr]"
            : "lg:grid-cols-[2fr_1fr_1fr_1fr]"
        }`}
      >
        <div>
          <div className="flex items-baseline gap-3 mb-5">
            <span className="font-display font-bold uppercase text-[40px] leading-none tracking-[-0.03em] text-ink">
              {f.name}
            </span>
            <span className="font-mono text-[11px] tracking-[0.18em] font-semibold text-ink-dim">
              {"// AI 2070"}
            </span>
          </div>
          <p className="text-ink-dim text-[15px] leading-[1.6] max-w-[24em]">
            {f.line}
          </p>
        </div>
        {columns.map((c) => (
          <FooterColumn
            key={c.title}
            title={c.title}
            items={c.items}
            world="field"
          />
        ))}
      </div>

      <div className="border-t border-line pt-6 flex justify-between stencil flex-wrap gap-4">
        <span>
          © {ET_YEAR} — {f.mark}
        </span>
        <span className="flex items-center gap-2">
          <span aria-hidden className="w-1.5 h-1.5 bg-accent inline-block" />
          {f.status[0]} <span className="text-accent">{f.status[1]}</span>
        </span>
        <span className="text-ink">{"// AI 2070"}</span>
      </div>
    </footer>
  );
}
