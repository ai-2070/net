"use client";

import { JSX } from "react";
import { SeedBanner } from "@/components/SeedBanner";
import { HeroSection } from "@/components/HeroSection";
import { AgentEconomySection } from "@/components/AgentEconomySection";
import { BridgeSection } from "@/components/BridgeSection";
import { EverywhereSection } from "@/components/EverywhereSection";
import { IdentitySection } from "@/components/IdentitySection";
import { BlackwallSection } from "@/components/BlackwallSection";
import { BenchmarksSection } from "@/components/BenchmarksSection";
import { UnderTheHoodSection } from "@/components/UnderTheHoodSection";
import { BuildingOnNetSection } from "@/components/BuildingOnNetSection";
import { ClosingSection } from "@/components/ClosingSection";
import { ReleasesSection } from "@/components/ReleasesSection";
import { FooterDivider } from "@/components/FooterDivider";
import { Footer } from "@/components/Footer";
import { NavBar } from "@/components/NavBar";

export default function Home(): JSX.Element {
  return (
    <>
      <NavBar />
      <main className="pt-20 max-w-[1440px] mx-auto">
        <SeedBanner />
        <HeroSection />
        <AgentEconomySection />
        <BridgeSection />
        <EverywhereSection />
        <IdentitySection />
        <BlackwallSection />
        <BenchmarksSection />
        <BuildingOnNetSection />
        <UnderTheHoodSection />
        <ClosingSection />
        <ReleasesSection />
        <FooterDivider />
        <Footer />
      </main>
    </>
  );
}
