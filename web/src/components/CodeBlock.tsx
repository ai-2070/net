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
  return (
    <div className="my-6 border border-line bg-bg-2 overflow-hidden">
      <div className="flex items-center justify-between border-b border-line px-3 py-1.5 bg-bg/40">
        <span className="font-mono text-[10px] tracking-[0.14em] uppercase text-accent-dim">
          <span className="text-accent">▸</span> {hasLang ? lang : "code"}
        </span>
        <CopyButton text={text} />
      </div>
      {children}
    </div>
  );
}
