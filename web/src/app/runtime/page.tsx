"use client";

import { NavBar } from "@/components/NavBar";
import { Footer } from "@/components/Footer";
import { FooterDivider } from "@/components/FooterDivider";
import { MikoshiSection } from "@/components/MikoshiSection";
import { ComputeRuntimeSection } from "@/components/ComputeRuntimeSection";

export default function RuntimePage() {
  return (
    <>
      <NavBar />
      <main className="pt-20 max-w-[1440px] mx-auto">
        <MikoshiSection />
        <ComputeRuntimeSection />
        <FooterDivider />
        <Footer />
      </main>
    </>
  );
}
