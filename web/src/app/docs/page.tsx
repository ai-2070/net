import Link from "next/link";
import {
  getDocTree,
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

// Pure SSG — the /docs landing renders the root README or auto-index;
// content comes from build-time filesystem reads, no runtime work.
export const dynamic = "force-static";
export const revalidate = false;

function folderHref(slug: string[]): string {
  return `/docs/${slug.join("/")}`;
}

function fileHref(slug: string[]): string {
  return `/docs/${slug.join("/")}`;
}

function countDocs(folder: DocFolder): number {
  let n = folder.readme ? 1 : 0;
  for (const c of folder.children) {
    if (c.kind === "file") n += 1;
    else n += countDocs(c);
  }
  return n;
}

// Shared right-rail wrapper. Hidden under xl so the grid collapses to two
// columns at lg; sticky-positioned at xl so it stays visible while scrolling.
function TocRail({ entries }: { entries: readonly TocEntry[] }) {
  return (
    <aside className="hidden xl:block xl:sticky xl:top-24 xl:self-start xl:max-h-[calc(100vh-7rem)] xl:overflow-y-auto pt-1 pr-2">
      <DocsToc entries={entries} />
    </aside>
  );
}

export default function DocsRootPage() {
  const tree = getDocTree();

  if (tree.rootReadme) {
    const source = readDocSource(tree.rootReadme);
    const toc = extractToc(source);
    const { prev, next } = getPrevNext([]);
    return (
      <>
        <main className="min-w-0 max-w-[740px]">
          <DocsPrevNextTop prev={prev} next={next} />
          <DocsContent source={source} format={tree.rootReadme.ext} />
          <DocsPrevNextBottom prev={prev} next={next} />
        </main>
        <TocRail entries={toc} />
      </>
    );
  }

  return (
    <>
      <main className="min-w-0 max-w-[740px]">
        <div>
          <div className="sec-label text-[10px] tracking-[0.2em] text-accent uppercase mb-3">
            ▸ documentation
          </div>
          <h1
            className="font-display text-ink mt-1 mb-8 leading-[1.15] tracking-[0.01em]"
            style={{ fontSize: "clamp(26px, 3vw, 32px)" }}
          >
            Net Docs
          </h1>
          <p className="text-[14px] text-ink-dim leading-[1.7] mb-10 max-w-[640px]">
            Reference, design notes, and release history — mirrored from{" "}
            <code className="font-mono text-accent">net/crates/net/docs/</code>{" "}
            in the source tree.
          </p>

          {tree.folders.length > 0 && (
            <div className="mb-10">
              <h2 className="font-mono text-[14px] tracking-[0.14em] uppercase text-ink-dim mb-4 font-semibold">
                Sections
              </h2>
              <ul className="grid grid-cols-1 md:grid-cols-2 gap-px bg-line border border-line">
                {tree.folders.map((folder) => (
                  <li key={folder.slug.join("/")} className="bg-bg p-5">
                    <Link
                      href={folderHref(folder.slug)}
                      className="block hover:text-accent"
                    >
                      <div className="font-mono text-[16px] text-ink leading-tight mb-1 tracking-[0.02em] font-semibold">
                        {folder.title}
                      </div>
                      <div className="font-mono text-[10px] text-ink-dim tracking-[0.12em] uppercase">
                        {countDocs(folder)} docs
                      </div>
                    </Link>
                  </li>
                ))}
              </ul>
            </div>
          )}

          {tree.rootFiles.length > 0 && (
            <div>
              <h2 className="font-mono text-[14px] tracking-[0.14em] uppercase text-ink-dim mb-4 font-semibold">
                Pages
              </h2>
              <ul className="border-t border-line">
                {tree.rootFiles.map((file) => (
                  <li
                    key={file.slug.join("/")}
                    className="border-b border-line"
                  >
                    <Link
                      href={fileHref(file.slug)}
                      className="flex items-center justify-between py-3 group hover:text-accent transition-colors"
                    >
                      <span className="text-[13px] text-ink-dim group-hover:text-accent">
                        {file.title}
                      </span>
                      <span className="font-mono text-[10px] text-ink-faint tracking-[0.1em]">
                        {file.slug.join("/")}
                      </span>
                    </Link>
                  </li>
                ))}
              </ul>
            </div>
          )}

          {tree.folders.length === 0 && tree.rootFiles.length === 0 && (
            <p className="text-ink-dim text-[13px]">
              No docs found. Run{" "}
              <code className="font-mono text-accent">npm run copy-docs</code>{" "}
              to mirror them from the source tree.
            </p>
          )}
        </div>
      </main>
      {/* No TOC for the auto-index — the page is its own contents. */}
      <TocRail entries={[]} />
    </>
  );
}
