import type { Metadata } from "next";
import Link from "next/link";
import { notFound } from "next/navigation";
import {
  getAllSlugs,
  resolveDoc,
  readDocSource,
  composeRendition,
  extractToc,
  getPrevNextByLanguage,
  assertEveryAdaptivePageDetected,
  assertNoCrossLanguageNeighbours,
  navChildren,
  renditionPath,
  boundaryPath,
  type AdaptivePage,
  type DocFolder,
  type TocEntry,
} from "@/lib/docs";
import { capabilityRow, type CapabilityRow } from "@/lib/capability-record";
import { RenditionLinks } from "@/components/RenditionLinks";
import {
  LENSES,
  LENS_SLUG,
  type Lens,
} from "@/lib/docs-language";
import { DocsContent } from "@/components/DocsContent";
import { DocsToc } from "@/components/DocsToc";
import { DocsPrevNextTop, DocsPrevNextBottom } from "@/components/DocsPrevNext";

interface PageProps {
  params: Promise<{ slug: string[] }>;
}

// Pure SSG. Every doc path is enumerated by `generateStaticParams` and
// baked at build time. Unknown slugs 404 (not dynamically rendered) and
// the output is never revalidated — change a doc → ship a new build.
export const dynamic = "force-static";
export const dynamicParams = false;
export const revalidate = false;

export function generateStaticParams(): Array<{ slug: string[] }> {
  // Both run once per build, and throw rather than warn. See each function's
  // comment: these are the acceptance tests for language-aware prev/next and
  // for adaptive-page detection, placed where they cannot be skipped.
  //
  // Detection first. When it breaks, EVERY adaptive page decomposes into five
  // ordinary siblings, and the cross-language check then reports a dozen
  // boundary crossings that are all one root cause — which reads as a content
  // problem and sends the reader into the order config.
  assertEveryAdaptivePageDetected();
  assertNoCrossLanguageNeighbours();
  return getAllSlugs().map((slug) => ({ slug }));
}

// Every docs page used to ship the same social and search preview: a title
// and nothing else, so 143 pages were indistinguishable to a crawler or a
// link unfurl. Titles and descriptions both come from the page's own
// frontmatter now, which is why this can be per-page without a second
// registry to maintain.
export async function generateMetadata({
  params,
}: PageProps): Promise<Metadata> {
  const { slug } = await params;
  const resolved = resolveDoc(slug);
  if (!resolved) return { title: "Not Found · Docs · Net" };

  const node = resolved.kind === "file" ? resolved.file : resolved.folder;
  const lensName =
    resolved.kind === "rendition"
      ? LENS_LABEL[resolved.lens]
      : resolved.kind === "boundary"
        ? "C"
        : null;
  const title = lensName
    ? `${node.title} in ${lensName} · Docs · Net`
    : `${node.title} · Docs · Net`;
  const description = node.description;
  const path = `/docs${slug.length ? `/${slug.join("/")}` : ""}`;

  // Every rendition of an adaptive page shares its universal body, so the four
  // routes carry a lot of identical text by design. Each is canonical for itself
  // and declares its siblings; without that the set reads as duplicate content
  // and a crawler picks a winner for us.
  const siblingsOf =
    resolved.kind === "rendition" || resolved.kind === "boundary"
      ? resolved.folder
      : null;
  const languages = siblingsOf
    ? Object.fromEntries([
        ...LENSES.map((l) => [
          LENS_HREFLANG[l],
          renditionPath(siblingsOf, LENS_SLUG[l]),
        ]),
        // x-default is the bare route in both shapes: the neutral router that
        // states the objective and offers the lenses, never one of them.
        ["x-default", `/docs/${siblingsOf.slug.join("/")}`],
      ])
    : undefined;

  return {
    title,
    description,
    alternates: { canonical: path, ...(languages ? { languages } : {}) },
    openGraph: {
      type: "article",
      siteName: "Net",
      title,
      description,
      url: path,
    },
    twitter: {
      card: "summary",
      title,
      description,
    },
  };
}


