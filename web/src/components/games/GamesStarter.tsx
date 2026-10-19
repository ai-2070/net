"use client";

import Image from "next/image";
import { useRef, useState } from "react";
import { FieldLabel as SectionLabel } from "../field/FieldLabel";
import { FieldHeading as DisplayHeading } from "../field/FieldHeading";
import { Lede } from "../field/Lede";
import { CopyButton } from "../CopyButton";

// Net Shooter: a small three.js FPS, open source under MIT, so it is the
// example to download and take apart (Rose & Blade's source is not open).
const FPS_PLAY_URL = "https://net-fps.vercel.app";
const FPS_REPO_URL = "https://github.com/ai-2070/net-fps";

const RUN = `git clone ${FPS_REPO_URL}
cd net-fps
npm install
npm start`;

interface Shot {
  src: string;
  // the full screenshot, fetched only when it is opened
  full: string;
  alt: string;
  caption: string;
  api: string;
}

const SHOTS: ReadonlyArray<Shot> = [
  {
    src: "/games/net-shooter-host.webp",
    full: "/games/net-shooter-host-full.webp",
    alt: "Net Shooter hosting a co-op match with two other players, with NET's debug panel open: both players linked directly at 1 ms, 30 Hz netcode, and a live latency map",
    caption: "hosting co-op, net debug panel open",
    api: "hostNetcode",
  },
  {
    src: "/games/net-shooter-lobbies.webp",
    full: "/games/net-shooter-lobbies-full.webp",
    alt: "Net Shooter's server browser listing three open games with region, map and player count",
    caption: "server browser",
    api: "listLobbies",
  },
];

const PIECES: ReadonlyArray<{ title: string; api: string; body: string }> = [
  {
    title: "Server browser",
    api: "listLobbies",
    body: "Open games with server, region, map and player count. Search, filter, sort, and a lock on passworded games.",
  },
  {
    title: "Host a game",
    api: "createLobby",
    body: "Server name, max players, map, region, password and friendly fire. The password is checked on the host's page.",
  },
  {
    title: "Host and clients",
    api: "hostNetcode · joinNetcode",
    body: "The host runs the match at 30 ticks a second. Each player predicts their own movement with the same rules, so it feels instant.",
  },
  {
    title: "Fair shots",
    api: "rewind",
    body: "The host decides what a shot hit, using where the targets were when the shooter fired.",
  },
];

