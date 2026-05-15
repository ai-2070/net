import Link from "next/link";
import { MDXRemote } from "next-mdx-remote/rsc";
import remarkGfm from "remark-gfm";
import rehypePrettyCode, {
  type Options as PrettyCodeOptions,
} from "rehype-pretty-code";
import rehypeSlug from "rehype-slug";
import rehypeAutolinkHeadings from "rehype-autolink-headings";
import type { ReactNode, AnchorHTMLAttributes } from "react";
import { CodeBlock } from "@/components/CodeBlock";

// Custom Shiki theme keyed to the site palette so code blends with the
// rest of the page (lime strings, cyan keywords, off-white identifiers,
// dim punctuation) instead of fighting it.
const NET_THEME = {
  name: "net",
  type: "dark",
  semanticHighlighting: false,
  colors: {
    "editor.foreground": "#d4dcd0",
    "editor.background": "#0f1410",
  },
  tokenColors: [
    // default fg — cool slate so anything unscoped reads as ambient terminal
    // text rather than full-white prose. Specific scopes below punch through.
    { settings: { foreground: "#a8b5c0" } },
    // comments
    {
      scope: [
        "comment",
        "punctuation.definition.comment",
        "string.quoted.docstring",
      ],
      settings: { foreground: "#6b7568", fontStyle: "italic" },
    },
    // keywords + storage modifiers
    {
      scope: [
        "keyword",
        "keyword.control",
        "keyword.other",
        "storage",
        "storage.type",
        "storage.modifier",
        "keyword.operator.new",
        "keyword.operator.expression",
      ],
      settings: { foreground: "#3df0ff" },
    },
    // strings
    {
      scope: [
        "string",
        "string.quoted",
        "string.template",
        "string.unquoted",
        "punctuation.definition.string",
      ],
      settings: { foreground: "#c4ff3d" },
    },
    // embedded expressions inside strings — back to default
    {
      scope: ["meta.embedded", "source.groovy.embedded"],
      settings: { foreground: "#d4dcd0" },
    },
    // numbers, booleans, null, escape chars
    {
      scope: [
        "constant.numeric",
        "constant.language",
        "constant.character",
        "constant.character.escape",
      ],
      settings: { foreground: "#ff5e3d" },
    },
    // types — warm amber so they stop blending with lime strings.
    // Be very liberal with scopes: Rust grammar uses `entity.name.type.enum`,
    // `entity.name.struct`, `entity.name.trait`, etc. for declarations,
    // and `entity.name.type` for references. TS/JS/Go/C++ have their own.
    {
      scope: [
        "entity.name.type",
        "entity.name.type.struct",
        "entity.name.type.enum",
        "entity.name.type.union",
        "entity.name.type.trait",
        "entity.name.type.impl",
        "entity.name.type.class",
        "entity.name.type.interface",
        "entity.name.type.module",
        "entity.name.type.alias",
        "entity.name.type-alias",
        "entity.name.struct",
        "entity.name.enum",
        "entity.name.union",
        "entity.name.trait",
        "entity.name.impl",
        "entity.name.class",
        "entity.name.interface",
        "entity.name.namespace",
        "entity.other.inherited-class",
        "support.type",
        "support.class",
        "support.type.primitive",
        "support.type.builtin",
        "meta.type.parameters entity.name.type",
        "meta.struct entity.name",
        "meta.enum entity.name",
        "meta.trait entity.name",
        "meta.impl entity.name",
        "storage.type.struct",
        "storage.type.enum",
        "storage.type.trait",
      ],
      settings: { foreground: "#fdf500" },
    },
    // enum variants / constructors — brighter gold so Some/None/Ok/Err
    // and friends read as their own family next to types
    {
      scope: [
        "variable.other.enummember",
        "constant.other.enum",
        "entity.name.constant.enum",
        "entity.other.attribute-name.enum",
      ],
      settings: { foreground: "#e8c44a" },
    },
    // generic variables — cool slate. Distinct from the bright keyword
    // cyan and sky-blue labels; reads as ambient identifier text.
    // Specific roles below (parameters, fields, properties) override.
    {
      scope: ["variable", "variable.other"],
      settings: { foreground: "#a8b5c0" },
    },
    // object keys / struct fields / function parameters / kwargs / attrs
    // — sky blue. These are the "labels" of code (the names of things in
    // a structure) and were invisible at default ink. Now they read as
    // their own role distinct from variable references.
    {
      scope: [
        // JS / TS object-literal keys
        "meta.object-literal.key",
        "meta.object-literal.key string",
        "meta.object.member.key",
        "meta.object.key",
        "string.unquoted.key",
        "support.type.property-name",

        // member / property access (foo.bar.baz)
        "variable.other.property",
        "variable.object.property",
        "variable.other.object.property",
        "variable.other.member",
        "variable.other.member.field",

        // function parameters — declarations AND usage inside the body
        // both light up; call-site positional args stay default
        "variable.parameter",
        "variable.parameter.function",
        "variable.parameter.function-call",
        "meta.function.parameters variable.parameter",
        "meta.function-call.arguments variable.parameter",
        "meta.function entity.name.parameter",

        // struct / record / enum-variant fields (Rust, Go, TS, etc.)
        "entity.name.field",
        "variable.other.field",
        "variable.other.declaration.field",
        "variable.other.assignment.field",
        "meta.struct.field variable.other",
        "meta.struct.body variable.other",
        "meta.enum.body variable.other",
        "meta.struct.variant variable.other",

        // Python kwargs
        "meta.function-call.python variable.parameter",

        // HTML / JSX / XML attributes
        "entity.other.attribute-name",
      ],
      settings: { foreground: "#7dd3fc" },
    },
    // function & method names — vivid violet. Functions are the verbs of
    // code and deserve their own neon hue against the cyan/lime/orange
    // backdrop. Violet reads as cyberpunk-neon rather than synthwave-pink.
    {
      scope: [
        "entity.name.function",
        "support.function",
        "entity.name.function.call",
        "entity.name.function.definition",
        "meta.function-call entity.name.function",
        "meta.function-call.method entity.name.function",
        "meta.method-call entity.name.function",
        "meta.function entity.name",
        "variable.function",
        "variable.other.method",
        "support.function.builtin",
      ],
      settings: { foreground: "#ff003e" },
    },
    // macros (Rust `assert_eq!`, `vec![]`, `println!`) — dim accent
    {
      scope: [
        "entity.name.function.macro",
        "support.function.macro",
        "keyword.other.macro",
        "meta.macro",
      ],
      settings: { foreground: "#9eda20" },
    },
    // punctuation + operators — fade out
    {
      scope: [
        "punctuation",
        "punctuation.separator",
        "punctuation.terminator",
        "meta.brace",
        "keyword.operator",
      ],
      settings: { foreground: "#6b7568" },
    },
    // decorators / attributes (Rust #[derive(...)], TS @decorator, HTML tags)
    {
      scope: [
        "meta.attribute",
        "punctuation.definition.attribute",
        "entity.name.tag",
        "tag.attribute",
      ],
      settings: { foreground: "#6b8a1e" },
    },
    // markdown
    {
      scope: ["markup.heading", "entity.name.section"],
      settings: { foreground: "#c4ff3d", fontStyle: "bold" },
    },
    {
      scope: ["markup.bold"],
      settings: { foreground: "#d4dcd0", fontStyle: "bold" },
    },
    {
      scope: ["markup.italic"],
      settings: { foreground: "#3df0ff", fontStyle: "italic" },
    },
    {
      scope: ["markup.inline.raw", "markup.fenced_code"],
      settings: { foreground: "#9eda20" },
    },
    {
      scope: ["markup.underline.link"],
      settings: { foreground: "#3df0ff" },
    },
    {
      scope: ["markup.inserted"],
      settings: { foreground: "#c4ff3d" },
    },
    {
      scope: ["markup.deleted"],
      settings: { foreground: "#ff5e3d" },
    },
  ],
} as const;

