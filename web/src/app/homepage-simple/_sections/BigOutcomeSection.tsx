import { SectionLabel } from "@/components/SectionLabel";
import { DisplayHeading } from "@/components/DisplayHeading";

export function BigOutcomeSection() {
  return (
    <section id="outcome" className="border-b border-line px-6 py-20">
      <SectionLabel>§08 / the big outcome</SectionLabel>
      <DisplayHeading>
        the future agent stack
        <br />
        needs an <span className="text-accent">operating fabric.</span>
      </DisplayHeading>

      <div className="grid grid-cols-1 lg:grid-cols-2 gap-10 mt-6">
        <div>
          <p className="text-[15px] text-ink leading-[1.7] mb-5">
            The first AI wave was about access to intelligence. The next wave is
            about letting that intelligence do useful work.
          </p>
          <p className="text-[13px] text-ink-dim leading-[1.7]">
            Useful work does not happen inside one chat box. It happens across
            machines, applications, files, streams, devices, services, and
            compute. Net is the layer that lets all of those participants work
            together.
          </p>
        </div>
        <div className="border border-line bg-bg-2 p-7 flex flex-col justify-center">
          <div className="text-[10px] text-ink-dim tracking-[0.14em] uppercase mb-4">
            the expansion, in one line
          </div>
          <p className="text-[14px] text-ink leading-[1.7]">
            It starts with agents using tools across trusted devices. It grows
            into a broader fabric for autonomous work:{" "}
            <span className="text-accent">
              personal compute, GPU networks, robotics, sensors, edge systems,
              and machine-to-machine services.
            </span>
          </p>
        </div>
      </div>

      <div className="mt-14 text-center py-16 border-t border-b border-accent-dim bg-accent/[0.02]">
        <p className="text-[11px] text-ink-dim tracking-[0.16em] uppercase mb-4">
          the long-term bet
        </p>
        <div
          className="font-display text-ink leading-[1.15]"
          style={{ fontSize: "clamp(26px, 3.8vw, 46px)" }}
        >
          as ai moves from answering to operating,
          <br />
          the <span className="text-accent">operating layer</span> becomes one
          of
          <br />
          the most important parts of the stack.
        </div>
      </div>
    </section>
  );
}
