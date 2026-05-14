import { SectionLabel } from "./SectionHeadings";
import { DisplayHeading } from "./DisplayHeading";

export function MikoshiSection() {
  return (
    <section id="mikoshi" className="border-b border-line px-6 py-20">
      <SectionLabel>§05 / mikoshi // engram transit</SectionLabel>
      <DisplayHeading>
        state moves.
        <br />
        connections don&apos;t.
      </DisplayHeading>

      <div className="grid grid-cols-1 md:grid-cols-2 gap-8 mt-6">
        <div>
          <p className="text-ink-dim text-[13px] leading-[1.7] mb-4">
            <strong className="text-ink font-medium">
              In Cyberpunk, Mikoshi is Arasaka&apos;s construct for storing
              engrams
            </strong>{" "}
            — consciousness held in digital space, minds persisting outside
            their original hardware.
          </p>
          <p className="text-ink-dim text-[13px] leading-[1.7] mb-4">
            Mikoshi in Net is how daemons move between machines. A running
            program on one node becomes a running program on another without
            losing its history, its pending work, or its place in the
            conversation. The source packages its state, the target unpacks it,
            and for a brief moment the entity exists on both nodes at once —
            spreading, superposed, then collapsed onto the target as routing
            cuts over.
          </p>
        </div>
        <div>
          <p className="text-ink-dim text-[13px] leading-[1.7] mb-4">
            <strong className="text-ink font-medium">
              The daemon doesn&apos;t know it moved.
            </strong>{" "}
            Neither does anything talking to it. Observer nodes watching the
            stream see the same causal chain continue uninterrupted, the same
            sequence numbers, the same entity speaking. The hardware underneath
            shifted. The stream didn&apos;t notice.
          </p>
          <p className="text-ink-dim text-[13px] leading-[1.7] mb-4">
            A factory controller hops from a dying edge box to a healthy one
            mid-shift. An inference daemon follows its user from laptop to
            desktop. A trading agent migrates to a node closer to the exchange{" "}
            <strong className="text-ink font-medium">
              without dropping a single tick
            </strong>
            .
          </p>
        </div>
      </div>

      <div className="border-l-2 border-accent pl-8 pr-8 py-6 bg-accent/[0.02] my-12 max-w-[900px]">
        <p className="text-[18px] text-ink leading-[1.5] font-light">
          It doesn&apos;t move a copy.{" "}
          <strong className="text-accent font-medium">
            Mikoshi carries the thing itself across.
          </strong>
        </p>
      </div>
    </section>
  );
}
