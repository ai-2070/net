"use client";

import Link from "next/link";
import { useEffect, useRef, useState } from "react";
import { SectionLabel } from "../SectionLabel";
import { HudCorners } from "./GamesHero";

// Where Rose & Blade is served. Set NEXT_PUBLIC_DEMO_URL at build time; unset,
// the section shows the game with its launch buttons marked "coming soon".
const DEMO_URL = process.env.NEXT_PUBLIC_DEMO_URL?.trim() || null;

const MODES: ReadonlyArray<{ tag: string; title: string; body: string }> = [
  {
    tag: "solo",
    title: "Hold the lists",
    body: "Waves of Lancastrians, each quicker and tougher than the last.",
  },
  {
    tag: "pve · 2–4 players",
    title: "Fight together",
    body: "Hold the lists with friends. Waves grow with every player.",
  },
  {
    tag: "pvp · up to 4 a side",
    title: "York vs Lancaster",
    body: "One life a round. First side to three rounds takes the match.",
  },
];

const NET_FACTS: ReadonlyArray<{ value: string; label: string }> = [
  { value: "60/s", label: "updates between players" },
  { value: "p2p", label: "every page linked to every other" },
  { value: "0", label: "servers you run (public anchor)" },
  { value: "host", label: "can leave, the fight goes on" },
];

export function GamesDemoSection() {
  const [playing, setPlaying] = useState(false);
  const frameWrap = useRef<HTMLDivElement | null>(null);

  // Esc belongs to the game (it pauses), so leaving the embed is a button, not
  // a key. Bring the section into view when the game starts.
  useEffect(() => {
    if (playing) {
      frameWrap.current?.scrollIntoView({ behavior: "smooth", block: "start" });
    }
  }, [playing]);

  const fullscreen = (): void => {
    const el = frameWrap.current;
    if (el && document.fullscreenEnabled) void el.requestFullscreen();
  };

  return (
    <section
      id="demo"
      className="relative overflow-hidden border-b border-line min-h-[calc(100svh-80px)] flex flex-col bg-[#050706] scroll-mt-20"
    >
      {playing && DEMO_URL ? (
        <div
          ref={frameWrap}
          // Exactly the viewport under the fixed nav, and scrolled to sit there.
          className="relative flex flex-col bg-black h-[calc(100svh-80px)] scroll-mt-20"
        >
          <div className="flex items-center justify-between gap-4 px-4 h-11 border-b border-line bg-bg text-[10px] tracking-[0.14em] uppercase">
            <span className="text-ink-dim truncate">
              <span className="text-accent">▸</span> rose &amp; blade // live
              over net
            </span>
            <div className="flex items-center gap-4 shrink-0">
              <button
                type="button"
                onClick={fullscreen}
                className="text-ink hover:text-accent transition-colors cursor-pointer"
              >
                ⛶ fullscreen
              </button>
              <a
                href={DEMO_URL}
                target="_blank"
                rel="noopener noreferrer"
                className="text-ink hover:text-accent transition-colors hidden sm:inline"
              >
                new tab ↗
              </a>
              <button
                type="button"
                onClick={() => setPlaying(false)}
                className="text-warn hover:text-ink transition-colors cursor-pointer"
              >
                ✕ exit
              </button>
            </div>
          </div>
          <iframe
            src={DEMO_URL}
            title="Rose & Blade, a multiplayer Three.js game running over NET"
            className="flex-1 min-h-0 w-full border-0 block"
            // The game needs fullscreen (and holds the keyboard there), pointer
            // lock for the mouse, and sound once the player clicks.
            allow="fullscreen; autoplay; gamepad; keyboard-map"
            allowFullScreen
          />
        </div>
      ) : (
        <DemoAttract onPlay={() => setPlaying(true)} />
      )}
    </section>
  );
}

