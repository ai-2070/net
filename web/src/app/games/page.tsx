import type { Metadata } from "next";
import { JSX } from "react";
import { NavBar } from "@/components/NavBar";
import { PageContainer } from "@/components/PageContainer";
import { FooterDivider } from "@/components/FooterDivider";
import { Footer } from "@/components/Footer";
import { GamesHero } from "@/components/games/GamesHero";
import { GamesDemoSection } from "@/components/games/GamesDemo";
import {
  GamesAiSection,
  GamesCodeSection,
  GamesEnginesSection,
  GamesModesSection,
  GamesOpenSection,
  GamesPropsSection,
  GamesSpeedSection,
} from "@/components/games/GamesSections";

export const metadata: Metadata = {
  title:
    "NET for games — multiplayer for any engine, plug and play for Three.js",
  description:
    "NET is an open multiplayer protocol for any game engine, and plug and play for Three.js: add co-op, multiplayer or MMO-scale play with @net-mesh/browser. Players connect directly, one player hosts, zero input delay. Open source and free to ship.",
};

export default function GamesPage(): JSX.Element {
  return (
    <PageContainer>
      <NavBar />
      {/* Hero and demo run edge to edge; everything else keeps the site's
          1440px column (their inner content aligns to the same column). */}
      <main className="pt-20">
        <GamesHero />
        <div className="max-w-[1440px] mx-auto">
          <GamesSpeedSection />
        </div>
        <GamesDemoSection />
        <div className="max-w-[1440px] mx-auto">
          <GamesModesSection />
          <GamesCodeSection />
          <GamesEnginesSection />
          <GamesAiSection />
          <GamesPropsSection />
          <GamesOpenSection />
          <FooterDivider />
          <Footer />
        </div>
      </main>
    </PageContainer>
  );
}
