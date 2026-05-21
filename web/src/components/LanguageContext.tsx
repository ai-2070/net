"use client";

import { createContext, useContext, useEffect, useState } from "react";
import {
  DEFAULT_LANGUAGE,
  isLanguage,
  type Language,
} from "@/lib/docs-language";

const STORAGE_KEY = "net-docs-lang";

type Ctx = {
  language: Language;
  setLanguage: (l: Language) => void;
  /** True after the client has hydrated from URL/localStorage. Use this to
   * gate render decisions that depend on the real (non-default) preference
   * so server and first-paint stay consistent. */
  hydrated: boolean;
};

const LanguageCtx = createContext<Ctx>({
  language: DEFAULT_LANGUAGE,
  setLanguage: () => {},
  hydrated: false,
});

export function LanguageProvider({ children }: { children: React.ReactNode }) {
  // Always start with the default on first render so the server-rendered
  // HTML matches the client's first paint. The real preference loads in
  // the post-mount effect below.
  const [language, setLanguageState] = useState<Language>(DEFAULT_LANGUAGE);
  const [hydrated, setHydrated] = useState(false);

  useEffect(() => {
    // URL param wins over localStorage so shared links land users in the
    // intended language even if their stored preference is something else.
    const params = new URLSearchParams(window.location.search);
    const fromUrl = params.get("lang");
    const fromStorage = window.localStorage.getItem(STORAGE_KEY);
    const initial = isLanguage(fromUrl)
      ? fromUrl
      : isLanguage(fromStorage)
        ? fromStorage
        : DEFAULT_LANGUAGE;
    setLanguageState(initial);
    // Persist the URL-derived choice so the next visit without the param
    // remembers it. No-op if it was already in storage.
    if (isLanguage(fromUrl)) {
      try {
        window.localStorage.setItem(STORAGE_KEY, fromUrl);
      } catch {
        // Storage may be unavailable (Safari private mode, embedded webview).
        // Falling back to in-memory state for the session is fine.
      }
    }
    setHydrated(true);
  }, []);

  useEffect(() => {
    // Cross-tab sync — picking a language in one tab updates the others.
    function onStorage(e: StorageEvent) {
      if (e.key !== STORAGE_KEY) return;
      if (isLanguage(e.newValue)) setLanguageState(e.newValue);
    }
    window.addEventListener("storage", onStorage);
    return () => window.removeEventListener("storage", onStorage);
  }, []);

  function setLanguage(l: Language) {
    setLanguageState(l);
    try {
      window.localStorage.setItem(STORAGE_KEY, l);
    } catch {
      // Same as above — storage failure shouldn't break the UI.
    }
  }

  return (
    <LanguageCtx.Provider value={{ language, setLanguage, hydrated }}>
      {children}
    </LanguageCtx.Provider>
  );
}

export function useLanguage(): Ctx {
  return useContext(LanguageCtx);
}

/** Returns true if an entry is visible under the current language. An
 * entry with no `languages` field (or an empty array) is universal. */
export function entryVisibleIn(
  entry: { languages?: Language[] },
  current: Language,
): boolean {
  if (!entry.languages || entry.languages.length === 0) return true;
  return entry.languages.includes(current);
}
