import type { NextConfig } from "next";

// Pages that were folded into another page keep their URLs.
//
// The four worldview comparison pages now live as sections of
// `how-net-compares` (`#mcp-and-net`, `#http-and-net`, `#nats-and-net`,
// `#zenoh-and-net`). In-repo links point at the sections directly, but the old
// URLs are addressed from outside this repository — the published skill corpus
// (`net-event-bus/gotchas.md`), the root README, and anything a reader has
// bookmarked — so they redirect rather than 404.
//
// `redirects()` is a Next feature, not a static file: it applies under
// `next dev`, `next build` and on the host. If this site is ever switched to
// `output: "export"`, these four have to become a host-level rule instead.
const MOVED: Record<string, string> = {
  "/docs/worldview/mcp-vs-net": "/docs/worldview/how-net-compares#mcp-and-net",
  "/docs/worldview/rest-vs-net":
    "/docs/worldview/how-net-compares#http-and-net",
  "/docs/worldview/nats-vs-net":
    "/docs/worldview/how-net-compares#nats-and-net",
  "/docs/worldview/zenoh-vs-net":
    "/docs/worldview/how-net-compares#zenoh-and-net",
};

const nextConfig: NextConfig = {
  async redirects() {
    return Object.entries(MOVED).map(([source, destination]) => ({
      source,
      destination,
      permanent: true,
    }));
  },
};

export default nextConfig;
