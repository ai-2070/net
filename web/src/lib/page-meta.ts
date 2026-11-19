import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import type { Metadata } from "next";

// How the brand is written in titles and share cards.
export const SITE_NAME = "NET";

// The site's own share image (src/app/opengraph-image.png and its alt text).
// Next.js drops the inherited file image as soon as a nested page sets its
// own openGraph, so pages without an image of their own (the docs) name it.
const SITE_IMAGE_ALT = readFileSync(
  resolve(process.cwd(), "src", "app", "opengraph-image.alt.txt"),
  "utf8",
).trim();
export const SITE_IMAGES = {
  openGraph: [
    {
      url: "/opengraph-image.png",
      width: 1200,
      height: 630,
      alt: SITE_IMAGE_ALT,
    },
  ],
  twitter: [{ url: "/twitter-image.png", alt: SITE_IMAGE_ALT }],
};

// The full set of search and share tags for a page: title, description,
// canonical URL, Open Graph and the X card. Paths are relative; the root
// layout's metadataBase makes them absolute.
//
// Next.js replaces (does not merge) `openGraph` and `twitter` per page, so
// every page builds both here instead of relying on the root layout. Share
// images still come from each folder's opengraph-image / twitter-image file.
export function pageMeta({
  title,
  description,
  path,
  type = "website",
  siteImage = false,
}: {
  title: string;
  description: string;
  path: string;
  type?: "website" | "article";
  /** Use the site's share image (for pages with no image file of their own). */
  siteImage?: boolean;
}): Metadata {
  return {
    title,
    description,
    alternates: { canonical: path },
    openGraph: {
      type,
      siteName: SITE_NAME,
      locale: "en_US",
      title,
      description,
      url: path,
      ...(siteImage ? { images: SITE_IMAGES.openGraph } : {}),
    },
    twitter: {
      card: "summary_large_image",
      title,
      description,
      ...(siteImage ? { images: SITE_IMAGES.twitter } : {}),
    },
  };
}

// Structured data for search engines, as a <script type="application/ld+json">
// body. `<` is escaped so the JSON can never close the script tag early.
export function jsonLd(data: Record<string, unknown>): { __html: string } {
  return { __html: JSON.stringify(data).replace(/</g, "\\u003c") };
}
