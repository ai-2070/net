"use client";

import { useEffect } from "react";
import type { Language } from "@/lib/docs-language";
import {
  useLanguageStore,
  type RenditionLinks as Links,
} from "@/store/useLanguageStore";

/** Publishes the current page's per-language hrefs for the language dock.
 *
 * Renders nothing. A rendition page mounts it; every other page does not, and
 * the dock falls back to setting reader context without navigating.
 *
 * Clearing on unmount is the load-bearing half. Without it, navigating from a
 * spine page to a page with one reading for everyone would leave the previous
 * page's links in the store, and the dock would offer to "switch language" by
 * sending the reader back to the page they just left.
 */
export function RenditionLinks({
  links,
  current,
}: {
  links: Links;
  /** The lens this URL *is*. Adopted as reader context — see below. */
  current?: Language;
}) {
  const setRenditions = useLanguageStore((s) => s.setRenditions);
  const setLanguage = useLanguageStore((s) => s.setLanguage);
  const language = useLanguageStore((s) => s.language);
  const hydrated = useLanguageStore((s) => s.hydrated);

  useEffect(() => {
    setRenditions(links);
    return () => setRenditions(null);
    // The map is rebuilt per render on the server but is value-stable per page;
    // key on its serialization so a same-shape remount does not thrash.
  }, [setRenditions, JSON.stringify(links)]);

  // THE URL WINS OVER THE STORED PREFERENCE, once hydration has settled.
  //
  // A rendition URL is explicit about its language: `/docs/sdk/python/invoke` is
  // the Python reading and nothing else. A reader arriving there from a shared
  // link, or from a prose link into another binding, is looking at Python — so
  // the persistent control has to say Python. Leaving the stored preference in
  // charge produced the worst version of this: the page showed Python while the
  // dock said Rust, which is the docs contradicting themselves about the one
  // piece of state they exist to remember.
  //
  // Gated on `hydrated` because `hydrate()` resolves storage and `?lang=`
  // post-mount; adopting before it lands would be overwritten a tick later.
  useEffect(() => {
    if (!hydrated || !current || current === language) return;
    setLanguage(current);
  }, [hydrated, current, language, setLanguage]);

  return null;
}