// Display names and hreflang codes. `Lens` ids stay `ts`; readers see
// "TypeScript" and crawlers see a real BCP-47-shaped tag.
const LENS_LABEL: Record<Lens, string> = {
  rust: "Rust",
  ts: "TypeScript",
  python: "Python",
  go: "Go",
};

// Not natural languages, so these are private-use subtags rather than a claim
// about human language. They exist to declare the four routes as alternates of
// one another; a crawler that ignores them still sees four self-canonical pages.
const LENS_HREFLANG: Record<Lens, string> = {
  rust: "x-rust",
  ts: "x-typescript",
  python: "x-python",
  go: "x-go",
};

/** The rendition chooser. Rendered on the bare route and under every rendition.
 *
 * Availability is explicit per lens: a fragment that does not exist says so
 * rather than being silently omitted, because an omitted lens is
 * indistinguishable from a lens nobody has looked at. */
function LensChooser({
  folder,
  page,
  current,
}: {
  folder: DocFolder;
  page: AdaptivePage;
  current?: Lens | "c";
}) {
  return (
    <div className="border border-line bg-bg-2/30 px-4 py-3 my-6">
      <div className="font-mono text-[9px] tracking-[0.22em] uppercase text-ink-faint mb-2.5">
        <span className="text-accent">$</span> read this in
      </div>
      <div className="flex flex-wrap gap-1.5">
        {LENSES.map((lens) => {
          const has = Boolean(page.fragments[lens]);
          const on = current === lens;
          return (
            <Link
              key={lens}
              href={renditionPath(folder, LENS_SLUG[lens])}
              className={`font-mono text-[11px] tracking-[0.04em] px-2 py-1 border transition-colors ${
                on
                  ? "border-accent text-accent bg-accent/[0.08]"
                  : has
                    ? "border-line text-ink-dim hover:text-ink hover:border-accent-dim"
                    : "border-line/60 text-ink-faint hover:text-ink-dim"
              }`}
            >
              {LENS_LABEL[lens]}
              {has ? null : <span className="ml-1.5 text-[9px]">not yet</span>}
            </Link>
          );
        })}
        <Link
          href={boundaryPath(folder, page)}
          className={`font-mono text-[11px] tracking-[0.04em] px-2 py-1 border transition-colors ${
            current === "c"
              ? "border-accent text-accent bg-accent/[0.08]"
              : "border-line/60 text-ink-dim hover:text-ink hover:border-accent-dim"
          }`}
        >
          C<span className="ml-1.5 text-[9px]">boundary</span>
        </Link>
      </div>
    </div>
  );
}

/** The parity row for this page's operation, RENDERED FROM THE RECORD.
 *
 * Nothing here is typed into a docs page. `docs/data/capabilities/*.yaml` is the
 * one authored answer to "does binding X support operation Y", every positive
 * cell resolves a real symbol in that binding's tree under CI, and the JSON this
 * reads is generated from it and equality-checked. A page therefore cannot
 * disagree with the record about what a binding does — which is the whole point,
 * because a page that is confidently wrong about a binding costs a reader more
 * than a page that says nothing.
 *
 * `core-only` is called out rather than folded into "supported": it is the
 * single most common way to be wrong about Net in Node and Python, and a reader
 * who sees "supported" and reaches for the ergonomic wrapper has been misled by
 * a technically true badge.
 */
