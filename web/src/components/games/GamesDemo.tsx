"use client";

import Image from "next/image";
import Link from "next/link";
import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import { SectionLabel } from "../SectionLabel";
import { HudCorners, usePrefersReducedMotion } from "./GamesHero";

// Where Rose & Blade is served. NEXT_PUBLIC_DEMO_URL overrides it at build
// time, e.g. to try a local copy of the game.
const DEMO_URL =
  process.env.NEXT_PUBLIC_DEMO_URL?.trim() ||
  "https://rose-and-blade.ai2070.net";

const MODES: ReadonlyArray<{ tag: string; title: string; body: string }> = [
  {
    tag: "solo",
    title: "The tournament",
    body: "Seven bouts, one life: the melee, named knights, then a champion. Or endless waves.",
  },
  {
    tag: "co-op · 2–4 players",
    title: "Fight together",
    body: "Hold the lists with friends. Waves grow with every player.",
  },
  {
    tag: "pvp · up to 8 a side",
    title: "York vs Lancaster",
    body: "16 players, one life a round. First side to three rounds takes the match.",
  },
];

const NET_FACTS: ReadonlyArray<{ value: string; label: string }> = [
  { value: "120/s", label: "updates between players, 60 above 8" },
  {
    value: "p2p",
    label: "every page linked to every other, direct when possible",
  },
  { value: "0", label: "servers you run (public anchor)" },
  {
    value: "host",
    label: "drops? the next player takes over, no one notices",
  },
];

export function GamesDemoSection() {
  return (
    <section
      id="demo"
      className="relative overflow-hidden border-b border-line min-h-[calc(100svh-80px)] flex flex-col bg-[#0b0907] scroll-mt-20"
    >
      <DemoAttract />
    </section>
  );
}

// A looping gameplay clip of Rose & Blade (16 s, no audio; it fades in from
// black and back out, so the loop has no seam). It plays muted behind the
// section, darkened so the text stays readable. Wide screens get the 1080p
// copy, smaller ones the lighter 720p one. Set to null to drop it: no <video>
// is rendered then, and the section sits on its grid and glow.
export const DEMO_REEL_SRC: string | null = "/games/rose-and-blade-reel.mp4";
const DEMO_REEL_HD_SRC = "/games/rose-and-blade-reel-1080.mp4";

// Rose & Blade's own look, for the parts of this section that are about the
// game: its type (Cinzel, EB Garamond; loaded on the games page), its cream
// and gold. What NET does in it stays in the site's style, in the bottom bar.
const RB_DISPLAY = "[font-family:var(--font-rb-display)]";
const RB_BODY = "[font-family:var(--font-rb-body)]";

