import type { Metadata } from "next";
import { JSX } from "react";
import { NavBar } from "@/components/NavBar";
import { PageContainer } from "@/components/PageContainer";
import { Footer } from "@/components/Footer";
import { BlacksiteHero } from "@/components/blacksite/BlacksiteHero";
import { BlacksiteLibrarySection } from "@/components/blacksite/BlacksiteLibrary";
import {
  BlacksiteBrainSection,
  BlacksiteChatsSection,
  BlacksiteEnginesSection,
  BlacksiteGetSection,
  BlacksiteLocalSection,
  BlacksiteProgramsSection,
  BlacksiteReviewSection,
  BlacksiteStationSection,
} from "@/components/blacksite/BlacksiteSections";

export const metadata: Metadata = {
  title: "Blacksite — run AI agents and teams from one local workspace",
  description:
    "Blacksite is a local AI agent dashboard. Hire from 270+ ready-made specialists, give each agent its own model, effort, tools and memory, and watch the team work on a live 3D map. Works with Claude Code, Codex, OpenRouter, local models and MCP. No account, runs on your computer. Free and open source.",
};

export default function BlacksitePage(): JSX.Element {
  return (
    <PageContainer className="world-field world-notebook field-grain">
      <NavBar world="field" />
      {/* The same notebook as the landing page and /games: each section is
          its own ruled box, the hero is the open hall, the footer its own
          band. */}
      <main className="pt-[84px] max-w-[1440px] mx-auto">
        <BlacksiteHero />
        <BlacksiteStationSection />
        <BlacksiteBrainSection />
        <BlacksiteLibrarySection />
        <BlacksiteEnginesSection />
        <BlacksiteChatsSection />
        <BlacksiteProgramsSection />
        <BlacksiteReviewSection />
        <BlacksiteLocalSection />
        <BlacksiteGetSection />
        <Footer world="field" page="blacksite" />
      </main>
    </PageContainer>
  );
}
