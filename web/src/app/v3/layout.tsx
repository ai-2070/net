import type { Metadata } from "next";

// A design rendition under review, not a second home page for search.
export const metadata: Metadata = {
  title: "NET (v3) — Network Event Transport. A latency-first encrypted mesh.",
  robots: { index: false, follow: false },
};

export default function V3Layout({ children }: { children: React.ReactNode }) {
  return children;
}