function DemoBackdrop() {
  const reduceMotion = usePrefersReducedMotion();
  const boxRef = useRef<HTMLDivElement | null>(null);
  const videoRef = useRef<HTMLVideoElement | null>(null);
  // The reel is only fetched once the section comes near the screen, and it
  // plays only while the section is on screen.
  const [near, setNear] = useState(false);
  const [onScreen, setOnScreen] = useState(false);
  const reelOn = DEMO_REEL_SRC !== null && !reduceMotion && near;
  const [reelReady, setReelReady] = useState(false);

  useEffect(() => {
    const box = boxRef.current;
    if (!box || DEMO_REEL_SRC === null) return;
    const nearBy = new IntersectionObserver(
      ([e]) => {
        if (e?.isIntersecting) setNear(true);
      },
      { rootMargin: "600px 0px" },
    );
    const visible = new IntersectionObserver(([e]) => {
      setOnScreen(e?.isIntersecting ?? false);
    });
    nearBy.observe(box);
    visible.observe(box);
    return () => {
      nearBy.disconnect();
      visible.disconnect();
    };
  }, []);

  useEffect(() => {
    const v = videoRef.current;
    if (!v) return;
    if (onScreen) v.play().catch(() => {});
    else v.pause();
  }, [onScreen, reelOn]);

  return (
    <div
      ref={boxRef}
      className="absolute inset-0 pointer-events-none"
      aria-hidden
    >
      <div
        className="absolute inset-0 opacity-50"
        style={{
          backgroundImage:
            "linear-gradient(rgba(201,165,78,0.05) 1px, transparent 1px), linear-gradient(90deg, rgba(201,165,78,0.05) 1px, transparent 1px)",
          backgroundSize: "48px 48px",
          maskImage:
            "radial-gradient(ellipse at center, black 30%, transparent 80%)",
        }}
      />
      <div
        className="absolute inset-0"
        style={{
          background:
            "radial-gradient(ellipse at 50% 30%, rgba(140,20,40,0.24), transparent 60%)",
        }}
      />
      {reelOn ? (
        <video
          ref={videoRef}
          className={`absolute inset-0 w-full h-full object-cover transition-opacity duration-700 ${
            reelReady ? "opacity-100" : "opacity-0"
          }`}
          muted
          loop
          playsInline
          preload="auto"
          onLoadedData={() => setReelReady(true)}
          onError={() => setReelReady(false)}
        >
          <source
            src={DEMO_REEL_HD_SRC}
            type="video/mp4"
            media="(min-width: 1280px)"
          />
          <source src={DEMO_REEL_SRC ?? undefined} type="video/mp4" />
        </video>
      ) : null}
      {/* legibility over the reel: a dark wash, darkest behind the text */}
      <div
        className="absolute inset-0"
        style={{
          background:
            "radial-gradient(ellipse at 50% 45%, rgba(11,9,7,0.86) 0%, rgba(11,9,7,0.72) 55%, rgba(11,9,7,0.88) 100%)",
        }}
      />
    </div>
  );
}

// A touch screen with no mouse or trackpad: a phone or a bare tablet, where
// the game can't be played. The server render assumes a computer.
const TOUCH_ONLY = "(hover: none) and (pointer: coarse)";

function subscribeTouchOnly(onChange: () => void): () => void {
  const mq = window.matchMedia(TOUCH_ONLY);
  mq.addEventListener("change", onChange);
  return () => mq.removeEventListener("change", onChange);
}

function useTouchOnly(): boolean {
  return useSyncExternalStore(
    subscribeTouchOnly,
    () => window.matchMedia(TOUCH_ONLY).matches,
    () => false,
  );
}

const PRIMARY_BTN =
  "btn-primary game-btn inline-flex items-center gap-2.5 px-8 py-4 text-[13px] tracking-[0.16em] uppercase font-semibold no-underline border border-accent bg-accent text-bg transition-all";

// On a computer: open the game. On a phone, where it can't run: send the
// link on (the share sheet, or the clipboard) to open later on a computer.
function PlayButton() {
  const touchOnly = useTouchOnly();
  const [copied, setCopied] = useState(false);

  if (!touchOnly) {
    return (
      <a
        href={DEMO_URL}
        target="_blank"
        rel="noopener noreferrer"
        className={PRIMARY_BTN}
      >
        ▶ Play <span className="text-sm">↗</span>
      </a>
    );
  }

  const send = async (): Promise<void> => {
    const data = {
      title: "Rose & Blade",
      text: "Play Rose & Blade on a computer:",
      url: DEMO_URL,
    };
    try {
      if (typeof navigator.share === "function") {
        await navigator.share(data);
        return;
      }
      await navigator.clipboard.writeText(DEMO_URL);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 2000);
    } catch {
      // the share sheet was dismissed, or the clipboard is unavailable
    }
  };

  return (
    <button
      type="button"
      onClick={send}
      className={`${PRIMARY_BTN} cursor-pointer`}
    >
      {copied ? "✓ Link copied" : "Send link to your computer"}
    </button>
  );
}

