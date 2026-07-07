"use client";

import { NavBar } from "@/components/NavBar";
import { Footer } from "@/components/Footer";
import { FooterDivider } from "@/components/FooterDivider";
import { ArpanetEssaySection } from "@/components/ArpanetEssaySection";
import { PropertiesSection } from "@/components/PropertiesSection";
import { ComponentsSection } from "@/components/ComponentsSection";

export default function ProtocolPage() {
  return (
    <>
      <NavBar />
      <main className="pt-20 max-w-[1440px] mx-auto">
        <ArpanetEssaySection />
        <PropertiesSection />
        <ComponentsSection />
        <FooterDivider />
        <Footer />
      </main>
    </>
  );
}
