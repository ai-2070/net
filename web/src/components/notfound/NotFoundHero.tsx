"use client";

import Link from "next/link";
import { usePathname } from "next/navigation";
import { useCallback, useEffect, useRef, useState } from "react";
import type { TraceEvent } from "./routeSim";

const FIRST_TRACE: ReadonlyArray<TraceEvent> = [
  { id: -1, kind: "hop", text: "looking for a route…" },
];

// The 404 page: a packet that keeps trying to reach a node that is not
// there, and the ways out.
export function NotFoundHero() {
  const pathname = usePathname() || "/";
  const inDocs = pathname === "/docs" || pathname.startsWith("/docs/");
  const [trace, setTrace] = useState<ReadonlyArray<TraceEvent>>(FIRST_TRACE);
  const onTrace = useCallback(
    (e: TraceEvent) => setTrace((t) => [e, ...t].slice(0, 4)),
    [],
  );

  return (
    <section
      id="hero"
      className="relative min-h-[calc(100svh-80px)] flex flex-col"
    >
      <div
        className="absolute inset-y-0 left-1/2 w-screen -translate-x-1/2 overflow-hidden bg-[#070907]"
        aria-hidden
      >
        <RouteMap path={pathname} onTrace={onTrace} />
      </div>
      {/* keep the text side dark enough to read */}
      <div
        aria-hidden
        className="absolute inset-y-0 left-1/2 w-screen -translate-x-1/2 pointer-events-none"
        style={{
          background:
            "linear-gradient(90deg, rgba(11,12,11,0.92) 0%, rgba(11,12,11,0.9) calc(max(0px, 50% - 720px) + min(38%, 560px)), rgba(11,12,11,0.3) calc(max(0px, 50% - 720px) + min(62%, 880px)), rgba(11,12,11,0) calc(max(0px, 50% - 720px) + min(78%, 1100px))), linear-gradient(0deg, rgba(11,12,11,1) 0%, rgba(11,12,11,0) 22%)",
        }}
      />
      <div
        aria-hidden
        className="absolute inset-y-0 left-1/2 w-screen -translate-x-1/2 pointer-events-none lg:hidden"
        style={{
          background:
            "linear-gradient(180deg, rgba(11,12,11,0.1) 0%, rgba(11,12,11,0.62) 40%, rgba(11,12,11,0.8) 100%)",
        }}
      />

      <div className="relative flex-1 flex flex-col px-6 lg:px-12 pt-12 lg:pt-14 pb-12">
        <div className="my-auto">
          <div
            className="field-fade font-mono text-[12px] tracking-[0.16em] mb-8 flex flex-wrap gap-3 items-center"
            style={{ animationDelay: "0.05s" }}
          >
            <span className="text-bg bg-warn px-2 py-1 font-bold">404</span>
            <span className="text-warn border border-warn/50 px-2 py-1">
              ROUTE NOT FOUND
            </span>
          </div>

          <h1
            className="font-display font-bold uppercase leading-[0.86] tracking-[-0.04em] text-ink"
            style={{ fontSize: "clamp(42px, 8.3vw, 132px)" }}
          >
            <span
              className="field-rise block whitespace-nowrap"
              style={{ animationDelay: "0.1s" }}
            >
              no route
            </span>{" "}
            <span
              className="field-rise block whitespace-nowrap text-accent"
              style={{ animationDelay: "0.24s" }}
            >
              to this page.
            </span>
          </h1>

          <div className="field-fade" style={{ animationDelay: "0.6s" }}>
            <p className="text-[20px] md:text-[22px] text-ink mt-9 max-w-[28em] leading-[1.45] font-light">
              This page isn&apos;t on the mesh. It may have moved, or the link
              has a typo.
            </p>

            <div className="mt-7 inline-flex max-w-full items-center gap-3 border border-line bg-bg/70 px-4 py-2.5 font-mono text-[13px]">
              <span className="text-ink-faint uppercase tracking-[0.14em] text-[11px] shrink-0">
                requested
              </span>
              <span className="text-ink truncate">{pathname}</span>
              <span className="flex items-center gap-1.5 text-warn text-[11px] uppercase tracking-[0.14em] shrink-0">
                <span
                  aria-hidden
                  className="w-1.5 h-1.5 bg-warn inline-block"
                />
                unreachable
              </span>
            </div>

            <div className="mt-9 flex gap-4 flex-wrap items-center">
              {inDocs ? (
                <>
                  <Link href="/docs" className="btn-field">
                    Open the docs
                  </Link>
                  <Link href="/" className="btn-ghost-field">
                    ← Back to home
                  </Link>
                </>
              ) : (
                <>
                  <Link href="/" className="btn-field">
                    ← Back to home
                  </Link>
                  <Link href="/docs" className="btn-ghost-field">
                    Read the docs
                  </Link>
                </>
              )}
            </div>

            <p className="mt-7 font-mono text-[13px] text-ink-dim tracking-[0.04em]">
              or try{" "}
              <Link href="/games" className="text-ink hover:text-accent">
                Games
              </Link>
              <span className="text-ink-faint"> · </span>
              <Link href="/blacksite" className="text-ink hover:text-accent">
                Blacksite
              </Link>
              <span className="text-ink-faint"> · </span>
              <Link
                href="/docs/releases"
                className="text-ink hover:text-accent"
              >
                Releases
              </Link>
            </p>
          </div>
        </div>

        <div className="pt-12 flex flex-wrap gap-x-10 gap-y-6 items-end justify-between">
          <Trace rows={trace} />
          <div className="stencil">err · no_route · 404</div>
        </div>
      </div>
    </section>
  );
}

