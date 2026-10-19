import type { CSSProperties } from "react";

/**
 * A cast-concrete block held in the field: a true CSS 3D cuboid, lit from
 * above, swaying a few degrees and bobbing over its own soft floor shadow.
 * Decorative; always aria-hidden.
 */
export function Monolith({
  w,
  h,
  d,
  rx = -10,
  ry = 32,
  rz = 0,
  sway = 7,
  period = 14,
  delay = 0,
  shadow = true,
  seam = false,
  className = "",
  style,
}: {
  w: number;
  h: number;
  d: number;
  rx?: number;
  ry?: number;
  rz?: number;
  sway?: number;
  period?: number;
  delay?: number;
  shadow?: boolean;
  seam?: boolean;
  className?: string;
  style?: CSSProperties;
}) {
  const face = (
    fw: number,
    fh: number,
    transform: string,
    tone: string,
    light: string,
  ): CSSProperties => ({
    position: "absolute",
    width: fw,
    height: fh,
    left: (w - fw) / 2,
    top: (h - fh) / 2,
    transform,
    backgroundColor: tone,
    backgroundImage: `${light}, var(--tex-pits), var(--tex-grain), var(--tex-stain)`,
    backgroundSize: "100% 100%, 300px 300px, 260px 260px, 900px 900px",
    backgroundBlendMode: "normal, normal, overlay, soft-light",
    backfaceVisibility: "hidden",
  });

  const vars = {
    "--m-rx": `${rx}deg`,
    "--m-ry": `${ry}deg`,
    "--m-rz": `${rz}deg`,
    "--m-sway": `${sway}deg`,
    "--m-period": `${period}s`,
    "--m-delay": `${delay}s`,
  } as CSSProperties;

  return (
    <div
      aria-hidden
      className={`pointer-events-none ${className}`}
      style={{ width: w, height: h, perspective: 1600, ...vars, ...style }}
    >
      <div className="monolith-bob" style={{ width: w, height: h }}>
        <div
          className="monolith-sway"
          style={{
            width: w,
            height: h,
            position: "relative",
            transformStyle: "preserve-3d",
          }}
        >
          {/* front */}
          <div
            style={face(
              w,
              h,
              `translateZ(${d / 2}px)`,
              "#34352f",
              "linear-gradient(180deg, rgba(255,255,255,0.07), rgba(0,0,0,0.35))",
            )}
          />
          {/* back */}
          <div
            style={face(
              w,
              h,
              `rotateY(180deg) translateZ(${d / 2}px)`,
              "#24251f",
              "linear-gradient(180deg, rgba(255,255,255,0.03), rgba(0,0,0,0.4))",
            )}
          />
          {/* left (lit) */}
          <div
            style={face(
              d,
              h,
              `rotateY(-90deg) translateZ(${w / 2}px)`,
              "#45463f",
              "linear-gradient(180deg, rgba(255,255,255,0.1), rgba(0,0,0,0.3))",
            )}
          />
          {/* right (shade) */}
          <div
            style={face(
              d,
              h,
              `rotateY(90deg) translateZ(${w / 2}px)`,
              "#1e1f1b",
              "linear-gradient(180deg, rgba(255,255,255,0.02), rgba(0,0,0,0.45))",
            )}
          />
          {/* top (catches the shaft) */}
          <div
            style={face(
              w,
              d,
              `rotateX(90deg) translateZ(${h / 2}px)`,
              "#6b6d65",
              "linear-gradient(90deg, rgba(238,241,232,0.12), rgba(0,0,0,0.12))",
            )}
          />
          {/* bottom */}
          <div
            style={face(
              w,
              d,
              `rotateX(-90deg) translateZ(${h / 2}px)`,
              "#0f100e",
              "linear-gradient(0deg, rgba(0,0,0,0.2), rgba(0,0,0,0.2))",
            )}
          />
        </div>
      </div>
      {shadow ? (
        <div
          className="monolith-shadow"
          style={{
            width: Math.max(w, d) * 1.3,
            marginLeft: (w - Math.max(w, d) * 1.3) / 2,
            marginTop: Math.max(40, h * 0.18),
          }}
        />
      ) : null}
      {seam ? (
        <div
          className="h-px bg-accent mx-auto opacity-80"
          style={{
            width: w * 0.9,
            marginTop: shadow ? 6 : Math.max(36, h * 0.16),
            boxShadow: "0 8px 18px 1px rgba(196,255,61,0.22)",
          }}
        />
      ) : null}
    </div>
  );
}
