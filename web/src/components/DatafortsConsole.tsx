"use client";

import { useState, useEffect } from "react";

interface PoolNode {
  id: string;
  fill: number;
}

interface PoolEvent {
  id: number;
  ts: string;
  kind: "push" | "heat" | "cool" | "absorb";
  body: string;
}

const POOL_INITIAL: ReadonlyArray<PoolNode> = [
  { id: "node.0x7af3", fill: 0.64 },
  { id: "node.0x2c91", fill: 0.91 },
  { id: "node.0xeb29", fill: 0.31 },
  { id: "node.0xfbb1", fill: 0.78 },
  { id: "node.0x9a3e", fill: 0.22 },
];

const POOL_HIGH = 0.85;
const POOL_LOW = 0.3;

function poolTs(): string {
  const d = new Date();
  const m = String(d.getMinutes()).padStart(2, "0");
  const s = String(d.getSeconds()).padStart(2, "0");
  const ms = String(d.getMilliseconds()).padStart(3, "0");
  return `${m}:${s}.${ms}`;
}

function shortHash(): string {
  return Math.floor(Math.random() * 0xffffff)
    .toString(16)
    .padStart(6, "0");
}

function pickHumanBytes(min: number, max: number): string {
  const mb = min + Math.random() * (max - min);
  return mb >= 1024 ? `${(mb / 1024).toFixed(1)}G` : `${mb.toFixed(1)}M`;
}

function renderBar(fill: number, width: number): string {
  const filled = Math.round(fill * width);
  return "▰".repeat(filled) + "░".repeat(width - filled);
}

const BAR_WIDTH = 22;

