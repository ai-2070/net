import { FieldLabel as SectionLabel } from "./field/FieldLabel";
import { FieldHeading as DisplayHeading } from "./field/FieldHeading";

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

export function ComponentsSection() {
  return (
    <section
      id="components"
      className="relative pl-6 pr-12 lg:pl-10 lg:pr-24 py-24 lg:py-32"
    >
      <SectionLabel>§09 / components on the mesh</SectionLabel>
      <DisplayHeading>
        four primitives.
        <br />
        one mesh.
      </DisplayHeading>

      <p className="text-[19px] md:text-[21px] text-ink max-w-[34em] leading-[1.5] font-light mb-14">
        The mesh moves bytes. Everything above is a thin, optional layer —
        local-first, feature-flagged, opt-in. Light up the ones you need; the
        wire doesn&apos;t care which.
      </p>

      <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-4 gap-5 lg:gap-6">
        {COMPONENTS.map((c) => (
          <div key={c.name} className="slab slab-lift p-7 flex flex-col">
            <h3 className="font-display font-bold text-[25px] leading-[1.05] text-ink mb-2 tracking-[-0.015em]">
              {c.name}
            </h3>
            <div className="text-[10px] text-ink-dim mb-4 tracking-[0.05em]">
              {c.tagline}
            </div>
            <p className="text-[14px] text-ink-dim leading-[1.6] mb-4 flex-1 max-w-[36em]">
              {c.body}
            </p>
            <div className="flex items-end justify-between gap-4 border-t border-line pt-4">
              <span className="font-mono text-[10.5px] text-ink-dim tracking-[0.04em]">
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
