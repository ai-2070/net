import { Monolith } from "./Monolith";

/**
 * Mikoshi, shown rather than told: a daemon (the lit cube) is held over its
 * source node, lifts, exists over both nodes for a moment, then collapses
 * onto the target as routing cuts over. Loops source → target → back.
 * Node ids are illustrative, like the rest of the page's telemetry.
 */
export function MikoshiTransit() {
  return (
    <div
      aria-hidden
      className="relative mt-20 h-[300px] md:h-[340px] overflow-hidden select-none"
    >
      {/* the stage floor */}
      <div
        className="absolute inset-x-0 bottom-0 h-[55%]"
        style={{
          background:
            "radial-gradient(ellipse 60% 70% at 50% 100%, rgba(238,241,232,0.06), rgba(11,12,11,0) 70%)",
        }}
      />
      <div className="absolute inset-0 flex items-end justify-between px-[6%] md:px-[14%] pb-10">
        <Plinth label="node.0x7af3" />
        <Plinth label="node.0x2c91" flip />
      </div>

      {/* the daemon, in its two places */}
      <div className="mikoshi-a absolute bottom-[150px] md:bottom-[166px] left-[6%] md:left-[14%] w-[150px] md:w-[190px] flex justify-center">
        <DaemonCube />
      </div>
      <div className="mikoshi-b absolute bottom-[150px] md:bottom-[166px] right-[6%] md:right-[14%] w-[150px] md:w-[190px] flex justify-center">
        <DaemonCube />
      </div>

      {/* the route between them: lit while the cutover happens */}
      <div className="absolute left-[calc(6%+150px)] right-[calc(6%+150px)] md:left-[calc(14%+190px)] md:right-[calc(14%+190px)] bottom-[124px] md:bottom-[136px] h-px">
        <div className="mikoshi-route h-px w-full bg-accent" />
      </div>
    </div>
  );
}

function Plinth({ label, flip = false }: { label: string; flip?: boolean }) {
  return (
    <div className="flex flex-col items-center gap-12 w-[150px] md:w-[190px] scale-[0.78] md:scale-100 origin-bottom">
      <Monolith
        w={130}
        h={56}
        d={110}
        rx={-22}
        ry={flip ? -24 : 24}
        sway={3}
        period={16}
        delay={flip ? -6 : 0}
        shadow={false}
      />
      <span className="stencil text-ink-dim">{label}</span>
    </div>
  );
}

function DaemonCube() {
  return (
    <div className="mikoshi-cube" style={{ perspective: 600 }}>
      <div className="mikoshi-cube-inner">
        {(
          [
            "translateZ(18px)",
            "rotateY(180deg) translateZ(18px)",
            "rotateY(90deg) translateZ(18px)",
            "rotateY(-90deg) translateZ(18px)",
            "rotateX(90deg) translateZ(18px)",
            "rotateX(-90deg) translateZ(18px)",
          ] as const
        ).map((t) => (
          <span key={t} className="mikoshi-face" style={{ transform: t }} />
        ))}
      </div>
    </div>
  );
}
