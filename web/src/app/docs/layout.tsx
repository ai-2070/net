import { NavBar } from "@/components/NavBar";
import { DocsSidebar } from "@/components/DocsSidebar";
import { DocsDrawer } from "@/components/DocsDrawer";
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
      <NavBar />
      {/* Mobile/tablet nav: sticky toggle bar + slide-in drawer (hidden at lg+). */}
      <DocsDrawer tree={tree} />
      <div className="pt-20 max-w-[1440px] mx-auto">
        <div className="grid grid-cols-1 lg:grid-cols-[260px_minmax(0,1fr)] gap-10 px-4 sm:px-6 py-8 lg:py-10">
          {/* Inline sidebar — only at lg+. Hidden via display:none on
              smaller breakpoints so the grid collapses to a single column. */}
          <aside className="hidden lg:block lg:sticky lg:top-24 lg:self-start lg:max-h-[calc(100vh-7rem)] lg:overflow-y-auto pr-3 lg:border-r lg:border-line">
            <DocsSidebar tree={tree} />
          </aside>
          <main className="min-w-0 max-w-[740px]">{children}</main>
        </div>
      </div>
    </>
  );
}
