import { SectionLabel } from "./SectionLabel";
import { DisplayHeading } from "./DisplayHeading";
import { DatafortsConsole } from "./DatafortsConsole";

const CAPABILITY_STRIP: ReadonlyArray<{
  num: string;
  name: string;
  body: string;
  isNew?: boolean;
}> = [
  {
    num: "mesh.storage.1",
    name: "Overflow",
    isNew: true,
    body: "storage doesn't run out. when one disk fills up, the mesh catches the spillover.",
  },
  {
    num: "mesh.storage.2",
    name: "Data Gravity",
    body: "the files aren't moved. files settle near nodes that use them.",
  },
  {
    num: "mesh.storage.3",
    name: "Read-your-writes",
    body: "if you wrote it, you can read it. right now. no coordination lag.",
  },
  {
    num: "mesh.storage.4",
    name: "BlobRef",
    body: "one handle gets you any file. the mesh finds it — wherever it lives.",
  },
];

export function DatafortsSection() {
  return (
    <section
      id="dataforts"
      className="relative overflow-hidden border-b border-line px-6 py-20"
    >
      <SectionLabel>§07 / storage // new</SectionLabel>
      <DisplayHeading>
        Dataforts:
        <br />
        <span className="text-accent">
          data became
          <br />a fluid.
        </span>
      </DisplayHeading>

      <p className="text-[16px] text-ink max-w-[740px] leading-[1.7] font-light mb-12">
        For 60 years, files were objects nailed to a location — a disk in a box.
        Traditional storage treats files like permanent objects locked to a
        single machine.
        <br />
        <br />
        <strong className="text-accent font-medium">
          Dataforts treats storage as flow.
        </strong>{" "}
        When a device approaches capacity, it overflows onto the mesh. The
        folder stays local. The capacity is the mesh. Reads create gravity. Hot
        data moves closer. Everything is in motion.
      </p>

      <DatafortsConsole />

      {/* Capability strip — compact horizontal list */}
      <div className="mt-12 grid grid-cols-1 md:grid-cols-2 lg:grid-cols-4 border-t border-l border-line">
        {CAPABILITY_STRIP.map((c) => (
          <div
            key={c.name}
            className="border-r border-b border-line bg-bg-2/40 p-5"
          >
            <div className="flex items-baseline justify-between mb-2">
              <span className="font-mono text-[10px] text-accent tracking-[0.14em]">
                ▸ {c.num}
              </span>
              {c.isNew ? (
                <span className="bg-accent text-bg px-1.5 py-0.5 text-[9px] font-bold tracking-[0.18em]">
                  NEW
                </span>
              ) : null}
            </div>
            <h3 className="font-head text-[16px] leading-tight text-ink mb-2 tracking-[0.04em] lowercase">
              {c.name}
            </h3>
            <p className="text-[11px] text-ink-dim leading-[1.55]">{c.body}</p>
          </div>
        ))}
      </div>
    </section>
  );
}
