"use client";

import { JSX } from "react";
import { MeshOsSection } from "@/components/MeshOsSection";
// import { SeedBanner } from "@/components/SeedBanner";
import { HeroSection } from "@/components/HeroSection";
import { MikoshiSection } from "@/components/MikoshiSection";
import { DatafortsSection } from "@/components/DatafortsSection";
import { ComponentsSection } from "@/components/ComponentsSection";
import { InstallSection } from "@/components/InstallSection";
import { ApplicationsSection } from "@/components/ApplicationsSection";
import { ReleasesSection } from "@/components/ReleasesSection";
import { ClosingSection } from "@/components/ClosingSection";
import { Footer } from "@/components/Footer";
import { NavBar } from "@/components/NavBar";
import { BlackwallSection } from "@/components/BlackwallSection";
import { ComputeRuntimeSection } from "@/components/ComputeRuntimeSection";
import { BenchmarksSection } from "@/components/BenchmarksSection";
import { PropertiesSection } from "@/components/PropertiesSection";
import { WhyNotBestEffortSection } from "@/components/WhyNotBestEffortSection";
import { TopologyClassesSection } from "@/components/TopologyClassesSection";
import { PageContainer } from "@/components/PageContainer";
import { WorldVariantProvider } from "@/components/field/WorldVariant";

// /v3: the /v2 lab-notebook rendition set in the master homepage's type:
// Major Mono Display for titles, JetBrains Mono for everything else.
export default function HomeV3(): JSX.Element {
  return (
    <WorldVariantProvider value="v3">
      <PageContainer className="world-field world-v2 world-v3 field-grain">
        <NavBar world="v2" />
        <main className="pt-[84px] max-w-[1440px] mx-auto">
          {/*<SeedBanner />*/}
          <HeroSection />
          <WhyNotBestEffortSection />
          <TopologyClassesSection />
          <PropertiesSection />
          <BenchmarksSection />
          <MikoshiSection />
          <ComputeRuntimeSection />
          <DatafortsSection />
          <MeshOsSection />
          <ComponentsSection />
          <InstallSection />
          <ApplicationsSection />
          <BlackwallSection />
          <ReleasesSection />
          <ClosingSection />
          <Footer world="field" />
        </main>
      </PageContainer>
    </WorldVariantProvider>
  );
}
