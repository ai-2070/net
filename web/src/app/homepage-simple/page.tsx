import { JSX } from "react";
import type { Metadata } from "next";
import { PageContainer } from "@/components/PageContainer";
import { FooterDivider } from "@/components/FooterDivider";
import { Footer } from "@/components/Footer";
import { SeedBanner } from "@/components/SeedBanner";
import { SimpleNav } from "./_sections/SimpleNav";
import { SimpleHero } from "./_sections/SimpleHero";
import { ProblemSection } from "./_sections/ProblemSection";
import { WhatNetDoesSection } from "./_sections/WhatNetDoesSection";
import { SimpleExampleSection } from "./_sections/SimpleExampleSection";
import { CapabilityCardsSection } from "./_sections/CapabilityCardsSection";
import { CompanyOpportunitySection } from "./_sections/CompanyOpportunitySection";
import { UseCasesSection } from "./_sections/UseCasesSection";
import { ControlSection } from "./_sections/ControlSection";
import { BigOutcomeSection } from "./_sections/BigOutcomeSection";
import { HowFastSection } from "./_sections/HowFastSection";
import { InstallSection } from "@/components/InstallSection";
import { ReleasesSection } from "@/components/ReleasesSection";
import { SimpleCtaSection } from "./_sections/SimpleCtaSection";

export const metadata: Metadata = {
  title:
    "NET — The operating layer that lets AI agents work across real machines.",
  description:
    "AI agents are starting to do real work. Net is the operating layer that lets them find, use, and coordinate the machines, tools, files, apps, and compute around them — while each resource keeps control over what it allows. The shared infrastructure layer for autonomous work.",
};

export default function HomepageSimple(): JSX.Element {
  return (
    <PageContainer>
      <SimpleNav />
      <main className="pt-20 max-w-[1440px] mx-auto">
        <SeedBanner />
        <SimpleHero />
        <ProblemSection />
        <UseCasesSection />
        <WhatNetDoesSection />
        <SimpleExampleSection />
        <CapabilityCardsSection />
        <CompanyOpportunitySection />
        <ControlSection />
        <BigOutcomeSection />
        <HowFastSection />
        <InstallSection id="install" label="§10 / try it yourself" />
        <ReleasesSection id="releases" label="§11 / releases" />
        <SimpleCtaSection />
        <FooterDivider />
        <Footer />
      </main>
    </PageContainer>
  );
}