export function GamesStarterSection() {
  const dialogRef = useRef<HTMLDialogElement | null>(null);
  const [shown, setShown] = useState<Shot | null>(null);

  const open = (s: Shot): void => {
    setShown(s);
    dialogRef.current?.showModal();
  };
  const close = (): void => dialogRef.current?.close();

  return (
    <section id="fps" className="relative px-6 lg:px-12 py-24 lg:py-32">
      <SectionLabel>§03 / take it apart</SectionLabel>
      <DisplayHeading>
        a shooter
        <br />
        <span className="text-accent">to learn from.</span>
      </DisplayHeading>

      <Lede>
        Net Shooter is a small, open-source three.js FPS with NET built in: a
        server browser, hosting, and joining as a client. Play it, fork it, ship
        it (MIT), or read how each piece maps to NET. All of the networking is
        in one file,{" "}
        <span className="font-mono text-[15px] text-accent">js/net.js</span>.
      </Lede>

      <div className="grid grid-cols-1 md:grid-cols-2 gap-5 lg:gap-6 mb-6">
        {SHOTS.map((s) => (
          <figure key={s.src} className="slab m-0 overflow-hidden">
            <button
              type="button"
              onClick={() => open(s)}
              aria-label={`Enlarge: ${s.caption}`}
              className="group relative block w-full p-0 border-0 bg-transparent cursor-zoom-in"
            >
              <Image
                src={s.src}
                alt={s.alt}
                width={1600}
                height={842}
                sizes="(min-width: 1440px) 700px, (min-width: 768px) 50vw, 100vw"
                className="w-full h-auto aspect-[1600/842] object-cover transition-opacity group-hover:opacity-90"
              />
              <span className="absolute bottom-3 right-3 text-[12px] tracking-[0.14em] uppercase text-ink bg-bg/80 border border-line px-2 py-1 opacity-0 transition-opacity group-hover:opacity-100 group-focus-visible:opacity-100">
                ⤢ enlarge
              </span>
            </button>
            <figcaption className="flex items-center justify-between gap-3 border-t border-line px-4 py-2.5 text-[12px] tracking-[0.14em] uppercase text-ink-dim">
              <span>{s.caption}</span>
              <span className="text-accent font-mono normal-case tracking-normal">
                {s.api}
              </span>
            </figcaption>
          </figure>
        ))}
      </div>

      {/* the enlarged screenshot: Esc, the close button or a click closes it */}
      <dialog
        ref={dialogRef}
        onClose={() => setShown(null)}
        onClick={close}
        aria-label={shown?.caption ?? "Screenshot"}
        className="m-auto p-0 border border-line bg-bg max-w-[96vw] max-h-[94vh] overflow-hidden backdrop:bg-black/85 cursor-zoom-out"
      >
        {shown ? (
          <figure className="m-0">
            <Image
              src={shown.full}
              alt={shown.alt}
              width={2880}
              height={1514}
              unoptimized
              className="block w-auto h-auto max-w-[96vw] max-h-[calc(94vh-42px)]"
            />
            <figcaption className="flex items-center justify-between gap-3 border-t border-line px-4 py-2.5 text-[12px] tracking-[0.14em] uppercase text-ink-dim">
              <span>{shown.caption}</span>
              <button
                type="button"
                onClick={close}
                className="text-ink hover:text-accent cursor-pointer bg-transparent border-0 p-0 text-[12px] tracking-[0.14em] uppercase"
              >
                close ✕
              </button>
            </figcaption>
          </figure>
        ) : null}
      </dialog>

      <div className="grid grid-cols-1 lg:grid-cols-[1fr_420px] gap-5 lg:gap-6 items-start">
        <div className="grid grid-cols-1 sm:grid-cols-2 gap-5 lg:gap-6">
          {PIECES.map((p) => (
            <div key={p.title} className="slab slab-lift p-6">
              <span className="text-[12px] text-accent border border-accent-dim px-1.5 py-0.5 font-mono">
                {p.api}
              </span>
              <h3 className="font-display font-bold tracking-[-0.015em] text-[21px] leading-[1.15] text-ink mt-4 mb-1.5">
                {p.title}
              </h3>
              <p className="text-ink-dim text-[15px] leading-[1.6]">{p.body}</p>
            </div>
          ))}
        </div>

        <div data-ui className="slab">
          <div className="flex items-center justify-between border-b border-line px-4 py-2 bg-bg-2/60">
            <span className="font-mono text-[12px] tracking-[0.14em] uppercase text-accent-dim">
              <span className="text-accent">▸</span> run it yourself
            </span>
            <CopyButton text={RUN} />
          </div>
          <pre className="p-5 text-[14px] leading-[1.7] text-ink overflow-x-auto font-mono bg-black/30">
            {RUN}
          </pre>
          <div className="border-t border-line px-5 py-4 font-sans text-[15px] text-ink-dim leading-[1.6]">
            Co-op for 2–4 players, over NET&apos;s public anchor, so there is no
            server to run. The first{" "}
            <span className="font-mono text-[14px] text-ink">npm start</span>{" "}
            needs mkcert for local HTTPS; the README has the steps.
          </div>
          <div className="flex flex-col sm:flex-row gap-3 p-5 pt-0">
            <a
              href={FPS_PLAY_URL}
              target="_blank"
              rel="noopener noreferrer"
              className="btn-field flex-1 justify-center"
            >
              ▶ Play it ↗
            </a>
            <a
              href={FPS_REPO_URL}
              target="_blank"
              rel="noopener noreferrer"
              className="btn-ghost-field flex-1 justify-center"
            >
              Source ↗
            </a>
          </div>
        </div>
      </div>
    </section>
  );
}
