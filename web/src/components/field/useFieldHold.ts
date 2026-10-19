"use client";

import { useEffect, useRef } from "react";

/**
 * Arms an element for the field's "lift" only when it starts below the
 * fold, then marks it held once it scrolls into view. Anything already on
 * screen at load is never hidden, and without script nothing is hidden.
 */
export function useFieldHold<T extends HTMLElement>() {
  const ref = useRef<T>(null);

  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    if (window.matchMedia("(prefers-reduced-motion: reduce)").matches) return;
    const rect = el.getBoundingClientRect();
    if (rect.top < window.innerHeight * 0.92) return;

    el.dataset.armed = "true";
    const io = new IntersectionObserver(
      ([entry]) => {
        if (entry?.isIntersecting) {
          el.dataset.held = "true";
          io.disconnect();
        }
      },
      { rootMargin: "0px 0px -12% 0px" },
    );
    io.observe(el);
    return () => io.disconnect();
  }, []);

  return ref;
}
