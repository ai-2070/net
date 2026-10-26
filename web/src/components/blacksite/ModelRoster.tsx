"use client";

import { useState } from "react";

// A working copy of the per-agent model and effort pickers. The models,
// their notes and each engine's effort range are Blacksite's own
// (web/models.ts, shared/effort.ts); switching to a model whose engine
// lacks the chosen level moves to the nearest one, as the app does.

const EFFORTS = ["minimal", "low", "medium", "high", "xhigh", "max"] as const;
type Effort = (typeof EFFORTS)[number];
const EFFORT_SHORT: Record<Effort, string> = {
  minimal: "Min",
  low: "Low",
  medium: "Med",
  high: "High",
  xhigh: "X-High",
  max: "Max",
};
const EFFORT_NAME: Record<Effort, string> = {
  minimal: "Minimal",
  low: "Low",
  medium: "Medium",
  high: "High",
  xhigh: "Extra high",
  max: "Max",
};

type Engine = "claude" | "codex" | "openrouter" | "local";
const RANGE: Record<Engine, [Effort, Effort]> = {
  claude: ["low", "max"], // claude --effort
  codex: ["minimal", "xhigh"], // model_reasoning_effort
  openrouter: ["minimal", "xhigh"], // OpenRouter reasoning.effort
  local: ["low", "high"], // reasoning_effort on a server on this PC
};
const levels = (e: Engine): Effort[] =>
  EFFORTS.slice(EFFORTS.indexOf(RANGE[e][0]), EFFORTS.indexOf(RANGE[e][1]) + 1);
function nearest(e: Engine, want: Effort): Effort {
  const ls = levels(e);
  if (ls.includes(want)) return want;
  const first = ls[0] ?? "low";
  const last = ls[ls.length - 1] ?? "high";
  return EFFORTS.indexOf(want) < EFFORTS.indexOf(first) ? first : last;
}

interface Model {
  id: string;
  name: string;
  engine: Engine;
  via: string;
  note: string;
}

const MODELS: ReadonlyArray<Model> = [
  {
    id: "opus",
    name: "Claude Opus 5.5",
    engine: "claude",
    via: "Claude Code",
    note: "most capable",
  },
  {
    id: "sonnet",
    name: "Claude Sonnet 5.5",
    engine: "claude",
    via: "Claude Code",
    note: "fast and smart",
  },
  {
    id: "haiku",
    name: "Claude Haiku 5.5",
    engine: "claude",
    via: "Claude Code",
    note: "fastest, cheapest",
  },
  {
    id: "fable",
    name: "Claude Fable 5.1",
    engine: "claude",
    via: "Claude Code",
    note: "creative writing",
  },
  {
    id: "gpt",
    name: "GPT-6 Astra",
    engine: "codex",
    via: "Codex",
    note: "OpenAI flagship",
  },
  {
    id: "deepseek",
    name: "DeepSeek",
    engine: "openrouter",
    via: "OpenRouter",
    note: "priced per M tokens",
  },
  {
    id: "gemini",
    name: "Gemini",
    engine: "openrouter",
    via: "OpenRouter",
    note: "priced per M tokens",
  },
  {
    id: "llama",
    name: "Llama",
    engine: "openrouter",
    via: "OpenRouter",
    note: "priced per M tokens",
  },
  {
    id: "ollama",
    name: "A local model",
    engine: "local",
    via: "Ollama",
    note: "runs on your own PC",
  },
];
const MODEL = new Map(MODELS.map((m) => [m.id, m]));

interface Seat {
  name: string;
  emoji: string;
  job: string;
  lead?: boolean;
  model: string;
  effort: Effort;
}

const START: ReadonlyArray<Seat> = [
  {
    name: "Studio Producer",
    emoji: "🎬",
    job: "plans the work, hands it out",
    lead: true,
    model: "sonnet",
    effort: "medium",
  },
  {
    name: "Story Writer",
    emoji: "✍️",
    job: "quest text and the trailer script",
    model: "fable",
    effort: "high",
  },
  {
    name: "3D Artist",
    emoji: "🧊",
    job: "helms, swords and set pieces in Blender",
    model: "opus",
    effort: "max",
  },
  {
    name: "Technical Artist",
    emoji: "🛠",
    job: "budgets, rigs, checks it in the engine",
    model: "gpt",
    effort: "high",
  },
  {
    name: "Community Manager",
    emoji: "📣",
    job: "posts, replies and the patch notes",
    model: "deepseek",
    effort: "low",
  },
  {
    name: "QA Tester",
    emoji: "🧐",
    job: "plays every build, files the bugs",
    model: "deepseek",
    effort: "minimal",
  },
  {
    name: "Asset Tagger",
    emoji: "🏷️",
    job: "names and sorts every file",
    model: "ollama",
    effort: "low",
  },
];

