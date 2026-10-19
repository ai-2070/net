"use client";

import { Children, Fragment, type ReactNode } from "react";

const LEDE =
  "text-[19px] md:text-[21px] text-ink max-w-[34em] leading-[1.5] font-light mb-14";

/**
 * A section's opening paragraph. Its first sentence is set bright and
 * medium, and the rest continues in grey ("Compute. Find reliable…"). The
 * words are never changed; only the weight and colour of the lead differ.
 */
export function Lede({ children }: { children: ReactNode }) {
  const lead: ReactNode[] = [];
  const rest: ReactNode[] = [];
  let split = false;
  Children.toArray(children).forEach((child, i) => {
    if (split) {
      rest.push(child);
      return;
    }
    if (typeof child === "string") {
      const m = child.match(/^([\s\S]*?\.)(\s[\s\S]*)?$/);
      if (m && m[1] && (m[2] !== undefined || child.endsWith("."))) {
        lead.push(m[1]);
        if (m[2]) rest.push(m[2]);
        split = true;
        return;
      }
    }
    lead.push(<Fragment key={i}>{child}</Fragment>);
  });

  return (
    <p className={`${LEDE} !text-ink-dim`}>
      <span className="text-ink font-medium">{lead}</span>
      {rest}
    </p>
  );
}
