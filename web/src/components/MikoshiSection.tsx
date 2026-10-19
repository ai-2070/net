import { FieldLabel as SectionLabel } from "./field/FieldLabel";
import { FieldHeading as DisplayHeading } from "./field/FieldHeading";
import { MikoshiTransit } from "./field/MikoshiTransit";

export function MikoshiSection() {
  return (
    <section
      id="mikoshi"
      className="relative pl-6 pr-12 lg:pl-10 lg:pr-24 py-24 lg:py-32"
    >
      <SectionLabel>§05 / mikoshi // engram transit</SectionLabel>
      <DisplayHeading>
        state moves.
        <br />
        connections don&apos;t.
      </DisplayHeading>

      <div className="grid grid-cols-1 md:grid-cols-2 gap-x-14 gap-y-1 mt-8">
        <div>
          <p className="text-ink-dim text-[15px] leading-[1.65] mb-5 max-w-[38em]">
            <strong className="text-ink font-medium">
              In Cyberpunk, Mikoshi is Arasaka&apos;s construct for storing
              engrams
            </strong>{" "}
            — consciousness held in digital space, minds persisting outside
            their original hardware.
          </p>
          <p className="text-ink-dim text-[15px] leading-[1.65] mb-5 max-w-[38em]">
            <strong className="text-ink font-medium">
              Mikoshi in NET lets a deamon hop between machines.
            </strong>{" "}
            The machine underneath changes; the daemon keeps its identity, its
            history, its pending work, and its place in the conversation. The
            source packages its state, the target unpacks it, and for a brief
            moment the daemon exists on both nodes, then collapses onto the
            target as routing cuts over.
          </p>
        </div>
        <div>
          <p className="text-ink-dim text-[15px] leading-[1.65] mb-5 max-w-[38em]">
            <strong className="text-ink font-medium">
              The daemon doesn&apos;t know it moved.
            </strong>{" "}
            Neither does anything talking to it. The hardware shifted; the
            stream didn't notice.
          </p>
          <p className="text-ink-dim text-[15px] leading-[1.65] mb-5 max-w-[38em]">
            A factory controller hops from a dying edge box to a healthy one
            mid-shift. A trading agent migrates to a node closer to the exchange{" "}
            <strong className="text-ink font-medium">
              without dropping a single tick
            </strong>
            .
          </p>
        </div>
      </div>

      <MikoshiTransit />

      <div className="slab slab-held px-8 md:px-12 py-10 md:py-12 mt-16 mb-4 max-w-[960px]">
        <p className="text-[22px] md:text-[28px] text-ink leading-[1.3] font-light tracking-[-0.01em]">
          Mikoshi doesn&apos;t move a copy.{" "}
          <strong className="text-accent font-medium">
            Mikoshi moves the daemon itself.
          </strong>
        </p>
      </div>
    </section>
  );
}