function ParityRow({
  row,
  current,
}: {
  row: CapabilityRow;
  current?: Lens | "c";
}) {
  const tone: Record<string, string> = {
    supported: "text-accent",
    partial: "text-cyan",
    experimental: "text-cyan",
    "not exposed": "text-ink-faint",
    "n/a": "text-ink-faint",
  };
  return (
    <div className="border border-line bg-bg-2/30 px-4 py-3 my-6">
      <div className="font-mono text-[9px] tracking-[0.22em] uppercase text-ink-faint mb-2.5">
        <span className="text-accent">§</span> parity · {row.operation}
        <span className="ml-2 text-ink-faint normal-case tracking-normal">
          from the capability record
        </span>
      </div>
      <div className="flex flex-wrap gap-x-5 gap-y-1.5">
        {row.cells.map(({ binding, lang, cell }) => {
          const mine = lang !== null && lang === current;
          return (
            <span
              key={binding}
              className={`font-mono text-[11px] ${mine ? "text-ink" : "text-ink-dim"}`}
            >
              <span className={mine ? "text-accent" : ""}>{binding}</span>{" "}
              <span className={tone[cell.status] ?? "text-ink-dim"}>
                {cell.status}
              </span>
              {cell.mode ? (
                <span className="text-cyan"> · {cell.mode}</span>
              ) : null}
            </span>
          );
        })}
      </div>
      {row.cells.some((c) => c.cell.mode === "core-only") ? (
        <p className="font-mono font-light text-[12px] text-ink-dim leading-[1.7] mt-2.5 mb-0">
          <span className="text-cyan">core-only</span> means the operation exists
          on the low-level binding but not on the ergonomic SDK wrapper. Reach one
          layer down; it is not a gap.
        </p>
      ) : null}
    </div>
  );
}

/** Shown when the selected lens has no fragment. Never a fallback to another
 *  language: that is the silent-fallback failure the doctrine forbids. */
function AbsenceNotice({ lens }: { lens: Lens }) {
  return (
    <div className="border border-line border-l-2 border-l-accent-dim bg-accent/[0.03] px-5 py-4 my-6">
      <div className="font-mono text-[10px] tracking-[0.18em] uppercase text-accent-dim mb-2">
        no {LENS_LABEL[lens]} rendition yet
      </div>
      <p className="font-mono font-light text-[13px] text-ink leading-[1.8] m-0">
        Everything above is language-neutral and applies to {LENS_LABEL[lens]}.
        What is missing is the {LENS_LABEL[lens]}-specific part: the install line,
        the construction, the runtime caveat, and how to verify it worked. This
        page will not show you another language&rsquo;s code in its place.
      </p>
    </div>
  );
}

/** The C segment. A generated projection, never an authored fragment. */
function BoundaryNotice({
  boundary,
}: {
  boundary?: { href: string; label: string };
}) {
  return (
    <div className="border border-line border-l-2 border-l-cyan bg-cyan/[0.03] px-5 py-4 my-6">
      <div className="font-mono text-[10px] tracking-[0.18em] uppercase text-cyan mb-2">
        C is a boundary surface
      </div>
      <p className="font-mono font-light text-[13px] text-ink leading-[1.8] mb-3">
        The C ABI is not a fifth SDK with the same ergonomics under different
        syntax. It exposes handles, buffers, lengths and explicit teardown, so it
        is documented by boundary concern rather than by capability — which is why
        this page has no C rendition and will not grow one.
      </p>
      <Link
        href={boundary?.href ?? "/docs/sdk/c"}
        className="font-mono text-[12px] text-accent hover:underline"
      >
        → {boundary?.label ?? "The C ABI section"}
      </Link>
      {/* Per-operation support status belongs here, generated from
          docs/data/capabilities/*.yaml. It is not wired yet: the site has no YAML
          parser, and the intended bridge is a generated, equality-checked JSON
          module — the same pattern as the skill coverage copies. Until then this
          panel makes no per-operation claim rather than guessing one. */}
    </div>
  );
}

function TocRail({ entries }: { entries: readonly TocEntry[] }) {
  return (
    <aside className="hidden xl:block xl:sticky xl:top-24 xl:self-start xl:max-h-[calc(100vh-7rem)] xl:overflow-y-auto pt-1 pr-2">
      <DocsToc entries={entries} />
    </aside>
  );
}

// Stable djb2 hash → deterministic "size" + inode tag per slug.
function fakeHash(s: string): number {
  let h = 5381;
  for (let i = 0; i < s.length; i++) {
    h = ((h << 5) + h + s.charCodeAt(i)) >>> 0;
  }
  return h;
}

function fakeSize(slug: string): string {
  const k = 1 + (fakeHash(slug) % 90) / 10; // 1.0k – 10.0k
  return `${k.toFixed(1)}k`;
}

