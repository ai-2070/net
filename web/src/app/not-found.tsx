import type { Metadata } from "next";
import { JSX } from "react";
import { NavBar } from "@/components/NavBar";
import { PageContainer } from "@/components/PageContainer";
import { Footer } from "@/components/Footer";
import { NotFoundHero } from "@/components/notfound/NotFoundHero";

export const metadata: Metadata = {
  title: "Page not found — NET",
  description:
    "This page isn't on the mesh. It may have moved, or the link has a typo.",
};

// Any URL the site doesn't have lands here, with the normal header and
// footer so there is always a way back.
export default function NotFound(): JSX.Element {
  return (
    <PageContainer className="world-field world-notebook field-grain">
      <NavBar world="field" />
      <main className="pt-[84px] max-w-[1440px] mx-auto">
        <NotFoundHero />
        <Footer world="field" />
      </main>
    </PageContainer>
  );
}
