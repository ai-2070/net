// A made-up Blacksite Station for the landing page hero. The scene is ported
// from the dashboard's own map (blacksite web/views/station.ts): the Brain in
// the middle, team pods with desks, monitors drawn on canvases, light running
// between them, and dashed wires from the connected programs at the top. No
// server: a small scheduler below invents the work.

import * as THREE from "three";
import { BRAND_LOGOS, TERMINAL_SVG } from "./brandLogos";

export interface FeedEvent {
  id: number;
  who: string;
  what: string;
}

export interface StationOptions {
  /** One still frame instead of the animated map. */
  reduced: boolean;
  /** Called for each thing that "happens", for the hero's Live feed. */
  onFeed?: (e: FeedEvent) => void;
  /** Called once the first frame is drawn. */
  onReady?: () => void;
}

// ── the made-up project ─────────────────────────────────────────────────────

interface ToolDef {
  id: string;
  label: string;
  /** A brand in BRAND_LOGOS, "terminal", "img:<url>" for a picture, or a glyph. */
  logo: string;
  color: string;
  acts: ReadonlyArray<[action: string, detail: string]>;
}

const TOOLS: ReadonlyArray<ToolDef> = [
  {
    id: "blender",
    label: "Blender",
    logo: "blender",
    color: "#f5792a",
    acts: [
      ["running a script", "crusader_helm.blend"],
      ["exporting .glb", "longsword_v3.glb"],
      ["rendering", "turntable_kite_shield"],
    ],
  },
  {
    id: "comfyui",
    label: "ComfyUI",
    // the mark from the ComfyUI Desktop app's own icon, on a clear background
    logo: "img:/blacksite/comfyui.png",
    color: "#f2ff59",
    acts: [
      ["rendering textures", "padded cloth, weathered"],
      ["rendering textures", "heraldry: lion on red"],
    ],
  },
  {
    id: "gmail",
    label: "Gmail",
    logo: "gmail",
    color: "#ea4335",
    acts: [
      ["sending the cut for review", "trailer-cut-v4.mp4"],
      ["drafting an email", "press kit for launch week"],
      ["reading", "inbox: 3 new from playtesters"],
    ],
  },
  {
    id: "browser",
    label: "Web search",
    logo: "chrome",
    color: "#a9d3e2",
    acts: [
      ["searching", "15th century armour effigies"],
      ["reading", "britishmuseum.org"],
      ["searching", "indie game launch week"],
    ],
  },
  {
    id: "github",
    label: "GitHub",
    logo: "github",
    color: "#e6e8e0",
    acts: [
      ["reading", "game-studio/issues"],
      ["opening a PR", "art: new helm set"],
    ],
  },
  {
    id: "x",
    label: "X",
    logo: "x",
    color: "#e6e8e0",
    acts: [
      ["drafting a post", "The new armour set is in…"],
      ["reading", "mentions"],
    ],
  },
  {
    id: "terminal",
    label: "Terminal",
    logo: "terminal",
    color: "#c4ff3d",
    acts: [
      ["running", "gltf-transform optimize helm.glb"],
      ["running", "bun run build"],
    ],
  },
];

const TOOL = new Map(TOOLS.map((t) => [t.id, t]));

// Picture logos, loaded once; the monitors draw them once they have arrived.
const images = new Map<string, HTMLImageElement>();
function imageFor(tool: ToolDef): HTMLImageElement | null {
  if (!tool.logo.startsWith("img:")) return null;
  const src = tool.logo.slice(4);
  let img = images.get(src);
  if (!img) {
    img = new Image();
    img.src = src;
    images.set(src, img);
  }
  return img;
}
/** Resolves when every picture logo has loaded (or failed). */
function logosReady(): Promise<void> {
  const waits = TOOLS.map(imageFor)
    .filter((i): i is HTMLImageElement => i !== null && !i.complete)
    .map(
      (i) =>
        new Promise<void>((done) => {
          i.addEventListener("load", () => done(), { once: true });
          i.addEventListener("error", () => done(), { once: true });
        }),
    );
  return Promise.all(waits).then(() => undefined);
}

/** The tool's logo as an inline SVG (or its glyph), for the tiles and badges. */
function logoNode(tool: ToolDef, size: number): Node {
  const pic = imageFor(tool);
  if (pic) {
    const img = document.createElement("img");
    img.src = pic.src;
    img.width = size;
    img.height = size;
    img.alt = "";
    return img;
  }
  const brand = BRAND_LOGOS[tool.logo];
  if (!brand && tool.logo !== "terminal")
    return document.createTextNode(tool.logo);
  const svg = document.createElementNS(SVGNS, "svg");
  svg.setAttribute("viewBox", "0 0 24 24");
  svg.setAttribute("width", String(size));
  svg.setAttribute("height", String(size));
  svg.setAttribute("aria-hidden", "true");
  if (brand) {
    const path = document.createElementNS(SVGNS, "path");
    path.setAttribute("d", brand.d);
    path.setAttribute("fill", brand.hex);
    svg.append(path);
  } else {
    svg.setAttribute("fill", "none");
    svg.setAttribute("stroke", tool.color);
    svg.setAttribute("stroke-width", "2.4");
    svg.setAttribute("stroke-linecap", "round");
    svg.setAttribute("stroke-linejoin", "round");
    svg.innerHTML = TERMINAL_SVG;
  }
  return svg;
}

/** The same logo drawn on a canvas, centred on (cx, cy) and `size` pixels wide (the desk monitors). */
function drawLogo(
  ctx: CanvasRenderingContext2D,
  tool: ToolDef,
  cx: number,
  cy: number,
  size: number,
): void {
  const brand = BRAND_LOGOS[tool.logo];
  const pic = imageFor(tool);
  ctx.save();
  if (pic) {
    if (pic.complete && pic.naturalWidth)
      ctx.drawImage(pic, cx - size / 2, cy - size / 2, size, size);
  } else if (brand || tool.logo === "terminal") {
    const k = size / 24;
    ctx.translate(cx - size / 2, cy - size / 2);
    ctx.scale(k, k);
    if (brand) {
      ctx.fillStyle = brand.hex;
      ctx.fill(new Path2D(brand.d));
    } else {
      ctx.strokeStyle = tool.color;
      ctx.lineWidth = 2.4;
      ctx.lineCap = "round";
      ctx.lineJoin = "round";
      ctx.stroke(new Path2D("M12 19h8"));
      ctx.stroke(new Path2D("m4 17 6-6-6-6"));
    }
  } else {
    ctx.font = `${Math.round(size * 0.9)}px ${EMOJI}`;
    ctx.textAlign = "center";
    ctx.textBaseline = "middle";
    ctx.fillText(tool.logo, cx, cy + 3);
  }
  ctx.restore();
}

interface AgentDef {
  id: string;
  name: string;
  model: string;
  emoji: string;
  tools: ReadonlyArray<string>;
  ask: string;
}

interface TeamDef {
  id: string;
  name: string;
  color: string;
  desc: string;
  /** Where the pod sits around the Brain, as a direction on screen (degrees, 0 = right, 90 = up). */
  at: number;
  dist: number;
  /** Which side of the pod its card sits on, on screen (degrees). */
  card: number;
  lead: string;
  agents: ReadonlyArray<AgentDef>;
}