function fakeInode(slug: string): string {
  return (fakeHash(slug) & 0xffff).toString(16).padStart(4, "0");
}

// Newest entry "now", each row ~30 days older.
function fakeMtime(i: number): string {
  const d = new Date();
  d.setDate(d.getDate() - i * 30);
  const y = d.getFullYear();
  const m = String(d.getMonth() + 1).padStart(2, "0");
  const day = String(d.getDate()).padStart(2, "0");
  return `${y}-${m}-${day}`;
}

function FolderIndex({ folder }: { folder: DocFolder }) {
  const children = navChildren(folder);
  const childCount = children.length;

  return (
    <div>
      {/* Eyebrow label — homepage-style section marker */}
      <div className="font-mono text-[10px] tracking-[0.22em] text-accent-dim uppercase mb-3 flex items-baseline justify-between">
        <span>
          <span className="text-accent">§</span> section ·{" "}
          <span className="text-ink-faint">/docs/{folder.slug.join("/")}</span>
        </span>
        <span className="text-ink-faint normal-case tracking-normal tabular-nums">
          {String(childCount).padStart(2, "0")} entries
        </span>
      </div>

      {/* Letter-spaced display title — record sleeve aesthetic */}
      <h1
        className="font-display text-ink mb-2 leading-[1]"
        style={{
          fontSize: "clamp(32px, 4.4vw, 56px)",
          letterSpacing: "0.04em",
        }}
      >
        {folder.title}
      </h1>
      <div
        aria-hidden
        className="border-t border-line/60 mb-10"
        style={{
          backgroundImage:
            "linear-gradient(90deg, transparent 0, transparent 60%, var(--color-accent-dim) 60%, var(--color-accent-dim) 62%, transparent 62%)",
        }}
      />

      {childCount === 0 ? (
        <p className="font-mono text-ink-dim text-[13px]">
          <span className="text-ink-faint">·</span> empty section
        </p>
      ) : (
        <div className="space-y-px">
          {children.map((child, i) => {
            const slugKey = child.slug.join("/");
            const isFolder = child.kind === "folder";
            const size = fakeSize(slugKey);
            const mtime = fakeMtime(i);
            const inode = fakeInode(slugKey);
            const isNew = i === 0;
            const num = String(i + 1).padStart(2, "0");
            return (
              <Link
                key={slugKey}
                href={`/docs/${slugKey}`}
                className="group relative block border border-line bg-bg-2/30 hover:bg-bg-2/60 hover:border-accent-dim transition-colors"
              >
                <div className="grid grid-cols-[auto_minmax(0,1fr)_auto] items-center gap-4 px-4 py-3.5">
                  {/* Big index number */}
                  <span
                    aria-hidden
                    className="font-mono font-light text-[28px] leading-none text-accent-dim group-hover:text-accent transition-colors tabular-nums shrink-0"
                  >
                    {num}
                  </span>

                  {/* Codename + meta */}
                  <div className="min-w-0">
                    <div
                      className={`font-mono uppercase text-[15px] leading-tight tracking-[0.04em] truncate transition-colors ${
                        isFolder
                          ? "text-cyan group-hover:text-accent"
                          : "text-ink group-hover:text-accent"
                      }`}
                    >
                      {isFolder ? `▸ ${child.title}` : child.title}
                    </div>
                    <div className="font-mono text-[10px] text-ink-faint tracking-[0.06em] tabular-nums mt-1.5 flex flex-wrap items-center gap-x-3 gap-y-0.5">
                      <span>
                        <span className="text-accent-dim">·</span> {mtime}
                      </span>
                      <span>
                        <span className="text-accent-dim">·</span> {size}
                      </span>
                      <span>
                        <span className="text-accent-dim">·</span> 0x{inode}
                      </span>
                      <span className="hidden sm:inline">
                        <span className="text-accent-dim">·</span>{" "}
                        {isFolder ? "section" : "doc"}
                      </span>
                    </div>
                  </div>

                  {/* Right slot: NEW badge on first row, otherwise an arrow */}
                  <span className="shrink-0 flex items-center">
                    {isNew ? (
                      <span className="font-mono text-[9px] tracking-[0.22em] uppercase bg-accent text-bg px-1.5 py-0.5 font-bold">
                        new
                      </span>
                    ) : (
                      <span
                        aria-hidden
                        className="font-mono text-ink-faint group-hover:text-accent transition-colors"
                      >
                        →
                      </span>
                    )}
                  </span>
                </div>
                {/* Bottom hairline that lights up on hover */}
                <span
                  aria-hidden
                  className="absolute left-0 right-0 bottom-0 h-px bg-line group-hover:bg-accent/40 transition-colors"
                />
              </Link>
            );
          })}
        </div>
      )}

      {/* Status footer — small live indicator + count */}
      {childCount > 0 ? (
        <div className="mt-6 flex items-center justify-between text-[9px] tracking-[0.18em] uppercase font-mono">
          <span className="flex items-center gap-1.5">
            <span className="w-1 h-1 rounded-full bg-accent inline-block animate-pulse-dot" />
            <span className="text-accent-dim">live</span>
          </span>
          <span className="text-ink-faint tabular-nums">
            {String(childCount).padStart(2, "0")} / total
          </span>
        </div>
      ) : null}
    </div>
  );
}

