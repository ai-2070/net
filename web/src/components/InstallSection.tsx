"use client";

import { useState } from "react";
import { SectionLabel } from "./SectionLabel";
import { DisplayHeading } from "./DisplayHeading";

interface InstallCard {
  lang: string;
  ext: string;
  cmd: string;
  copy: string;
  meta: string;
}

const INSTALL_CARDS: readonly InstallCard[] = [
  {
    lang: "Rust",
    ext: ".rs",
    cmd: "$ cargo add net-mesh-sdk",
    copy: "cargo add net-mesh-sdk",
    meta: "crate: net-mesh-sdk",
  },
  {
    lang: "TypeScript",
    ext: ".ts",
    cmd: "$ npm i @net-mesh/sdk\n       @net-mesh/core",
    copy: "npm i @net-mesh/sdk @net-mesh/core",
    meta: "scope: @net-mesh",
  },
  {
    lang: "Python",
    ext: ".py",
    cmd: "$ pip install net-mesh-sdk",
    copy: "pip install net-mesh-sdk",
    meta: "dist: net-mesh-sdk",
  },
  {
    lang: "Go",
    ext: ".go",
    cmd: "$ go get github.com/\n  ai-2070/net/go",
    copy: "go get github.com/ai-2070/net/go",
    meta: "module: ai-2070/net/go",
  },
];

export function InstallSection({
  id = "install",
  label = "§10 / install",
}: {
  id?: string;
  label?: string;
} = {}) {
  const [copied, setCopied] = useState<string | null>(null);

  const handleCopy = async (lang: string, text: string): Promise<void> => {
    try {
      await navigator.clipboard.writeText(text);
      setCopied(lang);
      window.setTimeout(() => {
        setCopied((current) => (current === lang ? null : current));
      }, 1800);
    } catch {
      // clipboard API can fail in insecure contexts; ignore silently
    }
  };

  return (
    <section id={id} className="bg-bg-2 border-b border-line px-6 py-20">
      <SectionLabel>{label}</SectionLabel>
      <DisplayHeading>
        five languages.
        <br />
        one engine.
      </DisplayHeading>

      <p className="text-[16px] text-ink max-w-[740px] leading-[1.6] font-light mb-12">
        All SDKs wrap the same Rust core. The SDK is the developer experience,
        the engine is Rust.
      </p>

      <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-4 gap-4 mt-5">
        {INSTALL_CARDS.map((c) => {
          const isCopied = copied === c.lang;
          return (
            <button
              key={c.lang}
              type="button"
              onClick={() => handleCopy(c.lang, c.copy)}
              aria-label={`Copy ${c.lang} install command`}
              className="text-left border border-line p-5 bg-bg transition-colors hover:border-accent-dim cursor-pointer focus:outline-none focus:border-accent"
            >
              <div className="flex items-center justify-between mb-4">
                <span className="text-[11px] text-ink tracking-[0.15em] uppercase font-semibold">
                  {c.lang}
                </span>
                <span
                  className={`text-[10px] px-1.5 py-0.5 transition-colors ${
                    isCopied
                      ? "text-bg bg-accent border border-accent font-semibold"
                      : "text-accent border border-accent-dim"
                  }`}
                >
                  {isCopied ? "✓ COPIED" : c.ext}
                </span>
              </div>
              <pre className="bg-bg-2 p-3 text-[11px] text-accent border-l-2 border-accent overflow-x-auto font-mono leading-[1.5]">
                {c.cmd}
              </pre>
              <div className="text-ink-dim text-[10px] mt-2.5">{c.meta}</div>
            </button>
          );
        })}
      </div>

      <p className="mt-7 text-[11px] text-ink-dim">
        <a
          href="https://github.com/ai-2070/net/tree/master/net/crates/net/include"
          target="_blank"
          rel="noopener noreferrer"
          className="hover:text-ink transition-colors"
        >
          // C bindings via <span className="text-accent">net.h</span>
        </a>{" "}
        — build cdylib with{" "}
        <span className="relative inline-block">
          <button
            type="button"
            onClick={() =>
              handleCopy(
                "ffi-build",
                "cargo build --release --features net,ffi,redex,cortex,netdb,redis,jetstream",
              )
            }
            aria-label="Copy cargo build command"
            className="text-accent font-mono cursor-pointer transition-colors hover:text-ink focus:outline-none focus:text-ink"
          >
            cargo build --release --features
            net,ffi,redex,cortex,netdb,redis,jetstream
          </button>
          {copied === "ffi-build" ? (
            <span
              aria-hidden
              className="slide-up-fade absolute left-0 -top-1 text-[10px] text-accent font-mono whitespace-nowrap"
            >
              ✓ copied
            </span>
          ) : null}
        </span>
        . Lower-level bindings (skip SDK ergonomics, talk directly to the
        engine): <span className="text-accent">net-mesh</span>,{" "}
        <span className="text-accent">@net-mesh/core</span>,{" "}
        <span className="text-accent">net-mesh</span> (PyPI binding).
      </p>
    </section>
  );
}