function DemoAttract() {
  return (
    <>
      <DemoBackdrop />
      <HudCorners className="inset-6" />

      {/* top HUD */}
      <div className="relative flex justify-between items-start w-full max-w-[1440px] mx-auto px-10 pt-10 text-[10px] tracking-[0.16em] uppercase">
        <div>
          <SectionLabel>§02 / play it</SectionLabel>
          <div className="text-ink-dim">demo // co-op + pvp</div>
        </div>
        <div className="text-right text-ink-dim hidden sm:block">
          <div>
            players <span className="text-ink">1–16</span>
          </div>
          <div>
            built with <span className="text-ink">three.js + rapier</span>
          </div>
        </div>
      </div>

      {/* about the game, in the game's own type */}
      <div className="relative flex-1 flex flex-col items-center justify-center text-center px-6 pt-4 pb-12 w-full max-w-[1440px] mx-auto [text-shadow:0_1px_12px_rgba(0,0,0,0.85)]">
        <Image
          src="/games/rose-and-blade-emblem.webp"
          alt=""
          width={112}
          height={104}
          className="w-[88px] md:w-[112px] h-auto mb-4 drop-shadow-[0_6px_18px_rgba(0,0,0,0.6)]"
        />
        <div
          className={`${RB_DISPLAY} text-[12px] md:text-[14px] font-bold tracking-[0.32em] uppercase text-[#c9a54e] mb-1`}
        >
          England, 1461
        </div>
        <h2
          className={`${RB_DISPLAY} font-black uppercase leading-[0.9] mb-6`}
          style={{ fontSize: "clamp(48px, 7.5vw, 104px)" }}
        >
          <span className="block text-[#ece4d4]">Rose</span>
          <span className="block text-[#c9a54e]">&amp; Blade</span>
        </h2>
        <div
          className="w-[min(320px,60%)] h-px mb-7 bg-[linear-gradient(90deg,transparent,#c9a54e,transparent)]"
          aria-hidden
        />
        <p
          className={`${RB_BODY} text-[21px] md:text-[24px] text-[#ece4d4] max-w-[680px] leading-[1.45] mb-3`}
        >
          A physics sword fight from the Wars of the Roses. Every knight is a
          ragdoll, and you swing your sword with the mouse or a controller.
        </p>
        <p
          className={`${RB_BODY} text-[17px] text-[#b3a68f] max-w-[620px] leading-[1.55] mb-9`}
        >
          Play alone, team up against the waves, or fight each other eight a
          side. If the host drops, the next player takes over on the spot and
          the fight plays on.
        </p>

        <div className="grid grid-cols-1 md:grid-cols-3 gap-3 w-full max-w-[920px] mb-9 text-left">
          {MODES.map((m) => (
            <div
              key={m.tag}
              className="border border-[#3b3122] bg-[#14110c]/80 backdrop-blur-sm p-5 transition-colors hover:border-[#c9a54e]/60"
            >
              <div
                className={`${RB_DISPLAY} text-[11px] tracking-[0.18em] uppercase text-[#c9a54e] mb-2`}
              >
                {m.tag}
              </div>
              <div
                className={`${RB_DISPLAY} text-[19px] font-bold text-[#ece4d4] mb-1.5`}
              >
                {m.title}
              </div>
              <div
                className={`${RB_BODY} text-[15px] text-[#b3a68f] leading-[1.5]`}
              >
                {m.body}
              </div>
            </div>
          ))}
        </div>

        <div className="flex gap-3 flex-wrap justify-center">
          <PlayButton />
          <Link
            href="/docs/sdk/browser/quickstart"
            className="btn-ghost inline-flex items-center gap-2.5 px-8 py-4 text-[13px] tracking-[0.16em] uppercase font-semibold no-underline border border-ink-faint text-ink bg-bg/40 transition-all"
          >
            Build your own <span className="text-sm">→</span>
          </Link>
        </div>
        <p className="mt-5 text-[11px] tracking-[0.1em] text-ink-dim">
          Plays on a computer or laptop, with a mouse and keyboard or a
          controller (not on phones). For multiplayer, open it in two browsers
          or send it to a friend.
        </p>
      </div>

      {/* bottom HUD: what NET does in it */}
      <div className="relative border-t border-line bg-bg/80 backdrop-blur-md">
        <div className="max-w-[1440px] mx-auto px-6 pt-3 text-[10px] tracking-[0.16em] uppercase text-accent">
          ▸ the multiplayer // net
        </div>
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
            wasd move · hold a mouse button + drag to swing · scroll to thrust ·
            esc pause
          </span>
          <span>
            also:{" "}
            <span className="text-ink">live weather · 570-strong crowd</span>
          </span>
        </div>
      </div>
    </>
  );
}
