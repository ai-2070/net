import { ArpanetMapBg } from "./ArpanetMapBg";
import { DisplayHeading } from "./DisplayHeading";
import { SectionLabel } from "./SectionLabel";

export function WhyNotBestEffortSection() {
  return (
    <section
      id="what"
      className="relative overflow-hidden border-b border-line px-6 py-20"
    >
      <ArpanetMapBg />
      <div className="relative">
        <SectionLabel>§01 / why not best-effort</SectionLabel>
        <DisplayHeading>
          arpanet assumed scarcity.
          <br />
          <span className="text-accent">net assumes abundance.</span>
        </DisplayHeading>

        <p className="text-[16px] text-ink max-w-[740px] leading-[1.6] font-light mb-12">
          TCP was designed when nuclear war was a real possibility. Packets were
          precious. The network had to guarantee delivery because the next
          packet might not get through.
        </p>

        <div className="grid grid-cols-1 md:grid-cols-2 gap-8 mt-6">
          <div>
            <p className="text-ink-dim text-[13px] leading-[1.7] mb-4">
              <strong className="text-ink font-medium">
                That was the right design for 1969.
              </strong>{" "}
              It&apos;s the wrong design now. Sensors don&apos;t pause. Token
              streams don&apos;t wait. Market feeds don&apos;t care that your
              queue is full. The firehose doesn&apos;t have a pause button.
            </p>
            <p className="text-ink-dim text-[13px] leading-[1.7] mb-4">
              In a world of abundance, guaranteeing delivery is a threat —
              you&apos;re promising to deliver data that will bury the receiver.
              The bottleneck isn&apos;t delivery. It&apos;s processing. Arrival
              doesn&apos;t equal usefulness.
            </p>
          </div>
          <div>
            <p className="text-ink-dim text-[13px] leading-[1.7] mb-4">
              <strong className="text-ink font-medium">
                NET inverts the default.
              </strong>{" "}
              TCP starts with trust and detects abuse. NET starts with zero
              assumptions and lets trust emerge from consistent behavior.
            </p>
            <p className="text-ink-dim text-[13px] leading-[1.7] mb-4">
              Nodes reject work they can&apos;t process within a time window.
              Dropping a packet and re-requesting from a faster node costs
              nanoseconds. Waiting for a congested node&apos;s guaranteed
              response costs milliseconds.{" "}
              <strong className="text-ink font-medium">
                When dropping is cheaper than waiting, delivery guarantees
                become overhead.
              </strong>
            </p>
          </div>
        </div>

        <div className="border-l-2 border-accent pl-8 pr-8 py-6 bg-accent/[0.02] my-12 max-w-[900px]">
          <p className="text-[18px] text-ink leading-[1.5] font-light">
            The remaining latency is physics: NIC, wire, speed of light.{" "}
            <span className="text-accent">
              The software got out of the way.
            </span>
          </p>
        </div>
      </div>
    </section>
  );
}
