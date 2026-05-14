import type { ReactNode } from "react";

export function DisplayHeading({ children }: { children: ReactNode }) {
  return (
    <h2
      className="font-display leading-none tracking-[-0.01em] text-ink mb-8 max-w-[900px]"
      style={{ fontSize: "clamp(36px, 5vw, 60px)" }}
    >
      {children}
    </h2>
  );
}
