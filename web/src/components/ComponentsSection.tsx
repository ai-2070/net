import { SectionLabel } from "./SectionLabel";
import { DisplayHeading } from "./DisplayHeading";

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
    tagline: "// typed request/response",
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
    tagline: "// append-only event log",
    body: (
      <>
        The log unbundled and local. 20-byte index records, optional disk
        persistence per channel, atomic backfill-then-live tailing. A Pi keeps a
        tiny log of its own readings; a server keeps a huge one. No cluster
        consensus — log is local, replay is local, retention is local.
      </>
    ),
    stats: "21.3 M append/s · 138 ns tail",
  },
  {
    num: "▸ component.03",
    name: "CortEX",
    tagline: "// RedEX, folded",
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
    stats: "8.98 ns find_unique · 8.87 M ingest/s",
  },
  {
    num: "▸ component.04",
    name: "NetDB",
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
    stats: "6.30 μs open · 48 KB / 1K rows",
  },
];

export function ComponentsSection() {
  return (
    <section id="components" className="border-b border-line px-6 py-20">
      <SectionLabel>§09 / components on the mesh</SectionLabel>
      <DisplayHeading>
        four primitives.
        <br />
        one mesh.
      </DisplayHeading>

      <p className="text-[16px] text-ink max-w-[740px] leading-[1.6] font-light mb-12">
        The mesh moves bytes. Everything above is a thin, optional layer —
        local-first, feature-flagged, opt-in. Light up the ones you need; the
        wire doesn&apos;t care which.
      </p>

      <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-4 gap-px bg-line border border-line">
        {COMPONENTS.map((c) => (
          <div
            key={c.name}
            className="bg-bg p-7 transition-colors hover:bg-bg-2 flex flex-col"
          >
            <div className="text-[10px] text-accent tracking-[0.18em] mb-2.5">
              {c.num}
            </div>
            <h3 className="font-head text-[22px] leading-tight text-ink mb-1.5 tracking-[0.04em] lowercase">
              {c.name}
            </h3>
            <div className="text-[10px] text-ink-dim mb-4 tracking-[0.05em]">
              {c.tagline}
            </div>
            <p className="text-[12px] text-ink-dim leading-[1.6] mb-4 flex-1">
              {c.body}
            </p>
            <div className="border-t border-dashed border-line pt-3 font-mono text-[10px] text-ink-dim tracking-[0.04em]">
              {c.stats}
            </div>
          </div>
        ))}
      </div>
    </section>
  );
}
