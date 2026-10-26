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
    import("./stationSim")
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
    return () => {
      cancelled = true;
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
        className={`absolute inset-0 transition-opacity duration-700 ${
          ready ? "opacity-100" : "opacity-0"
        }`}
      />
    </>
  );
}
