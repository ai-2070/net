import Link from "next/link";
import { getDocTree, readDocSource, type DocFolder } from "@/lib/docs";
import { DocsContent } from "@/components/DocsContent";

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

export default function DocsRootPage() {
  const tree = getDocTree();

  if (tree.rootReadme) {
    const source = readDocSource(tree.rootReadme);
    return <DocsContent source={source} format={tree.rootReadme.ext} />;
  }

  return (
    <div>
      <div className="sec-label text-[10px] tracking-[0.2em] text-accent uppercase mb-3">
        ▸ documentation
      </div>
      <h1
        className="font-display text-ink mb-7 leading-[1.05] tracking-[-0.01em]"
        style={{ fontSize: "clamp(32px, 4vw, 48px)" }}
      >
        Net Docs
      </h1>
      <p className="text-[14px] text-ink-dim leading-[1.7] mb-10 max-w-[640px]">
        Reference, design notes, and release history — mirrored from{" "}
        <code className="font-mono text-accent">net/crates/net/docs/</code> in
        the source tree.
      </p>

      {tree.folders.length > 0 && (
        <div className="mb-10">
          <h2 className="font-head text-[14px] tracking-[0.14em] uppercase text-ink-dim mb-4">
            Sections
          </h2>
          <ul className="grid grid-cols-1 md:grid-cols-2 gap-px bg-line border border-line">
            {tree.folders.map((folder) => (
              <li key={folder.slug.join("/")} className="bg-bg p-5">
                <Link
                  href={folderHref(folder.slug)}
                  className="block hover:text-accent"
                >
                  <div className="font-head text-[18px] text-ink leading-tight mb-1 tracking-[0.02em] lowercase">
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
          <h2 className="font-head text-[14px] tracking-[0.14em] uppercase text-ink-dim mb-4">
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
          <code className="font-mono text-accent">npm run copy-docs</code> to
          mirror them from the source tree.
        </p>
      )}
    </div>
  );
}
