"use client";

import { useMemo, useState } from "react";
import Link from "next/link";
import { usePathname } from "next/navigation";
import { entryVisibleIn, type Language } from "@/lib/docs-language";
import { useLanguageStore } from "@/store/useLanguageStore";
import type {
  ClientDocFile,
  ClientDocFolder,
  ClientDocNode,
  ClientDocTree,
} from "@/lib/docs";

// A section longer than COLLAPSE_THRESHOLD collapses at rest to its first
// COLLAPSE_TO entries. The threshold is set above the longest hand-curated
// section (Guides, 19) so only genuinely unbounded lists collapse — in
// practice that means Releases, which grows every version.
const COLLAPSE_THRESHOLD = 20;
const COLLAPSE_TO = 8;

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

// Drop entries that aren't visible under `lang`, then drop folders whose
// whole subtree collapsed (no readme + no surviving children). The same
// filter runs on the inline sidebar and the mobile drawer because both
// share this component.
/** True when `slug` is the page being read, or an ancestor of it.
 *
 * Language gating hides pages, and hiding the page the reader is ON strands
 * them: `/docs/sdk/go/invoke` renders fine with `rust` selected while the sidebar
 * has no entry for where they are, so there is nothing to navigate from and no
 * indication anything is missing. A gated entry on the active path is kept
 * regardless of language — the reader followed a link there deliberately, or
 * arrived from a search engine, and either way the nav should admit it exists. */
function onActivePath(slug: readonly string[], active: string): boolean {
  const here = slug.join("/");
  return active === here || active.startsWith(`${here}/`);
}

function filterFolder(
  folder: ClientDocFolder,
  lang: Language,
  active: string,
): ClientDocFolder | null {
  const pinned = onActivePath(folder.slug, active);
  if (!pinned && !entryVisibleIn(folder, lang)) return null;
  const children: ClientDocNode[] = [];
  for (const child of folder.children) {
    if (child.kind === "file") {
      if (entryVisibleIn(child, lang) || onActivePath(child.slug, active)) {
        children.push(child);
      }
    } else {
      const kept = filterFolder(child, lang, active);
      if (kept) children.push(kept);
    }
  }
  if (!folder.hasReadme && children.length === 0) return null;
  return { ...folder, children };
}

function filterTree(
  tree: ClientDocTree,
  lang: Language,
  active: string,
): ClientDocTree {
  const folders: ClientDocFolder[] = [];
  for (const f of tree.folders) {
    const kept = filterFolder(f, lang, active);
    if (kept) folders.push(kept);
  }
  return {
    hasRootReadme: tree.hasRootReadme,
    rootFiles: tree.rootFiles.filter(
      (f) => entryVisibleIn(f, lang) || onActivePath(f.slug, active),
    ),
    folders,
  };
}