const TEAMS: ReadonlyArray<TeamDef> = [
  {
    id: "art",
    name: "Art Team",
    color: "#b8a03a",
    desc: "Historically accurate, browser-ready 3D models for the game.",
    at: -25,
    dist: 19,
    card: -90,
    lead: "producer",
    agents: [
      {
        id: "producer",
        name: "Studio Producer",
        model: "opus",
        emoji: "🎬",
        tools: ["github", "browser", "gmail"],
        ask: "approve the asset list",
      },
      {
        id: "modeler",
        name: "3D Artist",
        model: "sonnet",
        emoji: "🧊",
        tools: ["blender"],
        ask: "overwrite helm.glb",
      },
      {
        id: "techart",
        name: "Technical Artist",
        model: "sonnet",
        emoji: "🛠",
        tools: ["terminal", "blender"],
        ask: "run a shell command",
      },
      {
        id: "texture",
        name: "Texture Artist",
        model: "sonnet",
        emoji: "🎨",
        tools: ["comfyui"],
        ask: "save 12 texture files",
      },
      {
        id: "historian",
        name: "Historian",
        model: "sonnet",
        emoji: "📜",
        tools: ["browser"],
        ask: "download reference scans",
      },
    ],
  },
  {
    id: "video",
    name: "Video & Motion",
    color: "#c07a34",
    desc: "Trailers, explainers and short clips for social.",
    at: 75,
    dist: 18,
    card: 0,
    lead: "editor",
    agents: [
      {
        id: "editor",
        name: "Video Editor",
        model: "sonnet",
        emoji: "✂️",
        tools: ["gmail"],
        ask: "render the final cut",
      },
      {
        id: "motion",
        name: "Motion Designer",
        model: "sonnet",
        emoji: "✨",
        tools: ["blender"],
        ask: "replace the title card",
      },
      {
        id: "audio",
        name: "Audio Engineer",
        model: "haiku",
        emoji: "🎧",
        tools: ["terminal"],
        ask: "export the music bed",
      },
    ],
  },
  {
    id: "social",
    name: "Social",
    color: "#3d58c0",
    desc: "Grow the studio account with a post a day.",
    at: 195,
    dist: 18,
    card: -90,
    lead: "strategist",
    agents: [
      {
        id: "strategist",
        name: "Social Strategist",
        model: "sonnet",
        emoji: "📈",
        tools: ["browser", "gmail"],
        ask: "approve this week's plan",
      },
      {
        id: "xwriter",
        name: "X Writer",
        model: "sonnet",
        emoji: "🐦",
        tools: ["x"],
        ask: "post a draft from the Outbox",
      },
      {
        id: "analytics",
        name: "Analytics Reporter",
        model: "haiku",
        emoji: "📊",
        tools: ["browser", "github"],
        ask: "read the account stats",
      },
    ],
  },
];

const AGENT_COUNT = TEAMS.reduce((n, t) => n + t.agents.length, 0);

// ── scene types ─────────────────────────────────────────────────────────────

type Status = "idle" | "working" | "needs";

interface Act {
  tool: ToolDef | null;
  action: string;
  detail: string;
  since: number;
}

interface CanvasTex {
  tex: THREE.CanvasTexture;
  canvas: HTMLCanvasElement;
  ctx: CanvasRenderingContext2D;
}

interface Desk {
  agent: AgentDef;
  team: Team;
  color: string;
  lead: boolean;
  group: THREE.Group;
  pos: THREE.Vector3;
  screen: CanvasTex;
  screenGlow: THREE.Sprite;
  person: THREE.Group;
  orb: THREE.Mesh<THREE.SphereGeometry, THREE.MeshBasicMaterial>;
  orbGlow: THREE.Sprite;
  ring: THREE.Mesh<THREE.TorusGeometry, THREE.MeshBasicMaterial>;
  status: Status;
  act: Act | null;
  until: number;
  lastDraw: number;
  chip: HTMLElement;
  seed: number;
}

interface Rail {
  material: THREE.Material & { opacity: number };
  base: number;
}

interface Team {
  def: TeamDef;
  color: THREE.Color;
  light: THREE.Color;
  pod: THREE.Group;
  radius: number;
  desks: Desk[];
  hub: {
    holo: THREE.Mesh;
    holo2: THREE.Mesh;
    hubGlow: THREE.Sprite;
  };
  rails: Rail[];
  flash: number;
  card: HTMLElement;
  foot: HTMLElement;
}

interface Particle {
  sprite: THREE.Sprite;
  parent: THREE.Object3D;
  path: (t: number) => THREE.Vector3;
  t: number;
  speed: number;
  loop: boolean;
}

interface Label {
  el: HTMLElement;
  obj: THREE.Object3D;
  offset: THREE.Vector3;
  /** Pin the label by its edge, growing away from the scene (team cards). */
  side?: { tx: number; ty: number };
  world: THREE.Vector3;
}

interface Wire {
  tool: ToolDef;
  desk: Desk;
  el: SVGPathElement;
  flash: number;
  flow: number;
  speed: number;
  curve: number[] | null;
}

interface Dot {
  wire: Wire;
  t: number;
  el: SVGCircleElement;
}

interface Pending {
  at: number;
  run: () => void;
}

// ── small helpers ───────────────────────────────────────────────────────────

const SVGNS = "http://www.w3.org/2000/svg";
const SANS = '"Segoe UI", system-ui, -apple-system, sans-serif';
const MONO = 'Consolas, "SFMono-Regular", ui-monospace, monospace';
const EMOJI =
  '"Segoe UI Emoji", "Apple Color Emoji", "Noto Color Emoji", sans-serif';

function pick<T>(arr: ReadonlyArray<T>): T {
  return arr[Math.floor(Math.random() * arr.length)] as T;
}

function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  className: string,
  text?: string,
): HTMLElementTagNameMap[K] {
  const e = document.createElement(tag);
  e.className = className;
  if (text !== undefined) e.textContent = text;
  return e;
}

function canvasTex(
  w: number,
  h: number,
  draw?: (ctx: CanvasRenderingContext2D) => void,
): CanvasTex {
  const canvas = document.createElement("canvas");
  canvas.width = w;
  canvas.height = h;
  const ctx = canvas.getContext("2d");
  if (!ctx) throw new Error("no 2d canvas");
  draw?.(ctx);
  const tex = new THREE.CanvasTexture(canvas);
  tex.colorSpace = THREE.SRGBColorSpace;
  tex.anisotropy = 4;
  return { tex, canvas, ctx };
}

function roundRect(
  ctx: CanvasRenderingContext2D,
  x: number,
  y: number,
  w: number,
  h: number,
  r: number,
): void {
  ctx.beginPath();
  ctx.moveTo(x + r, y);
  ctx.arcTo(x + w, y, x + w, y + h, r);
  ctx.arcTo(x + w, y + h, x, y + h, r);
  ctx.arcTo(x, y + h, x, y, r);
  ctx.arcTo(x, y, x + w, y, r);
  ctx.closePath();
}

function fitText(
  ctx: CanvasRenderingContext2D,
  text: string,
  max: number,
): string {
  let t = text;
  if (ctx.measureText(t).width <= max) return t;
  while (t.length > 1 && ctx.measureText(t + "…").width > max)
    t = t.slice(0, -1);
  return t + "…";
}

function polyline(points: THREE.Vector3[]): (t: number) => THREE.Vector3 {
  const lens: number[] = [];
  let total = 0;
  for (let i = 0; i < points.length - 1; i++) {
    const l = (points[i] as THREE.Vector3).distanceTo(
      points[i + 1] as THREE.Vector3,
    );
    lens.push(l);
    total += l;
  }
  return (t) => {
    let d = Math.max(0, Math.min(1, t)) * total;
    for (let i = 0; i < lens.length; i++) {
      const len = lens[i] as number;
      if (d <= len || i === lens.length - 1) {
        return (points[i] as THREE.Vector3)
          .clone()
          .lerp(points[i + 1] as THREE.Vector3, len ? Math.min(1, d / len) : 0);
      }
      d -= len;
    }
    return (points[0] as THREE.Vector3).clone();
  };
}

// The camera looks from the +x/+z corner (isometric). A direction on screen
// (degrees, 0 = right, 90 = up the floor) as a direction on the floor.
const AZ = Math.PI / 4;
const EL = Math.atan(1 / Math.SQRT2);
function floorDir(deg: number): THREE.Vector3 {
  const a = (deg * Math.PI) / 180;
  const right = new THREE.Vector3(Math.cos(AZ), 0, -Math.sin(AZ));
  const up = new THREE.Vector3(-Math.sin(AZ), 0, -Math.cos(AZ));
  return right
    .multiplyScalar(Math.cos(a))
    .addScaledVector(up, Math.sin(a))
    .normalize();
}

