// The Blackwall as a held structure: a row of concrete pillars, each one
// suspended on its own, with the field's light leaking through every gap.
// No single pillar is the wall; the gaps are where the constraint shows.

const PILLARS: ReadonlyArray<{ w: number; dy: number; t: number }> = [
  { w: 46, dy: 10, t: 11 },
  { w: 22, dy: -6, t: 13 },
  { w: 64, dy: 4, t: 10 },
  { w: 30, dy: -12, t: 14 },
  { w: 18, dy: 8, t: 12 },
  { w: 52, dy: -2, t: 15 },
  { w: 26, dy: 14, t: 11 },
  { w: 40, dy: -8, t: 13 },
  { w: 70, dy: 2, t: 16 },
  { w: 20, dy: -14, t: 12 },
  { w: 34, dy: 6, t: 10 },
  { w: 58, dy: -4, t: 14 },
  { w: 24, dy: 12, t: 13 },
  { w: 44, dy: -10, t: 11 },
  { w: 16, dy: 4, t: 15 },
  { w: 62, dy: -2, t: 12 },
  { w: 28, dy: 10, t: 14 },
  { w: 38, dy: -12, t: 10 },
  { w: 54, dy: 6, t: 13 },
  { w: 22, dy: -6, t: 16 },
];

export function BlackwallViz() {
  return (
    <div
      className="relative w-full h-[260px] md:h-[360px] overflow-hidden mb-16 bg-[#070807]"
      aria-hidden
    >
      {/* the field behind the wall */}
      <div
        className="absolute inset-0 blackwall-glow"
        style={{
          background:
            "radial-gradient(ellipse 55% 75% at 58% 50%, rgba(196,255,61,0.55), rgba(196,255,61,0.12) 45%, rgba(196,255,61,0) 75%)",
        }}
      />
      {/* the wall */}
      <div className="absolute inset-y-[-24px] inset-x-0 flex gap-[5px] md:gap-[7px] justify-center">
        {PILLARS.map((p, i) => (
          <div
            key={i}
            className="field-wave h-full shrink-0"
            style={
              {
                width: `${(p.w / 820) * 100}%`,
                "--i": i * 0.4,
              } as React.CSSProperties
            }
          >
            <div
              className="h-full w-full"
              style={{
                backgroundColor: "#1c1d1b",
                backgroundImage:
                  "linear-gradient(90deg, rgba(196,255,61,0.16) 0%, rgba(0,0,0,0) 14%, rgba(0,0,0,0) 86%, rgba(196,255,61,0.12) 100%), linear-gradient(180deg, rgba(255,255,255,0.06), rgba(0,0,0,0.5)), var(--tex-pits), var(--tex-grain), var(--tex-stain)",
                backgroundSize:
                  "100% 100%, 100% 100%, 300px 300px, 260px 260px, 900px 900px",
                backgroundBlendMode:
                  "normal, normal, normal, overlay, soft-light",
              }}
            />
          </div>
        ))}
      </div>
      {/* light spilling onto the floor in front of the wall */}
      <div
        className="absolute inset-x-0 bottom-0 h-1/3"
        style={{
          background:
            "linear-gradient(0deg, rgba(11,12,11,0.95), rgba(11,12,11,0))",
        }}
      />
      <div className="absolute inset-0 grid place-items-center">
        <div
          className="font-display font-bold uppercase text-ink text-[clamp(26px,5.4vw,64px)] tracking-[0.42em] pl-[0.42em]"
          style={{
            textShadow: "0 2px 0 rgba(0,0,0,0.8), 0 0 40px rgba(11,12,11,0.9)",
          }}
        >
          BLACKWALL
        </div>
      </div>
    </div>
  );
}
