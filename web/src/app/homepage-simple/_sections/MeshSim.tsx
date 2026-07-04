"use client";

import { useEffect, useRef, useState } from "react";

// A plain-language version of the index page's MeshOS animation. Same living
// mesh — nodes drift in, link up, and an "agent" hops between machines as work
// moves — but every label is a real-world name (laptop, gpu box, camera) and
// the event feed is written for someone who has never heard the word "daemon".

type Kind = "device" | "gpu" | "files" | "sensor";

interface KindSpec {
  rgb: string;
  glyph: string;
  label: string;
  names: ReadonlyArray<string>;
  offers: string;
}

const KINDS: Record<Kind, KindSpec> = {
  device: {
    rgb: "196, 255, 61",
    glyph: "◈",
    label: "device",
    names: ["laptop", "desktop", "phone", "tablet", "browser"],
    offers: "browser · files · terminal",
  },
  gpu: {
    rgb: "107, 138, 30",
    glyph: "▣",
    label: "gpu",
    names: ["gpu box", "gpu server", "inference node"],
    offers: "run large models",
  },
  files: {
    rgb: "61, 240, 255",
    glyph: "◉",
    label: "files",
    names: ["files", "nas", "drive", "backup"],
    offers: "store & move files",
  },
  sensor: {
    rgb: "212, 220, 208",
    glyph: "◇",
    label: "sensor",
    names: ["camera", "sensor", "mic", "drone"],
    offers: "a live feed",
  },
};

const KIND_ORDER: ReadonlyArray<Kind> = ["device", "gpu", "files", "sensor"];

const SIM_MONO_FONT =
  '"JetBrains Mono", ui-monospace, SFMono-Regular, monospace';

interface SimNode {
  id: number;
  name: string;
  kind: Kind;
  x: number;
  y: number;
  tx: number;
  ty: number;
  vx: number;
  vy: number;
  spawnDelay: number;
  age: number;
  emitted: boolean;
  offered: boolean;
}

interface TrailDot {
  x: number;
  y: number;
  age: number;
  ch: string;
}

interface SimAgent {
  id: number;
  hostIdx: number;
  migrating: boolean;
  fromIdx: number;
  toIdx: number;
  t: number;
  trail: TrailDot[];
}

interface SimLayout {
  count: number;
  agents: number;
  edgeRadius: number;
}

interface FeedLine {
  id: number;
  ts: string;
  prefix: string;
  color: string;
  body: React.ReactNode;
}

const FEED_MAX = 6;

function layoutFor(width: number): SimLayout {
  if (width < 540) return { count: 11, agents: 2, edgeRadius: 110 };
  if (width < 880) return { count: 15, agents: 2, edgeRadius: 135 };
  return { count: 19, agents: 3, edgeRadius: 155 };
}

function pickKind(): Kind {
  // Realistic mesh mix: lots of everyday devices, plenty of sensors and file
  // stores, and only a few (expensive) gpu boxes.
  const r = Math.random();
  if (r < 0.38) return "device";
  if (r < 0.64) return "sensor";
  if (r < 0.86) return "files";
  return "gpu";
}

function easeInOut(t: number): number {
  return t < 0.5 ? 2 * t * t : 1 - Math.pow(-2 * t + 2, 2) / 2;
}

function nowTs(): string {
  const d = new Date();
  return `${String(d.getMinutes()).padStart(2, "0")}:${String(
    d.getSeconds(),
  ).padStart(2, "0")}.${String(d.getMilliseconds()).padStart(3, "0")}`;
}

