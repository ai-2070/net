/**
 * Isometric line diagrams for the v2 lab-notebook rendition: thin wireframes
 * that explain a mechanism, drawn in the page's ink with acid green only on
 * the part that is live or moving. Each figure is built from three
 * primitives (box, plane, path) on one isometric grid.
 */

import type { ReactElement } from "react";

type Pt = readonly [number, number, number];

const COS = Math.cos(Math.PI / 6);
const SIN = Math.sin(Math.PI / 6);
const S = 9; // px per grid unit
const OX = 150;
const OY = 92;

function at([x, y, z]: Pt): [number, number] {
  return [OX + (x - y) * COS * S, OY + (x + y) * SIN * S - z * S];
}
function pts(list: ReadonlyArray<Pt>): string {
  return list
    .map((p) =>
      at(p)
        .map((n) => n.toFixed(1))
        .join(","),
    )
    .join(" ");
}

const INK = "rgba(230,232,224,0.78)";
const DIM = "rgba(230,232,224,0.32)";
const LIVE = "#c4ff3d";
const FACE = "#0d100d";

interface BoxOpts {
  live?: boolean;
  dim?: boolean;
  key: string;
}

/** A box sitting at (x, y, z) with width along x, depth along y, height z. */
function box(
  x: number,
  y: number,
  z: number,
  w: number,
  d: number,
  h: number,
  { live, dim, key }: BoxOpts,
): ReactElement {
  const stroke = live ? LIVE : dim ? DIM : INK;
  const top: Pt[] = [
    [x, y, z + h],
    [x + w, y, z + h],
    [x + w, y + d, z + h],
    [x, y + d, z + h],
  ];
  const right: Pt[] = [
    [x + w, y, z],
    [x + w, y + d, z],
    [x + w, y + d, z + h],
    [x + w, y, z + h],
  ];
  const left: Pt[] = [
    [x, y + d, z],
    [x + w, y + d, z],
    [x + w, y + d, z + h],
    [x, y + d, z + h],
  ];
  return (
    <g key={key} stroke={stroke} strokeWidth={1} strokeLinejoin="round">
      <polygon points={pts(left)} fill={FACE} />
      <polygon points={pts(right)} fill={FACE} />
      <polygon points={pts(top)} fill={live ? "rgba(196,255,61,0.12)" : FACE} />
    </g>
  );
}

/** A flat ground plane, drawn as a dashed outline. */
function plane(
  x: number,
  y: number,
  w: number,
  d: number,
  key: string,
): ReactElement {
  return (
    <polygon
      key={key}
      points={pts([
        [x, y, 0],
        [x + w, y, 0],
        [x + w, y + d, 0],
        [x, y + d, 0],
      ])}
      fill="none"
      stroke={DIM}
      strokeDasharray="2 3"
    />
  );
}

/** A path between grid points; `arrow` puts a head on the last segment. */
function path(
  list: ReadonlyArray<Pt>,
  key: string,
  {
    live,
    dashed,
    arrow,
  }: { live?: boolean; dashed?: boolean; arrow?: boolean } = {},
): ReactElement {
  const stroke = live ? LIVE : INK;
  const p = list.map(at);
  const a = p[p.length - 2]!;
  const b = p[p.length - 1]!;
  const ang = Math.atan2(b[1] - a[1], b[0] - a[0]);
  const head = arrow
    ? [
        [b[0] - 6 * Math.cos(ang - 0.45), b[1] - 6 * Math.sin(ang - 0.45)],
        b,
        [b[0] - 6 * Math.cos(ang + 0.45), b[1] - 6 * Math.sin(ang + 0.45)],
      ]
    : null;
  return (
    <g key={key} stroke={stroke} fill="none" strokeWidth={1}>
      <polyline
        points={p.map((q) => q.map((n) => n.toFixed(1)).join(",")).join(" ")}
        strokeDasharray={dashed ? "3 3" : undefined}
      />
      {head ? (
        <polyline
          points={head
            .map((q) => q.map((n) => n.toFixed(1)).join(","))
            .join(" ")}
        />
      ) : null}
    </g>
  );
}

/** An ellipse ring on the ground plane (gravity, reach). */
function ring(x: number, y: number, r: number, key: string): ReactElement {
  const list: Pt[] = Array.from({ length: 40 }, (_, i) => {
    const t = (i / 40) * Math.PI * 2;
    return [x + Math.cos(t) * r, y + Math.sin(t) * r, 0] as const;
  });
  return (
    <polygon
      key={key}
      points={pts(list)}
      fill="none"
      stroke={DIM}
      strokeDasharray="2 3"
    />
  );
}

export type IsoFigure =
  | "nrpc"
  | "redex"
  | "cortex"
  | "netdb"
  | "overflow"
  | "gravity"
  | "content-addressed"
  | "durability";

