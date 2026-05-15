"use client";

import Link from "next/link";
import { usePathname } from "next/navigation";
import type {
  ClientDocFile,
  ClientDocFolder,
  ClientDocTree,
} from "@/lib/docs";

function slugHref(slug: string[]): string {
  return slug.length === 0 ? "/docs" : `/docs/${slug.join("/")}`;
}

function activeFromPath(pathname: string): string[] {
  if (!pathname.startsWith("/docs")) return [];
  const rest = pathname.slice("/docs".length).replace(/^\/+|\/+$/g, "");
  return rest === "" ? [] : rest.split("/");
}

function isActive(slug: string[], active: string[]): boolean {
  if (slug.length !== active.length) return false;
  for (let i = 0; i < slug.length; i++) {
    if (slug[i] !== active[i]) return false;
  }
  return true;
}

function descendsFrom(slug: string[], active: string[]): boolean {
  if (active.length < slug.length) return false;
  for (let i = 0; i < slug.length; i++) {
    if (slug[i] !== active[i]) return false;
  }
  return true;
}

function countDocs(folder: ClientDocFolder): number {
  let n = folder.hasReadme ? 1 : 0;
  for (const c of folder.children) {
    if (c.kind === "file") n += 1;
    else n += countDocs(c);
  }
  return n;
}

function FileLink({
  node,
  active,
}: {
  node: ClientDocFile;
  active: string[];
}) {
  const on = isActive(node.slug, active);
  return (
    <Link
      href={slugHref(node.slug)}
      className={`group flex items-center gap-2 text-[12px] leading-[1.45] py-[5px] pl-3 pr-2 border-l transition-colors tracking-[0.02em] truncate ${
        on
          ? "border-accent text-accent bg-accent/[0.06]"
          : "border-line text-ink-dim hover:text-ink hover:border-accent-dim hover:bg-bg-2/40"
      }`}
    >
      <span
        className={`shrink-0 transition-opacity ${
          on ? "text-accent opacity-100" : "opacity-0 group-hover:opacity-60"
        }`}
        aria-hidden
      >
        ▸
      </span>
      <span className="truncate">{node.title}</span>
    </Link>
  );
}

function NestedFolder({
  folder,
  active,
}: {
  folder: ClientDocFolder;
  active: string[];
}) {
  const within = descendsFrom(folder.slug, active);
  return (
    <div className="ml-3 mt-2">
      <Link
        href={slugHref(folder.slug)}
        className={`block text-[10px] tracking-[0.14em] uppercase mb-1 font-mono transition-colors ${
          within ? "text-accent" : "text-ink-dim hover:text-ink"
        }`}
      >
        {folder.title}
      </Link>
      <div className="space-y-[1px]">
        {folder.children.map((child) =>
          child.kind === "file" ? (
            <FileLink
              key={child.slug.join("/")}
              node={child}
              active={active}
            />
          ) : (
            <NestedFolder
              key={child.slug.join("/")}
              folder={child}
              active={active}
            />
          ),
        )}
      </div>
    </div>
  );
}

function FolderBlock({
  folder,
  active,
}: {
  folder: ClientDocFolder;
  active: string[];
}) {
  const within = descendsFrom(folder.slug, active);
  const count = countDocs(folder);
  return (
    <section className="mb-6">
      <Link
        href={slugHref(folder.slug)}
        className={`group flex items-baseline justify-between mb-2 font-mono transition-colors ${
          within ? "text-accent" : "text-ink-dim hover:text-ink"
        }`}
      >
        <span className="text-[10px] tracking-[0.18em] uppercase flex items-center gap-1.5">
          <span className="text-accent">▸</span> {folder.title}
        </span>
        <span className="text-[9px] tracking-[0.1em] text-ink-faint group-hover:text-ink-dim">
          {count}
        </span>
      </Link>
      <div className="space-y-[1px]">
        {folder.children.map((child) =>
          child.kind === "file" ? (
            <FileLink
              key={child.slug.join("/")}
              node={child}
              active={active}
            />
          ) : (
            <NestedFolder
              key={child.slug.join("/")}
              folder={child}
              active={active}
            />
          ),
        )}
      </div>
    </section>
  );
}

export function DocsSidebar({ tree }: { tree: ClientDocTree }) {
  const pathname = usePathname() ?? "/docs";
  const active = activeFromPath(pathname);
  const rootOn = active.length === 0;
  const totalDocs =
    tree.rootFiles.length +
    tree.folders.reduce((sum, f) => sum + countDocs(f), 0) +
    (tree.hasRootReadme ? 1 : 0);

  return (
    <nav className="font-mono" aria-label="Docs navigation">
      {/* Header chrome — homepage-style section label. */}
      <div className="mb-6 pb-4 border-b border-dashed border-line">
        <Link
          href="/docs"
          className={`flex items-baseline justify-between mb-1.5 transition-colors ${
            rootOn ? "text-accent" : "text-ink hover:text-accent"
          }`}
        >
          <span className="text-[11px] tracking-[0.22em] uppercase flex items-center gap-2 font-mono">
            <span className="text-accent">§</span> net docs
          </span>
          <span className="text-[9px] tracking-[0.14em] text-ink-faint uppercase">
            {totalDocs} pages
          </span>
        </Link>
        <div className="text-[9px] text-ink-faint tracking-[0.06em] flex items-center gap-1.5">
          <span className="w-1 h-1 rounded-full bg-accent inline-block animate-pulse-dot" />
          <span>live documentation</span>
        </div>
      </div>

      {/* Root files render as an "overview" section, without a folder. */}
      {tree.rootFiles.length > 0 && (
        <section className="mb-6">
          <div className="text-[10px] tracking-[0.18em] uppercase text-ink-dim mb-2 font-mono">
            <span className="text-accent">▸</span> overview
          </div>
          <div className="space-y-[1px]">
            {tree.rootFiles.map((f) => (
              <FileLink key={f.slug.join("/")} node={f} active={active} />
            ))}
          </div>
        </section>
      )}

      {/* Top-level folders as sections. */}
      {tree.folders.map((folder) => (
        <FolderBlock
          key={folder.slug.join("/")}
          folder={folder}
          active={active}
        />
      ))}
    </nav>
  );
}
