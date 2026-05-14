import { useEffect, useRef } from "react";

const PACKET_RAIN_TOKENS: readonly string[] = [
  "node.0x2c",
  "node.0x7a",
  "node.0xff",
  "node.0x4e",
  "node.0x91",
  "→ relay.04",
  "→ relay.0a",
  "→ relay.1f",
  "→ gw.00",
  "pingwave",
  "mycelia",
  "engram",
  "mikoshi",
  "superpose",
  "0xc4ff3d",
  "0x4e4554",
  "0x7af301",
  "0xdeadbe",
  "0xfeedfa",
  "rt:hit",
  "rt:miss",
  "hb.ack",
  "hb.syn",
  "fwd.0",
  "fwd.1",
  "fwd.2",
  "cap.read",
  "cap.write",
  "cap.exec",
  "route()",
  "forward()",
  "38ns",
  "57ns",
  "113ns",
  "288ns",
  "0.93ns",
  "▸▸▸",
  "◀◀◀",
  "── ──",
  "·· ··",
  "01001110",
  "01000101",
  "01010100",
  "A→B",
  "B→G",
  "G→R1",
  "R2→B",
  "ACK",
  "SYN",
  "FIN",
  "NAK",
  "OK",
  "ERR",
];

interface RainColumn {
  x: number;
  y: number;
  speed: number;
  gap: number;
  len: number;
  tokens: string[];
  tick: number;
}

function pickToken(): string {
  const idx = Math.floor(Math.random() * PACKET_RAIN_TOKENS.length);
  return PACKET_RAIN_TOKENS[idx] ?? "";
}

export function PacketRain() {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;

    const dpr = Math.min(window.devicePixelRatio || 1, 2);
    const FONT_PX = 11;
    const COL_W = 90;
    let W = 0;
    let H = 0;
    let cols: RainColumn[] = [];
    let rafId = 0;

    const resize = (): void => {
      const rect = canvas.getBoundingClientRect();
      W = rect.width;
      H = rect.height;
      canvas.width = Math.max(1, Math.floor(W * dpr));
      canvas.height = Math.max(1, Math.floor(H * dpr));
      ctx.setTransform(1, 0, 0, 1, 0, 0);
      ctx.scale(dpr, dpr);
      ctx.font = `${FONT_PX}px "JetBrains Mono", ui-monospace, monospace`;
      ctx.textBaseline = "top";

      const colCount = Math.max(6, Math.ceil(W / COL_W));
      cols = Array.from({ length: colCount }, (_, i) => ({
        x: (i + 0.5) * (W / colCount) + (Math.random() - 0.5) * 20,
        y: -Math.random() * H,
        speed: 0.5 + Math.random() * 1.4,
        gap: 16 + Math.random() * 10,
        len: 8 + Math.floor(Math.random() * 14),
        tokens: Array.from({ length: 22 }, pickToken),
        tick: 0,
      }));
    };

    const frame = (): void => {
      ctx.fillStyle = "rgba(11, 13, 11, 0.18)";
      ctx.fillRect(0, 0, W, H);

      for (const c of cols) {
        c.y += c.speed;
        c.tick++;
        if (c.tick % 7 === 0) {
          c.tokens[Math.floor(Math.random() * c.tokens.length)] = pickToken();
        }

        for (let i = 0; i < c.len; i++) {
          const drawY = c.y - i * c.gap;
          if (drawY < -FONT_PX || drawY > H) continue;

          const headIdx = Math.floor(c.y / c.gap);
          const idx = headIdx - i;
          if (idx < 0) continue;
          const tok = c.tokens[idx % c.tokens.length] ?? "";

          if (i === 0) {
            ctx.fillStyle = "rgba(220, 255, 200, 0.95)";
          } else {
            const a = Math.max(0, 1 - i / c.len);
            ctx.fillStyle = `rgba(196, 255, 61, ${a * 0.55})`;
          }
          ctx.fillText(tok, c.x, drawY);
        }

        if (c.y - c.len * c.gap > H + 40) {
          c.y = -Math.random() * H * 0.5;
          c.speed = 0.5 + Math.random() * 1.4;
          c.gap = 16 + Math.random() * 10;
          c.len = 8 + Math.floor(Math.random() * 14);
          c.tokens = c.tokens.map(() => pickToken());
        }
      }
      rafId = requestAnimationFrame(frame);
    };

    const onResize = (): void => {
      ctx.setTransform(1, 0, 0, 1, 0, 0);
      resize();
      ctx.fillStyle = "#0b0d0b";
      ctx.fillRect(0, 0, W, H);
    };

    resize();
    ctx.fillStyle = "#0b0d0b";
    ctx.fillRect(0, 0, W, H);
    rafId = requestAnimationFrame(frame);
    window.addEventListener("resize", onResize);

    return () => {
      cancelAnimationFrame(rafId);
      window.removeEventListener("resize", onResize);
    };
  }, []);

  return (
    <canvas
      ref={canvasRef}
      className="packet-rain absolute inset-0 w-full h-full pointer-events-none"
      aria-hidden
    />
  );
}
