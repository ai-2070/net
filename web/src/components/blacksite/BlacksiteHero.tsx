"use client";

import { useCallback, useState } from "react";
import { BLACKSITE_DOWNLOAD_URL, BLACKSITE_REPO_URL } from "./links";
import { StationMap } from "./StationMap";
import type { FeedEvent } from "./stationSim";

// The Station's own "Live" feed, bottom left of the dashboard: who called
// what. These first lines show until the map starts; then each thing the
// made-up Station does is added on top.
const FIRST_FEED: ReadonlyArray<FeedEvent> = [
  { id: -1, who: "3D Artist", what: "Blender" },
  { id: -2, who: "Texture Artist", what: "ComfyUI" },
  { id: -3, who: "Technical Artist", what: "needs you" },
  { id: -4, who: "Historian", what: "Web search" },
];

function LiveFeed({ rows }: { rows: ReadonlyArray<FeedEvent> }) {
  return (
    <div
      className="font-mono text-[12px] leading-[1.9] min-w-0 max-w-full"
      aria-hidden
    >
      <div className="flex items-center gap-2 text-warn tracking-[0.16em] uppercase mb-1">
        <span className="w-1.5 h-1.5 bg-warn inline-block animate-pulse-dot" />
        live
      </div>
      {rows.map((r, i) => (
        <div
          key={r.id}
          className={`truncate ${i === 0 && r.id > 0 ? "field-fade" : ""}`}
        >
          <span className="text-ink-faint">just now</span>{" "}
          <span className="text-ink">{r.who}</span>{" "}
          <span className="text-ink-dim">→ {r.what}</span>
        </div>
      ))}
    </div>
  );
}

export function BlacksiteHero() {
  const [feed, setFeed] = useState<ReadonlyArray<FeedEvent>>(FIRST_FEED);
  const onFeed = useCallback(
    (e: FeedEvent) => setFeed((f) => [e, ...f].slice(0, 4)),
    [],
  );

  return (
    <section
      id="hero"
      className="relative min-h-[calc(100svh-80px)] flex flex-col"
    >
      {/* a live, made-up Station across the full width of the window */}
      <div
        className="absolute inset-y-0 left-1/2 w-screen -translate-x-1/2 overflow-hidden bg-[#070907]"
        aria-hidden
      >
        <StationMap onFeed={onFeed} />
      </div>
      {/* the text side stays dark enough to read, anchored to the content
          column's left edge like the /games hero */}
      <div
        aria-hidden
        className="absolute inset-y-0 left-1/2 w-screen -translate-x-1/2 pointer-events-none"
        style={{
          background:
            "linear-gradient(90deg, rgba(11,12,11,0.94) 0%, rgba(11,12,11,0.96) max(0px, 50% - 720px), rgba(11,12,11,0.93) calc(max(0px, 50% - 720px) + min(42%, 600px)), rgba(11,12,11,0.45) calc(max(0px, 50% - 720px) + min(62%, 890px)), rgba(11,12,11,0) calc(max(0px, 50% - 720px) + min(76%, 1090px))), linear-gradient(0deg, rgba(11,12,11,1) 0%, rgba(11,12,11,0) 24%)",
        }}
      />
      {/* phones: the map sits behind every line of text, so dim it more */}
      <div
        aria-hidden
        className="absolute inset-y-0 left-1/2 w-screen -translate-x-1/2 pointer-events-none lg:hidden"
        style={{
          background:
            "linear-gradient(180deg, rgba(11,12,11,0.2) 0%, rgba(11,12,11,0.78) 34%, rgba(11,12,11,0.86) 100%)",
        }}
      />

      <div className="relative flex-1 flex flex-col px-6 lg:px-12 pt-12 lg:pt-14 pb-12">
        <div className="my-auto">
          <div
            className="field-fade font-mono text-[12px] tracking-[0.16em] mb-8 flex flex-wrap gap-3 items-center"
            style={{ animationDelay: "0.05s" }}
          >
            <span className="text-bg bg-accent px-2 py-1 font-bold">NEW</span>
            <span className="text-accent border border-accent-dim px-2 py-1">
              BLACKSITE 0.1 · DESKTOP APP
            </span>
          </div>

          <h1
            className="font-display font-bold uppercase leading-[0.86] tracking-[-0.04em] text-ink"
            style={{ fontSize: "clamp(50px, 8.3vw, 132px)" }}
          >
            <span
              className="field-rise block"
              style={{ animationDelay: "0.1s" }}
            >
              your agents.
            </span>{" "}
            <span
              className="field-rise block text-accent"
              style={{ animationDelay: "0.24s" }}
            >
              one station.
            </span>
          </h1>

          <div className="field-fade" style={{ animationDelay: "0.6s" }}>
            <p className="text-[20px] md:text-[22px] text-ink mt-9 max-w-[30em] leading-[1.45] font-light">
              Run AI agents and teams from one local workspace. Give each agent
              a role, a model and the tools it needs, and watch them work on a
              live 3D map.
            </p>

            <p className="text-[17px] text-ink-dim mt-6 max-w-[36em] leading-[1.6]">
              No account and no login: it runs on your computer. Works with
              Claude Code, Codex, local models and MCP. Free and open source.
            </p>

            <div className="mt-9 flex gap-4 flex-wrap items-center">
              <a href={BLACKSITE_DOWNLOAD_URL} className="btn-field">
                ↓ Download
              </a>
              <a
                href={BLACKSITE_REPO_URL}
                target="_blank"
                rel="noopener noreferrer"
                className="btn-ghost-field"
              >
                View on GitHub <span className="text-sm">↗</span>
              </a>
            </div>
          </div>
        </div>

        <div className="pt-12 flex flex-wrap gap-x-10 gap-y-6 items-end justify-between">
          <LiveFeed rows={feed} />
          <div className="stencil flex flex-wrap gap-x-6 gap-y-2 items-center">
            <span>windows · macos · linux</span>
            <span>mit or apache-2.0</span>
          </div>
        </div>
      </div>
    </section>
  );
}
