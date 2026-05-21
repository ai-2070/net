"use client";

import { LANGUAGES, type Language } from "@/lib/docs-language";
import { useLanguage } from "@/components/LanguageContext";

const LABELS: Record<Language, string> = {
  rust: "Rust",
  ts: "TS",
  python: "Python",
  go: "Go",
  c: "C",
};

export function LanguageSwitcher({ className = "" }: { className?: string }) {
  const { language, setLanguage } = useLanguage();

  return (
    <div
      className={`border border-line bg-bg-2/30 px-3 py-2 ${className}`}
      role="radiogroup"
      aria-label="Documentation language"
    >
      <div className="flex items-center justify-between gap-2 mb-1.5">
        <span className="font-mono text-[9px] tracking-[0.22em] uppercase text-ink-faint">
          <span className="text-accent">$</span> lang
        </span>
        <span className="font-mono text-[9px] tracking-[0.14em] uppercase text-accent-dim">
          {LABELS[language]}
        </span>
      </div>
      <div className="flex flex-wrap gap-1">
        {LANGUAGES.map((l) => {
          const on = l === language;
          return (
            <button
              key={l}
              type="button"
              role="radio"
              aria-checked={on}
              onClick={() => setLanguage(l)}
              className={`cursor-pointer font-mono text-[10px] tracking-[0.06em] px-2 py-1 border transition-colors ${
                on
                  ? "border-accent text-accent bg-accent/[0.08]"
                  : "border-line text-ink-dim hover:text-ink hover:border-accent-dim"
              }`}
            >
              {LABELS[l]}
            </button>
          );
        })}
      </div>
    </div>
  );
}
