"use client";

import { JSX } from "react";
import { MeshOsSection } from "@/components/MeshOsSection";
import { SeedBanner } from "@/components/SeedBanner";
import { HeroSection } from "@/components/HeroSection";
import { MikoshiSection } from "@/components/MikoshiSection";
import { DatafortsSection } from "@/components/DatafortsSection";
import { ComponentsSection } from "@/components/ComponentsSection";
import { InstallSection } from "@/components/InstallSection";
import { ApplicationsSection } from "@/components/ApplicationSection";
import { ReleasesSection } from "@/components/ReleasesSection";
import { ClosingSection } from "@/components/ClosingSection";
import { FooterDivider } from "@/components/FooterDivider";
import { Footer } from "@/components/Footer";
import { NavBar } from "@/components/NavBar";
import { BlackwallSection } from "@/components/BlackwallSection";
import { ComputeRuntimeSection } from "@/components/ComputeRuntimeSection";
import { BenchmarksSection } from "@/components/BenchmarksSection";
import { PropertiesSection } from "@/components/PropertiesSection";
import { WhyNotBestEffortSection } from "@/components/WhyNotBestEffortSection";
import { TopologyClassesSection } from "@/components/TopologyClassesSection";

export default function Home(): JSX.Element {
  return (
    <>
      <NavBar />
      <main className="pt-20 max-w-[1440px] mx-auto">
        <SeedBanner />
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
        <FooterDivider />
        <Footer />
      </main>
    </>
  );
}
