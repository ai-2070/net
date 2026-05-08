"use client";

import { useEffect, useState } from "react";

import type { RepoInfo } from "@/lib/repo-info";

export function TopStatusBar({ version, codename, buildDate, sha }: RepoInfo) {
  const [evt, setEvt] = useState<string>("8.4M");
  const [p50, setP50] = useState<string>("38ns");

  useEffect(() => {
    const evtId = window.setInterval(() => {
      const base = 8_400_000;
      const jitter = Math.floor((Math.random() - 0.5) * 200_000);
      const n = base + jitter;
      setEvt((n / 1_000_000).toFixed(1) + "M");
    }, 2000);

    const p50Id = window.setInterval(() => {
      const base = 38;
      const j = Math.floor((Math.random() - 0.5) * 6);
      setP50(base + j + "ns");
    }, 2400);

    return () => {
      window.clearInterval(evtId);
      window.clearInterval(p50Id);
    };
  }, []);

  return (
    <div className="fixed top-0 left-0 right-0 h-7 bg-bg border-b border-line flex items-center px-4 text-[10px] text-ink-dim z-[100] tracking-[0.05em]">
      <span className="live-dot inline-flex items-center gap-1.5 text-accent">
        MESH ONLINE
      </span>
      <span className="text-ink-faint mx-3">│</span>
      <span>
        CODENAME:{" "}
        <b className="text-accent font-semibold uppercase">{codename}</b>
      </span>
      <span className="text-ink-faint mx-3">│</span>
      <span>
        EVT/SEC: <b className="text-ink font-semibold">{evt}</b>
      </span>
      <span className="text-ink-faint mx-3">│</span>
      <span>
        P50: <b className="text-ink font-semibold">{p50}</b>
      </span>
      <div className="ml-auto hidden md:flex gap-4">
        <span>{version}</span>
        <span>BUILD: {buildDate}</span>
        <span>
          SHA: <span className="text-accent">{sha}</span>
        </span>
      </div>
    </div>
  );
}
