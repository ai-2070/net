import { statSync } from "node:fs";
import { resolve } from "node:path";
import type { MetadataRoute } from "next";
import { getAllSlugs, resolveDoc } from "@/lib/docs";
import { siteUrl } from "@/lib/site-url";
import globals from "@/lib/globals";

// Static SSG sitemap. Next emits this as /sitemap.xml at build time; every
// docs path is enumerated from the same source of truth the pages use
// (`getAllSlugs`), so the sitemap can never drift from what actually ships.
export const dynamic = "force-static";

const APP_ROOT = resolve(process.cwd(), "src", "app");

// Last-modified time for a file on disk, falling back to `fallback` when
// the stat fails (file missing, or vanished between resolution and stat).
function fileLastModified(absPath: string, fallback: Date): Date {
  try {
    return statSync(absPath).mtime;
  } catch {
    return fallback;
  }
}

// Last-modified for a docs slug, read from the backing file's mtime. Folder
// indexes without a README have no file — fall back to the build time.
function docLastModified(slug: string[], fallback: Date): Date {
  const resolved = resolveDoc(slug);
  if (resolved?.kind === "file") {
    return fileLastModified(resolved.file.filePath, fallback);
  }
  return fallback;
}

export default function sitemap(): MetadataRoute.Sitemap {
  const now = new Date();

  const staticPages: MetadataRoute.Sitemap = [
    {
      url: `${globals.site.href}/`,
      lastModified: fileLastModified(resolve(APP_ROOT, "page.tsx"), now),
      changeFrequency: "weekly",
      priority: 1,
    },
    {
      url: `${globals.site.href}/docs`,
      lastModified: fileLastModified(
        resolve(APP_ROOT, "docs", "page.tsx"),
        now,
      ),
      changeFrequency: "weekly",
      priority: 0.8,
    },
  ];

  const docPages: MetadataRoute.Sitemap = getAllSlugs().map((slug) => ({
    url: `${globals.site.href}/docs/${slug.join("/")}`,
    lastModified: docLastModified(slug, now),
    changeFrequency: "weekly",
    priority: 0.8,
  }));

  return [...staticPages, ...docPages];
}
