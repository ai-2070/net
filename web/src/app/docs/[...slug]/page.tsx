import Link from "next/link";
import { notFound } from "next/navigation";
import {
  getAllSlugs,
  resolveDoc,
  readDocSource,
  extractToc,
  getPrevNext,
  type DocFolder,
  type TocEntry,
} from "@/lib/docs";
import { DocsContent } from "@/components/DocsContent";
import { DocsToc } from "@/components/DocsToc";
import {
  DocsPrevNextTop,
  DocsPrevNextBottom,
} from "@/components/DocsPrevNext";

interface PageProps {
  params: Promise<{ slug: string[] }>;
}

export function generateStaticParams(): Array<{ slug: string[] }> {
  return getAllSlugs().map((slug) => ({ slug }));
}

export async function generateMetadata({ params }: PageProps) {
  const { slug } = await params;
  const resolved = resolveDoc(slug);
  if (!resolved) return { title: "Not Found · Docs · Net" };
  const title =
    resolved.kind === "file" ? resolved.file.title : resolved.folder.title;
  return { title: `${title} · Docs · Net` };
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
  const childCount = folder.children.length;

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
          {folder.children.map((child, i) => {
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

  const source = readDocSource(resolved.file);
  const toc = extractToc(source);
  // For folder READMEs the URL uses the folder slug ([..., "plans"]) not
  // the README's own slug ([..., "plans", "readme"]). Pick the right one
  // so prev/next maps to the same key used by the sidebar order.
  const lookupSlug = resolved.folder ? resolved.folder.slug : resolved.file.slug;
  const { prev, next } = getPrevNext(lookupSlug);
  return (
    <>
      <main className="min-w-0 max-w-[740px]">
        <div className="text-[11px] text-ink-faint font-mono mb-4 tracking-[0.06em]">
          <Link href="/docs" className="hover:text-accent">
            docs
          </Link>
          {resolved.file.slug.slice(0, -1).map((seg, i) => {
            const path = resolved.file.slug.slice(0, i + 1).join("/");
            return (
              <span key={path}>
                <span className="text-ink-faint mx-1.5">/</span>
                <Link href={`/docs/${path}`} className="hover:text-accent">
                  {seg}
                </Link>
              </span>
            );
          })}
        </div>
        <DocsPrevNextTop prev={prev} next={next} />
        <DocsContent source={source} format={resolved.file.ext} />
        <DocsPrevNextBottom prev={prev} next={next} />
      </main>
      <TocRail entries={toc} />
    </>
  );
}
