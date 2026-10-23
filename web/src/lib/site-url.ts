// Single source of truth for the site's absolute origin, shared by the
// sitemap, robots, and root metadataBase so canonical / OG / <loc> URLs all
// agree. No trailing slash.
//
// Order: NEXT_PUBLIC_SITE_URL if set; on a Vercel production deploy, the
// project's production domain; on any other Vercel deploy, that deploy's URL;
// otherwise localhost for dev.
//
// Production must not fall back to VERCEL_URL. That is the per-deploy
// `*.vercel.app` address, and Vercel's deployment protection answers it with
// a login redirect, so Twitter and Discord could not fetch the share images
// it was used to build.
export function siteUrl(): string {
  const explicit = process.env.NEXT_PUBLIC_SITE_URL;
  if (explicit) return explicit.replace(/\/+$/, "");
  const production = process.env.VERCEL_PROJECT_PRODUCTION_URL;
  if (process.env.VERCEL_ENV === "production" && production) {
    return `https://${production}`;
  }
  const vercel = process.env.NEXT_PUBLIC_VERCEL_URL ?? process.env.VERCEL_URL;
  if (vercel) return `https://${vercel}`;
  return "http://localhost:3000";
}