function DemoAttract({ onPlay }: { onPlay: () => void }) {
  const live = DEMO_URL !== null;

  return (
    <>
      <div
        className="absolute inset-0 opacity-60"
        aria-hidden
        style={{
          backgroundImage:
            "linear-gradient(rgba(196,255,61,0.06) 1px, transparent 1px), linear-gradient(90deg, rgba(196,255,61,0.06) 1px, transparent 1px)",
          backgroundSize: "48px 48px",
          maskImage:
            "radial-gradient(ellipse at center, black 30%, transparent 80%)",
        }}
      />
      <div
        className="absolute inset-0 pointer-events-none"
        aria-hidden
        style={{
          background:
            "radial-gradient(ellipse at 50% 35%, rgba(140,20,40,0.22), transparent 60%)",
        }}
      />
      <div
        className="absolute inset-0 overflow-hidden pointer-events-none"
        aria-hidden
      >
        <div className="absolute inset-x-0 h-1/3 bg-[linear-gradient(180deg,transparent,rgba(196,255,61,0.05),transparent)] animate-demo-sweep" />
      </div>
      <HudCorners className="inset-6" />

      {/* top HUD */}
      <div className="relative flex justify-between items-start w-full max-w-[1440px] mx-auto px-10 pt-10 text-[10px] tracking-[0.16em] uppercase">
        <div>
          <SectionLabel>§02 / play it</SectionLabel>
          <div className="text-ink-dim">demo // pve + pvp</div>
        </div>
        <div className="text-right text-ink-dim">
          <div>
            players <span className="text-ink">1–8</span>
          </div>
          <div>
            built with <span className="text-ink">three.js + rapier</span>
          </div>
        </div>
      </div>

      {/* centre */}
      <div className="relative flex-1 flex flex-col items-center justify-center text-center px-6 py-8 w-full max-w-[1440px] mx-auto">
        <div className="text-[11px] tracking-[0.3em] uppercase text-accent mb-5">
          ▸ the demo game
        </div>
        <h2
          className="font-display text-ink leading-[0.95] mb-5"
          style={{ fontSize: "clamp(48px, 8vw, 116px)" }}
        >
          rose <span className="text-accent game-glow">&amp;</span> blade
        </h2>
        <p className="font-sans text-[18px] md:text-[20px] text-ink max-w-[640px] leading-[1.5] mb-3">
          A physics sword fight from the Wars of the Roses. Every knight is a
          ragdoll, and you swing your sword with the mouse.
        </p>
        <p className="font-sans text-[15px] text-ink-dim max-w-[600px] leading-[1.6] mb-8">
          Play alone, team up against the waves, or fight each other. The
          multiplayer is NET, running right in the page.
        </p>

        <div className="grid grid-cols-1 md:grid-cols-3 gap-3 w-full max-w-[900px] mb-8 text-left">
          {MODES.map((m) => (
            <div
              key={m.tag}
              className="game-card border border-line bg-bg/70 backdrop-blur-sm p-5"
            >
              <div className="text-[10px] tracking-[0.14em] uppercase text-accent mb-2">
                {m.tag}
              </div>
              <div className="font-sans text-[17px] font-semibold text-ink mb-1">
                {m.title}
              </div>
              <div className="font-sans text-[13px] text-ink-dim leading-[1.5]">
                {m.body}
              </div>
            </div>
          ))}
        </div>

        <div className="flex gap-3 flex-wrap justify-center">
          {live ? (
            <>
              <button
                type="button"
                onClick={onPlay}
                className="btn-primary game-btn inline-flex items-center gap-2.5 px-8 py-4 text-[13px] tracking-[0.16em] uppercase font-semibold border border-accent bg-accent text-bg transition-all cursor-pointer"
              >
                ▶ Play here
              </button>
              <a
                href={DEMO_URL ?? undefined}
                target="_blank"
                rel="noopener noreferrer"
                className="btn-ghost inline-flex items-center gap-2.5 px-8 py-4 text-[13px] tracking-[0.16em] uppercase font-semibold no-underline border border-ink-faint text-ink transition-all"
              >
                Open in new tab ↗
              </a>
            </>
          ) : (
            <>
              <button
                type="button"
                disabled
                className="game-btn inline-flex items-center gap-2.5 px-8 py-4 text-[13px] tracking-[0.16em] uppercase font-semibold border border-accent-dim text-accent-dim cursor-not-allowed"
              >
                ▶ Play here · coming soon
              </button>
              <Link
                href="/docs/sdk/browser/quickstart"
                className="btn-ghost inline-flex items-center gap-2.5 px-8 py-4 text-[13px] tracking-[0.16em] uppercase font-semibold no-underline border border-ink-faint text-ink transition-all"
              >
                Build your own <span className="text-sm">→</span>
              </Link>
            </>
          )}
        </div>
        <p className="mt-5 text-[11px] tracking-[0.1em] text-ink-dim">
          Needs a mouse and keyboard. For multiplayer, open it in two browsers
          or send it to a friend.
        </p>
      </div>

      {/* bottom HUD: what NET does in it */}
      <div className="relative border-t border-line bg-bg/70 backdrop-blur-md">
        <div className="grid grid-cols-2 md:grid-cols-4 max-w-[1440px] mx-auto">
          {NET_FACTS.map((f) => (
            <div
              key={f.label}
              className="px-6 py-4 border-r border-line last:border-r-0 [&:nth-child(2)]:max-md:border-r-0 max-md:[&:nth-child(-n+2)]:border-b"
            >
              <div className="font-sans text-accent text-[22px] font-semibold leading-none">
                {f.value}
              </div>
              <div className="text-[10px] tracking-[0.12em] uppercase text-ink-dim mt-1.5">
                {f.label}
              </div>
            </div>
          ))}
        </div>
        <div className="max-w-[1440px] mx-auto px-6 py-2 border-t border-line text-[10px] tracking-[0.14em] uppercase text-ink-dim hidden md:flex justify-between">
          <span>
            wasd move · hold a mouse button + drag to swing · esc pause
          </span>
          <span>
            also inside: <span className="text-ink">arena fps co-op</span>
          </span>
        </div>
      </div>
    </>
  );
}
