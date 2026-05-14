interface SpectrumTick {
  exp: number;
  x: number;
  unit: string;
}

const SPECTRUM_TICKS: ReadonlyArray<SpectrumTick> = [
  { exp: -9, x: 80, unit: "1 ns" },
  { exp: -8, x: 253, unit: "10 ns" },
  { exp: -7, x: 427, unit: "100 ns" },
  { exp: -6, x: 600, unit: "1 μs" },
  { exp: -5, x: 773, unit: "10 μs" },
  { exp: -4, x: 947, unit: "100 μs" },
  { exp: -3, x: 1120, unit: "1 ms" },
];

interface SpectrumMarker {
  x: number;
  label: string;
  sub: string;
  color: string;
  faint: string;
  glow?: boolean;
  slower?: string;
}

const SPECTRUM_MARKERS: ReadonlyArray<SpectrumMarker> = [
  {
    x: 80,
    label: "NET*",
    sub: "mesh transport",
    color: "#c4ff3d",
    faint: "#6b8a1e",
    glow: true,
  },
  {
    x: 687,
    label: "REAL-TIME†",
    sub: "CAN · EtherCAT · TSN",
    color: "#d4dcd0",
    faint: "#6b7568",
    slower: "~1,000× slower",
  },
  {
    x: 1120,
    label: "BEST-EFFORT",
    sub: "cloud · TCP · gRPC",
    color: "#6b7568",
    faint: "#4a5249",
    slower: "~1,000,000× slower",
  },
];

export function LatencySpectrum() {
  return (
    <div className="mt-10 border border-line bg-bg-2 px-6 py-7 hidden md:block">
      <div className="text-[10px] tracking-[0.18em] uppercase text-ink-dim mb-4 flex items-center justify-between flex-wrap gap-2">
        <span>// latency spectrum · log scale</span>
        <span className="text-ink-faint">10⁻⁹ → 10⁻³</span>
      </div>

      <div className="overflow-x-auto">
        <svg
          viewBox="0 0 1200 108"
          className="w-full"
          style={{ minWidth: 720 }}
          preserveAspectRatio="xMidYMid meet"
        >
          {SPECTRUM_MARKERS.map((m) => (
            <g key={m.label}>
              <text
                x={m.x}
                y="14"
                fontFamily="JetBrains Mono"
                fontSize="11"
                fill={m.color}
                textAnchor="middle"
                letterSpacing="1.4"
                fontWeight="600"
              >
                {m.label}
              </text>
              {m.slower ? (
                <>
                  <rect
                    x={m.x - (m.slower.length * 5.5) / 2 - 6}
                    y={18}
                    width={m.slower.length * 5.5 + 12}
                    height={13}
                    fill="#0a0c0a"
                    rx="2"
                  />
                  <text
                    x={m.x}
                    y="28"
                    fontFamily="JetBrains Mono"
                    fontSize="9"
                    fill={m.color}
                    textAnchor="middle"
                    letterSpacing="0.4"
                    fontStyle="italic"
                  >
                    {m.slower}
                  </text>
                </>
              ) : null}
              <text
                x={m.x}
                y="38"
                fontFamily="JetBrains Mono"
                fontSize="8"
                fill={m.faint}
                textAnchor="middle"
                letterSpacing="0.4"
              >
                {m.sub}
              </text>
              <line
                x1={m.x}
                y1="44"
                x2={m.x}
                y2="62"
                stroke={m.color}
                strokeWidth="0.7"
                strokeOpacity="0.55"
                strokeDasharray="2 2"
              />
              <polygon
                points={`${m.x - 4},58 ${m.x + 4},58 ${m.x},64`}
                fill={m.color}
                opacity="0.85"
              />
            </g>
          ))}

          <line
            x1="80"
            y1="70"
            x2="1120"
            y2="70"
            stroke="#2d352c"
            strokeWidth="1"
          />

          {SPECTRUM_TICKS.map((t) => (
            <g key={t.exp}>
              <line
                x1={t.x}
                y1="66"
                x2={t.x}
                y2="74"
                stroke="#6b7568"
                strokeWidth="0.7"
              />
              <text
                x={t.x}
                y="86"
                fontFamily="JetBrains Mono"
                fontSize="10"
                fill="#6b7568"
                textAnchor="middle"
              >
                10
                <tspan fontSize="7" baselineShift="super">
                  {t.exp}
                </tspan>
              </text>
              <text
                x={t.x}
                y="100"
                fontFamily="JetBrains Mono"
                fontSize="8"
                fill="#4a5249"
                textAnchor="middle"
                letterSpacing="0.4"
              >
                {t.unit}
              </text>
            </g>
          ))}

          {SPECTRUM_MARKERS.map((m) => (
            <g key={m.label + "-dot"}>
              {m.glow ? (
                <circle
                  cx={m.x}
                  cy="70"
                  r="9"
                  fill="none"
                  stroke={m.color}
                  strokeOpacity="0.4"
                />
              ) : null}
              <circle
                cx={m.x}
                cy="70"
                r={m.glow ? 5 : 4}
                fill={m.color}
                style={
                  m.glow
                    ? { filter: `drop-shadow(0 0 6px ${m.color})` }
                    : undefined
                }
              />
            </g>
          ))}
        </svg>
      </div>

      <p className="text-[10px] text-ink-faint mt-4 leading-[1.6] tracking-[0.04em] font-mono">
        * forward 0.20 ns · cap check 1.78 ns · pingwave 0.65 ns · header
        serialize 1.31 ns (i9-14900K)
        <br />† real-time guarantees only on dedicated hardware. NET hits the
        nanosecond range on commodity wire.
      </p>
    </div>
  );
}
