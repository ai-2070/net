import type { ReactNode } from "react";

/**
 * A sector marking stencilled down the section's right-hand pillar, at
 * every width, so it never sits above a heading. The parent section must
 * be `relative` and keep its right padding clear for it.
 */
export function FieldLabel({ children }: { children: ReactNode }) {
  return (
    <div className="stencil absolute right-3 top-24 lg:right-6 lg:top-32 flex items-center gap-3 [writing-mode:vertical-rl]">
      <span aria-hidden className="inline-block h-6 w-px bg-ink-faint" />
      {children}
    </div>
  );
}
