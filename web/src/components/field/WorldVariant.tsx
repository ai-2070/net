"use client";

import { createContext, useContext, type ReactNode } from "react";

/**
 * Which rendition of the landing page is mounted. "v1" is the homepage at
 * "/". "v2" is the lab-notebook rendition at "/v2" (hairline grid with
 * figure labels, isometric diagrams, bold-lead paragraphs, segmented nav).
 * Components that differ between the two read this instead of being forked.
 */
export type WorldVariant = "v1" | "v2" | "v3";

const WorldVariantContext = createContext<WorldVariant>("v1");

export function WorldVariantProvider({
  value,
  children,
}: {
  value: WorldVariant;
  children: ReactNode;
}) {
  return (
    <WorldVariantContext.Provider value={value}>
      {children}
    </WorldVariantContext.Provider>
  );
}

export function useWorldVariant(): WorldVariant {
  return useContext(WorldVariantContext);
}

/** The path on-page "#section" links should resolve against. */
export function useHomePath(): "/" | "/v2" | "/v3" {
  const v = useWorldVariant();
  return v === "v1" ? "/" : `/${v}`;
}

/** Rewrites "/#section" links so they stay on the current rendition. */
export function useHomeHref(): (href: string) => string {
  const home = useHomePath();
  return (href) =>
    home !== "/" && href.startsWith("/#") ? `${home}${href.slice(1)}` : href;
}
