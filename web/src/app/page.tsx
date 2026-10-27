import type { Metadata } from "next";
import { JSX } from "react";
import { HomePage } from "@/components/HomePage";
import globals from "@/lib/globals";
import { jsonLd, pageMeta, SITE_NAME } from "@/lib/page-meta";

export const metadata: Metadata = pageMeta({
  title: "NET — Network Event Transport. A latency-first encrypted mesh.",
  description:
    "NET is a latency-first encrypted mesh where every device is an equal node. Nanosecond scheduling, zero-copy forwarding, capability-based routing. Open source.",
  path: "/",
});

// Tells search engines what the site is and what NET is.
const STRUCTURED = {
  "@context": "https://schema.org",
  "@graph": [
    {
      "@type": "WebSite",
      name: SITE_NAME,
      url: globals.site.href,
    },
    {
      "@type": "SoftwareSourceCode",
      name: "NET (Network Event Transport)",
      description:
        "A latency-first encrypted mesh network protocol. Every node is an equal peer on a flat topology.",
      codeRepository: "https://github.com/ai-2070/net",
      programmingLanguage: ["Rust", "TypeScript", "Python", "Go", "C"],
      // dual licensed: MIT OR Apache-2.0
      license: [
        "https://opensource.org/licenses/MIT",
        "https://www.apache.org/licenses/LICENSE-2.0",
      ],
      url: globals.site.href,
    },
  ],
};

export default function Home(): JSX.Element {
  return (
    <>
      <script
        type="application/ld+json"
        dangerouslySetInnerHTML={jsonLd(STRUCTURED)}
      />
      <HomePage />
    </>
  );
}
