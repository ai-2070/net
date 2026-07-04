import { SectionLabel } from "@/components/SectionLabel";
import { DisplayHeading } from "@/components/DisplayHeading";
import { MeshSim } from "./MeshSim";

const VERBS: ReadonlyArray<string> = [
  "turn any machine into a tool",
  "borrow compute from anywhere",
  "run work on the machine built for it",
  "move files and models instantly",
  "stream any screen, camera, or sensor",
  "claim a gpu before anyone else",
  "outlive crashes and disconnects",
  "drive dozens of machines at once",
];

export function WhatNetDoesSection() {
  return (
    <section id="what" className="border-b border-line px-6 py-20">
      <SectionLabel>§03 / what net does</SectionLabel>
      <DisplayHeading>
        connect in.
        <br />
        <span className="text-accent">command everything.</span>
      </DisplayHeading>

      <p className="text-[16px] text-ink max-w-[760px] leading-[1.6] font-light mb-10">
        Connect an agent, a machine, or a program to Net and it gains powers no
        machine has on its own:
      </p>

      <ul className="daemon-list grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-4 gap-x-6 gap-y-3 mb-12">
        {VERBS.map((v) => (
          <li
            key={v}
            className="relative pl-6 text-[14px] text-ink leading-[1.5]"
          >
            {v}
          </li>
        ))}
      </ul>

      {/* the same idea, alive: machines join, say what they can do, and an
          agent moves work to wherever fits */}
      <div className="text-[10px] text-ink-dim tracking-[0.16em] uppercase mb-4">
        watch it happen — a mesh organizing itself
      </div>
      <MeshSim />
      <p className="text-[12px] text-ink-dim leading-[1.6] mt-4 max-w-[820px]">
        Every node is a real machine — a laptop, a phone, a gpu box, a camera, a
        file store. Each one joins on its own and says what it can safely do.
        The <span className="text-cyan">agent</span> discovers what is available
        and moves work to wherever it fits — no central controller telling
        everything what to do.
      </p>
    </section>
  );
}