const podRadius = (n: number): number =>
  Math.max(6, (Math.max(1, n) * 5.6) / (2 * Math.PI) + 3.4);

// ── the map ─────────────────────────────────────────────────────────────────

export function startStation(
  host: HTMLElement,
  opts: StationOptions,
): () => void {
  let renderer: THREE.WebGLRenderer;
  try {
    renderer = new THREE.WebGLRenderer({
      antialias: true,
      powerPreference: "high-performance",
    });
  } catch {
    // no WebGL: the screenshot underneath stays
    return () => {};
  }

  const root = el("div", "bs-station");
  const canvasHost = el("div", "bs-station-canvas");
  const wiresSvg = document.createElementNS(SVGNS, "svg");
  wiresSvg.setAttribute("class", "bs-station-wires");
  const wireLayer = document.createElementNS(SVGNS, "g");
  const dotLayer = document.createElementNS(SVGNS, "g");
  wiresSvg.append(wireLayer, dotLayer);
  const labels = el("div", "bs-station-labels");
  const tilesBar = el("div", "bs-station-tiles");
  root.append(canvasHost, wiresSvg, labels, tilesBar);
  host.append(root);

  renderer.setPixelRatio(Math.min(window.devicePixelRatio || 1, 1.75));
  renderer.setClearColor(0x070807, 1);
  canvasHost.append(renderer.domElement);

  const scene = new THREE.Scene();
  const camera = new THREE.OrthographicCamera(-1, 1, 1, -1, -600, 1200);
  scene.add(new THREE.AmbientLight(0xbfc8ff, 0.55));
  const sun = new THREE.DirectionalLight(0xffffff, 1.1);
  sun.position.set(30, 60, 20);
  scene.add(sun);
  const rimLight = new THREE.DirectionalLight(0x7fb6ff, 0.35);
  rimLight.position.set(-40, 20, -30);
  scene.add(rimLight);
  {
    const pts: number[] = [];
    for (let x = -150; x <= 150; x += 3)
      for (let z = -150; z <= 150; z += 3) pts.push(x, -0.01, z);
    const g = new THREE.BufferGeometry();
    g.setAttribute("position", new THREE.Float32BufferAttribute(pts, 3));
    scene.add(
      new THREE.Points(
        g,
        new THREE.PointsMaterial({
          color: 0x262b24,
          size: 2,
          sizeAttenuation: false,
        }),
      ),
    );
  }
  const world = new THREE.Group();
  scene.add(world);

  const textures: THREE.Texture[] = [];
  const glowTex = canvasTex(128, 128, (ctx) => {
    const g = ctx.createRadialGradient(64, 64, 0, 64, 64, 64);
    g.addColorStop(0, "rgba(255,255,255,1)");
    g.addColorStop(0.25, "rgba(255,255,255,0.55)");
    g.addColorStop(0.6, "rgba(255,255,255,0.12)");
    g.addColorStop(1, "rgba(255,255,255,0)");
    ctx.fillStyle = g;
    ctx.fillRect(0, 0, 128, 128);
  }).tex;
  textures.push(glowTex);

  const glyphTex = (glyph: string, color: string): THREE.Texture => {
    const t = canvasTex(128, 128, (ctx) => {
      ctx.fillStyle = "#0d100d";
      ctx.beginPath();
      ctx.arc(64, 64, 58, 0, Math.PI * 2);
      ctx.fill();
      ctx.strokeStyle = color;
      ctx.lineWidth = 5;
      ctx.stroke();
      ctx.font = `58px ${EMOJI}`;
      ctx.textAlign = "center";
      ctx.textBaseline = "middle";
      ctx.fillStyle = color;
      ctx.fillText(glyph, 64, 68);
    }).tex;
    textures.push(t);
    return t;
  };

  const glowSprite = (
    color: THREE.ColorRepresentation,
    opacity: number,
  ): THREE.Sprite =>
    new THREE.Sprite(
      new THREE.SpriteMaterial({
        map: glowTex,
        color,
        transparent: true,
        opacity,
        blending: THREE.AdditiveBlending,
        depthWrite: false,
      }),
    );

  // ── the Brain ──
  const brain = (() => {
    const g = new THREE.Group();
    const ped = new THREE.Mesh(
      new THREE.CylinderGeometry(3.6, 4.4, 0.8, 6),
      new THREE.MeshStandardMaterial({
        color: 0x121612,
        roughness: 0.8,
        metalness: 0.3,
      }),
    );
    ped.position.y = 0.4;
    g.add(ped);
    const pedEdges = new THREE.LineSegments(
      new THREE.EdgesGeometry(ped.geometry),
      new THREE.LineBasicMaterial({
        color: 0x7a9a2c,
        transparent: true,
        opacity: 0.6,
      }),
    );
    pedEdges.position.copy(ped.position);
    g.add(pedEdges);
    const N = 2200;
    const pos = new Float32Array(N * 3);
    const col = new Float32Array(N * 3);
    const c1 = new THREE.Color(0x7fb6ff);
    const c2 = new THREE.Color(0xe6f2ff);
    for (let i = 0; i < N; i++) {
      const r = 2.6 * Math.pow(Math.random(), 0.35);
      const th = Math.random() * Math.PI * 2;
      const ph = Math.acos(2 * Math.random() - 1);
      pos.set(
        [
          r * Math.sin(ph) * Math.cos(th),
          r * Math.cos(ph) * 0.8,
          r * Math.sin(ph) * Math.sin(th),
        ],
        i * 3,
      );
      const c = c1.clone().lerp(c2, Math.random());
      col.set([c.r, c.g, c.b], i * 3);
    }
    const pg = new THREE.BufferGeometry();
    pg.setAttribute("position", new THREE.BufferAttribute(pos, 3));
    pg.setAttribute("color", new THREE.BufferAttribute(col, 3));
    const cloud = new THREE.Points(
      pg,
      new THREE.PointsMaterial({
        size: 0.11,
        vertexColors: true,
        transparent: true,
        opacity: 0.9,
        blending: THREE.AdditiveBlending,
        depthWrite: false,
      }),
    );
    cloud.position.y = 3.2;
    g.add(cloud);
    const ico = new THREE.LineSegments(
      new THREE.EdgesGeometry(new THREE.IcosahedronGeometry(3.1, 1)),
      new THREE.LineBasicMaterial({
        color: 0xc4ff3d,
        transparent: true,
        opacity: 0.18,
      }),
    );
    ico.position.y = 3.2;
    g.add(ico);
    const halo = glowSprite(0x6fa8ff, 0.7);
    halo.scale.set(11, 11, 1);
    halo.position.y = 3.2;
    g.add(halo);
    const beam = new THREE.Mesh(
      new THREE.CylinderGeometry(0.18, 0.7, 16, 24, 1, true),
      new THREE.MeshBasicMaterial({
        color: 0xffd36e,
        transparent: true,
        opacity: 0.16,
        blending: THREE.AdditiveBlending,
        depthWrite: false,
        side: THREE.DoubleSide,
      }),
    );
    beam.position.y = 12;
    g.add(beam);
    const star = glowSprite(0xffcc55, 0.9);
    star.scale.set(6, 6, 1);
    star.position.y = 7.4;
    g.add(star);
    const light = new THREE.PointLight(0x9fd0ff, 30, 40, 1.6);
    light.position.y = 4;
    g.add(light);
    world.add(g);
    return { group: g, cloud, ico, halo, beam, star, light, pulse: 0 };
  })();

  // ── labels ──
  const labelList: Label[] = [];
  const addLabel = (l: Omit<Label, "world">): void => {
    labels.append(l.el);
    labelList.push({ ...l, world: new THREE.Vector3() });
  };

  // ── desks ──
  const drawScreen = (desk: Desk, tick: number): void => {
    const { ctx, canvas } = desk.screen;
    const W = canvas.width;
    const H = canvas.height;
    const a = desk.agent;
    ctx.textBaseline = "middle";
    if (desk.status === "idle") {
      ctx.fillStyle = "#e4e8dd";
      ctx.fillRect(0, 0, W, H);
      ctx.fillStyle = "#cdd2c6";
      ctx.fillRect(0, 0, W, 40);
      ["#ff6b6b", "#f0b04a", "#5fe3a0"].forEach((c, i) => {
        ctx.fillStyle = c;
        ctx.beginPath();
        ctx.arc(24 + i * 24, 20, 7, 0, Math.PI * 2);
        ctx.fill();
      });
      ctx.fillStyle = "#3a3f37";
      ctx.font = `bold 20px ${MONO}`;
      ctx.textAlign = "right";
      ctx.fillText(desk.lead ? "LEAD" : "TEAM", W - 20, 21);
      ctx.textAlign = "left";
      ctx.font = `84px ${EMOJI}`;
      ctx.fillText(a.emoji, 28, 140);
      ctx.fillStyle = "#16190f";
      ctx.font = `bold 40px ${SANS}`;
      ctx.fillText(fitText(ctx, a.name, W - 160), 140, 118);
      ctx.fillStyle = "#5f6559";
      ctx.font = `26px ${MONO}`;
      ctx.fillText(fitText(ctx, `○ ready · ${a.model}`, W - 160), 140, 162);
      for (let i = 0; i < 4; i++) {
        ctx.fillStyle = i === 0 ? desk.color : "#c3c8bc";
        roundRect(ctx, 28, 214 + i * 24, 80 + ((i * 97) % 300), 10, 5);
        ctx.fill();
      }
    } else if (desk.status === "needs") {
      ctx.fillStyle = "#21180a";
      ctx.fillRect(0, 0, W, H);
      ctx.fillStyle = Math.sin(tick * 6) > 0 ? "#f0b04a" : "#7a5a24";
      ctx.font = `bold 120px ${SANS}`;
      ctx.textAlign = "center";
      ctx.fillText("!", 90, H / 2 + 6);
      ctx.textAlign = "left";
      ctx.fillStyle = "#f0b04a";
      ctx.font = `bold 40px ${SANS}`;
      ctx.fillText("Waiting for you", 170, 120);
      ctx.fillStyle = "#e6e8e0";
      ctx.font = `24px ${SANS}`;
      ctx.fillText(fitText(ctx, a.ask, W - 190), 170, 168);
      ctx.fillStyle = "#9a8a6a";
      ctx.font = `22px ${MONO}`;
      ctx.fillText("open Needs you to answer", 170, 214);
    } else {
      const act = desk.act;
      const tool = act?.tool ?? null;
      const color = tool ? tool.color : "#c4ff3d";
      ctx.fillStyle = "#0b130a";
      ctx.fillRect(0, 0, W, H);
      const cx = 86;
      const cy = 150;
      const g = ctx.createRadialGradient(cx, cy, 10, cx, cy, 90);
      g.addColorStop(0, color + "55");
      g.addColorStop(1, color + "00");
      ctx.fillStyle = g;
      ctx.fillRect(0, 50, 200, 200);
      ctx.fillStyle = "#121a11";
      ctx.beginPath();
      ctx.arc(cx, cy, 54, 0, Math.PI * 2);
      ctx.fill();
      ctx.strokeStyle = color;
      ctx.lineWidth = 4;
      ctx.beginPath();
      ctx.arc(
        cx,
        cy,
        54,
        -Math.PI / 2 + tick * 3,
        -Math.PI / 2 + tick * 3 + Math.PI * 1.4,
      );
      ctx.stroke();
      if (tool) drawLogo(ctx, tool, cx, cy, 60);
      else {
        ctx.fillStyle = color;
        ctx.textAlign = "center";
        ctx.font = `56px ${EMOJI}`;
        ctx.fillText(a.emoji, cx, cy + 3);
      }
      ctx.textAlign = "left";
      ctx.textBaseline = "middle";
      ctx.fillStyle = "#e6e8e0";
      ctx.font = `bold 40px ${SANS}`;
      ctx.fillText(
        fitText(ctx, tool ? tool.label : act?.action || "Thinking", W - 190),
        170,
        104,
      );
      ctx.fillStyle = color;
      ctx.font = `28px ${SANS}`;
      const dots = ".".repeat(1 + (Math.floor(tick * 2) % 3));
      ctx.fillText(
        fitText(ctx, tool ? act?.action || "" : dots, W - 190),
        170,
        150,
      );
      if (act?.detail) {
        ctx.fillStyle = "#8f968a";
        ctx.font = `22px ${MONO}`;
        ctx.fillText(fitText(ctx, act.detail, W - 190), 170, 192);
      }
      ctx.fillStyle = "#1c2a18";
      roundRect(ctx, 28, 262, W - 160, 10, 5);
      ctx.fill();
      const pw = 120;
      const px = 28 + ((tick * 160) % (W - 160 + pw)) - pw;
      ctx.save();
      roundRect(ctx, 28, 262, W - 160, 10, 5);
      ctx.clip();
      ctx.fillStyle = color;
      ctx.fillRect(px, 262, pw, 10);
      ctx.restore();
      const secs = act ? Math.max(0, Math.round(tick - act.since)) : 0;
      ctx.fillStyle = "#8f968a";
      ctx.font = `bold 22px ${MONO}`;
      ctx.textAlign = "right";
      ctx.fillText(`${secs}s`, W - 22, 268);
      ctx.textAlign = "left";
      ctx.fillStyle = "#5a6455";
      ctx.font = `bold 18px ${MONO}`;
      ctx.fillText(
        fitText(ctx, `${a.name.toUpperCase()} · ${a.model}`, W - 60),
        28,
        26,
      );
    }
    desk.screen.tex.needsUpdate = true;
  };

  const makeDesk = (
    agent: AgentDef,
    team: Team,
    lead: boolean,
    pos: THREE.Vector3,
  ): Desk => {
    const g = new THREE.Group();
    const color = new THREE.Color(team.def.color);
    const dark = new THREE.MeshStandardMaterial({
      color: 0x1a1e1a,
      roughness: 0.7,
    });
    const top = new THREE.Mesh(
      new THREE.BoxGeometry(2.6, 0.12, 1.3),
      new THREE.MeshStandardMaterial({ color: 0x2a2f29, roughness: 0.6 }),
    );
    top.position.y = 0.9;
    g.add(top);
    for (const x of [-1.15, 1.15]) {
      const leg = new THREE.Mesh(new THREE.BoxGeometry(0.1, 0.9, 1.1), dark);
      leg.position.set(x, 0.45, 0);
      g.add(leg);
    }
    const mon = new THREE.Mesh(
      new THREE.BoxGeometry(1.7, 1.08, 0.08),
      new THREE.MeshStandardMaterial({
        color: 0x0e100e,
        roughness: 0.4,
        metalness: 0.4,
      }),
    );
    mon.position.set(0, 1.68, -0.38);
    g.add(mon);
    const stand = new THREE.Mesh(new THREE.BoxGeometry(0.12, 0.3, 0.12), dark);
    stand.position.set(0, 1.05, -0.38);
    g.add(stand);
    const screen = canvasTex(512, 320);
    textures.push(screen.tex);
    const scr = new THREE.Mesh(
      new THREE.PlaneGeometry(1.58, 0.98),
      new THREE.MeshBasicMaterial({ map: screen.tex, toneMapped: false }),
    );
    scr.position.set(0, 1.68, -0.33);
    g.add(scr);
    const screenGlow = glowSprite(0xdfe3d8, 0.12);
    screenGlow.scale.set(3.2, 2.4, 1);
    screenGlow.position.set(0, 1.7, -0.2);
    g.add(screenGlow);
    const chair = new THREE.Mesh(new THREE.BoxGeometry(0.75, 0.12, 0.7), dark);
    chair.position.set(0, 0.6, 0.75);
    g.add(chair);
    const back = new THREE.Mesh(new THREE.BoxGeometry(0.75, 0.7, 0.1), dark);
    back.position.set(0, 0.98, 1.08);
    g.add(back);
    const person = new THREE.Group();
    const body = new THREE.Mesh(
      new THREE.CylinderGeometry(0.22, 0.3, 0.65, 12),
      new THREE.MeshStandardMaterial({
        color: color.clone().lerp(new THREE.Color(0xffffff), 0.15),
        roughness: 0.6,
      }),
    );
    body.position.y = 0.98;
    const head = new THREE.Mesh(
      new THREE.SphereGeometry(0.21, 16, 12),
      new THREE.MeshStandardMaterial({ color: 0xe6e8e0, roughness: 0.5 }),
    );
    head.position.y = 1.5;
    person.add(body, head);
    person.position.z = 0.62;
    g.add(person);
    const orb = new THREE.Mesh(
      new THREE.SphereGeometry(0.13, 12, 10),
      new THREE.MeshBasicMaterial({ color: 0x6b7066 }),
    );
    orb.position.set(0.95, 2.45, -0.38);
    g.add(orb);
    const orbGlow = glowSprite(0xc4ff3d, 0);
    orbGlow.scale.set(1.4, 1.4, 1);
    orbGlow.position.copy(orb.position);
    g.add(orbGlow);
    const ring = new THREE.Mesh(
      new THREE.TorusGeometry(1.9, 0.07, 8, 48),
      new THREE.MeshBasicMaterial({
        color: 0xc4ff3d,
        transparent: true,
        opacity: 0,
      }),
    );
    ring.rotation.x = Math.PI / 2;
    ring.position.y = 0.08;
    g.add(ring);
    g.position.copy(pos).setY(0.5);
    // the monitor faces the camera, as on the real map
    g.rotation.y = AZ;

    const chip = el("div", "bs-chip");
    chip.append(el("span", "bs-chip-dot"));
    if (lead) chip.append(el("span", "bs-chip-lead", "★"));
    chip.append(el("span", "", agent.name));
    chip.append(el("span", "bs-chip-model", agent.model));

    const desk: Desk = {
      agent,
      team,
      color: team.def.color,
      lead,
      group: g,
      pos,
      screen,
      screenGlow,
      person,
      orb,
      orbGlow,
      ring,
      status: "idle",
      act: null,
      until: 0,
      lastDraw: -1,
      chip,
      seed: Math.random() * 10,
    };
    drawScreen(desk, 0);
    addLabel({ el: chip, obj: g, offset: new THREE.Vector3(0, 3.3, 0) });
    return desk;
  };

  // ── teams ──
  const particles: Particle[] = [];
  const spawn = (
    p: Omit<Particle, "sprite"> & {
      color: THREE.Color;
      size: number;
      opacity?: number;
    },
  ): void => {
    const sprite = glowSprite(p.color, p.opacity ?? 1);
    sprite.scale.set(p.size, p.size, 1);
    sprite.visible = false;
    p.parent.add(sprite);
    particles.push({
      sprite,
      parent: p.parent,
      path: p.path,
      t: p.t,
      speed: p.speed,
      loop: p.loop,
    });
  };

  const linkPoint = (team: Team, t: number): THREE.Vector3 => {
    const p2 = team.pod.position.clone().setY(1.7);
    const dir = p2.clone().setY(0);
    const len = dir.length() || 1;
    dir.divideScalar(len);
    const p0 = dir.clone().multiplyScalar(4.4).setY(0.9);
    const edge = team.pod.position
      .clone()
      .addScaledVector(dir, -team.radius)
      .setY(0.7);
    const near = t < 0.85;
    const end = near ? edge : p2;
    const tt = near ? t / 0.85 : (t - 0.85) / 0.15;
    const from = near ? p0 : edge;
    const ctrl = from
      .clone()
      .lerp(end, 0.5)
      .setY(Math.max(from.y, end.y) + (near ? Math.min(6, len * 0.12) : 0.5));
    const u = 1 - tt;
    return from
      .clone()
      .multiplyScalar(u * u)
      .addScaledVector(ctrl, 2 * u * tt)
      .addScaledVector(end, tt * tt);
  };

  const teams: Team[] = [];
  const desks: Desk[] = [];

  for (const def of TEAMS) {
    const color = new THREE.Color(def.color);
    const light = color.clone().lerp(new THREE.Color(0xffffff), 0.4);
    const R = podRadius(def.agents.length);
    const pod = new THREE.Group();
    pod.position.copy(floorDir(def.at).multiplyScalar(def.dist));
    world.add(pod);

    const slab = new THREE.Mesh(
      new THREE.CylinderGeometry(R, R + 0.25, 0.5, 64),
      new THREE.MeshStandardMaterial({
        color: color.clone().multiplyScalar(0.42),
        roughness: 0.55,
        metalness: 0.15,
        emissive: color,
        emissiveIntensity: 0.1,
      }),
    );
    slab.position.y = 0.25;
    pod.add(slab);
    const top = new THREE.Mesh(
      new THREE.CircleGeometry(R - 0.15, 64),
      new THREE.MeshStandardMaterial({
        color: color.clone().multiplyScalar(0.62),
        roughness: 0.75,
        emissive: color,
        emissiveIntensity: 0.07,
      }),
    );
    top.rotation.x = -Math.PI / 2;
    top.position.y = 0.51;
    pod.add(top);
    const rim = new THREE.LineSegments(
      new THREE.EdgesGeometry(slab.geometry, 30),
      new THREE.LineBasicMaterial({
        color: light,
        transparent: true,
        opacity: 0.7,
      }),
    );
    rim.position.copy(slab.position);
    pod.add(rim);
    const additive = (opacity: number): THREE.MeshBasicMaterial =>
      new THREE.MeshBasicMaterial({
        color: light,
        transparent: true,
        opacity,
        side: THREE.DoubleSide,
        blending: THREE.AdditiveBlending,
        depthWrite: false,
      });
    const track = new THREE.Mesh(
      new THREE.RingGeometry(R * 0.42, R * 0.42 + 0.08, 64),
      additive(0.35),
    );
    track.rotation.x = -Math.PI / 2;
    track.position.y = 0.53;
    pod.add(track);
    const under = glowSprite(color, 0.14);
    under.scale.set(R * 3, R * 2.2, 1);
    under.position.y = -0.2;
    pod.add(under);

    // the chat hub in the middle
    const base = new THREE.Mesh(
      new THREE.CylinderGeometry(0.95, 1.2, 0.55, 6),
      new THREE.MeshStandardMaterial({
        color: 0x141714,
        roughness: 0.5,
        metalness: 0.3,
        emissive: color,
        emissiveIntensity: 0.35,
      }),
    );
    base.position.y = 0.8;
    pod.add(base);
    const holo = new THREE.Mesh(
      new THREE.TorusGeometry(0.9, 0.05, 8, 48),
      additive(0.8),
    );
    holo.rotation.x = Math.PI / 2;
    holo.position.y = 2.1;
    pod.add(holo);
    const holo2 = new THREE.Mesh(
      new THREE.TorusGeometry(0.6, 0.04, 8, 40),
      additive(0.8),
    );
    holo2.position.y = 2.1;
    pod.add(holo2);
    const mark = new THREE.Sprite(
      new THREE.SpriteMaterial({
        map: glyphTex("💬", "#" + light.getHexString()),
        transparent: true,
        depthWrite: false,
      }),
    );
    mark.scale.set(1.3, 1.3, 1);
    mark.position.y = 2.1;
    pod.add(mark);
    const hubGlow = glowSprite(color, 0.45);
    hubGlow.scale.set(4.5, 4.5, 1);
    hubGlow.position.y = 1.8;
    pod.add(hubGlow);

    // the team card
    const card = el("div", "bs-card");
    card.style.setProperty("--c", def.color);
    const name = el("div", "bs-card-name");
    name.append(
      el("span", "bs-dot"),
      document.createTextNode(def.name.toUpperCase()),
    );
    const big = el("div", "bs-card-big");
    big.append(
      el("b", "", String(def.agents.length)),
      el("span", "", "AGENTS"),
      el(
        "span",
        "bs-card-lead",
        `★ ${def.agents.find((a) => a.id === def.lead)?.name ?? ""}`,
      ),
    );
    const foot = el("div", "bs-card-foot", "quiet");
    const icons = el("div", "bs-card-icons");
    for (const id of new Set(def.agents.flatMap((x) => x.tools))) {
      const tool = TOOL.get(id);
      if (!tool) continue;
      const badge = el("span", "bs-badge");
      badge.style.setProperty("--c", tool.color);
      badge.title = tool.label;
      badge.append(logoNode(tool, 12));
      icons.append(badge);
    }
    card.append(name, big, el("div", "bs-card-desc", def.desc), icons, foot);
    const cardDir = floorDir(def.card);
    const anchor = new THREE.Object3D();
    anchor.position.copy(cardDir.clone().multiplyScalar(R + 2.4));
    pod.add(anchor);
    const a = (def.card * Math.PI) / 180;
    const side =
      Math.abs(Math.cos(a)) >= Math.abs(Math.sin(a))
        ? { tx: Math.cos(a) < 0 ? -100 : 0, ty: -50 }
        : { tx: -50, ty: Math.sin(a) > 0 ? -100 : 0 };
    addLabel({
      el: card,
      obj: anchor,
      offset: new THREE.Vector3(0, 0.5, 0),
      side,
    });

    // the link to the Brain
    const linkMat = new THREE.LineBasicMaterial({
      color: light,
      transparent: true,
      opacity: 0.45,
      blending: THREE.AdditiveBlending,
      depthWrite: false,
    });
    const team: Team = {
      def,
      color,
      light,
      pod,
      radius: R,
      desks: [],
      hub: { holo, holo2, hubGlow },
      rails: [
        { material: track.material, base: 0.35 },
        { material: linkMat, base: 0.45 },
      ],
      flash: 0,
      card,
      foot,
    };
    const linkPts: THREE.Vector3[] = [];
    for (let k = 0; k <= 32; k++)
      linkPts.push(linkPoint(team, (k / 32) * 0.85));
    world.add(
      new THREE.Line(
        new THREE.BufferGeometry().setFromPoints(linkPts),
        linkMat,
      ),
    );

    // desks around the hub, the lead facing the Brain
    const toBrain = pod.position.clone().multiplyScalar(-1).setY(0);
    const baseAng = Math.atan2(toBrain.z, toBrain.x);
    const order = [
      ...def.agents.filter((x) => x.id === def.lead),
      ...def.agents.filter((x) => x.id !== def.lead),
    ];
    order.forEach((agent, k) => {
      const ang = baseAng + (k / order.length) * Math.PI * 2;
      const pos = new THREE.Vector3(
        Math.cos(ang) * (R - 1.55),
        0,
        Math.sin(ang) * (R - 1.55),
      );
      const lead = agent.id === def.lead;
      const desk = makeDesk(agent, team, lead, pos);
      pod.add(desk.group);
      team.desks.push(desk);
      desks.push(desk);
      if (lead) {
        const seat = new THREE.Mesh(
          new THREE.RingGeometry(1.75, 1.95, 48),
          new THREE.MeshBasicMaterial({
            color: 0xffcc55,
            transparent: true,
            opacity: 0.75,
            side: THREE.DoubleSide,
            blending: THREE.AdditiveBlending,
            depthWrite: false,
          }),
        );
        seat.rotation.x = -Math.PI / 2;
        seat.position.copy(pos).setY(0.56);
        pod.add(seat);
      }
      const spokeMat = new THREE.LineBasicMaterial({
        color: light,
        transparent: true,
        opacity: 0.3,
        blending: THREE.AdditiveBlending,
        depthWrite: false,
      });
      pod.add(
        new THREE.Line(
          new THREE.BufferGeometry().setFromPoints([
            pos.clone().setY(0.6),
            new THREE.Vector3(0, 0.6, 0),
          ]),
          spokeMat,
        ),
      );
      team.rails.push({ material: spokeMat, base: 0.3 });
    });

    // the conversation circling the hub, and a slow stream from the Brain
    for (let k = 0; k < 3; k++) {
      spawn({
        parent: pod,
        path: (t) =>
          new THREE.Vector3(
            Math.cos(t * Math.PI * 2) * R * 0.42,
            0.62,
            Math.sin(t * Math.PI * 2) * R * 0.42,
          ),
        t: k / 3,
        speed: 0.06,
        loop: true,
        color: light,
        size: 0.45,
        opacity: 0.55,
      });
    }
    for (let k = 0; k < 2; k++) {
      spawn({
        parent: world,
        path: (t) => linkPoint(team, t),
        t: k / 2,
        speed: 0.07 + Math.random() * 0.03,
        loop: true,
        color: light,
        size: 0.5,
        opacity: 0.45,
      });
    }
    teams.push(team);
  }

  {
    const label = el("div", "bs-brain");
    label.append(
      el("span", "bs-brain-mark", "◎"),
      document.createTextNode("THE BRAIN"),
      el("span", "bs-brain-n", `${teams.length} teams · ${AGENT_COUNT} agents`),
    );
    addLabel({
      el: label,
      obj: brain.group,
      offset: new THREE.Vector3(0, 9.5, 0),
    });
  }

  // ── the connected programs, with a wire to every desk that uses one ──
  const tileEls = new Map<string, HTMLElement>();
  {
    const head = el("span", "bs-tiles-head");
    head.append(
      el("span", "bs-tiles-dot"),
      document.createTextNode("CONNECTED TO"),
    );
    tilesBar.append(head);
    for (const t of TOOLS) {
      const tile = el("span", "bs-tile");
      tile.append(logoNode(t, 14));
      tile.style.setProperty("--c", t.color);
      tile.title = t.label;
      tilesBar.append(tile);
      tileEls.set(t.id, tile);
    }
  }
  const wires: Wire[] = [];
  for (const desk of desks) {
    for (const id of desk.agent.tools) {
      const tool = TOOL.get(id);
      if (!tool) continue;
      const path = document.createElementNS(SVGNS, "path");
      path.setAttribute("fill", "none");
      path.setAttribute("stroke-dasharray", "3 4");
      path.setAttribute("stroke", tool.color);
      wireLayer.append(path);
      wires.push({
        tool,
        desk,
        el: path,
        flash: 0,
        flow: Math.random() * 7,
        speed: 11,
        curve: null,
      });
    }
  }
  const dots: Dot[] = [];

  // ── camera ──
  let W = 1;
  let H = 1;
  const v3 = new THREE.Vector3();
  const updateCamera = (): void => {
    const wide = W >= 1024;
    // on wide screens the Brain sits right of the text; on phones it is centred and dimmed
    const fx = wide ? 0.66 : 0.5;
    // short windows: lift it so the cards below the pods stay on screen
    const fy = H < 700 ? 0.5 : 0.56;
    // world units from top to bottom of the hero: the map is about 58 units
    // tall and 88 wide, so it shrinks to whichever side is shorter
    const v = wide ? H / Math.min(H / 58, W / 88) : 64;
    const aspect = W / H;
    camera.left = -v * aspect * fx;
    camera.right = v * aspect * (1 - fx);
    camera.top = v * fy;
    camera.bottom = -v * (1 - fy);
    const D = 300;
    const target = new THREE.Vector3(0, 0, 0);
    camera.position.set(
      target.x + Math.sin(AZ) * Math.cos(EL) * D,
      target.y + Math.sin(EL) * D,
      target.z + Math.cos(AZ) * Math.cos(EL) * D,
    );
    camera.lookAt(target);
    camera.updateProjectionMatrix();
  };
  const resize = (): void => {
    const r = root.getBoundingClientRect();
    W = Math.max(1, r.width);
    H = Math.max(1, r.height);
    renderer.setSize(W, H, false);
    renderer.domElement.style.width = `${W}px`;
    renderer.domElement.style.height = `${H}px`;
    wiresSvg.setAttribute("viewBox", `0 0 ${W} ${H}`);
    updateCamera();
  };

  const placeLabels = (): void => {
    const pxPerUnit = H / (camera.top - camera.bottom);
    const cardScale = Math.min(1, Math.max(0.6, (16 * pxPerUnit) / 214));
    for (const l of labelList) {
      l.obj.getWorldPosition(l.world);
      v3.copy(l.world).add(l.offset).project(camera);
      const x = (v3.x + 1) * 0.5 * W;
      const y = (1 - v3.y) * 0.5 * H;
      const off = x < -260 || x > W + 260 || y < -260 || y > H + 260;
      l.el.style.display = off ? "none" : "";
      if (off) continue;
      const tx = l.side?.tx ?? -50;
      const ty = l.side?.ty ?? -50;
      const sc = l.side ? cardScale : 1;
      l.el.style.transformOrigin = `${-tx}% ${-ty}%`;
      l.el.style.transform = `translate(${x.toFixed(1)}px, ${y.toFixed(1)}px) translate(${tx}%, ${ty}%) scale(${sc.toFixed(3)})`;
    }
  };

  const drawWires = (dt: number, motion: boolean): void => {
    const box = root.getBoundingClientRect();
    for (const w of wires) {
      const tile = tileEls.get(w.tool.id);
      const r = tile?.getBoundingClientRect();
      if (!r || !r.width) {
        w.el.setAttribute("d", "");
        w.curve = null;
        continue;
      }
      const x1 = r.left + r.width / 2 - box.left;
      const y1 = r.bottom - box.top + 2;
      w.desk.group.getWorldPosition(v3);
      v3.y += 2.3;
      v3.project(camera);
      const x2 = (v3.x + 1) * 0.5 * W;
      const y2 = (1 - v3.y) * 0.5 * H;
      const dy = Math.max(60, (y2 - y1) * 0.55);
      const c = [x1, y1, x1, y1 + dy, x2, y2 - dy * 0.6, x2, y2];
      w.curve = c;
      w.el.setAttribute(
        "d",
        `M${c[0]?.toFixed(1)},${c[1]?.toFixed(1)} C${c[2]?.toFixed(1)},${c[3]?.toFixed(1)} ${c[4]?.toFixed(1)},${c[5]?.toFixed(1)} ${c[6]?.toFixed(1)},${c[7]?.toFixed(1)}`,
      );
      if (w.flash > 0) w.flash = Math.max(0, w.flash - dt * 0.7);
      w.el.setAttribute("stroke-opacity", String(0.22 + w.flash * 0.6));
      w.el.setAttribute("stroke-width", String(1 + w.flash * 1.2));
      const goal = motion ? 11 + w.flash * 40 : 0;
      w.speed += (goal - w.speed) * Math.min(1, dt * 5);
      w.flow = (w.flow + dt * w.speed) % 7;
      w.el.setAttribute("stroke-dashoffset", (-w.flow).toFixed(2));
    }
    for (let i = dots.length - 1; i >= 0; i--) {
      const d = dots[i] as Dot;
      d.t += dt * 0.9;
      if (d.t > 1) {
        d.el.remove();
        dots.splice(i, 1);
        continue;
      }
      const c = d.wire.curve;
      if (!c || d.t < 0) {
        d.el.setAttribute("opacity", "0");
        continue;
      }
      const t = d.t;
      const u = 1 - t;
      const at = (a: number, b: number, cc: number, dd: number): number =>
        u * u * u * a + 3 * u * u * t * b + 3 * u * t * t * cc + t * t * t * dd;
      d.el.setAttribute(
        "cx",
        at(c[0] ?? 0, c[2] ?? 0, c[4] ?? 0, c[6] ?? 0).toFixed(1),
      );
      d.el.setAttribute(
        "cy",
        at(c[1] ?? 0, c[3] ?? 0, c[5] ?? 0, c[7] ?? 0).toFixed(1),
      );
      d.el.setAttribute("opacity", "0.95");
    }
  };

  // ── the made-up work ──
  let clock = 0;
  let feedId = 0;
  const pending: Pending[] = [];
  const feed = (who: string, what: string): void => {
    opts.onFeed?.({ id: ++feedId, who, what });
  };
  const later = (secs: number, run: () => void): void => {
    pending.push({ at: clock + secs, run });
  };

  const setStatus = (desk: Desk, st: Status): void => {
    desk.status = st;
    desk.orb.material.color.set(
      st === "needs" ? 0xff5a5a : st === "working" ? 0xc4ff3d : 0x6b7066,
    );
    desk.orbGlow.material.color.set(st === "needs" ? 0xff5a5a : 0xc4ff3d);
    desk.screenGlow.material.color.set(
      st === "working" ? 0xc4ff3d : st === "needs" ? 0xf0b04a : 0xdfe3d8,
    );
    desk.ring.material.color.set(st === "needs" ? 0xf0b04a : 0xc4ff3d);
    desk.ring.material.opacity = st === "idle" ? 0 : 0.9;
    desk.chip.dataset.status = st;
    if (st === "idle") desk.act = null;
    drawScreen(desk, clock);
    updateCards();
  };

  const updateCards = (): void => {
    for (const t of teams) {
      const working = t.desks.filter((d) => d.status === "working");
      const needs = t.desks.some((d) => d.status === "needs");
      t.card.classList.toggle("live", working.length > 0);
      t.card.classList.toggle("needs", needs);
      t.foot.textContent = needs
        ? "● waiting for you"
        : working.length
          ? `● ${working.map((d) => d.agent.name).join(", ")} working`
          : "quiet";
      t.foot.classList.toggle("live", needs || working.length > 0);
    }
  };

  const callTool = (desk: Desk, tool: ToolDef): void => {
    const tile = tileEls.get(tool.id);
    if (tile) {
      tile.classList.remove("flash");
      void tile.offsetWidth;
      tile.classList.add("flash");
    }
    for (const w of wires) {
      if (w.desk !== desk || w.tool !== tool) continue;
      w.flash = 1;
      for (let i = 0; i < 3; i++) {
        const c = document.createElementNS(SVGNS, "circle");
        c.setAttribute("r", "3");
        c.setAttribute("fill", tool.color);
        c.setAttribute("opacity", "0");
        dotLayer.append(c);
        dots.push({ wire: w, t: -i * 0.12, el: c });
      }
    }
  };

  const startWork = (desk: Desk, withTool: boolean, quiet = false): void => {
    const toolId = withTool ? pick(desk.agent.tools) : null;
    const tool = toolId ? (TOOL.get(toolId) ?? null) : null;
    if (tool) {
      const [action, detail] = pick(tool.acts);
      desk.act = { tool, action, detail, since: clock };
      callTool(desk, tool);
      if (!quiet) feed(desk.agent.name, tool.label);
    } else {
      desk.act = { tool: null, action: "Thinking", detail: "", since: clock };
    }
    desk.until = clock + 3.5 + Math.random() * 4.5;
    setStatus(desk, "working");
  };

  const pulseHandoff = (team: Team, from: Desk, to: Desk): void => {
    const path = polyline([
      from.pos.clone().setY(1.9),
      new THREE.Vector3(0, 1.9, 0),
      to.pos.clone().setY(1.9),
    ]);
    for (let k = 0; k < 6; k++) {
      spawn({
        parent: team.pod,
        path,
        t: -k * 0.07,
        speed: 0.8,
        loop: false,
        color: new THREE.Color(0xfff2b0),
        size: 0.8,
      });
    }
    team.flash = 1;
  };

  const pulseBrief = (team: Team): void => {
    for (let k = 0; k < 5; k++) {
      spawn({
        parent: world,
        path: (t) => linkPoint(team, t),
        t: -k * 0.08,
        speed: 0.7,
        loop: false,
        color: new THREE.Color(0xfff2b0),
        size: 0.9,
      });
    }
    team.flash = Math.max(team.flash, 0.5);
    brain.pulse = 1;
  };

  const idle = (): Desk[] => desks.filter((d) => d.status === "idle");

  const event = (): void => {
    const free = idle();
    const busy = desks.length - free.length;
    const r = Math.random();
    if (!free.length) return;
    if (r < 0.5 && busy < 6) {
      startWork(pick(free), Math.random() < 0.8);
    } else if (r < 0.74) {
      const team = pick(teams);
      const lead = team.desks.find((d) => d.lead);
      const to = team.desks.filter((d) => !d.lead && d.status === "idle");
      if (!lead || !to.length) return;
      const target = pick(to);
      pulseHandoff(team, lead, target);
      feed(lead.agent.name, `handed to ${target.agent.name}`);
      later(0.9, () => target.status === "idle" && startWork(target, true));
    } else if (r < 0.9) {
      const team = pick(teams);
      pulseBrief(team);
      feed("The Brain", `brief for ${team.def.name}`);
      const lead = team.desks.find((d) => d.lead);
      if (lead)
        later(1.4, () => lead.status === "idle" && startWork(lead, false));
    } else if (!desks.some((d) => d.status === "needs")) {
      const desk = pick(free);
      desk.until = clock + 4 + Math.random() * 2;
      setStatus(desk, "needs");
      feed(
        desk.agent.name,
        desk.agent.id === "xwriter" ? "draft waiting in Outbox" : "needs you",
      );
    }
  };

  // a busy first frame: some agents already at work, one waiting on you
  for (const id of ["modeler", "texture", "editor", "xwriter", "historian"]) {
    const desk = desks.find((d) => d.agent.id === id);
    if (desk) startWork(desk, true, true);
  }
  {
    const tech = desks.find((d) => d.agent.id === "techart");
    if (tech) {
      tech.until = 3;
      setStatus(tech, "needs");
    }
  }
  // as if they started a while ago
  for (const d of desks) if (d.act) d.act.since = -Math.random() * 40;

  // ── the loop ──
  let raf = 0;
  let last = performance.now();
  let running = false;
  let nextEvent = 1.2;
  let visible = true;
  let ready = false;

  const frame = (dt: number, motion: boolean): void => {
    clock += dt;
    const t = clock;
    const busy = desks.filter((d) => d.status !== "idle").length;

    if (motion) {
      nextEvent -= dt;
      if (nextEvent <= 0) {
        event();
        nextEvent = 0.8 + Math.random() * 1.2;
      }
      for (let i = pending.length - 1; i >= 0; i--) {
        const p = pending[i] as Pending;
        if (p.at <= clock) {
          pending.splice(i, 1);
          p.run();
        }
      }
      for (const d of desks) {
        if (d.status !== "idle" && clock >= d.until) setStatus(d, "idle");
      }
    }

    const b = brain;
    if (b.pulse > 0) b.pulse = Math.max(0, b.pulse - dt * 0.8);
    b.cloud.rotation.y += dt * (0.15 + Math.min(busy, 4) * 0.25);
    b.ico.rotation.y -= dt * 0.1;
    b.ico.rotation.x += dt * 0.05;
    const pulse = 0.5 + 0.5 * Math.sin(t * (busy ? 4 : 1.4));
    b.halo.material.opacity =
      0.45 + pulse * (busy ? 0.45 : 0.15) + b.pulse * 0.3;
    b.halo.scale.setScalar(10 + pulse * (busy ? 3 : 1) + b.pulse * 3);
    (b.beam.material as THREE.MeshBasicMaterial).opacity =
      0.1 + (busy ? 0.12 + pulse * 0.12 : 0.04);
    b.star.scale.setScalar(5 + pulse * (busy ? 3 : 0.8));
    b.light.intensity = 25 + pulse * (busy ? 40 : 8);

    for (const desk of desks) {
      if (desk.status === "working") {
        desk.person.position.y = Math.abs(Math.sin(t * 6 + desk.seed)) * 0.06;
        desk.orb.scale.setScalar(1 + 0.35 * Math.sin(t * 5));
        desk.orbGlow.material.opacity = 0.5 + 0.3 * Math.sin(t * 5);
        desk.ring.scale.setScalar(1 + 0.08 * Math.sin(t * 5));
        if (t - desk.lastDraw > 0.35) {
          desk.lastDraw = t;
          drawScreen(desk, t);
        }
      } else if (desk.status === "needs") {
        const on = Math.sin(t * 8) > 0;
        if (t - desk.lastDraw > 0.5) {
          desk.lastDraw = t;
          drawScreen(desk, t);
        }
        desk.orb.scale.setScalar(on ? 1.5 : 1);
        desk.orbGlow.material.opacity = on ? 0.9 : 0.2;
        desk.person.position.y = 0;
      } else {
        desk.orb.scale.setScalar(1);
        desk.orbGlow.material.opacity = 0;
        desk.person.position.y = 0;
      }
    }

    for (const team of teams) {
      const live = team.desks.some((d) => d.status !== "idle");
      if (team.flash > 0) team.flash = Math.max(0, team.flash - dt * 0.9);
      for (const r of team.rails) {
        r.material.opacity = live
          ? Math.min(1, r.base + 0.35 + 0.2 * Math.sin(t * 6))
          : r.base + team.flash * 0.4;
      }
      team.hub.holo.rotation.z += dt * (live ? 1.6 : 0.35);
      team.hub.holo2.rotation.x += dt * (live ? 2.2 : 0.5);
      team.hub.hubGlow.material.opacity =
        0.35 + (live ? 0.25 + 0.15 * Math.sin(t * 5) : 0) + team.flash * 0.5;
      team.hub.hubGlow.scale.setScalar(
        4.5 + team.flash * 3 + (live ? Math.sin(t * 5) * 0.4 : 0),
      );
    }

    for (let i = particles.length - 1; i >= 0; i--) {
      const p = particles[i] as Particle;
      p.t += dt * p.speed;
      if (p.t > 1) {
        if (p.loop) p.t -= 1;
        else {
          p.parent.remove(p.sprite);
          p.sprite.material.dispose();
          particles.splice(i, 1);
          continue;
        }
      }
      p.sprite.visible = p.t >= 0;
      if (p.t >= 0) p.sprite.position.copy(p.path(p.t));
    }

    placeLabels();
    drawWires(dt, motion);
    renderer.render(scene, camera);
    if (!ready) {
      ready = true;
      opts.onReady?.();
    }
  };

  const loop = (now: number): void => {
    if (!running) return;
    const dt = Math.min(0.05, (now - last) / 1000);
    last = now;
    frame(dt, true);
    raf = requestAnimationFrame(loop);
  };
  const play = (): void => {
    if (running || opts.reduced || !visible || document.hidden) return;
    running = true;
    last = performance.now();
    raf = requestAnimationFrame(loop);
  };
  const pause = (): void => {
    running = false;
    cancelAnimationFrame(raf);
  };

  const ro = new ResizeObserver(() => {
    resize();
    if (!running) frame(0, false);
  });
  ro.observe(root);
  resize();

  // only run while the hero is on screen and the tab is visible
  const io = new IntersectionObserver((entries) => {
    visible = entries.some((e) => e.isIntersecting);
    if (visible) play();
    else pause();
  });
  io.observe(root);
  const onVis = (): void => {
    if (document.hidden) pause();
    else play();
  };
  document.addEventListener("visibilitychange", onVis);

  // let the canvas fonts settle before the first frame, so monitors aren't drawn in a fallback face
  const first = (): void => {
    for (const d of desks) drawScreen(d, clock);
    frame(0.016, false);
    play();
  };
  // …and the picture logos, so the still frame (reduced motion) has them too
  const fontsReady = document.fonts?.ready ?? Promise.resolve();
  Promise.all([fontsReady.catch(() => undefined), logosReady()]).then(
    first,
    first,
  );

  return () => {
    pause();
    ro.disconnect();
    io.disconnect();
    document.removeEventListener("visibilitychange", onVis);
    scene.traverse((o) => {
      const m = o as THREE.Mesh;
      m.geometry?.dispose();
      const mats = Array.isArray(m.material)
        ? m.material
        : m.material
          ? [m.material]
          : [];
      for (const mat of mats) mat.dispose();
    });
    for (const t of textures) t.dispose();
    renderer.dispose();
    root.remove();
  };
}
