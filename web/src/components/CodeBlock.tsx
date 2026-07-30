import { isValidElement, type ReactNode } from "react";
import { CopyButton } from "@/components/CopyButton";

// Recursively pull plain text out of MDX-rendered children so the copy
// button can copy raw code (without the colored token spans).
function extractText(node: ReactNode): string {
  if (node == null || typeof node === "boolean") return "";
  if (typeof node === "string") return node;
  if (typeof node === "number") return String(node);
  if (Array.isArray(node)) return node.map(extractText).join("");
  if (isValidElement(node)) {
    const props = node.props as { children?: ReactNode };
    return extractText(props.children);
  }
  return "";
}

// How a fence token reads in the chrome bar.
//
// Only the tokens that are cryptic or inconsistent are listed; anything absent
// falls through to the fence token itself, so a new language needs no edit here
// to get a correct (if terse) label. The corpus writes both `sh` and `bash` for
// shell and both `ts` and `typescript` for TypeScript — the reader should not
// have to notice that those are the same thing twice.
const LANG_LABEL: Record<string, string> = {
  sh: "shell",
  bash: "shell",
  powershell: "powershell",
  ts: "typescript",
  js: "javascript",
  py: "python",
  rs: "rust",
  jsonc: "json",
  md: "markdown",
};

// Wraps rehype-pretty-code's `<pre>` with a homepage-styled chrome bar
// (▸ language + copy button) and a bordered, accented container. The
// children passed in here ARE the original `<pre>` from rehype — we don't
// re-wrap it (that caused nested padding). The `pre` MDX handler strips
// margin/border when it's inside this figure.
export function CodeBlock({
  lang,
  children,
}: {
  lang?: string;
  children: ReactNode;
}) {
  const text = extractText(children).replace(/\n$/, "");
  const hasLang = typeof lang === "string" && lang.length > 0;
  const label = hasLang ? (LANG_LABEL[lang] ?? lang) : "code";
  return (
    <div className="my-6 border border-line overflow-hidden bg-[#050706]">
      <div className="flex items-center justify-between border-b border-line px-3 py-1.5 bg-bg-2/60">
        <span className="font-mono text-[10px] tracking-[0.14em] uppercase text-accent-dim">
          <span className="text-accent">▸</span> {label}
        </span>
        <CopyButton text={text} />
      </div>
      {children}
    </div>
  );
}