export function ModelRoster() {
  const [seats, setSeats] = useState<ReadonlyArray<Seat>>(START);

  const setModel = (i: number, id: string): void =>
    setSeats((s) =>
      s.map((seat, k) => {
        if (k !== i) return seat;
        const m = MODEL.get(id);
        return m
          ? { ...seat, model: id, effort: nearest(m.engine, seat.effort) }
          : seat;
      }),
    );
  const setEffort = (i: number, effort: Effort): void =>
    setSeats((s) => s.map((seat, k) => (k === i ? { ...seat, effort } : seat)));

  const engines = new Set(seats.map((s) => MODEL.get(s.model)?.via));

  return (
    <div data-ui className="slab">
      <div className="flex items-center justify-between gap-4 border-b border-line px-4 py-2 bg-bg-2/60">
        <span className="font-mono text-[12px] tracking-[0.14em] uppercase text-accent-dim">
          <span className="text-accent">▸</span> rose &amp; blade launch team
        </span>
        <span className="font-mono text-[12px] text-ink-dim">
          {seats.length} agents · {engines.size} engines
        </span>
      </div>

      <div className="divide-y divide-line bg-black/30">
        {seats.map((seat, i) => {
          const m = MODEL.get(seat.model);
          if (!m) return null;
          const ls = levels(m.engine);
          return (
            <div
              key={seat.name}
              className="grid grid-cols-1 md:grid-cols-[minmax(0,1fr)_minmax(0,1.05fr)] gap-x-5 gap-y-3 px-5 py-4"
            >
              <div className="flex items-center gap-3 min-w-0">
                <span
                  aria-hidden
                  className="grid place-items-center w-9 h-9 shrink-0 border border-line bg-bg-2 text-[18px]"
                >
                  {seat.emoji}
                </span>
                <div className="min-w-0">
                  <div className="font-display font-bold text-[16px] leading-[1.15] text-ink">
                    {seat.lead ? (
                      <span className="text-warn mr-1.5" title="Team lead">
                        ★
                      </span>
                    ) : null}
                    {seat.name}
                  </div>
                  <div className="font-sans text-[13px] text-ink-dim truncate">
                    {seat.job}
                  </div>
                  <div className="font-mono text-[11px] text-ink-faint mt-0.5">
                    runs on {m.via}
                  </div>
                </div>
              </div>

              <div className="flex flex-col gap-2 min-w-0">
                <label className="flex items-center gap-2 min-w-0">
                  <span className="sr-only">Model for {seat.name}</span>
                  <select
                    value={seat.model}
                    onChange={(e) => setModel(i, e.target.value)}
                    className="min-w-0 flex-1 bg-bg border border-line hover:border-ink-faint focus:border-accent-dim outline-none px-2.5 py-1.5 font-mono text-[12.5px] text-ink cursor-pointer"
                  >
                    {(
                      ["Claude Code", "Codex", "OpenRouter", "Ollama"] as const
                    ).map((via) => (
                      <optgroup key={via} label={via}>
                        {MODELS.filter((x) => x.via === via).map((x) => (
                          <option key={x.id} value={x.id}>
                            {x.name} — {x.note}
                          </option>
                        ))}
                      </optgroup>
                    ))}
                  </select>
                </label>
                <div className="flex items-center gap-2 flex-wrap">
                  <span className="font-mono text-[10.5px] tracking-[0.14em] uppercase text-ink-faint w-[46px]">
                    effort
                  </span>
                  <div
                    className="flex border border-line divide-x divide-line"
                    role="radiogroup"
                    aria-label={`Effort for ${seat.name}`}
                  >
                    {ls.map((lv) => {
                      const on = seat.effort === lv;
                      return (
                        <button
                          key={lv}
                          type="button"
                          role="radio"
                          aria-checked={on}
                          title={`${EFFORT_NAME[lv]} effort`}
                          onClick={() => setEffort(i, lv)}
                          className={`px-2 py-0.5 font-mono text-[11px] cursor-pointer transition-colors ${
                            on
                              ? "bg-accent text-bg font-semibold"
                              : "text-ink-dim hover:text-ink hover:bg-bg-2"
                          }`}
                        >
                          {EFFORT_SHORT[lv]}
                        </button>
                      );
                    })}
                  </div>
                </div>
              </div>
            </div>
          );
        })}
      </div>

      <div className="border-t border-line px-5 py-4 font-sans text-[14px] text-ink-dim leading-[1.6]">
        <span className="text-accent font-mono">{"//"}</span> Try it: give the
        writer a different model or turn the 3D Artist down. Each engine has its
        own effort range, so a level it doesn&apos;t have moves to the nearest
        one.
      </div>
    </div>
  );
}
