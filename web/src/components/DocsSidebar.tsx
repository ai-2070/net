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
  // /docs            → []
  // /docs/foo        → ["foo"]
  // /docs/foo/bar    → ["foo", "bar"]
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
      className={`block text-[12px] leading-[1.45] py-[3px] pl-3 border-l transition-colors tracking-[0.02em] truncate ${
        on
          ? "border-accent text-accent"
          : "border-line text-ink-dim hover:text-ink hover:border-accent-dim"
      }`}
    >
      {node.title}
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
  return (
    <div className="ml-3 mt-2">
      <Link
        href={slugHref(folder.slug)}
        className="block text-[10px] tracking-[0.14em] uppercase text-ink-dim hover:text-ink mb-1 font-mono"
      >
        {folder.title}
      </Link>
      <div className="space-y-px">
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
  const folderActive = isActive(folder.slug, active);
  return (
    <div className="mb-5">
      <Link
        href={slugHref(folder.slug)}
        className={`block text-[10px] tracking-[0.18em] uppercase mb-2 font-mono transition-colors ${
          folderActive ? "text-accent" : "text-ink-dim hover:text-ink"
        }`}
      >
        <span className="text-accent">▸</span> {folder.title}
      </Link>
      <div className="space-y-px">
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

export function DocsSidebar({ tree }: { tree: ClientDocTree }) {
  const pathname = usePathname() ?? "/docs";
  const active = activeFromPath(pathname);

  return (
    <nav className="font-mono" aria-label="Docs navigation">
      <Link
        href="/docs"
        className={`block text-[10px] tracking-[0.18em] uppercase mb-4 font-mono transition-colors ${
          active.length === 0 ? "text-accent" : "text-ink-dim hover:text-ink"
        }`}
      >
        <span className="text-accent">▸</span> Docs
      </Link>

      {tree.rootFiles.length > 0 && (
        <div className="mb-5 space-y-px">
          {tree.rootFiles.map((f) => (
            <FileLink key={f.slug.join("/")} node={f} active={active} />
          ))}
        </div>
      )}

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
