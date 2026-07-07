"use client";

import { NavBar } from "@/components/NavBar";
import { Footer } from "@/components/Footer";
import { FooterDivider } from "@/components/FooterDivider";
import { MeshOsSection } from "@/components/MeshOsSection";

export default function MeshOsPage() {
  return (
    <>
      <NavBar />
      <main className="pt-20 max-w-[1440px] mx-auto">
        <MeshOsSection />
        <FooterDivider />
        <Footer />
      </main>
    </>
  );
}
