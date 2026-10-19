import type { ReactNode } from "react";

/**
 * A sector marking. On wide screens it is stencilled vertically down the
 * section's right margin; on phones and tablets it sits flat at the
 * section's foot, never above a heading. The parent must be `relative`.
 */
export function FieldLabel({ children }: { children: ReactNode }) {
  return (
    <div className="stencil absolute left-6 right-6 bottom-9 !whitespace-normal lg:!whitespace-nowrap lg:left-auto lg:bottom-auto lg:right-4 lg:top-32 flex items-center gap-3 lg:[writing-mode:vertical-rl]">
      <span
        aria-hidden
        className="inline-block h-px w-6 lg:h-6 lg:w-px bg-ink-faint"
      />
      {children}
    </div>
  );
}
