import { NavBar } from "@/components/NavBar";
import { DocsSidebar } from "@/components/DocsSidebar";
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
      <div className="pt-20 max-w-[1440px] mx-auto">
        <div className="grid grid-cols-1 lg:grid-cols-[260px_minmax(0,1fr)] gap-10 px-6 py-10">
          <aside className="lg:sticky lg:top-24 lg:self-start lg:max-h-[calc(100vh-7rem)] lg:overflow-y-auto pr-2 lg:border-r lg:border-line">
            <DocsSidebar tree={tree} />
          </aside>
          <main className="min-w-0 max-w-[860px]">{children}</main>
        </div>
      </div>
    </>
  );
}
