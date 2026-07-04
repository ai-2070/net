import { SectionLabel } from "@/components/SectionLabel";
import { DisplayHeading } from "@/components/DisplayHeading";

interface Piece {
  layer: string;
  solves: string;
  blind: string;
}

const PIECES: ReadonlyArray<Piece> = [
  {
    layer: "tool protocols",
    solves: "function invocation",
    blind: "where work runs · who owns the resource",
  },
  {
    layer: "service meshes",
    solves: "cloud-to-cloud traffic",
    blind: "agents · devices · local authority",
  },
  {
    layer: "job queues",
    solves: "worker dispatch",
    blind: "streams · artifacts · discovery",
  },
  {
    layer: "saas apis",
    solves: "one vendor surface",
    blind: "the mesh between your machines",
  },
];

export function TheGapSection() {
  return (
    <section id="gap" className="border-b border-line px-6 py-20">
      <SectionLabel>§02 / the gap</SectionLabel>
      <DisplayHeading>
        tool calls are not
        <br />
        an agent <span className="text-accent">operating system.</span>
      </DisplayHeading>

      <p className="text-[16px] text-ink max-w-[760px] leading-[1.6] font-light mb-4">
        Tool protocols made it easier for models to call functions. That was
        necessary. It was not sufficient.
      </p>
      <p className="text-[13px] text-ink-dim max-w-[760px] leading-[1.7] mb-12">
        A function call does not tell an agent where work should run, who owns
        the resource, whether a GPU is already claimed, where the output
        artifact lives, how to resume after a disconnect, how to bridge a
        trusted device, or how to stream changing state back into the system.
      </p>

      <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-4 gap-px bg-line border border-line">
        {PIECES.map((p) => (
          <div key={p.layer} className="bg-bg p-7 flex flex-col">
            <h3 className="font-head text-[18px] leading-tight text-ink mb-2 tracking-[0.04em] lowercase">
              {p.layer}
            </h3>
            <div className="text-[11px] text-accent-dim mb-4 tracking-[0.04em]">
              solves: {p.solves}
            </div>
            <div className="border-t border-dashed border-line pt-3 mt-auto">
              <div className="text-[9px] text-ink-faint tracking-[0.12em] uppercase mb-1">
                blind to
              </div>
              <div className="text-[11px] text-ink-dim leading-[1.5]">
                {p.blind}
              </div>
            </div>
          </div>
        ))}
      </div>

      <div className="mt-10 border-l-2 border-accent-dim pl-5 max-w-[820px]">
        <p className="text-[14px] text-ink leading-[1.65]">
          These are pieces. They do not become an agent operating fabric by
          accident. Net is built for the layer{" "}
          <span className="text-accent">below the agent harness</span> and{" "}
          <span className="text-accent">above raw transport</span> — the
          coordination substrate autonomous participants need once they leave
          one process.
        </p>
      </div>
    </section>
  );
}
