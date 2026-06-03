// Single source of truth for the site's absolute origin, shared by the
// sitemap, robots, and root metadataBase so canonical / OG / <loc> URLs all
// agree. Set NEXT_PUBLIC_SITE_URL in prod (e.g. https://net.example.com);
// otherwise fall back to the Vercel deployment URL, then localhost for dev.
// No trailing slash.
export function siteUrl(): string {
  const explicit = process.env.NEXT_PUBLIC_SITE_URL;
  if (explicit) return explicit.replace(/\/+$/, "");
  const vercel = process.env.NEXT_PUBLIC_VERCEL_URL ?? process.env.VERCEL_URL;
  if (vercel) return `https://${vercel}`;
  return "http://localhost:3000";
}
