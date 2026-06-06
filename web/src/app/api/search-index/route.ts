import { NextResponse } from "next/server";
import { buildSearchIndex } from "@/lib/search-index";

// Build-time static route: Next bakes the response at `next build` and
// ships it as a static asset. The docs search modal fetches it lazily on
// first open. Cache-Control is default-immutable for SSG outputs.
export const dynamic = "force-static";

export function GET() {
  return NextResponse.json(buildSearchIndex());
}
