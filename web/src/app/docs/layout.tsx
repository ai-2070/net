import { NavBar } from "@/components/NavBar";
import { DocsSidebar } from "@/components/DocsSidebar";
import { DocsDrawer } from "@/components/DocsDrawer";
import { DocsSearchModal } from "@/components/DocsSearchModal";
import { LanguageHydrator } from "@/components/LanguageHydrator";
import { LanguageDock } from "@/components/LanguageDock";
import { getClientDocTree, getDocsVersion } from "@/lib/docs";
import { PageContainer } from "@/components/PageContainer";

export const metadata = {
  title: "Docs · Net",
};

export default function DocsLayout({
  children,
}: {
  children: React.ReactNode;
}) {
  const tree = getClientDocTree();
  const version = getDocsVersion();
  return (
    <PageContainer className="bg-black">
      <LanguageHydrator />
      <NavBar />
      {/* Press-`/` search overlay; renders only while open and mounts a
          global keypress listener for the lifetime of the docs layout. */}
      <DocsSearchModal />
      {/* Mobile/tablet nav: sticky toggle bar + slide-in drawer (hidden at lg+). */}
      <DocsDrawer tree={tree} version={version} />
      <div className="pt-20 max-w-[1440px] mx-auto">
        {/* `pb-24` clears the fixed dock. Without it the last thing on every
            page — the prev/next control, the thing a reader uses to continue —
            sits underneath the floating bar. */}
        <div className="grid grid-cols-1 lg:grid-cols-[260px_minmax(0,1fr)] xl:grid-cols-[260px_minmax(0,1fr)_220px] gap-8 lg:gap-10 px-4 sm:px-6 py-8 lg:py-10 pb-24 lg:pb-24">
          {/* Inline sidebar — only at lg+. Hidden via display:none on
              smaller breakpoints so the grid collapses to a single column. */}
          {/* The language switcher used to sit here, above the tree. It has
              moved to the floating dock below: inside this aside it was
              `hidden lg:block` (so mobile reached it only through the drawer)
              and it scrolled out of the viewport as soon as a reader started
              reading. Two controls for one piece of state would be worse than
              either, so this is a move rather than an addition. */}
          {/* `-10rem` rather than `-7rem`: the tree's own scroll region has to
              stop above the dock, or its last entries sit under it with no way
              to reach them. */}
          <aside className="hidden lg:block lg:sticky lg:top-24 lg:self-start lg:max-h-[calc(100vh-10rem)] lg:overflow-y-auto pr-2">
            <DocsSidebar tree={tree} version={version} />
          </aside>
          {/* Page renders <main>…</main> + <aside>TOC</aside>. At lg the
              aside is display:none so the grid sees only 2 items; at xl
              the aside lights up to fill the 3rd column. */}
          {children}
        </div>
      </div>
      {/* Persistent language context at every breakpoint. Fixed, so it is the
          one control that does not scroll away from the reader. */}
      <LanguageDock />
    </PageContainer>
  );
}
