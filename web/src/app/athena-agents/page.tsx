import type { Metadata } from "next";
import globals from "@/lib/globals";
import { jsonLd, pageMeta } from "@/lib/page-meta";
import { ATHENA_DOWNLOAD_URL } from "@/components/athena/links";
import { JSX } from "react";
import { NavBar } from "@/components/NavBar";
import { PageContainer } from "@/components/PageContainer";
import { Footer } from "@/components/Footer";
import { AthenaHero } from "@/components/athena/AthenaHero";
import { AthenaLibrarySection } from "@/components/athena/AthenaLibrary";
import {
  AthenaBrainSection,
  AthenaChatsSection,
  AthenaEnginesSection,
  AthenaGetSection,
  AthenaLearnSection,
  AthenaLocalSection,
  AthenaProgramsSection,
  AthenaReviewSection,
  AthenaStationSection,
  AthenaWorkSection,
} from "@/components/athena/AthenaSections";

export const metadata: Metadata = pageMeta({
  title: "Athena Agents — a home for your AI agents",
  description:
    "Build teams of AI agents, give each one a job, a model and the tools it needs, and watch them work together on a live map. Runs on your own computer, with no account and no login.",
  path: "/athena-agents",
});

// Tells search engines Athena Agents is a free desktop app.
const STRUCTURED = {
  "@context": "https://schema.org",
  "@type": "SoftwareApplication",
  name: "Athena Agents",
  description:
    "A home for your AI agents: build teams, give each agent a job, a model and the tools it needs, and watch them work together on a live map. Runs on your own computer.",
  applicationCategory: "DeveloperApplication",
  operatingSystem: "Windows, macOS, Linux",
  softwareVersion: "1.0",
  url: `${globals.site.href}/athena-agents`,
  downloadUrl: ATHENA_DOWNLOAD_URL,
  offers: { "@type": "Offer", price: "0", priceCurrency: "USD" },
};

export default function AthenaAgentsPage(): JSX.Element {
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
        <AthenaHero />
        <AthenaStationSection />
        <AthenaBrainSection />
        <AthenaLibrarySection />
        <AthenaEnginesSection />
        <AthenaChatsSection />
        <AthenaWorkSection />
        <AthenaReviewSection />
        <AthenaLearnSection />
        <AthenaProgramsSection />
        <AthenaLocalSection />
        <AthenaGetSection />
        <Footer world="field" page="athena" />
      </main>
    </PageContainer>
  );
}