// What kind of page this is, from its section. Concept, guide, tutorial,
// reference and agent brief are read differently — a tutorial is followed once,
// a reference is returned to — and until now the only signal was the URL.
// Section-derived rather than frontmatter-declared so it needs no content edit
// and cannot disagree with where the page actually lives.
const PAGE_KIND: Record<string, string> = {
  start: "getting started",
  worldview: "worldview",
  concepts: "concept",
  guides: "guide",
  tutorials: "tutorial",
  "agent-briefs": "agent brief",
  payments: "payments",
  reference: "reference",
  sdk: "sdk",
  releases: "release note",
};

function KindBadge({ section }: { section?: string }) {
  const kind = section ? PAGE_KIND[section] : undefined;
  if (!kind) return null;
  return (
    <span className="font-mono text-[9px] tracking-[0.18em] uppercase text-accent-dim border border-line px-1.5 py-0.5">
      {kind}
    </span>
  );
}


/** Breadcrumb plus the page-kind badge. `trailing` names the rendition when the
 *  page is one language's reading of an adaptive page, so a screenshot of the
 *  header is unambiguous about which lens it shows. */
function Breadcrumb({
  slug,
  section,
  trailing,
}: {
  slug: readonly string[];
  section?: string;
  trailing?: string;
}) {
  const crumbs = trailing ? slug : slug.slice(0, -1);
  return (
    <div className="flex items-center gap-3 mb-4">
      <div className="text-[11px] text-ink-faint font-mono tracking-[0.06em] min-w-0 truncate">
        <Link href="/docs" className="hover:text-accent">
          docs
        </Link>
        {crumbs.map((seg, i) => {
          const path = crumbs.slice(0, i + 1).join("/");
          return (
            <span key={path}>
              <span className="text-ink-faint mx-1.5">/</span>
              <Link href={`/docs/${path}`} className="hover:text-accent">
                {seg}
              </Link>
            </span>
          );
        })}
        {trailing ? (
          <span>
            <span className="text-ink-faint mx-1.5">/</span>
            <span className="text-accent">{trailing}</span>
          </span>
        ) : null}
      </div>
      <span className="ml-auto shrink-0">
        <KindBadge section={section} />
      </span>
    </div>
  );
}