function MeshCanvas() {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const [feed, setFeed] = useState<readonly FeedLine[]>([]);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;

    const dpr = Math.min(window.devicePixelRatio || 1, 2);
    const NODE_FONT_PX = 17;
    const LABEL_FONT_PX = 11;
    const EDGE_FONT_PX = 11;

    let W = 0;
    let H = 0;
    let layout = layoutFor(640);
    let nodes: SimNode[] = [];
    let agents: SimAgent[] = [];
    let lastT = performance.now();
    let driftTimer = 0;
    let migrateTimer = 2;
    let eventTimer = 0.6;
    let nextEventGap = 1.5;
    let rafId = 0;
    let feedCounter = 0;
    const nameCounters: Record<Kind, number> = {
      device: 0,
      gpu: 0,
      files: 0,
      sensor: 0,
    };

    const nextName = (kind: Kind): string => {
      const pool = KINDS[kind].names;
      const n = pool[nameCounters[kind] % pool.length] ?? kind;
      nameCounters[kind] += 1;
      return n;
    };

    const pushFeed = (
      prefix: string,
      color: string,
      body: React.ReactNode,
    ): void => {
      feedCounter += 1;
      setFeed((prev) =>
        [...prev, { id: feedCounter, ts: nowTs(), prefix, color, body }].slice(
          -FEED_MAX,
        ),
      );
    };

    const spawnFromEdge = (): { x: number; y: number } => {
      const edge = Math.floor(Math.random() * 4);
      if (edge === 0) return { x: -50, y: Math.random() * H };
      if (edge === 1) return { x: W + 50, y: Math.random() * H };
      if (edge === 2) return { x: Math.random() * W, y: -50 };
      return { x: Math.random() * W, y: H + 50 };
    };

    const newTarget = (): { x: number; y: number } => ({
      x: W * 0.1 + Math.random() * W * 0.8,
      y: H * 0.18 + Math.random() * H * 0.64,
    });

    const initNodes = (): void => {
      nodes = [];
      nameCounters.device = 0;
      nameCounters.gpu = 0;
      nameCounters.files = 0;
      nameCounters.sensor = 0;
      // decide the mix first, then guarantee the mesh always has at least one
      // gpu box and one file store so the relationships stay visible.
      const kinds: Kind[] = [];
      for (let i = 0; i < layout.count; i++) kinds.push(pickKind());
      const ensure = (k: Kind): void => {
        if (kinds.includes(k)) return;
        let idx = Math.floor(Math.random() * kinds.length);
        let safety = 8;
        while (
          (kinds[idx] === "gpu" || kinds[idx] === "files") &&
          safety-- > 0
        ) {
          idx = Math.floor(Math.random() * kinds.length);
        }
        kinds[idx] = k;
      };
      ensure("gpu");
      ensure("files");
      for (let i = 0; i < layout.count; i++) {
        const start = spawnFromEdge();
        const target = newTarget();
        const kind = kinds[i]!;
        nodes.push({
          id: i,
          name: nextName(kind),
          kind,
          x: start.x,
          y: start.y,
          tx: target.x,
          ty: target.y,
          vx: 0,
          vy: 0,
          spawnDelay: i * 130 + Math.random() * 80,
          age: 0,
          emitted: false,
          offered: false,
        });
      }
    };

    const initAgents = (): void => {
      agents = [];
      for (let i = 0; i < layout.agents; i++) {
        agents.push({
          id: i,
          hostIdx: Math.floor(Math.random() * Math.max(1, nodes.length)),
          migrating: false,
          fromIdx: 0,
          toIdx: 0,
          t: 0,
          trail: [],
        });
      }
    };

    // ---- plain-language event stream (mirrors real mesh behaviour) ----
    const liveNodes = (): SimNode[] =>
      nodes.filter((n) => n.age >= n.spawnDelay);
    const pickFrom = <T,>(arr: ReadonlyArray<T>): T | undefined =>
      arr.length ? arr[Math.floor(Math.random() * arr.length)] : undefined;
    const ofKind = (k: Kind): SimNode | undefined =>
      pickFrom(liveNodes().filter((n) => n.kind === k));
    const otherThan = (node: SimNode): SimNode | undefined =>
      pickFrom(liveNodes().filter((n) => n !== node));
    const cn = (node: SimNode): React.ReactNode => (
      <span style={{ color: `rgb(${KINDS[node.kind].rgb})` }}>{node.name}</span>
    );
    const agentTag = (): React.ReactNode => (
      <span className="text-cyan">agent</span>
    );
    const C = {
      lime: "rgb(196, 255, 61)",
      olive: "rgb(107, 138, 30)",
      cyan: "rgb(61, 240, 255)",
      pale: "rgb(212, 220, 208)",
      warn: "rgb(255, 94, 61)",
    };
    const ASK_VERBS = [
      "to open a browser",
      "to run a command",
      "to read a file",
      "to open an app",
    ];
    interface EventTpl {
      w: number;
      make: () => {
        prefix: string;
        color: string;
        body: React.ReactNode;
      } | null;
    }
    const EVENTS: ReadonlyArray<EventTpl> = [
      // a machine advertises what it can do
      {
        w: 3,
        make: () => {
          const n = pickFrom(liveNodes());
          if (!n) return null;
          return {
            prefix: "▸",
            color: C.olive,
            body: (
              <>
                {cn(n)} offers:{" "}
                <span className="text-ink-dim">{KINDS[n.kind].offers}</span>
              </>
            ),
          };
        },
      },
      // the agent asks a device for bounded access
      {
        w: 3,
        make: () => {
          const n = ofKind("device");
          if (!n) return null;
          const v = pickFrom(ASK_VERBS) ?? "for access";
          return {
            prefix: "▸",
            color: C.lime,
            body: (
              <>
                {agentTag()} asked {cn(n)} {v}
              </>
            ),
          };
        },
      },
      // scarce gpu gets reserved before use
      {
        w: 2,
        make: () => {
          const g = ofKind("gpu");
          if (!g) return null;
          return {
            prefix: "▸",
            color: C.lime,
            body: (
              <>
                {agentTag()} reserved {cn(g)}{" "}
                <span className="text-ink-faint">before running</span>
              </>
            ),
          };
        },
      },
      // a job runs on the gpu
      {
        w: 2,
        make: () => {
          const g = ofKind("gpu");
          if (!g) return null;
          return {
            prefix: "▸",
            color: C.lime,
            body: (
              <>
                {agentTag()} started a job on {cn(g)}
              </>
            ),
          };
        },
      },
      // a file moves between two machines
      {
        w: 3,
        make: () => {
          const f = ofKind("files");
          if (!f) return null;
          const t = otherThan(f);
          if (!t) return null;
          return {
            prefix: "↗",
            color: C.cyan,
            body: (
              <>
                {cn(f)} → {cn(t)}:{" "}
                <span className="text-ink-dim">moved a file</span>
              </>
            ),
          };
        },
      },
      // a result comes back
      {
        w: 2,
        make: () => {
          const f = ofKind("files");
          if (!f) return null;
          return {
            prefix: "↗",
            color: C.cyan,
            body: (
              <>
                {agentTag()} pulled a result from {cn(f)}
              </>
            ),
          };
        },
      },
      // a sensor streams live
      {
        w: 2,
        make: () => {
          const s = ofKind("sensor");
          if (!s) return null;
          return {
            prefix: "≈",
            color: C.pale,
            body: (
              <>
                {cn(s)} is streaming live to {agentTag()}
              </>
            ),
          };
        },
      },
      // compute moves to the data (data gravity)
      {
        w: 1,
        make: () => {
          const g = ofKind("gpu");
          const f = ofKind("files");
          if (!g || !f) return null;
          return {
            prefix: "▸",
            color: C.olive,
            body: (
              <>
                {cn(g)} pulled data from {cn(f)}{" "}
                <span className="text-ink-faint">· work moves to the data</span>
              </>
            ),
          };
        },
      },
      // a machine drops and work reroutes
      {
        w: 1,
        make: () => {
          const n = pickFrom(liveNodes());
          if (!n) return null;
          const o = otherThan(n);
          if (!o) return null;
          return {
            prefix: "⚠",
            color: C.warn,
            body: (
              <>
                {cn(n)} dropped — work moved to {cn(o)}
              </>
            ),
          };
        },
      },
      // a resource refuses something outside its lane (local control)
      {
        w: 1,
        make: () => {
          const n = pickFrom(liveNodes());
          if (!n) return null;
          return {
            prefix: "⚠",
            color: C.warn,
            body: (
              <>
                {cn(n)} declined —{" "}
                <span className="text-ink-faint">outside its lane</span>
              </>
            ),
          };
        },
      },
    ];
    const EVENTS_W = EVENTS.reduce((s, t) => s + t.w, 0);
    const emitEvent = (): void => {
      for (let tries = 0; tries < 6; tries++) {
        let r = Math.random() * EVENTS_W;
        let chosen = EVENTS[0]!;
        for (const t of EVENTS) {
          r -= t.w;
          if (r <= 0) {
            chosen = t;
            break;
          }
        }
        const res = chosen.make();
        if (res) {
          pushFeed(res.prefix, res.color, res.body);
          return;
        }
      }
      const cnt = liveNodes().length;
      pushFeed("▸", C.olive, <>mesh is healthy · {cnt} machines connected</>);
    };

    const resize = (): void => {
      const rect = canvas.getBoundingClientRect();
      W = rect.width;
      H = rect.height;
      canvas.width = Math.max(1, Math.floor(W * dpr));
      canvas.height = Math.max(1, Math.floor(H * dpr));
      ctx.setTransform(1, 0, 0, 1, 0, 0);
      ctx.scale(dpr, dpr);
      const next = layoutFor(W);
      const needInit = next.count !== layout.count || nodes.length === 0;
      layout = next;
      if (needInit) {
        initNodes();
        initAgents();
      } else {
        for (const n of nodes) {
          const t = newTarget();
          n.tx = t.x;
          n.ty = t.y;
        }
      }
    };

    const frame = (): void => {
      const now = performance.now();
      const dt = Math.min(0.05, (now - lastT) / 1000);
      lastT = now;

      // physics + "joined" events
      for (const n of nodes) {
        n.age += dt * 1000;
        if (n.age < n.spawnDelay) continue;
        if (!n.emitted) {
          n.emitted = true;
          pushFeed(
            "▸",
            `rgb(${KINDS[n.kind].rgb})`,
            <>
              <span style={{ color: `rgb(${KINDS[n.kind].rgb})` }}>
                {n.name}
              </span>{" "}
              joined the mesh
            </>,
          );
        }
        const dx = n.tx - n.x;
        const dy = n.ty - n.y;
        n.vx += dx * 3.1 * dt;
        n.vy += dy * 3.1 * dt;
        n.vx *= Math.max(0, 1 - 2.4 * dt);
        n.vy *= Math.max(0, 1 - 2.4 * dt);
        n.x += n.vx * dt;
        n.y += n.vy * dt;
      }

      // varied mesh activity — offers, access, jobs, files, streams, recovery
      eventTimer += dt;
      if (eventTimer > nextEventGap) {
        eventTimer = 0;
        nextEventGap = 1.2 + Math.random() * 0.9;
        emitEvent();
      }

      // gentle reflow so the formation keeps breathing
      driftTimer += dt;
      if (driftTimer > 5.5) {
        driftTimer = 0;
        const k = 2 + Math.floor(Math.random() * 2);
        for (let i = 0; i < k; i++) {
          const n = nodes[Math.floor(Math.random() * nodes.length)];
          if (n && n.age >= n.spawnDelay) {
            const t = newTarget();
            n.tx = t.x;
            n.ty = t.y;
          }
        }
      }

      // an agent occasionally moves work to another machine (slower, so the
      // feed is not dominated by movement)
      migrateTimer += dt;
      if (migrateTimer > 4.5) {
        migrateTimer = 0;
        const idle = agents.filter((a) => !a.migrating);
        const live = nodes.filter((n) => n.age >= n.spawnDelay);
        if (idle.length > 0 && live.length > 1) {
          const a = idle[Math.floor(Math.random() * idle.length)]!;
          const fromIdx = a.hostIdx;
          let toIdx = Math.floor(Math.random() * nodes.length);
          let safety = 8;
          while (
            (toIdx === fromIdx ||
              nodes[toIdx]!.age < nodes[toIdx]!.spawnDelay) &&
            safety-- > 0
          ) {
            toIdx = Math.floor(Math.random() * nodes.length);
          }
          a.fromIdx = fromIdx;
          a.toIdx = toIdx;
          a.migrating = true;
          a.t = 0;
          a.trail = [];
          const fromName = nodes[fromIdx]?.name ?? "a machine";
          const to = nodes[toIdx]!;
          const followsData = to.kind === "files" || to.kind === "gpu";
          pushFeed(
            "↗",
            "rgb(61, 240, 255)",
            <>
              agent moved: <span className="text-ink-dim">{fromName}</span> →{" "}
              <span style={{ color: `rgb(${KINDS[to.kind].rgb})` }}>
                {to.name}
              </span>
              {followsData ? (
                <span className="text-ink-faint"> · work follows the data</span>
              ) : null}
            </>,
          );
        }
      }

      for (const a of agents) {
        if (!a.migrating) continue;
        a.t += dt / 1.5;
        if (a.t >= 1) {
          a.migrating = false;
          a.hostIdx = a.toIdx;
          a.t = 0;
        }
      }

      // ---------- render ----------
      ctx.fillStyle = "rgba(10, 12, 10, 0.34)";
      ctx.fillRect(0, 0, W, H);
      ctx.textAlign = "center";
      ctx.textBaseline = "middle";

      // links between nearby nodes, with a moving packet
      const flow = (now / 1000) * 0.35;
      ctx.font = `${EDGE_FONT_PX}px ${SIM_MONO_FONT}`;
      for (let i = 0; i < nodes.length; i++) {
        const a = nodes[i]!;
        if (a.age < a.spawnDelay) continue;
        for (let j = i + 1; j < nodes.length; j++) {
          const b = nodes[j]!;
          if (b.age < b.spawnDelay) continue;
          const dx = b.x - a.x;
          const dy = b.y - a.y;
          const dist = Math.sqrt(dx * dx + dy * dy);
          if (dist > layout.edgeRadius) continue;
          const opacity = 1 - dist / layout.edgeRadius;
          const steps = Math.max(2, Math.floor(dist / 7));
          ctx.fillStyle = `rgba(196, 255, 61, ${opacity * 0.16})`;
          for (let s = 1; s < steps; s++) {
            const t = s / steps;
            ctx.fillText("·", a.x + dx * t, a.y + dy * t);
          }
          const packetT = (((flow + i * 0.13 + j * 0.07) % 1) + 1) % 1;
          ctx.fillStyle = `rgba(196, 255, 61, ${opacity * 0.85})`;
          ctx.fillText("▸", a.x + dx * packetT, a.y + dy * packetT);
        }
      }

      // nodes: glyph + plain name
      for (const n of nodes) {
        if (n.age < n.spawnDelay) continue;
        const rgb = KINDS[n.kind].rgb;
        const fade = Math.min(1, (n.age - n.spawnDelay) / 600);
        const glyph = KINDS[n.kind].glyph;
        ctx.fillStyle = `rgba(${rgb}, ${0.2 * fade})`;
        ctx.font = `${NODE_FONT_PX + 8}px ${SIM_MONO_FONT}`;
        ctx.fillText(glyph, n.x, n.y);
        ctx.fillStyle = `rgba(${rgb}, ${fade})`;
        ctx.font = `bold ${NODE_FONT_PX}px ${SIM_MONO_FONT}`;
        ctx.fillText(glyph, n.x, n.y);
        ctx.fillStyle = `rgba(${rgb}, ${0.85 * fade})`;
        ctx.font = `${LABEL_FONT_PX}px ${SIM_MONO_FONT}`;
        ctx.fillText(n.name, n.x, n.y + 17);
      }

      // agents: sitting on a host, or in transit with a trail
      for (const a of agents) {
        const from = nodes[a.fromIdx];
        const to = nodes[a.toIdx];
        const host = nodes[a.hostIdx];
        let x: number;
        let y: number;
        if (a.migrating && from && to) {
          const tt = easeInOut(a.t);
          const lift = Math.sin(tt * Math.PI) * 14;
          x = from.x + (to.x - from.x) * tt;
          y = from.y + (to.y - from.y) * tt - lift;
          if (Math.random() < 0.5) {
            a.trail.push({ x, y, age: 0, ch: Math.random() < 0.5 ? "▒" : "░" });
          }
        } else if (host) {
          x = host.x;
          y = host.y - 18;
        } else {
          continue;
        }

        ctx.font = `${EDGE_FONT_PX}px ${SIM_MONO_FONT}`;
        for (const t of a.trail) {
          t.age += dt;
          const al = Math.max(0, 0.7 - t.age * 2.4);
          if (al <= 0) continue;
          ctx.fillStyle = `rgba(61, 240, 255, ${al})`;
          ctx.fillText(t.ch, t.x, t.y);
        }
        a.trail = a.trail.filter((t) => t.age < 0.5);

        ctx.fillStyle = "rgba(61, 240, 255, 0.4)";
        ctx.font = `bold 21px ${SIM_MONO_FONT}`;
        ctx.fillText("◆", x, y);
        ctx.fillStyle = "rgba(255, 255, 255, 0.96)";
        ctx.font = `bold 14px ${SIM_MONO_FONT}`;
        ctx.fillText("◆", x, y);
        ctx.fillStyle = "rgba(61, 240, 255, 0.8)";
        ctx.font = `${LABEL_FONT_PX - 1}px ${SIM_MONO_FONT}`;
        ctx.fillText("agent", x, y - 13);
      }

      rafId = requestAnimationFrame(frame);
    };

    resize();
    ctx.fillStyle = "#0a0c0a";
    ctx.fillRect(0, 0, W, H);
    rafId = requestAnimationFrame(frame);
    const onResize = (): void => resize();
    window.addEventListener("resize", onResize);
    return () => {
      cancelAnimationFrame(rafId);
      window.removeEventListener("resize", onResize);
    };
  }, []);

  return (
    <div className="relative border border-line bg-bg overflow-hidden font-mono">
      <div className="flex items-center justify-between border-b border-line px-4 py-2 text-[10px] tracking-[0.14em] text-ink-dim uppercase">
        <span className="flex items-center gap-3">
          <span className="inline-flex gap-1">
            <span className="frame-dot-r w-[7px] h-[7px] rounded-full" />
            <span className="frame-dot-y w-[7px] h-[7px] rounded-full" />
            <span className="frame-dot-g w-[7px] h-[7px] rounded-full" />
          </span>
          <span className="text-accent">$</span>
          <span>
            net mesh{" "}
            <span className="text-ink-faint normal-case tracking-normal">
              --live --names=real
            </span>
          </span>
        </span>
        <span className="flex items-center gap-1.5 normal-case tracking-normal">
          <span className="w-1.5 h-1.5 rounded-full bg-accent inline-block animate-pulse-dot" />
          <span className="text-accent">live</span>
        </span>
      </div>

      <div className="relative bg-bg">
        <canvas
          ref={canvasRef}
          className="block w-full h-[340px] md:h-[420px] lg:h-[480px] bg-bg"
          aria-hidden
        />
        <div
          className="absolute inset-0 pointer-events-none"
          style={{
            backgroundImage:
              "repeating-linear-gradient(0deg, rgba(0,0,0,0) 0px, rgba(0,0,0,0) 2px, rgba(0,0,0,0.22) 3px, rgba(0,0,0,0.22) 3px)",
            mixBlendMode: "multiply",
          }}
          aria-hidden
        />
        <div
          className="absolute inset-0 pointer-events-none"
          style={{
            background:
              "radial-gradient(ellipse at center, transparent 55%, rgba(0,0,0,0.4) 100%)",
          }}
          aria-hidden
        />
      </div>

      <div className="border-t border-line bg-bg-2 px-4 py-3 text-[11px] leading-[1.7] min-h-[150px]">
        <div className="text-ink-faint text-[10px] tracking-[0.12em] uppercase mb-1.5 flex items-center justify-between">
          <span>
            <span className="text-accent">▸</span> what is happening
          </span>
          <span className="normal-case tracking-normal">live activity</span>
        </div>
        <div>
          {feed.length === 0 ? (
            <div className="text-ink-faint">
              <span className="text-accent animate-pulse-dot">█</span> waiting
              for machines to join...
            </div>
          ) : null}
          {feed.map((line) => (
            <div
              key={line.id}
              className="event-line-in flex items-baseline gap-3 whitespace-nowrap overflow-hidden"
            >
              <span className="text-ink-faint" style={{ minWidth: "9ch" }}>
                {line.ts}
              </span>
              <span style={{ color: line.color, minWidth: "2ch" }}>
                {line.prefix}
              </span>
              <span className="text-ink flex-1 truncate">{line.body}</span>
            </div>
          ))}
        </div>
      </div>

      <div className="flex flex-wrap gap-x-5 gap-y-2 border-t border-line px-4 py-3 text-[10px] tracking-[0.12em] text-ink-dim uppercase">
        {KIND_ORDER.map((k) => (
          <span key={k} className="flex items-center gap-2">
            <span
              className="font-mono"
              style={{ color: `rgb(${KINDS[k].rgb})` }}
            >
              {KINDS[k].glyph}
            </span>
            {KINDS[k].label}
          </span>
        ))}
        <span className="flex items-center gap-2 sm:ml-auto">
          <span className="font-mono" style={{ color: "rgb(61, 240, 255)" }}>
            ◆
          </span>
          agent · moving work
        </span>
      </div>
    </div>
  );
}

export function MeshSim() {
  return <MeshCanvas />;
}