export function DatafortsConsole() {
  const [nodes, setNodes] = useState<ReadonlyArray<PoolNode>>(POOL_INITIAL);
  const [events, setEvents] = useState<ReadonlyArray<PoolEvent>>([]);

  useEffect(() => {
    let eventCounter = 0;

    const pushEvent = (kind: PoolEvent["kind"], body: string): void => {
      eventCounter += 1;
      const e: PoolEvent = { id: eventCounter, ts: poolTs(), kind, body };
      setEvents((prev) => [...prev.slice(-5), e]);
    };

    // seed initial event log so the panel doesn't look empty
    pushEvent("push", `0x4d8d → 0xeb29 · 8.4M · accepted · 204ms`);
    pushEvent("heat", `0x7e3a · rate 0.78 · gravity → 0x7af3`);
    pushEvent("cool", `0xb547 · rate 0.12 · evictable`);
    pushEvent("absorb", `0x9a3e · free +18% · open`);
    pushEvent("push", `0x2c91 → 0x9a3e · 18.2M · accepted · 156ms`);

    const id = window.setInterval(() => {
      setNodes((prev) => {
        const next = prev.map((n) => {
          const drift = (Math.random() - 0.45) * 0.05;
          return {
            ...n,
            fill: Math.max(0.12, Math.min(0.96, n.fill + drift)),
          };
        });

        // any node over high-water → overflow into lowest neighbor
        const overIdx = next.findIndex((n) => n.fill >= POOL_HIGH);
        if (overIdx >= 0) {
          let toIdx = -1;
          let minFill = 1;
          for (let i = 0; i < next.length; i++) {
            if (i === overIdx) continue;
            const f = next[i]?.fill ?? 1;
            if (f < minFill) {
              minFill = f;
              toIdx = i;
            }
          }
          if (toIdx >= 0) {
            const amount = 0.1;
            const from = next[overIdx];
            const to = next[toIdx];
            if (from && to) {
              next[overIdx] = { ...from, fill: from.fill - amount };
              next[toIdx] = { ...to, fill: Math.min(0.96, to.fill + amount) };
              const size = pickHumanBytes(8, 220);
              const ms = Math.floor(150 + Math.random() * 220);
              const fromShort = from.id.slice(-4);
              const toShort = to.id.slice(-4);
              pushEvent(
                "push",
                `0x${shortHash().slice(0, 4)} · 0x${fromShort} → 0x${toShort} · ${size} · ${ms}ms`,
              );
            }
          }
        }

        // occasional heat/cool/absorb events
        const r = Math.random();
        if (r < 0.22) {
          const rate = (0.4 + Math.random() * 0.5).toFixed(2);
          pushEvent(
            "heat",
            `0x${shortHash().slice(0, 4)} · rate ${rate} · gravity active`,
          );
        } else if (r < 0.4) {
          const rate = (0.05 + Math.random() * 0.18).toFixed(2);
          pushEvent(
            "cool",
            `0x${shortHash().slice(0, 4)} · rate ${rate} · evictable`,
          );
        } else if (r < 0.5) {
          const node = next[Math.floor(Math.random() * next.length)];
          if (node && node.fill < 0.5) {
            const freePct = Math.round((1 - node.fill) * 100);
            pushEvent(
              "absorb",
              `${node.id.slice(-6)} · free ${freePct}% · open`,
            );
          }
        }

        return next;
      });
    }, 1400);

    return () => window.clearInterval(id);
  }, []);

  const totalFill =
    nodes.reduce((acc, n) => acc + n.fill, 0) / Math.max(1, nodes.length);
  const totalBar = renderBar(totalFill, 36);

  return (
    <div className="border border-line bg-bg-2 overflow-hidden font-mono text-[12px] leading-[1.75]">
      {/* terminal title bar */}
      <div className="flex items-center justify-between border-b border-line px-4 py-2 text-[10px] tracking-[0.14em] text-ink-dim uppercase">
        <span className="flex items-center gap-3">
          <span className="inline-flex gap-1">
            <span className="frame-dot-r w-[7px] h-[7px] rounded-full" />
            <span className="frame-dot-y w-[7px] h-[7px] rounded-full" />
            <span className="frame-dot-g w-[7px] h-[7px] rounded-full" />
          </span>
          <span className="text-accent">$</span>
          <span>
            net dataforts status{" "}
            <span className="text-ink-faint">--live --pool=mesh</span>
          </span>
        </span>
        <span className="flex items-center gap-1.5 normal-case tracking-normal">
          <span className="w-1.5 h-1.5 rounded-full bg-accent inline-block animate-pulse-dot" />
          <span className="text-accent">live</span>
        </span>
      </div>

      <div className="px-5 py-4">
        <div className="text-ink-dim flex items-center gap-3 whitespace-nowrap">
          <span>┌─ mesh storage pool</span>
          <span className="flex-1 border-t border-dashed border-line-/40 hidden" />
          <span className="text-ink-faint">5 nodes · 892 GB cap</span>
        </div>
        <div className="text-ink mt-1 flex items-center gap-3 whitespace-nowrap">
          <span className="text-ink-faint">│</span>
          <span className="text-ink-dim">pressure</span>
          <span className="text-accent">{totalBar}</span>
          <span className="text-ink">{Math.round(totalFill * 100)}%</span>
          <span className="text-ink-faint">·</span>
          <span
            className={
              totalFill >= POOL_HIGH
                ? "text-warn"
                : totalFill <= POOL_LOW
                  ? "text-cyan"
                  : "text-ink-dim"
            }
          >
            {totalFill >= POOL_HIGH
              ? "OVER"
              : totalFill <= POOL_LOW
                ? "COLD"
                : "STEADY"}
          </span>
        </div>
        <div className="text-ink-dim mt-1">├─ nodes</div>

        {nodes.map((n, i) => {
          const isOver = n.fill >= POOL_HIGH;
          const isUnder = n.fill <= POOL_LOW;
          const tag = isOver ? "PUSH" : isUnder ? "RECV" : "····";
          const barColor = isOver
            ? "text-warn"
            : isUnder
              ? "text-cyan"
              : "text-accent";
          const tagColor = isOver
            ? "text-warn"
            : isUnder
              ? "text-cyan"
              : "text-ink-faint";
          const tree = i === nodes.length - 1 ? "└─" : "├─";
          return (
            <div
              key={n.id}
              className="flex items-center gap-3 whitespace-nowrap"
            >
              <span className="text-ink-faint">│ {tree}</span>
              <span className="text-ink">{n.id}</span>
              <span className={barColor}>{renderBar(n.fill, BAR_WIDTH)}</span>
              <span className="text-ink">
                {String(Math.round(n.fill * 100)).padStart(3, " ")}%
              </span>
              <span className={tagColor}>{tag}</span>
            </div>
          );
        })}

        <div className="text-ink-dim mt-3">├─ recent events</div>
        <div className="min-h-[120px]">
          {events.map((e) => {
            const kindColor =
              e.kind === "push"
                ? "text-accent"
                : e.kind === "heat"
                  ? "text-accent-dim"
                  : e.kind === "cool"
                    ? "text-cyan"
                    : "text-ink-dim";
            return (
              <div
                key={e.id}
                className="event-line-in flex items-baseline gap-3 whitespace-nowrap overflow-hidden"
              >
                <span className="text-ink-faint">│</span>
                <span className="text-ink-faint" style={{ minWidth: "9ch" }}>
                  {e.ts}
                </span>
                <span className={`${kindColor}`} style={{ minWidth: "7ch" }}>
                  [{e.kind}]
                </span>
                <span className="text-ink-dim flex-1 truncate">{e.body}</span>
              </div>
            );
          })}
        </div>
        <div className="text-ink-dim mt-1">└─ end of stream</div>

        <div className="mt-4 text-ink-faint text-[10px] tracking-[0.04em]">
          ▸ press <span className="text-accent">^C</span> to detach · gravity
          recalc every <span className="text-accent">1.4s</span> · watermark
          high <span className="text-accent">·85</span> / low{" "}
          <span className="text-cyan">·30</span>
        </div>
      </div>
    </div>
  );
}
