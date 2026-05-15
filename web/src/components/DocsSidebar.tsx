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

function FileRow({
  node,
  active,
  isLast,
  depth,
}: {
  node: ClientDocFile;
  active: string[];
  isLast: boolean;
  depth: number;
}) {
  const on = isActive(node.slug, active);
  const treeChar = isLast ? "└─" : "├─";
  return (
    <Link
      href={slugHref(node.slug)}
      className={`group flex items-center text-[11.5px] leading-[1.55] py-[2px] pr-2 transition-colors ${
        on
          ? "text-accent bg-accent/[0.06]"
          : "text-ink-dim hover:text-ink hover:bg-bg-2/40"
      }`}
      style={{ paddingLeft: `${8 + depth * 12}px` }}
    >
      <span
        className={`shrink-0 mr-1.5 transition-colors ${
          on ? "text-accent" : "text-ink-faint"
        }`}
        aria-hidden
      >
        {on ? "▸ " : `${treeChar} `}
      </span>
      <span className="truncate">{node.title}</span>
    </Link>
  );
}

function NestedFolder({
  folder,
  active,
  depth,
}: {
  folder: ClientDocFolder;
  active: string[];
  depth: number;
}) {
  const within = descendsFrom(folder.slug, active);
  return (
    <div>
      <Link
        href={slugHref(folder.slug)}
        className={`block text-[10px] tracking-[0.14em] uppercase mt-1.5 mb-1 transition-colors ${
          within ? "text-accent" : "text-ink-dim hover:text-ink"
        }`}
        style={{ paddingLeft: `${8 + depth * 12}px` }}
      >
        ▸ {folder.title}
        <span className="text-ink-faint">/</span>
      </Link>
      <div>
        {folder.children.map((child, i, arr) => {
          const last = i === arr.length - 1;
          return child.kind === "file" ? (
            <FileRow
              key={child.slug.join("/")}
              node={child}
              active={active}
              isLast={last}
              depth={depth + 1}
            />
          ) : (
            <NestedFolder
              key={child.slug.join("/")}
              folder={child}
              active={active}
              depth={depth + 1}
            />
          );
        })}
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
    <section className="mb-4">
      <Link
        href={slugHref(folder.slug)}
        className={`group flex items-baseline justify-between mb-1 pl-2 pr-2 transition-colors ${
          within ? "text-accent" : "text-ink-dim hover:text-ink"
        }`}
      >
        <span className="text-[10px] tracking-[0.18em] uppercase">
          <span className="text-accent">▸</span> {folder.title}
          <span className="text-ink-faint">/</span>
        </span>
        <span className="text-[9px] text-ink-faint tracking-[0.1em] group-hover:text-ink-dim">
          [{count}]
        </span>
      </Link>
      <div>
        {folder.children.map((child, i, arr) => {
          const last = i === arr.length - 1;
          if (child.kind === "file") {
            return (
              <FileRow
                key={child.slug.join("/")}
                node={child}
                active={active}
                isLast={last}
                depth={1}
              />
            );
          }
          return (
            <NestedFolder
              key={child.slug.join("/")}
              folder={child}
              active={active}
              depth={1}
            />
          );
        })}
      </div>
    </section>
  );
}

function SidebarBody({
  tree,
  active,
}: {
  tree: ClientDocTree;
  active: string[];
}) {
  return (
    <>
      {/* Fake prompt */}
      <div className="px-2 mb-3 text-[10px] text-ink-faint tracking-[0.06em]">
        <span className="text-accent">$</span> tree --live
      </div>

      {tree.rootFiles.length > 0 && (
        <section className="mb-4">
          <div className="flex items-baseline justify-between mb-1 pl-2 pr-2 text-ink-dim">
            <span className="text-[10px] tracking-[0.18em] uppercase">
              <span className="text-accent">▸</span> overview
              <span className="text-ink-faint">/</span>
            </span>
            <span className="text-[9px] text-ink-faint tracking-[0.1em]">
              [{tree.rootFiles.length}]
            </span>
          </div>
          <div>
            {tree.rootFiles.map((f, i, arr) => (
              <FileRow
                key={f.slug.join("/")}
                node={f}
                active={active}
                isLast={i === arr.length - 1}
                depth={1}
              />
            ))}
          </div>
        </section>
      )}

      {tree.folders.map((folder) => (
        <FolderBlock
          key={folder.slug.join("/")}
          folder={folder}
          active={active}
        />
      ))}
    </>
  );
}

export function DocsSidebar({
  tree,
  chrome = true,
}: {
  tree: ClientDocTree;
  chrome?: boolean;
}) {
  const pathname = usePathname() ?? "/docs";
  const active = activeFromPath(pathname);
  const totalDocs =
    tree.rootFiles.length +
    tree.folders.reduce((sum, f) => sum + countDocs(f), 0) +
    (tree.hasRootReadme ? 1 : 0);

  // Bare-body variant used inside DocsDrawer (the drawer already has its
  // own terminal chrome, so we don't double-frame).
  if (!chrome) {
    return (
      <nav className="font-mono" aria-label="Docs navigation">
        <SidebarBody tree={tree} active={active} />
      </nav>
    );
  }

  return (
    <nav className="font-mono" aria-label="Docs navigation">
      <div className="border border-line bg-bg-2/30 overflow-hidden">
        {/* Terminal title bar */}
        <div className="border-b border-line px-3 py-2 flex items-center justify-between">
          <span className="flex items-center gap-2.5">
            <span className="inline-flex gap-1" aria-hidden>
              <span className="frame-dot-r w-[6px] h-[6px] rounded-full" />
              <span className="frame-dot-y w-[6px] h-[6px] rounded-full" />
              <span className="frame-dot-g w-[6px] h-[6px] rounded-full" />
            </span>
            <span className="text-accent text-[11px] leading-none">$</span>
            <Link
              href="/docs"
              className="text-[10px] tracking-[0.14em] uppercase text-ink-dim hover:text-accent transition-colors"
            >
              net.docs
            </Link>
          </span>
          <span className="text-[9px] tracking-[0.14em] uppercase text-ink-faint">
            {totalDocs} pages
          </span>
        </div>

        {/* Body */}
        <div className="py-3">
          <SidebarBody tree={tree} active={active} />
        </div>

        {/* Status footer */}
        <div className="border-t border-line px-3 py-1.5 flex items-center justify-between">
          <span className="flex items-center gap-1.5">
            <span className="w-1 h-1 rounded-full bg-accent inline-block animate-pulse-dot" />
            <span className="text-[9px] tracking-[0.18em] uppercase text-accent-dim">
              live
            </span>
          </span>
          <span className="text-[9px] tracking-[0.14em] uppercase text-ink-faint">
            v0.17
          </span>
        </div>
      </div>
    </nav>
  );
}
