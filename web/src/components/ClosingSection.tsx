import { SectionLabel } from "./SectionLabel";
import { DisplayHeading } from "./DisplayHeading";

export function ClosingSection() {
  return (
    <section id="post-cloud" className="border-b border-line px-6 py-20">
      <SectionLabel>§14 / post-cloud</SectionLabel>
      <DisplayHeading>
        not anti-cloud.
        <br />
        <span className="text-accent">post-cloud.</span>
      </DisplayHeading>

      <div className="grid grid-cols-1 md:grid-cols-2 gap-8 mt-6">
        <div>
          <p className="text-ink-dim text-[13px] leading-[1.7] mb-4">
            Cloud infrastructure solves the wrong problem. It moves compute
            closer to a central provider.{" "}
            <strong className="text-ink font-medium">
              NET decouples storage and compute from hardware and location.
            </strong>
          </p>
          <p className="text-ink-dim text-[13px] leading-[1.7] mb-4">
            Cloud adds a trusted intermediary by definition.{" "}
            <strong className="text-ink font-medium">
              NET has no intermediaries.
            </strong>{" "}
            Relay nodes forward encrypted bytes they cannot read. There is no
            Cloudflare, no AWS, no Azure in the path because the path is yours.
          </p>
        </div>
        <div>
          <p className="text-ink-dim text-[13px] leading-[1.7] mb-4">
            <strong className="text-ink font-medium">
              Cloud was the right answer when compute was scarce and hardware
              was expensive.
            </strong>{" "}
            Compute is abundant. Hardware is cheap. The coordination layer
            should reflect that.
          </p>
          <p className="text-ink-dim text-[13px] leading-[1.7] mb-4">
            A manufacturing plant running on NET doesn&apos;t route sensor data
            to AWS us-east-1 and back. The sensor talks directly to the decision
            system on the factory floor.{" "}
            <strong className="text-ink font-medium">
              The latency is physics, not geography plus cloud overhead.
            </strong>
          </p>
        </div>
      </div>

      <div className="mt-16 text-center py-16 border-t border-b border-accent-dim bg-accent/[0.02]">
        <div
          className="font-display text-ink leading-[1.1] mb-5"
          style={{ fontSize: "clamp(28px, 4vw, 48px)" }}
        >
          the mesh is <span className="text-accent">already</span>
          <br />
          running.
        </div>
        <a
          href="#install"
          className="btn-primary inline-flex items-center gap-2.5 px-5 py-3 text-[11px] tracking-[0.12em] uppercase font-semibold no-underline border border-accent bg-accent text-bg transition-all mt-5"
        >
          ↓ Join the NET <span className="text-sm">→</span>
        </a>
      </div>
    </section>
  );
}
