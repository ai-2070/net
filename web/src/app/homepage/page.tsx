import { JSX } from "react";
import type { Metadata } from "next";
import { PageContainer } from "@/components/PageContainer";
import { FooterDivider } from "@/components/FooterDivider";
import { Footer } from "@/components/Footer";
import { SeedBanner } from "@/components/SeedBanner";
import { VcNav } from "./_sections/VcNav";
import { VcHero } from "./_sections/VcHero";
import { WhyNowSection } from "./_sections/WhyNowSection";
import { TheGapSection } from "./_sections/TheGapSection";
import { WhatNetIsSection } from "./_sections/WhatNetIsSection";
import { CapabilitiesSection } from "./_sections/CapabilitiesSection";
import { EndStateSection } from "./_sections/EndStateSection";
import { UseCasesSection } from "./_sections/UseCasesSection";
import { AuthoritySection } from "./_sections/AuthoritySection";
import { SubnetsSection } from "./_sections/SubnetsSection";
import { VentureScaleSection } from "./_sections/VentureScaleSection";
import { FitCheckSection } from "./_sections/FitCheckSection";
import { VcCtaSection } from "./_sections/VcCtaSection";

export const metadata: Metadata = {
  title:
    "NET — The real-time substrate for agents that operate beyond one machine.",
  description:
    "Agents are leaving chat and becoming operators. NET is the coordination substrate for agents, devices, services, GPUs, streams, and artifacts that need to keep moving when the link goes dark. Discovery, typed capabilities, nRPC, artifacts, streams, durable tasks, claims, subnets, and local authority on one real-time mesh.",
};

export default function Homepage(): JSX.Element {
  return (
    <PageContainer>
      <VcNav />
      <main className="pt-20 max-w-[1440px] mx-auto">
        <SeedBanner />
        <VcHero />
        <WhyNowSection />
        <TheGapSection />
        <WhatNetIsSection />
        <CapabilitiesSection />
        <EndStateSection />
        <UseCasesSection />
        <AuthoritySection />
        <SubnetsSection />
        <VentureScaleSection />
        <FitCheckSection />
        <VcCtaSection />
        <FooterDivider />
        <Footer />
      </main>
    </PageContainer>
  );
}
