"use client";

import { useMemo, useState } from "react";
import { FieldLabel as SectionLabel } from "../field/FieldLabel";
import { FieldHeading as DisplayHeading } from "../field/FieldHeading";
import { Lede } from "../field/Lede";
import { LIBRARY, LIBRARY_COMMIT, LIBRARY_GROUPS } from "./libraryData";
import { ATHENA_REPO_URL } from "./links";

// A working copy of the Agent library's "Athena" tab. The agents are real:
// libraryData.ts is generated from library/athena-agents in the app's repo,
// so "Read" opens the same file the app hires from.

// The chip for the agents "Add Athena team" hires, as the app labels it.
const STARTER = "Starter team";

// The Library's other tabs, shown for the full picture.
const TABS: ReadonlyArray<{ label: string; on?: boolean }> = [
  { label: `Athena (${LIBRARY.length})`, on: true },
  { label: "Hermes profiles" },
  { label: "Claude Code subagents" },
  { label: "Your presets" },
];

// "Runs on": the engine a hire starts with (a few of the app's, its labels).
const RUNS_ON = ["Claude Code", "Codex", "Eve", "opencode"] as const;
type RunsOn = (typeof RUNS_ON)[number];

const SHOWN = 9;

const LIBRARY_POINTS: ReadonlyArray<{ title: string; body: string }> = [
  {
    title: `${LIBRARY.length} agents, written for Athena`,
    body: "Engineers, designers, marketers, researchers, a bookkeeper, a World Builder for the voxel world and more, each with the tools and skills its work needs.",
  },
  {
    title: "A starter team in one click",
    body: "Add Athena team on Home hires a Coordinator, an X Writer, an Image Maker, a Researcher and the rest of the starters at once.",
  },
  {
    title: "Or bring your own",
    body: "Use your Hermes profiles, your Claude Code subagents and the agents and teams you saved as presets.",
  },
];

export function AthenaLibrarySection() {
  const [query, setQuery] = useState("");
  const [group, setGroup] = useState<string>("");
  const [runsOn, setRunsOn] = useState<RunsOn>("Claude Code");
  const [hired, setHired] = useState<ReadonlyMap<string, RunsOn>>(new Map());

  const list = useMemo(() => {
    const q = query.trim().toLowerCase();
    return LIBRARY.filter(
      (a) =>
        (!group || (group === STARTER ? a.starter : a.group === group)) &&
        (!q ||
          `${a.name} ${a.group} ${a.description}`.toLowerCase().includes(q)),
    );
  }, [query, group]);

  const hire = (name: string): void =>
    setHired((m) => new Map(m).set(name, runsOn));

  return (
    <section id="library" className="relative px-6 lg:px-12 py-24 lg:py-32">
      <SectionLabel>§03 / the library</SectionLabel>
      <DisplayHeading>
        {LIBRARY.length} specialists.
        <br />
        <span className="text-accent">ready to hire.</span>
      </DisplayHeading>

      <Lede>
        Need someone the team doesn&apos;t have? Hire a specialist from the
        Agent library with its instructions already written. Its role is where
        it&apos;s strongest, not a limit: it takes on nearby work too.
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
            aria-label="Search the agents"
            placeholder='Search the agents: "unity", "seo", "security"…'
            className="w-full bg-bg border border-line focus:border-accent-dim outline-none px-4 py-3 font-sans text-[15px] text-ink placeholder:text-ink-faint"
          />

          <div
            className="flex flex-wrap gap-1.5 mt-4"
            role="radiogroup"
            aria-label="Group"
          >
            {["", STARTER, ...LIBRARY_GROUPS].map((d) => {
              const on = group === d;
              return (
                <button
                  key={d || "all"}
                  type="button"
                  role="radio"
                  aria-checked={on}
                  onClick={() => setGroup(d)}
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
                        {a.group}
                      </div>
                    </div>
                  </div>
                  <p className="font-sans text-[14px] leading-[1.5] text-ink-dim mt-3 flex-1">
                    {a.description}
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
                          href={`${ATHENA_REPO_URL}/blob/${LIBRARY_COMMIT}/${a.path}`}
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
              None of the {LIBRARY.length} match. Try another word.
            </p>
          ) : (
            <p className="font-mono text-[12px] text-ink-faint mt-5">
              Showing {Math.min(SHOWN, list.length)} of {list.length}. The same{" "}
              {LIBRARY.length} come with the app.
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
        <span className="text-accent font-mono">{"//"}</span> The agents are
        Athena&apos;s own, written for it and its tools, and come with the app,
        so the Library works with no download. Hiring one copies its
        instructions into a new agent you can change.
      </p>
    </section>
  );
}