// Stable 4-char hex hash per slug — fake inode tag for cyberpunk flavor.
function hashHex4(s: string): string {
  let h = 5381;
  for (let i = 0; i < s.length; i++) {
    h = ((h << 5) + h + s.charCodeAt(i)) >>> 0;
  }
  return (h & 0xffff).toString(16).padStart(4, "0");
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
  // const tag = hashHex4(node.slug.join("/"));

  return (
    <Link
      href={slugHref(node.slug)}
      className={`group flex items-center text-[11px] leading-[1.55] py-[2px] pr-2 transition-colors ${
        on
          ? "text-accent bg-accent/[0.07]"
          : "text-ink-dim hover:text-ink hover:bg-bg-2/40"
      }`}
      style={{ paddingLeft: `${6 + depth * 12}px` }}
    >
      <span
        className={`shrink-0 mr-1.5 transition-colors ${
          on ? "text-accent" : "text-ink-faint"
        }`}
        aria-hidden
      >
        {on ? "▸" : treeChar}
      </span>
      <span className="truncate flex-1">{node.title}</span>
      {on ? (
        <span
          aria-hidden
          className="text-accent ml-1 animate-pulse-dot leading-none"
        >
          █
        </span>
      ) : null}
      {/*<span
        className={`shrink-0 ml-2 text-[9px] tracking-[0.04em] tabular-nums transition-colors ${
          on ? "text-accent-dim" : "text-ink-faint group-hover:text-ink-dim"
        }`}
        aria-hidden
      >
        ·{tag}
      </span>*/}
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
        style={{ paddingLeft: `${6 + depth * 12}px` }}
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
  index,
}: {
  folder: ClientDocFolder;
  active: string[];
  index: number; // 1-based section number, for the §NN prefix
}) {
  const within = descendsFrom(folder.slug, active);
  const count = countDocs(folder);

  // Releases is 35 flat entries and dominated the sidebar, pushing every
  // other section below the fold. Long sections collapse to their first
  // few — newest first, which is the order that matters for releases — with
  // the rest one click away. Nothing is hidden from routing or search; this
  // is purely how much of a long list is shown at rest.
  const overflows = folder.children.length > COLLAPSE_THRESHOLD;
  // If the page you're on lives in the hidden tail, don't hide it — a
  // sidebar that can't show you where you are is worse than a long one.
  const activeIsHidden =
    overflows &&
    folder.children
      .slice(COLLAPSE_TO)
      .some((c) => isActive(c.slug, active) || descendsFrom(c.slug, active));
  const [expanded, setExpanded] = useState(false);
  const collapsed = overflows && !expanded && !activeIsHidden;
  const shown = collapsed
    ? folder.children.slice(0, COLLAPSE_TO)
    : folder.children;
  const hiddenCount = collapsed ? folder.children.length - COLLAPSE_TO : 0;

  return (
    <section className="mb-4">
      <Link
        href={slugHref(folder.slug)}
        className={`group flex items-baseline justify-between mb-1 pl-2 pr-2 transition-colors ${
          within ? "text-accent" : "text-ink-dim hover:text-ink"
        }`}
      >
        <span className="text-[10px] tracking-[0.18em] uppercase flex items-center gap-1.5">
          <span className="text-accent-dim group-hover:text-accent">
            §{String(index).padStart(2, "0")}
          </span>
          <span className="text-accent">▸</span>
          {folder.title}
          <span className="text-ink-faint">/</span>
        </span>
        <span className="text-[9px] text-ink-faint tracking-[0.1em] group-hover:text-ink-dim tabular-nums">
          [{String(count).padStart(2, "0")}]
        </span>
      </Link>
      {/* hard divider under the section header */}
      <div
        aria-hidden
        className="text-[8px] text-line tracking-[0.05em] leading-none px-2 mb-1 select-none overflow-hidden whitespace-nowrap"
      >
        ════════════════════════════════════════════
      </div>
      <div>
        {shown.map((child, i) => {
          const last = i === shown.length - 1 && !hiddenCount;
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
        {hiddenCount > 0 ? (
          <button
            type="button"
            onClick={() => setExpanded(true)}
            className="w-full text-left pl-2 pr-2 py-0.5 text-[10px] tracking-[0.12em] uppercase font-mono text-ink-faint hover:text-accent transition-colors"
          >
            <span className="text-accent-dim">⋯</span> show {hiddenCount} more
          </button>
        ) : null}
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
  // Section indexing: §01 = overview (root files), §02..N = top-level folders.
  return (
    <>
      {/* Fake prompt */}
      <div className="px-2 mb-3 text-[10px] text-ink-faint tracking-[0.06em]">
        <span className="text-accent">$</span> tree --live
        <span className="text-accent ml-1 animate-pulse-dot">█</span>
      </div>

      {tree.rootFiles.length > 0 && (
        <section className="mb-4">
          <div className="flex items-baseline justify-between mb-1 pl-2 pr-2 text-ink-dim">
            <span className="text-[10px] tracking-[0.18em] uppercase flex items-center gap-1.5">
              <span className="text-accent-dim">§01</span>
              <span className="text-accent">▸</span>
              overview
              <span className="text-ink-faint">/</span>
            </span>
            <span className="text-[9px] text-ink-faint tracking-[0.1em] tabular-nums">
              [{String(tree.rootFiles.length).padStart(2, "0")}]
            </span>
          </div>
          <div
            aria-hidden
            className="text-[8px] text-line tracking-[0.05em] leading-none px-2 mb-1 select-none overflow-hidden whitespace-nowrap"
          >
            ════════════════════════════════════════════
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

      {tree.folders.map((folder, i) => (
        <FolderBlock
          key={folder.slug.join("/")}
          folder={folder}
          active={active}
          index={i + 2 /* §01 was overview */}
        />
      ))}
    </>
  );
}

export function DocsSidebar({
  tree,
  chrome = true,
  version,
}: {
  tree: ClientDocTree;
  chrome?: boolean;
  /** Derived from the newest release note by `getDocsVersion()`. This footer
   *  used to print a hardcoded `v0.17` on all 149 pages while the newest
   *  release page was v0.33 — a stale claim nothing pointed at. */
  version?: string;
}) {
  const pathname = usePathname() ?? "/docs";
  const active = activeFromPath(pathname);
  const language = useLanguageStore((s) => s.language);
  const visibleTree = useMemo(
    () => filterTree(tree, language, active.join("/")),
    [tree, language, active],
  );
  const totalDocs =
    visibleTree.rootFiles.length +
    visibleTree.folders.reduce((sum, f) => sum + countDocs(f), 0) +
    (visibleTree.hasRootReadme ? 1 : 0);

  if (!chrome) {
    return (
      <nav className="font-mono" aria-label="Docs navigation">
        <SidebarBody tree={visibleTree} active={active} />
      </nav>
    );
  }

  return (
    <nav className="font-mono" aria-label="Docs navigation">
      <div className="border border-line bg-bg-2/30 overflow-hidden">
        {/* Title bar: traffic dots, $ prompt, page count */}
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
          <span className="text-[9px] tracking-[0.14em] uppercase text-ink-faint tabular-nums">
            {String(totalDocs).padStart(2, "0")} pages
          </span>
        </div>

        {/* Body */}
        <div className="py-3">
          <SidebarBody tree={visibleTree} active={active} />
        </div>

        {/* Status footer — vim-like key hints + live + version */}
        <div className="border-t border-line">
          <div className="px-3 py-1.5 flex items-center justify-between gap-2">
            <span className="flex items-center gap-1.5">
              <span className="w-1 h-1 rounded-full bg-accent inline-block animate-pulse-dot" />
              <span className="text-[9px] tracking-[0.18em] uppercase text-accent-dim">
                live
              </span>
            </span>
            <span
              className="text-[9px] tracking-[0.06em] text-ink-faint hidden sm:block"
              aria-hidden
            >
              <span className="text-accent-dim">j</span>↓{" "}
              <span className="text-accent-dim">k</span>↑{" "}
              <span className="text-accent-dim">/</span>find
            </span>
            <span className="text-[9px] tracking-[0.14em] uppercase text-ink-faint">
              {version}
            </span>
          </div>
        </div>
      </div>
    </nav>
  );
}
