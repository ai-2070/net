interface ArpanetNode {
  x: number;
  y: number;
  label?: string;
}

const ARPANET_NODES: Record<string, ArpanetNode> = {
  UCLA: { x: 165, y: 380, label: "UCLA" },
  SRI: { x: 110, y: 270, label: "SRI" },
  UCSB: { x: 140, y: 410, label: "UCSB" },
  RAND: { x: 220, y: 360, label: "RAND" },
  UTAH: { x: 320, y: 245, label: "UTAH" },
  ILL: { x: 640, y: 230, label: "UIUC" },
  CASE: { x: 790, y: 195, label: "CASE" },
  CMU: { x: 820, y: 245, label: "CMU" },
  MITRE: { x: 870, y: 275, label: "MITRE" },
  BBN: { x: 880, y: 160, label: "BBN" },
  MIT: { x: 920, y: 130, label: "MIT" },
  HVD: { x: 945, y: 105, label: "HVD" },
  LINC: { x: 920, y: 200, label: "LINC" },
  BURR: { x: 850, y: 105, label: "BURR" },
};

const ARPANET_EDGES: ReadonlyArray<readonly [string, string]> = [
  ["UCLA", "SRI"],
  ["UCLA", "UCSB"],
  ["UCLA", "RAND"],
  ["UCSB", "SRI"],
  ["SRI", "UTAH"],
  ["UTAH", "ILL"],
  ["UTAH", "CASE"],
  ["CASE", "CMU"],
  ["CASE", "MIT"],
  ["CMU", "HVD"],
  ["MIT", "BBN"],
  ["BBN", "HVD"],
  ["HVD", "LINC"],
  ["LINC", "BURR"],
  ["ILL", "MITRE"],
  ["MITRE", "BBN"],
  ["RAND", "BBN"],
];

