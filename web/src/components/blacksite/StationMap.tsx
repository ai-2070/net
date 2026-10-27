"use client";

import Image from "next/image";
import { useEffect, useRef, useState } from "react";
import type { FeedEvent } from "./stationSim";

// The hero's background: a live, made-up Station. three.js is loaded in its
// own chunk after the page is up; until the first frame is drawn (or if the
// browser has no WebGL) the dashboard screenshot shows instead.
export function StationMap({ onFeed }: { onFeed?: (e: FeedEvent) => void }) {
  const ref = useRef<HTMLDivElement | null>(null);
  const feedRef = useRef(onFeed);
  const [ready, setReady] = useState(false);

  useEffect(() => {
    feedRef.current = onFeed;
  }, [onFeed]);

  useEffect(() => {
    const host = ref.current;
    if (!host) return;
    let stop: (() => void) | null = null;
    let cancelled = false;
    const reduced = window.matchMedia(
      "(prefers-reduced-motion: reduce)",
    ).matches;
    // Fetch three.js now, but build the map only once the headline has risen
    // and the browser is idle: the screenshot covers the wait, and by then the
    // browser's GPU process is up, so creating the WebGL context is quick.
    const sim = import("./stationSim");
    let idle = 0;
    const start = (): void => {
      sim
        .then(({ startStation }) => {
          if (cancelled) return;
          stop = startStation(host, {
            reduced,
            onFeed: (e) => feedRef.current?.(e),
            onReady: () => setReady(true),
          });
        })
        .catch(() => {
          // the screenshot stays
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
  }, []);

  return (
    <>
      <Image
        src="/blacksite/hero.webp"
        alt=""
        fill
        priority
        sizes="100vw"
        className={`object-cover object-[72%_30%] transition-opacity duration-700 ${
          ready ? "opacity-0" : "opacity-90"
        }`}
      />
      <div
        ref={ref}
        // Not quite 0 while it warms up: the browser skips drawing a fully
        // transparent layer, and its first real draw would land mid-fade.
        className={`absolute inset-0 transition-opacity duration-700 ${
          ready ? "opacity-100" : "opacity-[0.01]"
        }`}
      />
    </>
  );
}
