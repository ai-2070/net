import type { Metadata } from "next";
import globals from "@/lib/globals";
import { jsonLd, pageMeta } from "@/lib/page-meta";
import { BLACKSITE_DOWNLOAD_URL } from "@/components/blacksite/links";
import { JSX } from "react";
import { NavBar } from "@/components/NavBar";
import { PageContainer } from "@/components/PageContainer";
import { Footer } from "@/components/Footer";
import { BlacksiteHero } from "@/components/blacksite/BlacksiteHero";
import { BlacksiteLibrarySection } from "@/components/blacksite/BlacksiteLibrary";
import {
  BlacksiteBrainSection,
  BlacksiteChatsSection,
  BlacksiteEnginesSection,
  BlacksiteGetSection,
  BlacksiteLocalSection,
  BlacksiteProgramsSection,
  BlacksiteReviewSection,
  BlacksiteStationSection,
} from "@/components/blacksite/BlacksiteSections";

export const metadata: Metadata = pageMeta({
  title: "Blacksite — run AI agents and teams from one local workspace",
  description:
    "Run AI agents and teams from one local workspace. Hire from 270+ specialists, pick each agent's model and tools, and watch them work on a live 3D map.",
  path: "/blacksite",
});

// Tells search engines Blacksite is a free desktop app.
const STRUCTURED = {
  "@context": "https://schema.org",
  "@type": "SoftwareApplication",
  name: "Blacksite",
  description:
    "A local dashboard for AI agents and teams: hire specialists from a library, give each its own model, effort, tools and memory, and watch them work on a live 3D map.",
  applicationCategory: "DeveloperApplication",
  operatingSystem: "Windows, macOS, Linux",
  softwareVersion: "0.1",
  url: `${globals.site.href}/blacksite`,
  downloadUrl: BLACKSITE_DOWNLOAD_URL,
  offers: { "@type": "Offer", price: "0", priceCurrency: "USD" },
};

export default function BlacksitePage(): JSX.Element {
  return (
    <PageContainer className="world-field world-notebook field-grain">
      <script
        type="application/ld+json"
        dangerouslySetInnerHTML={jsonLd(STRUCTURED)}
      />
      <NavBar world="field" />
      {/* The same notebook as the landing page and /games: each section is
          its own ruled box, the hero is the open hall, the footer its own
          band. */}
      <main className="pt-[84px] max-w-[1440px] mx-auto">
        <BlacksiteHero />
        <BlacksiteStationSection />
        <BlacksiteBrainSection />
        <BlacksiteLibrarySection />
        <BlacksiteEnginesSection />
        <BlacksiteChatsSection />
        <BlacksiteProgramsSection />
        <BlacksiteReviewSection />
        <BlacksiteLocalSection />
        <BlacksiteGetSection />
        <Footer world="field" page="blacksite" />
      </main>
    </PageContainer>
  );
}
