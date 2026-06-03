"use client";

import { create } from "zustand";
import { persist } from "zustand/middleware";
import { superjsonStorage } from "@/store/superjson";
import {
  DEFAULT_LANGUAGE,
  isLanguage,
  type Language,
} from "@/lib/docs-language";

const STORAGE_KEY = "net-docs-lang";

export interface LanguageState {
  language: Language;
  /** True once the client has resolved the real preference from URL /
   * localStorage. Gate first-paint-sensitive render on this so the server
   * and the client's first paint agree on DEFAULT_LANGUAGE. */
  hydrated: boolean;
  setLanguage: (l: Language) => void;
  /** Resolve the persisted preference (via the persist middleware) and let
   * the URL (`?lang=`) override it. Idempotent — safe to call repeatedly
   * (e.g. StrictMode's double-mount). Runs post-mount so SSR first paint
   * stays on the default. */
  hydrate: () => Promise<void>;
}

export const useLanguageStore = create<LanguageState>()(
  persist(
    (set, get) => ({
      // Always start on the default so the server-rendered HTML matches the
      // client's first paint. `hydrate()` loads the real preference post-mount
      // (persist runs with skipHydration, so nothing loads before then).
      language: DEFAULT_LANGUAGE,
      hydrated: false,
      // persist auto-writes the new language to superjson storage on change.
      setLanguage: (l: Language) => set({ language: l }),
      hydrate: async () => {
        if (get().hydrated) return;
        // Pull the persisted value in now (skipHydration deferred it).
        await useLanguageStore.persist.rehydrate();
        // URL param wins over storage so shared links land users in the
        // intended language. Setting it here also persists it for next time.
        const fromUrl = new URLSearchParams(window.location.search).get("lang");
        if (isLanguage(fromUrl)) set({ language: fromUrl });
        set({ hydrated: true });
      },
    }),
    {
      name: STORAGE_KEY,
      storage: superjsonStorage,
      // Only the preference is persisted — `hydrated` is per-session client
      // state, not something to restore from disk.
      partialize: (s) => ({ language: s.language }),
      // Keep SSR first paint on the default; `hydrate()` rehydrates on mount.
      skipHydration: true,
    },
  ),
);

// Cross-tab sync — picking a language in one tab updates the others by
// re-reading the persisted (superjson-encoded) value. Attached once at module
// load; the store is a session-lifetime singleton, so there's nothing to tear
// down.
if (typeof window !== "undefined") {
  window.addEventListener("storage", (e: StorageEvent) => {
    if (e.key !== STORAGE_KEY) return;
    void useLanguageStore.persist.rehydrate();
  });
}
