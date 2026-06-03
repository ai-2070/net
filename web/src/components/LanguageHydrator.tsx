"use client";

import { useEffect } from "react";
import { useLanguageStore } from "@/store/useLanguageStore";

/** Runs the one-time client resolution of the docs language (URL param →
 * localStorage → default) into the zustand store. Renders nothing; mount it
 * once near the root of the docs tree. Replaces the old LanguageProvider's
 * initialization effect now that language state is a global store. */
export function LanguageHydrator(): null {
  const hydrate = useLanguageStore((s) => s.hydrate);
  useEffect(() => {
    void hydrate();
  }, [hydrate]);
  return null;
}
