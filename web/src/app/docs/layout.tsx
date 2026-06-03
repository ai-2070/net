import { NavBar } from "@/components/NavBar";
import { DocsSidebar } from "@/components/DocsSidebar";
import { DocsDrawer } from "@/components/DocsDrawer";
import { LanguageHydrator } from "@/components/LanguageHydrator";
import { LanguageSwitcher } from "@/components/LanguageSwitcher";
import { getClientDocTree } from "@/lib/docs";

export const metadata = {
  title: "Docs · Net",
};

export default function DocsLayout({
  children,
}: {
  children: React.ReactNode;
}) {
  const tree = getClientDocTree();
  return (
    <>
      <LanguageHydrator />
      <NavBar />
      {/* Mobile/tablet nav: sticky toggle bar + slide-in drawer (hidden at lg+). */}
      <DocsDrawer tree={tree} />
      <div className="pt-20 max-w-[1440px] mx-auto">
        <div className="grid grid-cols-1 lg:grid-cols-[260px_minmax(0,1fr)] xl:grid-cols-[260px_minmax(0,1fr)_220px] gap-8 lg:gap-10 px-4 sm:px-6 py-8 lg:py-10">
          {/* Inline sidebar — only at lg+. Hidden via display:none on
              smaller breakpoints so the grid collapses to a single column. */}
          <aside className="hidden lg:block lg:sticky lg:top-24 lg:self-start lg:max-h-[calc(100vh-7rem)] lg:overflow-y-auto pr-2">
            <LanguageSwitcher className="mb-3" />
            <DocsSidebar tree={tree} />
          </aside>
          {/* Page renders <main>…</main> + <aside>TOC</aside>. At lg the
              aside is display:none so the grid sees only 2 items; at xl
              the aside lights up to fill the 3rd column. */}
          {children}
        </div>
      </div>
    </>
  );
}