const TRACE_ROWS = 4;

function Trace({ rows }: { rows: ReadonlyArray<TraceEvent> }) {
  // Always four rows and a fixed width, so new lines never move the text
  // above (the hero centres its content in the space that is left).
  const blanks = Math.max(0, TRACE_ROWS - rows.length);
  return (
    <div className="font-mono text-[12px] w-[26em] max-w-full" aria-hidden>
      <div className="stencil mb-2 flex items-center gap-2">
        <span className="w-1.5 h-1.5 bg-accent inline-block" />
        packet.trace
      </div>
      <ul className="flex flex-col gap-1">
        {rows.map((r, i) => (
          <li
            key={r.id}
            className={`truncate ${
              r.kind === "fail"
                ? "text-warn"
                : i === 0
                  ? "text-ink"
                  : "text-ink-dim"
            }`}
          >
            {r.text}
          </li>
        ))}
        {Array.from({ length: blanks }, (_, i) => (
          <li key={`blank-${i}`}>&nbsp;</li>
        ))}
      </ul>
    </div>
  );
}

// The mesh, drawn by three.js in its own chunk once the page is up.
function RouteMap({
  path,
  onTrace,
}: {
  path: string;
  onTrace: (e: TraceEvent) => void;
}) {
  const ref = useRef<HTMLDivElement | null>(null);
  const traceRef = useRef(onTrace);
  const [ready, setReady] = useState(false);

  useEffect(() => {
    traceRef.current = onTrace;
  }, [onTrace]);

  useEffect(() => {
    const host = ref.current;
    if (!host) return;
    let stop: (() => void) | null = null;
    let cancelled = false;
    const reduced = window.matchMedia(
      "(prefers-reduced-motion: reduce)",
    ).matches;
    // Fetch three.js now, but build the scene only once the headline has
    // risen and the browser is idle, so the two don't fight for frames.
    const sim = import("./routeSim");
    let idle = 0;
    const start = (): void => {
      sim
        .then(({ startRoute }) => {
          if (cancelled) return;
          stop = startRoute(host, {
            reduced,
            path,
            onTrace: (e) => traceRef.current(e),
            onReady: () => setReady(true),
          });
        })
        .catch(() => {
          // no WebGL: the page still works on the plain background
        });
    };
    const wait = window.setTimeout(() => {
      if ("requestIdleCallback" in window) {
        idle = window.requestIdleCallback(start, { timeout: 800 });
      } else start();
    }, 650);
    return () => {
      cancelled = true;
      window.clearTimeout(wait);
      if (idle) window.cancelIdleCallback(idle);
      stop?.();
    };
  }, [path]);

  return (
    <div
      ref={ref}
      // Not quite 0 while it warms up: the browser skips drawing a fully
      // transparent layer, and its first real draw would land mid-fade.
      className={`absolute inset-0 transition-opacity duration-1000 ${
        ready ? "opacity-100" : "opacity-[0.01]"
      }`}
    />
  );
}