export default async function DocPage({ params }: PageProps) {
  const { slug } = await params;
  const resolved = resolveDoc(slug);
  if (!resolved) notFound();

  if (resolved.kind === "folder-index") {
    return (
      <>
        <main className="min-w-0 max-w-[740px]">
          <FolderIndex folder={resolved.folder} />
        </main>
        <TocRail entries={[]} />
      </>
    );
  }

  // Adaptive page: one universal body, composed with the selected lens.
  if (
    resolved.kind === "rendition" ||
    resolved.kind === "boundary" ||
    resolved.kind === "adaptive-router"
  ) {
    const { folder, page } = resolved;
    const lens = resolved.kind === "rendition" ? resolved.lens : undefined;
    const composed =
      lens !== undefined
        ? composeRendition(page, lens)
        : { source: readDocSource(page.shared), hasFragment: false };
    // Prev/next is looked up at the URL the reader is on, not at the page's
    // bare slug — under the lens-prefix shape those are different folders, and
    // the bare slug is not a route at all.
    const neighbours = getPrevNextByLanguage(slug);
    // Under the lens prefix the language is already a path segment, so the
    // breadcrumb reads `docs / sdk / python` and needs no trailing lens label.
    // Under the suffix shape the lens is not in the crumbs, and the label is the
    // only thing making a screenshot of the header unambiguous.
    const lensInPath = folder.projected === true;
    const parity = capabilityRow(page.capability);
    // Published for the floating dock, so switching language on an adaptive
    // page navigates to the reader's rendition instead of only changing which
    // sidebar entries are visible. Computed here rather than pattern-matched in
    // the client because `renditionPath` and `boundaryPath` are the same two
    // functions the chooser uses — one source for "where does this lens live".
    const renditionLinks: Partial<Record<Lens | "c", string>> = {
      ...Object.fromEntries(
        LENSES.map((l) => [l, renditionPath(folder, LENS_SLUG[l])]),
      ),
      c: boundaryPath(folder, page),
    };
    return (
      <>
        <RenditionLinks
          links={renditionLinks}
          current={
            resolved.kind === "boundary" ? "c" : (lens ?? undefined)
          }
        />
        <main className="min-w-0 max-w-[740px]">
          <Breadcrumb
            slug={lensInPath ? slug : folder.slug}
            trailing={
              lensInPath || resolved.kind === "adaptive-router"
                ? undefined
                : lens
                  ? LENS_LABEL[lens]
                  : "C"
            }
            section={folder.slug[0]}
          />
          <DocsPrevNextTop neighbours={neighbours} />
          <DocsContent
            source={composed.source}
            format="md"
            baseDir={lensInPath ? slug.slice(0, -1) : folder.slug}
          />
          {parity ? (
            <ParityRow
              row={parity}
              current={resolved.kind === "boundary" ? "c" : lens}
            />
          ) : null}
          {resolved.kind === "boundary" ? (
            <BoundaryNotice boundary={page.boundary} />
          ) : null}
          {lens !== undefined && !composed.hasFragment ? (
            <AbsenceNotice lens={lens} />
          ) : null}
          <LensChooser
            folder={folder}
            page={page}
            current={resolved.kind === "boundary" ? "c" : lens}
          />
          <DocsPrevNextBottom neighbours={neighbours} />
        </main>
        <TocRail entries={extractToc(composed.source)} />
      </>
    );
  }

  const source = readDocSource(resolved.file);
  const toc = extractToc(source);
  // For folder READMEs the URL uses the folder slug ([..., "plans"]) not
  // the README's own slug ([..., "plans", "readme"]). Detect that via
  // identity — `resolveDoc` returns the same `DocFile` the folder holds as
  // its readme when (and only when) the URL pointed at the folder itself.
  // A bare truthy check on `resolved.folder` would fire for every nested
  // file (the resolver always sets the containing folder) and route
  // prev/next to the README's neighbors instead of the page's own.
  const isFolderReadme = resolved.folder?.readme === resolved.file;
  const lookupSlug = isFolderReadme
    ? resolved.folder!.slug
    : resolved.file.slug;
  const neighbours = getPrevNextByLanguage(lookupSlug);
  return (
    <>
      <main className="min-w-0 max-w-[740px]">
        <Breadcrumb
          slug={resolved.file.slug}
          section={resolved.file.slug[0]}
        />
        <DocsPrevNextTop neighbours={neighbours} />
        <DocsContent
          source={source}
          format={resolved.file.ext}
          baseDir={resolved.file.slug.slice(0, -1)}
        />
        <DocsPrevNextBottom neighbours={neighbours} />
      </main>
      <TocRail entries={toc} />
    </>
  );
}
