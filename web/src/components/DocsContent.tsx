import Link from "next/link";
import { MDXRemote } from "next-mdx-remote/rsc";
import remarkGfm from "remark-gfm";
import type { ReactNode, AnchorHTMLAttributes } from "react";

const Callout = ({
  variant,
  label,
  children,
}: {
  variant: "note" | "tip" | "warn";
  label: string;
  children?: ReactNode;
}) => {
  const styles = {
    note: {
      border: "border-cyan",
      bg: "bg-cyan/[0.04]",
      tag: "text-cyan",
      glyph: "▸",
    },
    tip: {
      border: "border-accent",
      bg: "bg-accent/[0.04]",
      tag: "text-accent",
      glyph: "★",
    },
    warn: {
      border: "border-warn",
      bg: "bg-warn/[0.04]",
      tag: "text-warn",
      glyph: "▲",
    },
  }[variant];
  return (
    <div
      className={`border-l-2 ${styles.border} ${styles.bg} pl-4 pr-4 py-3 my-5 text-[13px] text-ink leading-[1.65]`}
    >
      <div
        className={`${styles.tag} font-mono text-[10px] tracking-[0.14em] uppercase mb-1.5`}
      >
        {styles.glyph} {label}
      </div>
      <div className="docs-callout-body">{children}</div>
    </div>
  );
};

const mdxComponents = {
  h1: (props: { children?: ReactNode }) => (
    <h1
      className="font-display text-ink mb-7 mt-2 leading-[1.05] tracking-[-0.01em]"
      style={{ fontSize: "clamp(28px, 3.6vw, 42px)" }}
      {...props}
    />
  ),
  h2: (props: { children?: ReactNode }) => (
    <h2
      className="font-head text-ink mt-10 mb-4 leading-tight tracking-[0.02em] uppercase text-[18px] border-l-2 border-accent pl-3"
      {...props}
    />
  ),
  h3: (props: { children?: ReactNode }) => (
    <h3
      className="font-head text-ink mt-7 mb-3 leading-tight tracking-[0.02em] lowercase text-[15px]"
      {...props}
    />
  ),
  h4: (props: { children?: ReactNode }) => (
    <h4
      className="font-mono text-accent mt-5 mb-2 text-[12px] uppercase tracking-[0.12em]"
      {...props}
    />
  ),
  p: (props: { children?: ReactNode }) => (
    <p className="text-[14px] text-ink leading-[1.75] mb-4" {...props} />
  ),
  ul: (props: { children?: ReactNode }) => (
    <ul
      className="list-disc list-outside pl-6 mb-4 text-[14px] text-ink-dim leading-[1.7] marker:text-accent"
      {...props}
    />
  ),
  ol: (props: { children?: ReactNode }) => (
    <ol
      className="list-decimal list-outside pl-6 mb-4 text-[14px] text-ink-dim leading-[1.7] marker:text-accent"
      {...props}
    />
  ),
  li: (props: { children?: ReactNode }) => <li className="mb-1" {...props} />,
  blockquote: (props: { children?: ReactNode }) => (
    <blockquote
      className="border-l-2 border-accent bg-accent/[0.04] pl-4 pr-4 py-3 my-5 text-[13px] text-ink leading-[1.65]"
      {...props}
    />
  ),
  hr: () => <hr className="border-line my-8" />,
  code: (props: { children?: ReactNode; className?: string }) => {
    // Fenced code blocks land with a `language-*` class; inline code has none.
    const isBlock = typeof props.className === "string";
    if (isBlock) {
      return (
        <code
          className="block font-mono text-[12px] text-accent leading-[1.6] whitespace-pre overflow-x-auto"
          {...props}
        />
      );
    }
    return (
      <code
        className="font-mono text-[12.5px] text-accent bg-bg-2 px-[5px] py-[1px] border border-line"
        {...props}
      />
    );
  },
  pre: (props: { children?: ReactNode }) => (
    <pre
      className="border border-line bg-bg-2 p-4 mb-5 overflow-x-auto text-[12px] leading-[1.6]"
      {...props}
    />
  ),
  a: (props: AnchorHTMLAttributes<HTMLAnchorElement>) => {
    const href = props.href ?? "";
    const isInternal = href.startsWith("/") || href.startsWith("#");
    if (isInternal) {
      return (
        <Link
          href={href}
          className="text-accent underline decoration-accent-dim underline-offset-[3px] hover:text-ink"
        >
          {props.children}
        </Link>
      );
    }
    return (
      <a
        {...props}
        target="_blank"
        rel="noopener noreferrer"
        className="text-accent underline decoration-accent-dim underline-offset-[3px] hover:text-ink"
      />
    );
  },
  table: (props: { children?: ReactNode }) => (
    <div className="overflow-x-auto mb-5 border border-line">
      <table
        className="w-full text-[12.5px] text-ink-dim border-collapse"
        {...props}
      />
    </div>
  ),
  th: (props: { children?: ReactNode }) => (
    <th
      className="font-mono text-[11px] tracking-[0.1em] uppercase text-accent text-left px-3 py-2 border-b border-line bg-bg-2"
      {...props}
    />
  ),
  td: (props: { children?: ReactNode }) => (
    <td className="px-3 py-2 border-b border-line align-top" {...props} />
  ),
  strong: (props: { children?: ReactNode }) => (
    <strong className="text-ink font-semibold" {...props} />
  ),
  em: (props: { children?: ReactNode }) => (
    <em className="text-cyan not-italic" {...props} />
  ),

  // Custom components, usable in .mdx files without any import:
  //   <Note>Heads up — …</Note>
  //   <Tip>Pro tip — …</Tip>
  //   <Warn>Careful — …</Warn>
  //   <Demo title="…">…React children…</Demo>
  Note: ({ children }: { children?: ReactNode }) => (
    <Callout variant="note" label="note">
      {children}
    </Callout>
  ),
  Tip: ({ children }: { children?: ReactNode }) => (
    <Callout variant="tip" label="tip">
      {children}
    </Callout>
  ),
  Warn: ({ children }: { children?: ReactNode }) => (
    <Callout variant="warn" label="warning">
      {children}
    </Callout>
  ),
  Demo: ({
    title,
    children,
  }: {
    title?: string;
    children?: ReactNode;
  }) => (
    <div className="my-6 border border-line bg-bg-2 overflow-hidden">
      {title ? (
        <div className="border-b border-line px-4 py-2 text-[10px] tracking-[0.14em] text-ink-dim uppercase flex items-center gap-2">
          <span className="text-accent">▸</span> {title}
        </div>
      ) : null}
      <div className="p-5">{children}</div>
    </div>
  ),
  Kbd: ({ children }: { children?: ReactNode }) => (
    <kbd className="font-mono text-[11px] text-ink bg-bg-2 border border-line px-1.5 py-[2px] mx-[1px]">
      {children}
    </kbd>
  ),
};

export function DocsContent({
  source,
  format = "md",
}: {
  source: string;
  format?: "md" | "mdx";
}) {
  return (
    <article className="docs-content">
      <MDXRemote
        source={source}
        options={{
          mdxOptions: {
            format,
            remarkPlugins: [remarkGfm],
          },
        }}
        components={mdxComponents}
      />
    </article>
  );
}
