import { SectionLabel } from "@/components/SectionLabel";
import { DisplayHeading } from "@/components/DisplayHeading";

const REBUILT: ReadonlyArray<string> = [
  "tool routing",
  "file movement",
  "worker dispatch",
  "device access",
  "stream handling",
  "remote execution",
  "policy boundaries",
];

export function VentureScaleSection() {
  return (
    <section
      id="venture"
      className="compute-bg border-b border-line px-6 py-24"
    >
      <SectionLabel>§09 / why venture-scale</SectionLabel>
      <DisplayHeading>
        net is not a feature.
        <br />
        it is the <span className="text-accent">substrate.</span>
      </DisplayHeading>

      <div className="grid grid-cols-1 lg:grid-cols-2 gap-10 mt-6">
        <div>
          <p className="text-[15px] text-ink leading-[1.6] mb-5">
            Every major AI product is moving toward the same pressure point:
            models are becoming capable enough to act, but the operating
            environment around them is still fragmented.
          </p>
          <p className="text-[13px] text-ink-dim leading-[1.7]">
            Today, each company rebuilds its own version of the same plumbing.
            That work should not be trapped inside every agent harness. It
            belongs in a substrate.
          </p>
        </div>
        <div className="border border-line bg-bg-2 p-7">
          <div className="text-[10px] text-warn tracking-[0.14em] uppercase mb-4">
            rebuilt inside every harness today
          </div>
          <div className="flex flex-wrap gap-2">
            {REBUILT.map((r) => (
              <span
                key={r}
                className="border border-line px-3 py-1.5 text-[12px] text-ink-dim lowercase line-through decoration-warn/50"
              >
                {r}
              </span>
            ))}
          </div>
          <div className="border-t border-dashed border-line mt-6 pt-4">
            <p className="text-[13px] text-ink leading-[1.6]">
              One substrate, not N reinventions.{" "}
              <span className="text-accent">
                The coordination layer is the venture, not the chat UI.
              </span>
            </p>
          </div>
        </div>
      </div>

      <div className="mt-16 text-center py-14 border-t border-b border-accent-dim bg-accent/[0.02]">
        <div
          className="font-display text-ink leading-[1.1] mb-2"
          style={{ fontSize: "clamp(26px, 4vw, 46px)" }}
        >
          if agents become operators,
          <br />
          they need an <span className="text-accent">operating fabric.</span>
        </div>
        <p className="text-[13px] text-ink-dim mt-6 tracking-[0.06em] font-mono">
          Net is that fabric.
        </p>
      </div>
    </section>
  );
}
