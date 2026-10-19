"use client";

import {
  Children,
  cloneElement,
  isValidElement,
  type ReactElement,
  type ReactNode,
} from "react";
import { useFieldHold } from "./useFieldHold";

// The copy carries hard <br />s from the old stacked titles. The words stay;
// the breaks become spaces so a title flows onto at most two lines. Each
// top-level phrase still lifts into place on its own beat.
function unbreak(node: ReactNode): ReactNode {
  return Children.map(node, (child) => {
    if (isValidElement(child)) {
      if (child.type === "br") return " ";
      const el = child as ReactElement<{ children?: ReactNode }>;
      if (el.props.children !== undefined) {
        return cloneElement(el, undefined, unbreak(el.props.children));
      }
    }
    return child;
  });
}

function toPhrases(children: ReactNode): ReactNode[][] {
  const phrases: ReactNode[][] = [[]];
  Children.toArray(children).forEach((child) => {
    if (isValidElement(child) && child.type === "br") phrases.push([]);
    else phrases[phrases.length - 1]?.push(unbreak(child));
  });
  return phrases.filter((p) => p.length > 0);
}

export function FieldHeading({ children }: { children: ReactNode }) {
  const ref = useFieldHold<HTMLHeadingElement>();
  const phrases = toPhrases(children);
  return (
    <h2
      ref={ref}
      className="field-title font-display font-bold uppercase leading-[0.95] tracking-[-0.03em] text-ink mb-10 max-w-[19em] text-balance"
      style={{ fontSize: "clamp(30px, 5.2vw, 80px)" }}
    >
      {phrases.map((phrase, i) => (
        <span key={i}>
          <span
            className="lift-line !inline-block"
            style={{ transitionDelay: `${i * 110}ms` }}
          >
            {phrase}
          </span>
          {i < phrases.length - 1 ? " " : null}
        </span>
      ))}
    </h2>
  );
}
