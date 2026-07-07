"use client";

import { NavBar } from "@/components/NavBar";
import { Footer } from "@/components/Footer";
import { FooterDivider } from "@/components/FooterDivider";
import { DatafortsSection } from "@/components/DatafortsSection";

export default function DatafortsPage() {
  return (
    <>
      <NavBar />
      <main className="pt-20 max-w-[1440px] mx-auto">
        <DatafortsSection />
        <FooterDivider />
        <Footer />
      </main>
    </>
  );
}
