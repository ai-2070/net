export function SuperpositionViz() {
  return (
    <div className="mt-7 border border-line bg-bg-2">
      <div className="flex items-center justify-between border-b border-line px-5 py-2.5 text-[10px] tracking-[0.14em] text-ink-dim uppercase">
        <div className="flex items-center gap-3">
          <span className="text-accent">▸</span>
          <span>identity transfer · timeline</span>
        </div>
        <div className="flex items-center gap-4 font-mono normal-case tracking-normal">
          <span>
            <span className="text-ink-faint">window</span>{" "}
            <span className="text-accent">≈ 38ns</span>
          </span>
          <span>
            <span className="text-ink-faint">drop</span>{" "}
            <span className="text-accent">0</span>
          </span>
          <span className="flex items-center gap-1.5">
            <span className="w-1.5 h-1.5 rounded-full bg-accent inline-block animate-pulse-dot" />
            <span className="uppercase tracking-[0.12em]">live</span>
          </span>
        </div>
      </div>

      <div className="px-5 pt-6 pb-5">
        <svg
          className="w-full max-w-[600px] mx-auto aspect-[600/210] block"
          viewBox="0 0 600 210"
          preserveAspectRatio="xMidYMid meet"
        >
          <defs>
            <linearGradient id="superGrad" x1="0%" y1="0%" x2="100%" y2="0%">
              <stop offset="0%" stopColor="#c4ff3d" stopOpacity="1" />
              <stop offset="65%" stopColor="#c4ff3d" stopOpacity="0.55" />
              <stop offset="100%" stopColor="#c4ff3d" stopOpacity="0.1" />
            </linearGradient>
            <linearGradient id="superGrad2" x1="0%" y1="0%" x2="100%" y2="0%">
              <stop offset="0%" stopColor="#c4ff3d" stopOpacity="0.1" />
              <stop offset="35%" stopColor="#c4ff3d" stopOpacity="0.55" />
              <stop offset="100%" stopColor="#c4ff3d" stopOpacity="1" />
            </linearGradient>
            <linearGradient id="zoneFill" x1="0%" y1="0%" x2="0%" y2="100%">
              <stop offset="0%" stopColor="#c4ff3d" stopOpacity="0.03" />
              <stop offset="50%" stopColor="#c4ff3d" stopOpacity="0.09" />
              <stop offset="100%" stopColor="#c4ff3d" stopOpacity="0.03" />
            </linearGradient>
            <pattern
              id="superGrid"
              width="60"
              height="20"
              patternUnits="userSpaceOnUse"
            >
              <path
                d="M 60 0 L 0 0 0 20"
                fill="none"
                stroke="#1a1f1a"
                strokeWidth="0.4"
              />
            </pattern>
          </defs>

          <rect
            x="60"
            y="40"
            width="520"
            height="120"
            fill="url(#superGrid)"
            opacity="0.6"
          />

          <rect
            x="60"
            y="55"
            width="520"
            height="22"
            fill="#0e120e"
            opacity="0.6"
          />
          <rect
            x="60"
            y="123"
            width="520"
            height="22"
            fill="#0e120e"
            opacity="0.6"
          />

          <text
            x="54"
            y="69"
            textAnchor="end"
            fontFamily="JetBrains Mono"
            fontSize="9"
            fill="#c4ff3d"
            fontWeight="600"
          >
            node.A
          </text>
          <text
            x="54"
            y="79"
            textAnchor="end"
            fontFamily="JetBrains Mono"
            fontSize="7"
            fill="#4a5249"
          >
            0x7af3
          </text>
          <text
            x="54"
            y="138"
            textAnchor="end"
            fontFamily="JetBrains Mono"
            fontSize="9"
            fill="#c4ff3d"
            fontWeight="600"
          >
            node.B
          </text>
          <text
            x="54"
            y="148"
            textAnchor="end"
            fontFamily="JetBrains Mono"
            fontSize="7"
            fill="#4a5249"
          >
            0x2c91
          </text>

          <line
            x1="60"
            y1="40"
            x2="60"
            y2="160"
            stroke="#2d352c"
            strokeWidth="0.6"
          />

          <line
            x1="60"
            y1="66"
            x2="580"
            y2="66"
            stroke="#1a1f1a"
            strokeWidth="1"
          />
          <line
            x1="60"
            y1="66"
            x2="400"
            y2="66"
            stroke="url(#superGrad)"
            strokeWidth="2.5"
            strokeLinecap="round"
          />
          <circle cx="60" cy="66" r="4" fill="#c4ff3d" />
          <circle
            cx="60"
            cy="66"
            r="7"
            fill="none"
            stroke="#c4ff3d"
            strokeOpacity="0.3"
          />
          <g fontFamily="JetBrains Mono" fontSize="7" fill="#6b7568">
            <text x="80" y="62">
              ▸ exec
            </text>
            <text x="140" y="62">
              heap.alloc
            </text>
            <text x="220" y="62">
              cap.read
            </text>
            <text x="300" y="62">
              snap.encode
            </text>
          </g>

          <line
            x1="60"
            y1="134"
            x2="580"
            y2="134"
            stroke="#1a1f1a"
            strokeWidth="1"
          />
          <line
            x1="240"
            y1="134"
            x2="580"
            y2="134"
            stroke="url(#superGrad2)"
            strokeWidth="2.5"
            strokeLinecap="round"
          />
          <circle cx="580" cy="134" r="4" fill="#c4ff3d" />
          <circle
            cx="580"
            cy="134"
            r="7"
            fill="none"
            stroke="#c4ff3d"
            strokeOpacity="0.3"
          />
          <g fontFamily="JetBrains Mono" fontSize="7" fill="#6b7568">
            <text x="260" y="148">
              unpack
            </text>
            <text x="320" y="148">
              replay
            </text>
            <text x="390" y="148">
              ▸ exec
            </text>
            <text x="470" y="148">
              cap.write
            </text>
          </g>

          <rect
            className="superpose-zone"
            x="240"
            y="40"
            width="180"
            height="120"
            fill="url(#zoneFill)"
            stroke="#6b8a1e"
            strokeDasharray="3 3"
            strokeWidth="0.8"
          />
          <line
            x1="240"
            y1="36"
            x2="240"
            y2="40"
            stroke="#c4ff3d"
            strokeWidth="1"
          />
          <line
            x1="420"
            y1="36"
            x2="420"
            y2="40"
            stroke="#c4ff3d"
            strokeWidth="1"
          />
          <line
            x1="240"
            y1="160"
            x2="240"
            y2="164"
            stroke="#c4ff3d"
            strokeWidth="1"
          />
          <line
            x1="420"
            y1="160"
            x2="420"
            y2="164"
            stroke="#c4ff3d"
            strokeWidth="1"
          />

          <text
            x="330"
            y="100"
            fontFamily="Major Mono Display"
            fontSize="13"
            fill="#c4ff3d"
            textAnchor="middle"
            letterSpacing="2"
          >
            superposed
          </text>
          <text
            x="330"
            y="113"
            fontFamily="JetBrains Mono"
            fontSize="7"
            fill="#8a9482"
            textAnchor="middle"
            letterSpacing="1"
          >
            both nodes hold authority
          </text>

          <line
            x1="330"
            y1="40"
            x2="330"
            y2="160"
            stroke="#c4ff3d"
            strokeWidth="0.6"
            strokeDasharray="1 3"
            opacity="0.45"
          />

          <line
            x1="60"
            y1="180"
            x2="580"
            y2="180"
            stroke="#2d352c"
            strokeWidth="0.6"
          />
          <g fontFamily="JetBrains Mono" fontSize="7" fill="#4a5249">
            <line
              x1="60"
              y1="176"
              x2="60"
              y2="184"
              stroke="#4a5249"
              strokeWidth="0.6"
            />
            <text x="60" y="196" textAnchor="middle">
              0ns
            </text>
            <line
              x1="150"
              y1="178"
              x2="150"
              y2="182"
              stroke="#3a423a"
              strokeWidth="0.4"
            />
            <line
              x1="240"
              y1="174"
              x2="240"
              y2="186"
              stroke="#c4ff3d"
              strokeWidth="1"
            />
            <text
              x="240"
              y="196"
              textAnchor="middle"
              fill="#c4ff3d"
              fontWeight="600"
            >
              12ns
            </text>
            <line
              x1="330"
              y1="178"
              x2="330"
              y2="182"
              stroke="#3a423a"
              strokeWidth="0.4"
            />
            <line
              x1="420"
              y1="174"
              x2="420"
              y2="186"
              stroke="#c4ff3d"
              strokeWidth="1"
            />
            <text
              x="420"
              y="196"
              textAnchor="middle"
              fill="#c4ff3d"
              fontWeight="600"
            >
              50ns
            </text>
            <line
              x1="500"
              y1="178"
              x2="500"
              y2="182"
              stroke="#3a423a"
              strokeWidth="0.4"
            />
            <line
              x1="580"
              y1="176"
              x2="580"
              y2="184"
              stroke="#4a5249"
              strokeWidth="0.6"
            />
            <text x="580" y="196" textAnchor="middle">
              ~1µs
            </text>
          </g>

          <circle
            className="superpose-pkt-a"
            cx="60"
            cy="66"
            r="3.5"
            fill="#c4ff3d"
          />
          <circle
            className="superpose-pkt-b"
            cx="60"
            cy="134"
            r="3.5"
            fill="#c4ff3d"
          />
        </svg>

        <div className="grid grid-cols-3 gap-px bg-line border border-line mt-4 text-[10px]">
          <div className="bg-bg-2 px-3.5 py-3">
            <div className="flex items-baseline justify-between gap-2 mb-1">
              <span className="text-ink-dim tracking-[0.14em] uppercase">
                T₀
              </span>
              <span className="font-mono text-ink-faint">0–12ns</span>
            </div>
            <div className="text-ink mb-1">source running</div>
            <div className="text-ink-faint leading-[1.45]">
              A is sole authority. B is dark.
            </div>
          </div>
          <div className="bg-bg-2 px-3.5 py-3 relative">
            <div className="absolute inset-0 bg-accent/[0.05] pointer-events-none" />
            <div className="relative flex items-baseline justify-between gap-2 mb-1">
              <span className="text-accent tracking-[0.14em] uppercase font-semibold">
                T_super
              </span>
              <span className="font-mono text-accent">12–50ns</span>
            </div>
            <div className="relative text-ink mb-1">superposition</div>
            <div className="relative text-ink-faint leading-[1.45]">
              both A and B execute. routing flips.
            </div>
          </div>
          <div className="bg-bg-2 px-3.5 py-3">
            <div className="flex items-baseline justify-between gap-2 mb-1">
              <span className="text-ink-dim tracking-[0.14em] uppercase">
                T₁
              </span>
              <span className="font-mono text-ink-faint">50ns+</span>
            </div>
            <div className="text-ink mb-1">target authoritative</div>
            <div className="text-ink-faint leading-[1.45]">
              A releases. B holds identity.
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}
