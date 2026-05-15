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

function FolderIndex({ folder }: { folder: DocFolder }) {
  return (
    <div>
      <div className="sec-label text-[10px] tracking-[0.2em] text-accent uppercase mb-3">
        ▸ section
      </div>
      <h1
        className="font-mono text-ink mb-3 leading-[1.15] tracking-[0.02em] font-semibold"
        style={{ fontSize: "clamp(28px, 3.4vw, 40px)" }}
      >
        {folder.title}
      </h1>
      <p className="text-[12px] text-ink-faint font-mono tracking-[0.06em] mb-8">
        /docs/{folder.slug.join("/")}
      </p>
      {folder.children.length === 0 ? (
        <p className="text-ink-dim text-[13px]">
          No documents in this section.
        </p>
      ) : (
        <ul className="border-t border-line">
          {folder.children.map((child) => (
            <li key={child.slug.join("/")} className="border-b border-line">
              <Link
                href={`/docs/${child.slug.join("/")}`}
                className="flex items-center justify-between py-3 group hover:text-accent transition-colors"
              >
                <span className="text-[13px] text-ink-dim group-hover:text-accent">
                  {child.kind === "folder" ? `▸ ${child.title}` : child.title}
                </span>
                <span className="font-mono text-[10px] text-ink-faint tracking-[0.1em]">
                  {child.kind === "folder" ? "section" : "doc"}
                </span>
              </Link>
            </li>
          ))}
        </ul>
      )}
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
