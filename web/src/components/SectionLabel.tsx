import type { ReactNode } from "react";

export function SectionLabel({ children }: { children: ReactNode }) {
  return (
    <div className="sec-label text-[10px] tracking-[0.2em] text-accent uppercase mb-3 flex items-center">
      {children}
    </div>
  );
}
