// The 404 page's background: a mesh of nodes, and a packet that keeps
// trying to reach a node that is not there. Made up, no network involved.
//
// startRoute(host, …) draws into `host` and returns a cleanup function.

import * as THREE from "three";

export interface TraceEvent {
  id: number;
  kind: "hop" | "fail" | "retry";
  text: string;
}

interface Options {
  reduced: boolean;
  /** The path the visitor asked for, shown in the trace when the route fails. */
  path: string;
  onTrace?: (e: TraceEvent) => void;
  onReady?: () => void;
}

const ACCENT = new THREE.Color("#c4ff3d");
// the warning colour of the field pages (--color-warn in world-field.css)
const WARN = new THREE.Color("#f0b04a");
const EDGE = new THREE.Color("#2c3528");
const EDGE_LIT = new THREE.Color("#7a9a2c");
const NODE = new THREE.Color("#8d9488");

const NODE_COUNT = 46;
const SPEED = 15; // units per second along an edge
const FAIL_AT = 0.7; // how far into the gap the packet gets before bouncing

// Same layout on every visit.
function seeded(seed: number): () => number {
  let a = seed;
  return () => {
    a |= 0;
    a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

const hex = (n: number): string =>
  `0x${n.toString(16).toUpperCase().padStart(2, "0")}`;

// A soft round dot, drawn once and shared by every sprite.
function dotTexture(): THREE.CanvasTexture {
  const c = document.createElement("canvas");
  c.width = c.height = 64;
  const ctx = c.getContext("2d");
  if (ctx) {
    const g = ctx.createRadialGradient(32, 32, 0, 32, 32, 32);
    g.addColorStop(0, "rgba(255,255,255,1)");
    g.addColorStop(0.25, "rgba(255,255,255,0.85)");
    g.addColorStop(0.6, "rgba(255,255,255,0.18)");
    g.addColorStop(1, "rgba(255,255,255,0)");
    ctx.fillStyle = g;
    ctx.fillRect(0, 0, 64, 64);
  }
  const t = new THREE.CanvasTexture(c);
  t.colorSpace = THREE.SRGBColorSpace;
  return t;
}

export function startRoute(host: HTMLElement, opts: Options): () => void {
  const rand = seeded(2070);

  // ── the graph ──────────────────────────────────────────────────────────
  const pos: THREE.Vector3[] = [];
  let guard = 0;
  while (pos.length < NODE_COUNT && guard++ < 5000) {
    const p = new THREE.Vector3(
      (rand() * 2 - 1) * 27,
      (rand() * 2 - 1) * 14,
      (rand() * 2 - 1) * 12,
    );
    if ((p.x / 27) ** 2 + (p.y / 14) ** 2 + (p.z / 12) ** 2 > 1) continue;
    if (pos.every((q) => q.distanceTo(p) > 5.2)) pos.push(p);
  }
  const n = pos.length;

  // The node that should hold the page: the one nearest a spot right of
  // centre, so the gap sits in view.
  const want = new THREE.Vector3(7, 2, 5);
  let gap = 0;
  for (let i = 1; i < n; i++) {
    if (pos[i].distanceTo(want) < pos[gap].distanceTo(want)) gap = i;
  }

  // Each node links to its three nearest neighbours.
  const adj: Set<number>[] = pos.map(() => new Set<number>());
  for (let i = 0; i < n; i++) {
    const near = pos
      .map((p, j) => ({ j, d: p.distanceTo(pos[i]) }))
      .filter((x) => x.j !== i)
      .sort((a, b) => a.d - b.d)
      .slice(0, 3);
    for (const { j } of near) {
      adj[i].add(j);
      adj[j].add(i);
    }
  }
  const gapNeighbours = [...adj[gap]];

  // Shortest path that never passes through the gap.
  const route = (from: number, to: number): number[] | null => {
    const prev = new Map<number, number>([[from, -1]]);
    const queue = [from];
    while (queue.length) {
      const cur = queue.shift() as number;
      if (cur === to) break;
      for (const nb of adj[cur]) {
        if (nb === gap || prev.has(nb)) continue;
        prev.set(nb, cur);
        queue.push(nb);
      }
    }
    if (!prev.has(to)) return null;
    const out: number[] = [];
    for (let c = to; c !== -1; c = prev.get(c) as number) out.unshift(c);
    return out;
  };

  // ── three.js setup ─────────────────────────────────────────────────────
  const renderer = new THREE.WebGLRenderer({ antialias: true, alpha: true });
  renderer.setPixelRatio(Math.min(window.devicePixelRatio || 1, 1.5));
  renderer.setClearColor(0x000000, 0);
  renderer.domElement.style.display = "block";
  renderer.domElement.style.width = "100%";
  renderer.domElement.style.height = "100%";
  host.appendChild(renderer.domElement);

  const scene = new THREE.Scene();
  const camera = new THREE.PerspectiveCamera(38, 1, 1, 400);
  const world = new THREE.Group();
  scene.add(world);

  const tex = dotTexture();
  const disposables: { dispose: () => void }[] = [tex];
  const track = <T extends { dispose: () => void }>(x: T): T => {
    disposables.push(x);
    return x;
  };

  // faint dust for depth
  {
    const dust = new Float32Array(260 * 3);
    for (let i = 0; i < dust.length; i += 3) {
      dust[i] = (rand() * 2 - 1) * 70;
      dust[i + 1] = (rand() * 2 - 1) * 40;
      dust[i + 2] = (rand() * 2 - 1) * 40 - 10;
    }
    const g = track(new THREE.BufferGeometry());
    g.setAttribute("position", new THREE.BufferAttribute(dust, 3));
    const m = track(
      new THREE.PointsMaterial({
        size: 0.35,
        color: 0x4a5046,
        transparent: true,
        opacity: 0.7,
        depthWrite: false,
      }),
    );
    world.add(new THREE.Points(g, m));
  }

  // live edges (every edge that does not touch the gap)
  const live: [number, number][] = [];
  const dead: [number, number][] = [];
  for (let i = 0; i < n; i++) {
    for (const j of adj[i]) {
      if (j <= i) continue;
      (i === gap || j === gap ? dead : live).push([i, j]);
    }
  }
  const edgeIndex = new Map<string, number>();
  live.forEach(([a, b], k) => {
    edgeIndex.set(`${a}-${b}`, k);
    edgeIndex.set(`${b}-${a}`, k);
  });
  const edgeGeo = track(new THREE.BufferGeometry());
  const edgePos = new Float32Array(live.length * 6);
  const edgeCol = new Float32Array(live.length * 6);
  live.forEach(([a, b], k) => {
    pos[a].toArray(edgePos, k * 6);
    pos[b].toArray(edgePos, k * 6 + 3);
  });
  edgeGeo.setAttribute("position", new THREE.BufferAttribute(edgePos, 3));
  edgeGeo.setAttribute("color", new THREE.BufferAttribute(edgeCol, 3));
  const edgeMat = track(
    new THREE.LineBasicMaterial({
      vertexColors: true,
      transparent: true,
      opacity: 0.9,
    }),
  );
  world.add(new THREE.LineSegments(edgeGeo, edgeMat));
  // how lit each edge is (0 = plain, 1 = on the packet's path), eased each frame
  const edgeHeat = new Float32Array(live.length);

  // edges into the gap: dashed, in the warning colour
  const deadGeo = track(new THREE.BufferGeometry());
  const deadPos = new Float32Array(dead.length * 6);
  dead.forEach(([a, b], k) => {
    pos[a].toArray(deadPos, k * 6);
    pos[b].toArray(deadPos, k * 6 + 3);
  });
  deadGeo.setAttribute("position", new THREE.BufferAttribute(deadPos, 3));
  const deadMat = track(
    new THREE.LineDashedMaterial({
      color: WARN,
      dashSize: 0.7,
      gapSize: 0.55,
      transparent: true,
      opacity: 0.35,
    }),
  );
  const deadLines = new THREE.LineSegments(deadGeo, deadMat);
  deadLines.computeLineDistances();
  world.add(deadLines);

  // nodes
  const nodeMats: THREE.SpriteMaterial[] = [];
  const nodeSprites: THREE.Sprite[] = [];
  const nodeFlash = new Float32Array(n);
  for (let i = 0; i < n; i++) {
    if (i === gap) {
      nodeSprites.push(new THREE.Sprite());
      nodeMats.push(new THREE.SpriteMaterial());
      continue;
    }
    const m = track(
      new THREE.SpriteMaterial({
        map: tex,
        color: NODE.clone(),
        transparent: true,
        depthWrite: false,
        blending: THREE.AdditiveBlending,
      }),
    );
    const s = new THREE.Sprite(m);
    s.position.copy(pos[i]);
    s.scale.setScalar(1.5);
    world.add(s);
    nodeMats.push(m);
    nodeSprites.push(s);
  }

  // the gap: an empty socket where the node should be
  const socket = new THREE.Group();
  socket.position.copy(pos[gap]);
  world.add(socket);
  const ringMat = track(
    new THREE.MeshBasicMaterial({
      color: WARN,
      transparent: true,
      opacity: 0.8,
      side: THREE.DoubleSide,
      depthWrite: false,
    }),
  );
  const ring = new THREE.Mesh(
    track(new THREE.RingGeometry(1.55, 1.75, 48)),
    ringMat,
  );
  socket.add(ring);
  const outerMat = track(
    new THREE.MeshBasicMaterial({
      color: WARN,
      transparent: true,
      opacity: 0.25,
      side: THREE.DoubleSide,
      depthWrite: false,
    }),
  );
  const outer = new THREE.Mesh(
    track(new THREE.RingGeometry(2.5, 2.58, 48)),
    outerMat,
  );
  socket.add(outer);
  const coreMat = track(
    new THREE.MeshBasicMaterial({
      color: 0x070907,
      transparent: true,
      opacity: 0.92,
      side: THREE.DoubleSide,
    }),
  );
  socket.add(new THREE.Mesh(track(new THREE.CircleGeometry(1.5, 40)), coreMat));

  // the packet and its trail
  const packetMat = track(
    new THREE.SpriteMaterial({
      map: tex,
      color: ACCENT.clone(),
      transparent: true,
      depthWrite: false,
      blending: THREE.AdditiveBlending,
    }),
  );
  const packet = new THREE.Sprite(packetMat);
  packet.scale.setScalar(2.6);
  world.add(packet);
  const TRAIL = 9;
  const trail: THREE.Sprite[] = [];
  const trailMats: THREE.SpriteMaterial[] = [];
  for (let i = 0; i < TRAIL; i++) {
    const m = track(
      new THREE.SpriteMaterial({
        map: tex,
        color: ACCENT.clone(),
        transparent: true,
        opacity: 0.5 * (1 - i / TRAIL),
        depthWrite: false,
        blending: THREE.AdditiveBlending,
      }),
    );
    const s = new THREE.Sprite(m);
    s.scale.setScalar(1.6 * (1 - i / TRAIL) + 0.3);
    world.add(s);
    trail.push(s);
    trailMats.push(m);
  }
  const history: THREE.Vector3[] = [];

  // ── the packet's journey ───────────────────────────────────────────────
  let traceId = 0;
  const trace = (kind: TraceEvent["kind"], text: string): void => {
    opts.onTrace?.({ id: ++traceId, kind, text });
  };

  type Phase = "travel" | "probe" | "bounce" | "wait" | "fade";
  let phase: Phase = "travel";
  let path: number[] = [];
  let leg = 0; // index in path of the node we are leaving
  let along = 0; // 0..1 along the current leg
  let waitFor = 0;
  let tries = 0;
  let fail = 0; // 1 right after a failed attempt, eases to 0
  let fade = 1;
  const at = new THREE.Vector3();

  const farStart = (): number => {
    const options = pos
      .map((p, i) => ({ i, d: p.distanceTo(pos[gap]) }))
      .filter((x) => x.i !== gap && !adj[gap].has(x.i) && x.d > 16);
    return options[Math.floor(rand() * options.length)]?.i ?? 0;
  };

  const planFrom = (start: number): boolean => {
    const targets = [...gapNeighbours].sort(() => rand() - 0.5);
    for (const t of targets) {
      const r = route(start, t);
      if (r) {
        path = r;
        leg = 0;
        along = 0;
        return true;
      }
    }
    return false;
  };

  const newRun = (): void => {
    tries = 0;
    const s = farStart();
    planFrom(s);
    fade = 0;
    phase = "travel";
    history.length = 0;
    trace("retry", `new packet from ${hex(s)}`);
  };

  if (!opts.reduced) newRun();

  const edgeTarget = new Float32Array(live.length);
  const setEdgeTargets = (): Float32Array => {
    const target = edgeTarget.fill(0);
    for (let i = 0; i < Math.min(leg + 1, path.length - 1); i++) {
      const k = edgeIndex.get(`${path[i]}-${path[i + 1]}`);
      if (k !== undefined) target[k] = 1;
    }
    return target;
  };

  const step = (dt: number): void => {
    const last = path[path.length - 1];
    if (phase === "travel") {
      if (path.length < 2) {
        phase = "probe";
        along = 0;
      } else {
        const a = pos[path[leg]];
        const b = pos[path[leg + 1]];
        along += (dt * SPEED) / Math.max(a.distanceTo(b), 0.01);
        if (along >= 1) {
          leg++;
          along = 0;
          nodeFlash[path[leg]] = 1;
          if (leg >= path.length - 1) {
            phase = "probe";
            trace("hop", `at ${hex(path[leg])} · next hop ${hex(gap)}`);
          } else trace("hop", `hop ${leg} · ${hex(path[leg])}`);
        }
      }
      if (phase === "travel") {
        at.lerpVectors(pos[path[leg]], pos[path[leg + 1]], along);
      } else at.copy(pos[last]);
    } else if (phase === "probe") {
      along += (dt * SPEED * 0.55) / pos[last].distanceTo(pos[gap]);
      if (along >= FAIL_AT) {
        along = FAIL_AT;
        phase = "bounce";
        fail = 1;
        tries++;
        trace("fail", `no route to ${opts.path}`);
      }
      at.lerpVectors(pos[last], pos[gap], along);
    } else if (phase === "bounce") {
      along -= (dt * SPEED * 0.8) / pos[last].distanceTo(pos[gap]);
      if (along <= 0) {
        along = 0;
        phase = "wait";
        waitFor = 0.9;
      }
      at.lerpVectors(pos[last], pos[gap], along);
    } else if (phase === "wait") {
      at.copy(pos[last]);
      waitFor -= dt;
      if (waitFor <= 0) {
        // try again from another neighbour of the gap, if one is reachable
        const others = gapNeighbours
          .filter((g) => g !== last)
          .sort(() => rand() - 0.5);
        let r: number[] | null = null;
        for (const o of others) {
          r = route(last, o);
          if (r && r.length > 1) break;
        }
        if (tries >= 2 || !r || r.length < 2) {
          phase = "fade";
          trace("fail", "route lost");
        } else {
          path = r;
          leg = 0;
          along = 0;
          phase = "travel";
          trace("retry", `rerouting via ${hex(r[r.length - 1])}`);
        }
      }
    } else if (phase === "fade") {
      at.copy(pos[last]);
      fade -= dt * 1.6;
      if (fade <= 0) newRun();
    }
    if (phase !== "fade") fade = Math.min(1, fade + dt * 2);
  };

  // ── drawing ────────────────────────────────────────────────────────────
  let W = 1;
  let H = 1;
  const resize = (): void => {
    W = Math.max(1, host.clientWidth);
    H = Math.max(1, host.clientHeight);
    renderer.setSize(W, H, false);
    camera.aspect = W / H;
    const wide = W >= 1024;
    // on wide screens the mesh sits to the right of the text
    world.position.set(wide ? 15 : 0, wide ? 0 : 4, 0);
    const fit = wide ? 1 : Math.max(1, 1.25 / camera.aspect);
    camera.position.set(0, 3, 66 * Math.min(fit, 2.2));
    camera.lookAt(0, 0, 0);
    camera.updateProjectionMatrix();
  };
  resize();

  const pointer = { x: 0, y: 0, tx: 0, ty: 0 };
  const onPointer = (e: PointerEvent): void => {
    pointer.tx = (e.clientX / window.innerWidth) * 2 - 1;
    pointer.ty = (e.clientY / window.innerHeight) * 2 - 1;
  };
  window.addEventListener("pointermove", onPointer, { passive: true });

  const tmpColor = new THREE.Color();
  const inverseWorld = new THREE.Quaternion();
  let clock = 0;

  const draw = (dt: number): void => {
    clock += dt;
    pointer.x += (pointer.tx - pointer.x) * Math.min(1, dt * 3);
    pointer.y += (pointer.ty - pointer.y) * Math.min(1, dt * 3);
    world.rotation.y = Math.sin(clock * 0.07) * 0.35 + pointer.x * 0.22;
    world.rotation.x = 0.08 + pointer.y * 0.1;

    // edges warm up along the path and cool down behind it
    const target = setEdgeTargets();
    for (let k = 0; k < live.length; k++) {
      edgeHeat[k] += (target[k] * fade - edgeHeat[k]) * Math.min(1, dt * 4);
      tmpColor.copy(EDGE).lerp(EDGE_LIT, edgeHeat[k]);
      tmpColor.toArray(edgeCol, k * 6);
      tmpColor.toArray(edgeCol, k * 6 + 3);
    }
    (edgeGeo.getAttribute("color") as THREE.BufferAttribute).needsUpdate = true;

    fail = Math.max(0, fail - dt * 0.9);
    const flicker = fail > 0 ? 0.5 + 0.5 * Math.sin(clock * 48) : 0;
    deadMat.opacity = 0.3 + 0.6 * fail * flicker;
    ringMat.opacity = 0.65 + 0.35 * Math.sin(clock * 2.4) * 0.5 + fail * 0.3;
    outer.scale.setScalar(1 + ((clock * 0.6) % 1) * 0.9);
    outerMat.opacity = 0.3 * (1 - ((clock * 0.6) % 1)) + fail * 0.4;
    // the socket always faces the camera
    socket.quaternion
      .copy(camera.quaternion)
      .premultiply(inverseWorld.copy(world.quaternion).invert());

    for (let i = 0; i < n; i++) {
      if (i === gap) continue;
      nodeFlash[i] = Math.max(0, nodeFlash[i] - dt * 1.6);
      nodeMats[i].color.copy(NODE).lerp(ACCENT, nodeFlash[i]);
      nodeSprites[i].scale.setScalar(1.5 + nodeFlash[i] * 1.4);
    }

    const warm = phase === "bounce" || (phase === "wait" && fail > 0.3);
    packetMat.color.copy(warm ? WARN : ACCENT);
    packetMat.opacity = fade;
    packet.position.copy(at);
    const slot =
      history.length >= TRAIL * 2
        ? (history.pop() as THREE.Vector3)
        : new THREE.Vector3();
    history.unshift(slot.copy(at));
    for (let i = 0; i < TRAIL; i++) {
      const h = history[Math.min(history.length - 1, i * 2 + 1)] ?? at;
      trail[i].position.copy(h);
      trailMats[i].color.copy(warm ? WARN : ACCENT);
      trailMats[i].opacity = 0.5 * (1 - i / TRAIL) * fade;
    }

    renderer.render(scene, camera);
  };

  // ── loop, pausing when hidden ──────────────────────────────────────────
  let raf = 0;
  let last = 0;
  let onScreen = true;
  let running = false;
  let started = false; // set once the shaders are compiled

  let drawn = 0;
  const frame = (t: number): void => {
    const dt = Math.min(0.05, last ? (t - last) / 1000 : 0.016);
    last = t;
    step(dt);
    draw(dt);
    // fade in only once the first frames (and their hitches) are behind us
    if (++drawn === 8) opts.onReady?.();
    raf = requestAnimationFrame(frame);
  };
  const play = (): void => {
    if (running || opts.reduced || !started) return;
    if (!onScreen || document.hidden) return;
    running = true;
    last = 0;
    raf = requestAnimationFrame(frame);
  };
  const pause = (): void => {
    running = false;
    cancelAnimationFrame(raf);
  };

  const io = new IntersectionObserver((entries) => {
    onScreen = entries.some((e) => e.isIntersecting);
    if (onScreen) play();
    else pause();
  });
  io.observe(host);
  const onVis = (): void => (document.hidden ? pause() : play());
  document.addEventListener("visibilitychange", onVis);

  const ro = new ResizeObserver(() => {
    resize();
    if (started && !running) draw(0);
  });
  ro.observe(host);

  let stopped = false;
  const begin = (): void => {
    if (stopped) return;
    started = true;
    if (opts.reduced) still();
    else {
      step(0.016);
      play();
    }
  };
  // one still frame: the packet stopped at the gap
  const still = (): void => {
    phase = "probe";
    path = route(farStart(), gapNeighbours[0]) ?? [gapNeighbours[0]];
    leg = path.length - 1;
    along = FAIL_AT;
    fade = 1;
    fail = 0.6;
    at.lerpVectors(pos[path[path.length - 1]], pos[gap], FAIL_AT);
    phase = "bounce";
    edgeHeat.set(setEdgeTargets());
    draw(0.016);
    trace("fail", `no route to ${opts.path}`);
    opts.onReady?.();
  };
  // Compile the shaders off the main thread where the browser can, so the
  // first frame doesn't stall.
  renderer
    .compileAsync(scene, camera)
    .catch(() => undefined)
    .then(begin);

  return () => {
    stopped = true;
    pause();
    io.disconnect();
    ro.disconnect();
    document.removeEventListener("visibilitychange", onVis);
    window.removeEventListener("pointermove", onPointer);
    for (const d of disposables) d.dispose();
    renderer.dispose();
    renderer.domElement.remove();
  };
}
