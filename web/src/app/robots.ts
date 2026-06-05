import type { MetadataRoute } from "next";
import { siteUrl } from "@/lib/site-url";
import globals from "@/lib/globals";

// Static robots.txt emitted at build time. Allows all crawlers and points
// them at the sitemap so the docs/marketing pages get discovered.
export const dynamic = "force-static";

export default function robots(): MetadataRoute.Robots {
  const base = siteUrl();
  return {
    rules: {
      userAgent: "*",
      allow: "/",
    },
    sitemap: `${globals.site.href}/sitemap.xml`,
    host: base,
  };
}