const FIGURES: Record<IsoFigure, () => ReactElement[]> = {
  // a request stream out, a response stream back, paired into one handle
  nrpc: () => [
    plane(-8, -4, 16, 8, "p"),
    box(-8, -3, 0, 3, 3, 3, { key: "client" }),
    box(5, -1, 0, 3, 3, 3, { key: "server" }),
    path(
      [
        [-5, -2, 1.8],
        [5, 0, 1.8],
      ],
      "req",
      { live: true, arrow: true },
    ),
    path(
      [
        [5, 1.4, 1],
        [-5, -0.6, 1],
      ],
      "res",
      { arrow: true },
    ),
  ],
  // a local log: records stacked, the newest appended on top
  redex: () => [
    ...Array.from({ length: 6 }, (_, i) =>
      box(-2, -2, i * 1.1, 6, 4, 0.8, { key: `r${i}`, dim: i < 3 }),
    ),
    box(-2, -2, 6.6, 6, 4, 0.8, { key: "new", live: true }),
    path(
      [
        [8, -2, 9.6],
        [5, -1, 7.8],
      ],
      "append",
      { live: true, arrow: true },
    ),
  ],
  // the same log folded event by event into in-memory state
  cortex: () => [
    ...Array.from({ length: 5 }, (_, i) =>
      box(-9, -1, i * 1.1, 4, 3, 0.8, { key: `r${i}`, dim: true }),
    ),
    path(
      [
        [-4.5, 0.5, 2.6],
        [1, 0.5, 2.6],
      ],
      "fold",
      { arrow: true },
    ),
    box(2, -2, 0, 5, 5, 5, { key: "state", live: true }),
  ],
  // one handle over several typed collections
  netdb: () => [
    box(-4, -4, 0, 4, 4, 3, { key: "a", dim: true }),
    box(1, -4, 0, 4, 4, 3, { key: "b", dim: true }),
    box(-4, 1, 0, 4, 4, 3, { key: "c", dim: true }),
    box(1, 1, 0, 4, 4, 3, { key: "d", live: true }),
    path(
      [
        [0.5, 0.5, 7],
        [0.5, 0.5, 3.4],
      ],
      "handle",
      { arrow: true },
    ),
  ],
  // one disk is full; the overflow lands on its neighbours
  overflow: () => [
    plane(-9, -4, 18, 9, "p"),
    box(-2, -2, 0, 4, 4, 5, { key: "full", live: true }),
    box(-8, -2, 0, 3, 3, 2, { key: "n1" }),
    box(5, -1, 0, 3, 3, 2, { key: "n2" }),
    path(
      [
        [-2, 0, 5.5],
        [-6.5, -0.5, 2.6],
      ],
      "s1",
      { dashed: true, arrow: true },
    ),
    path(
      [
        [2, 0, 5.5],
        [6.5, 0.5, 2.6],
      ],
      "s2",
      { dashed: true, arrow: true },
    ),
  ],
  // reads pull files toward the node that uses them
  gravity: () => [
    plane(-9, -5, 18, 10, "p"),
    ring(0, 0, 3.4, "r1"),
    ring(0, 0, 6, "r2"),
    box(-1, -1, 0, 2, 2, 3, { key: "node", live: true }),
    box(-6.5, -3.5, 0, 1.2, 1.2, 1.2, { key: "f1" }),
    box(4.6, 2.2, 0, 1.2, 1.2, 1.2, { key: "f2" }),
    box(3.2, -3.6, 0, 1.2, 1.2, 1.2, { key: "f3" }),
    path(
      [
        [-5.5, -2.7, 0.6],
        [-2.2, -1, 0.6],
      ],
      "g1",
      { dashed: true, arrow: true },
    ),
    path(
      [
        [4.4, 2, 0.6],
        [1.6, 0.8, 0.6],
      ],
      "g2",
      { dashed: true, arrow: true },
    ),
  ],
  // one hash is the handle, wherever the file lives
  "content-addressed": () => [
    plane(-9, -4, 18, 9, "p"),
    box(-8, -2, 0, 3, 3, 2, { key: "a" }),
    box(-1.5, 2, 0, 3, 3, 2, { key: "b" }),
    box(5, -2, 0, 3, 3, 2, { key: "c" }),
    box(-1, -3, 6, 2, 2, 2, { key: "hash", live: true }),
    path(
      [
        [0, -2, 6],
        [-6.5, -0.5, 2],
      ],
      "l1",
      { dashed: true },
    ),
    path(
      [
        [0, -2, 6],
        [0, 3.5, 2],
      ],
      "l2",
      { dashed: true },
    ),
    path(
      [
        [0, -2, 6],
        [6.5, -0.5, 2],
      ],
      "l3",
      { dashed: true },
    ),
  ],
  // three guarantees, three heights: fire-and-forget, fsync, replicated
  durability: () => [
    plane(-9, -3, 18, 7, "p"),
    box(-8, -1, 0, 3, 3, 1, { key: "t1", dim: true }),
    box(-1.5, -1, 0, 3, 3, 1, { key: "t2a" }),
    box(-1.5, -1, 1.2, 3, 3, 1, { key: "t2b" }),
    box(5, -1, 0, 3, 3, 1, { key: "t3a" }),
    box(5, -1, 1.2, 3, 3, 1, { key: "t3b" }),
    box(5, -1, 2.4, 3, 3, 1, { key: "t3c", live: true }),
  ],
};

export function IsoDiagram({
  figure,
  className = "",
}: {
  figure: IsoFigure;
  className?: string;
}) {
  return (
    <svg
      viewBox="0 0 300 170"
      className={`block w-full h-auto ${className}`}
      aria-hidden
      focusable="false"
    >
      {FIGURES[figure]()}
    </svg>
  );
}
