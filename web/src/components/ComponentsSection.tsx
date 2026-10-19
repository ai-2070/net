"use client";

import { FieldLabel as SectionLabel } from "./field/FieldLabel";
import { FieldHeading as DisplayHeading } from "./field/FieldHeading";
import { Lede } from "./field/Lede";
import { IsoDiagram, type IsoFigure } from "./field/IsoDiagram";
import { useWorldVariant } from "./field/WorldVariant";

interface ComponentSpec {
  num: string;
  name: string;
  tagline: string;
  body: React.ReactNode;
  stats: string;
}

const COMPONENTS: readonly ComponentSpec[] = [
  {
    num: "▸ component.01",
    name: "nRPC",
    tagline: "// native rpc primitive",
    body: (
      <>
        Request/response semantics built from a pair of streams. A server
        registers a handler with{" "}
        <code className="font-mono text-accent">serve_rpc</code>; clients
        dispatch with <code className="font-mono text-accent">call_typed</code>.
        The streams stay primitive — nRPC just wraps them in a typed handle and
        completes when the response lands.
      </>
    ),
    stats: "TypedMeshRpc · paired streams · zero new wire",
  },
  {
    num: "▸ component.02",
    name: "RedEX",
    tagline: "// stream as state",
    body: (
      <>
        The log unbundled and local. 20-byte index records, optional disk
        persistence per channel, atomic backfill-then-live tailing. A Pi keeps a
        tiny log of its own readings; a server keeps a huge one. No cluster
        consensus — log is local, replay is local, retention is local.
      </>
    ),
    stats: "28.7 M append/s · 161 ns tail",
  },
  {
    num: "▸ component.03",
    name: "CortEX",
    tagline: "// folded RedEX streams",
    body: (
      <>
        A reactive, queryable projection of the log, updated event-by-event.
        Your &quot;database&quot; isn&apos;t a process you connect to —
        it&apos;s a{" "}
        <code className="font-mono text-accent">Vec&lt;Task&gt;</code> or{" "}
        <code className="font-mono text-accent">
          HashMap&lt;Uuid, Memory&gt;
        </code>{" "}
        in your code, updating as events fold in. Queries are direct memory
        access.
      </>
    ),
    stats: "9.11 ns find_unique · 4.07 M ingest/s",
  },
  {
    num: "▸ component.04",
    name: "NETDB",
    tagline: "// unified query façade",
    body: (
      <>
        One handle bundling typed collections under{" "}
        <code className="font-mono text-accent">db.tasks</code>,{" "}
        <code className="font-mono text-accent">db.memories</code>, and friends.
        Prisma-style <code className="font-mono text-accent">find_unique</code>{" "}
        / <code className="font-mono text-accent">find_many</code> across Rust,
        TypeScript, and Python — whole-database snapshots round-trip between
        languages.
      </>
    ),
    stats: "26.4 μs decode · 48 KB / 1K rows",
  },
];

const COMPONENT_FIGURES: ReadonlyArray<IsoFigure> = [
  "nrpc",
  "redex",
  "cortex",
  "netdb",
];

export function ComponentsSection() {
  const variant = useWorldVariant();
  return (
    <section id="components" className="relative px-6 lg:px-12 py-24 lg:py-32">
      <SectionLabel>§09 / components on the mesh</SectionLabel>
      <DisplayHeading>
        four primitives.
        <br />
        one mesh.
      </DisplayHeading>

      <Lede>
        The mesh moves bytes. Everything above is a thin, optional layer —
        local-first, feature-flagged, opt-in. Light up the ones you need; the
        wire doesn&apos;t care which.
      </Lede>

      <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-4 gap-5 lg:gap-6">
        {COMPONENTS.map((c, i) => (
          <div key={c.name} className="slab slab-lift p-7 flex flex-col">
            {variant !== "v1" && COMPONENT_FIGURES[i] ? (
              <IsoDiagram
                figure={COMPONENT_FIGURES[i]}
                className="-mx-2 mb-6"
              />
            ) : null}
            <h3 className="font-display font-bold text-[25px] leading-[1.05] text-ink mb-2 tracking-[-0.015em]">
              {c.name}
            </h3>
            <div className="text-[12px] text-ink-dim mb-4 tracking-[0.05em]">
              {c.tagline}
            </div>
            <p className="text-[16px] text-ink-dim leading-[1.55] mb-4 flex-1 max-w-[36em]">
              {c.body}
            </p>
            <div className="flex items-end justify-between gap-4 border-t border-line pt-4">
              <span className="font-mono text-[13px] text-ink-dim tracking-[0.04em]">
                {c.stats}
              </span>
              <span className="stencil shrink-0 normal-case">{c.num}</span>
            </div>
          </div>
        ))}
      </div>
    </section>
  );
}
