"use client";

import { useState } from "react";

export function CopyButton({ text }: { text: string }) {
  const [copied, setCopied] = useState(false);

  const onCopy = (): void => {
    if (!navigator.clipboard) return;
    navigator.clipboard
      .writeText(text)
      .then(() => {
        setCopied(true);
        window.setTimeout(() => setCopied(false), 1500);
      })
      .catch(() => {
        // clipboard API can fail in insecure contexts; ignore silently
      });
  };

  return (
    <button
      type="button"
      onClick={onCopy}
      aria-label={copied ? "Copied" : "Copy code"}
      className="font-mono text-[10px] tracking-[0.14em] uppercase text-ink-faint hover:text-accent transition-colors flex items-center gap-1.5 cursor-pointer"
    >
      {copied ? (
        <>
          <span aria-hidden className="text-accent">
            ✓
          </span>
          copied
        </>
      ) : (
        <>
          <span aria-hidden>⎘</span>
          copy
        </>
      )}
    </button>
  );
}
