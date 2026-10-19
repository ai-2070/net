import { ScarcityMesh } from "./field/ScarcityMesh";
import { FieldHeading as DisplayHeading } from "./field/FieldHeading";
import { FieldLabel as SectionLabel } from "./field/FieldLabel";

export function WhyNotBestEffortSection() {
  return (
    <section
      id="what"
      className="relative overflow-hidden px-6 lg:px-12 py-24 lg:py-32"
    >
      <SectionLabel>§01 / why not best-effort</SectionLabel>
      <div className="relative">
        <DisplayHeading>
          arpanet assumed scarcity.
          <br />
          <span className="text-accent">net assumes abundance.</span>
        </DisplayHeading>

        <p className="text-[19px] md:text-[21px] text-ink max-w-[34em] leading-[1.5] font-light mb-14">
          TCP was designed when nuclear war was a real possibility. Packets were
          precious. The network had to guarantee delivery because the next
          packet might not get through.
        </p>

        <div className="grid grid-cols-1 lg:grid-cols-[minmax(0,1fr)_minmax(0,1fr)] gap-x-16 gap-y-12 mt-8">
          <div>
            <p className="text-ink text-[17px] leading-[1.6] mb-5 max-w-[38em]">
              <strong className="text-ink font-medium">
                That was the right design for 1969.
              </strong>{" "}
              It&apos;s the wrong design now. Sensors don&apos;t pause. Token
              streams don&apos;t wait. Market feeds don&apos;t care that your
              queue is full. The firehose doesn&apos;t have a pause button.
            </p>
            <p className="text-ink text-[17px] leading-[1.6] mb-5 max-w-[38em]">
              In a world of abundance, guaranteeing delivery is a threat —
              you&apos;re promising to deliver data that will bury the receiver.
              The bottleneck isn&apos;t delivery. It&apos;s processing. Arrival
              doesn&apos;t equal usefulness.
            </p>
            <p className="text-ink text-[17px] leading-[1.6] mb-5 max-w-[38em]">
              <strong className="text-ink font-medium">
                NET inverts the default.
              </strong>{" "}
              TCP starts with trust and detects abuse. NET starts with zero
              assumptions and lets trust emerge from consistent behavior.
            </p>
            <p className="text-ink text-[17px] leading-[1.6] mb-5 max-w-[38em]">
              Nodes reject work they can&apos;t process within a time window.
              Dropping a packet and re-requesting from a faster node costs
              nanoseconds. Waiting for a congested node&apos;s guaranteed
              response costs milliseconds.{" "}
              <strong className="text-ink font-medium">
                When dropping is cheaper than waiting, delivery guarantees
                become overhead.
              </strong>
            </p>

            <div className="slab slab-held px-8 md:px-12 py-10 md:py-12 mt-12 mb-4">
              <p className="text-[22px] md:text-[26px] text-ink leading-[1.3] font-light tracking-[-0.01em]">
                The remaining latency is physics: NIC, wire, speed of light.{" "}
                <span className="text-accent">
                  The software got out of the way.
                </span>
              </p>
            </div>
          </div>
          <div className="lg:sticky lg:top-28 lg:self-start lg:pt-2">
            <ScarcityMesh />
          </div>
        </div>
      </div>
    </section>
  );
}
