"use client";

import { Children, isValidElement, type ReactNode } from "react";
import { useFieldHold } from "./useFieldHold";

// Splits the heading at its top-level <br />s so each line is lifted into
// place on its own beat. The words themselves are untouched.
function toLines(children: ReactNode): ReactNode[][] {
  const lines: ReactNode[][] = [[]];
  Children.toArray(children).forEach((child) => {
    if (isValidElement(child) && child.type === "br") {
      lines.push([]);
    } else {
      lines[lines.length - 1]?.push(child);
    }
  });
  return lines.filter((l) => l.length > 0);
}

export function FieldHeading({ children }: { children: ReactNode }) {
  const ref = useFieldHold<HTMLHeadingElement>();
  const lines = toLines(children);
  return (
    <h2
      ref={ref}
      className="font-display font-bold uppercase leading-[0.92] tracking-[-0.03em] text-ink mb-10 max-w-[15em]"
      style={{ fontSize: "clamp(40px, 5.6vw, 88px)" }}
    >
      {lines.map((line, i) => (
        <span
          key={i}
          className="lift-line"
          style={{ transitionDelay: `${i * 110}ms` }}
        >
          {line}
        </span>
      ))}
    </h2>
  );
}
