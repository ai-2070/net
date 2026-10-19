import type { Metadata } from "next";
import { Cinzel, EB_Garamond } from "next/font/google";
import { JSX } from "react";
import { NavBar } from "@/components/NavBar";
import { PageContainer } from "@/components/PageContainer";
import { Footer } from "@/components/Footer";
import { GamesHero } from "@/components/games/GamesHero";
import { GamesDemoSection } from "@/components/games/GamesDemo";
import { GamesStarterSection } from "@/components/games/GamesStarter";
import {
  GamesAiSection,
  GamesCodeSection,
  GamesEnginesSection,
  GamesModesSection,
  GamesOpenSection,
  GamesPropsSection,
  GamesShardsSection,
  GamesSpeedSection,
} from "@/components/games/GamesSections";

// Rose & Blade's own type, for the demo section. Loaded here, not in the root
// layout, so only this page fetches it.
const rbDisplay = Cinzel({
  variable: "--font-rb-display",
  subsets: ["latin"],
  weight: ["500", "700", "900"],
});

const rbBody = EB_Garamond({
  variable: "--font-rb-body",
  subsets: ["latin"],
  weight: ["400", "500", "600"],
});

export const metadata: Metadata = {
  title:
    "NET for games — multiplayer for any engine, plug and play for Three.js",
  description:
    "Multiplayer for any game engine, plug and play for Three.js. Players connect directly, one player hosts or several share a match, and there is no game server to run. Play Rose & Blade, a 16-player sword fight, or take apart Net Shooter, an open-source FPS. Free and open source.",
};

export default function GamesPage(): JSX.Element {
  return (
    <PageContainer className="world-field world-notebook field-grain">
      <NavBar world="field" />
      {/* The same notebook as the landing page: each section is its own
          ruled box, the hero is the open hall, the footer its own band. */}
      <main
        className={`pt-[84px] max-w-[1440px] mx-auto ${rbDisplay.variable} ${rbBody.variable}`}
      >
        <GamesHero />
        <GamesSpeedSection />
        <GamesDemoSection />
        <GamesStarterSection />
        <GamesModesSection />
        <GamesShardsSection />
        <GamesCodeSection />
        <GamesEnginesSection />
        <GamesAiSection />
        <GamesPropsSection />
        <GamesOpenSection />
        <Footer world="field" />
      </main>
    </PageContainer>
  );
}