export function ArpanetMapBg() {
  return (
    <div
      className="crt-scanlines-dense absolute inset-0 pointer-events-none"
      aria-hidden
      style={{
        WebkitMaskImage:
          "radial-gradient(ellipse 90% 80% at 50% 50%, #000 35%, transparent 95%)",
        maskImage:
          "radial-gradient(ellipse 90% 80% at 50% 50%, #000 35%, transparent 95%)",
      }}
    >
      <span className="absolute top-4 left-4 w-3 h-3 border-t border-l border-accent/55" />
      <span className="absolute top-4 right-4 w-3 h-3 border-t border-r border-accent/55" />
      <span className="absolute bottom-4 left-4 w-3 h-3 border-b border-l border-accent/55" />
      <span className="absolute bottom-4 right-4 w-3 h-3 border-b border-r border-accent/55" />
      <svg
        className="w-full h-full opacity-[0.4]"
        viewBox="0 0 1000 589"
        preserveAspectRatio="xMidYMid meet"
      >
        <defs>
          <pattern
            id="arpanet-grid"
            width="40"
            height="40"
            patternUnits="userSpaceOnUse"
          >
            <path
              d="M 40 0 L 0 0 0 40"
              fill="none"
              stroke="#c4ff3d"
              strokeWidth="0.3"
              strokeOpacity="0.18"
            />
          </pattern>
        </defs>
        <rect
          x="40"
          y="60"
          width="920"
          height="490"
          fill="url(#arpanet-grid)"
        />

        {/* latitude tick marks (left edge) */}
        <g
          fontFamily="JetBrains Mono"
          fontSize="7"
          fill="#c4ff3d"
          fillOpacity="0.5"
        >
          {[
            { y: 120, lat: "50°N" },
            { y: 220, lat: "45°N" },
            { y: 320, lat: "40°N" },
            { y: 420, lat: "35°N" },
            { y: 510, lat: "30°N" },
          ].map((t) => (
            <g key={t.lat}>
              <line
                x1="40"
                y1={t.y}
                x2="50"
                y2={t.y}
                stroke="#c4ff3d"
                strokeOpacity="0.5"
                strokeWidth="0.6"
              />
              <text x="14" y={t.y + 2.5} letterSpacing="0.5">
                {t.lat}
              </text>
            </g>
          ))}
        </g>

        {/* longitude tick marks (bottom edge) */}
        <g
          fontFamily="JetBrains Mono"
          fontSize="7"
          fill="#c4ff3d"
          fillOpacity="0.5"
        >
          {[
            { x: 130, lon: "120°W" },
            { x: 320, lon: "110°W" },
            { x: 510, lon: "100°W" },
            { x: 700, lon: "90°W" },
            { x: 890, lon: "80°W" },
          ].map((t) => (
            <g key={t.lon}>
              <line
                x1={t.x}
                y1="550"
                x2={t.x}
                y2="540"
                stroke="#c4ff3d"
                strokeOpacity="0.5"
                strokeWidth="0.6"
              />
              <text x={t.x} y="565" letterSpacing="0.5" textAnchor="middle">
                {t.lon}
              </text>
            </g>
          ))}
        </g>

        {/* compass — top right */}
        <g transform="translate(905,100)">
          <circle
            cx="0"
            cy="0"
            r="16"
            fill="none"
            stroke="#c4ff3d"
            strokeOpacity="0.45"
            strokeWidth="0.6"
          />
          <line
            x1="0"
            y1="-16"
            x2="0"
            y2="-22"
            stroke="#c4ff3d"
            strokeOpacity="0.7"
            strokeWidth="0.8"
          />
          <text
            x="0"
            y="-26"
            fontFamily="JetBrains Mono"
            fontSize="8"
            fill="#c4ff3d"
            fillOpacity="0.75"
            textAnchor="middle"
            fontWeight="600"
          >
            N
          </text>
          <line
            x1="0"
            y1="-12"
            x2="0"
            y2="12"
            stroke="#c4ff3d"
            strokeOpacity="0.55"
            strokeWidth="0.5"
          />
          <line
            x1="-12"
            y1="0"
            x2="12"
            y2="0"
            stroke="#c4ff3d"
            strokeOpacity="0.55"
            strokeWidth="0.5"
          />
        </g>

        {/* scale bar — bottom right */}
        <g transform="translate(820,500)">
          <line
            x1="0"
            y1="0"
            x2="120"
            y2="0"
            stroke="#c4ff3d"
            strokeOpacity="0.55"
            strokeWidth="0.7"
          />
          <line
            x1="0"
            y1="-3"
            x2="0"
            y2="3"
            stroke="#c4ff3d"
            strokeOpacity="0.55"
            strokeWidth="0.7"
          />
          <line
            x1="60"
            y1="-2"
            x2="60"
            y2="2"
            stroke="#c4ff3d"
            strokeOpacity="0.55"
            strokeWidth="0.6"
          />
          <line
            x1="120"
            y1="-3"
            x2="120"
            y2="3"
            stroke="#c4ff3d"
            strokeOpacity="0.55"
            strokeWidth="0.7"
          />
          <text
            x="0"
            y="14"
            fontFamily="JetBrains Mono"
            fontSize="7"
            fill="#c4ff3d"
            fillOpacity="0.55"
          >
            0
          </text>
          <text
            x="60"
            y="14"
            fontFamily="JetBrains Mono"
            fontSize="7"
            fill="#c4ff3d"
            fillOpacity="0.55"
            textAnchor="middle"
          >
            500 MI
          </text>
          <text
            x="120"
            y="14"
            fontFamily="JetBrains Mono"
            fontSize="7"
            fill="#c4ff3d"
            fillOpacity="0.55"
            textAnchor="end"
          >
            1000
          </text>
        </g>

        {/* topology stats — top left */}
        <g
          fontFamily="JetBrains Mono"
          fontSize="8"
          fill="#c4ff3d"
          fillOpacity="0.6"
          letterSpacing="1.2"
        >
          <text x="60" y="86">
            RFC-1 · IMP TOPOLOGY
          </text>
          <text x="60" y="100" fillOpacity="0.4">
            NODES: 14 · LINKS: 17
          </text>
          <text x="60" y="114" fillOpacity="0.4">
            PROTO: NCP · 50 KBPS LINES
          </text>
        </g>

        {/*{ARPANET_EDGES.map(([a, b]) => {
          const na = ARPANET_NODES[a];
          const nb = ARPANET_NODES[b];
          if (!na || !nb) return null;
          return (
            <line
              key={`${a}-${b}`}
              x1={na.x}
              y1={na.y}
              x2={nb.x}
              y2={nb.y}
              stroke="#c4ff3d"
              strokeWidth="0.7"
              strokeOpacity="0.5"
            />
          );
        })}
        {ARPANET_EDGES.map(([a, b], i) => {
          const na = ARPANET_NODES[a];
          const nb = ARPANET_NODES[b];
          if (!na || !nb) return null;
          const dist = Math.hypot(nb.x - na.x, nb.y - na.y);
          const dur = Math.max(1.4, dist / 90).toFixed(2) + "s";
          const begin = (i * 0.31).toFixed(2) + "s";
          const reverse = i % 2 === 0;
          const path = reverse
            ? `M ${nb.x} ${nb.y} L ${na.x} ${na.y}`
            : `M ${na.x} ${na.y} L ${nb.x} ${nb.y}`;
          return (
            <circle
              key={`pkt-${a}-${b}`}
              r="1.6"
              fill="#c4ff3d"
              opacity="0.9"
              style={{ filter: "drop-shadow(0 0 3px #c4ff3d)" }}
            >
              <animateMotion
                dur={dur}
                begin={begin}
                repeatCount="indefinite"
                path={path}
                rotate="auto"
              />
            </circle>
          );
        })}
        {Object.entries(ARPANET_NODES).map(([id, n]) => (
          <g key={id}>
            <circle
              cx={n.x}
              cy={n.y}
              r="6"
              fill="none"
              stroke="#c4ff3d"
              strokeOpacity="0.35"
            />
            <circle cx={n.x} cy={n.y} r="2.5" fill="#c4ff3d" />
            {n.label ? (
              <text
                x={n.x + 9}
                y={n.y + 3}
                fontFamily="JetBrains Mono"
                fontSize="9"
                fill="#c4ff3d"
                fillOpacity="0.7"
                letterSpacing="0.5"
              >
                {n.label}
              </text>
            ) : null}
          </g>
        ))}*/}
        <text
          x="80"
          y="540"
          fontFamily="JetBrains Mono"
          fontSize="10"
          fill="#c4ff3d"
          fillOpacity="0.55"
          letterSpacing="2"
        >
          ARPANET · IMP BACKBONE · DEC 1971
        </text>
      </svg>
    </div>
  );
}