const prettyCodeOptions: PrettyCodeOptions = {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  theme: NET_THEME as any,
  keepBackground: false,
  defaultLang: { block: "text", inline: "text" },
};

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

// Class names rehype-autolink-headings emits get stringified to a space-
// separated string by the MDX→React bridge; this checks both forms.
function hasAnchorClass(value: unknown): boolean {
  if (typeof value === "string") return value.split(/\s+/).includes("anchor");
  if (Array.isArray(value)) return value.includes("anchor");
  return false;
}

const headingClasses = {
  // hover-reveal the anchor link
  base: "group [&_.anchor]:opacity-0 hover:[&_.anchor]:opacity-100 focus-within:[&_.anchor]:opacity-100",
};

const mdxComponents = {
  h1: (props: { children?: ReactNode }) => (
    <h1
      className={`font-mono text-ink mb-8 mt-2 leading-[1.15] tracking-[0.02em] font-semibold ${headingClasses.base}`}
      style={{ fontSize: "clamp(26px, 3.2vw, 36px)" }}
      {...props}
    />
  ),
  h2: (props: { children?: ReactNode }) => (
    <h2
      className={`font-mono text-ink mt-16 mb-5 leading-tight tracking-[0.04em] uppercase text-[17px] font-semibold border-l-2 border-accent pl-3 scroll-mt-28 ${headingClasses.base}`}
      {...props}
    />
  ),
  h3: (props: { children?: ReactNode }) => (
    <h3
      className={`font-mono text-ink mt-12 mb-4 leading-snug tracking-[0.02em] text-[15px] font-semibold scroll-mt-28 ${headingClasses.base}`}
      {...props}
    />
  ),
  h4: (props: { children?: ReactNode }) => (
    <h4
      className={`font-mono text-accent mt-9 mb-3 text-[12px] uppercase tracking-[0.14em] font-semibold scroll-mt-28 ${headingClasses.base}`}
      {...props}
    />
  ),
  p: (props: { children?: ReactNode }) => (
    <p className="text-[14.5px] text-ink leading-[1.85] mb-6" {...props} />
  ),
  ul: (props: { children?: ReactNode }) => (
    <ul
      className="list-disc list-outside pl-6 mb-6 text-[14px] text-ink-dim leading-[1.8] marker:text-accent space-y-2.5"
      {...props}
    />
  ),
  ol: (props: { children?: ReactNode }) => (
    <ol
      className="list-decimal list-outside pl-6 mb-6 text-[14px] text-ink-dim leading-[1.8] marker:text-accent space-y-2.5"
      {...props}
    />
  ),
  li: (props: { children?: ReactNode }) => (
    <li className="[&>p]:mb-2 [&>p:last-child]:mb-0" {...props} />
  ),
  blockquote: (props: { children?: ReactNode }) => (
    <blockquote
      className="border-l-2 border-accent bg-accent/[0.04] pl-5 pr-5 py-4 my-7 text-[13.5px] text-ink leading-[1.75] [&>p:last-child]:mb-0"
      {...props}
    />
  ),
  hr: () => <hr className="border-line my-12" />,

  // Inline code only — block code comes through `figure` → `CodeBlock`.
  // Detect block via either `data-language` (rehype-pretty-code) OR
  // `language-foo` className (vanilla fenced markdown). Anything else =
  // inline `\`backtick\`` code and gets the boxed accent treatment.
  code: (props: {
    children?: ReactNode;
    className?: string;
    "data-language"?: string;
  }) => {
    const isBlock =
      typeof props["data-language"] === "string" ||
      (typeof props.className === "string" &&
        /\blanguage-/.test(props.className));
    if (isBlock) {
      return <code {...props} />;
    }
    return (
      <code
        className="font-mono text-[12.5px] text-accent bg-bg-2 px-[5px] py-[1px] border border-line break-words"
        {...props}
      />
    );
  },

  // rehype-pretty-code wraps fenced code in:
  //   <figure data-rehype-pretty-code-figure data-language="…"> <pre> <code> … </code> </pre> </figure>
  // We hijack the figure to render our CodeBlock chrome.
  figure: ({
    children,
    ...rest
  }: {
    children?: ReactNode;
    "data-rehype-pretty-code-figure"?: string | undefined;
    "data-language"?: string;
  }) => {
    if ("data-rehype-pretty-code-figure" in rest) {
      const lang = (rest as { "data-language"?: string })["data-language"];
      return <CodeBlock lang={lang}>{children}</CodeBlock>;
    }
    return <figure {...rest}>{children}</figure>;
  },

  // Pre inside a rehype-pretty-code figure has `data-language` set — CodeBlock
  // already provides the container chrome, so render the pre minimally (just
  // padding + scroll). Pre WITHOUT data-language is a standalone block (raw
  // HTML `<pre>`, no fence) — apply full container styling.
  pre: (props: {
    children?: ReactNode;
    "data-language"?: string;
    "data-theme"?: string;
    className?: string;
  }) => {
    const inFigure = typeof props["data-language"] === "string";
    if (inFigure) {
      return (
        <pre
          {...props}
          className="overflow-x-auto px-4 py-3 m-0 text-[12.5px] leading-[1.6] font-mono"
        />
      );
    }
    return (
      <pre
        className="my-6 border border-line bg-bg-2 px-4 py-3 overflow-x-auto text-[12.5px] leading-[1.6] font-mono"
        {...props}
      />
    );
  },

  a: (props: AnchorHTMLAttributes<HTMLAnchorElement>) => {
    const href = props.href ?? "";

    // Heading anchor link from rehype-autolink-headings.
    if (hasAnchorClass(props.className)) {
      return (
        <a
          {...props}
          className="anchor ml-2 text-ink-faint hover:text-accent transition-opacity no-underline font-mono align-middle"
          aria-label="Anchor"
        >
          #
        </a>
      );
    }

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
    <div className="overflow-x-auto my-6 border border-line bg-bg-2/30">
      <table
        className="w-full text-[12.5px] text-ink-dim border-collapse"
        {...props}
      />
    </div>
  ),
  thead: (props: { children?: ReactNode }) => (
    <thead className="bg-bg-2/70 border-b-2 border-accent/40" {...props} />
  ),
  tbody: (props: { children?: ReactNode }) => (
    <tbody
      className="[&>tr:nth-child(even)]:bg-bg-2/30 [&>tr]:transition-colors [&>tr:hover]:bg-accent/[0.04]"
      {...props}
    />
  ),
  tr: (props: { children?: ReactNode }) => (
    <tr className="border-b border-line/50 last:border-b-0" {...props} />
  ),
  th: (props: { children?: ReactNode }) => (
    <th
      className="font-mono text-[11px] tracking-[0.16em] uppercase text-accent text-left px-4 py-3 font-semibold"
      {...props}
    />
  ),
  td: (props: { children?: ReactNode }) => (
    <td
      className="px-4 py-2.5 align-top first:text-ink first:relative first:pl-5 first:before:content-['▸'] first:before:absolute first:before:left-1.5 first:before:text-accent/60 first:before:text-[10px] first:before:top-[11px]"
      {...props}
    />
  ),
  strong: (props: { children?: ReactNode }) => (
    <strong className="text-ink font-semibold" {...props} />
  ),
  em: (props: { children?: ReactNode }) => (
    <em className="text-cyan not-italic" {...props} />
  ),

  // Custom components, usable in .mdx files without any import:
  //   <Note>…</Note>  <Tip>…</Tip>  <Warn>…</Warn>
  //   <Demo title="…">…React children…</Demo>
  //   <Kbd>Ctrl</Kbd>
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
            rehypePlugins: [
              rehypeSlug,
              [rehypePrettyCode, prettyCodeOptions],
              [
                rehypeAutolinkHeadings,
                {
                  behavior: "append",
                  properties: {
                    className: ["anchor"],
                    ariaHidden: "true",
                    tabIndex: -1,
                  },
                },
              ],
            ],
          },
        }}
        components={mdxComponents}
      />
    </article>
  );
}
